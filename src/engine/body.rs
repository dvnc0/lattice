//! Nested request-body builder.
//!
//! An HTTP tool's `body` is a map of **dotted target paths** → value expressions. Keys
//! that share a prefix merge into nested JSON objects, so a flat MCP input can fan out
//! into a structured request body:
//!
//! ```text
//! body:
//!   user.name.first: $firstName     ->  { "user": { "name": { "first": "Bob",
//!   user.name.last:  $lastName               "last": "Lee" },
//!   user.active:     true                     "active": true } }
//! ```
//!
//! Each value is resolved with [`value::resolve_optional`]: a leaf whose top-level `$ref`
//! targets an **absent** input is **omitted** (so optional inputs simply don't appear),
//! while a ref resolving to `null` is **kept**. Literals and templates always appear.
//!
//! `body_from` is the escape hatch: a single value expression sent as the **entire** body
//! (no dotted-path fan-out). It and `body` are mutually exclusive — enforced at config
//! load (T5); if both are somehow present here, `body_from` wins.
//!
//! The builder is **pure** — it produces a [`serde_json::Value`]; choosing a content type
//! and serializing the request is the request builder's job (T8).

use serde_json::{Map, Value};
use thiserror::Error;

use super::value::{self, Ctx, ValueError};
use crate::config::ValueMap;

/// Errors from building a request body.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BodyError {
    /// A value expression failed to resolve.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// Two body keys disagree about a location's shape — one targets it as a leaf, the
    /// other descends through it as an object (e.g. `user` and `user.name`).
    #[error("body path '{0}' conflicts with another body key (a value and an object target the same location)")]
    PathConflict(String),
}

/// Build a request body from a tool's `body` map and/or `body_from` expression.
///
/// Returns `None` when there is no body to send: `body_from` resolves to an absent ref,
/// or `body` is empty (or every entry was omitted as an absent optional ref). `body_from`
/// takes precedence over `body` (they are mutually exclusive after validation).
pub fn build_body(
    body: &ValueMap,
    body_from: Option<&Value>,
    ctx: &Ctx,
) -> Result<Option<Value>, BodyError> {
    if let Some(body_from) = body_from {
        return Ok(value::resolve_optional(body_from, ctx)?);
    }

    let mut root = Map::new();
    for (path, expr) in body {
        if let Some(resolved) = value::resolve_optional(expr, ctx)? {
            insert_path(&mut root, path, resolved)?;
        }
    }

    if root.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(root)))
    }
}

/// Insert `value` into `root` at a dotted `path`, creating intermediate objects as needed.
///
/// Errors with [`BodyError::PathConflict`] when the path descends through a location an
/// earlier key already claimed as a leaf (e.g. `user` then `user.name`). The reverse —
/// a leaf overwriting an object subtree — can't arise here: keys arrive in sorted order,
/// so a prefix key (`a`) is always processed before its extension (`a.b`).
fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) -> Result<(), BodyError> {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_string(), value);
            return Ok(());
        }
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        current = match entry {
            Value::Object(map) => map,
            _ => return Err(BodyError::PathConflict(path.to_string())),
        };
    }
    // `split` always yields at least one segment, so the loop returns above; unreachable.
    Ok(())
}

#[cfg(test)]
mod body_builder {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// Build a `body` map from `(path, value-expr)` pairs.
    fn body(pairs: &[(&str, Value)]) -> ValueMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<BTreeMap<_, _>>()
    }

    fn build(b: &ValueMap, input: &Value) -> Result<Option<Value>, BodyError> {
        build_body(b, None, &Ctx::new(input))
    }

    #[test]
    fn merges_dotted_paths_into_nested_objects() {
        let b = body(&[
            ("user.name.first", json!("$firstName")),
            ("user.name.last", json!("$lastName")),
            ("user.active", json!(true)),
        ]);
        let input = json!({ "firstName": "Bob", "lastName": "Lee" });
        assert_eq!(
            build(&b, &input).unwrap(),
            Some(json!({ "user": { "name": { "first": "Bob", "last": "Lee" }, "active": true } }))
        );
    }

    #[test]
    fn preserves_native_types() {
        let b = body(&[("count", json!("$n")), ("flag", json!("$f"))]);
        let input = json!({ "n": 42, "f": false });
        assert_eq!(
            build(&b, &input).unwrap(),
            Some(json!({ "count": 42, "flag": false }))
        );
    }

    #[test]
    fn omits_absent_optional_refs_but_keeps_nulls() {
        let b = body(&[
            ("a", json!("$present")),
            ("b", json!("$absent")),
            ("c", json!("$nullable")),
        ]);
        let input = json!({ "present": "yes", "nullable": null });
        // `b` is dropped (absent); `c` is kept (present-but-null).
        assert_eq!(
            build(&b, &input).unwrap(),
            Some(json!({ "a": "yes", "c": null }))
        );
    }

    #[test]
    fn empty_or_fully_omitted_body_is_none() {
        let empty = body(&[]);
        assert_eq!(build(&empty, &json!({})).unwrap(), None);

        let all_absent = body(&[("a", json!("$x")), ("b", json!("$y"))]);
        assert_eq!(build(&all_absent, &json!({})).unwrap(), None);
    }

    #[test]
    fn renders_literals_and_templates() {
        let b = body(&[
            ("kind", json!("greeting")),
            ("text", json!("Hello {{ input.who }}")),
        ]);
        let input = json!({ "who": "world" });
        assert_eq!(
            build(&b, &input).unwrap(),
            Some(json!({ "kind": "greeting", "text": "Hello world" }))
        );
    }

    #[test]
    fn nested_object_value_resolves_its_inner_refs() {
        // A non-string leaf is resolved recursively (strict), then placed at the path.
        let b = body(&[("meta", json!({ "id": "$id", "tag": "fixed" }))]);
        let input = json!({ "id": 7 });
        assert_eq!(
            build(&b, &input).unwrap(),
            Some(json!({ "meta": { "id": 7, "tag": "fixed" } }))
        );
    }

    #[test]
    fn leaf_versus_object_conflict_errors() {
        // `user` targets a leaf, `user.name` descends through it — incompatible shapes.
        let b = body(&[("user", json!("$u")), ("user.name", json!("$n"))]);
        let input = json!({ "u": "scalar", "n": "Bob" });
        assert_eq!(
            build(&b, &input).unwrap_err(),
            BodyError::PathConflict("user.name".into())
        );
    }

    #[test]
    fn body_from_sends_whole_value() {
        let payload = json!("$payload");
        let input = json!({ "payload": { "arbitrary": [1, 2, 3] } });
        assert_eq!(
            build_body(&body(&[]), Some(&payload), &Ctx::new(&input)).unwrap(),
            Some(json!({ "arbitrary": [1, 2, 3] }))
        );
    }

    #[test]
    fn body_from_absent_ref_is_none() {
        let payload = json!("$payload");
        let input = json!({});
        assert_eq!(
            build_body(&body(&[]), Some(&payload), &Ctx::new(&input)).unwrap(),
            None
        );
    }

    #[test]
    fn body_from_present_null_is_kept() {
        // A present ref resolving to null sends a literal `null` body (vs absent → None).
        let payload = json!("$payload");
        let input = json!({ "payload": null });
        assert_eq!(
            build_body(&body(&[]), Some(&payload), &Ctx::new(&input)).unwrap(),
            Some(json!(null))
        );
    }

    #[test]
    fn body_from_takes_precedence_over_body() {
        let b = body(&[("ignored", json!("$x"))]);
        let payload = json!("$payload");
        let input = json!({ "x": "nope", "payload": "whole" });
        assert_eq!(
            build_body(&b, Some(&payload), &Ctx::new(&input)).unwrap(),
            Some(json!("whole"))
        );
    }

    #[test]
    fn template_error_propagates() {
        let b = body(&[("x", json!("{{ oops"))]);
        let err = build(&b, &json!({})).unwrap_err();
        assert!(matches!(err, BodyError::Value(ValueError::Template(_))));
    }
}
