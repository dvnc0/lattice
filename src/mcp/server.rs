//! The lattice [`ServerHandler`] — config-driven **tools mode** (task T14).
//!
//! `list_tools` returns one MCP tool per configured tool, each carrying its authored
//! `inputSchema` **verbatim**. `call_tool` looks the tool up by name, **validates the
//! arguments against that schema before doing anything else** (T17 — a violation is an
//! error result and nothing is executed), then resolves the arguments through the pure
//! engine ([`crate::engine`]) into a request/command spec, runs it through the executor
//! ([`crate::exec`]), and maps the [`ToolOutcome`] into a
//! [`CallToolResult`](rmcp::model::CallToolResult).
//!
//! Every failure the model could act on is surfaced as an `isError` **result**, never a
//! protocol error: a non-2xx HTTP status / non-zero CLI exit (the executor's own
//! `is_error`), a build failure from bad input, or a genuine execution failure. Messages
//! are scrubbed of anything that could echo an interpolated `${ENV}` secret (template
//! sources in particular — see [`safe_value_error`]).
//!
//! Two expose modes share the same route machinery (and the identical engine path): **tools
//! mode** lists every route as a first-class tool, while **dispatcher mode** (T16) lists only
//! `describe_route` + `call_route` and embeds an auto-generated route catalog in the server
//! instructions — see [`dispatcher`](super::dispatcher).

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

use super::{dispatcher, result};
use crate::config::{Config, ExposeMode, HttpTarget, Server, Target, Tool as ConfigTool};
use crate::engine::{
    build_command, build_request, BodyError, CommandError, Ctx, InputSchema, RequestError,
    ValueError,
};
use crate::exec::auth::AuthState;
use crate::exec::{self, ToolOutcome};

/// How long to wait for a TCP connection before giving up. Redirects are disabled and the
/// per-request wall-clock cap lives in the executor; this bounds DNS/connect setup.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A JSON object, matching `rmcp`'s tool-argument type (`serde_json::Map<String, Value>`).
type JsonObject = Map<String, Value>;

/// A configured tool, prepared once at startup for fast per-call dispatch.
struct PreparedTool {
    /// The MCP descriptor returned by `tools/list` (authored `inputSchema`, verbatim).
    descriptor: Tool,
    /// The config backing used to build the request/command on each call.
    config: ConfigTool,
    /// The compiled `inputSchema`, validated against call arguments before execution.
    /// `None` when the tool authored no schema, or its schema failed to compile (in which
    /// case validation is skipped and a warning was logged at startup).
    schema: Option<InputSchema>,
    /// Per-tool auth state (HTTP tools only), created once so an OAuth token cache
    /// survives across calls to this tool.
    auth: Option<AuthState>,
}

/// Immutable server state, shared behind an [`Arc`] so the handler clones cheaply (and the
/// per-tool [`AuthState`], which is not `Clone`, is shared rather than duplicated).
struct ServerInner {
    info: ServerInfo,
    /// How tools are surfaced: every route as a tool, or the two dispatcher tools.
    mode: ExposeMode,
    /// What `tools/list` returns — the per-route descriptors (tools mode) or
    /// `describe_route` + `call_route` (dispatcher mode).
    listed: Vec<Tool>,
    /// The actual routes (dispatch targets), looked up by name in both modes.
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
        let mode = config.server.expose;
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
            let schema = compile_schema(&tool);
            if by_name.insert(tool.name.clone(), tools.len()).is_some() {
                // MCP tool names must be unique; the later definition shadows the earlier.
                tracing::warn!(tool = %tool.name, "duplicate tool name; the later definition wins");
            }
            tools.push(PreparedTool {
                descriptor,
                config: tool,
                schema,
                auth,
            });
        }

        // The expose mode shapes the listed tools and the server instructions; the routes
        // themselves (and their dispatch path) are identical across modes.
        let (listed, info) = build_surface(mode, &config.server, &tools);

        Self {
            inner: Arc::new(ServerInner {
                info,
                mode,
                listed,
                tools,
                by_name,
                client,
            }),
        }
    }

    /// Validate `input` against a route's schema, then translate + execute it, mapping the
    /// outcome (or a model-safe failure message) into a [`CallToolResult`]. Shared by tools
    /// mode (`call_tool`) and dispatcher mode (`call_route`).
    async fn invoke(&self, prepared: &PreparedTool, input: Value) -> CallToolResult {
        // Validate against the inputSchema *before* building or running anything: a
        // violation is an error result listing every problem, and nothing is executed.
        if let Some(schema) = &prepared.schema {
            let violations = schema.validate(&input);
            if !violations.is_empty() {
                return result::error_result(validation_failure(
                    &prepared.descriptor.name,
                    &violations,
                ));
            }
        }

        let ctx = Ctx::new(&input);
        match self.dispatch(prepared, &ctx).await {
            Ok(outcome) => result::outcome_to_result(outcome),
            Err(message) => {
                tracing::warn!(tool = %prepared.descriptor.name, "tool call failed: {message}");
                result::error_result(message)
            }
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

    /// Tools mode: dispatch a call to the route named by the request.
    async fn call_named(&self, request: CallToolRequestParams) -> CallToolResult {
        let Some(&index) = self.inner.by_name.get(request.name.as_ref()) else {
            return result::error_result(format!("unknown tool: {}", request.name));
        };
        let input = arguments_to_input(request.arguments);
        self.invoke(&self.inner.tools[index], input).await
    }

    /// Dispatcher mode: handle the two synthetic tools, `describe_route` and `call_route`.
    async fn call_dispatcher(&self, request: CallToolRequestParams) -> CallToolResult {
        let arguments = request.arguments;
        match request.name.as_ref() {
            dispatcher::DESCRIBE_ROUTE => self.describe_route(arguments.as_ref()),
            dispatcher::CALL_ROUTE => self.call_route(arguments).await,
            other => result::error_result(format!(
                "unknown tool '{other}'; this server exposes only '{}' and '{}'",
                dispatcher::DESCRIBE_ROUTE,
                dispatcher::CALL_ROUTE
            )),
        }
    }

    /// `describe_route(route)` → the route's name, description, and authored input schema.
    fn describe_route(&self, arguments: Option<&JsonObject>) -> CallToolResult {
        let route = match dispatcher::route_arg(arguments) {
            Ok(route) => route,
            Err(message) => return result::error_result(message),
        };
        let Some(&index) = self.inner.by_name.get(route.as_str()) else {
            return result::error_result(format!("unknown route: {route}"));
        };
        let detail = dispatcher::route_detail(&self.inner.tools[index].descriptor);
        result::outcome_to_result(ToolOutcome {
            is_error: false,
            value: detail,
        })
    }

    /// `call_route(route, params)` → validate params against the route schema, then execute
    /// it on the identical engine path as tools mode.
    async fn call_route(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let (route, params) = match dispatcher::call_args(arguments) {
            Ok(parsed) => parsed,
            Err(message) => return result::error_result(message),
        };
        let Some(&index) = self.inner.by_name.get(route.as_str()) else {
            return result::error_result(format!("unknown route: {route}"));
        };
        self.invoke(&self.inner.tools[index], params).await
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
        // Precomputed at startup: per-route descriptors (tools mode) or the two dispatcher
        // tools (dispatcher mode).
        Ok(ListToolsResult::with_all_items(self.inner.listed.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        Ok(match self.inner.mode {
            ExposeMode::Tools => self.call_named(request).await,
            ExposeMode::Dispatcher => self.call_dispatcher(request).await,
        })
    }
}

/// Build the listed tools and [`ServerInfo`] for the chosen expose mode. Tools mode lists
/// the per-route descriptors and keeps the author's instructions; dispatcher mode lists the
/// two synthetic tools and embeds an auto-generated route catalog into the instructions
/// (unless the author supplied their own).
fn build_surface(
    mode: ExposeMode,
    server: &Server,
    tools: &[PreparedTool],
) -> (Vec<Tool>, ServerInfo) {
    match mode {
        ExposeMode::Tools => {
            let listed = tools.iter().map(|tool| tool.descriptor.clone()).collect();
            (listed, server_info(server, server.instructions.clone()))
        }
        ExposeMode::Dispatcher => {
            let catalog = dispatcher::build_catalog(tools.iter().map(|tool| {
                (
                    tool.descriptor.name.as_ref(),
                    tool.descriptor.description.as_deref(),
                )
            }));
            let listed = vec![
                dispatcher::describe_route_descriptor(),
                dispatcher::call_route_descriptor(&catalog),
            ];
            // An author's `instructions` override the auto-generated dispatcher guide.
            let instructions = server
                .instructions
                .clone()
                .unwrap_or_else(|| dispatcher::dispatcher_instructions(&catalog));
            (listed, server_info(server, Some(instructions)))
        }
    }
}

/// Build the [`ServerInfo`] reported at initialization, with the given instructions.
fn server_info(server: &Server, instructions: Option<String>) -> ServerInfo {
    let version = server
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(server.name.clone(), version));
    if let Some(instructions) = instructions {
        info = info.with_instructions(instructions);
    }
    info
}

/// Convert optional MCP call arguments into the engine's input value (an empty object when
/// absent). The engine resolves `$ref`s against this object.
fn arguments_to_input(arguments: Option<JsonObject>) -> Value {
    arguments
        .map(Value::Object)
        .unwrap_or_else(|| Value::Object(Map::new()))
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

/// Compile a tool's `inputSchema` for runtime validation. A tool with no schema isn't
/// validated (permissive by design); a schema that fails to compile disables validation
/// for that tool with a warning — the operator should have caught it with `lattice check`,
/// and the engine boundary is injection-safe regardless. The schema is operator-authored
/// (no `${ENV}` interpolation), so the compile error is safe to log.
fn compile_schema(tool: &ConfigTool) -> Option<InputSchema> {
    if tool.input_schema.is_empty() {
        return None;
    }
    match InputSchema::compile(&tool.input_schema) {
        Ok(schema) => Some(schema),
        Err(err) => {
            tracing::warn!(
                tool = %tool.name,
                "{err}; input validation disabled for this tool (run `lattice check`)"
            );
            None
        }
    }
}

/// Render schema violations into a single model-facing error message. The violations name
/// the offending input fields/values (the caller's own arguments) — no secrets.
fn validation_failure(tool: &str, violations: &[String]) -> String {
    let mut message = format!("input validation failed for '{tool}':");
    for violation in violations {
        message.push_str("\n- ");
        message.push_str(violation);
    }
    message
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
