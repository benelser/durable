"""Protocol client — bidirectional NDJSON over subprocess stdio."""

from __future__ import annotations

import json
import threading
import uuid
from typing import Any, Callable, Dict, Iterator, Optional
import subprocess


class RuntimeCrashed(Exception):
    """The durable-runtime binary exited unexpectedly."""

    def __init__(self, exit_code: Optional[int] = None, stderr: str = ""):
        self.exit_code = exit_code
        self.stderr = stderr.strip()
        msg = "durable-runtime crashed"
        if exit_code is not None:
            msg += f" (exit code {exit_code})"
        if self.stderr:
            # Show last 5 lines of stderr for context
            lines = self.stderr.split("\n")
            tail = "\n".join(lines[-5:])
            msg += f":\n{tail}"
        msg += "\n\nYour execution state is safe — resume with agent.resume(execution_id)"
        super().__init__(msg)


class ProtocolClient:
    """Bidirectional NDJSON protocol over subprocess stdio.

    Handles:
    - Message serialization/deserialization
    - Request/response correlation via message IDs
    - Callback dispatch (tool execution, LLM calls from the runtime)
    - Thread-safe concurrent I/O
    - Automatic crash detection with clear error messages
    """

    def __init__(self, process: subprocess.Popen) -> None:
        self._process = process
        self._callbacks: Dict[str, Callable] = {}
        self._agent_callbacks: Dict[str, Dict[str, Callable]] = {}  # agent_id -> {msg_type -> handler}
        self._pending: Dict[str, threading.Event] = {}
        self._responses: Dict[str, dict] = {}
        self._events: list = []
        self._agent_events: Dict[str, list] = {}  # agent_id -> event list
        self._event_lock = threading.Lock()
        self._lock = threading.Lock()
        self._reader_thread: Optional[threading.Thread] = None
        self._running = False
        self._crashed = False
        self._crash_event = threading.Event()

    def start(self) -> None:
        """Start the background reader thread."""
        self._running = True
        self._reader_thread = threading.Thread(
            target=self._read_loop, daemon=True, name="durable-protocol-reader"
        )
        self._reader_thread.start()

    def stop(self) -> None:
        """Stop the reader thread."""
        self._running = False
        if self._reader_thread:
            self._reader_thread.join(timeout=2)
            self._reader_thread = None

    def register_callback(self, msg_type: str, handler: Callable) -> None:
        """Register a global handler for a callback message type."""
        self._callbacks[msg_type] = handler

    def register_agent_callback(self, agent_id: str, msg_type: str, handler: Callable) -> None:
        """Register a per-agent callback handler."""
        if agent_id not in self._agent_callbacks:
            self._agent_callbacks[agent_id] = {}
        self._agent_callbacks[agent_id][msg_type] = handler

    def register_agent_buffer(self, agent_id: str) -> None:
        """Create a per-agent event buffer for demultiplexing."""
        with self._event_lock:
            if agent_id not in self._agent_events:
                self._agent_events[agent_id] = []

    def send_command(self, msg_type: str, **payload: Any) -> dict:
        """Send a command and wait for the correlated response."""
        self._check_crashed()

        msg_id = str(uuid.uuid4())
        event = threading.Event()

        with self._lock:
            self._pending[msg_id] = event

        message = {"type": msg_type, "id": msg_id, **payload}
        self._write(message)

        # Wait for response (timeout 300s for long-running agents)
        if not event.wait(timeout=300):
            with self._lock:
                self._pending.pop(msg_id, None)
            self._check_crashed()  # Check if crash caused the timeout
            raise TimeoutError(f"no response for {msg_type} (id={msg_id})")

        self._check_crashed()

        with self._lock:
            self._pending.pop(msg_id, None)
            return self._responses.pop(msg_id, {})

    def send_fire_and_forget(self, msg_type: str, **payload: Any) -> None:
        """Send a command without waiting for a response."""
        self._check_crashed()
        message = {"type": msg_type, "id": str(uuid.uuid4()), **payload}
        self._write(message)

    def send_callback_response(
        self, callback_id: str, msg_type: str, **payload: Any
    ) -> None:
        """Respond to a callback from the runtime."""
        message = {"type": msg_type, "callback_id": callback_id, "id": callback_id, **payload}
        self._write(message)

    def wait_for_event(self, *event_types: str, timeout: float = 300) -> dict:
        """Wait for a specific event type from the runtime."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._check_crashed()
            with self._event_lock:
                for i, event in enumerate(self._events):
                    if event.get("type") in event_types:
                        return self._events.pop(i)
            time.sleep(0.01)

        self._check_crashed()
        raise TimeoutError(f"no event of type {event_types} within {timeout}s")

    def collect_stream(self, *end_types: str, timeout: float = 300) -> Iterator[dict]:
        """Yield events until one of the end types is received."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._check_crashed()
            with self._event_lock:
                if self._events:
                    event = self._events.pop(0)
                    yield event
                    if event.get("type") in end_types:
                        return
                    continue
            time.sleep(0.005)

        self._check_crashed()
        raise TimeoutError(f"stream did not end within {timeout}s")

    def collect_agent_stream(self, agent_id: str, *end_types: str, timeout: float = 300) -> Iterator[dict]:
        """Yield events for a specific agent until one of the end types is received."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._check_crashed()
            with self._event_lock:
                buf = self._agent_events.get(agent_id)
                if buf:
                    event = buf.pop(0)
                    yield event
                    if event.get("type") in end_types:
                        return
                    continue
            time.sleep(0.005)

        self._check_crashed()
        raise TimeoutError(f"agent {agent_id} stream did not end within {timeout}s")

    def wait_for_agent_event(self, agent_id: str, *event_types: str, timeout: float = 300) -> dict:
        """Wait for a specific event type for a specific agent."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._check_crashed()
            with self._event_lock:
                buf = self._agent_events.get(agent_id, [])
                for i, event in enumerate(buf):
                    if event.get("type") in event_types:
                        return buf.pop(i)
            time.sleep(0.01)

        self._check_crashed()
        raise TimeoutError(f"no event of type {event_types} for agent {agent_id} within {timeout}s")

    def _check_crashed(self) -> None:
        """Raise RuntimeCrashed if the binary has exited."""
        if self._crashed:
            stderr = ""
            if self._process.stderr:
                try:
                    stderr = self._process.stderr.read().decode("utf-8", errors="replace")
                except Exception:
                    pass
            raise RuntimeCrashed(
                exit_code=self._process.returncode,
                stderr=stderr,
            )

    def _write(self, message: dict) -> None:
        """Write an NDJSON message to the subprocess stdin."""
        stdin = self._process.stdin
        if stdin is None or self._crashed:
            raise RuntimeCrashed(exit_code=self._process.returncode)
        try:
            line = json.dumps(message, separators=(",", ":")) + "\n"
            stdin.write(line.encode("utf-8"))
            stdin.flush()
        except (BrokenPipeError, OSError):
            self._crashed = True
            self._unblock_all_waiters()
            self._check_crashed()

    def _read_loop(self) -> None:
        """Background thread reading stdout and dispatching messages."""
        stdout = self._process.stdout
        if stdout is None:
            return

        while self._running:
            try:
                line = stdout.readline()
                if not line:
                    # EOF — process exited
                    self._crashed = True
                    self._process.wait()
                    self._unblock_all_waiters()
                    break

                data = json.loads(line.decode("utf-8").strip())
                msg_type = data.get("type", "")
                msg_id = data.get("id", "")
                callback_id = data.get("callback_id", "")

                # Check if this is a callback (runtime asking SDK to do something)
                # Try per-agent callback first, then global
                agent_id = data.get("agent_id", "")
                handler = None
                if agent_id and agent_id in self._agent_callbacks:
                    handler = self._agent_callbacks[agent_id].get(msg_type)
                if handler is None:
                    handler = self._callbacks.get(msg_type)

                if handler is not None:
                    cb_id = callback_id or msg_id
                    threading.Thread(
                        target=self._dispatch_callback,
                        args=(handler, cb_id, data),
                        daemon=True,
                    ).start()
                    continue

                # Check if this is a response to a pending request
                with self._lock:
                    if msg_id in self._pending:
                        self._responses[msg_id] = data
                        self._pending[msg_id].set()
                        continue

                # Otherwise, it's an event — route to per-agent buffer or global
                with self._event_lock:
                    if agent_id and agent_id in self._agent_events:
                        self._agent_events[agent_id].append(data)
                    else:
                        self._events.append(data)

            except (json.JSONDecodeError, ValueError, UnicodeDecodeError):
                continue
            except OSError:
                self._crashed = True
                self._unblock_all_waiters()
                break

    def _unblock_all_waiters(self) -> None:
        """Unblock all threads waiting for responses after a crash."""
        with self._lock:
            for event in self._pending.values():
                event.set()
        # Also push a crash event so collect_stream unblocks
        with self._event_lock:
            self._events.append({"type": "runtime_crashed"})

    def _dispatch_callback(
        self, handler: Callable, callback_id: str, data: dict
    ) -> None:
        """Run a callback handler and send the response."""
        try:
            result = handler(callback_id, data)
            if result is not None:
                self._write(result)
        except Exception as e:
            self.send_callback_response(
                callback_id,
                "error",
                message=str(e),
                retryable=False,
            )
