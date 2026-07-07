🇺🇸 English | 🇧🇷 [Português](README.pt-BR.md)

# Rosetta — OpenAI-to-ACP Proxy

![Rosetta](assets/Rosetta%201%20(English).excalidraw.png)
![Rosetta](assets/Rosetta%202%20(English).excalidraw.png)

Rosetta is an HTTP proxy written in Rust that translates between OpenAI's **Responses API** / **Chat Completions API** and the **Agent Client Protocol (ACP)**. It spawns an ACP-compatible agent (e.g. `opencode acp`) via stdio JSON-RPC 2.0 and exposes OpenAI-compatible HTTP endpoints.

## Table of Contents

- [Installation](#installation)
- [Configuration](#configuration) — CLI flags and environment variables, with precedence
- [Model & Agent Selection](#model--agent-selection)
- [Testing with curl](#testing-with-curl)
- [Running with the Mock Agent](#running-with-the-mock-agent-for-testing)
- [Architecture](#architecture)
- [Project Layout](#project-layout)
- [Response Format](#response-format)
- [Debugging](#debugging)
- [ACP Compatibility](#acp-compatibility)
- [Important Notes](#important-notes)(
- [Roadmap](#roadmap)
- [Development](#development)

## Installation

### Build

```bash
cargo build --release
```

The server binary is produced at `target/release/rosetta`.

## Configuration

Rosetta can be configured in **two ways**, which can be freely combined:

1. **Command-line flags** (`--acp-command`, `--acp-arg`, `--cwd`, `--mcp-servers`, `--listen`)
2. **Environment variables** (`ROSETTA_ACP_COMMAND`, `ROSETTA_ACP_ARGS`, `ROSETTA_CWD`, `ROSETTA_MCP_SERVERS`, `ROSETTA_LISTEN`)

### Precedence (highest to lowest)

```
1st  CLI flag             (--acp-command, --acp-arg, --cwd, --mcp-servers, --listen)
2nd  Environment variable  (ROSETTA_ACP_COMMAND, ROSETTA_ACP_ARGS, ROSETTA_CWD, ROSETTA_MCP_SERVERS, ROSETTA_LISTEN)
3rd  Built-in default value
```

**The CLI always wins.** If a flag is passed explicitly on the command line, the corresponding environment variable's value is ignored, even if both are set at the same time.

### Flag Reference

| CLI Flag | Environment Variable | Default | Description |
|----------|----------------------|---------|--------------|
| `-c, --acp-command <COMMAND>` | `ROSETTA_ACP_COMMAND` | `opencode` | Command used to launch the ACP agent |
| `-a, --acp-arg <ARG>` (repeatable) | `ROSETTA_ACP_ARGS` | `acp` | Argument passed to the ACP agent. Can be repeated (`--acp-arg foo --acp-arg bar`) or provided as a space-separated string |
| `-w, --cwd <PATH>` | `ROSETTA_CWD` | the process's current working directory | Working directory sent to the agent in `session/new` |
| `-m, --mcp-servers <JSON>` | `ROSETTA_MCP_SERVERS` | `[]` (none) | JSON array of MCP server configurations, passed through via `session/new`. Invalid JSON aborts the process with a clear error |
| `-l, --listen <HOST:PORT>` | `ROSETTA_LISTEN` | `0.0.0.0:3000` | Address/port the HTTP server listens on |

See all options and the built-in documentation:

```bash
./target/release/rosetta --help
```

### Example — environment variables only (backward compatible)

```bash
ROSETTA_ACP_COMMAND=opencode \
ROSETTA_ACP_ARGS="acp" \
./target/release/rosetta
```

### Example — CLI flags only

```bash
./target/release/rosetta \
  --acp-command opencode \
  --acp-arg acp \
  --listen 0.0.0.0:3000
```

### Example — multiple agent arguments via CLI

```bash
./target/release/rosetta \
  --acp-command opencode \
  --acp-arg acp \
  --acp-arg --verbose
```

### Example — MCP servers via CLI

```bash
./target/release/rosetta \
  --mcp-servers '[{"name":"fs","command":"mcp-fs"}]'
```

### Example — CLI overriding environment variables

```bash
# ROSETTA_ACP_COMMAND=python3 is set in the environment,
# but --acp-command opencode on the CLI takes precedence and wins.
ROSETTA_ACP_COMMAND=python3 \
./target/release/rosetta --acp-command opencode --acp-arg acp
# Result: the agent started is "opencode acp", not "python3"
```

## Model & Agent Selection

Rosetta lets you select both the **LLM model** and the **agent mode** using the request's `model` field.

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

Available models and agents depend on your ACP agent's configuration. Common prefixes:
- `opencode/` — OpenCode Zen agents (e.g. `opencode/gpt-5`, `opencode/claude-sonnet-4-5`)
- `opencode-go/` — OpenCode Go agents (e.g. `opencode-go/kimi-k2.6`)
- `openrouter/` — OpenRouter models (e.g. `openrouter/anthropic/claude-opus-4`)
- `google/` — Google models (e.g. `google/gemini-2.5-pro`)
- `groq/` — Groq models

**How it works:**
1. Rosetta spawns the ACP agent **without injecting any agent-specific configuration** — the agent uses its own configuration (config files, environment variables, etc.)
2. Rosetta parses the `model` field to extract the model and agent (e.g., `opencode/gpt-5:sisyphus`)
3. After `session/new`, Rosetta inspects `configOptions` in the ACP response
4. If a `category: "mode"` option matches the requested agent, Rosetta calls `session/set_config_option`
5. MCP servers can be passed to the agent via the `--mcp-servers` flag / `ROSETTA_MCP_SERVERS` variable
6. This is **fully ACP-agnostic** — any ACP agent works without Rosetta assuming anything about its internal configuration

## Testing with curl

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

## Running with the Mock Agent (for Testing)

A Python mock agent is included for integration testing:

```bash
./target/release/rosetta \
  --acp-command python3 \
  --acp-arg crates/rosetta-acp/tests/fixtures/mock_acp.py
```

**Note:** when using the mock agent, the `model` field is ignored. The mock agent always returns a fixed response.

## Architecture

```
┌─────────────┐      HTTP/JSON       ┌──────────────┐      stdio/NDJSON      ┌─────────────┐
│   Client    │  ──────────────────> │   Rosetta    │  ───────────────────>  │  ACP Agent  │
│  (OpenAI    │   /v1/responses      │   Server     │   JSON-RPC 2.0        │ (opencode   │
│    SDK)     │   /v1/chat/completions│  (Axum)     │   initialize          │   acp)      │
└─────────────┘                      └──────────────┘   session/new           └─────────────┘
                                                      session/prompt
                                                      session/update (streaming)
                                                      session/close
```

Configuration (CLI + env) is resolved once in `main()` (`crates/rosetta-server/src/cli.rs`) before the HTTP server comes up, and the result (`ResolvedConfig`) feeds the shared `AppState` that each request uses to spawn an ACP client.

## Project Layout

| Crate | Responsibility |
|-------|--------------|
| `rosetta-types` | OpenAI and ACP request/response types |
| `rosetta-acp` | JSON-RPC 2.0 client + stdio transport |
| `rosetta-core` | Translation layer between OpenAI and ACP |
| `rosetta-server` | Axum HTTP server + CLI (`clap`) + route handlers |

## Response Format

Rosetta translates ACP agent updates into OpenAI-compatible response structures:

| ACP Update Type | OpenAI Output | Description |
|----------------|---------------|-------------|
| `agent_thought_chunk` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `thinking`) | Model's internal reasoning |
| `agent_message_chunk` | `OutputItem::Message` (type: `message`) | Final user-facing text |
| `tool_call` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `tool_call`) | Agent's tool invocation (exposed as reasoning, not as a function call) |
| `available_commands_update` | *(silently dropped — logged at debug level)* | Agent announcing available commands/skills |
| other types | *(silently dropped — logged at debug level)* | Unhandled update types |

The `output_text` field in the response contains **only message text** (no reasoning/thinking text).

## Debugging

Rosetta uses structured logging via the `tracing` crate. Set `RUST_LOG` to control verbosity:

```bash
# Show only tool/skill invocations
RUST_LOG=rosetta_core=info ./target/release/rosetta

# Show all update types (including the dropped ones)
RUST_LOG=rosetta_core=debug ./target/release/rosetta

# Show the full JSON of every ACP session update
RUST_LOG=rosetta_core=trace ./target/release/rosetta
```

### Log Levels

| Level | What you see | Use case |
|-------|-------------|----------|
| `info` | `ACP tool_call received — agent invoked a tool/skill` | Confirm that a skill/tool was called |
| `debug` | `agent_thought_chunk received`, `Unhandled ACP session update type` | See which update types the agent sends |
| `trace` | Full JSON body of every ACP update | Debug raw ACP protocol communication |

## ACP Compatibility

Rosetta is built on top of the **Agent Client Protocol (ACP)**, which is defined de facto by opencode's ACP implementation. Below is a compatibility assessment for other ACP agents besides opencode.

### Protocol Layer

| Layer | Status | Details |
|-------|--------|---------|
| **Transport** | 🟢 ACP-compliant | Newline-delimited JSON over stdio. Standard for ACP. |
| **Initialize** | 🟢 ACP-compliant | `initialize` with `protocolVersion` — generic JSON-RPC 2.0. The `serverInfo` field accepts the `agentInfo` alias for backward compatibility. |
| **Session lifecycle** | 🟢 ACP-compliant | `session/new` → `session/prompt` → `session/close`. Standard flow. |
| **MCP servers** | 🟢 ACP-compliant | Passed via the standard `mcpServers` field in `session/new`. |
| `session/set_config_option` | 🟡 opencode-aligned | This method is defined in the ACP spec but primarily implemented by opencode. Other agents may not support it. Rosetta gracefully handles its absence (logs a message, no crash). |

### Update Format

| Aspect | Status | Details |
|--------|--------|---------|
| **Update type location** | 🟡 opencode-aligned | Rosetta checks TWO locations: `body.updateType` (flat format) and `body.update.sessionUpdate` (nested format). An agent using a third format would have all its updates silently dropped. |
| **Data payload location** | 🟡 opencode-aligned | Rosetta checks `body.data` and `body.update`. Same dual-format approach as above. |
| **Update type names** | 🔴 opencode-specific | Only `agent_thought_chunk`, `agent_message_chunk`, and `tool_call` are recognized. Other types (e.g., `agent_message`, `tool_call_update`, `user_message_chunk`, `plan`, `current_mode_update`) are silently dropped — logged at debug level. |
| **Content field structure** | 🟡 opencode-aligned | Extracts text from `content.type=="text" && content.text` (nested) or `content`/`text` as a plain string (flat). |
| **Tool call fields** | 🔴 opencode-specific | Expects `toolCallId`, `title`, `name`, `arguments` (with `params` as a fallback). Other agents may use different field names. |

### Content & Prompt

| Aspect | Status | Details |
|--------|--------|---------|
| **OpenAI message → ACP prompt** | 🟡 opencode-aligned | Prefixes messages with `[System]\n`, `[Assistant]\n`, `[Tool Result]\n` — opencode conventions. Other ACP agents may not understand these markers. |
| **Content types** | 🟡 ACP-compliant | Only `ContentBlock::Text` is generated. `InputImage` and `InputFile` content parts are silently dropped. |
| **Chat message order** | 🟡 ACP-compliant | Messages are translated in order with role prefixes. Standard behavior. |

### Configuration

| Aspect | Status | Details |
|--------|--------|---------|
| **Agent config injection** | 🟢 ACP-compliant | Rosetta does NOT inject any agent-specific configuration (e.g., `OPENCODE_CONFIG`). The agent uses its own configuration naturally. |
| **Model/agent selection** | 🟡 opencode-aligned | The `model:agent` syntax (e.g., `opencode/gpt-5:sisyphus`) is parsed from OpenAI's `model` field. After `session/new`, Rosetta inspects `configOptions` and calls `session/set_config_option` if a matching `mode` option is found. Agents without `configOptions` simply use their default. |
| **Environment variables** | 🟢 ACP-compliant | Uses `ROSETTA_*`-prefixed variables. No agent-specific variables are injected. |

### Missing Features

| Feature | Impact | Details |
|---------|--------|---------|
| **Tool execution loop** | 🔴 opencode-specific | When the agent makes a `tool_call`, Rosetta converts it into a `Reasoning` output item. There is no loop to execute the tool and send the results back to the agent. This means tool-dependent workflows (e.g., web search, file operations) won't complete. |
| **Multi-modal content** | 🟡 ACP-compliant | `InputImage` and `InputFile` are dropped. Only `InputText` is forwarded. An agent expecting images or files will not receive them. |
| **Token usage reporting** | 🟡 ACP-compliant | Currently hard-coded to zero. The ACP agent's `PromptResponse.usage` field is available but not yet parsed. |

### Summary

| Level | Definition | Coverage |
|-------|-----------|----------|
| 🟢 **ACP-compliant** | Works with any ACP agent that respects the protocol | Transport, init, session lifecycle, MCP servers, environment variables |
| 🟡 **opencode-aligned** | Tested with opencode; likely works with others with minor adjustments | Update format, content structure, configuration options |
| 🔴 **opencode-specific** | Only works with opencode | Update type names, tool call fields, tool execution loop |

**Bottom line:** a generic ACP agent that implements the basic protocol (initialize → session/new → session/prompt → session/update → session/close) will work for basic text conversations. Features like tool execution, multi-modal input, and specific update type handling are opencode-specific and would require adaptation.

## Important Notes

- **Runtime parameters** (`temperature`, `top_p`, etc.) are ignored per the ACP spec — they are not forwarded to the agent.
- **Streaming**: Rosetta supports two streaming paths:
  - Responses API: uses `response_to_streaming_events()` to generate proper SSE events from the accumulated response
  - Chat Completions: uses `response_to_chat_chunks()` to split the text into word-by-word delta chunks, with proper `role`/`finish_reason`/`usage` framing
  - A true streaming method (`send_prompt_streaming()`) is available on `AcpClient` via `async_stream` for real-time ACP update processing
- **MCP servers** are passed through the ACP-standard `mcpServers` field in `session/new` — configure via the `--mcp-servers` flag or the `ROSETTA_MCP_SERVERS` variable
- The `InputItem` enum requires `"type": "message"` in the input array.
- ACP field names use `camelCase` (e.g., `protocolVersion`, `sessionId`).
- `Client` input parts that aren't `input_text` (e.g., `input_file`, `input_image`) are silently dropped during prompt translation.

## Roadmap

### Known Limitations & Future Work

| Item | Description | Status |
|------|-------------|--------|
| **Skill trigger evaluation in ACP mode** | Skills from `~/.opencode/skills/` are loaded and announced via `available_commands_update`, but the ACP agent doesn't automatically evaluate SKILL.md trigger conditions. In CLI mode, opencode checks triggers before building the LLM prompt. In ACP mode, that logic isn't executed. This needs to be implemented on the ACP agent side (opencode), not in Rosetta. | 🔜 Future (opencode-side) |
| **Input file/image support** | `InputFile` and `InputImage` content parts in the OpenAI request are dropped during prompt translation. Only `InputText` parts are forwarded to the ACP agent. | 📋 Planned |
| **True streaming for the Responses API** | The current SSE path collects all updates first, then generates events from the finalized response. A true streaming path using `send_prompt_streaming()` exists on `AcpClient` but isn't yet wired into the HTTP route handler (requires a channel-based architecture). | 📋 Planned |
| **Token usage tracking** | Current usage is hard-coded to `{input_tokens: 0, output_tokens: 0, total_tokens: 0}`. The ACP agent's `PromptResponse.usage` field is available but not yet parsed. | 📋 Planned |
| **Tool call execution loop** | When the agent makes a `tool_call`, Rosetta converts it into a `Reasoning` output item. There is no loop to execute the tool and send the results back to the agent. | 🔜 Future |

## Development

### Run all tests

```bash
cargo test --workspace
```

### Run only the unit tests

```bash
cargo test -p rosetta-core
```

### Run the CLI tests (`rosetta-server`)

```bash
cargo test -p rosetta-server
```

### Run the integration test with the mock agent

```bash
cargo test -p rosetta-acp --test integration_test
```

### Run with debug logging

```bash
RUST_LOG=rosetta_core=debug cargo run
```

## License

MIT
</content>
</invoke>
