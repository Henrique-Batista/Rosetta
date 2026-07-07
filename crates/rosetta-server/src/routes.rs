 use axum::{
    extract::State,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use std::sync::Arc;
use tracing::{error, info};

use rosetta_types::openai::*;
use rosetta_acp::client::AcpClient;
use rosetta_core::translate::*;
use crate::streaming;

pub struct AppState {
    pub acp_command: String,
    pub acp_args: Vec<String>,
    pub cwd: String,
    pub mcp_servers: Vec<serde_json::Value>,
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

    let prompt = openai_input_to_acp_prompt(req.input.as_ref().unwrap_or(&ResponseInput::Text(String::new())));
    let _prompt_resp = client.send_prompt(&session_id, prompt).await.map_err(|e| {
        error!("Failed to send ACP prompt: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let updates = client.read_updates().await.map_err(|e| {
        error!("Failed to read ACP updates: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let _ = client.close_session(&session_id).await;

    let mut accumulator = ResponseAccumulator::new(response_id.clone(), model.clone());
    for update in &updates {
        accumulator.process_update(update);
    }
    let response = accumulator.finalize();

    if is_streaming {
        let events = response_to_streaming_events(&response);
        Ok(streaming::response_events_to_sse(events).into_response())
    } else {
        Ok(Json(response).into_response())
    }
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

    let prompt = chat_messages_to_acp_prompt(&req.messages);
    let _prompt_resp = client.send_prompt(&session_id, prompt).await.map_err(|e| {
        error!("Failed to send ACP prompt: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let updates = client.read_updates().await.map_err(|e| {
        error!("Failed to read ACP updates: {}", e);
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let _ = client.close_session(&session_id).await;

    let mut accumulator = ResponseAccumulator::new(response_id, model);
    for update in &updates {
        accumulator.process_update(update);
    }
    let response = accumulator.finalize();

    if is_streaming {
        let chat_chunks = response_to_chat_chunks(&response);
        Ok(streaming::chat_chunks_to_sse(chat_chunks).into_response())
    } else {
        let chat_response = response_to_chat_completion(&response);
        Ok(Json(chat_response).into_response())
    }
}
