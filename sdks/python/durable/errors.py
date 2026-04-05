"""Error types for the durable runtime SDK."""


class DurableError(Exception):
    """Base exception for all durable runtime errors."""

    def __init__(self, message: str, retryable: bool = False):
        super().__init__(message)
        self.retryable = retryable


class StorageError(DurableError):
    """Storage backend error."""

    def __init__(self, message: str):
        super().__init__(message, retryable=True)


class ToolError(DurableError):
    """Tool execution error."""

    def __init__(self, tool_name: str, message: str, retryable: bool = True):
        super().__init__(f"tool '{tool_name}' error: {message}", retryable=retryable)
        self.tool_name = tool_name


class LlmError(DurableError):
    """LLM call error."""

    def __init__(self, message: str, retryable: bool = True):
        super().__init__(f"LLM error: {message}", retryable=retryable)


class ContractViolation(DurableError):
    """An agent contract was violated."""

    def __init__(self, contract_name: str, step_name: str, reason: str):
        super().__init__(
            f"contract '{contract_name}' violated at '{step_name}': {reason}",
            retryable=False,
        )
        self.contract_name = contract_name
        self.step_name = step_name
        self.reason = reason


class BudgetExhausted(DurableError):
    """Execution budget was exhausted."""

    def __init__(self, dimension: str, limit: str, used: str):
        super().__init__(
            f"budget exhausted: {dimension} ({used} of {limit})",
            retryable=False,
        )
        self.dimension = dimension
        self.limit = limit
        self.used = used


class RuntimeNotFound(DurableError):
    """The durable-runtime binary was not found."""

    def __init__(self, searched: list):
        paths = ", ".join(searched)
        super().__init__(
            f"durable-runtime binary not found. Searched: {paths}",
            retryable=False,
        )
        self.searched = searched


class RuntimeCrashed(DurableError):
    """The durable-runtime binary exited unexpectedly.

    Your execution state is safe — the event log persists across crashes.
    Resume with ``agent.resume(execution_id)``.
    """

    def __init__(self, exit_code=None, stderr=""):
        self.exit_code = exit_code
        self.stderr = stderr.strip() if stderr else ""
        msg = "durable-runtime crashed"
        if exit_code is not None:
            msg += f" (exit code {exit_code})"
        if self.stderr:
            lines = self.stderr.split("\n")
            msg += f": {lines[-1]}"
        msg += "\n\nYour execution state is safe — resume with agent.resume(execution_id)"
        super().__init__(msg, retryable=False)
