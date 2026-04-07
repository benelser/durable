"""Agent — the main entry point for the durable runtime SDK.

Example::

    from durable import Agent, tool

    @tool("greet", description="Greet someone")
    def greet(name: str) -> dict:
        return {"greeting": f"Hello, {name}!"}

    with Agent("./data") as agent:
        agent.add_tool(greet)
        response = agent.run("Say hello to Alice")
        print(response)
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Callable, Dict, Iterator, List, Optional, Union

from .budget import Budget
from .contract import Contract
from .errors import DurableError, PromptDriftError, ToolDriftError
from .response import AgentResponse, ExecutionStatus, StreamChunk, SuspendReason
from .tool import ToolDefinition, ToolWrapper
from ._protocol import ProtocolClient, RuntimeCrashed
from ._runtime import RuntimeManager


import threading as _threading

# Thread-local storage for the currently executing tool call.
# Each callback thread gets its own idempotency key — safe for
# concurrent tool execution across multiple agents.
_tool_context = _threading.local()


def _raise_classified_error(message: str, retryable: bool = False) -> None:
    """Raise a specific error type based on the error message from the engine."""
    msg_lower = message.lower()
    if "prompt" in msg_lower and ("drift" in msg_lower or "changed" in msg_lower):
        raise PromptDriftError(message)
    if "tool" in msg_lower and ("drift" in msg_lower or "changed" in msg_lower):
        raise ToolDriftError(message)
    raise DurableError(message, retryable=retryable)


def current_idempotency_key() -> str:
    """Get the idempotency key for the currently executing tool call.

    Forward this to payment providers (Stripe, etc.) to prevent
    double-charges if the process crashes between tool execution
    and result persistence.

    Example::

        from durable.agent import current_idempotency_key

        @tool("charge", description="Charge payment")
        def charge(amount: float) -> dict:
            stripe.PaymentIntent.create(
                amount=int(amount * 100),
                idempotency_key=current_idempotency_key(),
            )
    """
    return getattr(_tool_context, "idempotency_key", "")


class Agent:
    """A durable AI agent backed by the Rust runtime.

    The Rust binary is managed as an invisible subprocess. All state is
    persisted to the data directory and survives crashes.

    Example::

        agent = Agent("./data")
        agent.add_tool(my_tool)
        response = agent.run("Hello")
        print(response)
    """

    def __init__(
        self,
        data_dir: Union[str, Path] = "./data",
        *,
        system_prompt: str = "You are a helpful assistant.",
        model: Optional[str] = None,
        max_iterations: int = 50,
        runtime: Optional[Any] = None,
        agent_id: Optional[str] = None,
    ) -> None:
        self._data_dir = str(Path(data_dir).resolve())
        self._system_prompt = system_prompt
        self._model = model
        self._max_iterations = max_iterations
        self._tools: Dict[str, ToolWrapper] = {}
        self._tool_definitions: List[ToolDefinition] = []
        self._contracts: List[Contract] = []
        self._budget: Optional[Budget] = None
        self._llm_handler: Optional[Callable] = None

        # Shared runtime support: if provided, share the subprocess
        self._shared_runtime = runtime
        self._agent_id: str = agent_id or ""

        if runtime is None:
            self._runtime = RuntimeManager()
        else:
            self._runtime = None  # type: ignore[assignment]

        self._protocol: Optional[ProtocolClient] = None
        self._started = False

    # --- Tool Registration ---

    def add_tool(self, tool_wrapper: ToolWrapper) -> None:
        """Register a tool created with the @tool decorator."""
        defn = tool_wrapper.definition
        self._tools[defn.name] = tool_wrapper
        self._tool_definitions.append(defn)

    # --- LLM Provider ---

    def set_llm(self, handler: Callable) -> None:
        """Set the LLM handler function.

        The handler receives ``(messages, tools, model)`` and returns a dict
        with either ``{"content": "..."}`` or ``{"tool_calls": [...]}``.

        Example::

            import anthropic

            client = anthropic.Anthropic()

            def call_claude(messages, tools=None, model=None):
                response = client.messages.create(
                    model=model or "claude-sonnet-4-20250514",
                    messages=messages,
                    tools=tools or [],
                    max_tokens=4096,
                )
                # ... parse response
                return {"content": response.content[0].text}

            agent.set_llm(call_claude)
        """
        self._llm_handler = handler

    # --- Budget ---

    @property
    def budget(self) -> Optional[Budget]:
        return self._budget

    @budget.setter
    def budget(self, value: Budget) -> None:
        self._budget = value

    # --- Contracts ---

    def contract(self, name: str) -> Callable:
        """Decorator to register an enforceable invariant.

        The function receives ``(step_name, args)`` and should raise
        ``ValueError`` if the invariant is violated.

        Example::

            @agent.contract("spending-limit")
            def check(step_name: str, args: dict) -> None:
                if args.get("amount", 0) > 100:
                    raise ValueError("charges over $100 need approval")
        """

        def decorator(fn: Callable) -> Callable:
            self._contracts.append(Contract(name=name, check=fn))
            return fn

        return decorator

    def add_contract(self, contract: Contract) -> None:
        """Register a contract imperatively."""
        self._contracts.append(contract)

    # --- Lifecycle ---

    def _ensure_started(self) -> ProtocolClient:
        """Start the runtime and create the agent if not already done."""
        if self._protocol and self._started:
            return self._protocol

        if self._shared_runtime is not None:
            # Shared runtime: use its protocol client
            self._protocol = self._shared_runtime._ensure_protocol()
            if not self._agent_id:
                import uuid as _uuid
                self._agent_id = str(_uuid.uuid4())
        else:
            # Standalone: start own Rust binary
            process = self._runtime.start()
            self._protocol = ProtocolClient(process)
            self._protocol.start()

        # Register callback handlers
        if self._agent_id:
            # Per-agent callbacks (multiplexed runtime)
            self._protocol.register_agent_buffer(self._agent_id)
            self._protocol.register_agent_callback(self._agent_id, "execute_tool", self._handle_tool_callback)
            self._protocol.register_agent_callback(self._agent_id, "chat_request", self._handle_llm_callback)
            self._protocol.register_agent_callback(self._agent_id, "check_contract", self._handle_contract_callback)
        else:
            # Global callbacks (standalone mode)
            self._protocol.register_callback("execute_tool", self._handle_tool_callback)
            self._protocol.register_callback("chat_request", self._handle_llm_callback)
            self._protocol.register_callback("check_contract", self._handle_contract_callback)

        # Send create_agent command
        config: dict = {
            "system_prompt": self._system_prompt,
            "max_iterations": self._max_iterations,
        }
        if self._model:
            config["model"] = self._model

        tools = [d.to_dict() for d in self._tool_definitions]

        create_kwargs: Dict[str, Any] = {
            "data_dir": self._data_dir,
            "config": config,
            "tools": tools,
            "budget": self._budget.to_dict() if self._budget else None,
            "contracts": [c.name for c in self._contracts],
        }
        if self._agent_id:
            create_kwargs["agent_id"] = self._agent_id

        self._protocol.send_fire_and_forget("create_agent", **create_kwargs)

        # Wait for agent_created or error
        if self._agent_id:
            response = self._protocol.wait_for_agent_event(
                self._agent_id, "agent_created", "error", timeout=10
            )
        else:
            response = self._protocol.wait_for_event("agent_created", "error", timeout=10)

        if response.get("type") == "error":
            raise DurableError(response.get("message", "failed to create agent"))

        self._started = True
        return self._protocol

    def close(self) -> None:
        """Shut down the runtime subprocess (standalone mode only)."""
        if self._shared_runtime is not None:
            # Shared runtime — don't kill the subprocess
            self._started = False
            return
        if self._protocol:
            self._protocol.stop()
        if self._runtime:
            self._runtime.stop()
        self._started = False
        self._protocol = None

    def __enter__(self) -> Agent:
        self._ensure_started()
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    # --- Execution ---

    def run(self, prompt: str, *, execution_id: Optional[str] = None) -> AgentResponse:
        """Run the agent synchronously. Blocks until complete or suspended."""
        protocol = self._ensure_started()

        kwargs: Dict[str, Any] = {"input": prompt}
        if execution_id:
            kwargs["execution_id"] = execution_id
        if self._agent_id:
            kwargs["agent_id"] = self._agent_id

        protocol.send_fire_and_forget("run_agent", **kwargs)

        # Collect events until completion (per-agent or global)
        if self._agent_id:
            stream = protocol.collect_agent_stream(self._agent_id, "completed", "suspended", "error")
        else:
            stream = protocol.collect_stream("completed", "suspended", "error")

        for event in stream:
            event_type = event.get("type", "")

            if event_type == "completed":
                return AgentResponse(
                    text=event.get("response", ""),
                    execution_id=event.get("execution_id", ""),
                    status=ExecutionStatus.COMPLETED,
                )

            if event_type == "suspended":
                reason_data = event.get("reason", {})
                return AgentResponse(
                    execution_id=event.get("execution_id", ""),
                    status=ExecutionStatus.SUSPENDED,
                    suspend_reason=SuspendReason.from_dict(reason_data),
                )

            if event_type == "error":
                _raise_classified_error(
                    event.get("message", "unknown error"),
                    retryable=event.get("retryable", False),
                )

        raise DurableError("no response from runtime")

    def stream(self, prompt: str) -> Iterator[StreamChunk]:
        """Stream the agent's response token by token."""
        protocol = self._ensure_started()
        kwargs: Dict[str, Any] = {"input": prompt, "stream": True}
        if self._agent_id:
            kwargs["agent_id"] = self._agent_id
        protocol.send_fire_and_forget("run_agent", **kwargs)

        if self._agent_id:
            stream_iter = protocol.collect_agent_stream(self._agent_id, "completed", "suspended", "error")
        else:
            stream_iter = protocol.collect_stream("completed", "suspended", "error")

        for event in stream_iter:
            event_type = event.get("type", "")

            if event_type == "text_delta":
                yield StreamChunk(text=event.get("delta", ""))

            elif event_type == "completed":
                yield StreamChunk(
                    text=event.get("response", ""), is_final=True
                )
                return

            elif event_type in ("suspended", "error"):
                return

    # --- Resumption ---

    def resume(self, execution_id: str) -> AgentResponse:
        """Resume a suspended execution."""
        protocol = self._ensure_started()

        resume_kwargs: Dict[str, Any] = {"execution_id": execution_id}
        if self._agent_id:
            resume_kwargs["agent_id"] = self._agent_id
        protocol.send_fire_and_forget("resume_agent", **resume_kwargs)

        if self._agent_id:
            stream = protocol.collect_agent_stream(self._agent_id, "completed", "suspended", "error")
        else:
            stream = protocol.collect_stream("completed", "suspended", "error")

        for event in stream:
            event_type = event.get("type", "")
            if event_type == "completed":
                return AgentResponse(
                    text=event.get("response", ""),
                    execution_id=execution_id,
                    status=ExecutionStatus.COMPLETED,
                )
            if event_type == "suspended":
                return AgentResponse(
                    execution_id=execution_id,
                    status=ExecutionStatus.SUSPENDED,
                    suspend_reason=SuspendReason.from_dict(event.get("reason", {})),
                )
            if event_type == "error":
                raise DurableError(event.get("message", "unknown error"))

        raise DurableError("no response from runtime")

    def signal(
        self, execution_id: str, signal_name: str, data: Any = None
    ) -> None:
        """Send a signal to a suspended execution."""
        protocol = self._ensure_started()
        protocol.send_fire_and_forget(
            "signal",
            execution_id=execution_id,
            signal_name=signal_name,
            data=data,
        )

    def approve(self, execution_id: str, confirmation_id: str) -> None:
        """Approve a confirmation gate."""
        self.signal(execution_id, confirmation_id, True)

    def reject(
        self, execution_id: str, confirmation_id: str, reason: str = ""
    ) -> None:
        """Reject a confirmation gate."""
        self.signal(
            execution_id,
            confirmation_id,
            {"approved": False, "reason": reason},
        )

    # --- Callback Handlers ---

    def _handle_tool_callback(self, callback_id: str, data: dict) -> dict:
        """Handle an execute_tool callback from the runtime."""
        tool_name = data.get("tool_name", "")
        arguments = data.get("arguments", {})

        # Make idempotency key available to tool functions via thread-local storage.
        # Tools can import: from durable.agent import current_idempotency_key
        _tool_context.idempotency_key = data.get("idempotency_key", "")

        tool_wrapper = self._tools.get(tool_name)
        if tool_wrapper is None:
            return {
                "type": "tool_result",
                "callback_id": callback_id,
                "id": callback_id,
                "output": None,
                "is_error": True,
                "message": f"unknown tool: {tool_name}",
                "retryable": False,
            }

        try:
            # Call the Python tool handler
            if isinstance(arguments, dict):
                result = tool_wrapper(**arguments)
            else:
                result = tool_wrapper()

            return {
                "type": "tool_result",
                "callback_id": callback_id,
                "id": callback_id,
                "output": result,
                "is_error": False,
            }
        except Exception as e:
            return {
                "type": "tool_result",
                "callback_id": callback_id,
                "id": callback_id,
                "output": str(e),
                "is_error": True,
                "message": str(e),
                "retryable": False,
            }

    def _handle_llm_callback(self, callback_id: str, data: dict) -> dict:
        """Handle a chat_request callback from the runtime."""
        if self._llm_handler is None:
            return {
                "type": "chat_response",
                "callback_id": callback_id,
                "id": callback_id,
                "content": "No LLM handler configured",
            }

        messages = data.get("messages", [])
        tools = data.get("tools")
        model = data.get("model")

        try:
            result = self._llm_handler(messages, tools=tools, model=model)

            if isinstance(result, str):
                result = {"content": result}

            response: dict = {
                "type": "chat_response",
                "callback_id": callback_id,
                "id": callback_id,
            }

            if "tool_calls" in result:
                response["tool_calls"] = result["tool_calls"]
            elif "content" in result:
                response["content"] = result["content"]
            elif "text" in result:
                response["content"] = result["text"]
            else:
                response["content"] = str(result)

            return response

        except Exception as e:
            return {
                "type": "error",
                "callback_id": callback_id,
                "id": callback_id,
                "message": str(e),
                "retryable": True,
            }

    def _handle_contract_callback(self, callback_id: str, data: dict) -> dict:
        """Handle a check_contract callback from the runtime."""
        contract_name = data.get("contract_name", "")
        step_name = data.get("step_name", "")
        arguments = data.get("arguments", {})

        # Find the matching contract
        for contract in self._contracts:
            if contract.name == contract_name:
                try:
                    contract.check(step_name, arguments)
                    return {
                        "type": "contract_result",
                        "callback_id": callback_id,
                        "id": callback_id,
                        "passed": True,
                    }
                except (ValueError, Exception) as e:
                    return {
                        "type": "contract_result",
                        "callback_id": callback_id,
                        "id": callback_id,
                        "passed": False,
                        "reason": str(e),
                    }

        # No matching contract — pass by default
        return {
            "type": "contract_result",
            "callback_id": callback_id,
            "id": callback_id,
            "passed": True,
        }
