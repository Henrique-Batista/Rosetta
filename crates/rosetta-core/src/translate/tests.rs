use super::*;
use rosetta_types::acp::{ContentBlock, SessionUpdate};
use serde_json::json;

#[test]
fn test_openai_input_text_to_acp_prompt() {
    let input = ResponseInput::Text("Hello, world!".to_string());
    let blocks = openai_input_to_acp_prompt(&input);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        ContentBlock::Text { text } => assert_eq!(text, "Hello, world!"),
        _ => panic!("Expected Text block"),
    }
}

#[test]
fn test_openai_input_items_to_acp_prompt() {
    let items = vec![
        InputItem::Message {
            role: "user".to_string(),
            content: ContentInput::Text("Hello".to_string()),
        },
        InputItem::Message {
            role: "system".to_string(),
            content: ContentInput::Text("Be helpful".to_string()),
        },
        InputItem::FunctionCallOutput {
            call_id: "call_1".to_string(),
            output: "{\"temp\": 24}".to_string(),
        },
    ];
    let input = ResponseInput::Items(items);
    let blocks = openai_input_to_acp_prompt(&input);
    
    assert_eq!(blocks.len(), 3);
    match &blocks[0] {
        ContentBlock::Text { text } => assert_eq!(text, "Hello"),
        _ => panic!("Expected Text block"),
    }
    match &blocks[1] {
        ContentBlock::Text { text } => assert_eq!(text, "[System]\nBe helpful"),
        _ => panic!("Expected Text block with system prefix"),
    }
    match &blocks[2] {
        ContentBlock::Text { text } => assert_eq!(text, "[Tool Result: call_1]\n{\"temp\": 24}"),
        _ => panic!("Expected Text block with tool result"),
    }
}

#[test]
fn test_chat_messages_to_acp_prompt() {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some("Be helpful".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some("Hello".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: Some("Hi there".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let blocks = chat_messages_to_acp_prompt(&messages);
    
    assert_eq!(blocks.len(), 3);
    match &blocks[0] {
        ContentBlock::Text { text } => assert_eq!(text, "[System]\nBe helpful"),
        _ => panic!("Expected system prefix"),
    }
    match &blocks[1] {
        ContentBlock::Text { text } => assert_eq!(text, "Hello"),
        _ => panic!("Expected no prefix for user"),
    }
    match &blocks[2] {
        ContentBlock::Text { text } => assert_eq!(text, "[Assistant]\nHi there"),
        _ => panic!("Expected assistant prefix"),
    }
}

#[test]
fn test_response_accumulator_message() {
    let mut acc = ResponseAccumulator::new("resp_123".to_string(), "gpt-4".to_string(), vec![]);
    
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({"updateType": "agent_message_chunk", "data": {"text": "Hello"}}),
    };
    let event = acc.process_update(&update);
    assert!(event.is_some());
    
    let update2 = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({"updateType": "agent_message_chunk", "data": {"text": " world"}}),
    };
    let event2 = acc.process_update(&update2);
    assert!(event2.is_some());
    
    let response = acc.finalize();
    assert_eq!(response.status, "completed");
    assert_eq!(response.output_text, "Hello world");
    assert_eq!(response.output.len(), 1);
}

#[test]
fn test_response_accumulator_internal_tool_call() {
    let mut acc = ResponseAccumulator::new("resp_123".to_string(), "gpt-4".to_string(), vec![]);
    
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "tool_call",
            "data": {
                "tool_call_id": "call_1",
                "title": "Get weather",
                "name": "get_weather",
                "arguments": "{\"city\": \"Paris\"}"
            }
        }),
    };
    let event = acc.process_update(&update);
    assert!(event.is_some());
    
    let response = acc.finalize();
    assert_eq!(response.output.len(), 1);
    match &response.output[0] {
        OutputItem::Reasoning { summary, .. } => {
            assert_eq!(summary.len(), 1);
            assert_eq!(summary[0].summary_type, "tool_call");
            assert!(summary[0].text.contains("get_weather"));
        }
        _ => panic!("Expected Reasoning for internal tool call, got {:?}", response.output[0]),
    }
}

// --- TDD RED: Tests that will fail until Wave 2 is implemented ---

#[test]
fn test_agent_thought_chunk_produces_reasoning() {
    let mut acc = ResponseAccumulator::new("resp_1".to_string(), "gpt-4".to_string(), vec![]);
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "agent_thought_chunk",
            "data": {"content": {"type": "text", "text": "I should check the weather."}}
        }),
    };
    let _event = acc.process_update(&update);
    let response = acc.finalize();

    assert!(!response.output.is_empty(), "expected at least one output item");
    let reasoning = response.output.iter().find(|item| matches!(item, OutputItem::Reasoning { .. }));
    assert!(reasoning.is_some(), "expected a Reasoning output item, got: {:?}", response.output);
    let no_message = !response.output.iter().any(|item| matches!(item, OutputItem::Message { .. }));
    assert!(no_message, "expected NO Message output item from thought chunk alone");
}

#[test]
fn test_agent_message_chunk_produces_message_only() {
    let mut acc = ResponseAccumulator::new("resp_2".to_string(), "gpt-4".to_string(), vec![]);
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "agent_message_chunk",
            "data": {"content": "Hello world"}
        }),
    };
    let _event = acc.process_update(&update);
    let response = acc.finalize();

    assert_eq!(response.output.len(), 1, "expected exactly one output item");
    match &response.output[0] {
        OutputItem::Message { content, .. } => {
            let text = content.iter().filter_map(|c| match c {
                OutputContent::OutputText { text, .. } => Some(text.as_str()),
                _ => None,
            }).collect::<String>();
            assert_eq!(text, "Hello world", "message text should match input");
        }
        _ => panic!("expected Message output item, got {:?}", response.output[0]),
    }
    assert_eq!(response.output_text, "Hello world", "output_text should match message text");
}

#[test]
fn test_thinking_not_in_output_text() {
    let mut acc = ResponseAccumulator::new("resp_3".to_string(), "gpt-4".to_string(), vec![]);

    let thought = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "agent_thought_chunk",
            "data": {"content": {"type": "text", "text": "I am thinking..."}}
        }),
    };
    let _e1 = acc.process_update(&thought);

    let msg = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "agent_message_chunk",
            "data": {"content": "Actual output"}
        }),
    };
    let _e2 = acc.process_update(&msg);

    let response = acc.finalize();

    assert!(!response.output_text.contains("I am thinking"),
        "output_text should NOT contain thinking: '{}'", response.output_text);
    assert!(response.output_text.contains("Actual output"),
        "output_text should contain message text: '{}'", response.output_text);

    let reasoning_count = response.output.iter().filter(|item| matches!(item, OutputItem::Reasoning { .. })).count();
    let message_count = response.output.iter().filter(|item| matches!(item, OutputItem::Message { .. })).count();
    assert_eq!(reasoning_count, 1, "expected 1 Reasoning item");
    assert_eq!(message_count, 1, "expected 1 Message item");
}

#[test]
fn test_internal_tool_call_produces_reasoning_not_function_call() {
    let mut acc = ResponseAccumulator::new("resp_4".to_string(), "gpt-4".to_string(), vec![]);
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "tool_call",
            "data": {
                "toolCallId": "call_weather",
                "title": "Check weather",
                "name": "get_weather",
                "arguments": "{\"city\": \"Tokyo\"}"
            }
        }),
    };
    let _event = acc.process_update(&update);
    let response = acc.finalize();

    assert_eq!(response.output.len(), 1, "expected exactly one output item");
    let has_function_call = response.output.iter().any(|item| matches!(item, OutputItem::FunctionCall { .. }));
    assert!(!has_function_call, "internal tool_call should NOT produce FunctionCall items");

    match &response.output[0] {
        OutputItem::Reasoning { summary, .. } => {
            assert_eq!(summary.len(), 1);
            assert_eq!(summary[0].summary_type, "tool_call");
            assert!(summary[0].text.contains("get_weather"), "reasoning text should mention tool name");
            assert!(summary[0].text.contains("Tokyo"), "reasoning text should mention arguments");
        }
        other => panic!("expected Reasoning item, got {other:?}"),
    }
}

#[test]
fn test_thought_then_message_then_thought_separate_items() {
    let mut acc = ResponseAccumulator::new("resp_5".to_string(), "gpt-4".to_string(), vec![]);

    let t1 = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({ "updateType": "agent_thought_chunk", "data": {"content": {"type": "text", "text": "First thought"}} }),
    };
    let _ = acc.process_update(&t1);

    let m1 = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({ "updateType": "agent_message_chunk", "data": {"content": "Output text"}}),
    };
    let _ = acc.process_update(&m1);

    let t2 = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({ "updateType": "agent_thought_chunk", "data": {"content": {"type": "text", "text": "Second thought"}} }),
    };
    let _ = acc.process_update(&t2);

    let response = acc.finalize();
    assert_eq!(response.output.len(), 3, "expected 3 output items: Reasoning, Message, Reasoning");

    assert_eq!(response.output_text, "Output text", "output_text should only contain message text");

    let reasoning_items: Vec<&OutputItem> = response.output.iter().filter(|item| matches!(item, OutputItem::Reasoning { .. })).collect();
    let message_items: Vec<&OutputItem> = response.output.iter().filter(|item| matches!(item, OutputItem::Message { .. })).collect();
    assert_eq!(reasoning_items.len(), 2, "expected 2 Reasoning items");
    assert_eq!(message_items.len(), 1, "expected 1 Message item");
}

#[test]
fn test_process_update_thought_emits_output_item_added() {
    let mut acc = ResponseAccumulator::new("resp_6".to_string(), "gpt-4".to_string(), vec![]);
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "agent_thought_chunk",
            "data": {"content": {"type": "text", "text": "Thinking..."}}
        }),
    };
    let event = acc.process_update(&update);
    assert!(event.is_some(), "process_update should return Some event for thought chunk");

    if let Some(ResponseEvent::OutputItemAdded { item, .. }) = &event {
        assert!(matches!(item, OutputItem::Reasoning { .. }),
            "expected OutputItemAdded event with Reasoning item, got {item:?}");
    } else {
        panic!("expected OutputItemAdded event, got {event:?}");
    }
}

#[test]
fn test_process_update_tool_call_emits_reasoning_event() {
    let mut acc = ResponseAccumulator::new("resp_7".to_string(), "gpt-4".to_string(), vec![]);
    let update = SessionUpdate {
        session_id: "sess_1".to_string(),
        body: json!({
            "updateType": "tool_call",
            "data": {
                "toolCallId": "call_x",
                "name": "search",
                "arguments": "{\"q\": \"test\"}"
            }
        }),
    };
    let event = acc.process_update(&update);
    assert!(event.is_some(), "process_update should return Some event for tool_call");

    if let Some(ResponseEvent::OutputItemAdded { item, .. }) = &event {
        assert!(matches!(item, OutputItem::Reasoning { .. }),
            "expected OutputItemAdded with Reasoning item for tool_call, got {item:?}");
    } else {
        panic!("expected OutputItemAdded event, got {event:?}");
    }
}

#[test]
fn test_response_to_chat_completion() {
    let response = Response {
        id: "resp_123".to_string(),
        object: "response",
        created_at: 1234567890,
        status: "completed".to_string(),
        model: "gpt-4".to_string(),
        output: vec![OutputItem::Message {
            id: "msg_1".to_string(),
            role: "assistant".to_string(),
            status: "completed".to_string(),
            content: vec![OutputContent::OutputText {
                text: "Hello".to_string(),
                annotations: vec![],
            }],
        }],
        output_text: "Hello".to_string(),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
        },
        parallel_tool_calls: true,
        error: None,
    };
    
    let chat = response_to_chat_completion(&response);
    assert_eq!(chat.object, "chat.completion");
    assert_eq!(chat.choices.len(), 1);
    assert_eq!(chat.choices[0].message.role, "assistant");
    assert_eq!(chat.choices[0].message.content, Some("Hello".to_string()));
    assert_eq!(chat.choices[0].finish_reason, "stop");
}

#[test]
fn test_generate_ids() {
    let resp_id = generate_response_id();
    assert!(resp_id.starts_with("resp_"));
    assert_eq!(resp_id.len(), 21);
    
    let msg_id = generate_message_id();
    assert!(msg_id.starts_with("msg_"));
    
    let call_id = generate_call_id();
    assert!(call_id.starts_with("call_"));
}

#[test]
fn test_response_to_chat_chunks_empty_text() {
    let response = Response {
        id: "resp_empty".to_string(),
        object: "response",
        created_at: 0,
        status: "completed".to_string(),
        model: "gpt-4".to_string(),
        output: vec![],
        output_text: String::new(),
        usage: Usage { input_tokens: 0, output_tokens: 0, total_tokens: 0 },
        parallel_tool_calls: true,
        error: None,
    };

    let chunks = response_to_chat_chunks(&response);
    // Role chunk + finish chunk
    assert_eq!(chunks.len(), 2, "expected 2 chunks: role + finish (no content)");
    assert_eq!(chunks[0].choices[0].delta.role, Some("assistant".to_string()));
    assert_eq!(chunks[1].choices[0].finish_reason, Some("stop".to_string()));
}

#[test]
fn test_response_to_chat_chunks_splits_text() {
    let response = Response {
        id: "resp_123".to_string(),
        object: "response",
        created_at: 1234567890,
        status: "completed".to_string(),
        model: "gpt-4".to_string(),
        output: vec![OutputItem::Message {
            id: "msg_1".to_string(),
            role: "assistant".to_string(),
            status: "completed".to_string(),
            content: vec![OutputContent::OutputText {
                text: "Hello world".to_string(),
                annotations: vec![],
            }],
        }],
        output_text: "Hello world".to_string(),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
        },
        parallel_tool_calls: true,
        error: None,
    };

    let chunks = response_to_chat_chunks(&response);

    // First chunk: role only, no content
    assert!(!chunks.is_empty(), "should produce at least 2 chunks");
    assert_eq!(chunks[0].choices[0].delta.role, Some("assistant".to_string()));
    assert_eq!(chunks[0].choices[0].delta.content, None);
    assert_eq!(chunks[0].choices[0].finish_reason, None);

    // Last chunk: finish_reason + usage
    let last = chunks.last().unwrap();
    assert_eq!(last.choices[0].finish_reason, Some("stop".to_string()));
    assert!(last.usage.is_some(), "last chunk should have usage");

    // Content chunks in between
    let content_chunks: Vec<&ChatCompletionChunk> = chunks[1..chunks.len()-1].iter().collect();
    assert!(!content_chunks.is_empty(), "should have content chunks between first and last");
    for chunk in &content_chunks {
        assert_eq!(chunk.choices[0].delta.role, None, "content chunks should not set role");
        assert!(chunk.choices[0].delta.content.is_some(), "content chunks should have content delta");
    }
}
