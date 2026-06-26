//! The lattice [`ServerHandler`] implementation.
//!
//! **Tracer bullet (task T2):** this currently exposes a single hardcoded `ping`
//! tool, purely to pin the `rmcp` 1.8 API surface before the translation engine
//! exists. Task T14 replaces the hardcoded tool with tools built from the loaded
//! config; the trait shape established here stays the same.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::json;

/// The lattice MCP server handler.
///
/// Currently fieldless; task T14 adds the loaded config, and T16 adds the
/// auto-generated dispatcher instructions.
#[derive(Clone, Default)]
pub struct LatticeServer {}

impl LatticeServer {
    /// Create a server with no instructions.
    pub fn new() -> Self {
        Self::default()
    }

    /// The hardcoded `ping` tool used by the tracer bullet.
    fn ping_tool() -> Tool {
        let schema = json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Text to echo back (defaults to \"pong\")."
                }
            }
        });
        // The literal above is always a JSON object.
        let schema = schema.as_object().cloned().unwrap_or_default();
        Tool::new(
            "ping",
            "Health-check tool that echoes a message back, prefixed with 'pong: '.",
            Arc::new(schema),
        )
    }
}

impl ServerHandler for LatticeServer {
    fn get_info(&self) -> ServerInfo {
        // Identify as lattice, not the rmcp crate (whose `from_build_env` default
        // would otherwise be reported to the harness).
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![Self::ping_tool()]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "ping" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("message"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("pong");
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "pong: {message}"
                ))]))
            }
            // Unknown tools surface as an error *result* (isError) so the model
            // can react, rather than a protocol-level error.
            other => Ok(CallToolResult::error(vec![Content::text(format!(
                "unknown tool: {other}"
            ))])),
        }
    }
}
