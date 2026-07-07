use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Generic JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest<T> {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: T,
}

/// Generic JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse<T> {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Generic JSON-RPC 2.0 notification (no `id` field).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification<T> {
    pub jsonrpc: String,
    pub method: String,
    pub params: T,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Parameters for the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u32,
}

/// Result of the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    #[serde(alias = "agentInfo")]
    pub server_info: ServerInfo,
}

/// Server metadata returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Parameters for the `session/new` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<serde_json::Value>>,
}

/// Result of the `session/new` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<serde_json::Value>>,
}

/// Parameters for the `session/prompt` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

/// Result of the `session/prompt` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

/// Parameters for the `session/set_config_option` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigOptionRequest {
    pub session_id: String,
    pub config_id: String,
    pub value: serde_json::Value,
}

/// Result of the `session/set_config_option` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigOptionResponse {
    pub config_options: Vec<SessionConfigOption>,
}

/// A single configuration option exposed by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(rename = "type")]
    pub option_type: String,
    pub current_value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ConfigOptionChoice>>,
}

/// A single choice within a select-type config option.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOptionChoice {
    pub value: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A `session/update` notification payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub session_id: String,
    #[serde(flatten)]
    pub body: Value,
}

/// A single block of content in a prompt or response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// Source of an image content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
}
