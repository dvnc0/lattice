//! Task T15 — stdio transport wiring + stdout purity.
//!
//! Spawns the **real `lattice` binary** with `--config`, drives a minimal MCP
//! handshake over its stdin, and asserts that stdout carries *only* framed
//! JSON-RPC — no log lines or stray prints leak onto the channel reserved for the
//! protocol. Logs must instead appear on stderr. This guards the project's
//! stdout-purity invariant against regressions (a stray `println!`, a tracing
//! writer pointed at stdout, a dependency that prints).

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Three newline-delimited JSON-RPC messages: initialize, the initialized
/// notification, then tools/list. Closing stdin afterwards (EOF) shuts the server down.
const REQUESTS: &str = concat!(
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"stdio-purity-test","version":"0.0.0"}}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    "\n",
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    "\n",
);

#[tokio::test]
async fn stdout_carries_only_framed_jsonrpc() -> anyhow::Result<()> {
    let config = format!("{}/tests/fixtures/stdio.yaml", env!("CARGO_MANIFEST_DIR"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_lattice"))
        .arg("--config")
        .arg(&config)
        // Pin the log level so the startup log is emitted regardless of the ambient
        // RUST_LOG — the stderr-logging assertion below must not depend on the caller's env.
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    {
        let mut stdin = child.stdin.take().expect("stdin piped");
        stdin.write_all(REQUESTS.as_bytes()).await?;
        stdin.flush().await?;
        // `stdin` drops here → EOF → the stdio transport closes → the server exits.
    }

    // Bound the run so a hung server fails fast rather than blocking CI.
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await??;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;

    // Every non-empty stdout line must be a JSON-RPC message — nothing else may leak.
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        !lines.is_empty(),
        "server produced no stdout.\nstderr:\n{stderr}"
    );
    for line in &lines {
        let message: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|err| {
            panic!("non-JSON line leaked onto stdout: {line:?} ({err})\nstderr:\n{stderr}")
        });
        assert_eq!(
            message.get("jsonrpc").and_then(|v| v.as_str()),
            Some("2.0"),
            "stdout line is not a JSON-RPC message: {line}"
        );
    }

    // The traffic is real: tools/list returned the configured tool.
    assert!(
        lines.iter().any(|line| line.contains("\"noop\"")),
        "tools/list did not include the configured tool.\nstdout:\n{stdout}"
    );

    // Logs went to stderr — and crucially *not* to stdout.
    assert!(
        stderr.contains("lattice"),
        "expected startup logging on stderr.\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("starting lattice MCP server"),
        "a log line leaked onto stdout.\nstdout:\n{stdout}"
    );

    Ok(())
}
