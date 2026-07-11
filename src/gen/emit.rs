//! Emitter: [`GeneratorInput`] → [`Config`].
//!
//! This module is pure — no file I/O, no environment reads, no printing.

use serde_json::{Map, Value};

use crate::config::{
    Auth, Config, Defaults, ExposeMode, HttpTarget, ResponseSpec, Server, Tool, ValueMap,
};
use crate::gen::openapi::{DetectedAuth, GeneratorInput, OperationInput, RequestBodyKind};

const AUTO_EXPOSE_THRESHOLD: usize = 20;

/// Emit a [`Config`] from a [`GeneratorInput`].
///
/// Returns the config plus a list of non-fatal warning strings (printed to
/// stderr by the caller).
pub fn emit(input: &GeneratorInput, expose_override: Option<ExposeMode>) -> (Config, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();

    let expose = expose_override.unwrap_or(if input.operations.len() > AUTO_EXPOSE_THRESHOLD {
        ExposeMode::Dispatcher
    } else {
        ExposeMode::Tools
    });

    let server = Server {
        name: sanitize_server_name(&input.title),
        version: input.version.clone(),
        instructions: input.description.clone(),
        expose,
    };

    let defaults = build_defaults(input, &mut warnings);
    let tools = build_tools(input, &mut warnings);

    (
        Config {
            server,
            defaults,
            tools,
        },
        warnings,
    )
}

// ── Server name ───────────────────────────────────────────────────────────────

/// Lowercase the title, replace non-alphanumeric runs with `_`, trim edges.
fn sanitize_server_name(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_underscore = true;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_owned()
}

// ── Defaults ──────────────────────────────────────────────────────────────────

fn build_defaults(input: &GeneratorInput, warnings: &mut Vec<String>) -> Defaults {
    let mut headers = ValueMap::new();
    headers.insert(
        "Accept".to_owned(),
        Value::String("application/json".to_owned()),
    );

    let base_url = match &input.base_url {
        Some(u) => Some(u.clone()),
        None => {
            warnings.push(
                "spec has no servers[0].url — set defaults.base_url in the generated config before use".to_owned(),
            );
            Some("https://REPLACE_WITH_BASE_URL".to_owned())
        }
    };

    Defaults {
        base_url,
        headers,
        auth: input.auth.as_ref().map(|a| detected_to_auth(a, warnings)),
    }
}

fn detected_to_auth(auth: &DetectedAuth, _warnings: &mut Vec<String>) -> Auth {
    match auth {
        DetectedAuth::Bearer { env_prefix } => Auth::Bearer {
            token: format!("${{{env_prefix}_TOKEN}}"),
        },
        DetectedAuth::Basic { env_prefix } => Auth::Basic {
            username: format!("${{{env_prefix}_USER}}"),
            password: format!("${{{env_prefix}_PASS}}"),
        },
        DetectedAuth::ApiKey {
            location,
            param_name,
            env_prefix,
        } => Auth::ApiKey {
            location: *location,
            name: param_name.clone(),
            value: format!("${{{env_prefix}_API_KEY}}"),
        },
        DetectedAuth::Oauth2 {
            token_url,
            env_prefix,
            scopes,
        } => Auth::Oauth2 {
            token_url: token_url.clone(),
            client_id: format!("${{{env_prefix}_CLIENT_ID}}"),
            client_secret: format!("${{{env_prefix}_CLIENT_SECRET}}"),
            scopes: scopes.clone(),
        },
    }
}

// ── Tools ─────────────────────────────────────────────────────────────────────

fn build_tools(input: &GeneratorInput, warnings: &mut Vec<String>) -> Vec<Tool> {
    input
        .operations
        .iter()
        .map(|op| build_tool(op, warnings))
        .collect()
}

fn build_tool(op: &OperationInput, _warnings: &mut Vec<String>) -> Tool {
    let (input_schema, body, body_from) = build_schema_and_body(op);

    Tool {
        name: op.name.clone(),
        description: op.description.clone(),
        input_schema,
        http: Some(HttpTarget {
            method: op.method.clone(),
            path: op.path.clone(),
            base_url: None,
            query: build_query(&op.query_params),
            headers: ValueMap::new(),
            body,
            body_from,
            auth: None,
            response: ResponseSpec::default(),
        }),
        cli: None,
    }
}

/// Build the `inputSchema`, `body`, and `body_from` fields for a tool.
fn build_schema_and_body(op: &OperationInput) -> (Map<String, Value>, ValueMap, Option<Value>) {
    let mut properties: Map<String, Value> = Map::new();
    let mut required_fields: Vec<Value> = Vec::new();

    // Path parameters.
    for p in &op.path_params {
        properties.insert(p.name.clone(), p.schema.clone());
        if p.required {
            required_fields.push(Value::String(p.name.clone()));
        }
    }

    // Query parameters.
    for p in &op.query_params {
        properties.insert(p.name.clone(), p.schema.clone());
        if p.required {
            required_fields.push(Value::String(p.name.clone()));
        }
    }

    // Request body.
    let (body, body_from) = match &op.body {
        None => (ValueMap::new(), None),

        Some(rb) if matches!(rb.kind, RequestBodyKind::FlatObject { .. }) => {
            let RequestBodyKind::FlatObject { properties: props } = &rb.kind else {
                unreachable!()
            };
            let mut body_map = ValueMap::new();
            for p in props {
                properties.insert(p.name.clone(), p.schema.clone());
                if p.required {
                    required_fields.push(Value::String(p.name.clone()));
                }
                body_map.insert(p.name.clone(), Value::String(format!("${}", p.name)));
            }
            (body_map, None)
        }

        Some(rb) => {
            // Passthrough: add a `body` input property with the raw schema.
            let RequestBodyKind::Passthrough { schema } = &rb.kind else {
                unreachable!()
            };
            properties.insert("body".to_owned(), schema.clone());
            if rb.required {
                required_fields.push(Value::String("body".to_owned()));
            }
            (ValueMap::new(), Some(Value::String("$body".to_owned())))
        }
    };

    let mut schema: Map<String, Value> = Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !required_fields.is_empty() {
        schema.insert("required".to_owned(), Value::Array(required_fields));
    }

    (schema, body, body_from)
}

fn build_query(params: &[crate::gen::openapi::ParamInput]) -> ValueMap {
    params
        .iter()
        .map(|p| (p.name.clone(), Value::String(format!("${}", p.name))))
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiKeyLocation;
    use crate::gen::openapi::{ParamInput, RequestBodyInput};

    fn bearer_input(ops: Vec<OperationInput>) -> GeneratorInput {
        GeneratorInput {
            title: "My Cool API".to_owned(),
            version: Some("1.0.0".to_owned()),
            description: Some("Does things.".to_owned()),
            base_url: Some("https://api.example.com".to_owned()),
            auth: Some(DetectedAuth::Bearer {
                env_prefix: "MY_COOL_API".to_owned(),
            }),
            env_prefix: "MY_COOL_API".to_owned(),
            operations: ops,
        }
    }

    fn no_op_input() -> GeneratorInput {
        GeneratorInput {
            title: "Test API".to_owned(),
            version: None,
            description: None,
            base_url: None,
            auth: None,
            env_prefix: "TEST_API".to_owned(),
            operations: vec![],
        }
    }

    fn simple_op(name: &str, method: &str) -> OperationInput {
        OperationInput {
            name: name.to_owned(),
            description: Some("Does something.".to_owned()),
            method: method.to_owned(),
            path: "/things".to_owned(),
            path_params: vec![],
            query_params: vec![],
            body: None,
        }
    }

    fn param(name: &str, required: bool) -> ParamInput {
        ParamInput {
            name: name.to_owned(),
            schema: serde_json::json!({ "type": "string" }),
            required,
        }
    }

    // ── server name ───────────────────────────────────────────────────────────

    #[test]
    fn server_name_lowercased_and_spaces_become_underscores() {
        assert_eq!(sanitize_server_name("My Cool API"), "my_cool_api");
    }

    #[test]
    fn server_name_strips_leading_trailing_underscores() {
        assert_eq!(sanitize_server_name("--My API--"), "my_api");
    }

    #[test]
    fn server_name_hyphens_and_slashes_become_underscores() {
        assert_eq!(sanitize_server_name("My-Cool API v2"), "my_cool_api_v2");
    }

    // ── expose auto-select ────────────────────────────────────────────────────

    #[test]
    fn expose_auto_tools_at_boundary() {
        let ops: Vec<_> = (0..20)
            .map(|i| simple_op(&format!("op_{i}"), "GET"))
            .collect();
        let (cfg, _) = emit(&bearer_input(ops), None);
        assert_eq!(cfg.server.expose, ExposeMode::Tools);
    }

    #[test]
    fn expose_auto_dispatcher_above_threshold() {
        let ops: Vec<_> = (0..21)
            .map(|i| simple_op(&format!("op_{i}"), "GET"))
            .collect();
        let (cfg, _) = emit(&bearer_input(ops), None);
        assert_eq!(cfg.server.expose, ExposeMode::Dispatcher);
    }

    #[test]
    fn expose_override_wins() {
        let (cfg, _) = emit(&no_op_input(), Some(ExposeMode::Dispatcher));
        assert_eq!(cfg.server.expose, ExposeMode::Dispatcher);
    }

    // ── auth mapping ──────────────────────────────────────────────────────────

    #[test]
    fn bearer_auth_uses_env_prefix() {
        let (cfg, _) = emit(&bearer_input(vec![]), None);
        let auth = cfg.defaults.auth.unwrap();
        assert!(matches!(auth, Auth::Bearer { token } if token == "${MY_COOL_API_TOKEN}"));
    }

    #[test]
    fn api_key_header_auth() {
        let input = GeneratorInput {
            auth: Some(DetectedAuth::ApiKey {
                location: ApiKeyLocation::Header,
                param_name: "X-Api-Key".to_owned(),
                env_prefix: "MY_API".to_owned(),
            }),
            ..no_op_input()
        };
        let (cfg, _) = emit(&input, None);
        let auth = cfg.defaults.auth.unwrap();
        assert!(matches!(
            auth,
            Auth::ApiKey { location: ApiKeyLocation::Header, name, value }
            if name == "X-Api-Key" && value == "${MY_API_API_KEY}"
        ));
    }

    #[test]
    fn oauth2_auth_maps_correctly() {
        let input = GeneratorInput {
            auth: Some(DetectedAuth::Oauth2 {
                token_url: "https://auth.example.com/token".to_owned(),
                env_prefix: "MY_API".to_owned(),
                scopes: vec!["read".to_owned()],
            }),
            ..no_op_input()
        };
        let (cfg, _) = emit(&input, None);
        let auth = cfg.defaults.auth.unwrap();
        assert!(matches!(
            auth,
            Auth::Oauth2 { token_url, client_id, .. }
            if token_url == "https://auth.example.com/token" && client_id == "${MY_API_CLIENT_ID}"
        ));
    }

    // ── tool: flat body ───────────────────────────────────────────────────────

    #[test]
    fn flat_body_tool_builds_schema_and_body_map() {
        let op = OperationInput {
            name: "create_pet".to_owned(),
            description: None,
            method: "POST".to_owned(),
            path: "/pets".to_owned(),
            path_params: vec![],
            query_params: vec![],
            body: Some(RequestBodyInput {
                required: true,
                kind: RequestBodyKind::FlatObject {
                    properties: vec![param("name", true), param("tag", false)],
                },
            }),
        };
        let (cfg, _) = emit(&bearer_input(vec![op]), None);
        let tool = &cfg.tools[0];
        assert_eq!(tool.input_schema["type"], "object");
        assert!(tool.input_schema["properties"]["name"].is_object());
        let required = tool.input_schema["required"].as_array().unwrap();
        let req_names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(req_names.contains(&"name"));
        let http = tool.http.as_ref().unwrap();
        assert_eq!(http.body["name"], "$name");
        assert!(http.body_from.is_none());
    }

    // ── tool: passthrough body ────────────────────────────────────────────────

    #[test]
    fn passthrough_body_tool_uses_body_from() {
        let op = OperationInput {
            name: "upload".to_owned(),
            description: None,
            method: "POST".to_owned(),
            path: "/upload".to_owned(),
            path_params: vec![],
            query_params: vec![],
            body: Some(RequestBodyInput {
                required: true,
                kind: RequestBodyKind::Passthrough {
                    schema: serde_json::json!({ "type": "array", "items": { "type": "string" } }),
                },
            }),
        };
        let (cfg, _) = emit(&bearer_input(vec![op]), None);
        let tool = &cfg.tools[0];
        let http = tool.http.as_ref().unwrap();
        assert_eq!(http.body_from, Some(Value::String("$body".to_owned())));
        assert!(http.body.is_empty());
        assert!(tool.input_schema["properties"]["body"].is_object());
    }

    // ── defaults always has Accept header ────────────────────────────────────

    #[test]
    fn defaults_always_has_accept_header() {
        let (cfg, _) = emit(&no_op_input(), None);
        let accept = &cfg.defaults.headers["Accept"];
        assert_eq!(accept, "application/json");
    }
}
