//! CLI command builder.
//!
//! Translates a [`CliTarget`] plus a tool call's `input` into a pure [`CommandSpec`] —
//! the program, argv, stdin, environment, and working directory. It does no I/O; the
//! executor (T13) runs the spec via `tokio::process`.
//!
//! **Argv-only, never a shell.** The `command` is a literal program name from config
//! (never a value expression, so model input can't choose which program runs), and each
//! argument is passed as a distinct argv entry — there is no shell, so no string is ever
//! interpreted for metacharacters. This is the injection-safety invariant.
//!
//! Value handling per part:
//! - **args** — each resolved with [`value::resolve_optional`]; a scalar becomes one arg,
//!   an array fans out into several (e.g. `$tags` → `a b c`), and an absent/`null` value
//!   contributes nothing. An object (or nested array) is an error.
//! - **env** — `name → value`; absent/`null` omitted, scalars stringified, objects error.
//! - **stdin** — a string is piped verbatim; any other JSON value is piped as compact
//!   JSON; absent or `null` means no stdin.
//! - **cwd** — a single scalar path; absent or `null` means inherit the parent's cwd.

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

use super::value::{self, Ctx, ValueError};
use crate::config::CliTarget;

/// A fully-resolved command invocation, ready for the executor to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program to execute (run directly as argv[0], never via a shell).
    pub program: String,
    /// Ordered argument list.
    pub argv: Vec<String>,
    /// Text piped to the process's standard input, if any.
    pub stdin: Option<String>,
    /// Extra environment variables layered onto the parent environment.
    pub env: BTreeMap<String, String>,
    /// Working directory; `None` inherits the parent's.
    pub cwd: Option<String>,
}

/// Errors from building a command.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    /// An argument/env/cwd value expression failed to resolve.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// A value resolved to something not representable as a command string (an object,
    /// or an array where a single scalar was required).
    #[error("{location} resolved to a non-scalar value (objects/nested arrays aren't allowed)")]
    NonScalar { location: String },
}

/// Build a [`CommandSpec`] from a tool's CLI target and the call's input context.
pub fn build_command(target: &CliTarget, ctx: &Ctx) -> Result<CommandSpec, CommandError> {
    let mut argv = Vec::new();
    for expr in &target.args {
        if let Some(resolved) = value::resolve_optional(expr, ctx)? {
            let parts = value::scalarize(&resolved).ok_or_else(|| CommandError::NonScalar {
                location: "argument".to_string(),
            })?;
            argv.extend(parts);
        }
    }

    let mut env = BTreeMap::new();
    for (name, expr) in &target.env {
        let Some(resolved) = value::resolve_optional(expr, ctx)? else {
            continue; // absent optional `$ref` — omit the variable
        };
        if resolved.is_null() {
            continue; // an explicit null env value is omitted too
        }
        let rendered =
            value::scalar_to_string(&resolved).ok_or_else(|| CommandError::NonScalar {
                location: format!("env '{name}'"),
            })?;
        env.insert(name.clone(), rendered);
    }

    let stdin = resolve_stdin(target, ctx)?;
    let cwd = resolve_cwd(target, ctx)?;

    Ok(CommandSpec {
        program: target.command.clone(),
        argv,
        stdin,
        env,
        cwd,
    })
}

/// Resolve stdin: a string is piped verbatim, any other value as compact JSON; absent or
/// `null` yields no stdin.
fn resolve_stdin(target: &CliTarget, ctx: &Ctx) -> Result<Option<String>, CommandError> {
    let Some(expr) = target.stdin.as_ref() else {
        return Ok(None);
    };
    Ok(match value::resolve_optional(expr, ctx)? {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

/// Resolve cwd: a single scalar path; absent or `null` inherits the parent's cwd.
fn resolve_cwd(target: &CliTarget, ctx: &Ctx) -> Result<Option<String>, CommandError> {
    let Some(expr) = target.cwd.as_ref() else {
        return Ok(None);
    };
    match value::resolve_optional(expr, ctx)? {
        None | Some(Value::Null) => Ok(None),
        Some(other) => {
            let rendered =
                value::scalar_to_string(&other).ok_or_else(|| CommandError::NonScalar {
                    location: "cwd".to_string(),
                })?;
            Ok(Some(rendered))
        }
    }
}

#[cfg(test)]
mod cli_command_builder {
    use super::*;
    use crate::config::{ParseMode, ResponseSpec};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// A bare CLI target; tests set only the fields they exercise.
    fn target(command: &str) -> CliTarget {
        CliTarget {
            command: command.to_string(),
            args: Vec::new(),
            stdin: None,
            env: BTreeMap::new(),
            cwd: None,
            parse: ParseMode::Raw,
            response: ResponseSpec::default(),
        }
    }

    fn build(t: &CliTarget, input: &Value) -> Result<CommandSpec, CommandError> {
        build_command(t, &Ctx::new(input))
    }

    #[test]
    fn program_is_literal_and_args_resolve() {
        let mut t = target("ls");
        t.args = vec![json!("-la"), json!("$dir")];
        let spec = build(&t, &json!({ "dir": "/tmp" })).unwrap();
        assert_eq!(spec.program, "ls");
        assert_eq!(spec.argv, vec!["-la", "/tmp"]);
        assert_eq!(spec.stdin, None);
        assert!(spec.env.is_empty());
        assert_eq!(spec.cwd, None);
    }

    #[test]
    fn array_arg_flattens_into_multiple_args() {
        let mut t = target("grep");
        t.args = vec![json!("-e"), json!("$patterns")];
        let spec = build(&t, &json!({ "patterns": ["foo", "bar", 3] })).unwrap();
        assert_eq!(spec.argv, vec!["-e", "foo", "bar", "3"]);
    }

    #[test]
    fn absent_and_null_args_are_omitted() {
        let mut t = target("cmd");
        t.args = vec![json!("--keep"), json!("$missing"), json!("$nothing")];
        let spec = build(&t, &json!({ "nothing": null })).unwrap();
        assert_eq!(spec.argv, vec!["--keep"]);
    }

    #[test]
    fn number_and_bool_args_stringify() {
        let mut t = target("cmd");
        t.args = vec![json!("$n"), json!("$flag")];
        let spec = build(&t, &json!({ "n": 42, "flag": true })).unwrap();
        assert_eq!(spec.argv, vec!["42", "true"]);
    }

    #[test]
    fn object_arg_errors() {
        let mut t = target("cmd");
        t.args = vec![json!("$obj")];
        let err = build(&t, &json!({ "obj": { "a": 1 } })).unwrap_err();
        assert_eq!(
            err,
            CommandError::NonScalar {
                location: "argument".to_string()
            }
        );
    }

    #[test]
    fn env_resolves_omitting_absent_and_erroring_on_objects() {
        let mut t = target("cmd");
        t.env = [
            ("TOKEN", json!("$tok")),
            ("ABSENT", json!("$nope")),
            ("LEVEL", json!(3)),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
        let spec = build(&t, &json!({ "tok": "secret" })).unwrap();
        assert_eq!(spec.env.get("TOKEN").map(String::as_str), Some("secret"));
        assert_eq!(spec.env.get("LEVEL").map(String::as_str), Some("3"));
        assert!(!spec.env.contains_key("ABSENT"));
    }

    #[test]
    fn non_scalar_env_value_errors() {
        let mut t = target("cmd");
        t.env = [("BAD", json!("$arr"))]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let err = build(&t, &json!({ "arr": [1, 2] })).unwrap_err();
        assert_eq!(
            err,
            CommandError::NonScalar {
                location: "env 'BAD'".to_string()
            }
        );
    }

    #[test]
    fn stdin_string_is_verbatim_and_object_is_json() {
        let mut t = target("cmd");
        t.stdin = Some(json!("$text"));
        let spec = build(&t, &json!({ "text": "hello\nworld" })).unwrap();
        assert_eq!(spec.stdin.as_deref(), Some("hello\nworld"));

        t.stdin = Some(json!("$payload"));
        let spec = build(&t, &json!({ "payload": { "a": 1 } })).unwrap();
        assert_eq!(spec.stdin.as_deref(), Some(r#"{"a":1}"#));
    }

    #[test]
    fn stdin_absent_or_null_is_none() {
        let mut t = target("cmd");
        assert_eq!(build(&t, &json!({})).unwrap().stdin, None);
        t.stdin = Some(json!("$missing"));
        assert_eq!(build(&t, &json!({})).unwrap().stdin, None);
        t.stdin = Some(json!("$x"));
        assert_eq!(build(&t, &json!({ "x": null })).unwrap().stdin, None);
    }

    #[test]
    fn cwd_resolves_and_defaults_to_none() {
        let mut t = target("cmd");
        t.cwd = Some(json!("$workdir"));
        assert_eq!(
            build(&t, &json!({ "workdir": "/srv" }))
                .unwrap()
                .cwd
                .as_deref(),
            Some("/srv")
        );
        // Absent ref → inherit parent's cwd.
        assert_eq!(build(&t, &json!({})).unwrap().cwd, None);
    }

    #[test]
    fn non_scalar_cwd_errors() {
        let mut t = target("cmd");
        t.cwd = Some(json!("$dir"));
        let err = build(&t, &json!({ "dir": ["a"] })).unwrap_err();
        assert_eq!(
            err,
            CommandError::NonScalar {
                location: "cwd".to_string()
            }
        );
    }
}
