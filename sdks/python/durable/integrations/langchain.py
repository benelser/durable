"""LangChain / LangGraph integration — durable checkpointer.

Add crash recovery to any LangGraph agent with one line::

    from durable.integrations.langchain import DurableCheckpointer

    compiled = graph.compile(checkpointer=DurableCheckpointer("./data"))

    # Same thread_id = resume from checkpoint
    result = compiled.invoke(
        {"messages": [HumanMessage(content="...")]},
        config={"configurable": {"thread_id": "order-123"}}
    )
"""

from __future__ import annotations

from typing import Any, Iterator, Optional, Sequence, Tuple

from ._base import DurableBackend

try:
    from langgraph.checkpoint.base import (
        BaseCheckpointSaver,
        Checkpoint,
        CheckpointMetadata,
        CheckpointTuple,
    )

    _HAS_LANGGRAPH = True
except ImportError:
    _HAS_LANGGRAPH = False

    # Stub for when langgraph isn't installed
    class BaseCheckpointSaver:  # type: ignore
        pass


class DurableCheckpointer(BaseCheckpointSaver):
    """LangGraph checkpoint saver backed by the durable runtime.

    Persists graph state to disk with atomic writes. Crash-safe:
    if the process dies mid-execution, the graph resumes from the
    last completed super-step on restart.

    Args:
        data_dir: Directory for checkpoint storage.
    """

    def __init__(self, data_dir: str = "./durable-data") -> None:
        if not _HAS_LANGGRAPH:
            raise ImportError(
                "LangGraph is required for DurableCheckpointer. "
                "Install with: pip install durable-runtime[langchain]"
            )
        super().__init__()
        self._backend = DurableBackend(data_dir)

    def _thread_id(self, config: dict) -> str:
        """Extract thread_id from LangGraph config."""
        configurable = config.get("configurable", {})
        return configurable.get("thread_id", "default")

    def put(
        self,
        config: dict,
        checkpoint: dict,
        metadata: dict,
        new_versions: Optional[dict] = None,
    ) -> dict:
        """Save a checkpoint."""
        thread_id = self._thread_id(config)
        checkpoint_id = checkpoint.get("id", f"cp-{id(checkpoint)}")
        self._backend.save_checkpoint(
            thread_id=thread_id,
            checkpoint=checkpoint,
            checkpoint_id=checkpoint_id,
            metadata=metadata,
        )

        return {
            "configurable": {
                "thread_id": thread_id,
                "checkpoint_id": checkpoint_id,
                "checkpoint_ns": config.get("configurable", {}).get("checkpoint_ns", ""),
            }
        }

    def put_writes(
        self,
        config: dict,
        writes: Sequence[Tuple[str, Any]],
        task_id: str,
    ) -> None:
        """Record pending writes for a checkpoint."""
        thread_id = self._thread_id(config)
        checkpoint_id = config.get("configurable", {}).get("checkpoint_id", "pending")
        self._backend.save_writes(thread_id, checkpoint_id, list(writes))

    def get_tuple(self, config: dict) -> Optional[Any]:
        """Load the latest checkpoint as a CheckpointTuple."""
        thread_id = self._thread_id(config)
        result = self._backend.load_checkpoint_tuple(thread_id)
        if result is None:
            return None

        checkpoint_id, checkpoint, metadata = result

        if _HAS_LANGGRAPH:
            return CheckpointTuple(
                config={
                    "configurable": {
                        "thread_id": thread_id,
                        "checkpoint_id": checkpoint_id,
                        "checkpoint_ns": "",
                    }
                },
                checkpoint=checkpoint,
                metadata=metadata,
                pending_writes=self._backend.load_writes(thread_id, checkpoint_id),
            )

        return {
            "config": {"configurable": {"thread_id": thread_id, "checkpoint_id": checkpoint_id}},
            "checkpoint": checkpoint,
            "metadata": metadata,
        }

    def list(
        self,
        config: Optional[dict] = None,
        *,
        filter: Optional[dict] = None,
        before: Optional[dict] = None,
        limit: Optional[int] = None,
    ) -> Iterator[Any]:
        """List checkpoints for a thread."""
        if config is None:
            return

        thread_id = self._thread_id(config)
        checkpoints = self._backend.list_checkpoints(thread_id)

        if limit:
            checkpoints = checkpoints[:limit]

        for cp in checkpoints:
            if _HAS_LANGGRAPH:
                yield CheckpointTuple(
                    config={
                        "configurable": {
                            "thread_id": thread_id,
                            "checkpoint_id": cp["id"],
                            "checkpoint_ns": "",
                        }
                    },
                    checkpoint=cp.get("checkpoint", {}),
                    metadata=cp.get("metadata", {}),
                )
            else:
                yield cp
