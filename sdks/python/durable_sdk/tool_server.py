"""Tool server that runs as a child process of the Rust runtime.

Reads tool execution requests from stdin, dispatches to registered
handlers, writes results to stdout. Speaks the durable NDJSON protocol.
"""

import json
import sys
import traceback
from typing import Any, Callable, Dict, Optional

from .protocol import ProtocolMessage, read_message, write_message


class DurableToolServer:
    """Register tool handlers and run the stdin/stdout dispatch loop.

    Example:
        server = DurableToolServer()

        @server.tool("greet", "Greet someone by name")
        def greet(name: str = "world"):
            return f"Hello, {name}!"

        server.run()
    """

    def __init__(self):
        self._handlers: Dict[str, Callable] = {}
        self._definitions: list = []

    def tool(
        self,
        name: str,
        description: str,
        parameters: Optional[dict] = None,
    ):
        """Decorator to register a tool handler."""
        def decorator(fn: Callable):
            self._handlers[name] = fn
            self._definitions.append({
                "name": name,
                "description": description,
                "parameters": parameters or {
                    "type": "object",
                    "properties": {},
                },
            })
            return fn
        return decorator

    def register(
        self,
        name: str,
        handler: Callable,
        description: str = "",
        parameters: Optional[dict] = None,
    ):
        """Register a tool handler imperatively."""
        self._handlers[name] = handler
        self._definitions.append({
            "name": name,
            "description": description,
            "parameters": parameters or {"type": "object", "properties": {}},
        })

    def run(self):
        """Run the tool server loop. Reads from stdin, writes to stdout."""
        while True:
            msg = read_message()
            if msg is None:
                break  # EOF

            if msg.type == "execute":
                tool_name = msg.payload.get("tool_name", "")
                arguments = msg.payload.get("arguments", {})
                self._handle_execute(tool_name, arguments)
            elif msg.type == "heartbeat":
                ts = msg.payload.get("timestamp", 0)
                write_message(ProtocolMessage("heartbeat_ack", timestamp=ts))
            else:
                write_message(ProtocolMessage(
                    "error",
                    message=f"unknown message type: {msg.type}",
                    retryable=False,
                ))

    def _handle_execute(self, tool_name: str, arguments: Any):
        handler = self._handlers.get(tool_name)
        if handler is None:
            write_message(ProtocolMessage(
                "error",
                message=f"unknown tool: {tool_name}",
                retryable=False,
            ))
            return

        try:
            if isinstance(arguments, dict):
                result = handler(**arguments)
            else:
                result = handler(arguments)
            write_message(ProtocolMessage("result", output=result))
        except Exception as e:
            write_message(ProtocolMessage(
                "error",
                message=str(e),
                retryable=False,
            ))
