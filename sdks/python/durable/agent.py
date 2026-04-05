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
from .errors import DurableError
from .response import AgentResponse, ExecutionStatus, StreamChunk, SuspendReason
from .tool import ToolDefinition, ToolWrapper
from ._protocol import ProtocolClient, RuntimeCrashed
from ._runtime import RuntimeManager


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

        self._runtime = RuntimeManager()
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

        # Start the Rust binary
        process = self._runtime.start()
        self._protocol = ProtocolClient(process)

        # Register callback handlers
        self._protocol.register_callback("execute_tool", self._handle_tool_callback)
        self._protocol.register_callback("chat_request", self._handle_llm_callback)

        # Start the reader thread
        self._protocol.start()

        # Send create_agent command
        config: dict = {
            "system_prompt": self._system_prompt,
            "max_iterations": self._max_iterations,
        }
        if self._model:
            config["model"] = self._model

        tools = [d.to_dict() for d in self._tool_definitions]

        self._protocol.send_fire_and_forget(
            "create_agent",
            data_dir=self._data_dir,
            config=config,
            tools=tools,
            budget=self._budget.to_dict() if self._budget else None,
            contracts=[c.name for c in self._contracts],
        )

        # Wait for agent_created or error
        response = self._protocol.wait_for_event("agent_created", "error", timeout=10)
        if response.get("type") == "error":
            raise DurableError(response.get("message", "failed to create agent"))

        self._started = True
        return self._protocol

    def close(self) -> None:
        """Shut down the runtime subprocess."""
        if self._protocol:
            self._protocol.stop()
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

        protocol.send_fire_and_forget("run_agent", **kwargs)

        # Collect events until completion
        for event in protocol.collect_stream("completed", "suspended", "error"):
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
                raise DurableError(
                    event.get("message", "unknown error"),
                    retryable=event.get("retryable", False),
                )

        raise DurableError("no response from runtime")

    def stream(self, prompt: str) -> Iterator[StreamChunk]:
        """Stream the agent's response token by token."""
        protocol = self._ensure_started()
        protocol.send_fire_and_forget("run_agent", input=prompt, stream=True)

        for event in protocol.collect_stream("completed", "suspended", "error"):
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
        protocol.send_fire_and_forget("resume_agent", execution_id=execution_id)

        for event in protocol.collect_stream("completed", "suspended", "error"):
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
