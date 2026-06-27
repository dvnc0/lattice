//! HTTP request builder.
//!
//! Translates a (defaults-merged) [`HttpTarget`] plus a tool call's `input` into a pure
//! [`HttpRequestSpec`] — the method, fully-resolved URL, query, headers, and body. It does
//! no I/O; the executor (T11) turns the spec into an actual `reqwest` call.
//!
//! Value handling per part:
//! - **path** — `{var}` placeholders filled from input ([`value::resolve_path`]); a missing
//!   placeholder is an error (the URL would otherwise be malformed).
//! - **query / headers** — each value resolved with [`value::resolve_optional`], so an
//!   absent optional `$ref` is **omitted**. A resolved scalar becomes one entry; an array
//!   fans out into repeated entries (e.g. `?tag=a&tag=b`); `null` is dropped; an object is
//!   an error (not representable as a query/header value).
//! - **body** — built by [`body::build_body`]; default content type `application/json`
//!   (unless the tool sets its own `Content-Type` header).

use serde_json::Value;
use thiserror::Error;

use super::body::{self, BodyError};
use super::value::{self, Ctx, ValueError};
use crate::config::{HttpTarget, ValueMap};

/// The default request body content type (form/raw bodies are deferred — see SPEC).
const JSON_CONTENT_TYPE: &str = "application/json";

/// A fully-resolved HTTP request, ready for the executor to send.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequestSpec {
    /// HTTP method, verbatim from config (parsed by the executor).
    pub method: String,
    /// Absolute (or base-joined) request URL with path vars filled.
    pub url: String,
    /// Query parameters as ordered `(name, value)` pairs (a name may repeat).
    pub query: Vec<(String, String)>,
    /// Request headers as ordered `(name, value)` pairs (a name may repeat).
    pub headers: Vec<(String, String)>,
    /// Request body, if any.
    pub body: Option<Value>,
    /// Content type to send with the body. `None` when there is no body, or when the
    /// tool already supplies its own `Content-Type` header.
    pub content_type: Option<String>,
}

/// Errors from building an HTTP request.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RequestError {
    /// A path/query/header value expression failed to resolve.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// The request body failed to build.
    #[error(transparent)]
    Body(#[from] BodyError),
    /// A query or header value resolved to something not representable as a string
    /// (an object, or an array containing non-scalars).
    #[error(
        "{part} '{name}' resolved to a non-scalar value (objects/nested arrays aren't allowed)"
    )]
    NonScalar { part: &'static str, name: String },
}

/// Build an [`HttpRequestSpec`] from a tool's HTTP target and the call's input context.
pub fn build_request(target: &HttpTarget, ctx: &Ctx) -> Result<HttpRequestSpec, RequestError> {
    let path = value::resolve_path(&target.path, ctx)?;
    let url = match &target.base_url {
        Some(base) => join_url(base, &path),
        None => path,
    };

    let query = resolve_pairs("query", &target.query, ctx)?;
    let headers = resolve_pairs("header", &target.headers, ctx)?;
    let body = body::build_body(&target.body, target.body_from.as_ref(), ctx)?;

    // Default to JSON for a body, unless the tool already declares a Content-Type header.
    let declares_content_type = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    let content_type = if body.is_some() && !declares_content_type {
        Some(JSON_CONTENT_TYPE.to_string())
    } else {
        None
    };

    Ok(HttpRequestSpec {
        method: target.method.clone(),
        url,
        query,
        headers,
        body,
        content_type,
    })
}

/// Resolve a `name → value-expr` map into ordered `(name, string)` pairs, omitting absent
/// optional refs and fanning arrays out into repeated entries. `part` labels errors.
fn resolve_pairs(
    part: &'static str,
    map: &ValueMap,
    ctx: &Ctx,
) -> Result<Vec<(String, String)>, RequestError> {
    let mut out = Vec::new();
    for (name, expr) in map {
        let Some(resolved) = value::resolve_optional(expr, ctx)? else {
            continue; // absent optional `$ref` — omit
        };
        stringify_into(part, name, &resolved, &mut out)?;
    }
    Ok(out)
}

/// Append `(name, value)` pair(s) for a resolved query/header value: a scalar yields one
/// entry, an array fans out, `null` is dropped, and anything else is a [`RequestError`].
fn stringify_into(
    part: &'static str,
    name: &str,
    value: &Value,
    out: &mut Vec<(String, String)>,
) -> Result<(), RequestError> {
    let rendered = value::scalarize(value).ok_or_else(|| RequestError::NonScalar {
        part,
        name: name.to_string(),
    })?;
    out.extend(rendered.into_iter().map(|v| (name.to_string(), v)));
    Ok(())
}

/// Join a base URL and a path with exactly one separating slash.
fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        base.to_string()
    } else if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod http_request_builder {
    use super::*;
    use crate::config::{HttpTarget, ResponseSpec};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// A bare HTTP target; tests set only the fields they exercise.
    fn target(method: &str, path: &str) -> HttpTarget {
        HttpTarget {
            method: method.to_string(),
            path: path.to_string(),
            base_url: None,
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: BTreeMap::new(),
            body_from: None,
            auth: None,
            response: ResponseSpec::default(),
        }
    }

    fn map(pairs: &[(&str, Value)]) -> ValueMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn build(t: &HttpTarget, input: &Value) -> Result<HttpRequestSpec, RequestError> {
        build_request(t, &Ctx::new(input))
    }

    #[test]
    fn builds_method_and_joins_base_url_with_path_var() {
        let mut t = target("GET", "/users/{id}/posts");
        t.base_url = Some("https://api.example.com/".to_string());
        let spec = build(&t, &json!({ "id": 42 })).unwrap();
        assert_eq!(spec.method, "GET");
        assert_eq!(spec.url, "https://api.example.com/users/42/posts");
        assert!(spec.body.is_none());
        assert_eq!(spec.content_type, None);
    }

    #[test]
    fn missing_path_var_errors() {
        let t = target("GET", "/users/{id}");
        let err = build(&t, &json!({})).unwrap_err();
        assert_eq!(
            err,
            RequestError::Value(ValueError::MissingPathVar("id".into()))
        );
    }

    #[test]
    fn no_base_url_uses_path_as_full_url() {
        let t = target("GET", "https://svc.local/health");
        let spec = build(&t, &json!({})).unwrap();
        assert_eq!(spec.url, "https://svc.local/health");
    }

    #[test]
    fn join_handles_slash_combinations() {
        assert_eq!(join_url("https://x.com", "/a"), "https://x.com/a");
        assert_eq!(join_url("https://x.com/", "/a"), "https://x.com/a");
        assert_eq!(join_url("https://x.com/", "a"), "https://x.com/a");
        assert_eq!(join_url("https://x.com", ""), "https://x.com");
    }

    #[test]
    fn query_resolves_omits_absent_and_stringifies_scalars() {
        let mut t = target("GET", "/search");
        t.query = map(&[
            ("q", json!("$term")),
            ("page", json!("$page")),     // absent → omitted
            ("limit", json!(10)),         // literal number → "10"
            ("active", json!("$active")), // bool → "true"
        ]);
        let spec = build(&t, &json!({ "term": "rust", "active": true })).unwrap();
        // BTreeMap order: active, limit, q (page omitted).
        assert_eq!(
            spec.query,
            vec![
                ("active".to_string(), "true".to_string()),
                ("limit".to_string(), "10".to_string()),
                ("q".to_string(), "rust".to_string()),
            ]
        );
    }

    #[test]
    fn query_array_fans_out_to_repeated_params() {
        let mut t = target("GET", "/search");
        t.query = map(&[("tag", json!("$tags"))]);
        let spec = build(&t, &json!({ "tags": ["a", "b", 3] })).unwrap();
        assert_eq!(
            spec.query,
            vec![
                ("tag".to_string(), "a".to_string()),
                ("tag".to_string(), "b".to_string()),
                ("tag".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn query_array_drops_null_elements() {
        let mut t = target("GET", "/search");
        t.query = map(&[("tag", json!("$tags"))]);
        let spec = build(&t, &json!({ "tags": ["a", null, "b"] })).unwrap();
        // null elements are dropped, consistent with a top-level null value.
        assert_eq!(
            spec.query,
            vec![
                ("tag".to_string(), "a".to_string()),
                ("tag".to_string(), "b".to_string()),
            ]
        );
    }

    #[test]
    fn non_scalar_query_value_errors() {
        let mut t = target("GET", "/x");
        t.query = map(&[("bad", json!("$obj"))]);
        let err = build(&t, &json!({ "obj": { "nested": 1 } })).unwrap_err();
        assert_eq!(
            err,
            RequestError::NonScalar {
                part: "query",
                name: "bad".into()
            }
        );
    }

    #[test]
    fn headers_resolve_and_template() {
        let mut t = target("GET", "/x");
        t.headers = map(&[
            ("Accept", json!("application/json")),
            ("X-Trace", json!("{{ input.trace }}")),
        ]);
        let spec = build(&t, &json!({ "trace": "abc" })).unwrap();
        assert_eq!(
            spec.headers,
            vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("X-Trace".to_string(), "abc".to_string()),
            ]
        );
    }

    #[test]
    fn body_builds_nested_and_defaults_json_content_type() {
        let mut t = target("POST", "/users");
        t.body = map(&[
            ("user.name.first", json!("$first")),
            ("user.active", json!(true)),
        ]);
        let spec = build(&t, &json!({ "first": "Bob" })).unwrap();
        assert_eq!(
            spec.body,
            Some(json!({ "user": { "name": { "first": "Bob" }, "active": true } }))
        );
        assert_eq!(spec.content_type, Some("application/json".to_string()));
    }

    #[test]
    fn explicit_content_type_header_suppresses_default() {
        let mut t = target("POST", "/x");
        t.body = map(&[("a", json!("$a"))]);
        t.headers = map(&[("content-type", json!("application/xml"))]);
        let spec = build(&t, &json!({ "a": 1 })).unwrap();
        assert_eq!(spec.body, Some(json!({ "a": 1 })));
        // The tool declared its own (lower-cased) Content-Type, so no default is added.
        assert_eq!(spec.content_type, None);
    }

    #[test]
    fn body_from_passes_through_whole_value() {
        let mut t = target("POST", "/raw");
        t.body_from = Some(json!("$payload"));
        let spec = build(&t, &json!({ "payload": [1, 2, 3] })).unwrap();
        assert_eq!(spec.body, Some(json!([1, 2, 3])));
        assert_eq!(spec.content_type, Some("application/json".to_string()));
    }

    #[test]
    fn empty_body_yields_no_content_type() {
        let t = target("POST", "/ping");
        let spec = build(&t, &json!({})).unwrap();
        assert_eq!(spec.body, None);
        assert_eq!(spec.content_type, None);
    }
}
