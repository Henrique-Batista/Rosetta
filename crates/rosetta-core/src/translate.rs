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
                "system" | "developer" => "[System]\n".to_string(),
                "assistant" => "[Assistant]\n".to_string(),
                "tool" => match &msg.tool_call_id {
                    Some(call_id) => format!("[Tool Result: {}]\n", call_id),
                    None => "[Tool Result]\n".to_string(),
                },
                _ => String::new(),
            };
            let text = msg.content.clone().unwrap_or_default();
            ContentBlock::Text {
                text: format!("{}{}", prefix, text),
            }
        })
        .collect()
}

/// Format Responses-API tool definitions as a `[Tool Definitions]` text block.
///
/// Returns an empty string when `tools` is empty so callers can prepend
/// unconditionally without a separate length check.
pub fn format_response_tool_definitions(tools: &[ToolDefinition]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let values: Vec<serde_json::Value> = tools.iter().map(tool_definition_to_value).collect();
    match serde_json::to_string(&values) {
        Ok(json) => format!("[Tool Definitions]\n{}", json),
        Err(_) => String::new(),
    }
}

/// Format Chat-Completions-API tool definitions as a `[Tool Definitions]` text block.
///
/// Returns an empty string when `tools` is empty so callers can prepend
/// unconditionally without a separate length check.
pub fn format_chat_tool_definitions(tools: &[ChatToolDefinition]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let values: Vec<serde_json::Value> = tools.iter().map(chat_tool_definition_to_value).collect();
    match serde_json::to_string(&values) {
        Ok(json) => format!("[Tool Definitions]\n{}", json),
        Err(_) => String::new(),
    }
}

/// Convert a Responses-API `ToolDefinition` into a `serde_json::Value` shaped
/// like the OpenAI wire format (`type` / `name` / `description` / `parameters` / `strict`).
fn tool_definition_to_value(t: &ToolDefinition) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), serde_json::Value::String(t.tool_type.clone()));
    obj.insert("name".to_string(), serde_json::Value::String(t.name.clone()));
    if let Some(desc) = &t.description {
        obj.insert("description".to_string(), serde_json::Value::String(desc.clone()));
    }
    if let Some(params) = &t.parameters {
        obj.insert("parameters".to_string(), params.clone());
    }
    if let Some(strict) = t.strict {
        obj.insert("strict".to_string(), serde_json::Value::Bool(strict));
    }
    serde_json::Value::Object(obj)
}

/// Convert a Chat-Completions-API `ChatToolDefinition` into a `serde_json::Value`
/// shaped like the OpenAI wire format (nested `function` object).
fn chat_tool_definition_to_value(t: &ChatToolDefinition) -> serde_json::Value {
    let mut func = serde_json::Map::new();
    func.insert("name".to_string(), serde_json::Value::String(t.function.name.clone()));
    if let Some(desc) = &t.function.description {
        func.insert("description".to_string(), serde_json::Value::String(desc.clone()));
    }
    if let Some(params) = &t.function.parameters {
        func.insert("parameters".to_string(), params.clone());
    }
    if let Some(strict) = t.function.strict {
        func.insert("strict".to_string(), serde_json::Value::Bool(strict));
    }

    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), serde_json::Value::String(t.tool_type.clone()));
    obj.insert("function".to_string(), serde_json::Value::Object(func));
    serde_json::Value::Object(obj)
}

/// Format a minimal Rosetta harness prompt that tells the ACP agent which
/// tools are client-executed (consumer tools) and should be announced via
/// `tool_call` rather than executed internally.
///
/// Returns an empty string when `names` is empty (backward compatible: no
/// harness → all tool calls are agent-internal).
pub fn format_rosetta_harness_prompt(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    format!(
        "[Rosetta Harness]\n\
         You are proxied through Rosetta. The following tools are executed by the client:\n\
         {}\n\
         To call one, emit a tool_call with its name and arguments.\n\
         All other tools (skills, MCP) work as usual.",
        names.join(", ")
    )
}

/// Extract tool call arguments from a tool_call data object.
///
/// Tries named fields (`arguments`, `params`, `input`, `args`) first.
/// Falls back to constructing an object from all non-metadata top-level
/// fields, handling opencode ACP format where parameters are spread
/// across fields like `locations`, `rawInput`, etc.
pub fn extract_tool_call_arguments(data: &serde_json::Value) -> String {
    // Prefer known argument field names
    if let Some(args) = data
        .get("arguments")
        .or_else(|| data.get("params"))
        .or_else(|| data.get("input"))
        .or_else(|| data.get("args"))
    {
        return args
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| args.to_string());
    }

    // Fallback: collect all top-level fields except metadata keys;
    // also drop empty arrays, empty objects, and nulls so the output
    // is cleaner for clients expecting OpenAI-style arguments.
    const META_KEYS: &[&str] = &[
        "toolCallId",
        "tool_call_id",
        "sessionUpdate",
        "title",
        "kind",
        "name",
        "status",
    ];
    if let Some(obj) = data.as_object() {
        let mut filtered = serde_json::Map::new();
        for (k, v) in obj {
            if META_KEYS.contains(&k.as_str()) {
                continue;
            }
            // Skip empty arrays/objects and null values.
            if v.is_array() && v.as_array().unwrap().is_empty() { continue; }
            if v.is_object() && v.as_object().unwrap().is_empty() { continue; }
            if v.is_null() { continue; }
            filtered.insert(k.clone(), v.clone());
        }
        if !filtered.is_empty() {
            return serde_json::Value::Object(filtered).to_string();
        }
    }

    "{}".to_string()
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
    pub consumer_tool_names: Vec<String>,
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

                    // Branch: client-defined tool → FunctionCall, agent-internal → Reasoning
                    if self.consumer_tool_names.iter().any(|n| n == &name) {
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
        let added_event = self.process_update(update);
        let is_item_added_message = matches!(&added_event, Some(ResponseEvent::OutputItemAdded { item: OutputItem::Message { .. }, .. }));

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

        // tool_call_update: update arguments on the matching FunctionCall output item
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
                let new_args = d
                    .get("arguments")
                    .or_else(|| d.get("params"))
                    .or_else(|| d.get("input"))
                    .or_else(|| d.get("args"))
                    .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))
                    .unwrap_or_default();
                if !update_call_id.is_empty() && !new_args.is_empty() {
                    // Update the matching FunctionCall's arguments in output_items
                    for item in self.output_items.iter_mut() {
                        if let OutputItem::FunctionCall { call_id, arguments, .. } = item {
                            if call_id == update_call_id {
                                *arguments = new_args.clone();
                                break;
                            }
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

pub fn response_to_chat_completion(response: &Response) -> ChatCompletionResponse {
    let message_content = response.output_text.clone();

    let tool_calls: Option<Vec<ToolCall>> = {
        let calls: Vec<ToolCall> = response
            .output
            .iter()
            .filter_map(|item| match item {
                OutputItem::FunctionCall {
                    call_id, name, arguments, ..
                } => Some(ToolCall {
                    id: call_id.clone(),
                    tool_type: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                }),
                _ => None,
            })
            .collect();
        if calls.is_empty() { None } else { Some(calls) }
    };

    let finish_reason = if tool_calls.is_some() {
        "tool_calls".to_string()
    } else {
        "stop".to_string()
    };

    ChatCompletionResponse {
        id: response.id.clone(),
        object: "chat.completion",
        created: response.created_at,
        model: response.model.clone(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: if tool_calls.is_some() { None } else { Some(message_content) },
                tool_calls,
                tool_call_id: None,
            },
            finish_reason,
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
            other => panic!("expected Reasoning item, got {:?}", other),
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
                "expected OutputItemAdded event with Reasoning item, got {:?}", item);
        } else {
            panic!("expected OutputItemAdded event, got {:?}", event);
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

pub fn chat_final_chunk(chat_id: &str, model: &str, created: i64, finish_reason: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: chat_id.to_string(),
        object: "chat.completion.chunk",
        created,
        model: model.to_string(),
        choices: vec![ChatChoiceDelta {
            index: 0,
            delta: ChatMessageDelta::default(),
            finish_reason: Some(finish_reason.to_string()),
        }],
        usage: Some(ChatUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }),
    }
}

pub struct ChatChunkAccumulator {
    pub chat_id: String,
    pub model: String,
    pub created: i64,
    pub role_sent: bool,
    pub consumer_tool_names: Vec<String>,
    pub had_client_tool_call: bool,
}

impl ChatChunkAccumulator {
    pub fn new(chat_id: String, model: String, created: i64, consumer_tool_names: Vec<String>) -> Self {
        Self { chat_id, model, created, role_sent: false, consumer_tool_names, had_client_tool_call: false }
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
            "agent_message_chunk" => {
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
                        tool_calls: None,
                    }));
                }

                Some(self.build_chunk(ChatMessageDelta {
                    role: None,
                    content: Some(text.to_string()),
                    tool_calls: None,
                }))
            }
            "agent_thought_chunk" => None,
            "tool_call" => {
                let tool_name = data
                    .and_then(|d| d.get("name").or_else(|| d.get("title")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let tool_call_id = data
                    .and_then(|d| d.get("toolCallId").or_else(|| d.get("tool_call_id")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let arguments = data
                    .and_then(|d| {
                        d.get("arguments")
                            .or_else(|| d.get("params"))
                            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
                    })
                    .unwrap_or_default();

                if tool_call_id.is_empty() || tool_name.is_empty() {
                    return None;
                }

                // Consumer tool: emit tool_calls delta
                if self.consumer_tool_names.contains(&tool_name.to_string()) {
                    // Skip probing calls (empty arguments)
                    if arguments.trim() == "{}" {
                        debug!("ACP tool_call (chat, consumer) — probing call silently consumed");
                        return None;
                    }
                    self.had_client_tool_call = true;
                    self.role_sent = true;
                    let tc = ToolCall {
                        id: tool_call_id.to_string(),
                        tool_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: tool_name.to_string(),
                            arguments: arguments.clone(),
                        },
                    };
                    return Some(self.build_chunk(ChatMessageDelta {
                        role: Some("assistant".to_string()),
                        content: None,
                        tool_calls: Some(vec![tc]),
                    }));
                }

                // Agent-internal tool: silently consume
                debug!("ACP tool_call (chat, agent-internal) — silently dropped");
                None
            }
            "tool_call_update" => {
                let update_call_id = data
                    .and_then(|d| d.get("tool_call_id").or_else(|| d.get("toolCallId")).and_then(|v| v.as_str()))
                    .unwrap_or("");
                let new_args = data
                    .and_then(|d| {
                        d.get("arguments")
                            .or_else(|| d.get("params"))
                            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
                    })
                    .unwrap_or_default();

                if update_call_id.is_empty() || new_args.is_empty() {
                    return None;
                }

                // Emit a delta chunk with updated arguments on the same call id
                // The index is 0 because we only emit one tool_call per chunk
                Some(self.build_chunk(ChatMessageDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: update_call_id.to_string(),
                        tool_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: "".to_string(), // name not sent on update
                            arguments: new_args,
                        },
                    }]),
                }))
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
