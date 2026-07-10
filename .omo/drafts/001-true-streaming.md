---
slug: 001-true-streaming
status: approved
intent: clear
review_required: false
pending-action: write .omo/plans/001-true-streaming.md
approach: Wire the existing AcpClient::send_prompt_streaming() into both HTTP route handlers via a task-owned bounded-channel architecture; reuse/extend ResponseAccumulator::process_update() to emit ResponseEvents live for the Responses API, and add an analogous per-update ChatCompletionChunk translator for Chat Completions; forward each raw ACP chunk as one delta (no artificial word re-splitting); handle disconnect/error cleanup via task abort + close_session.
---

# Draft: 001-true-streaming

## Components (topology ledger)
1. streaming-plumbing | task-owned AcpClient forwarding SessionUpdates over a bounded channel per streaming request | active | crates/rosetta-acp/src/client.rs:91-130
2. responses-incremental-events | live ResponseEvent emission via process_update() per update, streamed via response_event_stream_to_sse | active | crates/rosetta-core/src/translate.rs:92-266; crates/rosetta-server/src/streaming.rs:34-46
3. chat-incremental-chunks | new per-update ChatCompletionChunk translator + live SSE helper | active | crates/rosetta-core/src/translate.rs:517-579; crates/rosetta-server/src/streaming.rs:48-58
4. backpressure-bounded-buffer | bounded mpsc channel, sender backpressures instead of dropping (FR-009) | active | crates/rosetta-server/src/routes.rs:104-203
5. error-disconnect-cleanup | agent error -> terminal signal; client disconnect -> abort task + close_session within 5s (FR-004/005, US3) | active | crates/rosetta-acp/src/client.rs:215-217,247-262
6. test-infra | mock agent per-chunk delay knob + new integration tests (progressive delivery, clean completion, error/disconnect cleanup, concurrency isolation) | active | crates/rosetta-acp/tests/fixtures/mock_acp.py; crates/rosetta-acp/tests/integration_test.rs

## Open assumptions (announced defaults)
- No feature flag; true streaming replaces today's batch-then-replay streaming path outright | rationale: spec Assumptions only carve out non-streaming paths as unaffected; Roadmap already frames this as "the fix", not an opt-in | reversible: yes (could gate later)
- Bounded mpsc channel; full channel -> sender awaits capacity (backpressure), never drops content | rationale: satisfies FR-009 without violating FR-002 (no content loss) | reversible: yes
- available_commands_update / unrecognized types keep being silently dropped at debug level (FR-006), no new logic needed - process_update() already returns None for them | rationale: existing behavior, spec requires preservation | reversible: n/a (preserving existing)
- Text-delta granularity: forward each raw ACP chunk as ONE delta, no re-splitting into words (Option A, user-approved) | rationale: matches spec's literal "real-time" definition (Assumptions section); simplest; zero added latency | reversible: yes (Option B word-splitting could be layered on later)
- Test strategy: TDD (write failing mock-agent-delay + streaming integration tests first, against current batch behavior, then implement) | rationale: default per skill protocol, not overridden by user

## Findings (cited - path:lines)
- `AcpClient::send_prompt_streaming()` already exists and returns `impl Stream<Item = SessionUpdate>`, unused by any route handler — crates/rosetta-acp/src/client.rs:91-130
- Both route handlers call blocking `send_prompt()` + `read_updates()`, then fake-stream the finalized `Response` — crates/rosetta-server/src/routes.rs:104-203
- `ResponseAccumulator::process_update()` already returns `Option<ResponseEvent>` per single `SessionUpdate` but is called in a loop over a pre-collected Vec, discarding the per-update event — crates/rosetta-core/src/translate.rs:92-266
- Dead-code helper `streaming::response_event_stream_to_sse()` already converts a live `Stream<ResponseEvent>` to SSE, currently `#[allow(dead_code)]` and unused — crates/rosetta-server/src/streaming.rs:34-46
- No live per-update Chat Completions chunk translator exists; `response_to_chat_chunks` only works on a finalized `Response` — crates/rosetta-core/src/translate.rs:517-579
- Mock agent has no configurable inter-chunk delay — crates/rosetta-acp/tests/fixtures/mock_acp.py
- `AcpClient` methods take `&mut self`; `close_session`/`shutdown` need ownership — disconnect/cleanup requires a task-based ownership model (spawn task owning AcpClient, forward over channel, abort on drop) — crates/rosetta-acp/src/client.rs:215-217,247-262
- Existing tests: `test_response_to_chat_chunks_empty_text`, `test_response_to_chat_chunks_splits_text` cover old batch translator (translate.rs:983-1055) — must not regress non-streaming/batch paths

## Decisions (with rationale)
- Reuse `process_update()` unmodified for Responses API live streaming (it already returns per-update `ResponseEvent`s) — avoids duplicating translation logic.
- Introduce `ResponseAccumulator::process_update_chat()` (or equivalent) mirroring `process_update()`'s match arms but emitting `ChatCompletionChunk` deltas — keeps both translators symmetric and testable in isolation.
- Own `AcpClient` inside a spawned `tokio::task`; communicate via `tokio::sync::mpsc::channel::<SessionUpdate>(N)` (bounded, e.g. 32); task calls `close_session` + drops client on completion, error, or channel-receiver-drop (client disconnect detected via `Sender::send` returning Err, or via a `CancellationToken`/`select!` on the Sse stream's drop).
- On agent error mid-turn: Responses API emits `ResponseEvent::Error` (already exists) then ends the stream; Chat Completions ends the stream without a synthetic finish chunk (matches spec's "an error event, or the request simply ending").
- Non-streaming code paths (`is_streaming == false`) remain byte-for-byte unchanged: still `send_prompt()` + `read_updates()` + `finalize()`.

## Scope IN
- Responses API (`/v1/responses`, `stream: true`) real-time incremental delivery
- Chat Completions API (`/v1/chat/completions`, `stream: true`) real-time incremental delivery
- Bounded backpressure buffering
- Clean completion signal (role/finish_reason/usage) matching today's shape
- Error/disconnect handling with resource cleanup within 5s
- Concurrent streaming request isolation (>=50 concurrent, per SC-005)
- Mock agent per-chunk delay support for testability
- Integration + unit tests for all of the above

## Scope OUT (Must NOT have)
- No feature flag / opt-in toggle for true streaming (replaces existing streaming path outright)
- No changes to non-streaming (`stream: false`) request handling
- No new ACP update types recognized (still only agent_thought_chunk, agent_message_chunk, tool_call)
- No token usage tracking implementation (tracked separately in roadmap, unchanged — still hard-coded zero)
- No tool execution loop implementation (separate roadmap item)
- No artificial word re-splitting of raw agent chunks (Option A chosen over Option B)
- No admission-control / concurrency cap (no requirement to reject requests beyond SC-005's 50-concurrent sustain target)

## Open questions
(none - text-delta granularity and test strategy resolved via user approval of recommended defaults)

## Metis gap-analysis findings (resolved during planning, folded into plan)
- **Child-process cleanup on drop is NOT automatic** — confirmed via `crates/rosetta-acp/src/transport.rs:10-14,99-107`: `AcpTransport` wraps a `tokio::process::Child` with no `Drop` impl; `tokio::process::Child` does NOT kill its child on drop unless `kill_on_drop(true)` was set on `Command` (not set here, `transport.rs:30-34`). Only the explicit `AcpTransport::shutdown()` (→ `child.start_kill()` + `wait()`) terminates the process. **Resolution**: the streaming task (Todo 2/8) MUST explicitly call `client.shutdown()` on every exit path (normal completion, error, disconnect) — never rely on drop alone — or an orphaned agent process will violate SC-004's 5s budget.
- **`send_prompt_streaming()` discards the final `PromptResponse` and cannot distinguish normal-completion from disconnect** — confirmed via `crates/rosetta-acp/src/client.rs:106-129`: line 123 parses-and-discards the `PromptResponse` (`let _ = self.try_parse_response(...)`), and both the "got PromptResponse" path (line 122-124) and the "transport disconnected" path (line 114, `Ok(None) => return`) simply end the stream identically — there is no way for a consumer to tell normal completion from a mid-turn disconnect from the stream alone. **Resolution**: `send_prompt_streaming` must be modified (zero known callers today, safe to change) to yield a small enum distinguishing `Update(SessionUpdate)` from terminal `Completed` vs `Disconnected`, so the streaming task (Todo 2) can emit the correct `StreamOutcome` variant. `PromptResponse.usage` itself remains unparsed/discarded (matches the spec's explicit assumption that usage stays hard-coded — no scope creep here).
- TDD claim reconciled with wave order: unit-level TDD for Todos 1/2/3/5 (tests written alongside/before their implementation); integration-level tests-after for Wave 3 validating Wave 2's handler rewiring, with Todo 14 adding a meta-QA check proving the new tests would actually catch a regression to the old batch behavior.
- Mock-agent extensions consolidated into Todo 1 (delay + crash-after-N + per-request echo, single ownership/commit) instead of being scattered/implied across Todos 12-13.
- Added quantitative SC-002/SC-005 assertions, FR-006 debug-logging + live-Responses-path drop test, FR-008 baseline-diff test, end-to-end slow-HTTP-client backpressure test, a new long-running-turn/timeout todo, de-humanized + agent-executable Final verification wave, and forbade refactoring the existing batch `response_to_chat_chunks` (build `chat_final_chunk` standalone instead) to protect the Must-NOT-Have guardrail.

## Approval gate
status: plan-written
<!-- User approved recommended defaults (Option A raw-chunk forwarding, TDD test strategy) in prior turn. Plan written to .omo/plans/001-true-streaming.md with 15 todos + Metis gap-analysis findings folded in. Awaiting user's start-work-or-high-accuracy-review decision. -->

## Spec-kit format artifacts (delegated, per /speckit.plan request)
- Plan-agent scope restricts direct edits to .omo/*.md; the spec-kit-format artifacts below were written by a delegated worker task, content authored by Prometheus in this session, sourced from this draft + .omo/plans/001-true-streaming.md.
- specs/001-true-streaming/plan.md — Technical Context, Constitution Check (all PASS, 1 doc-sync follow-up noted), Project Structure — written and verified.
- specs/001-true-streaming/research.md — Phase 0, 6 decisions with rationale/alternatives (reuse send_prompt_streaming via bounded channel; AcpStreamItem completion/disconnect distinction; explicit shutdown() over Drop; raw-chunk forwarding; no feature flag; concurrency/backpressure test-scope limitation) — written.
- specs/001-true-streaming/data-model.md — Streaming Session, AcpStreamItem, StreamOutcome, Response Chunk, Completion Signal entities + validation rules + state transitions — written.
- specs/001-true-streaming/contracts/responses-sse-events.md, chat-completions-sse-chunks.md — written.
- specs/001-true-streaming/quickstart.md — 4 manual validation scenarios — written.
- All verified present via glob after delegated write.
