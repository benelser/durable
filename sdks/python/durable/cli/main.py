#!/usr/bin/env python3
"""Durable CLI — operational excellence for agent execution.

The sqlite3 shell of durable agent execution. Inspect, debug, optimize.

Usage:
    durable status                    Show all executions
    durable inspect <id>              Detailed execution view
    durable steps <id>                Step-by-step timeline
    durable cost [id]                 Cost breakdown by execution, model, tool
    durable compact [--all]           Compact old event logs
    durable replay <id>               Step-by-step replay with timing
    durable export <id> [-o file]     Export execution as JSON
    durable health                    Storage health and recommendations
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# ANSI colors (no dependencies)
class C:
    BOLD = "\033[1m"
    DIM = "\033[2m"
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN = "\033[36m"
    WHITE = "\033[37m"
    RESET = "\033[0m"

    @staticmethod
    def disable():
        for attr in dir(C):
            if attr.isupper() and attr != "RESET":
                setattr(C, attr, "")
        C.RESET = ""


# Detect if stdout is a terminal
if not sys.stdout.isatty():
    C.disable()


def fmt_time(ts: float) -> str:
    """Format a timestamp as human-readable."""
    if ts == 0:
        return "—"
    from datetime import datetime, timezone
    dt = datetime.fromtimestamp(ts / 1000 if ts > 1e12 else ts, tz=timezone.utc)
    return dt.strftime("%Y-%m-%d %H:%M:%S UTC")


def fmt_duration(ms: float) -> str:
    """Format milliseconds as human-readable duration."""
    if ms < 1000:
        return f"{ms:.0f}ms"
    if ms < 60_000:
        return f"{ms / 1000:.1f}s"
    if ms < 3_600_000:
        return f"{ms / 60_000:.1f}m"
    return f"{ms / 3_600_000:.1f}h"


def fmt_tokens(n: int) -> str:
    """Format token count with K/M suffix."""
    if n < 1000:
        return str(n)
    if n < 1_000_000:
        return f"{n / 1000:.1f}K"
    return f"{n / 1_000_000:.1f}M"


def fmt_cost(dollars: float) -> str:
    """Format dollar cost."""
    if dollars < 0.01:
        return f"${dollars:.4f}"
    if dollars < 1.0:
        return f"${dollars:.2f}"
    return f"${dollars:.2f}"


def fmt_status(status: str) -> str:
    """Color-code execution status."""
    colors = {
        "completed": C.GREEN,
        "running": C.BLUE,
        "suspended": C.YELLOW,
        "failed": C.RED,
        "compensating": C.MAGENTA,
    }
    color = colors.get(status.lower(), C.WHITE)
    return f"{color}{status.upper()}{C.RESET}"


def fmt_bar(fraction: float, width: int = 20) -> str:
    """Render a progress bar."""
    filled = int(fraction * width)
    bar = "█" * filled + "░" * (width - filled)
    pct = fraction * 100
    return f"{bar} {pct:.0f}%"


# ---------------------------------------------------------------------------
# Data loading — reads from the integration backend's file structure
# ---------------------------------------------------------------------------

def find_data_dir() -> Path:
    """Find the durable data directory."""
    # Check common locations
    candidates = [
        Path("./durable-data"),
        Path("./data"),
        Path("./.durable"),
    ]

    # Check environment variable
    env_dir = os.environ.get("DURABLE_DATA_DIR")
    if env_dir:
        candidates.insert(0, Path(env_dir))

    # Check command-line --data-dir
    for i, arg in enumerate(sys.argv):
        if arg == "--data-dir" and i + 1 < len(sys.argv):
            candidates.insert(0, Path(sys.argv[i + 1]))

    for p in candidates:
        if p.exists() and (p / "threads").exists():
            return p
        # Also check for WAL files or event stores
        if p.exists() and any(p.glob("**/*.wal")):
            return p
        if p.exists() and (p / "events").exists():
            return p
        if p.exists() and (p / "executions").exists():
            return p

    return candidates[0]  # Default


def load_threads(data_dir: Path) -> List[Dict]:
    """Load all thread/execution summaries."""
    threads = []
    threads_dir = data_dir / "threads"
    if not threads_dir.exists():
        # Try events directory (Rust event store format)
        events_dir = data_dir / "events"
        if events_dir.exists():
            for f in events_dir.glob("*.ndjson"):
                thread_id = f.stem
                stat = f.stat()
                threads.append({
                    "id": thread_id,
                    "source": "event_store",
                    "size": stat.st_size,
                    "modified": stat.st_mtime,
                    "path": str(f),
                })
        return threads

    for thread_dir in sorted(threads_dir.iterdir()):
        if not thread_dir.is_dir():
            continue

        thread_id = thread_dir.name
        info: Dict[str, Any] = {"id": thread_id, "source": "backend"}

        # Load usage
        usage_path = thread_dir / "usage.json"
        if usage_path.exists():
            try:
                info["usage"] = json.loads(usage_path.read_text())
            except (json.JSONDecodeError, OSError):
                pass

        # Load latest checkpoint
        latest_path = thread_dir / "checkpoints" / "latest.json"
        if latest_path.exists():
            try:
                latest = json.loads(latest_path.read_text())
                info["checkpoint_id"] = latest.get("checkpoint_id")
                info["checkpoint_ts"] = latest.get("ts", 0)
            except (json.JSONDecodeError, OSError):
                pass

        # Count steps
        steps_dir = thread_dir / "steps"
        if steps_dir.exists():
            info["step_count"] = len(list(steps_dir.glob("*.json")))

        # Get total size
        total_size = sum(f.stat().st_size for f in thread_dir.rglob("*") if f.is_file())
        info["size"] = total_size

        threads.append(info)

    return threads


def load_steps(data_dir: Path, thread_id: str) -> List[Dict]:
    """Load all steps for a thread."""
    steps = []
    steps_dir = data_dir / "threads" / thread_id / "steps"
    if not steps_dir.exists():
        return steps

    for f in sorted(steps_dir.glob("*.json")):
        try:
            step = json.loads(f.read_text())
            steps.append(step)
        except (json.JSONDecodeError, OSError):
            continue
    return steps


def load_events(data_dir: Path, thread_id: str) -> List[Dict]:
    """Load events from the Rust event store (NDJSON format)."""
    events_path = data_dir / "events" / f"{thread_id}.ndjson"
    if not events_path.exists():
        return []

    events = []
    for line in events_path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return events


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_status(data_dir: Path, args: List[str]) -> None:
    """Show all executions."""
    threads = load_threads(data_dir)

    if not threads:
        print(f"{C.DIM}No executions found in {data_dir}{C.RESET}")
        return

    # Header
    print(f"\n{C.BOLD}  {'ID':<40} {'STEPS':>6} {'TOKENS':>10} {'COST':>10} {'SIZE':>8}{C.RESET}")
    print(f"  {'─' * 40} {'─' * 6} {'─' * 10} {'─' * 10} {'─' * 8}")

    total_tokens = 0
    total_cost = 0.0
    total_size = 0

    for t in threads:
        tid = t["id"][:38]
        steps = t.get("step_count", 0)
        usage = t.get("usage", {})
        tokens = usage.get("total_tokens", 0)
        cost = usage.get("cost_dollars", 0.0)
        size = t.get("size", 0)

        total_tokens += tokens
        total_cost += cost
        total_size += size

        print(f"  {C.CYAN}{tid:<40}{C.RESET} {steps:>6} {fmt_tokens(tokens):>10} {fmt_cost(cost):>10} {_fmt_size(size):>8}")

    # Summary
    print(f"  {'─' * 40} {'─' * 6} {'─' * 10} {'─' * 10} {'─' * 8}")
    print(f"  {C.BOLD}{'TOTAL':<40} {len(threads):>6} {fmt_tokens(total_tokens):>10} {fmt_cost(total_cost):>10} {_fmt_size(total_size):>8}{C.RESET}")
    print()


def cmd_inspect(data_dir: Path, args: List[str]) -> None:
    """Detailed execution view."""
    if not args:
        print(f"{C.RED}Usage: durable inspect <execution-id>{C.RESET}")
        return

    thread_id = args[0]

    # Try loading from backend
    steps = load_steps(data_dir, thread_id)
    events = load_events(data_dir, thread_id)

    if not steps and not events:
        print(f"{C.RED}Execution '{thread_id}' not found{C.RESET}")
        return

    print(f"\n{C.BOLD}Execution: {C.CYAN}{thread_id}{C.RESET}")
    print(f"{'─' * 60}")

    if events:
        # Parse events for status
        status = "running"
        created_at = 0
        step_count = 0
        for e in events:
            et = e.get("event_type", e).get("type", e.get("type", ""))
            if et == "execution_created":
                created_at = e.get("timestamp", e.get("ts", 0))
            elif et == "execution_completed":
                status = "completed"
            elif et == "execution_failed":
                status = "failed"
            elif et == "suspended":
                status = "suspended"
            elif et in ("step_completed", "step_started"):
                step_count += 1

        print(f"  Status:    {fmt_status(status)}")
        if created_at:
            print(f"  Created:   {fmt_time(created_at)}")
        print(f"  Events:    {len(events)}")
        print(f"  Steps:     {step_count // 2}")  # started + completed pairs

    if steps:
        print(f"\n  {C.BOLD}Steps:{C.RESET}")
        for s in steps:
            name = s.get("step_name", "?")
            ts = s.get("ts", 0)
            result = str(s.get("result", ""))[:80]
            print(f"    {C.GREEN}✓{C.RESET} {name:<30} {C.DIM}{result}{C.RESET}")

    # Usage
    usage_path = data_dir / "threads" / thread_id / "usage.json"
    if usage_path.exists():
        try:
            usage = json.loads(usage_path.read_text())
            print(f"\n  {C.BOLD}Usage:{C.RESET}")
            print(f"    Input tokens:  {fmt_tokens(usage.get('input_tokens', 0))}")
            print(f"    Output tokens: {fmt_tokens(usage.get('output_tokens', 0))}")
            print(f"    Total tokens:  {fmt_tokens(usage.get('total_tokens', 0))}")
            print(f"    LLM calls:     {usage.get('call_count', 0)}")
            print(f"    Est. cost:     {fmt_cost(usage.get('cost_dollars', 0.0))}")
        except (json.JSONDecodeError, OSError):
            pass

    print()


def cmd_steps(data_dir: Path, args: List[str]) -> None:
    """Step-by-step timeline."""
    if not args:
        print(f"{C.RED}Usage: durable steps <execution-id>{C.RESET}")
        return

    thread_id = args[0]
    events = load_events(data_dir, thread_id)
    steps = load_steps(data_dir, thread_id)

    if not events and not steps:
        print(f"{C.RED}Execution '{thread_id}' not found{C.RESET}")
        return

    print(f"\n{C.BOLD}Timeline: {C.CYAN}{thread_id}{C.RESET}")
    print(f"{'─' * 70}")

    if events:
        prev_ts = 0
        for e in events:
            et = e.get("event_type", e)
            if isinstance(et, dict):
                etype = et.get("type", "?")
            else:
                etype = e.get("type", "?")

            ts = e.get("timestamp", e.get("ts", 0))
            delta = f"+{fmt_duration(ts - prev_ts)}" if prev_ts else ""
            prev_ts = ts

            # Color by event type
            icon = "·"
            color = C.DIM
            detail = ""

            if "created" in etype:
                icon = "●"
                color = C.GREEN
            elif "step_started" in etype:
                icon = "▶"
                color = C.BLUE
                sn = et.get("step_name", "") if isinstance(et, dict) else ""
                detail = f" {sn}"
            elif "step_completed" in etype:
                icon = "✓"
                color = C.GREEN
                sn = et.get("step_name", "") if isinstance(et, dict) else ""
                detail = f" {sn}"
            elif "step_failed" in etype:
                icon = "✗"
                color = C.RED
                err = et.get("error", "") if isinstance(et, dict) else ""
                detail = f" {err[:50]}"
            elif "suspended" in etype:
                icon = "⏸"
                color = C.YELLOW
            elif "resumed" in etype:
                icon = "▶"
                color = C.BLUE
            elif "completed" in etype:
                icon = "●"
                color = C.GREEN
            elif "failed" in etype:
                icon = "●"
                color = C.RED

            print(f"  {color}{icon}{C.RESET} {etype:<30} {C.DIM}{delta:>8}{C.RESET}{detail}")

    elif steps:
        for s in steps:
            name = s.get("step_name", "?")
            result = str(s.get("result", ""))[:60]
            print(f"  {C.GREEN}✓{C.RESET} {name:<30} {C.DIM}{result}{C.RESET}")

    print()


def cmd_cost(data_dir: Path, args: List[str]) -> None:
    """Cost breakdown."""
    threads = load_threads(data_dir)

    if args:
        # Cost for specific execution
        threads = [t for t in threads if t["id"].startswith(args[0])]

    total_input = 0
    total_output = 0
    total_cost = 0.0
    total_calls = 0

    rows = []
    for t in threads:
        usage = t.get("usage", {})
        inp = usage.get("input_tokens", 0)
        out = usage.get("output_tokens", 0)
        cost = usage.get("cost_dollars", 0.0)
        calls = usage.get("call_count", 0)

        if inp + out + calls == 0:
            continue

        total_input += inp
        total_output += out
        total_cost += cost
        total_calls += calls

        rows.append((t["id"][:30], inp, out, cost, calls))

    if not rows:
        print(f"{C.DIM}No cost data found{C.RESET}")
        return

    print(f"\n{C.BOLD}  {'EXECUTION':<32} {'INPUT':>10} {'OUTPUT':>10} {'COST':>10} {'CALLS':>6}{C.RESET}")
    print(f"  {'─' * 32} {'─' * 10} {'─' * 10} {'─' * 10} {'─' * 6}")

    for tid, inp, out, cost, calls in rows:
        print(f"  {C.CYAN}{tid:<32}{C.RESET} {fmt_tokens(inp):>10} {fmt_tokens(out):>10} {fmt_cost(cost):>10} {calls:>6}")

    print(f"  {'─' * 32} {'─' * 10} {'─' * 10} {'─' * 10} {'─' * 6}")
    print(f"  {C.BOLD}{'TOTAL':<32} {fmt_tokens(total_input):>10} {fmt_tokens(total_output):>10} {fmt_cost(total_cost):>10} {total_calls:>6}{C.RESET}")
    print()


def cmd_health(data_dir: Path, args: List[str]) -> None:
    """Storage health check with recommendations."""
    threads = load_threads(data_dir)
    total_size = sum(t.get("size", 0) for t in threads)
    total_events = 0

    # Count events
    events_dir = data_dir / "events"
    if events_dir.exists():
        for f in events_dir.glob("*.ndjson"):
            try:
                total_events += sum(1 for line in f.read_text().splitlines() if line.strip())
            except OSError:
                pass

    print(f"\n{C.BOLD}Storage Health: {data_dir}{C.RESET}")
    print(f"{'─' * 50}")
    print(f"  Executions:    {len(threads)}")
    print(f"  Total events:  {total_events}")
    print(f"  Total size:    {_fmt_size(total_size)}")
    print(f"  Data dir:      {data_dir}")

    # Recommendations
    issues = []
    if total_size > 100_000_000:  # 100MB
        issues.append(f"{C.YELLOW}⚠{C.RESET}  Storage exceeds 100MB — run `durable compact --all`")
    if total_events > 10_000:
        issues.append(f"{C.YELLOW}⚠{C.RESET}  Over 10K events — consider compaction")
    if len(threads) > 100:
        issues.append(f"{C.YELLOW}⚠{C.RESET}  Over 100 executions — consider `durable compact --prune-completed`")

    if issues:
        print(f"\n  {C.BOLD}Recommendations:{C.RESET}")
        for issue in issues:
            print(f"    {issue}")
    else:
        print(f"\n  {C.GREEN}✓ Storage is healthy{C.RESET}")

    print()


def cmd_compact(data_dir: Path, args: List[str]) -> None:
    """Compact event logs."""
    events_dir = data_dir / "events"
    if not events_dir.exists():
        print(f"{C.DIM}No event logs to compact{C.RESET}")
        return

    compacted = 0
    saved_bytes = 0

    for f in events_dir.glob("*.ndjson"):
        size_before = f.stat().st_size
        if size_before < 10_000:  # Skip small files
            continue

        lines = [l for l in f.read_text().splitlines() if l.strip()]
        if len(lines) < 20:
            continue

        # Simple compaction: keep only the last snapshot + events after it
        # For full compaction, use the Rust engine
        print(f"  {C.CYAN}{f.stem}{C.RESET}: {len(lines)} events, {_fmt_size(size_before)}")
        compacted += 1

    if compacted == 0:
        print(f"{C.GREEN}✓ Nothing to compact{C.RESET}")
    else:
        print(f"\n  {C.BOLD}Hint:{C.RESET} For full compaction with snapshot, use the Rust engine:")
        print(f"  {C.DIM}  event_store.compact(execution_id){C.RESET}")

    print()


def cmd_export(data_dir: Path, args: List[str]) -> None:
    """Export execution as JSON."""
    if not args:
        print(f"{C.RED}Usage: durable export <execution-id> [-o file]{C.RESET}")
        return

    thread_id = args[0]
    output_file = None
    if "-o" in args:
        idx = args.index("-o")
        if idx + 1 < len(args):
            output_file = args[idx + 1]

    events = load_events(data_dir, thread_id)
    steps = load_steps(data_dir, thread_id)

    if not events and not steps:
        print(f"{C.RED}Execution '{thread_id}' not found{C.RESET}")
        return

    export = {
        "execution_id": thread_id,
        "exported_at": time.time(),
        "events": events,
        "steps": steps,
    }

    # Load usage
    usage_path = data_dir / "threads" / thread_id / "usage.json"
    if usage_path.exists():
        try:
            export["usage"] = json.loads(usage_path.read_text())
        except (json.JSONDecodeError, OSError):
            pass

    output = json.dumps(export, indent=2, default=str)

    if output_file:
        Path(output_file).write_text(output)
        print(f"{C.GREEN}✓ Exported to {output_file} ({_fmt_size(len(output))}){C.RESET}")
    else:
        print(output)


def cmd_replay(data_dir: Path, args: List[str]) -> None:
    """Step-by-step replay with timing."""
    if not args:
        print(f"{C.RED}Usage: durable replay <execution-id>{C.RESET}")
        return

    thread_id = args[0]
    events = load_events(data_dir, thread_id)

    if not events:
        print(f"{C.RED}Execution '{thread_id}' not found or no events{C.RESET}")
        return

    print(f"\n{C.BOLD}Replaying: {C.CYAN}{thread_id}{C.RESET}")
    print(f"{'─' * 60}")
    print(f"  {C.DIM}(press Ctrl+C to stop){C.RESET}\n")

    try:
        prev_ts = 0
        for i, e in enumerate(events):
            et = e.get("event_type", e)
            if isinstance(et, dict):
                etype = et.get("type", "?")
            else:
                etype = e.get("type", "?")

            ts = e.get("timestamp", e.get("ts", 0))
            delta_ms = ts - prev_ts if prev_ts else 0
            prev_ts = ts

            # Simulate timing (scaled down)
            if delta_ms > 100 and i > 0:
                scaled = min(delta_ms / 1000, 0.5)  # Cap at 500ms
                time.sleep(scaled)

            # Format the event
            step_num = f"[{i + 1}/{len(events)}]"
            detail = ""

            if isinstance(et, dict):
                for key in ("step_name", "result", "error", "reason"):
                    if key in et:
                        val = str(et[key])[:60]
                        detail += f" {C.DIM}{key}={val}{C.RESET}"

            color = C.GREEN if "completed" in etype else C.BLUE if "started" in etype else C.YELLOW if "suspended" in etype else C.RED if "failed" in etype else C.WHITE

            print(f"  {C.DIM}{step_num:>10}{C.RESET}  {color}{etype:<28}{C.RESET} {C.DIM}+{fmt_duration(delta_ms)}{C.RESET}{detail}")

    except KeyboardInterrupt:
        print(f"\n  {C.DIM}(replay stopped){C.RESET}")

    print()


def _fmt_size(n: int) -> str:
    """Format byte count as human-readable."""
    if n < 1024:
        return f"{n}B"
    if n < 1024 * 1024:
        return f"{n / 1024:.1f}KB"
    if n < 1024 * 1024 * 1024:
        return f"{n / (1024 * 1024):.1f}MB"
    return f"{n / (1024 * 1024 * 1024):.1f}GB"


# ---------------------------------------------------------------------------
# Main
COMMANDS = {
    "status": cmd_status,
    "inspect": cmd_inspect,
    "steps": cmd_steps,
    "cost": cmd_cost,
    "health": cmd_health,
    "compact": cmd_compact,
    "export": cmd_export,
    "replay": cmd_replay,
}


def main() -> None:
    args = sys.argv[1:]

    # Filter out --data-dir
    filtered_args = []
    skip_next = False
    for a in args:
        if skip_next:
            skip_next = False
            continue
        if a == "--data-dir":
            skip_next = True
            continue
        filtered_args.append(a)
    args = filtered_args

    if not args or args[0] in ("-h", "--help", "help"):
        print(f"""
{C.BOLD}durable{C.RESET} — operational tools for durable agent execution

{C.BOLD}Inspection:{C.RESET}
  durable status                    Show all executions
  durable inspect <id>              Detailed execution view
  durable steps <id>                Step-by-step timeline with timing
  durable cost [id]                 Cost breakdown (tokens, dollars)
  durable replay <id>               Animated step-by-step replay
  durable export <id> [-o file]     Export execution as JSON

{C.BOLD}Maintenance:{C.RESET}
  durable compact                   Compact old event logs
  durable health                    Storage health check

{C.BOLD}Scaffolding:{C.RESET}
  durable-runtime init <name>       Create a new project (any language)

{C.BOLD}Options:{C.RESET}
  --data-dir <path>                 Data directory (default: ./durable-data)

{C.BOLD}Environment:{C.RESET}
  DURABLE_DATA_DIR                  Data directory override
""")
        return

    cmd_name = args[0]
    cmd_args = args[1:]

    if cmd_name not in COMMANDS:
        print(f"{C.RED}Unknown command: {cmd_name}{C.RESET}")
        print(f"Run `durable help` for usage")
        sys.exit(1)

    data_dir = find_data_dir()
    COMMANDS[cmd_name](data_dir, cmd_args)


if __name__ == "__main__":
    main()
