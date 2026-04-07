"""Durable — the SQLite of durable agent execution.

A Python SDK for the durable runtime. Zero dependencies. Crash-recoverable.
Exactly-once execution. First-class tools, hooks, contracts, and budgets.

Quick start::

    from durable import Agent, tool

    @tool("greet", description="Greet someone")
    def greet(name: str) -> dict:
        return {"greeting": f"Hello, {name}!"}

    with Agent("./data") as agent:
        agent.add_tool(greet)
        response = agent.run("Say hello to Alice")
        print(response)
"""

from .agent import Agent, current_idempotency_key
from .budget import Budget
from .runtime import Runtime
from .contract import Contract
from .errors import (
    BudgetExhausted,
    ContractViolation,
    DurableError,
    LlmError,
    RuntimeCrashed,
    RuntimeNotFound,
    StorageError,
    ToolError,
)
from .response import AgentResponse, ExecutionStatus, StreamChunk, SuspendReason
from .lifecycle import LifecycleManager
from .tool import ToolDefinition, ToolWrapper, tool

__version__ = "0.1.0"

__all__ = [
    # Core
    "Agent",
    "Runtime",
    "tool",
    "Budget",
    "Contract",
    # Response
    "AgentResponse",
    "StreamChunk",
    "SuspendReason",
    "ExecutionStatus",
    # Tools
    "ToolDefinition",
    "ToolWrapper",
    # Errors
    "DurableError",
    "StorageError",
    "ToolError",
    "LlmError",
    "ContractViolation",
    "BudgetExhausted",
    "RuntimeCrashed",
    "RuntimeNotFound",
    # Lifecycle
    "LifecycleManager",
]
