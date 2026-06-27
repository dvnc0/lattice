//! Value-expression resolution.
//!
//! A config "value leaf" — a body / query / header / arg value, **after** T4's
//! `${ENV}` interpolation — is interpreted against the MCP call's `input` arguments:
//!
//! | Form           | Meaning                                                   |
//! |----------------|-----------------------------------------------------------|
//! | `$a.b.c`       | **InputRef** — the referenced input value, native type    |
//! | `{{ ... }}`    | **Template** — a minijinja template rendered to a string  |
//! | anything else  | **Literal** — kept verbatim (native JSON type)            |
//!
//! Note there is **no `Env` variant**: `${ENV}` is resolved at load time (T4), so by
//! the time the engine runs no `${...}` remains. The resolution context is just the
//! call `input`. (Templates can read env indirectly: a `${ENV}` inside a `{{ ... }}`
//! string was already substituted at load, before the template is rendered here.)
//!
//! URL paths use a different sugar — `{name}` placeholders embedded in literal text,
//! handled by [`resolve_path`] (e.g. `/user/{userId}/update`).
//!
//! `$ref` is **strict** — an absent field errors (see [`resolve_optional`] for the
//! omit-on-absent variant). Templates are **lenient** — an undefined variable renders
//! empty (per Jinja) — and always produce a **string**. There is no escape for `{{`: a
//! literal value that contains `{{` is treated as a template.

use std::sync::OnceLock;

use minijinja::{context, Environment};
use serde_json::{Map, Value};
use thiserror::Error;

/// Errors from resolving a value expression.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValueError {
    /// A `$ref` pointed at an input field that wasn't supplied.
    #[error("input field '{0}' not found")]
    MissingInput(String),
    /// A path `{name}` placeholder pointed at an absent input field.
    #[error("path variable '{0}' not found in input")]
    MissingPathVar(String),
    /// A path `{name}` resolved to a non-scalar (object/array/null) value.
    #[error("path variable '{0}' is not a string, number, or boolean")]
    NonScalarPathVar(String),
    /// A `{{ ... }}` template failed to render.
    #[error("template error: {0}")]
    Template(String),
}

/// The context a value expression resolves against.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// The MCP call's arguments (expected to be a JSON object).
    pub input: &'a Value,
}

impl<'a> Ctx<'a> {
    /// Create a context from the call's input arguments.
    pub fn new(input: &'a Value) -> Self {
        Self { input }
    }
}

/// A parsed value expression from a single string leaf.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueExpr {
    /// `$a.b.c` — a dotted reference into the call input.
    InputRef(String),
    /// `{{ ... }}` — a minijinja template.
    Template(String),
    /// Anything else — a literal value.
    Literal(Value),
}

impl ValueExpr {
    /// Classify a config value into an expression. Non-strings are always literals.
    pub fn parse(value: &Value) -> ValueExpr {
        match value {
            Value::String(s) => Self::parse_str(s),
            other => ValueExpr::Literal(other.clone()),
        }
    }

    fn parse_str(s: &str) -> ValueExpr {
        if let Some(rest) = s.strip_prefix('$') {
            if is_input_ref(rest) {
                return ValueExpr::InputRef(rest.to_string());
            }
        }
        if s.contains("{{") {
            return ValueExpr::Template(s.to_string());
        }
        ValueExpr::Literal(Value::String(s.to_string()))
    }

    /// Resolve this expression against `ctx`.
    pub fn resolve(&self, ctx: &Ctx) -> Result<Value, ValueError> {
        match self {
            ValueExpr::InputRef(path) => lookup(ctx.input, path)
                .cloned()
                .ok_or_else(|| ValueError::MissingInput(path.clone())),
            ValueExpr::Template(template) => render(template, ctx).map(Value::String),
            ValueExpr::Literal(value) => Ok(value.clone()),
        }
    }
}

/// Resolve a value, recursing into arrays and objects so nested string leaves are
/// resolved too. Scalars that are not strings pass through unchanged.
pub fn resolve(value: &Value, ctx: &Ctx) -> Result<Value, ValueError> {
    match value {
        Value::String(_) => ValueExpr::parse(value).resolve(ctx),
        Value::Array(items) => items
            .iter()
            .map(|item| resolve(item, ctx))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, val) in map {
                out.insert(key.clone(), resolve(val, ctx)?);
            }
            Ok(Value::Object(out))
        }
        scalar => Ok(scalar.clone()),
    }
}

/// Like [`resolve`], but a top-level `$ref` to an **absent** input field yields
/// `Ok(None)` instead of an error — letting callers omit optional fields. Present refs
/// (including those resolving to `null`), templates, and literals resolve as usual.
pub fn resolve_optional(value: &Value, ctx: &Ctx) -> Result<Option<Value>, ValueError> {
    if let ValueExpr::InputRef(path) = ValueExpr::parse(value) {
        return Ok(lookup(ctx.input, &path).cloned());
    }
    resolve(value, ctx).map(Some)
}

/// Resolve a path/string template containing `{name}` placeholders, substituting each
/// with its (scalar) input value. Literal text and non-placeholder braces are kept
/// verbatim. Used for URL paths like `/user/{userId}/update`.
pub fn resolve_path(template: &str, ctx: &Ctx) -> Result<String, ValueError> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            out.push(ch);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if closed && is_input_ref(&name) {
            let value =
                lookup(ctx.input, &name).ok_or_else(|| ValueError::MissingPathVar(name.clone()))?;
            let rendered = scalar_to_string(value)
                .ok_or_else(|| ValueError::NonScalarPathVar(name.clone()))?;
            out.push_str(&rendered);
        } else {
            // Not a `{name}` placeholder (e.g. `{{ ... }}` or an invalid name) — verbatim.
            out.push('{');
            out.push_str(&name);
            if closed {
                out.push('}');
            }
        }
    }
    Ok(out)
}

/// Dotted lookup into a JSON value: object keys and array indices.
fn lookup<'a>(input: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = input;
    for segment in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Template execution budget. The template text is operator-authored, but a loop can be
/// driven by model-supplied `input`, so cap execution defensively.
const TEMPLATE_FUEL: u64 = 100_000;

/// A process-wide template environment, configured once and shared.
///
/// minijinja renders against `&self` and treats fuel as a *per-render* budget, so one
/// environment is safe to reuse across every leaf and thread — avoiding a fresh
/// allocation (and fuel reconfiguration) for each template, which adds up when a single
/// request resolves many template leaves.
fn template_env() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_fuel(Some(TEMPLATE_FUEL));
        env
    })
}

/// Render a minijinja template with the call input exposed as `input`.
fn render(template: &str, ctx: &Ctx) -> Result<String, ValueError> {
    template_env()
        .render_str(template, context! { input => ctx.input })
        .map_err(|err| ValueError::Template(err.to_string()))
}

/// A `$ref` name: dot-separated segments, the first an identifier, the rest identifiers
/// or array indices. (So `$5.00` is a literal, not a reference.)
fn is_input_ref(s: &str) -> bool {
    let mut segments = s.split('.');
    match segments.next() {
        Some(first) if is_ident(first) => {}
        _ => return false,
    }
    segments.all(|segment| is_ident(segment) || is_index(segment))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_index(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// String/number/bool render to a scalar string (path segment, query/header value);
/// null/array/object cannot.
pub(crate) fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Flatten a resolved value into the scalar strings for a positional/named slot — argv
/// elements, query params, header values. A scalar yields one string; an array fans out
/// into one string per element, skipping `null` elements; a `null` (top-level or array
/// element) contributes nothing. Returns `None` if the value — or a non-null array element
/// — is a non-scalar (object/nested array), which callers turn into a context-specific
/// error.
pub(crate) fn scalarize(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Null => Some(Vec::new()),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if item.is_null() {
                    continue; // a null element is dropped, like a top-level null
                }
                out.push(scalar_to_string(item)?);
            }
            Some(out)
        }
        scalar => scalar_to_string(scalar).map(|s| vec![s]),
    }
}

#[cfg(test)]
mod value_expr_tests {
    use super::*;
    use serde_json::json;

    fn ctx(input: &Value) -> Ctx<'_> {
        Ctx::new(input)
    }

    #[test]
    fn parses_each_form() {
        assert_eq!(
            ValueExpr::parse(&json!("$firstName")),
            ValueExpr::InputRef("firstName".into())
        );
        assert_eq!(
            ValueExpr::parse(&json!("$user.name.first")),
            ValueExpr::InputRef("user.name.first".into())
        );
        assert_eq!(
            ValueExpr::parse(&json!("{{ input.x }}")),
            ValueExpr::Template("{{ input.x }}".into())
        );
        assert_eq!(
            ValueExpr::parse(&json!("lattice")),
            ValueExpr::Literal(json!("lattice"))
        );
        // `$5.00` is a price, not a ref (first segment isn't an identifier).
        assert_eq!(
            ValueExpr::parse(&json!("$5.00")),
            ValueExpr::Literal(json!("$5.00"))
        );
        // Non-strings are always literals.
        assert_eq!(ValueExpr::parse(&json!(42)), ValueExpr::Literal(json!(42)));
        assert_eq!(
            ValueExpr::parse(&json!(true)),
            ValueExpr::Literal(json!(true))
        );
    }

    #[test]
    fn resolves_input_refs_preserving_type() {
        let input = json!({ "firstName": "Bob", "age": 30, "user": { "name": { "first": "B" } }, "items": [{ "id": 1 }] });
        let ctx = ctx(&input);
        assert_eq!(resolve(&json!("$firstName"), &ctx).unwrap(), json!("Bob"));
        assert_eq!(resolve(&json!("$age"), &ctx).unwrap(), json!(30));
        assert_eq!(
            resolve(&json!("$user.name.first"), &ctx).unwrap(),
            json!("B")
        );
        assert_eq!(resolve(&json!("$items.0.id"), &ctx).unwrap(), json!(1));
    }

    #[test]
    fn missing_input_ref_errors() {
        let input = json!({ "a": 1 });
        let err = resolve(&json!("$missing"), &ctx(&input)).unwrap_err();
        assert_eq!(err, ValueError::MissingInput("missing".into()));
    }

    #[test]
    fn renders_templates() {
        let input = json!({ "first": "Bob", "last": "Lee" });
        let out = resolve(&json!("{{ input.first }} {{ input.last }}"), &ctx(&input)).unwrap();
        assert_eq!(out, json!("Bob Lee"));
    }

    #[test]
    fn literals_pass_through() {
        let input = json!({});
        assert_eq!(
            resolve(&json!("hello"), &ctx(&input)).unwrap(),
            json!("hello")
        );
        assert_eq!(resolve(&json!(42), &ctx(&input)).unwrap(), json!(42));
    }

    #[test]
    fn resolves_nested_containers() {
        let input = json!({ "a": "A", "b": 2 });
        let value = json!({ "x": "$a", "list": ["$b", "lit"] });
        let resolved = resolve(&value, &ctx(&input)).unwrap();
        assert_eq!(resolved, json!({ "x": "A", "list": [2, "lit"] }));
    }

    #[test]
    fn resolves_path_placeholders() {
        let input = json!({ "userId": "u1", "v": 2 });
        let ctx = ctx(&input);
        assert_eq!(
            resolve_path("/user/{userId}/update", &ctx).unwrap(),
            "/user/u1/update"
        );
        assert_eq!(resolve_path("/x/{v}", &ctx).unwrap(), "/x/2");
        // A `{{ ... }}` is not a path placeholder — left verbatim.
        assert_eq!(resolve_path("/x/{{v}}", &ctx).unwrap(), "/x/{{v}}");
        // An invalid name is literal text.
        assert_eq!(
            resolve_path("/a/{not a var}/b", &ctx).unwrap(),
            "/a/{not a var}/b"
        );
    }

    #[test]
    fn path_placeholder_errors() {
        let input = json!({ "obj": { "x": 1 } });
        let ctx = ctx(&input);
        assert_eq!(
            resolve_path("/x/{missing}", &ctx).unwrap_err(),
            ValueError::MissingPathVar("missing".into())
        );
        assert_eq!(
            resolve_path("/x/{obj}", &ctx).unwrap_err(),
            ValueError::NonScalarPathVar("obj".into())
        );
    }

    #[test]
    fn resolve_optional_omits_absent_refs() {
        let input = json!({ "present": "yes", "nullable": null });
        let ctx = ctx(&input);
        // Absent ref → None (omit); present ref → Some, even when the value is null.
        assert_eq!(resolve_optional(&json!("$absent"), &ctx).unwrap(), None);
        assert_eq!(
            resolve_optional(&json!("$present"), &ctx).unwrap(),
            Some(json!("yes"))
        );
        assert_eq!(
            resolve_optional(&json!("$nullable"), &ctx).unwrap(),
            Some(json!(null))
        );
        // Literals and templates are always present.
        assert_eq!(
            resolve_optional(&json!("lit"), &ctx).unwrap(),
            Some(json!("lit"))
        );
    }

    #[test]
    fn template_syntax_error_is_reported() {
        let input = json!({});
        let err = resolve(&json!("{{ oops"), &ctx(&input)).unwrap_err();
        assert!(matches!(err, ValueError::Template(_)));
    }

    #[test]
    fn template_undefined_is_lenient() {
        // Per Jinja, an undefined variable renders empty (unlike strict `$ref`).
        let input = json!({});
        assert_eq!(
            resolve(&json!("{{ input.missing }}"), &ctx(&input)).unwrap(),
            json!("")
        );
    }
}
