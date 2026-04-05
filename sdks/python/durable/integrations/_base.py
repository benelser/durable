"""Shared durable backend for all framework integrations.

File-based persistence using only stdlib. No Rust binary required.
Provides step memoization, state checkpointing, and cost tracking.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Any, Dict, Iterator, List, Optional, Tuple


class DurableBackend:
    """Shared durable execution backend for all framework integrations.

    Stores checkpoints, step results, and usage data as JSON files.
    Thread-ID-based: same ID = resume, new ID = fresh execution.
    """

    def __init__(self, data_dir: str = "./durable-data") -> None:
        self._data_dir = Path(data_dir)
        self._data_dir.mkdir(parents=True, exist_ok=True)

    # -- Checkpoints --

    def save_checkpoint(
        self,
        thread_id: str,
        checkpoint: dict,
        checkpoint_id: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> str:
        """Persist execution state. Returns checkpoint ID."""
        cp_dir = self._checkpoint_dir(thread_id)
        cp_dir.mkdir(parents=True, exist_ok=True)

        if checkpoint_id is None:
            checkpoint_id = f"cp-{int(time.time() * 1000)}"

        data = {
            "id": checkpoint_id,
            "thread_id": thread_id,
            "ts": time.time(),
            "checkpoint": checkpoint,
            "metadata": metadata or {},
        }

        path = cp_dir / f"{checkpoint_id}.json"
        self._atomic_write(path, json.dumps(data, default=str))

        # Update the "latest" pointer
        self._atomic_write(cp_dir / "latest.json", json.dumps({
            "checkpoint_id": checkpoint_id,
            "ts": data["ts"],
        }))

        return checkpoint_id

    def load_checkpoint(self, thread_id: str) -> Optional[dict]:
        """Load the latest checkpoint for a thread."""
        cp_dir = self._checkpoint_dir(thread_id)
        latest_path = cp_dir / "latest.json"

        if not latest_path.exists():
            return None

        try:
            latest = json.loads(latest_path.read_text())
            cp_id = latest["checkpoint_id"]
            cp_path = cp_dir / f"{cp_id}.json"
            if cp_path.exists():
                data = json.loads(cp_path.read_text())
                return data.get("checkpoint")
        except (json.JSONDecodeError, KeyError, OSError):
            pass
        return None

    def load_checkpoint_tuple(self, thread_id: str) -> Optional[Tuple[str, dict, dict]]:
        """Load (checkpoint_id, checkpoint, metadata) for a thread."""
        cp_dir = self._checkpoint_dir(thread_id)
        latest_path = cp_dir / "latest.json"

        if not latest_path.exists():
            return None

        try:
            latest = json.loads(latest_path.read_text())
            cp_id = latest["checkpoint_id"]
            cp_path = cp_dir / f"{cp_id}.json"
            if cp_path.exists():
                data = json.loads(cp_path.read_text())
                return (cp_id, data.get("checkpoint", {}), data.get("metadata", {}))
        except (json.JSONDecodeError, KeyError, OSError):
            pass
        return None

    def list_checkpoints(self, thread_id: str) -> List[dict]:
        """List all checkpoints for a thread, newest first."""
        cp_dir = self._checkpoint_dir(thread_id)
        if not cp_dir.exists():
            return []

        checkpoints = []
        for path in sorted(cp_dir.glob("cp-*.json"), reverse=True):
            try:
                data = json.loads(path.read_text())
                checkpoints.append(data)
            except (json.JSONDecodeError, OSError):
                continue
        return checkpoints

    # -- Step memoization --

    def record_step(self, thread_id: str, step_name: str, step_index: int, result: Any) -> None:
        """Record a step result for memoization."""
        steps_dir = self._steps_dir(thread_id)
        steps_dir.mkdir(parents=True, exist_ok=True)

        data = {
            "step_name": step_name,
            "step_index": step_index,
            "result": result,
            "ts": time.time(),
        }
        path = steps_dir / f"{step_index:06d}_{step_name}.json"
        self._atomic_write(path, json.dumps(data, default=str))

    def get_step(self, thread_id: str, step_name: str, step_index: int) -> Optional[Any]:
        """Get a cached step result."""
        path = self._steps_dir(thread_id) / f"{step_index:06d}_{step_name}.json"
        if not path.exists():
            return None
        try:
            data = json.loads(path.read_text())
            return data.get("result")
        except (json.JSONDecodeError, OSError):
            return None

    def completed_steps(self, thread_id: str) -> List[dict]:
        """List all completed steps for a thread."""
        steps_dir = self._steps_dir(thread_id)
        if not steps_dir.exists():
            return []

        steps = []
        for path in sorted(steps_dir.glob("*.json")):
            try:
                data = json.loads(path.read_text())
                steps.append(data)
            except (json.JSONDecodeError, OSError):
                continue
        return steps

    # -- Cost tracking --

    def record_usage(
        self, thread_id: str, input_tokens: int = 0, output_tokens: int = 0, cost_dollars: float = 0.0
    ) -> None:
        """Record token usage for cost tracking."""
        usage_path = self._thread_dir(thread_id) / "usage.json"
        existing = self._load_usage(thread_id)
        existing["input_tokens"] += input_tokens
        existing["output_tokens"] += output_tokens
        existing["total_tokens"] += input_tokens + output_tokens
        existing["cost_dollars"] += cost_dollars
        existing["call_count"] += 1
        self._atomic_write(usage_path, json.dumps(existing))

    def get_usage(self, thread_id: str) -> dict:
        """Get cumulative usage for a thread."""
        return self._load_usage(thread_id)

    def _load_usage(self, thread_id: str) -> dict:
        usage_path = self._thread_dir(thread_id) / "usage.json"
        if usage_path.exists():
            try:
                return json.loads(usage_path.read_text())
            except (json.JSONDecodeError, OSError):
                pass
        return {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "cost_dollars": 0.0,
            "call_count": 0,
        }

    # -- Pending writes (for LangGraph compatibility) --

    def save_writes(self, thread_id: str, checkpoint_id: str, writes: List[Tuple[str, Any]]) -> None:
        """Record pending writes for a checkpoint."""
        writes_dir = self._thread_dir(thread_id) / "writes"
        writes_dir.mkdir(parents=True, exist_ok=True)
        data = {"checkpoint_id": checkpoint_id, "writes": writes, "ts": time.time()}
        path = writes_dir / f"{checkpoint_id}.json"
        self._atomic_write(path, json.dumps(data, default=str))

    def load_writes(self, thread_id: str, checkpoint_id: str) -> List[Tuple[str, Any]]:
        """Load pending writes for a checkpoint."""
        path = self._thread_dir(thread_id) / "writes" / f"{checkpoint_id}.json"
        if not path.exists():
            return []
        try:
            data = json.loads(path.read_text())
            return data.get("writes", [])
        except (json.JSONDecodeError, OSError):
            return []

    # -- Internal helpers --

    def _thread_dir(self, thread_id: str) -> Path:
        return self._data_dir / "threads" / thread_id

    def _checkpoint_dir(self, thread_id: str) -> Path:
        return self._thread_dir(thread_id) / "checkpoints"

    def _steps_dir(self, thread_id: str) -> Path:
        return self._thread_dir(thread_id) / "steps"

    def _atomic_write(self, path: Path, content: str) -> None:
        """Write atomically: temp file + rename."""
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(f".{os.getpid()}.tmp")
        tmp.write_text(content)
        tmp.replace(path)
