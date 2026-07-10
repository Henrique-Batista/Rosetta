use futures::StreamExt;
use rosetta_acp::{AcpClient, AcpStreamItem, AcpTransport};
use rosetta_types::acp::ContentBlock;
use std::time::Duration;

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

    // 4. Read buffered updates (two agent_thought_chunk + one tool_call + one tool_call_update + two agent_message_chunk)
    let updates = client.read_updates().await.expect("read_updates failed");
    assert_eq!(updates.len(), 6);

    let get_type = |u: &rosetta_types::acp::SessionUpdate| -> String {
        u.body.get("updateType")
            .and_then(|v| v.as_str())
            .or_else(|| u.body.get("update").and_then(|u| u.get("sessionUpdate")).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(get_type(&updates[0]), "agent_thought_chunk");
    assert_eq!(get_type(&updates[1]), "tool_call");
    assert_eq!(get_type(&updates[2]), "tool_call_update");
    assert_eq!(get_type(&updates[3]), "agent_thought_chunk");
    assert_eq!(get_type(&updates[4]), "agent_message_chunk");
    assert_eq!(get_type(&updates[5]), "agent_message_chunk");

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

fn mock_script_path() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mock_acp.py")
}

#[tokio::test]
async fn test_send_prompt_streaming_yields_completed_on_normal_end() {
    let transport = AcpTransport::new("python3", &[mock_script_path()])
        .await
        .expect("Failed to spawn mock ACP agent");
    let mut client = AcpClient::new(transport);
    client.initialize().await.expect("initialize failed");
    let session_resp = client
        .new_session("/tmp", None)
        .await
        .expect("new_session failed");
    let session_id = session_resp.session_id;

    let prompt = vec![ContentBlock::Text {
        text: "Say hello".to_string(),
    }];
    let mut items = Vec::new();
    {
        let stream = client.send_prompt_streaming(&session_id, prompt);
        let mut stream = Box::pin(stream);
        while let Some(item) = stream.next().await {
            items.push(item);
        }
    }

    let update_count = items
        .iter()
        .filter(|i| matches!(i, AcpStreamItem::Update(_)))
        .count();
    assert_eq!(update_count, 6, "expected 6 forwarded session updates");
    assert!(
        matches!(items.last(), Some(AcpStreamItem::Completed)),
        "expected the last stream item to be Completed on normal end, got {:?}",
        items.last()
    );

    client.shutdown().await.expect("shutdown failed");
}

#[tokio::test]
async fn test_send_prompt_streaming_yields_disconnected_on_agent_crash() {
    let env_vars = vec![("MOCK_ACP_CRASH_AFTER_CHUNKS".to_string(), "2".to_string())];
    let transport = AcpTransport::new_with_env("python3", &[mock_script_path()], &env_vars)
        .await
        .expect("Failed to spawn mock ACP agent with crash env");
    let mut client = AcpClient::new(transport);
    client.initialize().await.expect("initialize failed");
    let session_resp = client
        .new_session("/tmp", None)
        .await
        .expect("new_session failed");
    let session_id = session_resp.session_id;

    let prompt = vec![ContentBlock::Text {
        text: "Say hello".to_string(),
    }];
    let mut items = Vec::new();
    {
        let stream = client.send_prompt_streaming(&session_id, prompt);
        let mut stream = Box::pin(stream);
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(item) = stream.next().await {
                items.push(item);
            }
        })
        .await;
        assert!(result.is_ok(), "stream should terminate within 5s, not hang");
    }

    let update_count = items
        .iter()
        .filter(|i| matches!(i, AcpStreamItem::Update(_)))
        .count();
    assert_eq!(update_count, 2, "expected exactly 2 chunks before crash");
    assert!(
        matches!(items.last(), Some(AcpStreamItem::Disconnected)),
        "expected the last stream item to be Disconnected on agent crash, got {:?}",
        items.last()
    );

    let _ = client.shutdown().await;
}
