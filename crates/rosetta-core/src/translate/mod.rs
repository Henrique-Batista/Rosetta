use rosetta_types::acp::ContentBlock;
use rosetta_types::openai::*;
use tracing::debug;

pub mod helpers;
pub mod accumulator;
#[cfg(test)]
mod tests;

// Re-export all public items from sub-modules
pub use accumulator::{ChatChunkAccumulator, MessageAccumulator, ResponseAccumulator};
pub use helpers::{
    chat_final_chunk, extract_tool_call_arguments, format_chat_tool_definitions,
    format_response_tool_definitions, format_rosetta_harness_prompt,
    format_tool_reasoning_text, generate_call_id, generate_message_id, generate_response_id,
    ConsumerToolInfo,
};

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
                        text: format!("{prefix}{text}"),
                    })
                }
                InputItem::FunctionCallOutput { call_id, output } => Some(ContentBlock::Text {
                    text: format!("[Tool Result: {call_id}]\n{output}"),
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
                    Some(call_id) => format!("[Tool Result: {call_id}]\n"),
                    None => "[Tool Result]\n".to_string(),
                },
                _ => String::new(),
            };
            let text = msg.content.clone().unwrap_or_default();
            ContentBlock::Text {
                text: format!("{prefix}{text}"),
            }
        })
        .collect()
}

pub fn response_to_chat_completion(response: &Response) -> ChatCompletionResponse {
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

    let reasoning: String = response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::Reasoning { summary, .. } => {
                let text: String = summary
                    .iter()
                    .filter_map(|s| if s.text.is_empty() { None } else { Some(s.text.as_str()) })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            }
            OutputItem::FunctionCall { name, arguments, .. } => {
                if arguments.trim().is_empty() || arguments.trim() == "{}" {
                    None
                } else {
                    Some(format_tool_reasoning_text(name, arguments))
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let reasoning_content = if reasoning.is_empty() { None } else { Some(reasoning) };

    let content = if tool_calls.is_some() {
        None
    } else {
        Some(response.output_text.clone())
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
                content,
                reasoning_content,
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
                                delta: format!("{word} "),
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
    let chat_id = format!("chatcmpl-{}", response.id);

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
                reasoning_content: None,
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
            let delta_text = if word.is_empty() { " ".to_string() } else { format!("{word} ") };
            chunks.push(ChatCompletionChunk {
                id: chat_id.clone(),
                object: "chat.completion.chunk",
                created: response.created_at,
                model: response.model.clone(),
                choices: vec![ChatChoiceDelta {
                    index: 0,
                    delta: ChatMessageDelta {
                        role: None,
                        reasoning_content: None,
                        content: Some(delta_text),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            });
        }
    }

    // Tool call chunks: emit FunctionCall items as tool_calls deltas
    for item in &response.output {
        if let OutputItem::FunctionCall { id: _, call_id, name, arguments, .. } = item {
            // Only emit if arguments is non-empty (skip probing/empty calls)
            if arguments.trim().is_empty() || arguments.trim() == "{}" {
                continue;
            }
            chunks.push(ChatCompletionChunk {
                id: chat_id.clone(),
                object: "chat.completion.chunk",
                created: response.created_at,
                model: response.model.clone(),
                choices: vec![ChatChoiceDelta {
                    index: 0,
                    delta: ChatMessageDelta {
                        role: None,
                        content: None,
                        reasoning_content: None,
                        tool_calls: Some(vec![ToolCall {
                            id: call_id.clone(),
                            tool_type: "function".to_string(),
                            function: ToolCallFunction {
                                name: name.clone(),
                                arguments: arguments.clone(),
                            },
                        }]),
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
