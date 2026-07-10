use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseCreateRequest {
    pub model: String,
    #[serde(default)]
    pub input: Option<ResponseInput>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub store: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
    Items(Vec<InputItem>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message { role: String, content: ContentInput },
    FunctionCallOutput { call_id: String, output: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentInput {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    InputText { text: String },
    InputImage { image_url: String },
    InputFile { file_id: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub id: String,
    pub object: &'static str,
    pub created_at: i64,
    pub status: String,
    pub model: String,
    pub output: Vec<OutputItem>,
    pub output_text: String,
    pub usage: Usage,
    pub parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        role: String,
        status: String,
        content: Vec<OutputContent>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: String,
    },
    Reasoning {
        id: String,
        summary: Vec<ReasoningSummary>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContent {
    OutputText { text: String, annotations: Vec<Value> },
    OutputRefusal { refusal: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ReasoningSummary {
    #[serde(rename = "type")]
    pub summary_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Auto(String),
    None(String),
    Required(String),
    Function {
        #[serde(rename = "type")]
        tool_type: String,
        name: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseEvent {
    #[serde(rename = "response.created")]
    ResponseCreated { sequence_number: u32 },
    #[serde(rename = "response.in_progress")]
    ResponseInProgress { sequence_number: u32 },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { sequence_number: u32, output_index: usize, item: OutputItem },
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded { sequence_number: u32, item_id: String, output_index: usize, content_index: usize, part: OutputContent },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { sequence_number: u32, item_id: String, output_index: usize, content_index: usize, delta: String },
    #[serde(rename = "response.output_text.done")]
    OutputTextDone { sequence_number: u32, item_id: String, output_index: usize, content_index: usize, text: String },
    #[serde(rename = "response.content_part.done")]
    ContentPartDone { sequence_number: u32, item_id: String, output_index: usize, content_index: usize },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { sequence_number: u32, output_index: usize, item: OutputItem },
    #[serde(rename = "response.completed")]
    ResponseCompleted { sequence_number: u32, response: Response },
    #[serde(rename = "error")]
    Error { sequence_number: u32, code: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Vec<ChatToolDefinition>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ChatFunctionDefinition,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatFunctionDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: ChatUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoiceDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoiceDelta {
    pub index: u32,
    pub delta: ChatMessageDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ChatMessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}
