//! Unit tests for the generate emitter using hand-constructed GeneratorInput.

use lattice::config::{ApiKeyLocation, Auth, ExposeMode};
use lattice::gen::emit::emit;
use lattice::gen::openapi::{
    DetectedAuth, GeneratorInput, OperationInput, ParamInput, RequestBodyInput, RequestBodyKind,
};
use serde_json::Value;

fn no_auth_input(ops: Vec<OperationInput>) -> GeneratorInput {
    GeneratorInput {
        title: "Test API".to_owned(),
        version: Some("1.0.0".to_owned()),
        description: Some("A test API.".to_owned()),
        base_url: Some("https://api.test.example.com".to_owned()),
        auth: None,
        env_prefix: "TEST_API".to_owned(),
        operations: ops,
    }
}

fn bearer_input(ops: Vec<OperationInput>) -> GeneratorInput {
    GeneratorInput {
        auth: Some(DetectedAuth::Bearer {
            env_prefix: "TEST_API".to_owned(),
        }),
        ..no_auth_input(ops)
    }
}

fn str_param(name: &str, required: bool) -> ParamInput {
    ParamInput {
        name: name.to_owned(),
        schema: serde_json::json!({ "type": "string" }),
        required,
    }
}

fn simple_op(name: &str, method: &str, path: &str) -> OperationInput {
    OperationInput {
        name: name.to_owned(),
        description: Some(format!("Does {name}.")),
        method: method.to_owned(),
        path: path.to_owned(),
        path_params: vec![],
        query_params: vec![],
        body: None,
    }
}

// ── Server block ────────────────���─────────────────────────────────────────────

#[test]
fn server_name_sanitized() {
    let (cfg, _) = emit(&no_auth_input(vec![]), None);
    assert_eq!(cfg.server.name, "test_api");
}

#[test]
fn server_version_forwarded() {
    let (cfg, _) = emit(&no_auth_input(vec![]), None);
    assert_eq!(cfg.server.version.as_deref(), Some("1.0.0"));
}

#[test]
fn server_description_forwarded() {
    let (cfg, _) = emit(&no_auth_input(vec![]), None);
    assert_eq!(cfg.server.instructions.as_deref(), Some("A test API."));
}

// ── Expose auto-select ─────────────────────��─────────────────────────────────���

#[test]
fn expose_tools_at_threshold() {
    let ops: Vec<_> = (0..20)
        .map(|i| simple_op(&format!("op_{i}"), "GET", "/x"))
        .collect();
    let (cfg, _) = emit(&no_auth_input(ops), None);
    assert_eq!(cfg.server.expose, ExposeMode::Tools);
}

#[test]
fn expose_dispatcher_above_threshold() {
    let ops: Vec<_> = (0..21)
        .map(|i| simple_op(&format!("op_{i}"), "GET", "/x"))
        .collect();
    let (cfg, _) = emit(&no_auth_input(ops), None);
    assert_eq!(cfg.server.expose, ExposeMode::Dispatcher);
}

#[test]
fn expose_override_wins() {
    let (cfg, _) = emit(&no_auth_input(vec![]), Some(ExposeMode::Dispatcher));
    assert_eq!(cfg.server.expose, ExposeMode::Dispatcher);
}

// ── Auth mapping ──────────────────��───────────────────────────────────────────

#[test]
fn bearer_auth_env_var() {
    let (cfg, _) = emit(&bearer_input(vec![]), None);
    let auth = cfg.defaults.auth.unwrap();
    assert!(
        matches!(auth, Auth::Bearer { ref token } if token == "${TEST_API_TOKEN}"),
        "unexpected: {auth:?}"
    );
}

#[test]
fn basic_auth_env_vars() {
    let input = GeneratorInput {
        auth: Some(DetectedAuth::Basic {
            env_prefix: "MY_SVC".to_owned(),
        }),
        ..no_auth_input(vec![])
    };
    let (cfg, _) = emit(&input, None);
    let auth = cfg.defaults.auth.unwrap();
    assert!(
        matches!(auth, Auth::Basic { ref username, ref password }
            if username == "${MY_SVC_USER}" && password == "${MY_SVC_PASS}"),
        "unexpected: {auth:?}"
    );
}

#[test]
fn api_key_header_auth() {
    let input = GeneratorInput {
        auth: Some(DetectedAuth::ApiKey {
            location: ApiKeyLocation::Header,
            param_name: "X-Api-Key".to_owned(),
            env_prefix: "MY_SVC".to_owned(),
        }),
        ..no_auth_input(vec![])
    };
    let (cfg, _) = emit(&input, None);
    let auth = cfg.defaults.auth.unwrap();
    assert!(
        matches!(auth, Auth::ApiKey { location: ApiKeyLocation::Header, ref name, ref value }
            if name == "X-Api-Key" && value == "${MY_SVC_API_KEY}"),
        "unexpected: {auth:?}"
    );
}

#[test]
fn api_key_query_auth() {
    let input = GeneratorInput {
        auth: Some(DetectedAuth::ApiKey {
            location: ApiKeyLocation::Query,
            param_name: "api_key".to_owned(),
            env_prefix: "MY_SVC".to_owned(),
        }),
        ..no_auth_input(vec![])
    };
    let (cfg, _) = emit(&input, None);
    let auth = cfg.defaults.auth.unwrap();
    assert!(
        matches!(
            auth,
            Auth::ApiKey {
                location: ApiKeyLocation::Query,
                ..
            }
        ),
        "unexpected: {auth:?}"
    );
}

#[test]
fn oauth2_auth() {
    let input = GeneratorInput {
        auth: Some(DetectedAuth::Oauth2 {
            token_url: "https://auth.example.com/token".to_owned(),
            env_prefix: "MY_SVC".to_owned(),
            scopes: vec!["read".to_owned()],
        }),
        ..no_auth_input(vec![])
    };
    let (cfg, _) = emit(&input, None);
    let auth = cfg.defaults.auth.unwrap();
    assert!(
        matches!(auth, Auth::Oauth2 { ref token_url, ref client_id, ..}
            if token_url == "https://auth.example.com/token"
                && client_id == "${MY_SVC_CLIENT_ID}"),
        "unexpected: {auth:?}"
    );
}

// ── Defaults ─────────────────────────────────────────────────���────────────────

#[test]
fn defaults_always_has_accept_header() {
    let (cfg, _) = emit(&no_auth_input(vec![]), None);
    assert_eq!(cfg.defaults.headers["Accept"], "application/json");
}

#[test]
fn defaults_base_url_forwarded() {
    let (cfg, _) = emit(&no_auth_input(vec![]), None);
    assert_eq!(
        cfg.defaults.base_url.as_deref(),
        Some("https://api.test.example.com")
    );
}

// ── Tool: flat body ───────────────────��───────────────────────────────────────

#[test]
fn flat_body_tool() {
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
                properties: vec![str_param("name", true), str_param("tag", false)],
            },
        }),
    };
    let (cfg, _) = emit(&bearer_input(vec![op]), None);
    let tool = &cfg.tools[0];
    assert_eq!(tool.input_schema["type"], "object");
    assert!(tool.input_schema["properties"]["name"].is_object());
    assert!(tool.input_schema["properties"]["tag"].is_object());
    let required: Vec<&str> = tool.input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(required.contains(&"name"), "{required:?}");
    assert!(!required.contains(&"tag"), "{required:?}");
    let http = tool.http.as_ref().unwrap();
    assert_eq!(http.body["name"], "$name");
    assert!(http.body_from.is_none());
}

// ── Tool: passthrough body ───────────────────────���───────────────────────────��

#[test]
fn passthrough_body_tool() {
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

// ── Tool: path + query params ─────────────────────────────────────────────────

#[test]
fn path_and_query_params_in_schema() {
    let op = OperationInput {
        name: "get_pet".to_owned(),
        description: None,
        method: "GET".to_owned(),
        path: "/pets/{petId}".to_owned(),
        path_params: vec![str_param("petId", true)],
        query_params: vec![str_param("format", false)],
        body: None,
    };
    let (cfg, _) = emit(&bearer_input(vec![op]), None);
    let tool = &cfg.tools[0];
    assert!(tool.input_schema["properties"]["petId"].is_object());
    assert!(tool.input_schema["properties"]["format"].is_object());
    let http = tool.http.as_ref().unwrap();
    assert_eq!(http.query["format"], "$format");
}
