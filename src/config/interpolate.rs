//! `${ENV}` interpolation over a loaded [`Config`].
//!
//! Runs after parsing and before defaults-merge. Every value-bearing **string leaf**
//! has `${VAR}` occurrences replaced from the environment: server fields, `base_url`,
//! `path`, all auth fields, and the value-expression leaves (query / headers / body /
//! `body_from` / args / stdin / env / cwd). Two things are deliberately left alone:
//!
//! - **`inputSchema`** — passed verbatim to the harness, so a `${...}` in a schema
//!   description is not touched (and never reported missing/malformed).
//! - **bare `$ref`** (no braces) — an input reference resolved by the engine (T6);
//!   only the `${...}` form is an environment variable.
//!
//! Lookups are injected so tests don't mutate the process environment. Issues are
//! collected across the whole config: every unset **`missing`** variable and every
//! **`malformed`** `${...}` (invalid name / unterminated) left verbatim.
//!
//! There is no escape for a literal `${...}` (e.g. `$${VAR}` is not special) — the
//! `${name}` form is always treated as a variable reference.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{Auth, CliTarget, Config, Defaults, HttpTarget, Server, Tool};

/// An environment lookup: variable name → value (if set).
type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Issues found during interpolation.
#[derive(Debug, Default)]
pub(super) struct Issues {
    /// Well-formed `${VAR}` references whose variable was unset (sorted, deduped).
    pub missing: BTreeSet<String>,
    /// Malformed `${...}` substrings left verbatim (invalid name / unterminated).
    pub malformed: BTreeSet<String>,
}

/// A lookup backed by the process environment.
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Interpolate `${ENV}` across `config` using the process environment, returning an
/// error listing all unset (well-formed) variables. Malformed placeholders do not fail
/// the load — they surface in `check` (see [`collect_issues`]).
pub(super) fn interpolate(config: &mut Config) -> Result<(), Vec<String>> {
    interpolate_with(config, &process_env)
}

/// Interpolate using the process environment, returning all [`Issues`] without failing.
pub(super) fn collect_issues(config: &mut Config) -> Issues {
    interpolate_collect(config, &process_env)
}

/// As [`interpolate`] but with a caller-supplied lookup; fails on missing variables.
pub(super) fn interpolate_with(config: &mut Config, env: &Env) -> Result<(), Vec<String>> {
    let issues = interpolate_collect(config, env);
    if issues.missing.is_empty() {
        Ok(())
    } else {
        Err(issues.missing.into_iter().collect())
    }
}

/// Interpolate and return all [`Issues`] (missing + malformed) without failing.
pub(super) fn interpolate_collect(config: &mut Config, env: &Env) -> Issues {
    let mut issues = Issues::default();
    walk_config(config, env, &mut issues);
    issues
}

/// Replace `${NAME}` occurrences in a single string.
///
/// A bare `$` (not followed by `{`) is left intact. A well-formed but unset variable is
/// recorded in `issues.missing` with its placeholder preserved (nothing is silently
/// blanked). A malformed `${...}` (invalid name or unterminated) is left verbatim and
/// recorded in `issues.malformed`.
fn interpolate_str(input: &str, env: &Env, issues: &mut Issues) -> String {
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
                        issues.missing.insert(name.clone());
                        out.push_str("${");
                        out.push_str(&name);
                        out.push('}');
                    }
                }
            } else {
                // Malformed or invalid name — re-emit verbatim and record it.
                let mut snippet = format!("${{{name}");
                if closed {
                    snippet.push('}');
                }
                out.push_str(&snippet);
                issues.malformed.insert(snippet);
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

fn walk_config(config: &mut Config, env: &Env, issues: &mut Issues) {
    walk_server(&mut config.server, env, issues);
    walk_defaults(&mut config.defaults, env, issues);
    for tool in &mut config.tools {
        walk_tool(tool, env, issues);
    }
}

fn walk_server(server: &mut Server, env: &Env, issues: &mut Issues) {
    istr(&mut server.name, env, issues);
    iopt(&mut server.version, env, issues);
    iopt(&mut server.instructions, env, issues);
}

fn walk_defaults(defaults: &mut Defaults, env: &Env, issues: &mut Issues) {
    iopt(&mut defaults.base_url, env, issues);
    imap(&mut defaults.headers, env, issues);
    if let Some(auth) = defaults.auth.as_mut() {
        walk_auth(auth, env, issues);
    }
}

fn walk_tool(tool: &mut Tool, env: &Env, issues: &mut Issues) {
    istr(&mut tool.name, env, issues);
    iopt(&mut tool.description, env, issues);
    // `input_schema` is intentionally not interpolated (verbatim to the harness).
    if let Some(http) = tool.http.as_mut() {
        walk_http(http, env, issues);
    }
    if let Some(cli) = tool.cli.as_mut() {
        walk_cli(cli, env, issues);
    }
}

fn walk_http(http: &mut HttpTarget, env: &Env, issues: &mut Issues) {
    istr(&mut http.path, env, issues);
    iopt(&mut http.base_url, env, issues);
    imap(&mut http.query, env, issues);
    imap(&mut http.headers, env, issues);
    imap(&mut http.body, env, issues);
    if let Some(value) = http.body_from.as_mut() {
        ivalue(value, env, issues);
    }
    if let Some(auth) = http.auth.as_mut() {
        walk_auth(auth, env, issues);
    }
}

fn walk_cli(cli: &mut CliTarget, env: &Env, issues: &mut Issues) {
    istr(&mut cli.command, env, issues);
    for arg in &mut cli.args {
        ivalue(arg, env, issues);
    }
    if let Some(stdin) = cli.stdin.as_mut() {
        ivalue(stdin, env, issues);
    }
    imap(&mut cli.env, env, issues);
    if let Some(cwd) = cli.cwd.as_mut() {
        ivalue(cwd, env, issues);
    }
}

/// Exhaustive so a new [`Auth`] variant forces an interpolation decision here.
fn walk_auth(auth: &mut Auth, env: &Env, issues: &mut Issues) {
    match auth {
        Auth::Bearer { token } => istr(token, env, issues),
        Auth::Basic { username, password } => {
            istr(username, env, issues);
            istr(password, env, issues);
        }
        Auth::ApiKey {
            location: _,
            name,
            value,
        } => {
            istr(name, env, issues);
            istr(value, env, issues);
        }
        Auth::Oauth2 {
            token_url,
            client_id,
            client_secret,
            scopes,
        } => {
            istr(token_url, env, issues);
            istr(client_id, env, issues);
            istr(client_secret, env, issues);
            for scope in scopes {
                istr(scope, env, issues);
            }
        }
    }
}

// --- leaf helpers -----------------------------------------------------------

fn istr(s: &mut String, env: &Env, issues: &mut Issues) {
    *s = interpolate_str(s, env, issues);
}

fn iopt(s: &mut Option<String>, env: &Env, issues: &mut Issues) {
    if let Some(s) = s.as_mut() {
        istr(s, env, issues);
    }
}

fn imap(map: &mut super::ValueMap, env: &Env, issues: &mut Issues) {
    for value in map.values_mut() {
        ivalue(value, env, issues);
    }
}

/// Recurse into a value expression, interpolating any string it contains.
fn ivalue(value: &mut Value, env: &Env, issues: &mut Issues) {
    match value {
        Value::String(s) => *s = interpolate_str(s, env, issues),
        Value::Array(items) => {
            for item in items {
                ivalue(item, env, issues);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                ivalue(v, env, issues);
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

    fn run(input: &str, env: &[(&str, &str)]) -> (String, Issues) {
        let mut issues = Issues::default();
        let out = interpolate_str(input, &env_from(env), &mut issues);
        (out, issues)
    }

    #[test]
    fn interpolate_str_replaces_and_preserves() {
        let env = &[("A", "1"), ("B", "two")];
        assert_eq!(run("${A}", env).0, "1");
        assert_eq!(run("x${A}-${B}y", env).0, "x1-twoy");
        // A bare `$` (no braces) is left intact — it's an input ref for the engine.
        assert_eq!(run("$A and ${A}", env).0, "$A and 1");
        assert_eq!(run("price is $5", env).0, "price is $5");
        assert_eq!(run("plain", env).0, "plain");
    }

    #[test]
    fn interpolate_str_records_malformed() {
        let env = &[("A", "1")];
        let (out, issues) = run("${1BAD} ${has space} ${unterminated", env);
        assert_eq!(out, "${1BAD} ${has space} ${unterminated");
        assert!(issues.missing.is_empty());
        assert_eq!(
            issues.malformed.into_iter().collect::<Vec<_>>(),
            vec!["${1BAD}", "${has space}", "${unterminated"]
        );
    }

    #[test]
    fn interpolate_str_records_missing_and_keeps_placeholder() {
        let (out, issues) = run("${X}-${A}-${Y}", &[("A", "1")]);
        assert_eq!(out, "${X}-1-${Y}");
        assert_eq!(
            issues.missing.into_iter().collect::<Vec<_>>(),
            vec!["X", "Y"]
        );
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

        // NOT_INTERPOLATED is absent from the env; interpolation succeeding proves the
        // inputSchema was skipped (else it would be reported missing).
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
