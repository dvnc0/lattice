//! Execution layer: run the engine's pure specs against real I/O.
//!
//! The engine ([`crate::engine`]) is pure — it turns config + a call's input into an
//! [`HttpRequestSpec`](crate::engine::HttpRequestSpec) or
//! [`CommandSpec`](crate::engine::CommandSpec). This module *runs* those specs: [`http`]
//! sends HTTP requests via `reqwest` (T11), with auth (T12) and the CLI executor (T13)
//! landing alongside it.
//!
//! Both executors converge on a [`ToolOutcome`]: the (response-filtered) result value plus
//! an `is_error` flag. A non-success HTTP status / non-zero exit is **not** a transport
//! error — it is a normal outcome with `is_error: true` so the MCP layer can surface it as
//! `CallToolResult { is_error: true }` and the model can react. [`ExecError`] is reserved
//! for genuine I/O failures (couldn't reach the server, a malformed request, a serialize
//! failure) that produce no usable response at all.

pub mod http;

use serde_json::Value;
use thiserror::Error;

/// The result of executing a tool: a filtered value and whether it represents an error
/// the model should see (non-2xx HTTP / non-zero CLI exit).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// `true` when the underlying call failed (non-success status / non-zero exit). The
    /// `value` is still populated (the filtered error body) so the model can react.
    pub is_error: bool,
    /// The response value after [`response`](crate::engine::response) filtering.
    pub value: Value,
}

/// A genuine execution failure that yields no usable response.
///
/// `Display`/`Debug` are deliberately scrubbed of request URLs and bodies so an
/// interpolated `${ENV}` secret riding in a query/header/path can't leak into logs.
#[derive(Debug, Error)]
pub enum ExecError {
    /// The configured HTTP method was not a valid method token.
    #[error("invalid HTTP method '{0}'")]
    InvalidMethod(String),
    /// A header name or value was not valid (e.g. control characters / CRLF).
    #[error("invalid header '{0}'")]
    InvalidHeader(String),
    /// The request body could not be serialized.
    #[error("failed to serialize request body: {0}")]
    Body(String),
    /// The response body exceeded the maximum size we will buffer.
    #[error("response body exceeded the {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    /// The HTTP request could not be completed (DNS/connect/timeout/transport). The
    /// message is taken from `reqwest` with the URL stripped to avoid leaking secrets.
    #[error("HTTP request failed: {0}")]
    Request(String),
}
