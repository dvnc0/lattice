//! Runtime input-schema validation (task T17).
//!
//! Before any request/command is built, a tool call's arguments are validated against the
//! tool's authored `inputSchema`, which is compiled **once** (at server startup) into a
//! reusable [`Validator`]. Violations are returned as a list of human-readable messages so
//! the MCP layer can reject the call with an `isError` result that names what was wrong —
//! having executed nothing.
//!
//! **ReDoS note.** `jsonschema` matches `pattern` keywords via `fancy-regex`, which enforces
//! a backtrack limit. A pathological pattern combined with adversarial input therefore
//! surfaces as a `BacktrackLimitExceeded` *validation error* (one more entry in the list),
//! not an unbounded hang — so model-supplied input can't wedge the validator. The schema
//! itself is operator-authored and compiled a single time; only the instance being checked
//! is attacker-influenced.

use jsonschema::{ValidationError, Validator};
use serde_json::{Map, Value};
use thiserror::Error;

/// A tool's compiled input schema, ready to validate call arguments without recompiling.
pub struct InputSchema {
    validator: Validator,
}

/// An `inputSchema` that could not be compiled into a validator.
#[derive(Debug, Error)]
#[error("invalid inputSchema: {0}")]
pub struct SchemaError(String);

impl InputSchema {
    /// Compile a tool's `inputSchema` object into a reusable validator.
    pub fn compile(schema: &Map<String, Value>) -> Result<Self, SchemaError> {
        let document = Value::Object(schema.clone());
        let validator =
            jsonschema::validator_for(&document).map_err(|err| SchemaError(err.to_string()))?;
        Ok(Self { validator })
    }

    /// Validate `input` against the schema, returning every violation (empty ⇒ valid).
    pub fn validate(&self, input: &Value) -> Vec<String> {
        self.validator
            .iter_errors(input)
            .map(format_violation)
            .collect()
    }
}

/// Render one validation error as `"<instance-path>: <message>"` (or just the message at the
/// document root). The message echoes the offending *input* value — which is the caller's own
/// argument, never an `${ENV}` secret (those live in config leaves, not the call input).
fn format_violation(err: ValidationError<'_>) -> String {
    let path = err.instance_path().to_string();
    if path.is_empty() {
        err.to_string()
    } else {
        format!("{path}: {err}")
    }
}

#[cfg(test)]
mod schema_validation {
    use super::*;
    use serde_json::json;

    /// Compile a schema literal (panicking on a bad schema — tests pass valid ones).
    fn schema(document: Value) -> InputSchema {
        InputSchema::compile(document.as_object().expect("object literal")).expect("valid schema")
    }

    #[test]
    fn valid_input_has_no_violations() {
        let schema = schema(json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"],
        }));
        assert!(schema.validate(&json!({ "id": 7 })).is_empty());
    }

    #[test]
    fn missing_required_field_is_reported() {
        let schema = schema(json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"],
        }));
        let violations = schema.validate(&json!({}));
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("id"), "{violations:?}");
    }

    #[test]
    fn wrong_type_reports_instance_path() {
        let schema = schema(json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
        }));
        let violations = schema.validate(&json!({ "id": "not-a-number" }));
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].starts_with("/id:"), "{violations:?}");
    }

    #[test]
    fn all_violations_are_listed() {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "a": { "type": "integer" },
                "b": { "type": "string" },
            },
            "required": ["a", "b"],
        }));
        // `/a` is the wrong type and `b` is missing — both must be reported.
        let violations = schema.validate(&json!({ "a": "x" }));
        assert_eq!(violations.len(), 2, "{violations:?}");
    }

    #[test]
    fn invalid_schema_fails_to_compile() {
        // `type` must be a string (or array of strings), not a number.
        let result = InputSchema::compile(json!({ "type": 123 }).as_object().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn catastrophic_pattern_is_bounded_not_hung() {
        // A classic catastrophic-backtracking pattern against adversarial input must return
        // (via fancy-regex's backtrack limit), not hang — surfacing as a single violation.
        let schema = schema(json!({
            "type": "object",
            "properties": { "s": { "type": "string", "pattern": "^(a+)+$" } },
        }));
        let adversarial = format!("{}!", "a".repeat(50));
        let violations = schema.validate(&json!({ "s": adversarial }));
        assert_eq!(violations.len(), 1, "{violations:?}");
    }
}
