"""Anthropic provider — Claude Sonnet, Opus, Haiku.

Zero dependencies. Uses stdlib ``urllib.request`` for HTTP.

Usage::

    from durable import Agent
    from durable.providers import Anthropic

    agent = Agent("./data")
    agent.set_llm(Anthropic())  # reads ANTHROPIC_API_KEY from environment
    response = agent.run("Hello")
"""

from __future__ import annotations

import os
from typing import Any, Dict, List, Optional

from ._http import post_json


class Anthropic:
    """Anthropic Claude chat provider.

    Args:
        api_key: API key. Defaults to ``ANTHROPIC_API_KEY`` environment variable.
        model: Model identifier. Defaults to ``"claude-sonnet-4-20250514"``.
        max_tokens: Maximum tokens in the response. Defaults to 4096.
        temperature: Sampling temperature.
        timeout: HTTP timeout in seconds.
    """

    API_URL = "https://api.anthropic.com/v1/messages"
    API_VERSION = "2023-06-01"

    def __init__(
        self,
        *,
        api_key: Optional[str] = None,
        model: str = "claude-sonnet-4-20250514",
        max_tokens: int = 4096,
        temperature: Optional[float] = None,
        timeout: float = 120,
    ) -> None:
        self.api_key = api_key or os.environ.get("ANTHROPIC_API_KEY", "")
        if not self.api_key:
            raise ValueError(
                "Anthropic API key required. Set ANTHROPIC_API_KEY or pass api_key=."
            )
        self.model = model
        self.max_tokens = max_tokens
        self.temperature = temperature
        self.timeout = timeout

    def __call__(
        self,
        messages: List[dict],
        tools: Optional[Any] = None,
        model: Optional[str] = None,
    ) -> dict:
        """Handle an LLM request from the durable runtime."""
        # Separate system prompt from messages
        system_prompt, user_messages = self._split_system(messages)

        body: Dict[str, Any] = {
            "model": model or self.model,
            "max_tokens": self.max_tokens,
            "messages": self._translate_messages(user_messages),
        }

        if system_prompt:
            body["system"] = system_prompt

        if self.temperature is not None:
            body["temperature"] = self.temperature

        if tools:
            anthropic_tools = self._translate_tools(tools)
            if anthropic_tools:
                body["tools"] = anthropic_tools

        response = post_json(
            self.API_URL,
            body,
            headers={
                "x-api-key": self.api_key,
                "anthropic-version": self.API_VERSION,
            },
            timeout=self.timeout,
        )

        return self._parse_response(response)

    def _split_system(self, messages: List[dict]) -> tuple:
        """Extract system prompt from message list."""
        system = ""
        user_msgs = []
        for msg in messages:
            if msg.get("role") == "system":
                system = str(msg.get("content", ""))
            else:
                user_msgs.append(msg)
        return system, user_msgs

    def _translate_messages(self, messages: List[dict]) -> List[dict]:
        """Translate runtime messages to Anthropic format."""
        result = []
        for msg in messages:
            role = msg.get("role", "user")

            if role == "tool":
                # Tool result → Anthropic uses role="user" with tool_result content block
                result.append({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": msg.get("tool_call_id", ""),
                        "content": str(msg.get("content", "")),
                    }],
                })
            elif "tool_calls" in msg:
                # Assistant message with tool use
                calls = msg["tool_calls"]
                content = []
                for call in (calls if isinstance(calls, list) else []):
                    content.append({
                        "type": "tool_use",
                        "id": call.get("id", ""),
                        "name": call.get("name", ""),
                        "input": call.get("arguments", {}),
                    })
                result.append({"role": "assistant", "content": content})
            else:
                content = msg.get("content", "")
                result.append({
                    "role": role if role in ("user", "assistant") else "user",
                    "content": str(content),
                })

        return result

    def _translate_tools(self, tools: Any) -> List[dict]:
        """Translate runtime tool definitions to Anthropic format."""
        if isinstance(tools, list):
            result = []
            for t in tools:
                if isinstance(t, dict):
                    if "name" in t:
                        result.append({
                            "name": t["name"],
                            "description": t.get("description", ""),
                            "input_schema": t.get("parameters", {"type": "object"}),
                        })
                    elif t.get("type") == "function":
                        fn = t.get("function", {})
                        result.append({
                            "name": fn.get("name", ""),
                            "description": fn.get("description", ""),
                            "input_schema": fn.get("parameters", {"type": "object"}),
                        })
            return result
        return []

    def _parse_response(self, response: dict) -> dict:
        """Parse Anthropic response to runtime format, including token usage."""
        result: dict = {}

        # Extract token usage
        usage = response.get("usage", {})
        if usage:
            result["usage"] = {
                "input_tokens": usage.get("input_tokens", 0),
                "output_tokens": usage.get("output_tokens", 0),
            }

        content = response.get("content", [])
        if not content:
            result["content"] = ""
            return result

        # Check for tool use
        tool_calls = []
        text_parts = []

        for block in content:
            if block.get("type") == "tool_use":
                tool_calls.append({
                    "id": block.get("id", ""),
                    "name": block.get("name", ""),
                    "arguments": block.get("input", {}),
                })
            elif block.get("type") == "text":
                text_parts.append(block.get("text", ""))

        if tool_calls:
            result["tool_calls"] = tool_calls
            return result

        result["content"] = "\n".join(text_parts)
        return result
