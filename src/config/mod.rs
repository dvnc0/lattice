//! The lattice configuration model.
//!
//! A config file (YAML or JSON) describes one MCP server and the tools it exposes.
//! These types are the deserialization target; loading, defaults-merging, and
//! validation live in [`load`]. Value-expression leaves (request bodies, query
//! params, CLI args, …) are kept as raw [`serde_json::Value`] here — they are
//! interpreted by the engine in later tasks (T6+), not parsed at config-load time.

mod interpolate;
mod load;

pub use load::{check, check_str, load_config, parse_config, CheckReport, ConfigError, Format};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object (used for hand-written JSON Schemas).
pub type JsonObject = Map<String, Value>;

/// A map of named value-expression leaves (headers, query, body, env).
///
/// Backed by a `BTreeMap` so parsing is order-independent and two configs that
/// differ only in key order compare equal.
pub type ValueMap = BTreeMap<String, Value>;

// ── skip_serializing_if helpers ──────────────────────────────────────────────

fn skip_empty_map(m: &ValueMap) -> bool {
    m.is_empty()
}

fn skip_empty_json_map(m: &JsonObject) -> bool {
    m.is_empty()
}

fn skip_parse_raw(p: &ParseMode) -> bool {
    matches!(p, ParseMode::Raw)
}

fn skip_response_empty(r: &ResponseSpec) -> bool {
    r.include.is_none() && r.exclude.is_none()
}

fn skip_defaults_empty(d: &Defaults) -> bool {
    d.base_url.is_none() && d.headers.is_empty() && d.auth.is_none()
}

// ── Config types ─────────────────────────────────────────────────────────────

/// A full lattice configuration: one MCP server and its tools.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Server identity and MCP-surface options.
    pub server: Server,
    /// Defaults inherited by all HTTP tools.
    #[serde(default, skip_serializing_if = "skip_defaults_empty")]
    pub defaults: Defaults,
    /// The tools this server exposes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

/// Server identity and how tools are surfaced to the harness.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Server name reported to the harness.
    pub name: String,
    /// Optional server version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Optional human-readable instructions surfaced at initialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// How routes are exposed (see [`ExposeMode`]).
    #[serde(default)]
    pub expose: ExposeMode,
}

/// The MCP-surface strategy for a server's routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposeMode {
    /// Every route is a first-class MCP tool with its schema in `tools/list`.
    #[default]
    Tools,
    /// Routes hide behind `describe_route` + `call_route` (for large APIs).
    Dispatcher,
}

/// Defaults inherited by all HTTP tools (CLI tools ignore these).
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Base URL prefixed to each HTTP tool's `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Headers applied to every HTTP request (tool headers override per-key).
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub headers: ValueMap,
    /// Authentication applied to every HTTP request unless a tool overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

/// Authentication strategy, selected by the `type` field.
///
/// `Debug` is hand-written (below) to redact secret-bearing fields, so credentials
/// never reach logs once `${ENV}` interpolation (T4) fills in real values.
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
    /// HTTP Basic auth.
    Basic { username: String, password: String },
    /// A static API key placed in a header or query parameter.
    ApiKey {
        /// Where to place the key.
        #[serde(rename = "in", default)]
        location: ApiKeyLocation,
        /// Header or query-parameter name.
        name: String,
        /// The key value (typically `${ENV}`).
        value: String,
    },
    /// OAuth2 client-credentials grant (token fetched, cached, refreshed).
    Oauth2 {
        token_url: String,
        client_id: String,
        client_secret: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
    },
}

impl std::fmt::Debug for Auth {
    /// Redacts secret-bearing fields so credentials never reach logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const REDACTED: &str = "***";
        match self {
            Auth::Bearer { .. } => f.debug_struct("Bearer").field("token", &REDACTED).finish(),
            Auth::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &REDACTED)
                .finish(),
            Auth::ApiKey { location, name, .. } => f
                .debug_struct("ApiKey")
                .field("location", location)
                .field("name", name)
                .field("value", &REDACTED)
                .finish(),
            Auth::Oauth2 {
                token_url,
                client_id,
                scopes,
                ..
            } => f
                .debug_struct("Oauth2")
                .field("token_url", token_url)
                .field("client_id", client_id)
                .field("client_secret", &REDACTED)
                .field("scopes", scopes)
                .finish(),
        }
    }
}

/// Where an API key is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    /// In a request header.
    #[default]
    Header,
    /// In a query parameter.
    Query,
}

/// A single MCP tool, backed by exactly one of an HTTP request or a CLI command.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    /// Tool name as seen by the harness.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Hand-written JSON Schema, passed verbatim to `tools/list`.
    #[serde(
        rename = "inputSchema",
        default,
        skip_serializing_if = "skip_empty_json_map"
    )]
    pub input_schema: JsonObject,
    /// HTTP backing (mutually exclusive with [`Tool::cli`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpTarget>,
    /// CLI backing (mutually exclusive with [`Tool::http`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliTarget>,
}

/// The resolved backing of a tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target<'a> {
    Http(&'a HttpTarget),
    Cli(&'a CliTarget),
}

impl Tool {
    /// Return the tool's single backing target, or an error if it declares both
    /// or neither of `http`/`cli`.
    pub fn target(&self) -> Result<Target<'_>, ConfigError> {
        match (&self.http, &self.cli) {
            (Some(http), None) => Ok(Target::Http(http)),
            (None, Some(cli)) => Ok(Target::Cli(cli)),
            (Some(_), Some(_)) => Err(ConfigError::Validation(format!(
                "tool '{}' declares both `http` and `cli`; exactly one is required",
                self.name
            ))),
            (None, None) => Err(ConfigError::Validation(format!(
                "tool '{}' declares neither `http` nor `cli`; exactly one is required",
                self.name
            ))),
        }
    }
}

/// An HTTP-backed tool.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTarget {
    /// HTTP method (e.g. `GET`, `POST`).
    pub method: String,
    /// Request path; may contain `{var}` placeholders filled from input.
    pub path: String,
    /// Base URL override (otherwise inherited from [`Defaults::base_url`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Query parameters: name → value expression.
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub query: ValueMap,
    /// Headers: name → value expression (merged over [`Defaults::headers`]).
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub headers: ValueMap,
    /// Request body: dotted target path → value expression.
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub body: ValueMap,
    /// Send a single referenced value as the entire body (passthrough).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_from: Option<Value>,
    /// Per-tool auth override (otherwise inherited from [`Defaults::auth`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    /// Response filtering.
    #[serde(default, skip_serializing_if = "skip_response_empty")]
    pub response: ResponseSpec,
}

/// A CLI-backed tool.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliTarget {
    /// Program to execute (run directly, never via a shell).
    pub command: String,
    /// Arguments: ordered list of value expressions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Value>,
    /// Optional standard input (a value expression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<Value>,
    /// Environment variables: name → value expression.
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub env: ValueMap,
    /// Working directory (a value expression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<Value>,
    /// How to interpret the command's stdout.
    #[serde(default, skip_serializing_if = "skip_parse_raw")]
    pub parse: ParseMode,
    /// Response filtering (applies when `parse` yields JSON).
    #[serde(default, skip_serializing_if = "skip_response_empty")]
    pub response: ResponseSpec,
}

/// How to interpret a CLI command's stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseMode {
    /// Return stdout verbatim as text.
    #[default]
    Raw,
    /// Parse stdout as a single JSON value.
    Json,
    /// Split stdout into an array of lines.
    Lines,
}

/// Response field filtering. `include` and `exclude` are mutually exclusive
/// (enforced during validation, task T5).
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseSpec {
    /// Keep only these dotted field paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    /// Drop these dotted field paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

// ── Round-trip test ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::config::load::{parse_config, Format};

    #[test]
    fn config_serialize_roundtrip() {
        let cases = [
            ("examples/httpbin.yaml", Format::Yaml),
            ("examples/github.yaml", Format::Yaml),
            ("examples/ls.yaml", Format::Yaml),
        ];
        for (path, format) in cases {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
            let original = parse_config(&text, format)
                .unwrap_or_else(|e| panic!("failed to parse {path}: {e}"));
            let yaml = serde_norway::to_string(&original)
                .unwrap_or_else(|e| panic!("failed to serialize {path}: {e}"));
            let reparsed = parse_config(&yaml, Format::Yaml)
                .unwrap_or_else(|e| panic!("failed to re-parse {path}: {e}"));
            assert_eq!(original, reparsed, "round-trip failed for {path}");
        }
    }
}
