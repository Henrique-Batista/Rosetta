# consumer-tool-calling - Work Plan

## TL;DR (For humans)
<!-- Fill this LAST, after the detailed plan below is written, so it summarizes the REAL plan. -->
<!-- Plain English for a non-engineer: NO file paths, NO todo numbers, NO wave/agent/tool names. -->

**What you'll get:** Rosetta consumers can send tool/function definitions when calling the API (just like OpenAI) and receive proper `function_call` outputs when the agent invokes a tool — instead of getting a generic "reasoning" blob.

**Why this approach:** System prompt injection is the simplest, most ACP-agnostic way to forward tool definitions — it works with any ACP agent without protocol changes. All the type infrastructure (`OutputItem::FunctionCall`, `ToolDefinition`, `FunctionCallOutput`) already exists in the codebase; the main work is bridging the gap between what's deserialized and what's actually used.

**What it will NOT do:** Rosetta will NOT execute the tools themselves (no tool execution loop). The consumer still receives the tool call and must submit the result. MCP server tools and agent-side skills are outside this scope.

**Effort:** Medium
**Risk:** Low - existing types already have FunctionCall/FuncionCallOutput enum variants; changes are additive, not refactoring
**Decisions to sanity-check:** System prompt injection approach (instead of ACP extension); tool call becomes FunctionCall output (not Reasoning)

Your next move: approve and run `$start-work consumer-tool-calling`. Full execution detail follows below.

---

> TL;DR (machine): Medium effort, Low risk. Make ACP tool_call → OpenAI FunctionCall. Inject tool definitions via system prompt. Wire tools/tool_choice from request. Chat Completions tool_calls delta. Extend mock agent. ~8 implementation tasks.

## Scope
### Must have
- Translate ACP `tool_call` → `OutputItem::FunctionCall` in ResponseAccumulator.process_update()
- Inject consumer `tools` as `[Tool Definitions]\n{json}` into the ACP prompt
- Wire `tools`/`tool_choice` from `ResponseCreateRequest` / `ChatCompletionRequest` into the prompt pipeline
- Emit `tool_calls` delta in `ChatChunkAccumulator` on `tool_call` updates
- Accept `FunctionCallOutput` from consumer and forward to ACP agent
- Extend mock ACP agent to emit configurable `tool_call` updates
- Full test coverage in rosetta-core and streaming integration tests
- Streaming and non-streaming paths

### Must NOT have (guardrails, anti-slop, scope boundaries)
- DO NOT implement a tool execution loop (Rosetta does NOT call the tool itself)
- DO NOT add ACP protocol extensions for tools
- DO NOT add MCP-based tool integration
- DO NOT refactor existing `process_update()` signature — use its current shape
- DO NOT break existing non-tool tests or streaming behavior

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: TDD — tests-first for every translation rule change
- Evidence: .omo/evidence/task-<N>-consumer-tool-calling.txt

## Execution strategy
### Parallel execution waves

**Wave 1 (core translation)**: T1, T2 — changes in translate.rs only, foundation for everything else.
**Wave 2 (tool injection + wiring)**: T3, T4 — build on Wave 1, modify routes.rs + translate.rs.
**Wave 3 (Chat Completions + mock agent)**: T5, T7 — parallel to Wave 2, no shared files.
**Wave 4 (integration tests + docs)**: T6, T8 — depends on Waves 2+3 completing.

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| T1 | — | T2, T3, T4, T5 | — |
| T2 | T1 | T6 | — |
| T3 | T1 | T4, T6 | T5, T7 |
| T4 | T3 | T6 | T5, T7 |
| T5 | T1 | T6 | T3, T4, T7 |
| T6 | T2, T4, T5, T7 | — | — |
| T7 | T1 | T6 | T3, T4, T5 |
| T8 | T6 | — | — |

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->
- [x] 1. Tool call → FunctionCall translation in ResponseAccumulator.process_update()
  What to do / Must NOT do: Change the "tool_call" branch in process_update() (translate.rs L275-317) to construct OutputItem::FunctionCall instead of OutputItem::Reasoning. Extract toolCallId, name, arguments from data. Use generate_message_id() for the FunctionCall id field. DO NOT change the function signature. DO NOT break existing agent_thought_chunk or agent_message_chunk handling.
  Parallelization: Wave 1 | Blocked by: none | Blocks: 2, 3, 4, 5
  References: translate.rs L275-317, openai.rs L78-84 (OutputItem::FunctionCall), openai.rs L222-228 (ToolCall struct), translate.rs L737-743 (generate_message_id)
  Acceptance criteria (agent-executable): cargo test -p rosetta-core test_tool_call_emits_function_call works. cargo test --workspace still passes all existing tests.
  QA scenarios: happy: send a tool_call SessionUpdate → verify OutputItemAdded with FunctionCall containing correct call_id/name/arguments. Evidence .omo/evidence/task-1-consumer-tool-calling.txt
  Commit: Y | feat(rosetta-core): emit OutputItem::FunctionCall instead of Reasoning for tool_call

- [x] 2. Unit tests for FunctionCall output from process_update()
  What to do / Must NOT do: Add tests in translate.rs #[cfg(test)] module: (a) tool_call update with all fields → verifies OutputItem::FunctionCall has correct id, call_id, name, arguments. (b) tool_call missing optional fields → verifies graceful defaults. (c) tool_call followed by agent_message_chunk → verifies message is a new output item (tool does NOT leak into message). (d) existing test_process_update_events_* tests still pass.
  Parallelization: Wave 1 | Blocked by: T1 | Blocks: T6
  References: translate.rs tests module starting ~L1100, existing test_process_update_events tests
  Acceptance criteria (agent-executable): cargo test -p rosetta-core 'test_tool_call' and related new test functions all pass.
  QA scenarios: Run the 3 new test functions. Evidence .omo/evidence/task-2-consumer-tool-calling.txt
  Commit: Y | test(rosetta-core): add tests for tool_call → FunctionCall translation

- [x] 3. Tool definition injection into system prompt
  What to do / Must NOT do: Modify openai_input_to_acp_prompt() (translate.rs L6-42) to accept an optional &[ToolDefinition] parameter. When tools are present, prepend a "[Tool Definitions]\n" text block before the user message. Format: JSON-serialized tool definitions with name, description, parameters. Also modify chat_messages_to_acp_prompt() similarly for ChatToolDefinition. Must NOT crash on empty tools vec. Must NOT inject tools when tools is empty.
  Parallelization: Wave 2 | Blocked by: T1 | Blocks: T4, T6 | Can parallelize with: T5, T7
  References: translate.rs L6-42 (openai_input_to_acp_prompt), openai.rs L112-123 (ToolDefinition), openai.rs L187-203 (ChatToolDefinition)
  Acceptance criteria (agent-executable): cargo test -p rosetta-core test_tool_definition_injection. Build passes.
  QA scenarios: Call openai_input_to_acp_prompt with tools → verify output contains "[Tool Definitions]\n". Call without tools → verify no tool definitions block. Evidence .omo/evidence/task-3-consumer-tool-calling.txt
  Commit: Y | feat(rosetta-core): inject tool definitions into ACP system prompt

- [x] 4. Wire tools/tool_choice from request into prompt pipeline
  What to do / Must NOT do: In routes.rs create_response(): pass req.tools and req.tool_choice through to the prompt construction. In routes.rs create_chat_completion(): pass req.tools and req.tool_choice. On the non-streaming path, pass them to openai_input_to_acp_prompt / chat_messages_to_acp_prompt. On the streaming path, do the same before spawn_streaming_prompt. Must NOT break existing streaming/non-streaming paths. DO NOT modify the AcpClient or streaming_task.rs.
  Parallelization: Wave 2 | Blocked by: T3 | Blocks: T6 | Can parallelize with: T5, T7
  References: routes.rs L105-155 (create_response), routes.rs L202-255 (create_chat_completion), openai.rs L16-18 (tools/tool_choice on request)
  Acceptance criteria (agent-executable): cargo build --release. All existing tests pass.
  QA scenarios: Send curl request with tools to Rosetta running with mock agent → verify tool definition appears in mock agent logs (echo mode). Evidence .omo/evidence/task-4-consumer-tool-calling.txt
  Commit: Y | feat(rosetta-server): wire tools/tool_choice from request into prompt

- [x] 5. Chat Completions tool_calls delta support
  What to do / Must NOT do: In ChatChunkAccumulator.process_update() (translate.rs L642-717), add a "tool_call" match arm that emits a ChatCompletionChunk with tool_calls delta containing a ToolCall with the correct id, type: "function", name, and arguments. The finish_reason should NOT be set (agent will send more chunks after tool_call). DO NOT break existing agent_message_chunk and agent_thought_chunk handling.
  Parallelization: Wave 3 | Blocked by: T1 | Blocks: T6 | Can parallelize with: T3, T4, T7
  References: translate.rs L642-717, openai.rs L260-286 (ChatCompletionChunk, ChatChoiceDelta, ChatMessageDelta, ToolCall), openai.rs L222-234 (ToolCall, ToolCallFunction)
  Acceptance criteria (agent-executable): cargo test -p rosetta-core test_chat_tool_call_delta. All existing chat tests pass.
  QA scenarios: Send tool_call update through ChatChunkAccumulator → verify chunk has tool_calls with correct id/function.name/function.arguments. Evidence .omo/evidence/task-5-consumer-tool-calling.txt
  Commit: Y | feat(rosetta-core): emit tool_calls delta in ChatChunkAccumulator for tool_call updates

- [x] 6. Integration tests for tool calling flows
  What to do / Must NOT do: Add integration tests in streaming_integration_test.rs: (a) test_responses_api_tool_call_returns_function_call — send request with tools, verify SSE contains function_call output item. (b) test_chat_completions_tool_call_returns_tool_calls_delta — send chat request with tools, verify SSE contains tool_calls delta. (c) test tool_call in non-streaming path. MUST use mock agent which already emits tool_call. MUST NOT break 12 existing tests.
  Parallelization: Wave 4 | Blocked by: T2, T4, T5, T7 | Blocks: T8
  References: streaming_integration_test.rs, mock_acp.py
  Acceptance criteria (agent-executable): cargo test -p rosetta-server --test streaming_integration_test passes all tests including new ones.
  QA scenarios: Run all integration tests. Evidence .omo/evidence/task-6-consumer-tool-calling.txt
  Commit: Y | test(rosetta-server): add integration tests for tool calling flows

- [x] 7. Extend mock agent for configurable tool_call behavior
  What to do / Must NOT do: Add env var MOCK_ACP_TOOL_NAME and MOCK_ACP_TOOL_ARGS to mock_acp.py (optional, defaults unchanged). When set, the mock agent uses these values in the tool_call notification instead of hardcoded "get_weather" / '{"city": "Paris"}'. Also add MOCK_ACP_TOOL_CALL=0 option to skip the tool_call notification entirely (for testing requests without tool calls). Must keep existing behavior unchanged when env vars are not set.
  Parallelization: Wave 3 | Blocked by: T1 | Blocks: T6 | Can parallelize with: T3, T4, T5
  References: mock_acp.py L122-129 (hardcoded tool_call), mock_acp.py L14-24 (existing env var pattern)
  Acceptance criteria (agent-executable): python3 crates/rosetta-acp/tests/fixtures/mock_acp.py < test_input works with new env vars.
  QA scenarios: Run with MOCK_ACP_TOOL_NAME="search" MOCK_ACP_TOOL_ARGS='{"q":"test"}' → verify tool_call uses these values. Evidence .omo/evidence/task-7-consumer-tool-calling.txt
  Commit: Y | test(mock-acp): add configurable tool name/args via env vars

- [x] 8. Update documentation
  What to do / Must NOT do: Update AGENTS.md Response Structure table: change "tool_call → OutputItem::Reasoning (exposed as reasoning, not function call)" to "tool_call → OutputItem::FunctionCall (type: function_call)". Update Roadmap "Tool call execution loop" status to reflect partial implementation (detection + translation done, execution loop still future). Update Important Notes if needed.
  Parallelization: Wave 5 | Blocked by: T6 | Blocks: none
  References: AGENTS.md L169-177 (response structure table), AGENTS.md L273-281 (roadmap)
  Acceptance criteria (agent-executable): grep for "function_call" in AGENTS.md confirms the change. No remaining references to "exposed as reasoning" for tool calls.
  QA scenarios: Read AGENTS.md and verify accuracy. Evidence .omo/evidence/task-8-consumer-tool-calling.txt
  Commit: Y | docs: update AGENTS.md for consumer tool calling feature

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [x] F1. Plan compliance audit — verify all scope items are covered and scope-out items are NOT implemented
- [x] F2. Code quality review — no dead code, no unsafe, no any, proper error handling
- [x] F3. Full workspace test suite — cargo test --workspace passes (all crates)
- [x] F4. Scope fidelity — build passes, manual curl test against mock agent shows function_call output

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [x] F1. Plan compliance audit
- [x] F2. Code quality review
- [x] F3. Real manual QA
- [x] F4. Scope fidelity

## Commit strategy

## Success criteria
