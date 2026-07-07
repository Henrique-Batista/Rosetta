# Rosetta — OpenAI-to-ACP Proxy

Rosetta is a Rust-based HTTP proxy that translates between OpenAI's **Responses API** / **Chat Completions API** and the **Agent Client Protocol (ACP)**. It spawns an ACP-compatible agent (e.g. `opencode acp`) via stdio JSON-RPC 2.0 and exposes standard OpenAI-compatible HTTP endpoints.

## Quick Start

### 1. Build

```bash
cargo build --release
```

The server binary is produced at `target/release/rosetta`.

### 2. Run with a real ACP agent (OpenCode)

```bash
ROSETTA_ACP_COMMAND=opencode \
ROSETTA_ACP_ARGS="acp" \
./target/release/rosetta
```

The server listens on `0.0.0.0:3000`.

### Model & Agent Selection

Rosetta supports selecting both the **LLM model** and the **agent mode** using the `model` field.

**Syntax:** `model:agent` (e.g. `opencode/gpt-5:sisyphus`)
- The part **before** `:` selects the LLM model
- The part **after** `:` selects the agent/mode (optional)

**Example — using a specific model:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5",
    "input": [
      {"type": "message", "role": "user", "content": "Hello"}
    ]
  }'
```

**Example — using a specific model + agent:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5:sisyphus",
    "input": [
      {"type": "message", "role": "user", "content": "Build a web server"}
    ]
  }'
```

**Example — using a coding agent:**

```bash
curl http://localhost:3000/v1/chat/completions \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/claude-sonnet-4-5:hephaestus",
    "messages": [
      {"role": "user", "content": "Review this code"}
    ]
  }'
```

Available models and agents depend on your ACP agent configuration. Common prefixes:
- `opencode/` — OpenCode Zen agents (e.g. `opencode/gpt-5`, `opencode/claude-sonnet-4-5`)
- `opencode-go/` — OpenCode Go agents (e.g. `opencode-go/kimi-k2.6`)
- `openrouter/` — OpenRouter models (e.g. `openrouter/anthropic/claude-opus-4`)
- `google/` — Google models (e.g. `google/gemini-2.5-pro`)
- `groq/` — Groq models

**How it works:**
1. Rosetta spawns the ACP agent **without injecting any agent-specific config overrides** — the agent uses its own configuration (config files, env vars, etc.)
2. Rosetta parses the `model` field to extract model and agent (e.g., `opencode/gpt-5:sisyphus`)
3. After `session/new`, Rosetta inspects `configOptions` from the ACP response
4. If a `category: "mode"` option matches the requested agent, it calls `session/set_config_option`
5. MCP servers can be passed to the agent via the `ROSETTA_MCP_SERVERS` environment variable
6. This is **fully ACP-agnostic** — any ACP agent works without Rosetta assuming anything about its internal configuration

### 3. Test with curl

**Responses API:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "input": [
      {"type": "message", "role": "user", "content": "Hello"}
    ]
  }'
```

**Chat Completions API:**

```bash
curl http://localhost:3000/v1/chat/completions \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `ROSETTA_ACP_COMMAND` | Command to spawn the ACP agent | *(required)* |
| `ROSETTA_ACP_ARGS` | Arguments for the ACP agent (space-separated) | *(none)* |
| `ROSETTA_CWD` | Working directory passed to the agent on `session/new` | current working directory |
| `ROSETTA_MCP_SERVERS` | JSON array of MCP server configurations passed to the agent via `session/new` | *(none)* |

**ACP agent configuration:** Rosetta does **not** inject any agent-specific config overrides (e.g., `OPENCODE_CONFIG`). The ACP agent process reads its own configuration naturally — config files, environment variables, or built-in defaults. This ensures Rosetta works with **any** ACP agent without assumptions.

## Running with the Mock Agent (for testing)

A Python mock agent is included for integration testing:

```bash
ROSETTA_ACP_COMMAND=python3 \
ROSETTA_ACP_ARGS="crates/rosetta-acp/tests/fixtures/mock_acp.py" \
./target/release/rosetta
```

**Note:** When using the mock agent, the `model` field is ignored. The mock agent always returns a fixed response.

## Architecture

```
┌─────────────┐      HTTP/JSON       ┌──────────────┐      stdio/NDJSON      ┌─────────────┐
│   Client    │  ──────────────────> │   Rosetta    │  ───────────────────>  │  ACP Agent  │
│  (OpenAI    │   /v1/responses      │   Server     │   JSON-RPC 2.0        │ (opencode   │
│   SDK)      │   /v1/chat/completions│  (Axum)     │   initialize          │   acp)      │
└─────────────┘                      └──────────────┘   session/new           └─────────────┘
                                                      session/prompt
                                                      session/update (streaming)
                                                      session/close
```

## Project Layout

| Crate | Responsibility |
|-------|--------------|
| `rosetta-types` | OpenAI and ACP request/response types |
| `rosetta-acp` | JSON-RPC 2.0 client + stdio transport |
| `rosetta-core` | Translation layer between OpenAI and ACP |
| `rosetta-server` | Axum HTTP server + route handlers |

## Response Structure

Rosetta translates ACP agent updates into OpenAI-compatible response structures:

| ACP Update Type | OpenAI Output | Description |
|----------------|---------------|-------------|
| `agent_thought_chunk` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `thinking`) | Model's internal reasoning/thinking |
| `agent_message_chunk` | `OutputItem::Message` (type: `message`) | Final user-facing output text |
| `tool_call` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `tool_call`) | Agent's tool invocation (exposed as reasoning, not function call) |
| `available_commands_update` | *(silently dropped — logged at debug level)* | Agent announcing available commands/skills |
| other types | *(silently dropped — logged at debug level)* | Unhandled update types |

The `output_text` field in the response contains **only message text** (no thinking/reasoning text).

## Debugging

Rosetta uses structured logging via the `tracing` crate. Set `RUST_LOG` to control verbosity:

```bash
# Show only tool/skill invocations
RUST_LOG=rosetta_core=info ./target/release/rosetta

# Show all update types (including silently dropped ones)
RUST_LOG=rosetta_core=debug ./target/release/rosetta

# Show full JSON of every ACP session update
RUST_LOG=rosetta_core=trace ./target/release/rosetta
```

### Log Levels

| Level | What you see | Use case |
|-------|-------------|----------|
| `info` | `ACP tool_call received — agent invoked a tool/skill` | Confirm a skill/tool was called |
| `debug` | `agent_thought_chunk received`, `Unhandled ACP session update type` | See what update types the agent sends |
| `trace` | Full JSON body of each ACP update | Debug raw ACP protocol communication |

## ACP Compatibility

Rosetta is built on top of the **Agent Client Protocol (ACP)**, which is defined de facto by the opencode ACP implementation. Below is a compatibility assessment for potential ACP agents beyond opencode.

### Protocol Layer

| Layer | Status | Details |
|-------|--------|---------|
| **Transport** | 🟢 ACP-compliant | Newline-delimited JSON over stdio. Standard for ACP. |
| **Initialize** | 🟢 ACP-compliant | `initialize` with `protocolVersion` — generic JSON-RPC 2.0. The `serverInfo` field accepts `agentInfo` alias for backward compat. |
| **Session lifecycle** | 🟢 ACP-compliant | `session/new` → `session/prompt` → `session/close`. Standard flow. |
| **MCP servers** | 🟢 ACP-compliant | Passed via standard `mcpServers` field in `session/new`. |
| `session/set_config_option` | 🟡 opencode-aligned | This method is defined in the ACP spec but primarily implemented by opencode. Other agents may not support it. Rosetta gracefully handles absence (log message, no crash). |

### Update Format

| Aspect | Status | Details |
|--------|--------|---------|
| **Update type location** | 🟡 opencode-aligned | Rosetta checks TWO locations: `body.updateType` (flat format) and `body.update.sessionUpdate` (nested format). An agent using a third format would have all updates silently dropped. |
| **Data payload location** | 🟡 opencode-aligned | Rosetta checks `body.data` and `body.update`. Same dual-format approach as above. |
| **Update type names** | 🔴 opencode-specific | Only `agent_thought_chunk`, `agent_message_chunk`, and `tool_call` are recognized. All other update types (e.g., `agent_message`, `tool_call_update`, `user_message_chunk`, `plan`, `current_mode_update`) are silently dropped — logged at `debug` level. |
| **Content field structure** | 🟡 opencode-aligned | Extracts text from `content.type=="text" && content.text` (nested) or `content`/`text` as plain string (flat). |
| **Tool call fields** | 🔴 opencode-specific | Expects `toolCallId`, `title`, `name`, `arguments` (and fallback `params`). Other agents may use different field names. |

### Content & Prompt

| Aspect | Status | Details |
|--------|--------|---------|
| **OpenAI message → ACP prompt** | 🟡 opencode-aligned | Prefixes messages with `[System]\n`, `[Assistant]\n`, `[Tool Result]\n` — these are opencode conventions. Other ACP agents may not understand these markers. |
| **Content types** | 🟡 ACP-compliant | Only `ContentBlock::Text` is generated. `InputImage` and `InputFile` content parts are silently dropped. |
| **Chat message order** | 🟡 ACP-compliant | Messages are translated in order with role prefixes. Standard behavior. |

### Configuration

| Aspect | Status | Details |
|--------|--------|---------|
| **Agent config injection** | 🟢 ACP-compliant | Rosetta does NOT inject any agent-specific config (e.g., `OPENCODE_CONFIG`). The agent uses its own configuration naturally. |
| **Model/Agent selection** | 🟡 opencode-aligned | The `model:agent` syntax (e.g., `opencode/gpt-5:sisyphus`) is parsed from the OpenAI `model` field. After `session/new`, Rosetta inspects `configOptions` and calls `session/set_config_option` if a matching `mode` option is found. Agents without configOptions will simply use their default. |
| **Environment variables** | 🟢 ACP-compliant | Uses `ROSETTA_*` prefixed env vars. No agent-specific env vars are injected. |

### Missing Features

| Feature | Impact | Details |
|---------|--------|---------|
| **Tool execution loop** | 🔴 opencode-specific | When the agent makes a `tool_call`, Rosetta converts it to a `Reasoning` output item. There is no loop to execute the tool and send results back to the agent. This means tool-dependent workflows (e.g., web search, file operations) won't complete. |
| **Multi-modal content** | 🟡 ACP-compliant | `InputImage` and `InputFile` are dropped. Only `InputText` is forwarded. An agent expecting images or files will not receive them. |
| **Token usage reporting** | 🟡 ACP-compliant | Currently hard-coded to zero. The ACP agent's `PromptResponse.usage` is available but not yet parsed. |

### Summary

| Level | Definition | Coverage |
|-------|-----------|----------|
| 🟢 **ACP-compliant** | Works with any ACP agent respecting the protocol | Transport, init, session lifecycle, MCP servers, env vars |
| 🟡 **opencode-aligned** | Tested with opencode; likely works with others with minor adjustments | Update format, content structure, config options |
| 🔴 **opencode-specific** | Only works with opencode | Update type names, tool call fields, tool execution loop |

**Bottom line:** A generic ACP agent that implements the basic protocol (initialize → session/new → session/prompt → session/update → session/close) will work for basic text conversations. Features like tool execution, multi-modal input, and specific update type handling are opencode-specific and would require adaptation.

## Important Notes

- **Runtime parameters** (`temperature`, `top_p`, etc.) are ignored per ACP spec — they are not forwarded to the agent.
- **Streaming**: Rosetta supports two streaming paths:
  - Responses API: uses `response_to_streaming_events()` to generate proper SSE events from the accumulated response
  - Chat Completions: uses `response_to_chat_chunks()` to split text into word-by-word delta chunks with proper `role`/`finish_reason`/`usage` framing
  - A true streaming method (`send_prompt_streaming()`) is available in `AcpClient` via `async_stream` for real-time ACP update processing
- **MCP servers** are passed through the ACP-standard `mcpServers` field in `session/new` — configure via `ROSETTA_MCP_SERVERS` env var
- The `InputItem` enum requires `"type": "message"` in the input array.
- ACP field names use `camelCase` (e.g. `protocolVersion`, `sessionId`).
- `Client` input parts that are not `input_text` (e.g. `input_file`, `input_image`) are silently dropped during prompt translation.

## Roadmap

### Known Limitations & Future Work

| Item | Description | Status |
|------|-------------|--------|
| **Skill trigger evaluation in ACP mode** | Skills from `~/.opencode/skills/` are loaded and announced via `available_commands_update`, but the ACP agent does not evaluate SKILL.md trigger conditions automatically. In CLI mode, opencode checks triggers before building the LLM prompt. In ACP mode, the trigger logic is not executed. This needs to be implemented in the ACP agent (opencode), not in Rosetta. | 🔜 Future (opencode-side) |
| **Input file/image support** | `InputFile` and `InputImage` content parts in the OpenAI request are dropped during prompt translation. Only `InputText` parts are forwarded to the ACP agent. | 📋 Planned |
| **True streaming for Responses API** | The current SSE path collects all updates first, then generates events from the finalized response. A true streaming path using `send_prompt_streaming()` exists in `AcpClient` but is not yet wired into the HTTP route handler (requires channel-based architecture). | 📋 Planned |
| **Token usage tracking** | Current usage is hard-coded to `{input_tokens: 0, output_tokens: 0, total_tokens: 0}`. The ACP agent's `PromptResponse.usage` is available but not yet parsed. | 📋 Planned |
| **Tool call execution loop** | When the agent makes a `tool_call`, Rosetta converts it to a `Reasoning` output item. There is no loop to execute the tool and send results back to the agent. | 🔜 Future |

## Development

### Run all tests

```bash
cargo test --workspace
```

### Run only unit tests

```bash
cargo test -p rosetta-core
```

### Run integration test with mock agent

```bash
cargo test -p rosetta-acp --test integration_test
```

### Run with debug logging

```bash
RUST_LOG=rosetta_core=debug cargo run
```

## License

MIT
