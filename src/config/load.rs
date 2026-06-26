//! Loading, defaults-merging, and structural validation of a [`Config`].
//!
//! `${ENV}` interpolation (task T4) and richer validation — JSON Schema checks,
//! value-reference sanity, include/exclude exclusivity (task T5) — layer on top of
//! this. Here we cover: read → parse (by extension) → merge defaults → enforce the
//! one structural invariant (each tool has exactly one of `http`/`cli`).

use std::path::Path;

use thiserror::Error;

use super::Config;

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

/// Enforce structural invariants that hold regardless of inputs.
///
/// Currently: each tool declares exactly one of `http`/`cli`. Task T5 extends this.
fn validate(config: &Config) -> Result<(), ConfigError> {
    for tool in &config.tools {
        tool.target()?;
    }
    Ok(())
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
