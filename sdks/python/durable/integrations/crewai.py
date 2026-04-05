"""CrewAI integration — durable crew execution with crash recovery.

Add crash recovery to any CrewAI crew with one import::

    from durable.integrations.crewai import DurableCrew

    crew = DurableCrew(
        agents=[researcher, writer],
        tasks=[research_task, write_task],
        data_dir="./data",
    )

    result = crew.kickoff(inputs={"topic": "AI safety"})
    # Crash mid-task → next kickoff() resumes from last completed task
"""

from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

from ._base import DurableBackend

try:
    from crewai import Crew

    _HAS_CREWAI = True
except ImportError:
    _HAS_CREWAI = False


class DurableCrew:
    """A CrewAI Crew wrapper with durable execution.

    Wraps ``Crew.kickoff()`` with crash recovery: completed tasks are
    memoized and skipped on restart. Token usage is tracked across tasks.

    Args:
        agents: List of CrewAI agents.
        tasks: List of CrewAI tasks.
        data_dir: Directory for durable state.
        thread_id: Execution ID for resumption (default: auto-generated from task descriptions).
        **crew_kwargs: Additional arguments passed to ``Crew()``.
    """

    def __init__(
        self,
        agents: list,
        tasks: list,
        data_dir: str = "./durable-data",
        thread_id: Optional[str] = None,
        **crew_kwargs: Any,
    ) -> None:
        if not _HAS_CREWAI:
            raise ImportError(
                "CrewAI is required for DurableCrew. "
                "Install with: pip install durable-runtime[crewai]"
            )

        self._agents = agents
        self._tasks = tasks
        self._crew_kwargs = crew_kwargs
        self._backend = DurableBackend(data_dir)
        self._thread_id = thread_id or self._derive_thread_id(tasks)

    def _derive_thread_id(self, tasks: list) -> str:
        """Generate a stable thread ID from task descriptions."""
        import hashlib

        desc = "|".join(
            getattr(t, "description", str(t))[:100] for t in tasks
        )
        return hashlib.sha256(desc.encode()).hexdigest()[:16]

    def kickoff(self, inputs: Optional[Dict[str, Any]] = None) -> Any:
        """Run the crew with crash recovery.

        Completed tasks are skipped on restart. Results are memoized.
        """
        completed = self._backend.completed_steps(self._thread_id)
        completed_indices = {s["step_index"] for s in completed}

        # Filter to tasks that haven't completed yet
        remaining_tasks = []
        remaining_agents = set()
        for i, task in enumerate(self._tasks):
            if i in completed_indices:
                continue
            remaining_tasks.append(task)
            if hasattr(task, "agent") and task.agent:
                remaining_agents.add(id(task.agent))

        if not remaining_tasks:
            # All tasks completed — return the last result
            if completed:
                last = completed[-1]
                return last.get("result", "")
            return ""

        # Build a crew with only remaining tasks
        # Include all agents (some may be needed for delegation)
        crew = Crew(
            agents=self._agents,
            tasks=remaining_tasks,
            step_callback=self._make_step_callback(len(self._tasks) - len(remaining_tasks)),
            task_callback=self._make_task_callback(len(self._tasks) - len(remaining_tasks)),
            **self._crew_kwargs,
        )

        result = crew.kickoff(inputs=inputs)

        # Save final checkpoint
        self._backend.save_checkpoint(
            self._thread_id,
            {
                "status": "completed",
                "result": str(result),
                "tasks_completed": len(self._tasks),
            },
        )

        return result

    def _make_step_callback(self, offset: int):
        """Create a step callback that records each agent step."""
        step_counter = [0]

        def callback(step_output: Any) -> None:
            step_counter[0] += 1

            # Extract token usage if available
            usage = getattr(step_output, "token_usage", None)
            if usage:
                self._backend.record_usage(
                    self._thread_id,
                    input_tokens=getattr(usage, "prompt_tokens", 0),
                    output_tokens=getattr(usage, "completion_tokens", 0),
                )

        return callback

    def _make_task_callback(self, offset: int):
        """Create a task callback that records completed tasks."""
        task_counter = [offset]

        def callback(task_output: Any) -> None:
            index = task_counter[0]
            task_counter[0] += 1

            # Record the completed task
            result = str(task_output) if task_output else ""
            self._backend.record_step(
                self._thread_id,
                step_name=f"task_{index}",
                step_index=index,
                result=result,
            )

            # Save checkpoint
            self._backend.save_checkpoint(
                self._thread_id,
                {
                    "status": "running",
                    "tasks_completed": index + 1,
                    "tasks_total": len(self._tasks),
                    "last_result": result[:500],
                },
            )

        return callback

    def get_usage(self) -> dict:
        """Get cumulative token usage for this crew execution."""
        return self._backend.get_usage(self._thread_id)

    def reset(self) -> None:
        """Clear all checkpoints and step results for this crew."""
        import shutil

        thread_dir = self._backend._thread_dir(self._thread_id)
        if thread_dir.exists():
            shutil.rmtree(thread_dir)
