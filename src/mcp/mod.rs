//! The MCP surface: the `rmcp` server handler that exposes lattice tools, plus the
//! Streamable HTTP transport that serves it over the network (T18). The stdio transport is
//! wired directly in `main`.

mod dispatcher;
mod result;
mod server;

use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

pub use server::LatticeServer;

/// The HTTP path the Streamable HTTP transport is mounted at.
pub const HTTP_PATH: &str = "/mcp";

/// Serve `server` over the Streamable HTTP transport on an already-bound `listener`.
///
/// Taking a bound listener (rather than an address) lets the caller read the actual local
/// address first — `main` logs it, tests bind an ephemeral port. The transport defaults to
/// **loopback-only `Host` validation** (DNS-rebinding protection), so binding to a public
/// interface additionally requires overriding the allowed hosts. Every HTTP session shares
/// the one `server` — and therefore its OAuth token caches and HTTP client — via its
/// internal `Arc`.
pub async fn serve_http(listener: TcpListener, server: LatticeServer) -> std::io::Result<()> {
    let service = StreamableHttpService::new(
        // One shared server backs every session (cheap `Arc` clone per session).
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service(HTTP_PATH, service);
    axum::serve(listener, router).await
}
