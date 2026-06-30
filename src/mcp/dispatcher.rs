//! Dispatcher expose mode (task T16).
//!
//! With `expose: dispatcher`, `tools/list` shows exactly two synthetic tools —
//! [`DESCRIBE_ROUTE`] and [`CALL_ROUTE`] — instead of one tool per route. A standard
//! harness only ever sees `tools/list` + the server `instructions`, so the **route
//! catalog** (each route's name + one-line summary, *no schemas*) is embedded into both
//! the auto-generated server instructions and the `call_route` description. The model's
//! flow is: read the catalog → optionally `describe_route(route)` for the exact parameter
//! schema → `call_route(route, params)`.
//!
//! This module is pure surface-shaping: it builds the two descriptors, the catalog text,
//! and parses the dispatcher tools' arguments. The actual route lookup, schema validation,
//! and execution stay in [`server`](super::server) and reuse the identical engine path as
//! tools mode.

use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::{json, Map, Value};

/// The tool name for the optional schema "zoom-in" step.
pub const DESCRIBE_ROUTE: &str = "describe_route";
/// The tool name that runs a route by name.
pub const CALL_ROUTE: &str = "call_route";

/// A JSON object, matching `rmcp`'s argument type (`serde_json::Map<String, Value>`).
type JsonObject = Map<String, Value>;

/// The `describe_route` descriptor: takes a `route` name, returns that route's schema.
pub fn describe_route_descriptor() -> Tool {
    let schema = json!({
        "type": "object",
        "properties": {
            "route": { "type": "string", "description": "Name of the route to describe." }
        },
        "required": ["route"],
        "additionalProperties": false
    });
    Tool::new(
        DESCRIBE_ROUTE,
        "Return the input schema and details for one route — the optional zoom-in step before call_route.",
        schema_object(schema),
    )
}

/// The `call_route` descriptor, with the route catalog embedded in its description so a
/// model that lists tools sees the routes without a separate fetch.
pub fn call_route_descriptor(catalog: &str) -> Tool {
    let schema = json!({
        "type": "object",
        "properties": {
            "route": { "type": "string", "description": "Name of the route to call." },
            "params": {
                "type": "object",
                "description": "Arguments for the route, matching its input schema."
            }
        },
        "required": ["route"],
        "additionalProperties": false
    });
    let description = format!(
        "Call a route by name. Optionally call {DESCRIBE_ROUTE}(route) first for its exact \
         parameters.\n\n{catalog}"
    );
    Tool::new(CALL_ROUTE, description, schema_object(schema))
}

/// Build the lightweight route catalog: one line per route (`- name: summary`), no schemas.
/// `routes` yields each route's name and optional description.
pub fn build_catalog<'a>(routes: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let mut catalog = String::from("Routes:\n");
    for (name, description) in routes {
        match description {
            // Only the first line of the description goes in the catalog (it is a summary).
            Some(desc) => {
                let summary = desc.lines().next().unwrap_or_default().trim();
                catalog.push_str(&format!("- {name}: {summary}\n"));
            }
            None => catalog.push_str(&format!("- {name}\n")),
        }
    }
    catalog
}

/// The auto-generated server `instructions` for dispatcher mode (used unless the config
/// author supplied their own `server.instructions`).
pub fn dispatcher_instructions(catalog: &str) -> String {
    format!(
        "This server exposes its routes through two tools: {DESCRIBE_ROUTE} and {CALL_ROUTE}. \
         To run a route, optionally call {DESCRIBE_ROUTE}(route) to see its exact parameters, \
         then call {CALL_ROUTE}(route, params).\n\n{catalog}"
    )
}

/// Extract the `route` argument for `describe_route`.
pub fn route_arg(arguments: Option<&JsonObject>) -> Result<String, String> {
    arguments
        .and_then(|args| args.get("route"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{DESCRIBE_ROUTE} requires a string 'route' argument"))
}

/// Extract `(route, params)` for `call_route`. `params` defaults to an empty object and
/// must be an object when present.
pub fn call_args(arguments: Option<JsonObject>) -> Result<(String, Value), String> {
    let mut arguments = arguments.unwrap_or_default();
    let route = arguments
        .get("route")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{CALL_ROUTE} requires a string 'route' argument"))?;
    let params = match arguments.remove("params") {
        Some(Value::Object(map)) => Value::Object(map),
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(_) => return Err(format!("{CALL_ROUTE} 'params' must be an object")),
    };
    Ok((route, params))
}

/// Build the `describe_route` payload for one route: its name, description, and the authored
/// input schema (verbatim).
pub fn route_detail(descriptor: &Tool) -> Value {
    json!({
        "route": descriptor.name.as_ref(),
        "description": descriptor.description.as_deref(),
        "inputSchema": Value::Object((*descriptor.input_schema).clone()),
    })
}

/// Wrap a JSON object literal as an `Arc<JsonObject>` for a tool's `input_schema`.
fn schema_object(value: Value) -> Arc<JsonObject> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_routes_with_summaries() {
        let routes = [
            ("get_user", Some("Fetch a user by id.\nignored second line")),
            ("list_dir", None),
        ];
        let catalog = build_catalog(routes.iter().map(|(n, d)| (*n, *d)));
        assert!(catalog.contains("- get_user: Fetch a user by id."));
        assert!(catalog.contains("- list_dir\n"));
        // Only the first line of a multi-line description is used.
        assert!(!catalog.contains("ignored second line"));
    }

    #[test]
    fn route_arg_requires_a_string() {
        assert_eq!(
            route_arg(Some(
                &json!({ "route": "get_user" }).as_object().unwrap().clone()
            )),
            Ok("get_user".to_string())
        );
        assert!(route_arg(None).is_err());
        assert!(route_arg(Some(&json!({ "route": 7 }).as_object().unwrap().clone())).is_err());
    }

    #[test]
    fn call_args_parses_route_and_defaults_params() {
        let args = json!({ "route": "get_user", "params": { "id": 1 } })
            .as_object()
            .unwrap()
            .clone();
        let (route, params) = call_args(Some(args)).unwrap();
        assert_eq!(route, "get_user");
        assert_eq!(params, json!({ "id": 1 }));

        // Missing params → empty object.
        let args = json!({ "route": "get_user" }).as_object().unwrap().clone();
        let (_, params) = call_args(Some(args)).unwrap();
        assert_eq!(params, json!({}));
    }

    #[test]
    fn call_args_rejects_non_object_params_and_missing_route() {
        let args = json!({ "route": "x", "params": [1, 2] })
            .as_object()
            .unwrap()
            .clone();
        assert!(call_args(Some(args)).is_err());
        assert!(call_args(None).is_err());
    }
}
