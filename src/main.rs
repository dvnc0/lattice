//! `lattice` — config-driven MCP shell binary.
//!
//! Loads a config (T3–T5), builds config-driven tools over the engine (T6–T10) and
//! executors (T11–T13), and serves them as an MCP server over **stdio** (T14) or, with
//! `--http <addr>`, over the **Streamable HTTP** transport (T18).

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

    /// Serve over Streamable HTTP at this address instead of stdio (e.g. 127.0.0.1:8080).
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
        None => match &cli.http {
            Some(addr) => serve_http(cli.config.as_deref(), addr).await,
            None => serve_stdio(cli.config.as_deref()).await,
        },
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

/// Validate a config and print a report. Exits non-zero if the config has errors.
fn check(config: Option<&Path>) -> anyhow::Result<()> {
    // A missing config is a usage error: fail with a non-zero exit so CI can't read
    // it as a passing check.
    let Some(path) = config else {
        anyhow::bail!("check: --config <path> is required");
    };

    let report = lattice::config::check(path)?;
    println!(
        "{}: {} tool(s), expose = {:?}",
        path.display(),
        report.tool_count,
        report.expose
    );
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    for error in &report.errors {
        println!("  error: {error}");
    }
    if report.is_valid() {
        println!("OK");
        Ok(())
    } else {
        anyhow::bail!("{} error(s) found", report.errors.len());
    }
}

/// Serve the config's tools as an MCP server over stdio.
async fn serve_stdio(config: Option<&Path>) -> anyhow::Result<()> {
    let config = load_or_bail(config)?;
    tracing::info!(
        server = %config.server.name,
        tools = config.tools.len(),
        "starting lattice MCP server over stdio"
    );
    let service = LatticeServer::new(config).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Serve the config's tools over the Streamable HTTP transport at `addr` (loopback host
/// validation by default; bind a loopback address).
async fn serve_http(config: Option<&Path>, addr: &str) -> anyhow::Result<()> {
    let config = load_or_bail(config)?;
    let server = LatticeServer::new(config);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(%local_addr, path = lattice::mcp::HTTP_PATH, "starting lattice MCP server over Streamable HTTP");
    lattice::mcp::serve_http(listener, server).await?;
    Ok(())
}

/// Load and validate a config, failing with a usage error when `--config` is absent
/// (serving needs a config — there are no tools without one).
fn load_or_bail(config: Option<&Path>) -> anyhow::Result<lattice::config::Config> {
    let Some(path) = config else {
        anyhow::bail!("serving requires --config <path>");
    };
    Ok(lattice::config::load_config(path)?)
}
