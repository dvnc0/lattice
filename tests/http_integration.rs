//! Integration tests for the HTTP executor (T11) and auth (T12) against a `wiremock`
//! mock server.
//!
//! These assert the executor builds the right request (method, path, query, headers,
//! body, content-type), applies each auth scheme, parses and filters the response, and
//! maps non-2xx to an `is_error` outcome rather than a transport error.

use std::collections::BTreeMap;

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use lattice::config::{ApiKeyLocation, Auth, HttpTarget, ResponseSpec};
use lattice::engine::{build_request, Ctx, HttpRequestSpec};
use lattice::exec::auth::AuthState;
use lattice::exec::http::execute;
use lattice::exec::ExecError;

/// A bare request spec; tests fill in the fields they exercise.
fn spec(method: &str, url: String) -> HttpRequestSpec {
    HttpRequestSpec {
        method: method.to_string(),
        url,
        query: Vec::new(),
        headers: Vec::new(),
        body: None,
        content_type: None,
    }
}

#[tokio::test]
async fn get_returns_parsed_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "pong": true })))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let outcome = execute(
        &client,
        &spec("GET", format!("{}/ping", server.uri())),
        &ResponseSpec::default(),
        None,
    )
    .await
    .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!({ "pong": true }));
}

#[tokio::test]
async fn post_sends_json_body_and_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/users"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({ "name": "Bob", "active": true })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 7 })))
        .mount(&server)
        .await;

    let mut s = spec("POST", format!("{}/users", server.uri()));
    s.body = Some(json!({ "name": "Bob", "active": true }));
    s.content_type = Some("application/json".to_string());

    let client = reqwest::Client::new();
    let outcome = execute(&client, &s, &ResponseSpec::default(), None)
        .await
        .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!({ "id": 7 }));
}

#[tokio::test]
async fn query_and_headers_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust"))
        .and(query_param("tag", "b"))
        .and(header("x-trace", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let mut s = spec("GET", format!("{}/search", server.uri()));
    s.query = vec![
        ("q".to_string(), "rust".to_string()),
        ("tag".to_string(), "a".to_string()),
        ("tag".to_string(), "b".to_string()),
    ];
    s.headers = vec![("x-trace".to_string(), "abc".to_string())];

    let client = reqwest::Client::new();
    let outcome = execute(&client, &s, &ResponseSpec::default(), None)
        .await
        .unwrap();
    assert!(!outcome.is_error);
}

#[tokio::test]
async fn non_2xx_is_error_with_filtered_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "error": "not found", "trace": "internal-secret" })),
        )
        .mount(&server)
        .await;

    let response_spec = ResponseSpec {
        include: None,
        exclude: Some(vec!["trace".to_string()]),
    };

    let client = reqwest::Client::new();
    let outcome = execute(
        &client,
        &spec("GET", format!("{}/missing", server.uri())),
        &response_spec,
        None,
    )
    .await
    .unwrap();

    // A 404 is a tool error the model should see — not a transport failure — and the body
    // is still filtered (the internal trace is dropped).
    assert!(outcome.is_error);
    assert_eq!(outcome.value, json!({ "error": "not found" }));
}

#[tokio::test]
async fn include_filter_trims_success_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "id": 1, "name": "Bob", "secret": "x", "meta": { "ok": true } }),
        ))
        .mount(&server)
        .await;

    let response_spec = ResponseSpec {
        include: Some(vec!["id".to_string(), "meta.ok".to_string()]),
        exclude: None,
    };

    let client = reqwest::Client::new();
    let outcome = execute(
        &client,
        &spec("GET", format!("{}/user", server.uri())),
        &response_spec,
        None,
    )
    .await
    .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!({ "id": 1, "meta": { "ok": true } }));
}

#[tokio::test]
async fn non_json_body_returned_as_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/text"))
        .respond_with(ResponseTemplate::new(200).set_body_string("plain text"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let outcome = execute(
        &client,
        &spec("GET", format!("{}/text", server.uri())),
        &ResponseSpec::default(),
        None,
    )
    .await
    .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!("plain text"));
}

#[tokio::test]
async fn end_to_end_build_request_then_execute() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/42/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 3 })))
        .mount(&server)
        .await;

    // Build the spec from a config target + model input (exercises T8 + T11 together).
    let target = HttpTarget {
        method: "GET".to_string(),
        path: "/users/{id}/posts".to_string(),
        base_url: Some(server.uri()),
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body: BTreeMap::new(),
        body_from: None,
        auth: None,
        response: ResponseSpec::default(),
    };
    let input = json!({ "id": 42 });
    let request_spec = build_request(&target, &Ctx::new(&input)).unwrap();

    let client = reqwest::Client::new();
    let outcome = execute(&client, &request_spec, &target.response, None)
        .await
        .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(outcome.value, json!({ "count": 3 }));
}

#[tokio::test]
async fn path_var_special_chars_are_encoded_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    // A path var carrying `/` and a space must stay a single, encoded segment.
    let target = HttpTarget {
        method: "GET".to_string(),
        path: "/items/{id}".to_string(),
        base_url: Some(server.uri()),
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body: BTreeMap::new(),
        body_from: None,
        auth: None,
        response: ResponseSpec::default(),
    };
    let input = json!({ "id": "a/b c" });
    let request_spec = build_request(&target, &Ctx::new(&input)).unwrap();

    let client = reqwest::Client::new();
    execute(&client, &request_spec, &target.response, None)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/items/a%2Fb%20c");
}

#[tokio::test]
async fn crlf_in_header_value_is_rejected_without_leaking() {
    // A model-supplied header value with CRLF must be refused (header injection), and the
    // value (which could be a secret) must not appear in the error.
    let mut s = spec("GET", "http://127.0.0.1:1/x".to_string());
    s.headers = vec![("X-Evil".to_string(), "abc\r\nInjected: 1".to_string())];

    let client = reqwest::Client::new();
    let err = execute(&client, &s, &ResponseSpec::default(), None)
        .await
        .unwrap_err();

    assert!(matches!(err, ExecError::InvalidHeader(ref name) if name == "X-Evil"));
    assert!(!err.to_string().contains("Injected"));
}

#[tokio::test]
async fn transport_failure_is_exec_error() {
    // Port 1 refuses connections — no response is produced, so this is an ExecError, not
    // an is_error outcome.
    let client = reqwest::Client::new();
    let result = execute(
        &client,
        &spec("GET", "http://127.0.0.1:1/nope".to_string()),
        &ResponseSpec::default(),
        None,
    )
    .await;

    assert!(result.is_err());
}

// ---- T12: auth ----------------------------------------------------------------------

/// Run a GET against `path` on `server` with `auth`, returning whether the mock matched.
async fn get_with_auth(server: &MockServer, path: &str, auth: &AuthState) -> bool {
    let client = reqwest::Client::new();
    let outcome = execute(
        &client,
        &spec("GET", format!("{}{path}", server.uri())),
        &ResponseSpec::default(),
        Some(auth),
    )
    .await
    .unwrap();
    !outcome.is_error
}

#[tokio::test]
async fn auth_bearer_sets_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(header("authorization", "Bearer tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let state = AuthState::new(Auth::Bearer {
        token: "tok-123".to_string(),
    });
    // A 404 (mock miss) would surface as is_error; success proves the header was sent.
    assert!(get_with_auth(&server, "/x", &state).await);
}

#[tokio::test]
async fn auth_basic_sets_authorization_header() {
    let server = MockServer::start().await;
    // base64("user:pass") == "dXNlcjpwYXNz".
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let state = AuthState::new(Auth::Basic {
        username: "user".to_string(),
        password: "pass".to_string(),
    });
    assert!(get_with_auth(&server, "/x", &state).await);
}

#[tokio::test]
async fn auth_api_key_in_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(header("x-api-key", "sekret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let state = AuthState::new(Auth::ApiKey {
        location: ApiKeyLocation::Header,
        name: "X-API-Key".to_string(),
        value: "sekret".to_string(),
    });
    assert!(get_with_auth(&server, "/x", &state).await);
}

#[tokio::test]
async fn auth_api_key_in_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(query_param("api_key", "sekret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let state = AuthState::new(Auth::ApiKey {
        location: ApiKeyLocation::Query,
        name: "api_key".to_string(),
        value: "sekret".to_string(),
    });
    assert!(get_with_auth(&server, "/x", &state).await);
}

/// Count how many requests hit `/token`.
async fn token_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/token")
        .count()
}

fn oauth(server: &MockServer) -> AuthState {
    AuthState::new(Auth::Oauth2 {
        token_url: format!("{}/token", server.uri()),
        client_id: "cid".to_string(),
        client_secret: "csecret".to_string(),
        scopes: vec![],
    })
}

#[tokio::test]
async fn auth_oauth2_fetches_and_caches_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "T1", "expires_in": 3600 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer T1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    let state = oauth(&server);
    // Two calls; the token must be fetched once and reused.
    assert!(get_with_auth(&server, "/data", &state).await);
    assert!(get_with_auth(&server, "/data", &state).await);
    assert_eq!(token_requests(&server).await, 1);
}

#[tokio::test]
async fn auth_oauth2_refreshes_on_401() {
    let server = MockServer::start().await;
    // Token endpoint: T1 once, then T2. Deterministic because wiremock matches mounts in
    // (priority, insertion) order, first-match wins, and `up_to_n_times(1)` stops the T1
    // mock from matching after one hit — so the 2nd token request falls through to T2.
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "T1", "expires_in": 3600 })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "T2", "expires_in": 3600 })),
        )
        .mount(&server)
        .await;
    // Resource: the (expired) T1 → 401, the refreshed T2 → 200.
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer T1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "expired" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer T2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    let state = oauth(&server);
    // The 401 on T1 triggers one refresh to T2, which succeeds.
    assert!(get_with_auth(&server, "/data", &state).await);
    assert_eq!(token_requests(&server).await, 2);
}

#[tokio::test]
async fn auth_oauth2_token_endpoint_failure_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let state = oauth(&server);
    let client = reqwest::Client::new();
    let err = execute(
        &client,
        &spec("GET", format!("{}/data", server.uri())),
        &ResponseSpec::default(),
        Some(&state),
    )
    .await
    .unwrap_err();

    // A failed token fetch is an ExecError (no usable response), and must not leak the
    // client secret into the message.
    assert!(matches!(err, ExecError::Auth(_)));
    assert!(!err.to_string().contains("csecret"));
}

#[tokio::test]
async fn auth_oauth2_missing_access_token_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "no_token_here": true })))
        .mount(&server)
        .await;

    let state = oauth(&server);
    let client = reqwest::Client::new();
    let err = execute(
        &client,
        &spec("GET", format!("{}/data", server.uri())),
        &ResponseSpec::default(),
        Some(&state),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, ExecError::Auth(_)));
}

#[tokio::test]
async fn auth_oauth2_refetches_when_below_expiry_margin() {
    let server = MockServer::start().await;
    // A token that lives 1s is already inside the 60s refresh margin, so each call refetches.
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "T1", "expires_in": 1 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer T1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    let state = oauth(&server);
    assert!(get_with_auth(&server, "/data", &state).await);
    assert!(get_with_auth(&server, "/data", &state).await);
    assert_eq!(token_requests(&server).await, 2);
}
