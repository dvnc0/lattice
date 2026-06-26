//! Task T3/T4 — config model + load (+ `${ENV}` interpolation through `load_config`).
//!
//! Verifies that the YAML and JSON fixtures parse into the *same* `Config`, that
//! server-level defaults merge into HTTP tools, that `${ENV}` references resolve
//! through the full load pipeline, and that the headline config features (expose
//! mode, nested body map, path vars, oauth2 auth, response filtering) survive.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Once;

use lattice::config::{load_config, Auth, Config, ExposeMode, ParseMode, Target};
use serde_json::json;

/// The fixtures reference these via `${ENV}`. Set them exactly once; `Once` provides
/// the happens-before so every (parallel) test reads them only after the single write.
///
/// Caveat: this sets the vars process-wide for the whole test binary, so any future
/// test here that needs the `MissingEnv` path through `load_config` must not depend on
/// these being unset. (`std::env::set_var` also becomes `unsafe` in edition 2024.)
static ENV: Once = Once::new();
fn load(name: &str) -> Config {
    ENV.call_once(|| {
        std::env::set_var("EXAMPLE_CLIENT_ID", "client-123");
        std::env::set_var("EXAMPLE_CLIENT_SECRET", "secret-xyz");
    });
    load_config(&fixture(name)).expect("config loads")
}

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn yaml_and_json_parse_identically() {
    assert_eq!(
        load("example.yaml"),
        load("example.json"),
        "the YAML and JSON fixtures must deserialize to the same Config"
    );
}

#[test]
fn server_and_expose_mode_parse() {
    let config = load("example.yaml");
    assert_eq!(config.server.name, "example-api");
    assert_eq!(config.server.version.as_deref(), Some("0.1.0"));
    assert_eq!(config.server.expose, ExposeMode::Dispatcher);
    assert_eq!(config.tools.len(), 2);
}

#[test]
fn defaults_merge_into_http_tool() {
    let config = load("example.json");
    let Target::Http(http) = config.tools[0].target().unwrap() else {
        panic!("update_user should be an HTTP tool");
    };

    // base_url inherited from defaults.
    assert_eq!(http.base_url.as_deref(), Some("https://api.example.com"));

    // Default header present AND the tool's own header, tool not clobbered.
    let mut expected_headers = BTreeMap::new();
    expected_headers.insert("Accept".to_string(), json!("application/json"));
    expected_headers.insert("X-Source".to_string(), json!("lattice"));
    assert_eq!(http.headers, expected_headers);

    // oauth2 auth inherited from defaults, with ${ENV} interpolated by load_config.
    match http.auth.as_ref().expect("auth inherited") {
        Auth::Oauth2 {
            token_url,
            client_id,
            scopes,
            ..
        } => {
            assert_eq!(token_url, "https://auth.example.com/token");
            assert_eq!(client_id, "client-123"); // ${EXAMPLE_CLIENT_ID} resolved
            assert_eq!(scopes, &vec!["read".to_string(), "write".to_string()]);
        }
        other => panic!("expected oauth2 auth, got {other:?}"),
    }
}

#[test]
fn nested_body_and_path_var_preserved() {
    let config = load("example.yaml");
    let Target::Http(http) = config.tools[0].target().unwrap() else {
        panic!("expected HTTP tool");
    };
    assert_eq!(http.path, "/user/{userId}/update");
    // `$firstName` is an input ref — left untouched by env interpolation.
    assert_eq!(http.body.get("user.name.first"), Some(&json!("$firstName")));
    assert_eq!(http.body.get("source"), Some(&json!("lattice")));
    assert_eq!(
        http.response.include.as_ref().unwrap(),
        &vec![
            "id".to_string(),
            "user.name".to_string(),
            "updatedAt".to_string()
        ]
    );
}

#[test]
fn cli_tool_parses_and_skips_defaults() {
    let config = load("example.json");
    let Target::Cli(cli) = config.tools[1].target().unwrap() else {
        panic!("expected CLI tool");
    };
    assert_eq!(cli.command, "ls");
    assert_eq!(cli.args, vec![json!("-la"), json!("$dir")]);
    assert_eq!(cli.parse, ParseMode::Json);
    assert_eq!(
        cli.response.exclude.as_ref().unwrap(),
        &vec!["permissions".to_string()]
    );
}
