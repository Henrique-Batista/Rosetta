use rosetta_types::openai::*;
use serde_json;

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

/// Describes a consumer (client-defined) tool for the harness prompt.
pub struct ConsumerToolInfo {
    pub name: String,
    pub description: Option<String>,
}

/// Default harness prompt template. `{tools}` is replaced with the per-tool
/// `- name: description` lines.
const DEFAULT_HARNESS_TEMPLATE: &str = "\
[Rosetta Harness]\n\
Client tools:\n\
{tools}\n\
Call → tool_call(name, args). On failure → retry with your own equivalent tool (by name or purpose). Both fail → inform user and continue.\n\
Skills, MCP work as usual.";

/// Format a Rosetta harness prompt that tells the ACP agent which tools are
/// client-executed (consumer tools), with name + description so the agent can
/// match them to its own tools for fallback.
///
/// The agent is instructed to call consumer tools first, fall back to its own
/// equivalent tool (by name or purpose) on client failure, and inform the user
/// if both fail.
///
/// `template_override` replaces the built-in prompt template. Use `{tools}`
/// as a placeholder for the formatted tool list (`- name: description` lines).
/// Pass `None` to use the default template.
///
/// Returns an empty string when `tools` is empty (backward compatible: no
/// harness → all tool calls are agent-internal).
pub fn format_rosetta_harness_prompt(tools: &[ConsumerToolInfo], template_override: Option<&str>) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let tools_text: Vec<String> = tools
        .iter()
        .map(|t| match &t.description {
            Some(desc) => format!("- {}: {}", t.name, desc),
            None => format!("- {}", t.name),
        })
        .collect();
    let joined = tools_text.join("\n");
    let template = template_override.unwrap_or(DEFAULT_HARNESS_TEMPLATE);
    template.replace("{tools}", &joined)
}

/// Extract tool call arguments from a tool_call data object.
///
/// Tries named fields (`arguments`, `params`, `input`, `args`) first.
/// Falls back to constructing an object from all non-metadata top-level
/// fields, handling opencode ACP format where parameters are spread
/// across fields like `locations`, `rawInput`, etc.
pub fn extract_tool_call_arguments(data: &serde_json::Value) -> String {
    // Prefer known argument field names. When the named field exists but
    // yields a plain string (not JSON object/array), skip it — the opencode
    // agent's "input" field often carries conversational text rather than
    // structured parameters. The fallback below collects the actual parameter
    // fields spread across top-level keys.
    if let Some(args) = data
        .get("arguments")
        .or_else(|| data.get("params"))
        .or_else(|| data.get("input"))
        .or_else(|| data.get("args"))
    {
        let extracted: String = args
            .as_str()
            .map_or_else(|| args.to_string(), |s| s.to_string());
        let trimmed = extracted.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return extracted;
        }
        // Plain string — fall through to collect top-level parameter fields.
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
        "content",
        "rawOutput",
    ];
    if let Some(obj) = data.as_object() {
        let mut filtered = serde_json::Map::new();
        for (k, v) in obj {
            if META_KEYS.contains(&k.as_str()) {
                continue;
            }
            // The opencode agent nests real tool parameters inside `rawInput`.
            // Unpack its contents into the top level instead of preserving the nesting.
            if k == "rawInput" {
                if let Some(inner) = v.as_object() {
                    for (ik, iv) in inner {
                        if !META_KEYS.contains(&ik.as_str()) {
                            filtered.insert(ik.clone(), iv.clone());
                        }
                    }
                }
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

const MAX_REASONING_ARG_LENGTH: usize = 500;

pub fn format_tool_reasoning_text(tool_name: &str, args: &str) -> String {
    let displayed = if args.len() > MAX_REASONING_ARG_LENGTH {
        let truncated: String = args.chars().take(MAX_REASONING_ARG_LENGTH).collect();
        format!("{truncated}…")
    } else {
        args.to_string()
    };
    format!("**{tool_name}**\n\n{displayed}")
}

pub fn generate_response_id() -> String {
    format!(
        "resp_{}",
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .get(0..16)
            .unwrap_or("")
    )
}

pub fn generate_message_id() -> String {
    format!(
        "msg_{}",
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .get(0..16)
            .unwrap_or("")
    )
}

pub fn generate_call_id() -> String {
    format!(
        "call_{}",
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .get(0..16)
            .unwrap_or("")
    )
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
