//! Mapping execution results into MCP [`CallToolResult`]s (task T14).
//!
//! A successful [`ToolOutcome`] becomes a result whose text content mirrors the
//! (response-filtered) value, plus — when that value is a JSON object —
//! `structuredContent` for structure-aware clients. A non-2xx HTTP status / non-zero
//! CLI exit arrives as `ToolOutcome { is_error: true }` and is surfaced as a tool-level
//! error result (still carrying the filtered body) so the model can react; engine/exec
//! failures use [`error_result`] with a message the caller has already scrubbed of
//! anything that could echo an interpolated secret.

use rmcp::model::{CallToolResult, Content};
use serde_json::Value;

use crate::exec::ToolOutcome;

/// Convert a [`ToolOutcome`] into a [`CallToolResult`].
///
/// The filtered value becomes the result's text content; when it is a JSON object it is
/// also attached as `structuredContent`. A string value is surfaced verbatim (not
/// re-quoted); any other value is pretty-printed JSON. The outcome's `is_error` flag
/// propagates so a failed call reads as a tool error rather than a transport error.
pub fn outcome_to_result(outcome: ToolOutcome) -> CallToolResult {
    let ToolOutcome { is_error, value } = outcome;
    let content = vec![Content::text(render_text(&value))];
    // `structuredContent` is, per the MCP spec, a JSON object — attach it only then.
    let structured_content = if value.is_object() { Some(value) } else { None };

    // `CallToolResult` is `#[non_exhaustive]`, so go through the constructors (which set
    // `is_error`) and then attach the structured payload via the public field.
    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = structured_content;
    result
}

/// Build a tool-level error result carrying `message` as text content. The message must
/// already be free of secrets (callers scrub engine/exec errors before passing them here).
pub fn error_result(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message.into())])
}

/// Render a result value as the text content block: strings verbatim, everything else as
/// pretty-printed JSON (readable for the model).
fn render_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_value_sets_structured_content_and_text() {
        let outcome = ToolOutcome {
            is_error: false,
            value: json!({ "id": 1, "name": "Ada" }),
        };
        let result = outcome_to_result(outcome);
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(json!({ "id": 1, "name": "Ada" }))
        );
        // Text content is the pretty-printed JSON mirror.
        let text = result.content[0].as_text().unwrap().text.clone();
        assert_eq!(
            text,
            serde_json::to_string_pretty(&json!({ "id": 1, "name": "Ada" })).unwrap()
        );
    }

    #[test]
    fn string_value_is_verbatim_without_structured_content() {
        let outcome = ToolOutcome {
            is_error: false,
            value: json!("hello\nworld"),
        };
        let result = outcome_to_result(outcome);
        assert_eq!(result.structured_content, None);
        assert_eq!(result.content[0].as_text().unwrap().text, "hello\nworld");
    }

    #[test]
    fn error_outcome_propagates_is_error() {
        let outcome = ToolOutcome {
            is_error: true,
            value: json!({ "message": "not found" }),
        };
        let result = outcome_to_result(outcome);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(json!({ "message": "not found" }))
        );
    }

    #[test]
    fn error_result_is_error_with_message() {
        let result = error_result("boom");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content[0].as_text().unwrap().text, "boom");
        assert_eq!(result.structured_content, None);
    }
}
