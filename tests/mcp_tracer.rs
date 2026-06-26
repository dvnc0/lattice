//! Task T2 — rmcp tracer bullet.
//!
//! Drives the lattice server with an in-process rmcp client over an in-memory
//! duplex transport, exercising `tools/list` and `tools/call` end to end. This
//! pins the SDK API shape before the translation engine is built.

use rmcp::model::CallToolRequestParams;
use rmcp::ServiceExt;
use serde_json::json;

use lattice::mcp::LatticeServer;

#[tokio::test]
async fn tracer_lists_and_calls_ping() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);

    // Run the lattice server on the server end of the pipe.
    let server = tokio::spawn(async move {
        LatticeServer::new()
            .serve(server_transport)
            .await?
            .waiting()
            .await?;
        anyhow::Ok(())
    });

    // A trivial client ( `()` implements ClientHandler ) on the other end.
    let client = ().serve(client_transport).await?;

    // The server identifies as lattice (not the rmcp crate's build-env default).
    let server_info = client.peer_info().expect("server info after initialize");
    assert_eq!(server_info.server_info.name, "lattice");

    // tools/list exposes the hardcoded ping tool, schema included.
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 1, "expected exactly the ping tool");
    assert_eq!(tools[0].name.as_ref(), "ping");
    assert!(
        tools[0].input_schema.contains_key("properties"),
        "ping tool should carry its input schema"
    );

    // tools/call ping echoes the supplied message.
    let args = json!({ "message": "hello" })
        .as_object()
        .expect("object literal")
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("ping").with_arguments(args))
        .await?;

    assert_eq!(result.is_error, Some(false));
    let text = result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str())
        .unwrap_or_default();
    assert_eq!(text, "pong: hello");

    // An unknown tool comes back as an error result, not a transport failure.
    let unknown = client
        .call_tool(CallToolRequestParams::new("does_not_exist"))
        .await?;
    assert_eq!(unknown.is_error, Some(true));

    client.cancel().await?;
    server.abort();
    Ok(())
}
