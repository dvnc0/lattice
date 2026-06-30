//! Task T14 — config-driven tools mode, end to end over an in-process rmcp client.
//!
//! Builds a [`LatticeServer`] from a config, drives it with an rmcp client over an
//! in-memory duplex transport, and exercises the full path: server identity from config,
//! `tools/list` with verbatim `inputSchema`, and `tools/call` →
//! engine → executor → response filter → `CallToolResult`, including `isError`
//! propagation. The HTTP tool runs against a `wiremock` mock; the CLI tool runs a real
//! process.

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::json;
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use lattice::config::{parse_config, Format};
use lattice::mcp::LatticeServer;

/// A config with one HTTP tool (base URL filled in per test) and one CLI tool. The HTTP
/// tool filters its response down to `id` + `name`, dropping `secret`.
const CONFIG: &str = r#"
server:
  name: test-server
  version: 9.9.9
  instructions: Use get_user to look up users.
tools:
  - name: get_user
    description: Fetch a user by id.
    inputSchema:
      type: object
      properties:
        id:
          type: integer
      required: [id]
    http:
      method: GET
      path: /users/{id}
      base_url: __BASE_URL__
      response:
        include: [id, name]
  - name: echo_stdin
    description: Echo text supplied on stdin.
    inputSchema:
      type: object
      properties:
        text:
          type: string
    cli:
      command: cat
      stdin: $text
"#;

/// Start a [`LatticeServer`] built from `config_text` on one end of a duplex pipe and
/// return a connected rmcp client plus the server task handle (abort it when done).
async fn serve(
    config_text: &str,
) -> anyhow::Result<(RunningService<RoleClient, ()>, JoinHandle<()>)> {
    let config = parse_config(config_text, Format::Yaml)?;
    let (server_transport, client_transport) = tokio::io::duplex(8192);
    let handle = tokio::spawn(async move {
        if let Ok(running) = LatticeServer::new(config).serve(server_transport).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_transport).await?;
    Ok((client, handle))
}

#[tokio::test]
async fn tools_mode_http_roundtrip() -> anyhow::Result<()> {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42,
            "name": "Ada",
            "secret": "hunter2",
        })))
        .mount(&mock)
        .await;

    let config_text = CONFIG.replace("__BASE_URL__", &mock.uri());
    let (client, handle) = serve(&config_text).await?;

    // Server identity comes from the config's `server` block, not a build-env default.
    let info = client.peer_info().expect("server info after initialize");
    assert_eq!(info.server_info.name, "test-server");
    assert_eq!(info.server_info.version, "9.9.9");
    assert_eq!(
        info.instructions.as_deref(),
        Some("Use get_user to look up users.")
    );

    // tools/list returns the config tools, each carrying its authored schema verbatim.
    let tools = client.list_all_tools().await?;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"get_user") && names.contains(&"echo_stdin"));
    let get_user = tools.iter().find(|t| t.name == "get_user").unwrap();
    let props = get_user
        .input_schema
        .get("properties")
        .and_then(|v| v.as_object())
        .expect("inputSchema carried verbatim");
    assert!(props.contains_key("id"));

    // tools/call get_user → path var filled, request sent, response filtered to id+name.
    let args = json!({ "id": 42 }).as_object().unwrap().clone();
    let result = client
        .call_tool(CallToolRequestParams::new("get_user").with_arguments(args))
        .await?;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({ "id": 42, "name": "Ada" })),
        "response filter should keep id+name and drop secret"
    );

    // A non-2xx upstream status surfaces as a tool error result (not a transport error).
    let args = json!({ "id": 99 }).as_object().unwrap().clone();
    let missing = client
        .call_tool(CallToolRequestParams::new("get_user").with_arguments(args))
        .await?;
    assert_eq!(missing.is_error, Some(true));

    // An unknown tool also comes back as an error result, not a protocol failure.
    let unknown = client
        .call_tool(CallToolRequestParams::new("does_not_exist"))
        .await?;
    assert_eq!(unknown.is_error, Some(true));

    // Missing required input is rejected by inputSchema validation before anything runs:
    // an error result the model can correct (not a protocol error), naming the field.
    let bad = client
        .call_tool(CallToolRequestParams::new("get_user"))
        .await?;
    assert_eq!(bad.is_error, Some(true));
    let message = bad
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str())
        .unwrap_or_default();
    assert!(
        message.contains("id"),
        "validation error should name the missing field: {message}"
    );

    client.cancel().await?;
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn tools_mode_cli_roundtrip() -> anyhow::Result<()> {
    // The CLI tool needs no upstream; the base-URL placeholder is left unused here.
    let (client, handle) = serve(CONFIG).await?;

    let args = json!({ "text": "hello lattice" })
        .as_object()
        .unwrap()
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("echo_stdin").with_arguments(args))
        .await?;
    assert_eq!(result.is_error, Some(false));
    let text = result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str())
        .unwrap_or_default();
    assert_eq!(text, "hello lattice");

    client.cancel().await?;
    handle.abort();
    Ok(())
}
