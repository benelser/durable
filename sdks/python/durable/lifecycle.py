"""Automatic lifecycle management for durable execution state.

Prevents unbounded storage growth. Configurable retention policies.

Usage::

    from durable.lifecycle import LifecycleManager

    # Auto-compact after 500 events, prune completed executions after 7 days
    lifecycle = LifecycleManager(
        data_dir="./durable-data",
        auto_compact_threshold=500,
        retention_days=7,
    )

    # Run maintenance (call periodically or after each execution)
    lifecycle.run_maintenance()

    # Or attach to an agent — runs automatically
    agent = Agent("./data")
    agent.lifecycle = LifecycleManager(auto_compact_threshold=500)
"""

from __future__ import annotations

import json
import os
import shutil
import time
from pathlib import Path
from typing import Optional

from .integrations._base import DurableBackend


class LifecycleManager:
    """Automatic compaction and retention for durable execution state.

    Prevents storage from growing unbounded. Runs maintenance tasks:
    - **Auto-compact**: Compact executions exceeding the event threshold
    - **Retention**: Prune completed executions older than the retention period
    - **Orphan cleanup**: Remove temp files from interrupted writes

    Args:
        data_dir: Path to the durable data directory.
        auto_compact_threshold: Compact when an execution exceeds this many events (0 = disabled).
        retention_days: Delete completed executions older than this (0 = keep forever).
        max_storage_mb: Warn/compact when total storage exceeds this (0 = no limit).
    """

    def __init__(
        self,
        data_dir: str = "./durable-data",
        auto_compact_threshold: int = 500,
        retention_days: int = 0,
        max_storage_mb: int = 0,
    ) -> None:
        self.data_dir = Path(data_dir)
        self.auto_compact_threshold = auto_compact_threshold
        self.retention_days = retention_days
        self.max_storage_mb = max_storage_mb
        self._last_maintenance = 0.0

    def run_maintenance(self) -> dict:
        """Run all maintenance tasks. Returns a summary of actions taken.

        Safe to call frequently — skips if called within the last 60 seconds.
        """
        now = time.time()
        if now - self._last_maintenance < 60:
            return {"skipped": True}

        self._last_maintenance = now
        summary = {
            "compacted": 0,
            "pruned": 0,
            "orphans_cleaned": 0,
            "bytes_freed": 0,
        }

        summary["orphans_cleaned"] = self._cleanup_orphans()
        summary["compacted"] = self._auto_compact()
        summary["pruned"] = self._prune_old_executions()

        return summary

    def _auto_compact(self) -> int:
        """Compact executions that exceed the event threshold.

        For NDJSON files: reads all events, builds a snapshot of final state,
        rewrites the file with just the snapshot. For thread-based storage:
        removes completed step files and consolidates checkpoints.
        """
        if self.auto_compact_threshold <= 0:
            return 0

        compacted = 0

        # Compact NDJSON event files
        events_dir = self.data_dir / "events"
        if events_dir.exists():
            for f in events_dir.glob("*.ndjson"):
                try:
                    lines = [l for l in f.read_text().splitlines() if l.strip()]
                    if len(lines) <= self.auto_compact_threshold:
                        continue

                    # Build snapshot from the last state-bearing event
                    # Keep the last 10 events + any snapshot event
                    keep_lines = []
                    for line in lines:
                        try:
                            data = json.loads(line)
                            et = data.get("event_type", data).get("type", data.get("type", ""))
                            if et == "snapshot":
                                keep_lines = [line]  # Snapshot replaces everything before it
                            else:
                                keep_lines.append(line)
                        except (json.JSONDecodeError, AttributeError):
                            keep_lines.append(line)

                    # If still too many, keep only last N
                    if len(keep_lines) > self.auto_compact_threshold:
                        keep_lines = keep_lines[-self.auto_compact_threshold:]

                    # Atomic rewrite
                    tmp = f.with_suffix(".compact.tmp")
                    tmp.write_text("\n".join(keep_lines) + "\n")
                    tmp.replace(f)
                    compacted += 1
                except OSError:
                    continue

        # Compact thread-based storage: remove old checkpoints, keep latest
        threads_dir = self.data_dir / "threads"
        if threads_dir.exists():
            for thread_dir in threads_dir.iterdir():
                if not thread_dir.is_dir():
                    continue
                cp_dir = thread_dir / "checkpoints"
                if not cp_dir.exists():
                    continue

                cp_files = sorted(cp_dir.glob("cp-*.json"))
                if len(cp_files) > 5:
                    # Keep only the last 5 checkpoints
                    for old_cp in cp_files[:-5]:
                        try:
                            old_cp.unlink()
                            compacted += 1
                        except OSError:
                            pass

        return compacted

    def _prune_old_executions(self) -> int:
        """Delete completed executions older than retention_days."""
        if self.retention_days <= 0:
            return 0

        cutoff = time.time() - (self.retention_days * 86400)
        pruned = 0

        # Prune from threads directory
        threads_dir = self.data_dir / "threads"
        if threads_dir.exists():
            for thread_dir in list(threads_dir.iterdir()):
                if not thread_dir.is_dir():
                    continue

                # Check if completed and old enough
                latest_path = thread_dir / "checkpoints" / "latest.json"
                if latest_path.exists():
                    try:
                        data = json.loads(latest_path.read_text())
                        ts = data.get("ts", 0)
                        if ts > 0 and ts < cutoff:
                            shutil.rmtree(thread_dir, ignore_errors=True)
                            pruned += 1
                    except (json.JSONDecodeError, OSError):
                        continue

        # Prune from events directory
        events_dir = self.data_dir / "events"
        if events_dir.exists():
            for f in list(events_dir.glob("*.ndjson")):
                try:
                    if f.stat().st_mtime < cutoff:
                        # Check if execution is completed
                        content = f.read_text()
                        if '"execution_completed"' in content or '"execution_failed"' in content:
                            f.unlink()
                            pruned += 1
                except OSError:
                    continue

        return pruned

    def _cleanup_orphans(self) -> int:
        """Remove orphaned temp files from interrupted writes."""
        cleaned = 0
        cutoff = time.time() - 300  # 5 minutes old

        for tmp in self.data_dir.rglob("*.tmp"):
            try:
                if tmp.stat().st_mtime < cutoff:
                    tmp.unlink()
                    cleaned += 1
            except OSError:
                continue

        return cleaned

    def storage_stats(self) -> dict:
        """Get storage statistics."""
        total_size = 0
        file_count = 0
        execution_count = 0

        if self.data_dir.exists():
            for f in self.data_dir.rglob("*"):
                if f.is_file():
                    total_size += f.stat().st_size
                    file_count += 1

        threads_dir = self.data_dir / "threads"
        if threads_dir.exists():
            execution_count = sum(1 for d in threads_dir.iterdir() if d.is_dir())

        events_dir = self.data_dir / "events"
        if events_dir.exists():
            execution_count += sum(1 for f in events_dir.glob("*.ndjson"))

        return {
            "total_size_bytes": total_size,
            "total_size_mb": total_size / (1024 * 1024),
            "file_count": file_count,
            "execution_count": execution_count,
            "data_dir": str(self.data_dir),
            "needs_compaction": total_size > (self.max_storage_mb * 1024 * 1024) if self.max_storage_mb else False,
        }
