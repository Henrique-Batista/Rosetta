use rosetta_acp::{AcpClient, AcpTransport};
use rosetta_types::acp::ContentBlock;

#[tokio::test]
async fn test_mock_acp_agent_end_to_end() {
    // Path to the mock agent script
    let mock_script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mock_acp.py"
    );

    let transport = AcpTransport::new("python3", &[mock_script])
        .await
        .expect("Failed to spawn mock ACP agent");

    let mut client = AcpClient::new(transport);

    // 1. Initialize
    let init_resp = client.initialize().await.expect("initialize failed");
    assert_eq!(init_resp.protocol_version, 1);

    // 2. Create session
    let session_resp = client
        .new_session("/tmp", None)
        .await
        .expect("new_session failed");
    let session_id = session_resp.session_id;
    assert_eq!(session_id, "mock-session-123");

    // 3. Send prompt
    let prompt = vec![ContentBlock::Text {
        text: "Say hello".to_string(),
    }];
    let prompt_resp = client
        .send_prompt(&session_id, prompt)
        .await
        .expect("send_prompt failed");
    assert_eq!(prompt_resp.session_id, Some(session_id.to_string()));
    assert!(prompt_resp.done.unwrap_or(false));

    // 4. Read buffered updates (two agent_thought_chunk + one tool_call + two agent_message_chunk)
    let updates = client.read_updates().await.expect("read_updates failed");
    assert_eq!(updates.len(), 5);

    let get_type = |u: &rosetta_types::acp::SessionUpdate| -> String {
        u.body.get("updateType")
            .and_then(|v| v.as_str())
            .or_else(|| u.body.get("update").and_then(|u| u.get("sessionUpdate")).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(get_type(&updates[0]), "agent_thought_chunk");
    assert_eq!(get_type(&updates[1]), "tool_call");
    assert_eq!(get_type(&updates[2]), "agent_thought_chunk");
    assert_eq!(get_type(&updates[3]), "agent_message_chunk");
    assert_eq!(get_type(&updates[4]), "agent_message_chunk");

    // 5. Close session
    client
        .close_session(&session_id)
        .await
        .expect("close_session failed");

    // 6. Shutdown transport
    client
        .shutdown()
        .await
        .expect("shutdown failed");
}
