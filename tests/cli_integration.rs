//! Integration tests for the CLI executor (T13) against a real `tests/fixtures/script.sh`.
//!
//! Cover argv/stdin/env/cwd, the three parse modes, response filtering, non-zero exit →
//! `is_error` with stderr, and a spawn failure → `ExecError`.

use std::collections::BTreeMap;

use serde_json::json;

use lattice::config::{ParseMode, ResponseSpec};
use lattice::engine::CommandSpec;
use lattice::exec::cli::execute;
use lattice::exec::ExecError;

fn fixtures_dir() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

fn script() -> String {
    format!("{}/script.sh", fixtures_dir())
}

/// A command spec invoking the test script with `args`.
fn spec(args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: script(),
        argv: args.iter().map(|s| s.to_string()).collect(),
        stdin: None,
        env: BTreeMap::new(),
        cwd: None,
    }
}

#[tokio::test]
async fn parses_json_stdout() {
    let outcome = execute(&spec(&["json"]), ParseMode::Json, &ResponseSpec::default())
        .await
        .unwrap();
    assert!(!outcome.is_error);
    assert_eq!(
        outcome.value,
        json!({ "hello": "world", "secret": "x", "meta": { "ok": true } })
    );
}

#[tokio::test]
async fn json_output_is_response_filtered() {
    let response_spec = ResponseSpec {
        include: None,
        exclude: Some(vec!["secret".to_string()]),
    };
    let outcome = execute(&spec(&["json"]), ParseMode::Json, &response_spec)
        .await
        .unwrap();
    assert!(!outcome.is_error);
    assert_eq!(
        outcome.value,
        json!({ "hello": "world", "meta": { "ok": true } })
    );
}

#[tokio::test]
async fn raw_output_is_verbatim_text() {
    let outcome = execute(
        &spec(&["echo", "hi there"]),
        ParseMode::Raw,
        &ResponseSpec::default(),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!("hi there\n"));
}

#[tokio::test]
async fn lines_output_splits_into_array() {
    let outcome = execute(
        &spec(&["lines"]),
        ParseMode::Lines,
        &ResponseSpec::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.value, json!(["a", "b", "c"]));
}

#[tokio::test]
async fn stdin_is_piped_to_the_command() {
    let mut s = spec(&["stdin"]);
    s.stdin = Some("piped input".to_string());
    let outcome = execute(&s, ParseMode::Raw, &ResponseSpec::default())
        .await
        .unwrap();
    assert_eq!(outcome.value, json!("piped input"));
}

#[tokio::test]
async fn env_vars_are_passed() {
    let mut s = spec(&["env"]);
    s.env = BTreeMap::from([("MY_VAR".to_string(), "hello".to_string())]);
    let outcome = execute(&s, ParseMode::Raw, &ResponseSpec::default())
        .await
        .unwrap();
    assert_eq!(outcome.value, json!("hello\n"));
}

#[tokio::test]
async fn cwd_sets_the_working_directory() {
    let mut s = spec(&["cwd"]);
    s.cwd = Some(fixtures_dir());
    let outcome = execute(&s, ParseMode::Raw, &ResponseSpec::default())
        .await
        .unwrap();

    let printed = outcome.value.as_str().unwrap().trim_end();
    let expected = std::fs::canonicalize(fixtures_dir()).unwrap();
    assert_eq!(printed, expected.to_str().unwrap());
}

#[tokio::test]
async fn nonzero_exit_is_error_with_stderr() {
    let outcome = execute(&spec(&["fail"]), ParseMode::Raw, &ResponseSpec::default())
        .await
        .unwrap();
    // Non-zero exit is a tool error the model should see, carrying the code and stderr.
    assert!(outcome.is_error);
    assert_eq!(outcome.value, json!({ "exit_code": 3, "stderr": "boom\n" }));
}

#[tokio::test]
async fn signal_kill_is_error_with_null_exit_code() {
    let outcome = execute(&spec(&["signal"]), ParseMode::Raw, &ResponseSpec::default())
        .await
        .unwrap();
    // Killed by a signal → no exit code; still an is_error outcome the model sees.
    assert!(outcome.is_error);
    assert_eq!(outcome.value["exit_code"], json!(null));
}

#[tokio::test]
async fn parse_json_mismatch_is_exec_error() {
    // A successful command whose stdout isn't JSON, under parse:json, is an ExecError
    // (a config/tool mismatch with no usable result) — not an is_error outcome.
    let result = execute(
        &spec(&["echo", "not json"]),
        ParseMode::Json,
        &ResponseSpec::default(),
    )
    .await;
    assert!(matches!(result, Err(ExecError::Parse(_))));
}

#[tokio::test]
async fn spawn_failure_is_exec_error() {
    let mut s = spec(&[]);
    s.program = "/nonexistent/definitely-not-a-binary".to_string();
    let result = execute(&s, ParseMode::Raw, &ResponseSpec::default()).await;
    assert!(matches!(result, Err(ExecError::Process(_))));
}
