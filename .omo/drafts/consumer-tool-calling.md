---
slug: consumer-tool-calling
status: awaiting-approval
intent: clear
pending-action: write .omo/plans/consumer-tool-calling.md
approach: System prompt injection for tool definitions + intercepting ACP tool_call updates to emit OutputItem::FunctionCall
---

# Draft: consumer-tool-calling

## Components (topology ledger)

| id | outcome | status | evidence path |
|----|---------|--------|--------------|
| tool-call-translation | Change process_update() tool_call branch to emit OutputItem::FunctionCall instead of Reasoning | planned | translate.rs L275-317 |
| tool-def-injection | Inject consumer tools as [Tool Definitions] section in system prompt | planned | openai_input_to_acp_prompt at translate.rs L6 |
| tool-choice-wiring | Wire tools/tool_choice from request into prompt | planned | routes.rs create_response L105-155 |
| chat-tool-support | ChatChunkAccumulator emit tool_calls delta on tool_call update | planned | translate.rs L642-717 |
| mock-agent-ext | Extend mock_acp.py to support configurable tool_call behavior | planned | mock_acp.py |
| function-call-output | Ensure FunctionCallOutput forwarding works end-to-end | planned | InputItem::FunctionCallOutput already exists |
| integration-tests | New streaming integration tests for tool calling flows | planned | streaming_integration_test.rs |

## Decisions (with rationale)

1. **System prompt injection** (Option A, user-approved): Tool definitions are serialized into a `[Tool Definitions]\n` section prepended to the ACP agent's prompt. Rationale: ACP-agnostic, works with any agent that reads its prompt text. No protocol extensions needed.
2. **Emit FunctionCall from process_update()**: Change line 300-307 to create `OutputItem::FunctionCall` instead of `OutputItem::Reasoning`. Rationale: The OpenAI spec expects `function_call` typed output items, not reasoning items, for tool invocations. The current code was a placeholder.
3. **ChatCompletions: emit tool_calls delta**: When ChatChunkAccumulator receives a `tool_call` update, emit `ChatMessageDelta.tool_calls` with proper `ToolCall` format.
4. **No native ACP multi-turn session**: Each Responses API request = new ACP session. FunctionCallOutput creates a new prompt in a new session with accumulated context injected in the system prompt area.

## Findings (cited - path:lines)

- `OutputItem::FunctionCall` already exists in types (openai.rs L78-84) but is never constructed — current code uses `OutputItem::Reasoning` with `summary_type: "tool_call"` instead (translate.rs L300-307)
- `ResponseCreateRequest.tools` deserializes `Vec<ToolDefinition>` (openai.rs L16) but is never read — dead field in create_response()
- `ResponseCreateRequest.tool_choice` deserializes `Option<ToolChoice>` (openai.rs L18) but is never read
- Mock agent already emits `tool_call` with `toolCallId`, `title`, `name`, `arguments` (mock_acp.py L122-129)
- `openai_input_to_acp_prompt()` already handles `InputItem::FunctionCallOutput` (translate.rs L36-38) — injects `[Tool Result: call_id]\noutput` in the prompt
- `ChatMessageDelta` already has `tool_calls: Option<Vec<ToolCall>>` (openai.rs L285) — ready for Chat Completions
- `ChatCompletionsRequest.tools` deserializes `Vec<ChatToolDefinition>` (openai.rs L170) but is never read
- `ChatCompletionsRequest.tool_choice` deserializes `Option<Value>` (openai.rs L172) but is never read
- `finalize()` at translate.rs L354 does not need special FunctionCall handling — tool_call items are already pushed to output_items during process_update() (line 309)
- Integration tests at streaming_integration_test.rs contain 12 existing tests — must pass without regression

## Scope IN

- Translate ACP `tool_call` updates to OpenAI `OutputItem::FunctionCall` (Responses API)
- Inject consumer tool definitions into ACP system prompt as `[Tool Definitions]\n`
- Wire `tools`/`tool_choice` from request into the prompt pipeline
- Chat Completions: emit `tool_calls` delta for `tool_call` updates
- Accept `FunctionCallOutput` from consumer and forward to ACP agent (already partially works)
- Extend mock agent for deterministic tool_call testing
- Integration tests for all new flows
- Streaming and non-streaming modes
- Update documentation (response structure table, roadmap)

## Scope OUT (Must NOT have)

- Full tool execution loop (Rosetta does NOT execute the tool itself — only detects/translates/forwards)
- Parallel tool calls in a single update (agent emits one tool_call at a time; consumer may batch results)
- MCP-based tool definitions (MCP servers are separate from consumer `tools`)
- Agent-side skill evaluation (opencode-side concern)
- Token usage tracking (out of scope per roadmap)
- ACP protocol extension for tools (we use prompt injection only)

## Open questions

None — spec was already clarified with user (chose Option A: system prompt injection).
