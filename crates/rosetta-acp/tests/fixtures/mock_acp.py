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
"""

import json
import sys
import time


def send_line(obj: dict):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
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

            # 1. Thinking / reasoning
            send_line(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "updateType": "agent_thought_chunk",
                        "data": {
                            "content": {
                                "type": "text",
                                "text": "I should check the weather first.",
                            }
                        },
                    },
                }
            )
            time.sleep(0.05)

            # 2. Tool call (agent decides to check weather)
            send_line(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "updateType": "tool_call",
                        "data": {
                            "toolCallId": "call_1",
                            "title": "Get weather",
                            "name": "get_weather",
                            "arguments": '{"city": "Paris"}',
                        },
                    },
                }
            )
            time.sleep(0.05)

            # 3. More thinking after tool call
            send_line(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "updateType": "agent_thought_chunk",
                        "data": {
                            "content": {
                                "type": "text",
                                "text": "Now I can answer with the weather data.",
                            }
                        },
                    },
                }
            )
            time.sleep(0.05)

            # 4. Output text — first chunk
            send_line(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "updateType": "agent_message_chunk",
                        "data": {"content": "Hello "},
                    },
                }
            )
            time.sleep(0.05)

            # 5. Output text — second chunk
            send_line(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "updateType": "agent_message_chunk",
                        "data": {"content": "world! The weather in Paris is sunny."},
                    },
                }
            )

            # 6. Final prompt response
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
