"""Agent response types."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, Optional


class ExecutionStatus(str, Enum):
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    SUSPENDED = "suspended"


@dataclass(frozen=True)
class SuspendReason:
    """Why an execution was suspended."""

    type: str
    details: Dict[str, Any] = field(default_factory=dict)

    @property
    def tool_name(self) -> Optional[str]:
        return self.details.get("tool_name")

    @property
    def confirmation_id(self) -> Optional[str]:
        return self.details.get("confirmation_id")

    @property
    def signal_name(self) -> Optional[str]:
        return self.details.get("signal_name")

    @property
    def contract_name(self) -> Optional[str]:
        return self.details.get("contract_name")

    @property
    def dimension(self) -> Optional[str]:
        return self.details.get("dimension")

    @classmethod
    def from_dict(cls, data: dict) -> SuspendReason:
        reason_type = data.get("type", "unknown")
        details = {k: v for k, v in data.items() if k != "type"}
        return cls(type=reason_type, details=details)

    def __str__(self) -> str:
        if self.type == "contract_violation":
            return f"contract '{self.contract_name}' violated: {self.details.get('reason', '')}"
        if self.type == "budget_exhausted":
            return f"budget exhausted: {self.dimension}"
        if self.type == "waiting_for_confirmation":
            return f"waiting for confirmation on '{self.tool_name}'"
        if self.type == "waiting_for_signal":
            return f"waiting for signal '{self.signal_name}'"
        if self.type == "waiting_for_input":
            return f"waiting for input"
        return f"{self.type}: {self.details}"


@dataclass
class AgentResponse:
    """Result of running an agent."""

    text: Optional[str] = None
    execution_id: str = ""
    status: ExecutionStatus = ExecutionStatus.COMPLETED
    suspend_reason: Optional[SuspendReason] = None

    @property
    def is_suspended(self) -> bool:
        return self.status == ExecutionStatus.SUSPENDED

    @property
    def is_completed(self) -> bool:
        return self.status == ExecutionStatus.COMPLETED

    def __str__(self) -> str:
        if self.text:
            return self.text
        if self.is_suspended and self.suspend_reason:
            return f"[suspended: {self.suspend_reason}]"
        if self.is_suspended:
            return "[suspended]"
        return ""

    def __repr__(self) -> str:
        return f"AgentResponse(status={self.status.value}, text={self.text!r})"


@dataclass(frozen=True)
class StreamChunk:
    """A chunk from streaming output."""

    text: str = ""
    is_final: bool = False

    def __str__(self) -> str:
        return self.text
