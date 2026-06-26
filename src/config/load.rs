//! Loading, defaults-merging, and validation of a [`Config`].
//!
//! `load_config` does: read → parse (by extension) → `${ENV}` interpolation → merge
//! defaults → enforce the structural invariant (each tool has exactly one of
//! `http`/`cli`). The `check`/`check_str` functions add the preflight validations used
//! by `lattice check`: JSON Schema validity, include/exclude exclusivity, `body` vs
//! `body_from`, unknown `auth` keys, missing env, and malformed-`${...}` warnings.

use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use super::{Config, ExposeMode, ResponseSpec};

/// Errors raised while loading or validating a config.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("failed to read config {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    /// The file extension is not `.yaml`, `.yml`, or `.json`.
    #[error("unsupported config extension for {path} (use .yaml, .yml, or .json)")]
    UnknownFormat { path: String },
    /// YAML failed to parse.
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_norway::Error),
    /// JSON failed to parse.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    /// The config parsed but is structurally invalid.
    #[error("invalid config: {0}")]
    Validation(String),
    /// One or more `${ENV}` references could not be resolved.
    #[error("missing environment variable(s): {}", .0.join(", "))]
    MissingEnv(Vec<String>),
}

/// The serialization format of a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
}

/// Load, merge defaults into, and validate a config from a file path.
///
/// The format is chosen by file extension.
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let format = format_from_path(path)?;
    let mut config = parse_config(&text, format)?;
    super::interpolate::interpolate(&mut config).map_err(ConfigError::MissingEnv)?;
    apply_defaults(&mut config);
    validate(&config)?;
    Ok(config)
}

/// Parse a config from text in the given format (no defaults-merge or validation).
pub fn parse_config(text: &str, format: Format) -> Result<Config, ConfigError> {
    match format {
        Format::Yaml => Ok(serde_norway::from_str(text)?),
        Format::Json => Ok(serde_json::from_str(text)?),
    }
}

/// Determine the format from a path's extension.
fn format_from_path(path: &Path) -> Result<Format, ConfigError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("yaml") | Some("yml") => Ok(Format::Yaml),
        Some("json") => Ok(Format::Json),
        _ => Err(ConfigError::UnknownFormat {
            path: path.display().to_string(),
        }),
    }
}

/// Merge server [`Defaults`](super::Defaults) into each HTTP tool: base URL and
/// auth fill in where the tool left them unset; default headers are applied first
/// and overridden per-key by the tool's own headers. CLI tools are untouched.
fn apply_defaults(config: &mut Config) {
    let defaults = config.defaults.clone();
    for tool in &mut config.tools {
        let Some(http) = tool.http.as_mut() else {
            continue;
        };
        if http.base_url.is_none() {
            http.base_url = defaults.base_url.clone();
        }
        if http.auth.is_none() {
            http.auth = defaults.auth.clone();
        }
        if !defaults.headers.is_empty() {
            // Start from the defaults, then let the tool's headers win per-key.
            let mut merged = defaults.headers.clone();
            merged.append(&mut http.headers);
            http.headers = merged;
        }
    }
}

/// Enforce the structural invariant used at serve time: each tool declares exactly one
/// of `http`/`cli`. (The richer preflight checks live in [`check_str`].)
fn validate(config: &Config) -> Result<(), ConfigError> {
    for tool in &config.tools {
        tool.target()?;
    }
    Ok(())
}

/// The result of validating a config without serving it (the `check` command).
#[derive(Debug)]
pub struct CheckReport {
    /// Number of tools declared.
    pub tool_count: usize,
    /// The configured expose mode.
    pub expose: ExposeMode,
    /// Problems that make the config invalid.
    pub errors: Vec<String>,
    /// Non-fatal advisories.
    pub warnings: Vec<String>,
}

impl CheckReport {
    /// Whether the config is free of errors (warnings don't count).
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a config file without serving it: parse, structural checks, JSON Schema
/// validity, `${ENV}` resolution, and advisories. Returns a [`CheckReport`]; only a
/// read/format/parse failure (which prevents any analysis) is surfaced as `Err`.
pub fn check(path: &Path) -> Result<CheckReport, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let format = format_from_path(path)?;
    check_str(&text, format)
}

/// As [`check`], on already-loaded text.
pub fn check_str(text: &str, format: Format) -> Result<CheckReport, ConfigError> {
    let mut config = parse_config(text, format)?;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if config.tools.is_empty() {
        warnings.push("config declares no tools".to_string());
    }
    validate_structure(&config, &mut errors);
    validate_schemas(&config, &mut errors, &mut warnings);
    check_auth_keys(text, format, &mut errors);

    // Resolve ${ENV} against the process environment, collecting issues rather than
    // bailing so the report covers everything at once.
    let issues = super::interpolate::collect_issues(&mut config);
    for var in &issues.missing {
        errors.push(format!("missing environment variable: ${{{var}}}"));
    }
    for placeholder in &issues.malformed {
        warnings.push(format!("malformed reference left as-is: {placeholder}"));
    }

    Ok(CheckReport {
        tool_count: config.tools.len(),
        expose: config.server.expose,
        errors,
        warnings,
    })
}

/// Structural checks that don't depend on the environment.
fn validate_structure(config: &Config, errors: &mut Vec<String>) {
    for tool in &config.tools {
        match tool.target() {
            Ok(_) => {}
            Err(ConfigError::Validation(msg)) => errors.push(msg),
            Err(other) => errors.push(other.to_string()),
        }
        if let Some(http) = &tool.http {
            check_response(&tool.name, &http.response, errors);
            if !http.body.is_empty() && http.body_from.is_some() {
                errors.push(format!(
                    "tool '{}': set either `body` or `body_from`, not both",
                    tool.name
                ));
            }
        }
        if let Some(cli) = &tool.cli {
            check_response(&tool.name, &cli.response, errors);
        }
    }
}

fn check_response(tool: &str, response: &ResponseSpec, errors: &mut Vec<String>) {
    if response.include.is_some() && response.exclude.is_some() {
        errors.push(format!(
            "tool '{tool}': response sets both `include` and `exclude`; choose one"
        ));
    }
}

/// Validate that each tool's `inputSchema` compiles as a JSON Schema, warning when absent.
///
/// This proves the schema is *compilable*, not meta-valid: unknown keyword names are
/// valid JSON Schema and silently ignored, so a typo like `requird` is not caught here.
fn validate_schemas(config: &Config, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    for tool in &config.tools {
        if tool.input_schema.is_empty() {
            warnings.push(format!(
                "tool '{}': no inputSchema (the harness will see no argument schema)",
                tool.name
            ));
            continue;
        }
        let schema = Value::Object(tool.input_schema.clone());
        if let Err(err) = jsonschema::validator_for(&schema) {
            errors.push(format!("tool '{}': invalid inputSchema: {err}", tool.name));
        }
    }
}

/// Catch unknown keys in `auth` blocks, which the internally-tagged `Auth` enum drops
/// silently (serde forbids `deny_unknown_fields` on tagged enums). Operates on the raw
/// document since the typed parse has already discarded the extras.
fn check_auth_keys(text: &str, format: Format, errors: &mut Vec<String>) {
    // Re-parse the raw document generically to inspect auth keys. A parse failure can't
    // happen here (the typed parse in check_str already succeeded) but is skipped
    // defensively rather than unwrapped.
    let value: Value = match format {
        Format::Yaml => match serde_norway::from_str(text) {
            Ok(value) => value,
            Err(_) => return,
        },
        Format::Json => match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => return,
        },
    };

    if let Some(auth) = value
        .get("defaults")
        .and_then(|defaults| defaults.get("auth"))
    {
        check_one_auth(auth, "defaults.auth", errors);
    }
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for (index, tool) in tools.iter().enumerate() {
            let label = tool
                .get("name")
                .and_then(Value::as_str)
                .map(|name| format!("tool '{name}'"))
                .unwrap_or_else(|| format!("tools[{index}]"));
            if let Some(auth) = tool.get("http").and_then(|http| http.get("auth")) {
                check_one_auth(auth, &format!("{label} http.auth"), errors);
            }
        }
    }
}

fn check_one_auth(auth: &Value, location: &str, errors: &mut Vec<String>) {
    let Some(object) = auth.as_object() else {
        return;
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return; // a missing/invalid `type` is already a typed-parse error
    };
    let allowed: &[&str] = match kind {
        "bearer" => &["type", "token"],
        "basic" => &["type", "username", "password"],
        "api_key" => &["type", "in", "name", "value"],
        "oauth2" => &["type", "token_url", "client_id", "client_secret", "scopes"],
        _ => return, // an unknown type is already a typed-parse error
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(format!(
                "{location}: unknown field '{key}' for auth type '{kind}'"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> Result<Config, ConfigError> {
        let mut config = parse_config(yaml, Format::Yaml)?;
        apply_defaults(&mut config);
        validate(&config)?;
        Ok(config)
    }

    #[test]
    fn rejects_tool_with_both_targets() {
        let err = cfg(r#"
server: { name: s }
tools:
  - name: bad
    http: { method: GET, path: /x }
    cli: { command: ls }
"#)
        .unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn rejects_tool_with_no_target() {
        let err = cfg(r#"
server: { name: s }
tools:
  - name: bad
"#)
        .unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn rejects_unknown_field() {
        let err = parse_config("server: { name: s }\nbogus: 1\n", Format::Yaml).unwrap_err();
        assert!(matches!(err, ConfigError::Yaml(_)));
    }

    #[test]
    fn tool_values_override_defaults() {
        let config = cfg(r#"
server: { name: s }
defaults:
  base_url: "https://default.example.com"
  headers: { Accept: "application/json", X-Trace: "on" }
tools:
  - name: t
    http:
      method: GET
      path: /x
      base_url: "https://tool.example.com"
      headers: { Accept: "text/plain" }
"#)
        .unwrap();
        let http = config.tools[0].http.as_ref().unwrap();
        // Tool-level values win over the defaults...
        assert_eq!(http.base_url.as_deref(), Some("https://tool.example.com"));
        assert_eq!(http.headers["Accept"], serde_json::json!("text/plain"));
        // ...while non-overridden default headers are still merged in.
        assert_eq!(http.headers["X-Trace"], serde_json::json!("on"));
    }

    #[test]
    fn debug_redacts_auth_secrets() {
        let config = cfg(r#"
server: { name: s }
defaults:
  auth: { type: bearer, token: "super-secret-token" }
tools: []
"#)
        .unwrap();
        let auth = config.defaults.auth.as_ref().unwrap();
        let rendered = format!("{auth:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "secret leaked into Debug output: {rendered}"
        );
        assert!(
            rendered.contains("***"),
            "expected redaction marker in: {rendered}"
        );
    }
}
