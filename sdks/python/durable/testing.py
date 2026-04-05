"""Testing utilities — MockAgent for unit tests without the Rust binary."""

from __future__ import annotations

from typing import Any, Dict, Iterator, List, Optional

from .response import AgentResponse, ExecutionStatus, StreamChunk, SuspendReason


class MockAgent:
    """Drop-in replacement for Agent in unit tests.

    No Rust binary needed. Scripted responses for deterministic tests.

    Example::

        from durable.testing import MockAgent

        agent = MockAgent(responses=["The weather is sunny."])
        response = agent.run("What's the weather?")
        assert response.text == "The weather is sunny."
        assert agent.last_prompt == "What's the weather?"
    """

    def __init__(
        self,
        responses: Optional[List[str]] = None,
        *,
        suspended: bool = False,
        suspend_reason: Optional[SuspendReason] = None,
    ) -> None:
        self._responses = list(responses or [])
        self._suspended = suspended
        self._suspend_reason = suspend_reason
        self._prompts: List[str] = []
        self._tools: Dict[str, Any] = {}
        self._contracts: List[Any] = []

    def add_tool(self, tool: Any) -> None:
        name = getattr(tool, "name", str(tool))
        self._tools[name] = tool

    def add_response(self, text: str) -> None:
        self._responses.append(text)

    @property
    def last_prompt(self) -> Optional[str]:
        return self._prompts[-1] if self._prompts else None

    @property
    def prompts(self) -> List[str]:
        return list(self._prompts)

    def run(self, prompt: str, **kwargs: Any) -> AgentResponse:
        self._prompts.append(prompt)

        if self._suspended:
            return AgentResponse(
                status=ExecutionStatus.SUSPENDED,
                suspend_reason=self._suspend_reason,
                execution_id="mock-exec-001",
            )

        text = self._responses.pop(0) if self._responses else ""
        return AgentResponse(
            text=text,
            status=ExecutionStatus.COMPLETED,
            execution_id="mock-exec-001",
        )

    def stream(self, prompt: str) -> Iterator[StreamChunk]:
        response = self.run(prompt)
        if response.text:
            # Emit word by word for realistic streaming
            words = response.text.split()
            for i, word in enumerate(words):
                suffix = " " if i < len(words) - 1 else ""
                yield StreamChunk(text=word + suffix)
            yield StreamChunk(text="", is_final=True)

    def resume(self, execution_id: str) -> AgentResponse:
        return AgentResponse(
            text="resumed",
            status=ExecutionStatus.COMPLETED,
            execution_id=execution_id,
        )

    def signal(self, execution_id: str, signal_name: str, data: Any = None) -> None:
        pass

    def approve(self, execution_id: str, confirmation_id: str) -> None:
        pass

    def reject(self, execution_id: str, confirmation_id: str, reason: str = "") -> None:
        pass

    def contract(self, name: str):
        def decorator(fn):
            self._contracts.append((name, fn))
            return fn
        return decorator

    def set_llm(self, handler: Any) -> None:
        pass

    def close(self) -> None:
        pass

    def __enter__(self) -> MockAgent:
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()
