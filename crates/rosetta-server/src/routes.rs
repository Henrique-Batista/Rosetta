 use axum::{
    extract::State,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use std::sync::Arc;
use tracing::{error, info};

use rosetta_types::openai::*;
use rosetta_types::acp::ContentBlock;
use rosetta_acp::client::AcpClient;
use rosetta_core::translate::*;
use crate::session_cache::SessionCache;
use crate::streaming;
use crate::streaming_task::{spawn_streaming_prompt, StreamOutcome, STREAM_CHANNEL_CAPACITY};

pub struct AppState {
    pub acp_command: String,
    pub acp_args: Vec<String>,
    pub cwd: String,
    pub mcp_servers: Vec<serde_json::Value>,
    pub session_cache: Arc<SessionCache>,
    pub harness_prompt: Option<String>,
    pub harness_disabled: bool,
}

/// Parse the `model` field from the OpenAI request into (model, agent).
/// Syntax: `provider/model:agent` where `:agent` is optional.
fn parse_model_agent(model: &str) -> (String, Option<String>) {
    if let Some((model_part, agent_part)) = model.rsplit_once(':') {
        (model_part.to_string(), Some(agent_part.to_string()))
    } else {
        (model.to_string(), None)
    }
}

/// Spawn a fresh ACP client without overriding the agent's own configuration.
/// The agent reads its own config (config file, env vars, etc.) — Rosetta does
/// not inject any agent-specific config overrides, preserving cross-agent compatibility.
async fn create_acp_client(state: &AppState, _model: &str) -> Result<AcpClient, axum::http::StatusCode> {
    let args_ref: Vec<&str> = state.acp_args.iter().map(|s| s.as_str()).collect();
    // Pass standard ACP or ROSETTA_* env vars if set, but do NOT inject
    // agent-specific config like OPENCODE_CONFIG — let the agent discover its own.
    let env_vars: Vec<(String, String)> = Vec::new();

    let mut client = AcpClient::spawn(&state.acp_command, &args_ref, &env_vars).await.map_err(|e| {
        error!("Failed to spawn ACP client: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let _init = client.initialize().await.map_err(|e| {
        error!("Failed to initialize ACP client: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(client)
}

/// Configure the ACP agent mode if available in configOptions.
async fn configure_agent(
    client: &mut AcpClient,
    session_id: &str,
    agent_name: &str,
    config_options: &Option<Vec<serde_json::Value>>,
) -> Result<(), String> {
    let options = match config_options {
        Some(opts) => opts,
        None => return Ok(()),
    };

    for opt in options {
        let category = opt.get("category").and_then(|v| v.as_str());
        let id = opt.get("id").and_then(|v| v.as_str());
        
        if category == Some("mode") || id == Some("mode") {
            let choices = opt.get("options").and_then(|v| v.as_array());
            if let Some(choices) = choices {
                for choice in choices {
                    let value = choice.get("value").and_then(|v| v.as_str());
                    let name = choice.get("name").and_then(|v| v.as_str());
                    
                    // Match by exact value or by name containing the agent name
                    if value == Some(agent_name) || 
                       name.map(|n| n.to_lowercase().contains(&agent_name.to_lowercase())).unwrap_or(false) {
                        let config_value = value.unwrap_or(agent_name);
                        info!("Configuring agent mode: {} -> {}", agent_name, config_value);
                        
                        client.set_config_option(session_id, "mode", serde_json::json!(config_value))
                            .await
                            .map_err(|e| format!("Failed to set config option: {}", e))?;
                        return Ok(());
                    }
                }
            }
        }
    }
    
    info!("Agent '{}' not found in configOptions, using default", agent_name);
    Ok(())
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/responses", post(create_response))
        .route("/v1/chat/completions", post(create_chat_completion))
        .with_state(state)
}

async fn create_response(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let model = req.model.clone();
    let response_id = generate_response_id();
    let is_streaming = req.stream;
    let is_continuation = req.previous_response_id.is_some();

    let consumer_tool_names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
    let consumer_tool_info: Vec<ConsumerToolInfo> = req.tools.iter().map(|t| ConsumerToolInfo {
        name: t.name.clone(),
        description: t.description.clone(),
    }).collect();
    let harness_disabled = state.harness_disabled;

    // 2.3: reuse the cached session for a continuation request, or spawn a fresh one.
    let (mut client, session_id) = if let Some(ref prev_id) = req.previous_response_id {
        match state.session_cache.take(prev_id).await {
            Some((cached_client, cached_session_id)) => {
                info!(prev_id, "Reusing cached session");
                (cached_client, cached_session_id)
            }
            None => return Err(axum::http::StatusCode::NOT_FOUND),
        }
    } else {
        let (_model_only, agent) = parse_model_agent(&model);
        let mut new_client = create_acp_client(&state, &model).await?;

        let mcp = Some(if state.mcp_servers.is_empty() { Vec::new() } else { state.mcp_servers.clone() });
        let session_resp = new_client.new_session(&state.cwd, mcp).await.map_err(|e| {
            error!("Failed to create ACP session: {}", e);
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let sid = session_resp.session_id;

        if let Some(agent_name) = agent
            && let Err(e) = configure_agent(&mut new_client, &sid, &agent_name, &session_resp.config_options).await
        {
            error!("Failed to configure agent: {}", e);
        }

        (new_client, sid)
    };

    // 2.4: a continuation prompt carries only the tool results — the agent
    // already holds the rest of the conversation in its live session.
    let prompt = if is_continuation {
        match req.input.as_ref() {
            Some(ResponseInput::Items(items)) => items
                .iter()
                .filter_map(|item| match item {
                    InputItem::FunctionCallOutput { call_id, output } => Some(ContentBlock::Text {
                        text: format!("[Tool Result: {}]\n{}", call_id, output),
                    }),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    } else {
        let mut prompt = openai_input_to_acp_prompt(req.input.as_ref().unwrap_or(&ResponseInput::Text(String::new())));
        if !consumer_tool_names.is_empty() {
            let tools_text = format_response_tool_definitions(&req.tools);
            prompt.insert(0, ContentBlock::Text { text: tools_text });
            if !harness_disabled {
                let harness_text = format_rosetta_harness_prompt(&consumer_tool_info, state.harness_prompt.as_deref());
                prompt.insert(0, ContentBlock::Text { text: harness_text });
            }
        }
        prompt
    };

    if is_streaming {
        let rx = spawn_streaming_prompt(client, session_id, prompt, STREAM_CHANNEL_CAPACITY);
        let event_stream = build_live_response_events(
            rx,
            response_id,
            model,
            consumer_tool_names,
            state.session_cache.clone(),
        );
        return Ok(streaming::response_event_stream_to_sse(event_stream).into_response());
    }

    let _prompt_resp = client.send_prompt(&session_id, prompt).await.map_err(|e| {
        error!("Failed to send prompt: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let updates = client.read_updates().await.map_err(|e| {
        error!("Failed to read updates: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut accumulator = ResponseAccumulator::new(response_id.clone(), model, consumer_tool_names);
    for update in &updates {
        accumulator.process_update_events(update);
    }
    let response = accumulator.finalize();

    // 2.5: cache the session when the agent is waiting on a tool result;
    // otherwise the turn is complete and the session can be closed normally.
    let has_function_calls = response
        .output
        .iter()
        .any(|item| matches!(item, OutputItem::FunctionCall { .. }));
    if has_function_calls {
        state.session_cache.insert(response_id, client, session_id).await;
    } else {
        let _ = client.close_session(&session_id).await;
    }

    Ok(Json(response).into_response())
}

/// Build the live `ResponseEvent` stream from a streaming task's
/// `StreamOutcome` channel: bookend events first, then a per-update event
/// as each `SessionUpdate` arrives, then a terminal `ResponseCompleted`/
/// `Error` event once the underlying agent turn ends.
fn build_live_response_events(
    mut rx: tokio::sync::mpsc::Receiver<StreamOutcome>,
    response_id: String,
    model: String,
    consumer_tool_names: Vec<String>,
    session_cache: Arc<SessionCache>,
) -> impl futures::Stream<Item = ResponseEvent> {
    async_stream::stream! {
        let mut accumulator = ResponseAccumulator::new(response_id.clone(), model, consumer_tool_names);
        yield ResponseEvent::ResponseCreated { sequence_number: 0 };
        yield ResponseEvent::ResponseInProgress { sequence_number: 1 };

        while let Some(outcome) = rx.recv().await {
            match outcome {
                StreamOutcome::Update(update) => {
                    for event in accumulator.process_update_events(&update) {
                        yield event;
                    }
                }
                StreamOutcome::DoneWithCache { client, session_id } => {
                    let response = accumulator.finalize();
                    let has_function_calls = response
                        .output
                        .iter()
                        .any(|item| matches!(item, OutputItem::FunctionCall { .. }));
                    if has_function_calls {
                        session_cache.insert(response_id, *client, session_id).await;
                    } else {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown()).await;
                    }
                    let seq = response_completed_seq(&accumulator);
                    yield ResponseEvent::ResponseCompleted { sequence_number: seq, response };
                    return;
                }
                StreamOutcome::Error(message) => {
                    let seq = response_completed_seq(&accumulator);
                    yield ResponseEvent::Error {
                        sequence_number: seq,
                        code: "agent_error".to_string(),
                        message,
                    };
                    return;
                }
            }
        }
    }
}

fn response_completed_seq(accumulator: &ResponseAccumulator) -> u32 {
    accumulator.sequence_number + 1
}

async fn create_chat_completion(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let model = req.model.clone();
    let response_id = generate_response_id();
    let is_streaming = req.stream;

    let (_model_only, agent) = parse_model_agent(&model);
    let mut client = create_acp_client(&state, &model).await?;

    let mcp = Some(if state.mcp_servers.is_empty() { Vec::new() } else { state.mcp_servers.clone() });
    let session_resp = client.new_session(&state.cwd, mcp).await.map_err(|e| {
        error!("Failed to create ACP session: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let session_id = session_resp.session_id;

    if let Some(agent_name) = agent
        && let Err(e) = configure_agent(&mut client, &session_id, &agent_name, &session_resp.config_options).await {
            error!("Failed to configure agent: {}", e);
    }

    let mut prompt = chat_messages_to_acp_prompt(&req.messages);
    let consumer_tool_names: Vec<String> = req.tools.iter().map(|t| t.function.name.clone()).collect();
    let consumer_tool_info: Vec<ConsumerToolInfo> = req.tools.iter().map(|t| ConsumerToolInfo {
        name: t.function.name.clone(),
        description: t.function.description.clone(),
    }).collect();
    if !consumer_tool_names.is_empty() {
        let tools_text = format_chat_tool_definitions(&req.tools);
        prompt.insert(0, ContentBlock::Text { text: tools_text });
        if !state.harness_disabled {
            let harness_text = format_rosetta_harness_prompt(&consumer_tool_info, state.harness_prompt.as_deref());
            prompt.insert(0, ContentBlock::Text { text: harness_text });
        }
    }

    if is_streaming {
        let chat_id = format!("chatcmpl-{}", response_id);
        let created = chrono::Utc::now().timestamp();
        let rx = spawn_streaming_prompt(client, session_id, prompt, STREAM_CHANNEL_CAPACITY);
        let event_stream = build_live_chat_sse_events(
            rx,
            chat_id,
            model,
            created,
            consumer_tool_names,
            state.session_cache.clone(),
            response_id,
        );
        return Ok(axum::response::sse::Sse::new(event_stream).into_response());
    }

    let _prompt_resp = client.send_prompt(&session_id, prompt).await.map_err(|e| {
        error!("Failed to send prompt: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let updates = client.read_updates().await.map_err(|e| {
        error!("Failed to read updates: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut accumulator = ResponseAccumulator::new(response_id, model, consumer_tool_names);
    for update in &updates {
        accumulator.process_update_events(update);
    }
    let response = accumulator.finalize();

    let _ = client.close_session(&session_id).await;

    let chat_response = response_to_chat_completion(&response);
    Ok(Json(chat_response).into_response())
}

/// Build the live Chat Completions SSE `Event` stream from a streaming
/// task's `StreamOutcome` channel. The `[DONE]` sentinel is appended ONLY on
/// normal completion (`StreamOutcome::Done`); on `StreamOutcome::Error` the
/// stream ends immediately without a synthetic finish chunk or `[DONE]`,
/// matching the contract in `specs/001-true-streaming/contracts/chat-completions-sse-chunks.md`
/// ("the stream ends WITHOUT emitting chunk (3) or the [DONE] sentinel").
fn build_live_chat_sse_events(
    mut rx: tokio::sync::mpsc::Receiver<StreamOutcome>,
    chat_id: String,
    model: String,
    created: i64,
    consumer_tool_names: Vec<String>,
    session_cache: Arc<SessionCache>,
    response_id: String,
) -> impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> {
    async_stream::stream! {
        let mut accumulator = ChatChunkAccumulator::new(chat_id.clone(), model.clone(), created, consumer_tool_names);

        while let Some(outcome) = rx.recv().await {
            match outcome {
                StreamOutcome::Update(update) => {
                    if let Some(chunk) = accumulator.process_update(&update) {
                        let data = serde_json::to_string(&chunk).unwrap_or_default();
                        yield Ok(axum::response::sse::Event::default().data(data));
                    }
                }
                StreamOutcome::DoneWithCache { client, session_id } => {
                    if accumulator.had_client_tool_call {
                        session_cache.insert(response_id, *client, session_id).await;
                    } else {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown()).await;
                    }
                    let finish_reason = if accumulator.had_client_tool_call { "tool_calls" } else { "stop" };
                    let chunk = chat_final_chunk(&chat_id, &model, created, finish_reason);
                    let data = serde_json::to_string(&chunk).unwrap_or_default();
                    yield Ok(axum::response::sse::Event::default().data(data));
                    yield Ok(axum::response::sse::Event::default().data("[DONE]"));
                    return;
                }
                StreamOutcome::Error(_) => return,
            }
        }
    }
}
