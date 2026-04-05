"""Tests for the integration backend and framework wrappers.

These tests verify the shared DurableBackend without requiring
LangChain, CrewAI, or ADK to be installed.
"""

import json
import os
import shutil
import sys
import tempfile

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable.integrations._base import DurableBackend


@pytest.fixture
def backend():
    d = tempfile.mkdtemp(prefix="durable_int_test_")
    b = DurableBackend(d)
    yield b
    shutil.rmtree(d, ignore_errors=True)


# ===========================================================================
# Checkpoint persistence
# ===========================================================================

def test_save_and_load_checkpoint(backend):
    state = {"messages": ["hello", "world"], "step": 3}
    cp_id = backend.save_checkpoint("thread-1", state)
    assert cp_id.startswith("cp-")

    loaded = backend.load_checkpoint("thread-1")
    assert loaded == state


def test_load_nonexistent_checkpoint(backend):
    assert backend.load_checkpoint("nonexistent") is None


def test_multiple_checkpoints_returns_latest(backend):
    backend.save_checkpoint("t1", {"step": 1})
    backend.save_checkpoint("t1", {"step": 2})
    backend.save_checkpoint("t1", {"step": 3})

    loaded = backend.load_checkpoint("t1")
    assert loaded["step"] == 3


def test_checkpoint_tuple(backend):
    backend.save_checkpoint("t1", {"data": "value"}, checkpoint_id="cp-100", metadata={"source": "test"})

    result = backend.load_checkpoint_tuple("t1")
    assert result is not None
    cp_id, checkpoint, metadata = result
    assert cp_id == "cp-100"
    assert checkpoint == {"data": "value"}
    assert metadata["source"] == "test"


def test_list_checkpoints(backend):
    backend.save_checkpoint("t1", {"step": 1}, checkpoint_id="cp-001")
    backend.save_checkpoint("t1", {"step": 2}, checkpoint_id="cp-002")
    backend.save_checkpoint("t1", {"step": 3}, checkpoint_id="cp-003")

    cps = backend.list_checkpoints("t1")
    assert len(cps) == 3


# ===========================================================================
# Step memoization
# ===========================================================================

def test_record_and_get_step(backend):
    backend.record_step("t1", "llm_call", 0, {"response": "hello"})
    result = backend.get_step("t1", "llm_call", 0)
    assert result == {"response": "hello"}


def test_get_nonexistent_step(backend):
    assert backend.get_step("t1", "missing", 0) is None


def test_completed_steps_ordered(backend):
    backend.record_step("t1", "step_a", 0, "result_a")
    backend.record_step("t1", "step_b", 1, "result_b")
    backend.record_step("t1", "step_c", 2, "result_c")

    steps = backend.completed_steps("t1")
    assert len(steps) == 3
    assert steps[0]["step_name"] == "step_a"
    assert steps[2]["step_name"] == "step_c"


# ===========================================================================
# Cost tracking
# ===========================================================================

def test_record_usage_accumulates(backend):
    backend.record_usage("t1", input_tokens=100, output_tokens=50)
    backend.record_usage("t1", input_tokens=200, output_tokens=100)

    usage = backend.get_usage("t1")
    assert usage["input_tokens"] == 300
    assert usage["output_tokens"] == 150
    assert usage["total_tokens"] == 450
    assert usage["call_count"] == 2


def test_usage_for_unknown_thread(backend):
    usage = backend.get_usage("unknown")
    assert usage["input_tokens"] == 0
    assert usage["call_count"] == 0


# ===========================================================================
# Pending writes (LangGraph compatibility)
# ===========================================================================

def test_save_and_load_writes(backend):
    writes = [("messages", {"content": "hello"}), ("step", 1)]
    backend.save_writes("t1", "cp-1", writes)

    loaded = backend.load_writes("t1", "cp-1")
    assert len(loaded) == 2
    assert loaded[0][0] == "messages"


def test_load_writes_nonexistent(backend):
    assert backend.load_writes("t1", "missing") == []


# ===========================================================================
# Thread isolation
# ===========================================================================

def test_threads_are_isolated(backend):
    backend.save_checkpoint("thread-a", {"data": "a"})
    backend.save_checkpoint("thread-b", {"data": "b"})

    assert backend.load_checkpoint("thread-a")["data"] == "a"
    assert backend.load_checkpoint("thread-b")["data"] == "b"

    backend.record_step("thread-a", "step", 0, "result-a")
    assert backend.get_step("thread-a", "step", 0) == "result-a"
    assert backend.get_step("thread-b", "step", 0) is None
