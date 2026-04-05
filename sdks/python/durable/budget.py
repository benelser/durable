"""Execution budget — cost-aware execution with suspend-on-exhaustion."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional


@dataclass
class Budget:
    """Budget limits for an agent execution.

    When any dimension is exhausted, the agent suspends — not crashes.
    The user can approve more budget and resume.

    Example::

        Budget(max_dollars=2.00, max_llm_calls=10)
    """

    max_dollars: Optional[float] = None
    max_llm_calls: Optional[int] = None
    max_tool_calls: Optional[int] = None
    max_wall_time_secs: Optional[float] = None

    def to_dict(self) -> dict:
        d: dict = {}
        if self.max_dollars is not None:
            d["max_dollars"] = self.max_dollars
        if self.max_llm_calls is not None:
            d["max_llm_calls"] = self.max_llm_calls
        if self.max_tool_calls is not None:
            d["max_tool_calls"] = self.max_tool_calls
        if self.max_wall_time_secs is not None:
            d["max_wall_time_millis"] = int(self.max_wall_time_secs * 1000)
        return d
