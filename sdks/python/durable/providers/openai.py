"""OpenAI provider — GPT-4, GPT-4o, o1, o3, etc.

Zero dependencies. Uses stdlib ``urllib.request`` for HTTP.

Usage::

    from durable import Agent
    from durable.providers import OpenAI

    agent = Agent("./data")
    agent.set_llm(OpenAI())  # reads OPENAI_API_KEY from environment
    response = agent.run("Hello")
"""

from __future__ import annotations

import os
from typing import Any, Dict, List, Optional

from ._http import post_json


class OpenAI:
    """OpenAI chat completion provider.

    Args:
        api_key: API key. Defaults to ``OPENAI_API_KEY`` environment variable.
        model: Model identifier. Defaults to ``"gpt-4o"``.
        base_url: API base URL. Defaults to ``"https://api.openai.com/v1"``.
        temperature: Sampling temperature.
        max_tokens: Maximum tokens in the response.
        timeout: HTTP timeout in seconds.
    """

    def __init__(
        self,
        *,
        api_key: Optional[str] = None,
        model: str = "gpt-4o",
        base_url: str = "https://api.openai.com/v1",
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
        timeout: float = 120,
    ) -> None:
        self.api_key = api_key or os.environ.get("OPENAI_API_KEY", "")
        if not self.api_key:
            raise ValueError(
                "OpenAI API key required. Set OPENAI_API_KEY or pass api_key=."
            )
        self.model = model
        self.base_url = base_url.rstrip("/")
        self.temperature = temperature
        self.max_tokens = max_tokens
        self.timeout = timeout

    def __call__(
        self,
        messages: List[dict],
        tools: Optional[Any] = None,
        model: Optional[str] = None,
    ) -> dict:
        """Handle an LLM request from the durable runtime.

        Translates the runtime's message format to OpenAI's API format
        and returns the response in the runtime's expected format.
        """
        # Build the request
        body: Dict[str, Any] = {
            "model": model or self.model,
            "messages": self._translate_messages(messages),
        }

        if self.temperature is not None:
            body["temperature"] = self.temperature
        if self.max_tokens is not None:
            body["max_tokens"] = self.max_tokens

        # Add tools if provided
        if tools:
            openai_tools = self._translate_tools(tools)
            if openai_tools:
                body["tools"] = openai_tools

        # Call the API
        response = post_json(
            f"{self.base_url}/chat/completions",
            body,
            headers={"Authorization": f"Bearer {self.api_key}"},
            timeout=self.timeout,
        )

        # Parse the response
        return self._parse_response(response)

    def _translate_messages(self, messages: List[dict]) -> List[dict]:
        """Translate runtime messages to OpenAI format."""
        result = []
        for msg in messages:
            role = msg.get("role", "user")

            if role == "tool":
                # Tool result message
                result.append({
                    "role": "tool",
                    "tool_call_id": msg.get("tool_call_id", ""),
                    "content": str(msg.get("content", "")),
                })
            elif "tool_calls" in msg:
                # Assistant message with tool calls
                calls = msg["tool_calls"]
                openai_calls = []
                for call in (calls if isinstance(calls, list) else []):
                    args = call.get("arguments", {})
                    if isinstance(args, dict):
                        import json
                        args = json.dumps(args)
                    openai_calls.append({
                        "id": call.get("id", ""),
                        "type": "function",
                        "function": {
                            "name": call.get("name", ""),
                            "arguments": args,
                        },
                    })
                result.append({
                    "role": "assistant",
                    "tool_calls": openai_calls,
                })
            else:
                # Regular message
                result.append({
                    "role": role,
                    "content": str(msg.get("content", "")),
                })
        return result

    def _translate_tools(self, tools: Any) -> List[dict]:
        """Translate runtime tool definitions to OpenAI format."""
        if isinstance(tools, list):
            result = []
            for t in tools:
                if isinstance(t, dict):
                    # Already in OpenAI format or runtime format
                    if t.get("type") == "function":
                        result.append(t)
                    elif "name" in t:
                        result.append({
                            "type": "function",
                            "function": {
                                "name": t["name"],
                                "description": t.get("description", ""),
                                "parameters": t.get("parameters", {}),
                            },
                        })
            return result
        return []

    def _parse_response(self, response: dict) -> dict:
        """Parse OpenAI response to runtime format, including token usage."""
        choices = response.get("choices", [])
        if not choices:
            return {"content": ""}

        message = choices[0].get("message", {})

        # Extract token usage
        usage = response.get("usage", {})
        result: Dict[str, Any] = {}
        if usage:
            result["usage"] = {
                "input_tokens": usage.get("prompt_tokens", 0),
                "output_tokens": usage.get("completion_tokens", 0),
            }

        # Check for tool calls
        tool_calls = message.get("tool_calls")
        if tool_calls:
            import json
            calls = []
            for tc in tool_calls:
                fn = tc.get("function", {})
                args = fn.get("arguments", "{}")
                try:
                    parsed_args = json.loads(args)
                except (json.JSONDecodeError, TypeError):
                    parsed_args = {}
                calls.append({
                    "id": tc.get("id", ""),
                    "name": fn.get("name", ""),
                    "arguments": parsed_args,
                })
            result["tool_calls"] = calls
            return result

        # Text response
        result["content"] = message.get("content", "")
        return result
