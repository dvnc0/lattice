//! `${ENV}` interpolation over a loaded [`Config`].
//!
//! Runs after parsing and before defaults-merge. Every value-bearing **string leaf**
//! has `${VAR}` occurrences replaced from the environment: server fields, `base_url`,
//! `path`, all auth fields, and the value-expression leaves (query / headers / body /
//! `body_from` / args / stdin / env / cwd). Two things are deliberately left alone:
//!
//! - **`inputSchema`** — passed verbatim to the harness, so a `${...}` in a schema
//!   description is not touched (and never reported missing).
//! - **bare `$ref`** (no braces) — an input reference resolved by the engine (T6);
//!   only the `${...}` form is an environment variable.
//!
//! Lookups are injected so tests don't mutate the process environment. Every missing
//! variable across the whole config is collected, so one load reports them all at once.
//!
//! There is no escape for a literal `${...}` (e.g. `$${VAR}` is not special) — the
//! `${name}` form is always treated as a variable reference. A malformed or
//! invalid-name `${...}` is left verbatim and not reported (T5's `check` will warn on
//! any residual `${...}`).

use std::collections::BTreeSet;

use serde_json::Value;

use super::{Auth, CliTarget, Config, Defaults, HttpTarget, Server, Tool};

/// An environment lookup: variable name → value (if set).
type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Interpolate `${ENV}` across `config` using the process environment.
///
/// Returns the sorted list of variables that were referenced but unset.
pub(super) fn interpolate(config: &mut Config) -> Result<(), Vec<String>> {
    interpolate_with(config, &|name| std::env::var(name).ok())
}

/// Interpolate `${ENV}` across `config` using a caller-supplied lookup.
pub(super) fn interpolate_with(config: &mut Config, env: &Env) -> Result<(), Vec<String>> {
    let mut missing = BTreeSet::new();
    walk_config(config, env, &mut missing);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.into_iter().collect())
    }
}

/// Replace `${NAME}` occurrences in a single string.
///
/// A bare `$` (not followed by `{`) and any malformed/`${...}` with an invalid name
/// are left exactly as-is. An unset (but well-formed) variable is recorded in `missing`
/// and its placeholder is preserved so nothing is silently blanked.
fn interpolate_str(input: &str, env: &Env, missing: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut name = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                name.push(c);
            }
            if closed && is_valid_env_name(&name) {
                match env(&name) {
                    Some(value) => out.push_str(&value),
                    None => {
                        missing.insert(name.clone());
                        out.push_str("${");
                        out.push_str(&name);
                        out.push('}');
                    }
                }
            } else {
                // Malformed or invalid name — re-emit verbatim.
                out.push_str("${");
                out.push_str(&name);
                if closed {
                    out.push('}');
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Whether `name` is a valid environment-variable identifier (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// --- typed-tree walkers -----------------------------------------------------

fn walk_config(config: &mut Config, env: &Env, missing: &mut BTreeSet<String>) {
    walk_server(&mut config.server, env, missing);
    walk_defaults(&mut config.defaults, env, missing);
    for tool in &mut config.tools {
        walk_tool(tool, env, missing);
    }
}

fn walk_server(server: &mut Server, env: &Env, missing: &mut BTreeSet<String>) {
    istr(&mut server.name, env, missing);
    iopt(&mut server.version, env, missing);
    iopt(&mut server.instructions, env, missing);
}

fn walk_defaults(defaults: &mut Defaults, env: &Env, missing: &mut BTreeSet<String>) {
    iopt(&mut defaults.base_url, env, missing);
    imap(&mut defaults.headers, env, missing);
    if let Some(auth) = defaults.auth.as_mut() {
        walk_auth(auth, env, missing);
    }
}

fn walk_tool(tool: &mut Tool, env: &Env, missing: &mut BTreeSet<String>) {
    istr(&mut tool.name, env, missing);
    iopt(&mut tool.description, env, missing);
    // `input_schema` is intentionally not interpolated (verbatim to the harness).
    if let Some(http) = tool.http.as_mut() {
        walk_http(http, env, missing);
    }
    if let Some(cli) = tool.cli.as_mut() {
        walk_cli(cli, env, missing);
    }
}

fn walk_http(http: &mut HttpTarget, env: &Env, missing: &mut BTreeSet<String>) {
    istr(&mut http.path, env, missing);
    iopt(&mut http.base_url, env, missing);
    imap(&mut http.query, env, missing);
    imap(&mut http.headers, env, missing);
    imap(&mut http.body, env, missing);
    if let Some(value) = http.body_from.as_mut() {
        ivalue(value, env, missing);
    }
    if let Some(auth) = http.auth.as_mut() {
        walk_auth(auth, env, missing);
    }
}

fn walk_cli(cli: &mut CliTarget, env: &Env, missing: &mut BTreeSet<String>) {
    istr(&mut cli.command, env, missing);
    for arg in &mut cli.args {
        ivalue(arg, env, missing);
    }
    if let Some(stdin) = cli.stdin.as_mut() {
        ivalue(stdin, env, missing);
    }
    imap(&mut cli.env, env, missing);
    if let Some(cwd) = cli.cwd.as_mut() {
        ivalue(cwd, env, missing);
    }
}

/// Exhaustive so a new [`Auth`] variant forces an interpolation decision here.
fn walk_auth(auth: &mut Auth, env: &Env, missing: &mut BTreeSet<String>) {
    match auth {
        Auth::Bearer { token } => istr(token, env, missing),
        Auth::Basic { username, password } => {
            istr(username, env, missing);
            istr(password, env, missing);
        }
        Auth::ApiKey {
            location: _,
            name,
            value,
        } => {
            istr(name, env, missing);
            istr(value, env, missing);
        }
        Auth::Oauth2 {
            token_url,
            client_id,
            client_secret,
            scopes,
        } => {
            istr(token_url, env, missing);
            istr(client_id, env, missing);
            istr(client_secret, env, missing);
            for scope in scopes {
                istr(scope, env, missing);
            }
        }
    }
}

// --- leaf helpers -----------------------------------------------------------

fn istr(s: &mut String, env: &Env, missing: &mut BTreeSet<String>) {
    *s = interpolate_str(s, env, missing);
}

fn iopt(s: &mut Option<String>, env: &Env, missing: &mut BTreeSet<String>) {
    if let Some(s) = s.as_mut() {
        istr(s, env, missing);
    }
}

fn imap(map: &mut super::ValueMap, env: &Env, missing: &mut BTreeSet<String>) {
    for value in map.values_mut() {
        ivalue(value, env, missing);
    }
}

/// Recurse into a value expression, interpolating any string it contains.
fn ivalue(value: &mut Value, env: &Env, missing: &mut BTreeSet<String>) {
    match value {
        Value::String(s) => *s = interpolate_str(s, env, missing),
        Value::Array(items) => {
            for item in items {
                ivalue(item, env, missing);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                ivalue(v, env, missing);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_config, Format};
    use serde_json::json;

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn interpolate_str_replaces_and_preserves() {
        let env = env_from(&[("A", "1"), ("B", "two")]);
        let mut missing = BTreeSet::new();
        assert_eq!(interpolate_str("${A}", &env, &mut missing), "1");
        assert_eq!(
            interpolate_str("x${A}-${B}y", &env, &mut missing),
            "x1-twoy"
        );
        // A bare `$` (no braces) is left intact — it's an input ref for the engine.
        assert_eq!(
            interpolate_str("$A and ${A}", &env, &mut missing),
            "$A and 1"
        );
        assert_eq!(
            interpolate_str("price is $5", &env, &mut missing),
            "price is $5"
        );
        assert_eq!(interpolate_str("plain", &env, &mut missing), "plain");
        assert!(missing.is_empty());
    }

    #[test]
    fn interpolate_str_leaves_malformed_intact() {
        let env = env_from(&[("A", "1")]);
        let mut missing = BTreeSet::new();
        assert_eq!(interpolate_str("${1BAD}", &env, &mut missing), "${1BAD}");
        assert_eq!(
            interpolate_str("${has space}", &env, &mut missing),
            "${has space}"
        );
        assert_eq!(
            interpolate_str("${unterminated", &env, &mut missing),
            "${unterminated"
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn interpolate_str_records_missing_and_keeps_placeholder() {
        let env = env_from(&[("A", "1")]);
        let mut missing = BTreeSet::new();
        let out = interpolate_str("${X}-${A}-${Y}", &env, &mut missing);
        assert_eq!(out, "${X}-1-${Y}");
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), vec!["X", "Y"]);
    }

    #[test]
    fn interpolates_config_leaves_but_not_schema_or_input_refs() {
        let mut config = parse_config(
            r#"
server: { name: s }
defaults:
  base_url: "${BASE}"
  headers: { Authorization: "Bearer ${TOKEN}" }
  auth: { type: bearer, token: "${TOKEN}" }
tools:
  - name: t
    inputSchema:
      type: object
      description: "uses ${NOT_INTERPOLATED}"
    http:
      method: GET
      path: "/x/${SEG}"
      body: { ref: "$firstName", key: "${TOKEN}" }
"#,
            Format::Yaml,
        )
        .unwrap();

        // Note: NOT_INTERPOLATED is absent from the env. interpolation succeeding proves
        // the inputSchema was skipped (else it would be reported missing).
        let env = env_from(&[("BASE", "https://h"), ("TOKEN", "secret"), ("SEG", "abc")]);
        interpolate_with(&mut config, &env).unwrap();

        assert_eq!(config.defaults.base_url.as_deref(), Some("https://h"));
        assert_eq!(
            config.defaults.headers["Authorization"],
            json!("Bearer secret")
        );
        match config.defaults.auth.as_ref().unwrap() {
            Auth::Bearer { token } => assert_eq!(token, "secret"),
            other => panic!("expected bearer, got {other:?}"),
        }

        let http = config.tools[0].http.as_ref().unwrap();
        assert_eq!(http.path, "/x/abc");
        assert_eq!(http.body["key"], json!("secret"));
        // Bare `$ref` is left for the engine.
        assert_eq!(http.body["ref"], json!("$firstName"));
        // Schema untouched.
        assert_eq!(
            config.tools[0].input_schema["description"],
            json!("uses ${NOT_INTERPOLATED}")
        );
    }

    #[test]
    fn collects_all_missing_variables() {
        let mut config = parse_config(
            r#"
server: { name: "${SRV}" }
defaults: { base_url: "${BASE}" }
tools:
  - name: t
    http: { method: GET, path: "/${SEG}" }
"#,
            Format::Yaml,
        )
        .unwrap();
        let err = interpolate_with(&mut config, &env_from(&[])).unwrap_err();
        assert_eq!(err, vec!["BASE", "SEG", "SRV"]);
    }
}
