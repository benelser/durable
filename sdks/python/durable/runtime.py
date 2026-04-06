"""Runtime — a shared, multiplexed durable execution runtime.

One Runtime, N agents. Each agent registers with the runtime and shares
a single Rust subprocess. Agents run as durable threads — spawn with
``rt.go()``, communicate with ``rt.signal()``, react with callbacks.

Example::

    from durable import Runtime, Agent, tool
    from durable.providers import OpenAI

    rt = Runtime("./data")

    @tool("greet", description="Greet someone")
    def greet(name: str) -> dict:
        return {"message": f"Hello, {name}!"}

    agent = Agent("./data", runtime=rt, agent_id="greeter")
    agent.add_tool(greet)
    agent.set_llm(OpenAI())

    exec_id = rt.go(agent, "Greet Alice")
    # Returns immediately. Agent runs in background.
"""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Any, Callable, Dict, Optional, Union

from ._protocol import ProtocolClient
from ._runtime import RuntimeManager
from .errors import DurableError


class Runtime:
    """A shared durable execution runtime.

    Manages one Rust subprocess that multiplexes N agents. Agents register
    via ``Agent(..., runtime=rt)`` and share the subprocess.

    Example::

        rt = Runtime("./data")
        agent_a = Agent("./data", runtime=rt, agent_id="order-bot", ...)
        agent_b = Agent("./data", runtime=rt, agent_id="support-bot", ...)

        exec_id = rt.go(agent_a, "Process order #123")
    """

    def __init__(self, data_dir: Union[str, Path] = "./data") -> None:
        self._data_dir = str(Path(data_dir).resolve())
        self._runtime_mgr = RuntimeManager()
        self._protocol: Optional[ProtocolClient] = None
        self._started = False
        self._on_complete_cb: Optional[Callable] = None
        self._on_suspend_cb: Optional[Callable] = None
        self._agents: Dict[str, Any] = {}  # agent_id -> Agent

    def _ensure_protocol(self) -> ProtocolClient:
        """Start the shared subprocess if needed. Returns the ProtocolClient."""
        if self._protocol and self._started:
            return self._protocol

        process = self._runtime_mgr.start()
        self._protocol = ProtocolClient(process)
        self._protocol.start()
        self._started = True
        return self._protocol

    def go(self, agent: Any, prompt: str, *, execution_id: Optional[str] = None) -> str:
        """Start an agent execution. Non-blocking. Returns execution_id.

        The agent runs in a background thread inside the Rust runtime.
        Use ``on_complete`` and ``on_suspend`` callbacks to react to lifecycle events,
        or call ``agent.run()`` for synchronous blocking execution.
        """
        # Ensure agent is registered with this runtime
        if agent._shared_runtime is not self:
            agent._shared_runtime = self
        if not agent._agent_id:
            import uuid
            agent._agent_id = str(uuid.uuid4())

        protocol = agent._ensure_started()

        import uuid as _uuid
        exec_id = execution_id or str(_uuid.uuid4())

        kwargs: Dict[str, Any] = {
            "input": prompt,
            "agent_id": agent._agent_id,
            "execution_id": exec_id,
        }
        protocol.send_fire_and_forget("run_agent", **kwargs)

        # Track for lifecycle callbacks
        self._agents[agent._agent_id] = agent

        # If lifecycle callbacks are registered, start a watcher thread for this execution
        if self._on_complete_cb or self._on_suspend_cb:
            self._watch_execution(agent._agent_id, exec_id)

        return exec_id

    def signal(self, execution_id: str, signal_name: str, data: Any = None) -> None:
        """Send a signal to a suspended execution. The runtime auto-resumes."""
        protocol = self._ensure_protocol()
        protocol.send_fire_and_forget(
            "signal",
            execution_id=execution_id,
            signal_name=signal_name,
            data=data,
        )

    def on_complete(self, callback: Callable) -> Callable:
        """Register a callback for when any agent completes.

        Callback signature: ``(agent_id: str, execution_id: str, response: str) -> None``

        Can be used as a decorator::

            @rt.on_complete
            def handle_done(agent_id, execution_id, response):
                print(f"Agent {agent_id} finished: {response}")
        """
        self._on_complete_cb = callback
        return callback

    def on_suspend(self, callback: Callable) -> Callable:
        """Register a callback for when any agent suspends.

        Callback signature: ``(agent_id: str, execution_id: str, reason: dict) -> None``
        """
        self._on_suspend_cb = callback
        return callback

    def _watch_execution(self, agent_id: str, exec_id: str) -> None:
        """Spawn a daemon thread that watches for lifecycle events."""
        protocol = self._protocol
        on_complete = self._on_complete_cb
        on_suspend = self._on_suspend_cb

        def watcher():
            if protocol is None:
                return
            try:
                for event in protocol.collect_agent_stream(
                    agent_id, "completed", "suspended", "error", timeout=3600
                ):
                    etype = event.get("type", "")
                    if etype == "completed" and on_complete:
                        on_complete(agent_id, exec_id, event.get("response", ""))
                    elif etype == "suspended" and on_suspend:
                        on_suspend(agent_id, exec_id, event.get("reason", {}))
                    if etype in ("completed", "suspended", "error"):
                        break
            except (TimeoutError, Exception):
                pass

        threading.Thread(target=watcher, daemon=True, name=f"rt-watch-{exec_id[:8]}").start()

    def shutdown(self, timeout: float = 30) -> None:
        """Shut down the runtime. Active agents are interrupted."""
        if self._protocol:
            self._protocol.stop()
        self._runtime_mgr.stop()
        self._started = False
        self._protocol = None

    def __enter__(self) -> Runtime:
        self._ensure_protocol()
        return self

    def __exit__(self, *exc: Any) -> None:
        self.shutdown()
