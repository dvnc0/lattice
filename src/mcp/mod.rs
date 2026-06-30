//! The MCP surface: the `rmcp` server handler that exposes lattice tools.

mod dispatcher;
mod result;
mod server;

pub use server::LatticeServer;
