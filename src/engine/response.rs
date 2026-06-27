//! Response parsing and field filtering.
//!
//! Two pure steps applied to what a tool produced before it is returned to the harness:
//!
//! 1. [`parse_output`] interprets a CLI command's raw stdout per its [`ParseMode`] — as
//!    text (`raw`), a parsed JSON value (`json`), or an array of lines (`lines`). HTTP
//!    responses are already JSON/text and skip this step.
//! 2. [`filter`] trims a JSON **object** to the configured [`ResponseSpec`]: `include`
//!    keeps only the listed dotted field paths, `exclude` drops them, and neither leaves
//!    the value untouched.
//!
//! Dotted paths navigate **objects** (`user.name`); array indices are not addressed, and
//! non-object values (arrays, scalars, raw text) pass through `filter` unchanged.

use serde_json::{Map, Value};
use thiserror::Error;

use crate::config::{ParseMode, ResponseSpec};

/// Errors from parsing command output.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResponseError {
    /// `parse: json` was requested but stdout was not valid JSON.
    #[error("failed to parse command output as JSON: {0}")]
    Json(String),
}

/// Interpret raw command stdout according to `mode`.
pub fn parse_output(text: &str, mode: ParseMode) -> Result<Value, ResponseError> {
    match mode {
        ParseMode::Raw => Ok(Value::String(text.to_string())),
        ParseMode::Json => {
            serde_json::from_str(text).map_err(|err| ResponseError::Json(err.to_string()))
        }
        ParseMode::Lines => Ok(Value::Array(
            text.lines()
                .map(|line| Value::String(line.to_string()))
                .collect(),
        )),
    }
}

/// Filter a JSON value to a response spec.
///
/// `include` rebuilds an object containing only the listed dotted paths; `exclude` removes
/// them from the value in place; with neither (or a non-object value) the input is
/// returned as-is.
/// `include` and `exclude` are mutually exclusive (enforced at config load, T5); if both
/// are set here, `include` wins.
pub fn filter(value: Value, spec: &ResponseSpec) -> Value {
    // Field-path filtering only makes sense on objects; arrays, scalars, and raw/lines
    // output have no addressable fields, so they pass through untouched.
    if !value.is_object() {
        return value;
    }

    if let Some(paths) = &spec.include {
        let mut out = Map::new();
        for path in paths {
            if let Some(found) = get_path(&value, path) {
                insert_path(&mut out, path, found.clone());
            }
        }
        Value::Object(out)
    } else if let Some(paths) = &spec.exclude {
        let mut out = value;
        for path in paths {
            remove_path(&mut out, path);
        }
        out
    } else {
        value
    }
}

/// Follow a dotted path through nested objects, returning the value at the end if present.
fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// Insert `value` at a dotted `path` in `root`, creating intermediate objects as needed.
///
/// Only ever called with paths that [`get_path`] found in a self-consistent source, so an
/// intermediate is always an object; the defensive bail can't fire in practice.
fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_string(), value);
            return;
        }
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        match entry {
            Value::Object(map) => current = map,
            _ => return,
        }
    }
}

/// Remove the value at a dotted `path`, walking through nested objects. A path that does
/// not exist (or runs through a non-object) leaves the value unchanged.
fn remove_path(value: &mut Value, path: &str) {
    let mut segments = path.split('.').peekable();
    let mut current = value;
    while let Some(segment) = segments.next() {
        let Value::Object(map) = current else {
            return; // path runs through a non-object — nothing to remove
        };
        if segments.peek().is_none() {
            map.remove(segment);
            return;
        }
        match map.get_mut(segment) {
            Some(next) => current = next,
            None => return, // path absent
        }
    }
}

#[cfg(test)]
mod response_filter {
    use super::*;
    use serde_json::json;

    fn include(paths: &[&str]) -> ResponseSpec {
        ResponseSpec {
            include: Some(paths.iter().map(|s| s.to_string()).collect()),
            exclude: None,
        }
    }

    fn exclude(paths: &[&str]) -> ResponseSpec {
        ResponseSpec {
            include: None,
            exclude: Some(paths.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn parse_raw_wraps_text_as_string() {
        assert_eq!(
            parse_output("hello world\n", ParseMode::Raw).unwrap(),
            json!("hello world\n")
        );
    }

    #[test]
    fn parse_json_parses_value() {
        assert_eq!(
            parse_output(r#"{"a":1,"b":[2,3]}"#, ParseMode::Json).unwrap(),
            json!({ "a": 1, "b": [2, 3] })
        );
    }

    #[test]
    fn parse_json_rejects_invalid() {
        let err = parse_output("not json", ParseMode::Json).unwrap_err();
        assert!(matches!(err, ResponseError::Json(_)));
    }

    #[test]
    fn parse_lines_splits_into_array() {
        assert_eq!(
            parse_output("a\nb\r\nc", ParseMode::Lines).unwrap(),
            json!(["a", "b", "c"])
        );
        assert_eq!(parse_output("", ParseMode::Lines).unwrap(), json!([]));
    }

    #[test]
    fn no_spec_returns_value_unchanged() {
        let v = json!({ "a": 1, "b": 2 });
        assert_eq!(filter(v.clone(), &ResponseSpec::default()), v);
    }

    #[test]
    fn include_keeps_only_listed_paths_nested() {
        let v = json!({
            "id": 1,
            "user": { "name": "Bob", "email": "b@x.com" },
            "audit": { "by": "sys" }
        });
        assert_eq!(
            filter(v, &include(&["id", "user.name"])),
            json!({ "id": 1, "user": { "name": "Bob" } })
        );
    }

    #[test]
    fn include_of_whole_subtree_keeps_it() {
        let v = json!({ "user": { "name": "Bob", "email": "b@x.com" }, "extra": 9 });
        assert_eq!(
            filter(v, &include(&["user"])),
            json!({ "user": { "name": "Bob", "email": "b@x.com" } })
        );
    }

    #[test]
    fn include_skips_missing_paths() {
        let v = json!({ "id": 1 });
        assert_eq!(
            filter(v, &include(&["id", "nope.deep"])),
            json!({ "id": 1 })
        );
    }

    #[test]
    fn exclude_drops_listed_paths() {
        let v = json!({
            "id": 1,
            "user": { "name": "Bob", "secret": "x" },
            "_internal": true
        });
        assert_eq!(
            filter(v, &exclude(&["_internal", "user.secret"])),
            json!({ "id": 1, "user": { "name": "Bob" } })
        );
    }

    #[test]
    fn exclude_of_missing_path_is_noop() {
        let v = json!({ "id": 1 });
        assert_eq!(filter(v.clone(), &exclude(&["nope", "a.b.c"])), v);
    }

    #[test]
    fn non_object_values_pass_through_filtering() {
        // Raw text / arrays have no addressable fields — include and exclude are no-ops.
        assert_eq!(
            filter(json!("raw text"), &include(&["anything"])),
            json!("raw text")
        );
        assert_eq!(filter(json!([1, 2, 3]), &exclude(&["0"])), json!([1, 2, 3]));
    }

    #[test]
    fn parse_then_filter_compose() {
        let parsed = parse_output(r#"{"keep":1,"drop":2}"#, ParseMode::Json).unwrap();
        assert_eq!(filter(parsed, &include(&["keep"])), json!({ "keep": 1 }));
    }
}
