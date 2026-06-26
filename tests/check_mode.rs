//! Task T5 — `check` mode.
//!
//! Exercises each validation path via `check_str` with inline configs (no temp files).

use lattice::config::{check_str, ExposeMode, Format};

fn check(yaml: &str) -> lattice::config::CheckReport {
    check_str(yaml, Format::Yaml).expect("config parses")
}

fn any_contains(items: &[String], needle: &str) -> bool {
    items.iter().any(|item| item.contains(needle))
}

#[test]
fn good_config_passes() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: get_user
    description: Get a user.
    inputSchema:
      type: object
      properties:
        id: { type: string }
      required: [id]
    http: { method: GET, path: "/users/{id}" }
"#,
    );
    assert!(report.is_valid(), "unexpected errors: {:?}", report.errors);
    assert_eq!(report.tool_count, 1);
    assert_eq!(report.expose, ExposeMode::Tools);
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
}

#[test]
fn rejects_both_and_neither_targets() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: both
    inputSchema: { type: object }
    http: { method: GET, path: /x }
    cli: { command: ls }
  - name: neither
    inputSchema: { type: object }
"#,
    );
    assert!(!report.is_valid());
    assert!(any_contains(&report.errors, "both"));
    assert!(any_contains(&report.errors, "neither"));
}

#[test]
fn rejects_invalid_input_schema() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: 5 }
    http: { method: GET, path: /x }
"#,
    );
    assert!(!report.is_valid());
    assert!(
        any_contains(&report.errors, "inputSchema"),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn reports_missing_env() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: object }
    http:
      method: GET
      path: /x
      headers: { Authorization: "Bearer ${LATTICE_NONEXISTENT_ENV_VAR_XYZ}" }
"#,
    );
    assert!(!report.is_valid());
    assert!(any_contains(
        &report.errors,
        "LATTICE_NONEXISTENT_ENV_VAR_XYZ"
    ));
}

#[test]
fn warns_on_malformed_placeholder() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: object }
    http: { method: GET, path: "/x/${bad name}" }
"#,
    );
    assert!(report.is_valid(), "errors: {:?}", report.errors);
    assert!(any_contains(&report.warnings, "malformed"));
}

#[test]
fn rejects_include_and_exclude() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: object }
    http:
      method: GET
      path: /x
      response: { include: [a], exclude: [b] }
"#,
    );
    assert!(!report.is_valid());
    assert!(any_contains(&report.errors, "include"));
}

#[test]
fn rejects_body_and_body_from() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: object }
    http:
      method: POST
      path: /x
      body: { a: "$x" }
      body_from: "$payload"
"#,
    );
    assert!(!report.is_valid());
    assert!(any_contains(&report.errors, "body_from"));
}

#[test]
fn rejects_unknown_auth_key() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t
    inputSchema: { type: object }
    http:
      method: GET
      path: /x
      auth:
        type: oauth2
        token_url: "u"
        client_id: "i"
        client_secret: "s"
        scope: "read"
"#,
    );
    assert!(!report.is_valid());
    assert!(
        any_contains(&report.errors, "scope"),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn warns_on_no_tools() {
    let report = check("server: { name: api }\n");
    assert!(report.is_valid());
    assert_eq!(report.tool_count, 0);
    assert!(any_contains(&report.warnings, "no tools"));
}

#[test]
fn propagates_parse_error() {
    let result = check_str("server: { name: api }\nbogus: 1\n", Format::Yaml);
    assert!(result.is_err());
}

#[test]
fn validates_json_format() {
    let report = check_str(
        r#"{ "server": { "name": "api" },
            "tools": [ { "name": "t", "inputSchema": { "type": "object" },
                         "http": { "method": "GET", "path": "/x" } } ] }"#,
        Format::Json,
    )
    .expect("config parses");
    assert!(report.is_valid(), "errors: {:?}", report.errors);
    assert_eq!(report.tool_count, 1);
}

/// Drift guard: a fully-populated auth block of every type must pass with zero
/// unknown-key errors. This fails the moment a field is added to `Auth` but not to
/// `check_one_auth`'s allowed-key list, catching that silent drift.
#[test]
fn fully_populated_auth_has_no_unknown_keys() {
    let report = check(
        r#"
server: { name: api }
tools:
  - name: t_bearer
    inputSchema: { type: object }
    http: { method: GET, path: /a, auth: { type: bearer, token: "t" } }
  - name: t_basic
    inputSchema: { type: object }
    http: { method: GET, path: /b, auth: { type: basic, username: "u", password: "p" } }
  - name: t_apikey
    inputSchema: { type: object }
    http: { method: GET, path: /c, auth: { type: api_key, in: header, name: "X-Key", value: "v" } }
  - name: t_oauth2
    inputSchema: { type: object }
    http:
      method: GET
      path: /d
      auth:
        type: oauth2
        token_url: "u"
        client_id: "i"
        client_secret: "s"
        scopes: [read, write]
"#,
    );
    assert!(
        !any_contains(&report.errors, "unknown field"),
        "a real auth field was flagged as unknown — check_one_auth has drifted from Auth: {:?}",
        report.errors
    );
    assert!(report.is_valid(), "unexpected errors: {:?}", report.errors);
}
