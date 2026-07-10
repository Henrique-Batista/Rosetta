# Dual Tool Routing — Rosetta Harness Plan

## 1. Problem

Rosetta currently treats **all** ACP `tool_call` updates the same way:

| API | What happens | Problem |
|-----|-------------|---------|
| Responses API | Always emits `OutputItem::Reasoning` | Client-defined tools (`req.tools`) are **never** forwarded as `FunctionCall` — client can't invoke them |
| Chat Completions | Always silently consumed (`None` in ChatChunkAccumulator) | Same — `tool_calls` delta never appears |

The agent (opencode) has **its own tools** (skills, MCP servers) that it executes internally. These should remain agent-internal (shown as `Reasoning` / silently consumed in Chat). But when the **client** provides tool definitions via the OpenAI API, they expect Rosetta to forward those calls so they can execute them.

**The core insight**: We need a *dual awareness* — the agent must know which tools belong to the client vs. which are its own, and Rosetta must route accordingly.

---

## 2. Design Goals

| # | Goal | Why |
|---|------|-----|
| 1 | Agent-internal tools stay agent-internal | Skills, MCP — agent executes them; client never sees `tool_calls` |
| 2 | Client-defined tools produce `tool_calls`/`FunctionCall` | Client expects this per the OpenAI API contract |
| 3 | The agent decides which tools to call | Agent has context and can choose the right tool |
| 4 | Multi-turn flow works | Client invokes tool → sends result → agent continues |
| 5 | Streaming works | Tool calls appear mid-stream; client can continue the stream |
| 6 | Backward compatible | All existing tests pass; behavior unchanged when `req.tools` is empty |

---

## 3. Architecture — The Rosetta Harness Prompt

### 3.1 New system prompt block

Currently, tool definitions are injected as a bare `[Tool Definitions]\n<JSON>` text block. The agent has **no context** about what these are or how they differ from its own tools.

We add a **second text block** — the Rosetta Harness Prompt — that explains the dual context:

```
[Rosetta Harness]
You are running as an ACP agent through Rosetta, an OpenAI-to-ACP proxy.
The CLIENT calling you provides these tools and will execute them when you call them:

<client_tools>
  get_weather(params) — Get the weather for a city
  search_web(params) — Search the web
</client_tools>

When you need a client tool, invoke it by name — Rosetta will forward the call
to the client. The result will come back in the next turn.

Your own internal tools (skills, MCP servers) are also available. Use them
directly — they will be shown as reasoning to the client.

IMPORTANT: Only call a CLIENT tool when you truly need the client to execute it.
For tools you can execute yourself, use your own capabilities.
```

### 3.2 Where it goes

```
[Rosetta Harness]        ← NEW: explains dual context, lists client tools
[Tool Definitions]       ← EXISTING: JSON tool definitions for reference
[System]                 ← EXISTING: user's system message
...
```

Prepended to the prompt before the user's messages.

### 3.3 Generation

A new function:

```rust
fn format_rosetta_harness_prompt(client_tool_names: &[String]) -> String {
    if client_tool_names.is_empty() {
        return String::new();
    }
    let tool_list: String = client_tool_names
        .iter()
        .map(|n| format!("  {}(params)", n))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Rosetta Harness]\n\
         You are running through Rosetta, an OpenAI-to-ACP proxy.\n\
         The CLIENT provides these tools and will execute them when called:\n\
         \n\
         <client_tools>\n{}\n</client_tools>\n\
         \n\
         When you need a client tool, invoke it by name. Rosetta forwards\n\
         the call to the client. The result comes back next turn.\n\
         \n\
         Your own internal tools (skills, MCP servers) are also available.\n\
         Use them directly — they show as reasoning to the client.\n\
         \n\
         Only call a CLIENT tool when you truly need client execution.",
        tool_list
    )
}
```

---

## 4. Tool Call Routing

### 4.1 Distinction mechanism

The ACP agent emits `tool_call` updates uniformly for all tools. Rosetta distinguishes them by **name**:

```
match update_type {
    "tool_call" => {
        let name = extract_tool_name(data);
        if client_tool_names.contains(&name) {
            // → Forward to client as FunctionCall / tool_calls
            emit_client_tool_call(data, name)
        } else {
            // → Agent-internal, show as reasoning
            emit_agent_internal_reasoning(data)
        }
    }
}
```

### 4.2 Client tool call → FunctionCall (Responses API)

```rust
fn emit_client_tool_call(data, name, client_tool_names) -> OutputItem {
    let call_id = data.get("toolCallId")...;
    let arguments = extract_tool_call_arguments(data);  // existing function
    OutputItem::FunctionCall {
        id: generate_id(),
        call_id,
        name,
        arguments,
        status: "completed",
    }
}
```

`response_to_chat_completion` and `response_to_chat_chunks` already have `FunctionCall` → `tool_calls` / `finish_reason: "tool_calls"` logic from the old code — **restore it**, gated by `client_tool_names`.

### 4.3 Client tool call → Chat Completions `tool_calls` delta

Restore the `ChatChunkAccumulator` logic for client tool calls:

```
"tool_call" if client_tool_names.contains(&name) → emit tool_calls delta
"tool_call" if !client_tool_names.contains(&name) → None (silently consumed)
```

### 4.4 Probing call handling

The agent emits probing calls (`status: "pending"`, empty args) before real calls. The existing `extract_tool_call_arguments` returns `{}` for these. **Skip them** — same logic as before.

---

## 5. Multi-turn Flow (Non-Streaming)

### 5.1 Flow diagram

```
Client                  Rosetta                 ACP Agent
  │                       │                        │
  │  POST /v1/responses   │                        │
  │  tools=[get_weather]  │                        │
  │ ─────────────────────>│                        │
  │                       │  session/new           │
  │                       │ ──────────────────────>│
  │                       │  session/prompt        │
  │                       │  [Rosetta Harness]     │
  │                       │  [Tool Definitions]    │
  │                       │  [System] + messages   │
  │                       │ ──────────────────────>│
  │                       │                        │
  │                       │  session/update        │
  │                       │  tool_call:            │
  │                       │    name="get_weather"  │
  │                       │  ←────────────────────│
  │                       │                        │
  │  HTTP 200             │                        │
  │  {output: [           │                        │
  │    FunctionCall       │                        │
  │      name=get_weather │                        │
  │  ], status: completed}│                        │
  │  ←───────────────────│                        │
  │                       │                        │
  │  (Client executes     │                        │
  │   get_weather →       │                        │
  │   result: "25°C")     │                        │
  │                       │                        │
  │  POST /v1/responses   │                        │
  │  previous_response_id │                        │
  │  input: [{call_id,    │                        │
  │    output: "25°C"}]   │                        │
  │ ─────────────────────>│                        │
  │                       │  Reuse cached session  │
  │                       │  session/prompt        │
  │                       │  [Tool Result: call_1] │
  │                       │    "The weather is 25°C"
  │                       │  + original context     │
  │                       │ ──────────────────────>│
  │                       │                        │
  │                       │  agent_message_chunk   │
  │                       │  "The weather in Paris │
  │                       │   is 25°C and sunny."  │
  │                       │  ←────────────────────│
  │                       │                        │
  │  HTTP 200             │                        │
  │  {output_text:        │                        │
  │   "The weather..."}   │                        │
  │  ←───────────────────│                        │
```

### 5.2 Session caching (restore)

When a non-streaming request produces client tool calls:

1. **Do NOT close the session** — cache it in `SessionCache` keyed by `response_id`
2. **Return the response** with `FunctionCall` items to the client
3. **On the next request** with `previous_response_id`:
   - Look up the cached session
   - Feed the `FunctionCallOutput` as a `[Tool Result: call_id]` message
   - Agents continue processing
   - Close session when no more client tool calls are made

### 5.3 Time-bounded cache

`SessionCache` holds `(AcpClient, session_id)` with a TTL (configurable, default 5 min). `evict_expired()` runs on each cache hit. Same design as the removed `session_cache.rs`.

### 5.4 `previous_response_id` handling (Responses API)

The OpenAI Responses API supports `previous_response_id` for continuation. The request includes `input` with `FunctionCallOutput` items:

```json
{
  "previous_response_id": "resp_abc",
  "input": [
    {
      "type": "function_call_output",
      "call_id": "call_1",
      "output": "{\"temperature\": 25}"
    }
  ]
}
```

Rosetta translates `FunctionCallOutput` → `[Tool Result: call_1]\n{"temperature": 25}` and appends it to the conversation.

### 5.5 Chat Completions — continuation via messages

Chat Completions doesn't have `previous_response_id`. The client just includes the tool result as a `tool`-role message:

```json
{
  "messages": [
    {"role": "assistant", "content": null, "tool_calls": [...]},
    {"role": "tool", "tool_call_id": "call_1", "content": "25°C"}
  ]
}
```

Rosetta translates `tool`-role messages → `[Tool Result: call_1]\n25°C`. A new session is created each time (no caching needed for Chat Completions).

---

## 6. Streaming with Client Tool Calls

### 6.1 Responses API streaming

When a client tool call is detected mid-stream:

```
event: response.output_item.added
data: {"type": "function_call", "id": "fc_1", "name": "get_weather", "arguments": "", "status": "in_progress"}

event: response.output_text.delta
data: {"delta": "{\"city\": \"Paris\"}"}

event: response.output_text.done  (or output_item.done)

event: response.completed
data: {"response": {"status": "completed", "output": [{"type": "function_call", ...}]}}
```

The stream terminates normally (as if the turn completed). The client sees the function_call, executes the tool, and sends a **new** request with the result.

For streaming, there's no live mid-stream pause + resume (that would require HTTP/2 bidirectional streaming which is out of scope). The client gets the tool call, executes, and starts a new stream.

### 6.2 Chat Completions streaming

```
data: {"choices": [{"delta": {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\": \"Paris\"}"}}]}}]}

data: {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}

data: [DONE]
```

Same pattern — stream terminates with `finish_reason: "tool_calls"` and the `[DONE]` sentinel.

---

## 7. Changes Required

### 7.1 `rosetta-core/src/translate.rs`

| Change | Details |
|--------|---------|
| `ResponseAccumulator` — re-add `consumer_tool_names: Vec<String>` | Passed in `new()`, used in `tool_call` handler |
| `ResponseAccumulator::process_update("tool_call")` — branch on name | If name in `consumer_tool_names` → `FunctionCall`; else → `Reasoning` |
| `ResponseAccumulator` — re-add `process_update_events` tool_call delta (for bulk events) | Emit `OutputItemAdded` for `FunctionCall` |
| `ChatChunkAccumulator` — re-add `consumer_tool_names` and `tool_call` / `tool_call_update` handlers | Only for client tools; agent-internal → None |
| `response_to_chat_completion` — re-add `FunctionCall` → `tool_calls` conversion with `finish_reason: "tool_calls"` | Gated: only when client tool calls exist |
| `response_to_chat_chunks` — re-add `FunctionCall` → chunk logic | Same |
| `response_to_streaming_events` — re-add `FunctionCall` → event logic | Same |
| `chat_final_chunk` — accept `finish_reason: &str` instead of hardcoding `"stop"` | Pass `"tool_calls"` when applicable |
| `format_rosetta_harness_prompt` — **NEW** | Generates the harness system prompt (§3) |

### 7.2 `rosetta-server/src/routes.rs`

| Change | Details |
|--------|---------|
| Restore `session_cache: SessionCache` in `AppState` | For multi-turn Responses API flow |
| Restore `consumer_tool_names` extraction | From `req.tools` |
| Restore session cache logic in `create_response` | `previous_response_id` → cache look up |
| Non-streaming: detect client tool calls → cache session, return response | Don't close session if client tools were called |
| Streaming: pass `consumer_tool_names` to accumulators | For routing |
| `format_response_tool_definitions` → also generate harness prompt | Insert `[Rosetta Harness]` before `[Tool Definitions]` |

### 7.3 `rosetta-server/src/session_cache.rs` — **RESTORE**

Re-create the removed file with the same `SessionCache` struct:

```rust
pub struct SessionCache { ... }
impl SessionCache {
    pub fn new(ttl: Option<Duration>) -> Self;
    pub async fn insert(response_id, client, session_id);
    pub async fn take(response_id) -> Option<(AcpClient, String)>;
    pub async fn evict_expired(&self);
}
```

### 7.4 `rosetta-server/src/main.rs` — **RESTORE session_cache**

### 7.5 `rosetta-server/tests/streaming_integration_test.rs` — **UPDATE**

- Restore `session_cache` in test `AppState` construction
- The tool_call tests already exist as we updated them — some need reverting:
  - `test_non_streaming_chat_completions_unaffected_by_feature` — restore `tool_calls` assertion (but only when `tools` includes the matching name)
  - `test_streaming_chat_completions_content_only_no_tool_calls` — restore tool_calls delta expectation when `tools` matches
  - Add new test: `test_client_tool_call_produces_function_call_in_responses_api`
  - Add new test: `test_multi_turn_tool_call_continuation`

---

## 8. Detailed Behavior Matrix

| Scenario | ACP Update | `consumer_tool_names` contains name? | Responses API | Chat Completions |
|----------|-----------|--------------------------------------|---------------|------------------|
| Agent uses internal skill | `tool_call` name="grep" | No | `Reasoning` (thinking) | Silently consumed |
| Agent uses MCP tool | `tool_call` name="fs_read" | No | `Reasoning` (thinking) | Silently consumed |
| Agent calls client tool | `tool_call` name="get_weather" | Yes | `FunctionCall` | `tool_calls` delta |
| Probing call (empty args) | `tool_call` args="{}" | Yes | Silently consumed | Silently consumed |
| No client tools in request | `tool_call` any name | N/A (empty list) | Always `Reasoning` | Always silently consumed |

---

## 9. Edge Cases

### 9.1 Agent calls a client tool it shouldn't

The agent might call a client tool when it could have used its own capabilities. This is a **prompt engineering issue** — the harness prompt should instruct the agent to prefer its own tools for tasks it can handle. Unavoidable in an LLM-based system; the client will get a `tool_calls` response and can decide to handle it or not.

### 9.2 Agent uses a client tool name that clashes with an internal tool

If the agent has an internal tool named "get_weather" AND the client also defines one, Rosetta treats it as a **client tool** (the name matches `consumer_tool_names`). The agent can still use its internal tool — it just needs a different internal name. This is a naming convention issue.

### 9.3 `previous_response_id` with no cached session (expired)

Session cache has a TTL (default 5 minutes). If the client comes back after the cache expired, Rosetta returns 404. The client must re-send the full conversation.

### 9.4 Streaming + tool calls at the end of stream

If a tool_call arrives after the agent has already started producing message text, Rosetta:
1. Completes the current message
2. Emits the tool call as a new output item
3. Terminates with `status: "completed"` / `finish_reason: "tool_calls"`

This matches the OpenAI API behavior where the model can produce both text and tool_calls in one turn.

### 9.5 Multiple client tool calls in one turn

The agent may call multiple client tools simultaneously (e.g., `get_weather` and `search_web`). Rosetta emits multiple `FunctionCall` items / `tool_calls` entries. The client executes them and sends results back in the next request. All tool results are fed to the agent together (in order).

### 9.6 Client sends tool definitions but never expects tool_calls (e.g., for prompt context only)

Some clients include tool definitions just for context (to influence the model's behavior). Rosetta can't distinguish this use case — if tool definitions are sent and the agent calls one, Rosetta forwards the call. The client can always choose to ignore the `tool_calls` in the response.

---

## 10. Implementation Order

### Phase 1: Restore core routing (atomic, testable)
1. Restore `consumer_tool_names` in `ResponseAccumulator`, `ChatChunkAccumulator`
2. Add branching: client tool → `FunctionCall` / agent tool → `Reasoning`
3. Restore `response_to_chat_completion` FunctionCall handling
4. Restore `ChatChunkAccumulator` client tool_call + tool_call_update handlers
5. Add `format_rosetta_harness_prompt` and inject it in routes.rs
6. ✅ All 55 existing tests pass (behavior unchanged when `consumer_tool_names` empty)

### Phase 2: Multi-turn flow
1. Restore `session_cache.rs`
2. Restore session cache logic in `create_response`
3. Implement session caching on client tool call detection
4. Implement `previous_response_id` → feed tool results → continue
5. ✅ Integration test: multi-turn tool call cycle

### Phase 3: Streaming
1. Pass `consumer_tool_names` to streaming accumulators
2. Forward client tool calls as SSE events / chunks
3. ✅ Integration test: streaming + tool_call

### Phase 4: Polish
1. Update all integration tests
2. Document the harness prompt contract
3. Remove unused `extract_tool_call_arguments` if superseded (keep for now)

---

## 11. Open Questions

1. **Should `tool_choice` / `parallel_tool_calls` be forwarded?** The Responses API supports `tool_choice: "auto" | "required" | "none"`. With the harness prompt, the agent already decides which tools to use — `tool_choice` could be injected as an additional instruction in the harness prompt. `parallel_tool_calls` is harder to control since the ACP agent handles parallelism internally.

2. **Chat Completions `tool_choice`?** Same consideration — could be a harness instruction.

3. **Should Rosetta inject the full `[Tool Definitions]` JSON or just the names?** The agent already has its own tool definitions via skills/MCP. The JSON format in `[Tool Definitions]` might not match the agent's internal tool schema. A simpler approach: the harness prompt lists tool names + descriptions, and the `[Tool Definitions]` block is kept as-is for reference. The agent figure out parameters from context.

4. **What about `strict` mode?** OpenAI's `strict: true` for structured outputs is a client-side concern. The ACP agent doesn't support it, so Rosetta ignores it.

---

## 12. Success Criteria

- [ ] Agent-internal tool calls (skills, MCP) produce `Reasoning` items / silently consumed in Chat
- [ ] Client-defined tool calls produce `FunctionCall` / `tool_calls` with correct names and arguments
- [ ] Empty `req.tools` → all tool calls are agent-internal (existing behavior, 0 regression)
- [ ] Non-streaming: multi-turn flow works with `previous_response_id`
- [ ] Streaming: tool calls appear mid-stream, stream terminates cleanly
- [ ] Probing calls (empty args) are silently consumed
- [ ] All 55 existing tests still pass
