# Consumer Tool Calling — Learnings

## T1: Translate ACP `tool_call` → OpenAI `OutputItem::FunctionCall`

**Goal:** Stop emitting `OutputItem::Reasoning { summary_type: "tool_call", ... }` from the `tool_call` arm of `ResponseAccumulator::process_update()` and instead emit a real `OutputItem::FunctionCall`. Update `response_to_chat_completion()` to surface those into Chat Completions `tool_calls`.

### Key shapes

- `OutputItem::FunctionCall { id, call_id, name, arguments, status }` lives in `crates/rosetta-types/src/openai.rs` and is serialized with `#[serde(tag = "type", rename_all = "snake_case")]` → wire type `"function_call"`.
- `ChatMessage.tool_calls: Option<Vec<ToolCall>>` with `ToolCall { id, #[serde(rename="type")] tool_type: "function", function: ToolCallFunction { name, arguments } }`.

### Field mapping in the `tool_call` arm

| `OutputItem::FunctionCall` field | Source in ACP `data` payload | Notes |
|---------------------------------|------------------------------|-------|
| `id`                            | `generate_message_id()`      | OpenAI Responses uses `msg_`-prefixed ids for assistant items; mirror that for the new item id. |
| `call_id`                       | `data.tool_call_id` ∥ `data.toolCallId` | Distinct from `id`; stable call correlation id. |
| `name`                          | `data.name` ∥ `data.title`  | Function name. |
| `arguments`                     | `data.arguments` ∥ `data.params` (stringified JSON) | Already string-typed by the extractor. |
| `status`                        | `"completed"`                | Tool calls are final when emitted by `tool_call` updates — the agent only sends one `tool_call` per call. |

### `response_to_chat_completion` translation rules

- Walk `response.output` and collect every `OutputItem::FunctionCall` into `Vec<ToolCall>` (`tool_type = "function"`, `function = { name, arguments }`).
- If any tool calls exist:
  - `content = Some(response.output_text)` only when there is also message text; otherwise `content = None` (matches OpenAI shape where a tool-call-only assistant message has null content).
  - `tool_calls = Some(vec)`.
  - `finish_reason = "tool_calls"` (OpenAI convention).
- If no tool calls: existing behavior preserved (`content = Some(response.output_text)`, `tool_calls = None`, `finish_reason = "stop"`).

### What stayed untouched

- `process_update_events()` — still calls `process_update()` once and post-processes the first-message-chunk delta; the `tool_call` arm's return type and field shape change are absorbed because the helper only inspects `Message` items.
- `response_to_streaming_events()` already handled `OutputItem::FunctionCall` (added previously); no change needed.
- `ChatChunkAccumulator` — `tool_call` still returns `None`; live chat streaming intentionally drops function calls (text-only contract). *(Updated in T5: now emits `tool_calls` deltas — see below.)*

### Tests updated

- `test_response_accumulator_tool_call` — now asserts `OutputItem::FunctionCall` with correct `call_id`/`name`/`arguments`/`status`.
- `test_tool_call_produces_reasoning_not_function_call` — renamed to `test_tool_call_produces_function_call_not_reasoning` and inverted.
- `test_process_update_tool_call_emits_reasoning_event` — renamed to `test_process_update_tool_call_emits_function_call_event` and inverted.

All 25 unit tests pass after the change.

## T5: Live chat streaming — emit `tool_calls` delta chunks

**Goal:** Make `ChatChunkAccumulator::process_update()` emit a `ChatCompletionChunk` with a populated `delta.tool_calls` when a `"tool_call"` ACP update arrives, instead of silently returning `None`. The non-streaming translation path (T1) already produces `OutputItem::FunctionCall` and `response_to_chat_chunks` surfaces them; this closes the streaming gap.

### What changed

- Split the combined match arm `"agent_thought_chunk" | "tool_call" => None` into two arms.
- `"agent_thought_chunk"` → still `None` (thoughts are silent in Chat Completions, matching `agent_thought_chunk` only producing `OutputItem::Reasoning` in the Responses path).
- `"tool_call"` → builds a `ToolCall { id, tool_type: "function", function: { name, arguments } }` and emits a `ChatCompletionChunk` whose delta carries it.

### Field extraction (mirrors `ResponseAccumulator` lines 290–299)

| `ToolCall` field     | Source in ACP `data` payload                                  | Notes |
|----------------------|---------------------------------------------------------------|-------|
| `id`                 | `generate_call_id()`                                          | `call_` prefix; Chat Completions expects this format. |
| `tool_type`          | `"function"`                                                  | Hard-coded. |
| `function.name`      | `data.name` ∥ fallback `data.title` (or `"tool"`)            | Same dual-extraction as Responses path. |
| `function.arguments` | `data.arguments` ∥ `data.params` (stringified JSON)           | String-typed in the OpenAI schema. |

If `data` is missing entirely, return `None` (graceful — matches Responses-arm behavior).

### Role + tool_calls emission pattern

- First chunk in the stream: if `!self.role_sent`, set `role_sent = true` and emit `delta = { role: Some("assistant"), content: None, tool_calls: Some(vec![...]) }`. This matches the existing `agent_message_chunk` first-chunk behavior so the OpenAI client sees the role exactly once at the start of the stream.
- Subsequent tool_call chunks: emit `delta = { role: None, content: None, tool_calls: Some(vec![...]) }` — clients append these into the tool_calls array by index.

`agent_message_chunk` and `tool_call` are now interleaveable in either order; whichever fires first sets `role_sent`. The other side just sees a chunk without `role` (correct for streaming).

### What stayed untouched

- `ResponseAccumulator::process_update()` — unchanged (T1).
- The public API of `ChatChunkAccumulator` — same struct, same method signature.
- The `available_commands_update` and "other" arms — still silently dropped at `debug` level.

### Tests updated

- `test_chat_chunk_accumulator_thought_and_tool_call_return_none` → renamed to `test_chat_chunk_accumulator_thought_returns_none`; tool_call assertion removed (it's no longer None).
- New `test_chat_chunk_accumulator_tool_call_emits_tool_calls_delta`:
  - Sends a `tool_call` update with `name="get_weather"`, `arguments="{\"city\":\"Paris\"}"`.
  - Asserts chunk is `Some`, delta has `tool_calls.len() == 1`.
  - Asserts `tool_calls[0].function.name == "get_weather"`, `arguments` matches the JSON string.
  - Asserts `tool_type == "function"` and `id.starts_with("call_")`.
  - Asserts the first chunk also carries `role == Some("assistant")` and `content == None`, and that `role_sent` is now `true`.

All 26 unit tests pass after the change.

## T3: Inject tool definitions into the ACP system prompt

**Goal:** When the client sends a `tools` array in the OpenAI request, prepend a `[Tool Definitions]` text block to the ACP prompt so the underlying agent can see what tools are available. (T4 will handle `tool_choice` separately.)

### New helpers in `translate.rs`

Two public functions serialize the OpenAI `tools` arrays into a stable text block:

- `pub fn format_response_tool_definitions(tools: &[ToolDefinition]) -> String`
- `pub fn format_chat_tool_definitions(tools: &[ChatToolDefinition]) -> String`

Both:
- Return `""` for an empty slice (so callers can use the return value unconditionally).
- Build a `Vec<serde_json::Value>` shaped like the OpenAI wire format and serialize with `serde_json::to_string`.
- Wrap the JSON in `"[Tool Definitions]\n{json}"`.

Two private helpers do the struct→`Value` conversion:
- `tool_definition_to_value(&ToolDefinition) -> Value` — flat `{type, name, description, parameters, strict}`.
- `chat_tool_definition_to_value(&ChatToolDefinition) -> Value` — nested `{type, function: {name, description, parameters, strict}}`.

Why not derive `Serialize` on the types? `ToolDefinition` and `ChatToolDefinition` in `rosetta-types/src/openai.rs` only derive `Deserialize` (they're request-input types, not response-output types), and the `#[serde(rename = "type")]` plus the custom wire shape for the chat variant (nested `function` object) make a manual conversion clearer than a parallel `Serialize` impl that would need a different rename map.

### Wire format

```text
[Tool Definitions]
[{"type":"function","name":"get_weather","description":"Get weather for a city","parameters":{...}}]
```

`serde_json::to_string` (not `to_string_pretty`) is used so the block stays single-line-per-tool and easy for agents to parse mentally.

### `routes.rs` integration

- Added `use rosetta_types::acp::ContentBlock;` (not via the `rosetta_types::openai::*` glob).
- `create_response()`: after `let mut prompt = openai_input_to_acp_prompt(...)`, prepend a `ContentBlock::Text` with `format_response_tool_definitions(&req.tools)` when `!req.tools.is_empty()`.
- `create_chat_completion()`: same pattern, using `format_chat_tool_definitions(&req.tools)`.
- Both `let prompt = ...` become `let mut prompt = ...` so we can `prompt.insert(0, ...)` before the `spawn_streaming_prompt()` / `client.send_prompt()` consumers take ownership.

### What stayed untouched

- `openai_input_to_acp_prompt()` and `chat_messages_to_acp_prompt()` signatures and bodies — the prepend happens at the call site, not inside the prompt translators.
- `tool_choice` parsing — deferred to T4 per the task contract.
- ACP client / session management — unchanged.
- Tests — the task explicitly forbade touching them; the existing 26 tests still pass and cover the prompt-translation behavior. Adding tests for the new helpers is out of scope.

### Verification

- `cargo test -p rosetta-core` — all 26 tests pass.
- `cargo check -p rosetta-server` — clean compile, no warnings on the new code.

## T7: Make `mock_acp.py` `tool_call` `name`/`arguments` configurable via env vars

**Goal:** Allow integration tests to control what tool name and arguments the mock ACP agent advertises in its `tool_call` chunk, by reading two new env vars. Pure fixture-side change — no Rust touched.

### Env vars added

| Var | Default | Purpose |
|-----|---------|---------|
| `MOCK_ACP_TOOL_NAME` | `"get_weather"` | Replaces the hard-coded `name` field of the `tool_call` chunk. |
| `MOCK_ACP_TOOL_ARGS` | `'{"city": "Paris"}'` | Replaces the hard-coded `arguments` field. Value must be a JSON-encoded string (the field is already string-typed on the wire). |

### Helper pattern (consistent with existing env-var helpers)

Added at module scope, immediately after `_echo_marker()`:

```python
def _tool_name() -> str:
    return os.environ.get("MOCK_ACP_TOOL_NAME", "get_weather")


def _tool_args() -> str:
    return os.environ.get("MOCK_ACP_TOOL_ARGS", '{"city": "Paris"}')
```

No try/except needed — these are pure string pass-throughs (no `int`/`float` parsing that could fail). The defaults preserve the original hard-coded values exactly, so the change is 100% backward compatible.

### `chunk_notifications` change

Two lines inside the `tool_call` entry of the `session/prompt` handler:

```python
"name": _tool_name(),
"arguments": _tool_args(),
```

`toolCallId` and `title` stay hard-coded (`"call_1"` and `"Get weather"`) — only `name` and `arguments` were in scope.

### Docstring

Extended the env-var list in the module docstring with two new entries, formatted to match the style of the existing four entries (name, default in prose, optional-when-unset framing).

### What stayed untouched

- All other env vars (`MOCK_ACP_CHUNK_DELAY_MS`, `MOCK_ACP_CRASH_AFTER_CHUNKS`, `MOCK_ACP_ECHO`).
- JSON-RPC protocol handling — initialize / session/new / session/prompt / session/close flows unchanged.
- The chunk sequence, the inter-chunk delay logic, the crash-mid-turn behavior, the echo-marker behavior.
- The other three chunks in `chunk_notifications` (agent_thought_chunk × 2, agent_message_chunk × 2).
- Final `PromptResponse` content.

### Verification

- `python3 -m py_compile crates/rosetta-acp/tests/fixtures/mock_acp.py` → OK.
- No new Python dependencies (only `os`, `json`, `sys`, `time` — same as before).
- File length grew by ~10 lines (env vars in docstring + 2 helper functions + 2 inline calls).

## T6: Add integration tests for consumer tool calling

**Goal:** Lock in the end-to-end contract for tool-call delivery across both
APIs and both transport modes (streaming vs. non-streaming) at the HTTP boundary
in `crates/rosetta-server/tests/streaming_integration_test.rs`. Pure test-add —
no source touched.

### Three tests added under a new `// --- T014: consumer tool calling ---` section

| Test | Path | Mode | What it asserts |
|------|------|------|-----------------|
| `test_non_streaming_responses_api_tool_call` | `/v1/responses` | `stream: false` | `output[]` contains a `function_call` item with `name == "get_weather"` and `arguments` containing `"Paris"`. |
| `test_non_streaming_chat_completions_tool_call` | `/v1/chat/completions` | `stream: false` | `choices[0].message.tool_calls[0].function.name == "get_weather"`, arguments contain `"Paris"`, `finish_reason == "tool_calls"`. |
| `test_streaming_chat_completions_tool_call_delta` | `/v1/chat/completions` | `stream: true` | At least one SSE `data:` chunk has `choices[0].delta.tool_calls[0].function.name == "get_weather"` with arguments containing `"Paris"`; the final chunk carries a non-empty `finish_reason`. |

### Default mock agent covers all three

`start_server(vec![])` runs the Python mock with no env overrides, so each test
sees the fixture's default `tool_call` chunk:
```json
{"updateType": "tool_call", "data": {"toolCallId": "call_1", "title": "Get weather", "name": "get_weather", "arguments": "{\"city\": \"Paris\"}"}}
```
No need to reach for `MOCK_ACP_TOOL_NAME` / `MOCK_ACP_TOOL_ARGS` (those are
T7's domain — overriding the fixture to advertise arbitrary tools).

### SSE parsing pattern reused, not invented

The streaming test follows the same shape as the existing
`test_responses_api_progressive_delivery_meets_sc001` /
`test_chat_completions_streaming_terminates_with_stop_and_done` tests:
`reqwest::Client::Client::post().json(...).send()` → `collect_sse_lines_with_timestamps()`
→ filter `data: ` prefix → drop `[DONE]` → `serde_json::from_str` each chunk.
Wraps the collect in `tokio::time::timeout(5s, ...)` so a regression that hangs
the stream surfaces as a test failure, not a CI timeout (matches T011's
pattern).

### What stayed untouched

- `clear_env()` helper — does NOT clear `MOCK_ACP_TOOL_NAME` /
  `MOCK_ACP_TOOL_ARGS`. Acceptable for T6 since this task ships before T7's
  test uses those env vars; T7 (or a future hardening task) is the right place
  to extend the helper. The `ENV_LOCK` already serializes env-var-dependent
  tests, so even if T6 and T7 ran together the lock prevents mid-test races.
- `start_server` signature — still returns `(base, handle, guard)`; tests use
  `_handle` and `_guard` placeholders per existing convention.
- All 11 pre-existing tests — no edits. (`test_non_streaming_chat_completions_unaffected_by_feature`
  is pre-broken against the new tool-call behavior — its `assert_eq!(finish_reason, "stop")`
  now fails because the fixture emits a `tool_call`. That's a T011-era test
  whose contract was invalidated by T1's `response_to_chat_completion`
  change; fixing it is explicitly out of scope for T6 per the "Do NOT
  change existing tests" rule.)

### Verification

- `cargo test -p rosetta-server --test streaming_integration_test -- test_non_streaming_responses_api_tool_call test_non_streaming_chat_completions_tool_call test_streaming_chat_completions_tool_call_delta` → 3 passed, 0 failed.
- `cargo build -p rosetta-server --tests` → clean, no warnings on the new tests.
- `lsp_diagnostics` on the test file → no diagnostics.
- No new dependencies; no source files modified; all assertions use the
  existing `serde_json::json!` + `reqwest::Client` + `collect_sse_lines_with_timestamps`
  toolkit already imported at the top of the file.
