"""LLM adapter that wraps any LLM provider to speak the durable protocol.

Reads chat requests from stdin, calls the provided LLM function,
writes responses to stdout.
"""

import json
import sys
from typing import Any, Callable, Dict, List, Optional

from .protocol import ProtocolMessage, read_message, write_message
from .types import Message, ToolCall


class DurableLlmAdapter:
    """Wrap an LLM provider to speak the durable NDJSON protocol.

    Example:
        def my_llm(messages, tools=None, model=None):
            # Call OpenAI, Anthropic, local model, etc.
            return {"content": "Hello!"}

        adapter = DurableLlmAdapter(my_llm)
        adapter.run()
    """

    def __init__(self, handler: Callable):
        """
        handler: A function that takes (messages, tools, model) and returns
                 either {"content": "..."} or {"tool_calls": [...]}.
        """
        self._handler = handler

    def run(self):
        """Run the adapter loop. Reads from stdin, writes to stdout."""
        while True:
            msg = read_message()
            if msg is None:
                break

            if msg.type == "chat":
                messages = msg.payload.get("messages", [])
                tools = msg.payload.get("tools")
                model = msg.payload.get("model")
                self._handle_chat(messages, tools, model)
            elif msg.type == "heartbeat":
                ts = msg.payload.get("timestamp", 0)
                write_message(ProtocolMessage("heartbeat_ack", timestamp=ts))
            else:
                write_message(ProtocolMessage(
                    "error",
                    message=f"unknown message type: {msg.type}",
                    retryable=False,
                ))

    def _handle_chat(self, messages: list, tools: Any, model: Optional[str]):
        try:
            result = self._handler(messages, tools=tools, model=model)

            if isinstance(result, dict):
                if "tool_calls" in result:
                    write_message(ProtocolMessage(
                        "tool_calls",
                        calls=result["tool_calls"],
                    ))
                else:
                    content = result.get("content", result.get("text", ""))
                    write_message(ProtocolMessage("text", content=content))
            elif isinstance(result, str):
                write_message(ProtocolMessage("text", content=result))
            else:
                write_message(ProtocolMessage("text", content=str(result)))

        except Exception as e:
            write_message(ProtocolMessage(
                "error",
                message=str(e),
                retryable=True,
            ))
