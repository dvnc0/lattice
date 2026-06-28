//! CLI command execution via `tokio::process`.
//!
//! Takes a pure [`CommandSpec`] (from the T9 command builder) plus the tool's
//! [`ParseMode`]/[`ResponseSpec`], runs the program, and returns a response-filtered
//! [`ToolOutcome`]. The program is run **directly (argv), never via a shell** — the spec
//! is already shell-free, so there is no injection surface here.
//!
//! A non-zero exit (or termination by signal) is reported as `is_error: true` carrying the
//! exit code and stderr — not an [`ExecError`]; `ExecError` is reserved for failures that
//! produce no usable result (couldn't spawn, an I/O error, output too large, or a
//! `parse: json` mismatch).
//!
//! Hardening: stdout/stderr are read concurrently with the stdin write (no pipe deadlock)
//! and each is capped at [`MAX_OUTPUT_BYTES`]; the whole run has a wall-clock timeout; and
//! the one log line carries only the program name and exit code — never argv, env, stdin,
//! or output, any of which may hold an interpolated `${ENV}` secret.

use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};

use super::{ExecError, ToolOutcome};
use crate::config::{ParseMode, ResponseSpec};
use crate::engine::{response, CommandSpec};

/// Maximum stdout (and, separately, stderr) we will buffer (10 MiB). A command driven by
/// model-supplied args could otherwise produce an unbounded stream and OOM the process.
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
/// Wall-clock cap on a single command (defense against a hanging child). Not yet
/// operator-configurable — see TASKS.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// Run a command spec and return the response-filtered outcome.
pub async fn execute(
    spec: &CommandSpec,
    parse: ParseMode,
    response_spec: &ResponseSpec,
) -> Result<ToolOutcome, ExecError> {
    let captured = match tokio::time::timeout(COMMAND_TIMEOUT, run(spec)).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(ExecError::Process(format!(
                "command timed out after {}s",
                COMMAND_TIMEOUT.as_secs()
            )))
        }
    };

    // Curated, secret-free log line — never argv/env/stdin/output.
    tracing::debug!(program = %spec.program, exit = ?captured.code, "command complete");

    if captured.code != Some(0) {
        // Non-zero exit or signal: a tool error the model should see, carrying stderr.
        return Ok(ToolOutcome {
            is_error: true,
            value: json!({
                "exit_code": captured.code,
                "stderr": String::from_utf8_lossy(&captured.stderr),
            }),
        });
    }

    let stdout = String::from_utf8_lossy(&captured.stdout);
    let parsed = response::parse_output(&stdout, parse)?;
    Ok(ToolOutcome {
        is_error: false,
        value: response::filter(parsed, response_spec),
    })
}

/// What a finished process produced.
struct Captured {
    /// Exit code, or `None` if terminated by a signal.
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Spawn the command and capture its output and exit status.
async fn run(spec: &CommandSpec) -> Result<Captured, ExecError> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.argv)
        .envs(&spec.env)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true); // don't leave an orphan if we time out / are cancelled
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .map_err(|err| ExecError::Process(err.to_string()))?;

    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Write stdin while draining stdout/stderr — doing these concurrently avoids a pipe
    // deadlock (a child blocked writing a full stdout pipe while we block writing stdin).
    // `try_join!` cancels the siblings if a capped read trips, so the blocked child is
    // dropped (and killed) rather than deadlocking.
    let (_, stdout, stderr) = tokio::try_join!(
        write_stdin(stdin, spec.stdin.as_deref()),
        read_capped(&mut stdout, MAX_OUTPUT_BYTES),
        read_capped(&mut stderr, MAX_OUTPUT_BYTES),
    )?;

    let status = child
        .wait()
        .await
        .map_err(|err| ExecError::Process(err.to_string()))?;
    Ok(Captured {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// Write `data` to the child's stdin (if any) and close it. Best-effort: a command that
/// doesn't consume all of stdin yields a broken pipe, which is not an error for us.
async fn write_stdin(stdin: Option<ChildStdin>, data: Option<&str>) -> Result<(), ExecError> {
    if let Some(mut stdin) = stdin {
        if let Some(data) = data {
            let _ = stdin.write_all(data.as_bytes()).await;
        }
        let _ = stdin.shutdown().await;
        // `stdin` drops here → the write end closes, signalling EOF to the child.
    }
    Ok(())
}

/// Read a stream into a buffer, aborting once more than `limit` bytes are produced.
async fn read_capped<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, ExecError> {
    let mut buffer = Vec::new();
    // Read one past the limit so an exactly-at-limit stream still succeeds.
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut buffer)
        .await
        .map_err(|err| ExecError::Process(err.to_string()))?;
    if buffer.len() > limit {
        return Err(ExecError::ResponseTooLarge { limit });
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_capped_enforces_limit() {
        let data = b"hello world";
        assert!(matches!(
            read_capped(&mut &data[..], 5).await,
            Err(ExecError::ResponseTooLarge { limit: 5 })
        ));
        assert_eq!(read_capped(&mut &data[..], 100).await.unwrap(), data);
        // Exactly at the limit is allowed.
        assert_eq!(read_capped(&mut &data[..], data.len()).await.unwrap(), data);
    }
}
