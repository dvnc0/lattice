//! The lattice [`ServerHandler`] — config-driven **tools mode** (task T14).
//!
//! `list_tools` returns one MCP tool per configured tool, each carrying its authored
//! `inputSchema` **verbatim**. `call_tool` looks the tool up by name, resolves the call's
//! arguments through the pure engine ([`crate::engine`]) into a request/command spec, runs
//! it through the executor ([`crate::exec`]), and maps the [`ToolOutcome`] into a
//! [`CallToolResult`](rmcp::model::CallToolResult).
//!
//! Every failure the model could act on is surfaced as an `isError` **result**, never a
//! protocol error: a non-2xx HTTP status / non-zero CLI exit (the executor's own
//! `is_error`), a build failure from bad input, or a genuine execution failure. Messages
//! are scrubbed of anything that could echo an interpolated `${ENV}` secret (template
//! sources in particular — see [`safe_value_error`]).
//!
//! The dispatcher expose mode (`describe_route`/`call_route`) lands in T16; this handler
//! always serves tools mode and logs a warning if a config requests dispatcher.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{Map, Value};

use super::result;
use crate::config::{Config, ExposeMode, HttpTarget, Server, Target, Tool as ConfigTool};
use crate::engine::{
    build_command, build_request, BodyError, CommandError, Ctx, RequestError, ValueError,
};
use crate::exec::auth::AuthState;
use crate::exec::{self, ToolOutcome};

/// How long to wait for a TCP connection before giving up. Redirects are disabled and the
/// per-request wall-clock cap lives in the executor; this bounds DNS/connect setup.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A configured tool, prepared once at startup for fast per-call dispatch.
struct PreparedTool {
    /// The MCP descriptor returned by `tools/list` (authored `inputSchema`, verbatim).
    descriptor: Tool,
    /// The config backing used to build the request/command on each call.
    config: ConfigTool,
    /// Per-tool auth state (HTTP tools only), created once so an OAuth token cache
    /// survives across calls to this tool.
    auth: Option<AuthState>,
}

/// Immutable server state, shared behind an [`Arc`] so the handler clones cheaply (and the
/// per-tool [`AuthState`], which is not `Clone`, is shared rather than duplicated).
struct ServerInner {
    info: ServerInfo,
    tools: Vec<PreparedTool>,
    by_name: HashMap<String, usize>,
    client: reqwest::Client,
}

/// The lattice MCP server handler.
#[derive(Clone)]
pub struct LatticeServer {
    inner: Arc<ServerInner>,
}

impl LatticeServer {
    /// Build a server from a loaded, defaults-merged [`Config`].
    pub fn new(config: Config) -> Self {
        if config.server.expose == ExposeMode::Dispatcher {
            tracing::warn!(
                "expose: dispatcher is not implemented yet (task T16); serving tools mode"
            );
        }

        let info = server_info(&config.server);
        let client = build_client();

        let mut tools = Vec::with_capacity(config.tools.len());
        let mut by_name = HashMap::with_capacity(config.tools.len());
        for tool in config.tools {
            // Build auth state once per tool so its OAuth token cache persists across calls.
            let auth = match tool.target() {
                Ok(Target::Http(http)) => {
                    warn_if_cleartext_auth(&tool.name, http);
                    http.auth.clone().map(AuthState::new)
                }
                _ => None,
            };
            let descriptor = descriptor(&tool);
            if by_name.insert(tool.name.clone(), tools.len()).is_some() {
                // MCP tool names must be unique; the later definition shadows the earlier.
                tracing::warn!(tool = %tool.name, "duplicate tool name; the later definition wins");
            }
            tools.push(PreparedTool {
                descriptor,
                config: tool,
                auth,
            });
        }

        Self {
            inner: Arc::new(ServerInner {
                info,
                tools,
                by_name,
                client,
            }),
        }
    }

    /// Resolve and run a tool call, returning either an outcome or a model-safe error
    /// message (engine build failure or genuine execution failure).
    async fn dispatch(
        &self,
        prepared: &PreparedTool,
        ctx: &Ctx<'_>,
    ) -> Result<ToolOutcome, String> {
        match prepared.config.target() {
            Ok(Target::Http(http)) => {
                let spec = build_request(http, ctx).map_err(|err| safe_request_error(&err))?;
                exec::http::execute(
                    &self.inner.client,
                    &spec,
                    &http.response,
                    prepared.auth.as_ref(),
                )
                .await
                .map_err(|err| err.to_string())
            }
            Ok(Target::Cli(cli)) => {
                let spec = build_command(cli, ctx).map_err(|err| safe_command_error(&err))?;
                exec::cli::execute(&spec, cli.parse, &cli.response)
                    .await
                    .map_err(|err| err.to_string())
            }
            // Exactly-one-of http/cli is validated at load; this arm is defensive. The
            // `ConfigError` message names only the tool, never a secret.
            Err(err) => Err(err.to_string()),
        }
    }
}

impl ServerHandler for LatticeServer {
    fn get_info(&self) -> ServerInfo {
        self.inner.info.clone()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .inner
            .tools
            .iter()
            .map(|tool| tool.descriptor.clone())
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Unknown tool → an error *result* (not a protocol error) so the model can react.
        let Some(&index) = self.inner.by_name.get(request.name.as_ref()) else {
            return Ok(result::error_result(format!(
                "unknown tool: {}",
                request.name
            )));
        };
        let prepared = &self.inner.tools[index];

        // MCP arguments are an optional JSON object; the engine resolves refs against it.
        let input = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Map::new()));
        let ctx = Ctx::new(&input);

        Ok(match self.dispatch(prepared, &ctx).await {
            Ok(outcome) => result::outcome_to_result(outcome),
            Err(message) => {
                tracing::warn!(tool = %prepared.descriptor.name, "tool call failed: {message}");
                result::error_result(message)
            }
        })
    }
}

/// Build the [`ServerInfo`] reported at initialization from the config's `server` block.
fn server_info(server: &Server) -> ServerInfo {
    let version = server
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(server.name.clone(), version));
    if let Some(instructions) = &server.instructions {
        info = info.with_instructions(instructions.clone());
    }
    info
}

/// Build the MCP tool descriptor for `tools/list`, carrying the authored schema verbatim.
fn descriptor(tool: &ConfigTool) -> Tool {
    let schema = Arc::new(tool.input_schema.clone());
    // `Tool::new` requires a description; restore `None` when the config omits one rather
    // than advertising an empty string.
    let mut descriptor = Tool::new(
        tool.name.clone(),
        tool.description.clone().unwrap_or_default(),
        schema,
    );
    if tool.description.is_none() {
        descriptor.description = None;
    }
    descriptor
}

/// Build the production reqwest client. Redirects are **disabled** (a hostile upstream
/// could 302 to an internal host, and reqwest doesn't strip custom auth headers across
/// hosts), and a connect timeout bounds DNS/connect; per-request timeouts live in the
/// executor.
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        // Only fails if the TLS backend can't initialize — a fatal startup condition,
        // mirroring reqwest's own `Client::new()` contract.
        .expect("failed to build HTTP client")
}

/// Warn (once, at startup) when a tool would send an auth credential over cleartext HTTP.
fn warn_if_cleartext_auth(name: &str, http: &HttpTarget) {
    if http.auth.is_none() {
        return;
    }
    // Best-effort: inspect the effective origin (base URL, else the path if it's a full URL).
    let origin = http.base_url.as_deref().unwrap_or(&http.path);
    if origin
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("http://")
    {
        tracing::warn!(tool = %name, "auth credential will be sent over cleartext http://");
    }
}

/// A model- and log-safe message for an HTTP request build failure. Field/path names are
/// safe (and useful) to surface; a template error is scrubbed (see [`safe_value_error`]).
fn safe_request_error(err: &RequestError) -> String {
    match err {
        RequestError::Value(value) => safe_value_error(value),
        RequestError::Body(BodyError::Value(value)) => safe_value_error(value),
        // `Body(PathConflict)` and `NonScalar` carry only config/field names.
        other => other.to_string(),
    }
}

/// A model- and log-safe message for a CLI command build failure (see [`safe_request_error`]).
fn safe_command_error(err: &CommandError) -> String {
    match err {
        CommandError::Value(value) => safe_value_error(value),
        // `NonScalar` carries only the location label.
        other => other.to_string(),
    }
}

/// Scrub a [`ValueError`]: a `Template` message can echo the (already `${ENV}`-interpolated)
/// template source, so it is replaced with a generic message; every other variant carries
/// only input field / path names, which are safe and useful to the model.
fn safe_value_error(err: &ValueError) -> String {
    match err {
        ValueError::Template(_) => "a template expression failed to render".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ValueError;

    #[test]
    fn template_value_error_is_scrubbed() {
        // A template error could echo interpolated secrets — must not be surfaced verbatim.
        let leaked = ValueError::Template("Bearer sk-secret-123 is invalid".to_string());
        let msg = safe_value_error(&leaked);
        assert!(
            !msg.contains("sk-secret-123"),
            "template error leaked: {msg}"
        );
    }

    #[test]
    fn field_name_errors_are_surfaced() {
        let err = ValueError::MissingPathVar("userId".to_string());
        assert!(safe_value_error(&err).contains("userId"));
    }

    #[test]
    fn cleartext_origin_detection_is_scheme_only() {
        // Sanity-check the substring guard used by `warn_if_cleartext_auth`.
        assert!("http://api.local"
            .to_ascii_lowercase()
            .starts_with("http://"));
        assert!(!"https://api.local"
            .to_ascii_lowercase()
            .starts_with("http://"));
    }
}
