//! Task T18 — Streamable HTTP transport.
//!
//! Starts a [`LatticeServer`] behind lattice's Streamable HTTP transport on an ephemeral
//! loopback port, then drives it with rmcp's reqwest-backed Streamable HTTP **client** —
//! `tools/list` + `tools/call` end to end over real HTTP, with the tool's upstream mocked
//! by `wiremock`.

use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use lattice::config::{parse_config, Format};
use lattice::mcp::LatticeServer;

/// One HTTP tool whose upstream base URL is filled in per test; the response is filtered to
/// `id` + `name`, dropping `secret`.
const CONFIG: &str = r#"
server:
  name: http-transport-test
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
"#;

#[tokio::test]
async fn http_transport_lists_and_calls_tool() -> anyhow::Result<()> {
    // Mock the tool's upstream.
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42,
            "name": "Ada",
            "secret": "hunter2",
        })))
        .mount(&upstream)
        .await;

    let config_text = CONFIG.replace("__BASE_URL__", &upstream.uri());
    let config = parse_config(&config_text, Format::Yaml)?;
    let server = LatticeServer::new(config);

    // Serve lattice over Streamable HTTP on an ephemeral loopback port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server_handle = tokio::spawn(async move {
        let _ = lattice::mcp::serve_http(listener, server).await;
    });

    // Connect an rmcp client over the Streamable HTTP transport.
    let uri = format!("http://{addr}{}", lattice::mcp::HTTP_PATH);
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri),
    );
    let client = ().serve(transport).await?;

    // The server identifies itself from the config.
    assert_eq!(
        client.peer_info().expect("server info").server_info.name,
        "http-transport-test"
    );

    // tools/list over HTTP returns the configured tool with its schema.
    let tools = client.list_all_tools().await?;
    let get_user = tools
        .iter()
        .find(|t| t.name == "get_user")
        .expect("get_user listed over HTTP");
    assert!(get_user.input_schema.contains_key("properties"));

    // tools/call over HTTP runs the full engine→exec→filter path.
    let args = json!({ "id": 42 }).as_object().unwrap().clone();
    let result = client
        .call_tool(CallToolRequestParams::new("get_user").with_arguments(args))
        .await?;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({ "id": 42, "name": "Ada" }))
    );

    client.cancel().await?;
    server_handle.abort();
    Ok(())
}
