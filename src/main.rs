//! `lattice` — config-driven MCP shell binary.
//!
//! Tracer-bullet stage: serves a hardcoded `ping` tool over stdio. Config loading
//! (T3–T5), the translation engine (T6–T10), execution (T11–T13), and the HTTP
//! transport (T18) arrive in later tasks.

use std::path::Path;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use lattice::mcp::LatticeServer;

/// Turn existing REST APIs and CLI tools into MCP servers from a config file.
#[derive(Parser, Debug)]
#[command(name = "lattice", version, about)]
struct Cli {
    /// Path to the config file (YAML or JSON).
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Also serve over Streamable HTTP at this address (e.g. 127.0.0.1:8080).
    #[arg(long, global = true)]
    http: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Validate the config without starting the server.
    Check,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Check) => check(cli.config.as_deref()),
        None => {
            if let Some(addr) = &cli.http {
                tracing::warn!(%addr, "HTTP transport not implemented yet (task T18); serving stdio only");
            }
            serve_stdio(cli.config.as_deref()).await
        }
    }
}

/// Initialize logging to **stderr**. Stdout is reserved for the JSON-RPC stream in
/// stdio mode, so nothing else may write there.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

/// Validate a config. (Real validation lands in tasks T3–T5.)
fn check(config: Option<&Path>) -> anyhow::Result<()> {
    // A missing config is a usage error: fail with a non-zero exit so CI can't read
    // it as a passing check.
    let Some(path) = config else {
        anyhow::bail!("check: --config <path> is required");
    };
    eprintln!(
        "check {}: not yet implemented (tasks T3-T5)",
        path.display()
    );
    Ok(())
}

/// Serve the MCP server over stdio.
async fn serve_stdio(config: Option<&Path>) -> anyhow::Result<()> {
    if let Some(path) = config {
        tracing::info!(config = %path.display(), "config provided (loading lands in task T14)");
    }
    tracing::info!("starting lattice MCP server over stdio (tracer bullet: 'ping' tool)");
    let service = LatticeServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
