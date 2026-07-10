use std::collections::{HashMap, HashSet};

use rosetta_types::acp::SessionUpdate;
use rosetta_types::openai::*;
use tracing::{debug, info, trace};

use super::helpers::{extract_tool_call_arguments, format_tool_reasoning_text, generate_call_id};

pub struct ResponseAccumulator {
    pub response_id: String,
    pub model: String,
    pub current_message: Option<MessageAccumulator>,
    pub current_thought: Option<MessageAccumulator>,
    pub output_items: Vec<OutputItem>,
    pub text_buffer: String,
    pub thought_buffer: String,
    pub sequence_number: u32,
    pub consumer_tool_names: Vec<String>,
    tool_names_by_call: HashMap<String, String>,
}

pub struct MessageAccumulator {
    pub id: String,
    pub text: String,
}

impl ResponseAccumulator {
    pub fn new(response_id: String, model: String, consumer_tool_names: Vec<String>) -> Self {
        Self {
            response_id,
            model,
            current_message: None,
            current_thought: None,
            output_items: Vec::new(),
            text_buffer: String::new(),
            thought_buffer: String::new(),
            sequence_number: 0,
            consumer_tool_names,
            tool_names_by_call: HashMap::new(),
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
            .map_or_else(|| "msg_default".to_string(), |s| s.to_string());

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
                        } else {
                            return Some(ResponseEvent::OutputTextDelta {
                                sequence_number: self.next_seq(),
                                item_id: self.current_thought.as_ref().unwrap().id.clone(),
                                output_index: self.output_items.len(),
                                content_index: 0,
                                delta: t.to_string(),
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
                                output_index: self.output_items.len(),
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

                    // Branch: client-defined tool → FunctionCall, agent-internal → Reasoning
                    if self.consumer_tool_names.iter().any(|n| n == &name) {
                        self.tool_names_by_call.insert(tool_call_id.clone(), name.clone());
                        let arguments = extract_tool_call_arguments(d);
                        // Skip probing calls with no arguments
                        if arguments == "{}" {
                            debug!(tool_name = %name, "ACP client tool_call (probing, no args) — silently dropped");
                            return None;
                        }
                        info!(tool_name = %name, "ACP client tool_call — forwarding as FunctionCall");
                        let call_id = tool_call_id.clone();
                        let item = OutputItem::FunctionCall {
                            id: generate_call_id(),
                            call_id,
                            name: name.clone(),
                            arguments,
                            status: "completed".to_string(),
                        };
                        let output_index = self.output_items.len();
                        self.output_items.push(item.clone());
                        return Some(ResponseEvent::OutputItemAdded {
                            sequence_number: self.next_seq(),
                            output_index,
                            item,
                        });
                    } else {
                        self.tool_names_by_call.insert(tool_call_id.clone(), name.clone());
                        let arguments = extract_tool_call_arguments(d);
                        // Skip probing calls — tool_call_update will fill in real args
                        if arguments.trim() == "{}" {
                            return None;
                        }
                        let summary_text = format_tool_reasoning_text(&name, &arguments);
                        let item = OutputItem::Reasoning {
                            id: format!("rs_{tool_call_id}"),
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
                }
                None
            }
            _ => None,
        }
    }

    /// Live-streaming variant of `process_update` that yields a `Vec` so
    /// both the item-added event and the first delta can be yielded together.
    pub fn process_update_events(&mut self, update: &SessionUpdate) -> Vec<ResponseEvent> {
        let update_type = Self::update_type_of(update);
        let is_first_message_chunk = self.current_message.is_none() && update_type == "agent_message_chunk";
        let is_first_thought_chunk = self.current_thought.is_none() && update_type == "agent_thought_chunk";
        let added_event = self.process_update(update);
        let is_item_added_message = matches!(&added_event, Some(ResponseEvent::OutputItemAdded { item: OutputItem::Message { .. }, .. }));
        let is_item_added_reasoning = matches!(&added_event, Some(ResponseEvent::OutputItemAdded { item: OutputItem::Reasoning { .. }, .. }));

        let mut events = match added_event {
            Some(event) => vec![event],
            None => vec![],
        };

        if is_first_message_chunk && is_item_added_message
            && let Some(msg) = self.current_message.as_ref().filter(|m| !m.text.is_empty())
        {
            let item_id = msg.id.clone();
            let delta = msg.text.clone();
            let output_index = self.output_items.len();
            events.push(ResponseEvent::OutputTextDelta {
                sequence_number: self.next_seq(),
                item_id,
                output_index,
                content_index: 0,
                delta,
            });
        }

        if is_first_thought_chunk && is_item_added_reasoning
            && let Some(thought) = self.current_thought.as_ref().filter(|t| !t.text.is_empty())
        {
            let item_id = thought.id.clone();
            let delta = thought.text.clone();
            let output_index = self.output_items.len();
            events.push(ResponseEvent::OutputTextDelta {
                sequence_number: self.next_seq(),
                item_id,
                output_index,
                content_index: 0,
                delta,
            });
        }

        // tool_call_update: update arguments on the matching output item
        if update_type == "tool_call_update" {
            let data = update
                .body
                .get("data")
                .or_else(|| update.body.get("update"));
            if let Some(d) = data {
                let update_call_id = d
                    .get("tool_call_id")
                    .or_else(|| d.get("toolCallId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_name = d
                    .get("name")
                    .or_else(|| d.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_args = extract_tool_call_arguments(d);
                if update_call_id.is_empty() || new_args.is_empty() {
                    // skip
                } else {
                    // Update matching FunctionCall's arguments, or create one
                    // if the initial tool_call was probing and was skipped.
                    {
                        let mut found = false;
                        for item in &mut self.output_items {
                            if let OutputItem::FunctionCall { call_id, arguments, .. } = item
                                && call_id == update_call_id
                            {
                                if new_args.trim() != "{}" {
                                    arguments.clone_from(&new_args);
                                }
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            let call_name = self.tool_names_by_call.get(update_call_id).cloned();
                            if let Some(ref name) = call_name
                                && self.consumer_tool_names.iter().any(|n| n == name)
                                && new_args.trim() != "{}"
                            {
                                self.output_items.push(OutputItem::FunctionCall {
                                    id: generate_call_id(),
                                    call_id: update_call_id.to_string(),
                                    name: name.clone(),
                                    arguments: new_args.clone(),
                                    status: "completed".to_string(),
                                });
                            }
                        }
                    }
                    // Update or create Reasoning item (agent-internal tool call).
                    // Skip completed updates when reasoning already exists:
                    // completed update carries tool *output* (huge), not input args.
                    let status = d.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let id_prefix = format!("rs_{update_call_id}");
                    let already_exists = self.output_items.iter().any(|item| {
                        matches!(item, OutputItem::Reasoning { id, .. } if id.starts_with(&id_prefix))
                    });
                    if status == "completed" && already_exists {
                        // skip — keep the smaller input-args reasoning item
                    } else {
                        let stored = self.tool_names_by_call.get(update_call_id);
                        let display_name = stored.map_or(tool_name, |s| s.as_str());
                        if display_name.is_empty() {
                            debug!(?update_call_id, ?stored, ?tool_name, ?status, "EMPTY tool name in tool_call_update reasoning creation");
                        }
                        let updated_text = format_tool_reasoning_text(display_name, &new_args);
                        let mut found = false;
                        for item in &mut self.output_items {
                            if let OutputItem::Reasoning { id, summary } = item
                                && id.starts_with(&id_prefix)
                            {
                                if let Some(s) = summary.first_mut() {
                                    s.text.clone_from(&updated_text);
                                }
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            let item = OutputItem::Reasoning {
                                id: id_prefix,
                                summary: vec![ReasoningSummary {
                                    summary_type: "tool_call".to_string(),
                                    text: updated_text,
                                }],
                            };
                            self.output_items.push(item);
                        }
                    }
                }
            }
        }

        events
    }

    fn update_type_of(update: &SessionUpdate) -> &str {
        update
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
            .unwrap_or("")
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

pub struct ChatChunkAccumulator {
    pub chat_id: String,
    pub model: String,
    pub created: i64,
    pub role_sent: bool,
    pub consumer_tool_names: Vec<String>,
    pub had_client_tool_call: bool,
    consumer_call_ids: HashMap<String, String>,
    agent_call_ids: HashSet<String>,
    agent_reasoning_emitted: HashSet<String>,
    last_reasoning_kind: Option<u8>, // 1=thought, 2=tool, None=none
}

impl ChatChunkAccumulator {
    pub fn new(chat_id: String, model: String, created: i64, consumer_tool_names: Vec<String>) -> Self {
        Self { chat_id, model, created, role_sent: false, consumer_tool_names, had_client_tool_call: false, consumer_call_ids: HashMap::new(), agent_call_ids: HashSet::new(), agent_reasoning_emitted: HashSet::new(), last_reasoning_kind: None }
    }

    pub fn process_update(&mut self, update: &SessionUpdate) -> Option<ChatCompletionChunk> {
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

        let data = update
            .body
            .get("data")
            .or_else(|| update.body.get("update"));

        match update_type {
            "agent_thought_chunk" => {
                let mut text = data.and_then(|d| {
                    d.get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            d.get("content").or_else(|| d.get("text")).and_then(|v| v.as_str())
                        })
                })?.to_string();

                let is_new_batch = self.role_sent && self.last_reasoning_kind != Some(1);
                self.last_reasoning_kind = Some(1);

                if is_new_batch {
                    text = format!("\n\n{text}");
                }

                if !self.role_sent {
                    self.role_sent = true;
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: Some("assistant".to_string()),
                        content: None,
                        reasoning_content: Some(text),
                        tool_calls: None,
                    }));
                }

                Some(self.build_chunk(ChatMessageDelta {
                    role: None,
                    content: None,
                    reasoning_content: Some(text),
                    tool_calls: None,
                }))
            }
            "agent_message_chunk" => {
                self.last_reasoning_kind = None;
                let text = data.and_then(|d| {
                    d.get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            d.get("content").or_else(|| d.get("text")).and_then(|v| v.as_str())
                        })
                })?;

                if !self.role_sent {
                    self.role_sent = true;
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: Some("assistant".to_string()),
                        content: Some(text.to_string()),
                        reasoning_content: None,
                        tool_calls: None,
                    }));
                }

                Some(self.build_chunk(ChatMessageDelta {
                    role: None,
                    content: Some(text.to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                }))
            }
            "tool_call" => {
                let tool_name = data
                    .and_then(|d| d.get("name").or_else(|| d.get("title")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let tool_call_id = data
                    .and_then(|d| d.get("toolCallId").or_else(|| d.get("tool_call_id")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let arguments = data
                    .map(extract_tool_call_arguments)
                    .unwrap_or_default();

                if tool_call_id.is_empty() || tool_name.is_empty() {
                    return None;
                }

                // Consumer tool: emit tool_calls delta
                if self.consumer_tool_names.contains(&tool_name.to_string()) {
                    self.had_client_tool_call = true;
                    self.role_sent = true;
                    self.last_reasoning_kind = Some(2);
                    self.consumer_call_ids.insert(tool_call_id.to_string(), tool_name.to_string());
                    // Skip probing calls (empty arguments)
                    if arguments.trim() == "{}" {
                        debug!("ACP tool_call (chat, consumer) — probing call silently consumed");
                        return None;
                    }
                    let tc = ToolCall {
                        id: tool_call_id.to_string(),
                        tool_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: tool_name.to_string(),
                            arguments: arguments.clone(),
                        },
                    };
                    let tool_text = format_tool_reasoning_text(tool_name, &arguments);
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: Some("assistant".to_string()),
                        content: None,
                        reasoning_content: Some(tool_text),
                        tool_calls: Some(vec![tc]),
                    }));
                }

                // Agent-internal tool: show as reasoning content delta
                self.agent_call_ids.insert(tool_call_id.to_string());
                // Skip probing calls (empty args) — wait for tool_call_update with real args
                if arguments.trim() == "{}" {
                    return None;
                }
                let tool_text = format_tool_reasoning_text(tool_name, &arguments);
                let sep = if self.role_sent && self.last_reasoning_kind.is_some_and(|k| k != 2) {
                    "\n\n"
                } else { "" };
                self.last_reasoning_kind = Some(2);
                if !self.role_sent {
                    self.role_sent = true;
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: Some("assistant".to_string()),
                        content: None,
                        reasoning_content: Some(format!("{sep}{tool_text}")),
                        tool_calls: None,
                    }));
                }
                Some(self.build_chunk(ChatMessageDelta {
                    role: None,
                    content: None,
                    reasoning_content: Some(format!("{sep}{tool_text}")),
                    tool_calls: None,
                }))
            }
            "tool_call_update" => {
                let update_call_id = data
                    .and_then(|d| d.get("tool_call_id").or_else(|| d.get("toolCallId")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let tool_name = data
                    .and_then(|d| d.get("name").or_else(|| d.get("title")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let new_args = data
                    .map(extract_tool_call_arguments)
                    .unwrap_or_default();

                if update_call_id.is_empty() || new_args.is_empty() {
                    return None;
                }

                // Consumer tool call update: emit tool_calls delta
                if let Some(stored_name) = self.consumer_call_ids.get(update_call_id) {
                    let tool_text = format_tool_reasoning_text(stored_name, &new_args);
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: None,
                        content: None,
                        reasoning_content: Some(tool_text),
                        tool_calls: Some(vec![ToolCall {
                            id: update_call_id.to_string(),
                            tool_type: "function".to_string(),
                            function: ToolCallFunction {
                                name: stored_name.clone(),
                                arguments: new_args,
                            },
                        }]),
                    }));
                }

                // Emit once: completed updates carry tool output, not input args.
                if self.agent_call_ids.contains(update_call_id)
                    && !self.agent_reasoning_emitted.contains(update_call_id)
                {
                    self.agent_reasoning_emitted.insert(update_call_id.to_string());
                    let tool_text = format_tool_reasoning_text(tool_name, &new_args);
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: None,
                        content: None,
                        reasoning_content: Some(tool_text),
                        tool_calls: None,
                    }));
                }

                None
            }
            other => {
                debug!(update_type = other, "Unhandled ACP session update type (chat, silently dropped)");
                None
            }
        }
    }

    fn build_chunk(&self, delta: ChatMessageDelta) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: self.chat_id.clone(),
            object: "chat.completion.chunk",
            created: self.created,
            model: self.model.clone(),
            choices: vec![ChatChoiceDelta { index: 0, delta, finish_reason: None }],
            usage: None,
        }
    }
}
