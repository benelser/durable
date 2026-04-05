"""Durable AI Agent Runtime — Python SDK.

Zero dependencies. Pure stdlib Python.

Provides:
- DurableToolServer: Run tools that speak the durable protocol on stdio
- DurableLlmAdapter: Wrap any LLM provider to speak the protocol
"""

from .tool_server import DurableToolServer
from .llm_adapter import DurableLlmAdapter
from .protocol import read_message, write_message, ProtocolMessage
from .types import ToolCall, ToolResult, Message

__all__ = [
    "DurableToolServer",
    "DurableLlmAdapter",
    "read_message",
    "write_message",
    "ProtocolMessage",
    "ToolCall",
    "ToolResult",
    "Message",
]
