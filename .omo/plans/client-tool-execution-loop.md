# Client Tool Execution Loop — Incremental Plan

## 0. Context

### What exists today (commit state)

The dual routing infrastructure is built but **disabled**:

- `ResponseAccumulator` and `ChatChunkAccumulator` both have `consumer_tool_names: Vec<String>` fields and branching logic: client tool → `FunctionCall` / `tool_calls`, agent tool → `Reasoning` / reasoning delta
- `format_rosetta_harness_prompt()` is defined in `translate.rs` — generates a 4-line `[Rosetta Harness]` block listing client tool names and instructing the agent to call them by name
- `previous_response_id` is deserialized from `ResponseCreateRequest` but never read
- `InputItem::FunctionCallOutput` → `[Tool Result: call_id]` prompt translation exists

What's **missing**:
- `consumer_tool_names` hardcoded to `vec![]` in both route handlers (routes.rs:137, routes.rs:241)
- Harness prompt never injected into ACP prompt
- No `SessionCache` — file was removed; sessions always close after one turn
- No `previous_response_id` handling
- `AppState` has no cache field
- `tool`-role messages not translated to `[Tool Result]` for Chat Completions

### Decisions from architecture interview

| # | Decision |
|---|----------|
| D1 | **Client tools take priority.** Harness prompt tells agent which tools to call by name vs execute internally. Agent tools (skills, MCPs) remain available, surfaced as `Reasoning` |
| D2 | **Both API patterns needed:** Responses API with `SessionCache` + Chat Completions with `tool`-role messages |
| D3 | **Harness always injected** when `req.tools` non-empty, with `ROSETTA_HARNESS_DISABLED=1` escape hatch |
| D4 | **Streaming terminates on tool call.** Client starts new stream with results. Follow existing plan |
| D5 | **Incremental delivery.** Each phase independently buildable, testable, mergeable |

### Routing contract (dual awareness)

```
Client defines tool "get_weather" → consumer_tool_names = ["get_weather"]
Rosetta injects [Rosetta Harness] → "These tools are client-executed: get_weather"
Agent calls client tool → name matches → FunctionCall / tool_calls delta
Agent uses internal skill (grep) → name doesn't match → Reasoning / silently consumed
```

Backward compatible: empty `req.tools` → no harness, all tool calls → `Reasoning`.

---

## Phase 1 — Enable Client Tool Routing (activate existing infrastructure)

**Goal:** Client-defined tools appear as `FunctionCall`/`tool_calls`. Harness prompt tells agent which tools are client-executed. All existing tests pass. Zero new infrastructure — just wire what exists.

### Changes

#### 1.1 `crates/rosetta-server/src/routes.rs` — populate `consumer_tool_names`

Replace both `let consumer_tool_names: Vec<String> = vec![]` hardcodes (L137, L241) with:

```rust
let consumer_tool_names: Vec<String> = req.tools.iter().map(|t| match t {
    ToolDefinition { name, .. } => name.clone(),
}).collect();
```

For Chat Completions, same extraction from `req.tools` (which is `Vec<ChatToolDefinition>` — extract `t.function.name`):

```rust
let consumer_tool_names: Vec<String> = req.tools.iter().map(|t| t.function.name.clone()).collect();
```

#### 1.2 `crates/rosetta-server/src/routes.rs` — inject harness prompt

In both `create_response` (after L133-134) and `create_chat_completion` (after L237-238), prepend the harness prompt when `consumer_tool_names` is non-empty:

```rust
let harness_disabled = std::env::var("ROSETTA_HARNESS_DISABLED")
    .map(|v| v == "1")
    .unwrap_or(false);
if !consumer_tool_names.is_empty() && !harness_disabled {
    let harness = format_rosetta_harness_prompt(&consumer_tool_names);
    prompt.insert(0, ContentBlock::Text { text: harness });
}
```

Order in prompt: `[Rosetta Harness]` → `[Tool Definitions]` → `[System]` → messages.

#### 1.3 `crates/rosetta-core/src/translate.rs` — no changes needed

The routing logic in `process_update` and `ChatChunkAccumulator::process_update` already branches on `self.consumer_tool_names.iter().any(|n| n == &name)`. It just needs non-empty input.

### Tests

- **Existing:** all 52 tests pass with `consumer_tool_names = vec![]` (empty tools request)
- **New unit test:** `test_client_tool_call_produces_function_call` — send `tool_call` with name matching `consumer_tool_names`, assert `OutputItemAdded` event with `OutputItem::FunctionCall`
- **New unit test:** `test_agent_tool_call_still_produces_reasoning` — send `tool_call` with name NOT matching, assert `Reasoning` output
- **New unit test:** `test_harness_prompt_generated_when_tools_present` — assert prompt contains `[Rosetta Harness]`
- **New integration test:** `test_chat_completions_produces_tool_calls_delta` — mock agent emits client tool call, stream response contains `tool_calls` in delta

### Acceptance criteria

- [x] `curl` with `tools: [{type: "function", function: {name: "get_weather"}}]` produces `FunctionCall` in Responses API response
- [x] Chat Completions with tools produces `tool_calls` delta in stream
- [x] Agent-internal tool calls (skills, MCP) still appear as `Reasoning`
- [x] `ROSETTA_HARNESS_DISABLED=1` suppresses harness prompt
- [x] Empty tools → no harness, no FunctionCall, behavior identical to today
- [x] All 52 existing tests pass

---

## Phase 2 — Responses API Multi-turn Flow

**Goal:** `SessionCache` restores session continuity. Client sends tool result via `previous_response_id` → agent continues processing → final text response.

### Changes

#### 2.1 `crates/rosetta-server/src/session_cache.rs` — **NEW FILE**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use rosetta_acp::client::AcpClient;
use tracing::warn;

struct CacheEntry {
    client: AcpClient,
    session_id: String,
    inserted_at: Instant,
}

pub struct SessionCache {
    entries: Arc<Mutex<HashMap<String, CacheEntry>>>,
    ttl: Duration,
}

impl SessionCache {
    pub fn new(ttl: Duration) -> Self {
        Self { entries: Arc::new(Mutex::new(HashMap::new())), ttl }
    }

    pub async fn insert(&self, response_id: String, client: AcpClient, session_id: String) {
        let mut map = self.entries.lock().await;
        self.evict_expired_locked(&mut map);
        map.insert(response_id, CacheEntry { client, session_id, inserted_at: Instant::now() });
    }

    pub async fn take(&self, response_id: &str) -> Option<(AcpClient, String)> {
        let mut map = self.entries.lock().await;
        self.evict_expired_locked(&mut map);
        map.remove(response_id).map(|e| (e.client, e.session_id))
    }

    fn evict_expired_locked(&self, map: &mut HashMap<String, CacheEntry>) {
        let now = Instant::now();
        let expired: Vec<String> = map.iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) > self.ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            if let Some(entry) = map.remove(&key) {
                warn!(response_id = %key, "Session cache entry expired");
                // client.shutdown() must be called — transport has no Drop
                tokio::spawn(async move {
                    let _ = entry.client.shutdown().await;
                });
            }
        }
    }
}
```

TTL default: 5 minutes (matching `dual-tool-routing.md` §5.3).

#### 2.2 `crates/rosetta-server/src/main.rs` — wire `SessionCache` into `AppState`

Add `session_cache: Arc<SessionCache>` to `AppState` (routes.rs L17-22) and instantiate in `main.rs`:

```rust
let state = Arc::new(AppState {
    acp_command: cfg.acp_command,
    acp_args: cfg.acp_args,
    cwd: cfg.cwd,
    mcp_servers: cfg.mcp_servers,
    session_cache: Arc::new(SessionCache::new(Duration::from_secs(300))),
});
```

Add `pub mod session_cache;` to `crates/rosetta-server/src/lib.rs`.

#### 2.3 `crates/rosetta-server/src/routes.rs` — session lookup on `previous_response_id`

In `create_response`, before creating a new client:

```rust
let (mut client, session_id) = if let Some(ref prev_id) = req.previous_response_id {
    match state.session_cache.take(prev_id).await {
        Some((cached_client, cached_session_id)) => {
            info!(prev_id, "Reusing cached session for previous_response_id");
            (cached_client, cached_session_id)
        }
        None => {
            return Err(StatusCode::NOT_FOUND); // cache expired or invalid id
        }
    }
} else {
    // New session as before
    let client = create_acp_client(&state, &model).await?;
    let session_resp = client.new_session(&state.cwd, mcp).await?;
    // configure_agent as before...
    (client, session_resp.session_id)
};
```

When `previous_response_id` is used: skip `new_session` and `configure_agent`.

#### 2.4 `crates/rosetta-server/src/routes.rs` — translate `FunctionCallOutput` → `[Tool Result]`

When `previous_response_id` is set, the request's `input` contains `FunctionCallOutput` items. Translate them to `[Tool Result: call_id]\n{output}` text blocks (already handled by `openai_input_to_acp_prompt` at translate.rs L37-39). Append these to the prompt **after** the harness/tool-definitions but **before** user messages:

```rust
if req.previous_response_id.is_some() {
    // openai_input_to_acp_prompt already translates FunctionCallOutput → [Tool Result]
    let result_blocks = openai_input_to_acp_prompt(&req.input.unwrap_or_default());
    prompt.extend(result_blocks);
} else {
    let mut user_prompt = openai_input_to_acp_prompt(&req.input.unwrap_or_default());
    prompt.append(&mut user_prompt);
}
```

Wait — this is tricky. `openai_input_to_acp_prompt` produces `[Tool Result]` blocks for `FunctionCallOutput` items. But in the normal (non-continuation) flow, the input is user messages. In the continuation flow, the input is `FunctionCallOutput` items. The translator handles both via the same `InputItem` enum.

Actually, the prompt construction already calls `openai_input_to_acp_prompt(req.input.as_ref().unwrap_or(...))`. The `FunctionCallOutput` items already translate correctly. The only difference: in continuation flow, we want the tool results **appended** to the existing conversation (which the agent already has in its session context), not replacing it. So the prompt should only contain the new `FunctionCallOutput` items, not the full conversation.

Fix: in continuation flow, only send the `FunctionCallOutput` items as the prompt (the agent already has the conversation history in its session). Filter the input:

```rust
let prompt = if req.previous_response_id.is_some() {
    // Continuation: only send tool results — agent already has conversation context
    let input = req.input.as_ref().unwrap_or(&ResponseInput::Text(String::new()));
    match input {
        ResponseInput::Items(items) => {
            items.iter()
                .filter(|item| matches!(item, InputItem::FunctionCallOutput { .. }))
                .map(|item| match item {
                    InputItem::FunctionCallOutput { call_id, output } => 
                        ContentBlock::Text { text: format!("[Tool Result: {}]\n{}", call_id, output) },
                    _ => unreachable!(),
                })
                .collect()
        }
        _ => vec![], // no tool results → empty prompt
    }
} else {
    // Normal flow: full conversation
    let mut prompt = openai_input_to_acp_prompt(&req.input.unwrap_or_default());
    // inject harness + tool definitions
    prompt
};
```

#### 2.5 `crates/rosetta-server/src/routes.rs` — conditional session close

After processing: if the response contains `FunctionCall` items, `session_cache.insert(response_id, client, session_id)` instead of `close_session`. Otherwise, close normally:

```rust
let has_function_calls = response.output.iter().any(|item| matches!(item, OutputItem::FunctionCall { .. }));
if has_function_calls {
    state.session_cache.insert(response_id.clone(), client, session_id).await;
} else {
    let _ = client.close_session(&session_id).await;
}
```

### Tests

- **New integration test:** `test_responses_api_multi_turn_tool_call` — request 1 produces FunctionCall, request 2 with `previous_response_id` continues and produces final text
- **New integration test:** `test_previous_response_id_not_found_returns_404` — expired/invalid cache key
- **New unit test:** `test_session_cache_insert_take` — basic cache semantics
- **New unit test:** `test_session_cache_ttl_eviction` — entries expire after TTL

### Acceptance criteria

- [x] Two-request cycle: client tool call → tool result → final text response
- [x] `previous_response_id` with expired cache → 404
- [x] Session closed on non-tool-call responses
- [x] Session cached on FunctionCall responses (kept alive for continuation)
- [x] Cache eviction calls `client.shutdown()` (no child process leak)
- [x] All existing tests + Phase 1 tests pass

---

## Phase 3 — Chat Completions Tool Call Support

**Goal:** Chat Completions API handles `tool`-role messages, emits `tool_calls` delta, sets `finish_reason: "tool_calls"`. No session cache needed — Chat uses the multi-message pattern.

### Changes

#### 3.1 `crates/rosetta-core/src/translate.rs` — `chat_messages_to_acp_prompt` tool-role support

Already handles `tool` role at L51-55:
```rust
"tool" => match &msg.tool_call_id {
    Some(call_id) => format!("[Tool Result: {}]\n", call_id),
    None => "[Tool Result]\n".to_string(),
},
```

But the message content also needs to be appended. Currently L58 is `msg.content.clone().unwrap_or_default()` — this already works because tool messages have `content: Some("tool output")`, so the full text becomes `[Tool Result: call_1]\n25°C`. ✅ No changes needed.

#### 3.2 `crates/rosetta-core/src/translate.rs` — `response_to_chat_completion` with tool_calls

Already handles `FunctionCall` → `tool_calls` + `finish_reason: "tool_calls"` at L663-689. ✅ No changes needed — activated by Phase 1's `consumer_tool_names` population.

#### 3.3 `crates/rosetta-core/src/translate.rs` — `response_to_chat_chunks` with tool_calls

Currently doesn't emit `tool_calls` in chunks. For non-streaming (batch mode), chunks should include tool call deltas. Add `FunctionCall` → chunk logic similar to streaming `ChatChunkAccumulator`:

```rust
// In response_to_chat_chunks: after content chunks, before final chunk
for item in &response.output {
    if let OutputItem::FunctionCall { id, call_id, name, arguments, .. } = item {
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
```

#### 3.4 `crates/rosetta-server/src/routes.rs` — `build_live_chat_sse_events` tool_call finish_reason

Already uses `accumulator.had_client_tool_call` for `finish_reason` at L297:
```rust
let finish_reason = if accumulator.had_client_tool_call { "tool_calls" } else { "stop" };
```
✅ No changes needed.

### Tests

- **New integration test:** `test_chat_completions_multi_message_tool_cycle` — request 1 with tools produces `tool_calls`, request 2 with `tool`-role message produces final text
- **New unit test:** `test_chat_messages_tool_role_formats_correctly` — assert `[Tool Result: call_1]\noutput`

### Acceptance criteria

- [x] Chat Completions with tools produces `tool_calls` in response and `finish_reason: "tool_calls"`
- [x] Client sends `tool`-role message with result → agent continues → final text response
- [x] Non-streaming chat chunks include tool call deltas
- [x] All existing tests + Phase 1-2 tests pass

---

## Phase 4 — Streaming Client Tool Calls

**Goal:** Client tool calls appear mid-stream, stream terminates with tool call, client starts new stream. Live detection in streaming accumulators.

### Changes

#### 4.1 `crates/rosetta-core/src/translate.rs` — client tool call in `ChatChunkAccumulator`

Already implemented at L1571-1590: consumer tool → `tool_calls` delta chunk. ✅ Activated by Phase 1.

#### 4.2 `crates/rosetta-core/src/translate.rs` — client tool_call_update in `ChatChunkAccumulator`

Already implemented at L1627-1641: updates tool call arguments via delta. ✅ Activated by Phase 1.

#### 4.3 `crates/rosetta-core/src/translate.rs` — `ResponseAccumulator::process_update_events` client tool_call

The non-streaming `process_update_events` handles tool_call in `process_update` (L410-434) — `OutputItem::FunctionCall` event. Already complete. ✅

#### 4.4 `crates/rosetta-server/src/routes.rs` — streaming cache decision

In `build_live_chat_sse_events` (L278-308), after the accumulator finishes, check `had_client_tool_call` and cache session:

```rust
StreamOutcome::Done => {
    if accumulator.had_client_tool_call {
        // Cache session for continuation (need response_id available here)
        // → requires passing response_id into the SSE builder
    }
    let finish_reason = if accumulator.had_client_tool_call { "tool_calls" } else { "stop" };
    // ... existing final chunk + [DONE]
}
```

Actually, streaming sessions need caching too. The Responses API streaming path (`build_live_response_events`) needs the same logic. But streaming caches only need to store the `(AcpClient, session_id)` — the `response_id` is generated before streaming starts.

Change signature: pass `response_id` and `session_cache` into `build_live_response_events` / `build_live_chat_sse_events`.

#### 4.5 `crates/rosetta-server/src/routes.rs` — streaming task owns client

Currently `spawn_streaming_prompt` takes ownership of `client` and shuts it down on completion. For caching, the task must **not** shutdown if tool calls were emitted. Change `StreamOutcome` to include a `Cache` variant or return the client:

```rust
pub enum StreamOutcome {
    Update(SessionUpdate),
    Done,
    Error(String),
    ToolCall { client: AcpClient, session_id: String },
}
```

When the agent produces tool calls, the streaming task yields `ToolCall` instead of `Done`, returning `client` ownership to the caller for caching.

Alternatively: add `had_tool_call: bool` field to `Done` and make the streaming task NOT call `client.shutdown()` when tool calls occurred — the caller then caches the client.

Simplest approach: make `spawn_streaming_prompt` accept a flag `cache_on_tool_calls: bool` and conditionally skip shutdown. The caller gets `Done` with the accumulator's `had_client_tool_call` state and decides to cache.

Actually, the streaming task already owns the client. Changing it to optionally return the client is the cleanest approach:

```rust
pub enum StreamOutcome {
    Update(SessionUpdate),
    Done { had_tool_call: bool },
    Error(String),
}
```

Then in the SSE builder:
```rust
StreamOutcome::Done { had_tool_call } => {
    if had_tool_call {
        // We still have the client (owned by the task). 
        // Need access to it — requires redesign.
    }
}
```

This is the trickiest part. Let me think of a simpler approach.

**Simpler approach:** Don't cache streaming sessions. The plan document §6 already says: "For streaming, there's no live mid-stream pause + resume (that would require HTTP/2 bidirectional streaming which is out of scope). The client gets the tool call, executes, and starts a new stream."

So for streaming: **don't cache**. The client gets the tool call, closes the stream, executes the tool, and starts a **new** request with the tool result. This new request can be non-streaming (which goes through the Phase 2 cache path) OR a new streaming request.

But wait — if the new request is streaming, we'd need the session to still be alive. Unless we create a new session for the continuation (which loses conversation context).

Actually, re-reading plan §5.5: `Chat Completions doesn't have previous_response_id. The client just includes the tool result as a tool-role message. A new session is created each time (no caching needed for Chat Completions).`

So for Chat Completions streaming: new session each time, full conversation in messages. No cache needed. ✅

For Responses API streaming: the stream terminates with the FunctionCall. The client starts a new request with `previous_response_id` which hits the Phase 2 cache. But if the first request was streaming, the session was never cached!

Solution for Responses API streaming: **don't stream on tool-call turns**. When `req.tools` is non-empty and `req.stream` is true, buffer the entire turn (non-streaming internally), then emit as streaming events. This way the session can be cached.

Or simpler: for Responses API streaming with tools, treat it the same as non-streaming after `previous_response_id` — the continuation request always creates a new streaming task from the cached session.

Actually, the simplest and most correct approach: **streaming caches the session in `spawn_streaming_prompt`**, and the task returns the client via a new `StreamOutcome` variant. Let me design this properly.

```rust
pub enum StreamOutcome {
    Update(SessionUpdate),
    Done,
    Error(String),
    DoneWithCache { client: AcpClient, session_id: String }, // NEW
}
```

In `spawn_streaming_prompt`:
- Normal completion → yield `DoneWithCache` only if `had_tool_call` is detected (track via the SSE builder's accumulator)
- Actually, the task doesn't know about tool calls — the accumulator in the SSE builder does
- So the task always yields `DoneWithCache { client, session_id }` on normal completion
- The SSE builder decides whether to cache or shutdown based on `accumulator.had_client_tool_call`

This means `Done` is removed and replaced with `DoneWithCache`. On error, `Error` is yielded (no client to return).

Then in `build_live_chat_sse_events` / `build_live_response_events`:
```rust
StreamOutcome::DoneWithCache { client, session_id } => {
    if accumulator.had_client_tool_call {
        state.session_cache.insert(response_id, client, session_id).await;
    } else {
        let _ = tokio::time::timeout(Duration::from_secs(3), client.shutdown()).await;
    }
    // emit final chunk / [DONE]
}
```

This is clean and doesn't leak child processes. Let me use this approach.

### Tests

- **New integration test:** `test_streaming_chat_completions_produces_tool_calls_mid_stream` — mock agent emits tool call, stream contains `tool_calls` delta
- **New integration test:** `test_streaming_responses_api_terminates_with_function_call` — mock agent emits tool call, stream ends with `response.completed` containing `FunctionCall`
- **Existing:** `test_streaming_chat_completions_content_only_no_tool_calls` — already passes, tests non-tool-call streaming

### Acceptance criteria

- [x] Streaming Chat Completions emits `tool_calls` delta mid-stream
- [x] Streaming Responses API emits `OutputItemAdded { FunctionCall }` event
- [x] Stream terminates correctly with tool call (finish_reason: "tool_calls" or status: "completed")
- [x] Streaming sessions cached when tool calls present (not leaked)
- [x] Streaming sessions closed when no tool calls (not leaked)
- [x] All existing tests + Phase 1-3 tests pass

---

## 5. Files Changed (summary)

| Phase | File | Change |
|-------|------|--------|
| 1 | `crates/rosetta-server/src/routes.rs` | Extract `consumer_tool_names` from `req.tools`, inject harness prompt |
| 1 | `crates/rosetta-core/src/translate.rs` | No changes (routing already built) |
| 1 | `crates/rosetta-core/src/lib.rs` | No changes |
| 2 | `crates/rosetta-server/src/session_cache.rs` | **NEW** — `SessionCache` with TTL eviction + shutdown on expire |
| 2 | `crates/rosetta-server/src/lib.rs` | Add `pub mod session_cache` |
| 2 | `crates/rosetta-server/src/main.rs` | Instantiate `SessionCache`, pass to `AppState` |
| 2 | `crates/rosetta-server/src/routes.rs` | `previous_response_id` → cache lookup, conditional close, tool result prompt |
| 3 | `crates/rosetta-core/src/translate.rs` | `response_to_chat_chunks` — add FunctionCall → chunk logic |
| 3 | `crates/rosetta-server/src/routes.rs` | No changes (already handles tool-role messages via `chat_messages_to_acp_prompt`) |
| 4 | `crates/rosetta-server/src/streaming_task.rs` | `StreamOutcome::DoneWithCache { client, session_id }` variant |
| 4 | `crates/rosetta-server/src/routes.rs` | Pass `session_cache` + `response_id` to SSE builders; cache/shutdown on `DoneWithCache` |
| 4 | `crates/rosetta-core/src/translate.rs` | No changes |

---

## 6. Dependency Graph

```
Phase 1 (enable routing) ───── no dependencies ───── can ship first
    │
    ├── Phase 2 (Responses API multi-turn) ─── needs Phase 1 (needs FunctionCall detection)
    │
    ├── Phase 3 (Chat Completions tools) ─── needs Phase 1 (needs tool_calls routing)
    │
    └── Phase 4 (streaming tools) ─── needs Phase 1-2-3 (needs all detection + cache)
```

Phases 2 and 3 are independent of each other and could be built in parallel, but Phase 4 needs both.

---

## 7. Success Criteria

- [x] Client-defined tools appear as `FunctionCall` / `tool_calls` in both APIs
- [x] Agent-internal tools (skills, MCP) continue as `Reasoning` — no regression
- [x] Responses API: two-request tool call cycle works end-to-end
- [x] Chat Completions: multi-message tool call cycle works end-to-end
- [x] Streaming: tool calls appear mid-stream, stream terminates cleanly
- [x] Sessions cached correctly, evicted on TTL, no child process leaks
- [x] `ROSETTA_HARNESS_DISABLED=1` suppresses harness prompt
- [x] Empty `req.tools` → identical behavior to today (backward compatible)
- [x] All 52+ existing tests pass + new phase-specific tests
