use rosetta_types::acp::{ContentBlock, SessionUpdate};
use rosetta_types::openai::*;
use tracing::{debug, info, trace};

#[allow(clippy::unnecessary_filter_map)]
pub fn openai_input_to_acp_prompt(input: &ResponseInput) -> Vec<ContentBlock> {
    match input {
        ResponseInput::Text(text) => vec![ContentBlock::Text { text: text.clone() }],
        ResponseInput::Items(items) => items
            .iter()
            .filter_map(|item| match item {
                InputItem::Message { role, content } => {
                    let text = match content {
                        ContentInput::Text(t) => t.clone(),
                        ContentInput::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::InputText { text } => Some(text.as_str()),
            _unhandled => {
                debug!("Unhandled ACP session update type (silently dropped)");
                None
            }
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    let prefix = match role.as_str() {
                        "system" | "developer" => "[System]\n",
                        "assistant" => "[Assistant]\n",
                        _ => "",
                    };
                    Some(ContentBlock::Text {
                        text: format!("{}{}", prefix, text),
                    })
                }
                InputItem::FunctionCallOutput { call_id, output } => Some(ContentBlock::Text {
                    text: format!("[Tool Result: {}]\n{}", call_id, output),
                }),
            })
            .collect(),
    }
}

pub fn chat_messages_to_acp_prompt(messages: &[ChatMessage]) -> Vec<ContentBlock> {
    messages
        .iter()
        .map(|msg| {
            let prefix = match msg.role.as_str() {
                "system" | "developer" => "[System]\n",
                "assistant" => "[Assistant]\n",
                "tool" => "[Tool Result]\n",
                _ => "",
            };
            let text = msg.content.clone().unwrap_or_default();
            ContentBlock::Text {
                text: format!("{}{}", prefix, text),
            }
        })
        .collect()
}

pub struct ResponseAccumulator {
    pub response_id: String,
    pub model: String,
    pub current_message: Option<MessageAccumulator>,
    pub current_thought: Option<MessageAccumulator>,
    pub output_items: Vec<OutputItem>,
    pub text_buffer: String,
    pub thought_buffer: String,
    pub sequence_number: u32,
}

pub struct MessageAccumulator {
    pub id: String,
    pub text: String,
}

impl ResponseAccumulator {
    pub fn new(response_id: String, model: String) -> Self {
        Self {
            response_id,
            model,
            current_message: None,
            current_thought: None,
            output_items: Vec::new(),
            text_buffer: String::new(),
            thought_buffer: String::new(),
            sequence_number: 0,
        }
    }

    pub fn process_update(&mut self, update: &SessionUpdate) -> Option<ResponseEvent> {
        // Extract update_type from either format:
        // - body.updateType (mock agent format)
        // - body.update.sessionUpdate (real agent format)
        let update_type = update
            .body
            .get("updateType")
            .and_then(|v| v.as_str())
            .or_else(|| {
                update
                    .body
                    .get("update")
                    .and_then(|u| u.get("sessionUpdate"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");

        if let Ok(body_str) = serde_json::to_string(&update.body) {
            trace!(update_type, body = body_str.as_str(), "ACP session update received");
        }

        // Extract data/update object:
        // - body.data (mock agent format)
        // - body.update (real agent format)
        let data = update
            .body
            .get("data")
            .or_else(|| update.body.get("update"));

        // Extract messageId if present (real agent format)
        let msg_id = data
            .and_then(|d| d.get("messageId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "msg_default".to_string());

        match update_type {
            "agent_thought_chunk" => {
                debug!("agent_thought_chunk received — accumulating reasoning text");
                // Flush any pending message before starting thought
                self.flush_message();
                let is_new = self.current_thought.is_none();
                if is_new {
                    self.current_thought = Some(MessageAccumulator {
                        id: msg_id.clone(),
                        text: String::new(),
                    });
                }
                if let Some(d) = data {
                    let text = d
                        .get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            d.get("content")
                                .or_else(|| d.get("text"))
                                .and_then(|v| v.as_str())
                        });

                    if let Some(t) = text {
                        if let Some(ref mut thought) = self.current_thought {
                            thought.text.push_str(t);
                            self.thought_buffer.push_str(t);
                        }
                        if is_new {
                            return Some(ResponseEvent::OutputItemAdded {
                                sequence_number: self.next_seq(),
                                output_index: self.output_items.len(),
                                item: OutputItem::Reasoning {
                                    id: msg_id,
                                    summary: vec![],
                                },
                            });
                        }
                    }
                }
                None
            }
            "agent_message_chunk" => {
                debug!("agent_message_chunk received — accumulating output text");
                // Flush any pending thought before starting message
                self.flush_thought();
                let is_new = self.current_message.is_none();
                if is_new {
                    self.current_message = Some(MessageAccumulator {
                        id: msg_id.clone(),
                        text: String::new(),
                    });
                }
                if let Some(d) = data {
                    let text = d
                        .get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            d.get("content")
                                .or_else(|| d.get("text"))
                                .and_then(|v| v.as_str())
                        });

                    if let Some(t) = text {
                        if let Some(ref mut msg) = self.current_message {
                            msg.text.push_str(t);
                            self.text_buffer.push_str(t);
                        }
                        if is_new {
                            return Some(ResponseEvent::OutputItemAdded {
                                sequence_number: self.next_seq(),
                                output_index: self.output_items.len(),
                                item: OutputItem::Message {
                                    id: msg_id,
                                    role: "assistant".to_string(),
                                    status: "in_progress".to_string(),
                                    content: vec![],
                                },
                            });
                        } else {
                            return Some(ResponseEvent::OutputTextDelta {
                                sequence_number: self.next_seq(),
                                item_id: self.current_message.as_ref().unwrap().id.clone(),
                                output_index: self.output_items.len().saturating_sub(1),
                                content_index: 0,
                                delta: t.to_string(),
                            });
                        }
                    }
                }
                None
            }
            "tool_call" => {
                if let Some(d) = data {
                    let tool_name = d.get("name").or_else(|| d.get("title")).and_then(|v| v.as_str()).unwrap_or("unknown");
                    info!(tool_name, "ACP tool_call received — agent invoked a tool/skill");
                    let tool_call_id = d
                        .get("tool_call_id")
                        .or_else(|| d.get("toolCallId"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_default")
                        .to_string();
                    let title = d
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    let name = d
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&title)
                        .to_string();
                    let arguments = d
                        .get("arguments")
                        .or_else(|| d.get("params"))
                        .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    let summary_text = format!("Called tool: {} with arguments: {}", name, arguments);
                    let item = OutputItem::Reasoning {
                        id: format!("rs_{}", tool_call_id),
                        summary: vec![ReasoningSummary {
                            summary_type: "tool_call".to_string(),
                            text: summary_text,
                        }],
                    };
                    let output_index = self.output_items.len();
                    self.output_items.push(item.clone());
                    return Some(ResponseEvent::OutputItemAdded {
                        sequence_number: self.next_seq(),
                        output_index,
                        item,
                    });
                }
                None
            }
            _ => None,
        }
    }

    /// Flush the current thought to output_items as a Reasoning item.
    /// Called when transitioning from thought to another update type.
    fn flush_thought(&mut self) {
        if let Some(thought) = self.current_thought.take() && !thought.text.is_empty() {
            self.output_items.push(OutputItem::Reasoning {
                id: thought.id,
                summary: vec![ReasoningSummary {
                    summary_type: "thinking".to_string(),
                    text: thought.text,
                }],
            });
        }
    }

    /// Flush the current message to output_items as a Message item.
    /// Called when transitioning from message to another update type.
    fn flush_message(&mut self) {
        if let Some(msg) = self.current_message.take()
            && !msg.text.is_empty() {
                let item = OutputItem::Message {
                    id: msg.id.clone(),
                    role: "assistant".to_string(),
                    status: "completed".to_string(),
                    content: vec![OutputContent::OutputText {
                        text: msg.text.clone(),
                        annotations: vec![],
                    }],
                };
                self.output_items.push(item);
            }
    }

    pub fn finalize(&mut self) -> Response {
        // Push any accumulated thought as a Reasoning item
        if let Some(thought) = self.current_thought.take() && !thought.text.is_empty() {
            self.output_items.push(OutputItem::Reasoning {
                id: thought.id,
                summary: vec![ReasoningSummary {
                    summary_type: "thinking".to_string(),
                    text: thought.text,
                }],
            });
        }
        // Push any accumulated message as a Message item
        if let Some(msg) = self.current_message.take() {
            let output_item = OutputItem::Message {
                id: msg.id.clone(),
                role: "assistant".to_string(),
                status: "completed".to_string(),
                content: vec![OutputContent::OutputText {
                    text: msg.text.clone(),
                    annotations: vec![],
                }],
            };
            self.output_items.push(output_item);
        }
        // output_text only contains message text (not thinking)
        Response {
            id: self.response_id.clone(),
            object: "response",
            created_at: chrono::Utc::now().timestamp(),
            status: "completed".to_string(),
            model: self.model.clone(),
            output: self.output_items.clone(),
            output_text: self.text_buffer.clone(),
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            parallel_tool_calls: true,
            error: None,
        }
    }

    fn next_seq(&mut self) -> u32 {
        self.sequence_number += 1;
        self.sequence_number
    }
}

pub fn response_to_chat_completion(response: &Response) -> ChatCompletionResponse {
    let message_content = response.output_text.clone();
    ChatCompletionResponse {
        id: response.id.clone(),
        object: "chat.completion",
        created: response.created_at,
        model: response.model.clone(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(message_content),
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: ChatUsage {
            prompt_tokens: response.usage.input_tokens,
            completion_tokens: response.usage.output_tokens,
            total_tokens: response.usage.total_tokens,
        },
    }
}

pub fn response_to_streaming_events(response: &Response) -> Vec<ResponseEvent> {
    let mut events = Vec::new();
    let mut seq = 0u32;
    events.push(ResponseEvent::ResponseCreated { sequence_number: seq });
    seq += 1;
    events.push(ResponseEvent::ResponseInProgress { sequence_number: seq });
    seq += 1;
    for (output_index, item) in response.output.iter().enumerate() {
        match item {
            OutputItem::Message { id, role, status: _, content } => {
                events.push(ResponseEvent::OutputItemAdded {
                    sequence_number: seq,
                    output_index,
                    item: OutputItem::Message {
                        id: id.clone(),
                        role: role.clone(),
                        status: "in_progress".to_string(),
                        content: vec![],
                    },
                });
                seq += 1;
                for (content_index, part) in content.iter().enumerate() {
                    if let OutputContent::OutputText { text, .. } = part {
                        events.push(ResponseEvent::ContentPartAdded {
                            sequence_number: seq,
                            item_id: id.clone(),
                            output_index,
                            content_index,
                            part: OutputContent::OutputText {
                                text: String::new(),
                                annotations: vec![],
                            },
                        });
                        seq += 1;
                        for word in text.split(' ') {
                            events.push(ResponseEvent::OutputTextDelta {
                                sequence_number: seq,
                                item_id: id.clone(),
                                output_index,
                                content_index,
                                delta: format!("{} ", word),
                            });
                            seq += 1;
                        }
                        events.push(ResponseEvent::OutputTextDone {
                            sequence_number: seq,
                            item_id: id.clone(),
                            output_index,
                            content_index,
                            text: text.clone(),
                        });
                        seq += 1;
                        events.push(ResponseEvent::ContentPartDone {
                            sequence_number: seq,
                            item_id: id.clone(),
                            output_index,
                            content_index,
                        });
                        seq += 1;
                    }
                }
                events.push(ResponseEvent::OutputItemDone {
                    sequence_number: seq,
                    output_index,
                    item: item.clone(),
                });
                seq += 1;
            }
            OutputItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                status: _,
            } => {
                events.push(ResponseEvent::OutputItemAdded {
                    sequence_number: seq,
                    output_index,
                    item: OutputItem::FunctionCall {
                        id: id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        status: "in_progress".to_string(),
                    },
                });
                seq += 1;
                let arg_chunks: Vec<String> = arguments
                    .chars()
                    .collect::<Vec<_>>()
                    .chunks(10)
                    .map(|chunk| chunk.iter().collect())
                    .collect();
                for chunk in arg_chunks {
                    events.push(ResponseEvent::OutputTextDelta {
                        sequence_number: seq,
                        item_id: id.clone(),
                        output_index,
                        content_index: 0,
                        delta: chunk,
                    });
                    seq += 1;
                }
                events.push(ResponseEvent::OutputItemDone {
                    sequence_number: seq,
                    output_index,
                    item: item.clone(),
                });
                seq += 1;
            }
            OutputItem::Reasoning { id, summary } => {
                events.push(ResponseEvent::OutputItemAdded {
                    sequence_number: seq,
                    output_index,
                    item: OutputItem::Reasoning {
                        id: id.clone(),
                        summary: vec![],
                    },
                });
                seq += 1;
                events.push(ResponseEvent::OutputItemDone {
                    sequence_number: seq,
                    output_index,
                    item: OutputItem::Reasoning {
                        id: id.clone(),
                        summary: summary.clone(),
                    },
                });
                seq += 1;
            }
        }
    }
    let mut final_response = response.clone();
    final_response.status = "completed".to_string();
    events.push(ResponseEvent::ResponseCompleted {
        sequence_number: seq,
        response: final_response,
    });
    events
}

/// Split a Response into proper delta chunks for Chat Completions streaming.
/// Produces: first chunk (role only), content deltas (word-by-word), final chunk (finish_reason + usage).
pub fn response_to_chat_chunks(response: &Response) -> Vec<ChatCompletionChunk> {
    let mut chunks = Vec::new();
    let chat_id = format!("chatcmpl-{}", &response.id);

    // First chunk: role only
    chunks.push(ChatCompletionChunk {
        id: chat_id.clone(),
        object: "chat.completion.chunk",
        created: response.created_at,
        model: response.model.clone(),
        choices: vec![ChatChoiceDelta {
            index: 0,
            delta: ChatMessageDelta {
                role: Some("assistant".to_string()),
                content: None,
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    });

    // Content chunks: split by words
    let text = &response.output_text;
    if !text.is_empty() {
        for word in text.split(' ') {
            let delta_text = if word.is_empty() { " ".to_string() } else { format!("{} ", word) };
            chunks.push(ChatCompletionChunk {
                id: chat_id.clone(),
                object: "chat.completion.chunk",
                created: response.created_at,
                model: response.model.clone(),
                choices: vec![ChatChoiceDelta {
                    index: 0,
                    delta: ChatMessageDelta {
                        role: None,
                        content: Some(delta_text),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            });
        }
    }

    // Final chunk: finish_reason + usage
    chunks.push(ChatCompletionChunk {
        id: chat_id,
        object: "chat.completion.chunk",
        created: response.created_at,
        model: response.model.clone(),
        choices: vec![ChatChoiceDelta {
            index: 0,
            delta: ChatMessageDelta::default(),
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(ChatUsage {
            prompt_tokens: response.usage.input_tokens,
            completion_tokens: response.usage.output_tokens,
            total_tokens: response.usage.total_tokens,
        }),
    });

    chunks
}

pub fn generate_response_id() -> String {
    format!("resp_{}", uuid::Uuid::new_v4().to_string().replace('-', "").get(0..16).unwrap_or(""))
}

pub fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', "").get(0..16).unwrap_or(""))
}

pub fn generate_call_id() -> String {
    format!("call_{}", uuid::Uuid::new_v4().to_string().replace('-', "").get(0..16).unwrap_or(""))
}

#[cfg(test)]
mod tests {
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
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("Hi there".to_string()),
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
        let mut acc = ResponseAccumulator::new("resp_123".to_string(), "gpt-4".to_string());
        
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
    fn test_response_accumulator_tool_call() {
        let mut acc = ResponseAccumulator::new("resp_123".to_string(), "gpt-4".to_string());
        
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
            _ => panic!("Expected Reasoning for tool call, got {:?}", response.output[0]),
        }
    }

    // --- TDD RED: Tests that will fail until Wave 2 is implemented ---

    #[test]
    fn test_agent_thought_chunk_produces_reasoning() {
        let mut acc = ResponseAccumulator::new("resp_1".to_string(), "gpt-4".to_string());
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
        let mut acc = ResponseAccumulator::new("resp_2".to_string(), "gpt-4".to_string());
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
        let mut acc = ResponseAccumulator::new("resp_3".to_string(), "gpt-4".to_string());

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
    fn test_tool_call_produces_reasoning_not_function_call() {
        let mut acc = ResponseAccumulator::new("resp_4".to_string(), "gpt-4".to_string());
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
        assert!(!has_function_call, "tool_call should NOT produce FunctionCall items");

        match &response.output[0] {
            OutputItem::Reasoning { summary, .. } => {
                assert_eq!(summary.len(), 1);
                assert_eq!(summary[0].summary_type, "tool_call");
                assert!(summary[0].text.contains("get_weather"), "reasoning text should mention tool name");
                assert!(summary[0].text.contains("Tokyo"), "reasoning text should mention arguments");
            }
            other => panic!("expected Reasoning item, got {:?}", other),
        }
    }

    #[test]
    fn test_thought_then_message_then_thought_separate_items() {
        let mut acc = ResponseAccumulator::new("resp_5".to_string(), "gpt-4".to_string());

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
        let mut acc = ResponseAccumulator::new("resp_6".to_string(), "gpt-4".to_string());
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
                "expected OutputItemAdded event with Reasoning item, got {:?}", item);
        } else {
            panic!("expected OutputItemAdded event, got {:?}", event);
        }
    }

    #[test]
    fn test_process_update_tool_call_emits_reasoning_event() {
        let mut acc = ResponseAccumulator::new("resp_7".to_string(), "gpt-4".to_string());
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
                "expected OutputItemAdded with Reasoning item for tool_call, got {:?}", item);
        } else {
            panic!("expected OutputItemAdded event, got {:?}", event);
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
}
