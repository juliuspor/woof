use std::path::Path;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, BufReader};
use woof_core::ApiToken;
use woof_mcp::{
    read_frame, tool_definitions, write_frame, FrameMode, McpBridge, DEFAULT_DAEMON_URL,
    MCP_PROTOCOL,
};

fn bridge() -> McpBridge {
    McpBridge::new(
        DEFAULT_DAEMON_URL,
        ApiToken::parse_file(Path::new("fixture"), vec![b'a'; 64]).expect("token"),
    )
    .expect("bridge")
}

#[tokio::test]
async fn initialize_matches_the_current_contract() {
    let response = bridge()
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "fixture", "version": "1"}
            }
        }))
        .await
        .expect("response");
    assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL);
    assert_eq!(response["result"]["serverInfo"]["name"], "woof");
    assert_eq!(
        response["result"]["capabilities"],
        json!({"prompts": {}, "resources": {}, "tools": {}})
    );
}

#[test]
fn exactly_ten_public_tools_match_the_woof_contract() {
    let tools = tool_definitions();
    assert_eq!(tools.len(), 10);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect::<Vec<_>>(),
        [
            "search_memory",
            "get_chronicle",
            "get_working_memory",
            "get_recent_activity",
            "get_snapshots",
            "search_wiki",
            "get_wiki_page",
            "list_wiki",
            "get_time_report",
            "list_time_rules",
        ]
    );
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/backend/mcp-tools.json"
    ))
    .expect("schema fixture");
    assert_eq!(Value::Array(tools), fixture);
}

#[tokio::test]
async fn framing_supports_content_length_and_newline_clients() {
    let message = json!({"jsonrpc":"2.0","id":7,"method":"ping"});
    for mode in [FrameMode::ContentLength, FrameMode::Newline] {
        let (mut writer, reader) = tokio::io::duplex(4096);
        write_frame(&mut writer, mode, &message)
            .await
            .expect("write frame");
        drop(writer);
        let mut reader = BufReader::new(reader);
        let (read_mode, body) = read_frame(&mut reader)
            .await
            .expect("read frame")
            .expect("frame");
        assert_eq!(read_mode, mode);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("JSON"),
            message
        );
        let mut trailing = Vec::new();
        reader.read_to_end(&mut trailing).await.expect("EOF");
    }
}

#[tokio::test]
async fn notifications_do_not_emit_responses() {
    assert!(bridge()
        .handle(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await
        .is_none());
}

#[tokio::test]
async fn invalid_json_rpc_version_is_rejected() {
    let response = bridge()
        .handle(json!({"jsonrpc": "1.0", "id": 9, "method": "ping"}))
        .await
        .expect("error response");
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(response["id"], 9);
}

#[tokio::test]
async fn initialize_negotiates_a_supported_version_for_newer_clients() {
    let response = bridge()
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "initialize",
            "params": {"protocolVersion": "2099-01-01"}
        }))
        .await
        .expect("error response");
    assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL);
}

#[tokio::test]
async fn initialize_echoes_a_supported_prior_version_and_rejects_malformed_values() {
    let response = bridge()
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}
        }))
        .await
        .expect("supported-version response");
    assert_eq!(response["result"]["protocolVersion"], "2024-11-05");

    for invalid in [
        json!(null),
        json!(""),
        json!("bad version"),
        json!("x".repeat(65)),
    ] {
        let response = bridge()
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 12,
                "method": "initialize",
                "params": {"protocolVersion": invalid}
            }))
            .await
            .expect("error response");
        assert_eq!(response["error"]["code"], -32602);
    }
}

#[tokio::test]
async fn stdio_session_requires_initialize_before_tools() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}\n"
    );
    let mut reader = BufReader::new(input.as_bytes());
    let (mut output_reader, output_writer) = tokio::io::duplex(128 * 1024);
    bridge()
        .serve(&mut reader, output_writer)
        .await
        .expect("serve fixture");
    let mut output = String::new();
    output_reader
        .read_to_string(&mut output)
        .await
        .expect("read output");
    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("response JSON"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["error"]["code"], -32002);
    assert_eq!(responses[1]["result"]["protocolVersion"], MCP_PROTOCOL);
    assert_eq!(
        responses[2]["result"]["tools"].as_array().unwrap().len(),
        10
    );
}
