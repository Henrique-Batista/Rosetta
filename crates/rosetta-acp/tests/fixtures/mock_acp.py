#!/usr/bin/env python3
"""Mock ACP agent that speaks JSON-RPC 2.0 over stdio for integration testing.

Emits a realistic sequence of updates:
1. agent_thought_chunk — initial thinking
2. tool_call — agent decides to call a tool
3. agent_thought_chunk — post-tool reasoning
4. agent_message_chunk — first output text chunk
5. agent_message_chunk — second output text chunk
6. Final prompt response

Temporal delays simulate real agent streaming behavior.

Env vars (all optional, default behavior unchanged when unset):
- MOCK_ACP_CHUNK_DELAY_MS: sleep this many ms between each emitted chunk
  notification (default 0 = no delay, current fixed-delay behavior kept).
  Non-numeric values fall back to 0 rather than crashing.
- MOCK_ACP_CRASH_AFTER_CHUNKS=N: after emitting N chunk notifications for a
  given session/prompt request, close stdout / exit without sending the
  final PromptResponse line, simulating an agent crash mid-turn.
- MOCK_ACP_ECHO=1: include a distinguishing substring from the incoming
  prompt text in the emitted agent_message_chunk content, so concurrent
  requests can be told apart by their content.
- MOCK_ACP_TOOL_NAME: override the `name` field of the emitted tool_call
  chunk (default "get_weather"). Allows integration tests to advertise
  arbitrary tools.
- MOCK_ACP_TOOL_ARGS: override the `arguments` field of the emitted
  tool_call chunk (default '{"city": "Paris"}'). Must be a JSON-encoded
  string if set, exactly as it should appear on the wire.
"""

import json
import os
import sys
import time


def send_line(obj: dict):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def _chunk_delay_ms() -> float:
    raw = os.environ.get("MOCK_ACP_CHUNK_DELAY_MS", "0")
    try:
        return max(0.0, float(raw))
    except ValueError:
        return 0.0


def _crash_after_chunks():
    raw = os.environ.get("MOCK_ACP_CRASH_AFTER_CHUNKS")
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def _echo_marker(params: dict) -> str:
    """Extract a short distinguishing substring from the incoming prompt."""
    prompt = params.get("prompt", [])
    for block in prompt:
        text = block.get("text")
        if text:
            return text.strip()[:40]
    return ""


def _tool_name() -> str:
    return os.environ.get("MOCK_ACP_TOOL_NAME", "get_weather")


def _tool_args() -> str:
    return os.environ.get("MOCK_ACP_TOOL_ARGS", '{"city": "Paris"}')


def main():
    delay_s = _chunk_delay_ms() / 1000.0
    crash_after = _crash_after_chunks()
    echo_enabled = os.environ.get("MOCK_ACP_ECHO") == "1"

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue

        req_id = req.get("id")
        method = req.get("method", "")
        params = req.get("params", {})

        if method == "initialize":
            send_line(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": 1,
                        "serverInfo": {
                            "name": "mock-acp-agent",
                            "version": "0.1.0",
                        },
                    },
                }
            )

        elif method == "session/new":
            send_line(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"sessionId": "mock-session-123"},
                }
            )

        elif method == "session/prompt":
            session_id = params.get("sessionId", "")
            marker = _echo_marker(params) if echo_enabled else ""
            marker_suffix = f" [{marker}]" if marker else ""

            tool_args = _tool_args()

            chunk_notifications = [
                {
                    "updateType": "agent_thought_chunk",
                    "data": {
                        "content": {
                            "type": "text",
                            "text": f"I should check the weather first.{marker_suffix}",
                        }
                    },
                },
                {
                    "updateType": "tool_call",
                    "data": {
                        "toolCallId": "call_1",
                        "title": "Get weather",
                        "name": _tool_name(),
                        "arguments": tool_args[: max(1, len(tool_args) // 2)],
                    },
                },
                {
                    "updateType": "tool_call_update",
                    "data": {
                        "toolCallId": "call_1",
                        "title": "Get weather",
                        "name": _tool_name(),
                        "arguments": tool_args,
                    },
                },
                {
                    "updateType": "agent_thought_chunk",
                    "data": {
                        "content": {
                            "type": "text",
                            "text": "Now I can answer with the weather data.",
                        }
                    },
                },
                {
                    "updateType": "agent_message_chunk",
                    "data": {"content": f"Hello{marker_suffix} "},
                },
                {
                    "updateType": "agent_message_chunk",
                    "data": {"content": "world! The weather in Paris is sunny."},
                },
            ]


            chunks_sent = 0
            for i, notification in enumerate(chunk_notifications):
                send_line(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {"sessionId": session_id, **notification},
                    }
                )
                chunks_sent += 1

                if crash_after is not None and chunks_sent >= crash_after:
                    # Simulate an agent crash mid-turn: close stdout without
                    # ever sending the final PromptResponse line.
                    sys.stdout.close()
                    return

                if i < len(chunk_notifications) - 1 and delay_s > 0:
                    time.sleep(delay_s)

            # Final prompt response
            send_line(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "sessionId": session_id,
                        "content": [
                            {"type": "text", "text": "Hello world! The weather in Paris is sunny."},
                        ],
                        "done": True,
                    },
                }
            )

        elif method == "session/close":
            send_line({"jsonrpc": "2.0", "id": req_id, "result": {}})


if __name__ == "__main__":
    main()
