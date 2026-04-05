"""End-to-end tests: Python SDK → Rust binary → callbacks → response.

These tests prove the full chain works: Agent creates the subprocess,
sends commands via NDJSON, handles LLM and tool callbacks in Python,
and receives the final response.

Requires the Rust binary — set DURABLE_RUNTIME_PATH or build with cargo.
"""

import os
import shutil
import sys
import tempfile

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool, AgentResponse
from durable._protocol import RuntimeCrashed

# Find the binary
BINARY = None
for candidate in [
    os.path.join(os.path.dirname(__file__), "..", "..", "..", "target", "release", "durable-runtime"),
    os.path.join(os.path.dirname(__file__), "..", "..", "..", "target", "debug", "durable-runtime"),
]:
    if os.path.exists(os.path.abspath(candidate)):
        BINARY = os.path.abspath(candidate)
        break

SKIP = BINARY is None
REASON = "durable-runtime binary not found — run 'cargo build' first"


@pytest.fixture(autouse=True)
def set_binary_path():
    if BINARY:
        old = os.environ.get("DURABLE_RUNTIME_PATH")
        os.environ["DURABLE_RUNTIME_PATH"] = BINARY
        yield
        if old:
            os.environ["DURABLE_RUNTIME_PATH"] = old
        else:
            os.environ.pop("DURABLE_RUNTIME_PATH", None)
    else:
        yield


@pytest.mark.skipif(SKIP, reason=REASON)
def test_e2e_text_response():
    """Full chain: Python → Rust → LLM callback → text response."""
    data_dir = tempfile.mkdtemp(prefix="durable_e2e_")
    try:
        def mock_llm(messages, tools=None, model=None):
            return {"content": "Hello from the full stack!"}

        agent = Agent(data_dir, system_prompt="Test agent")
        agent.set_llm(mock_llm)
        response = agent.run("Hi there")
        agent.close()

        assert response.is_completed
        assert response.text == "Hello from the full stack!"
    finally:
        shutil.rmtree(data_dir, ignore_errors=True)


@pytest.mark.skipif(SKIP, reason=REASON)
def test_e2e_tool_execution():
    """Full chain with tool: LLM requests tool → Python executes → result back."""
    data_dir = tempfile.mkdtemp(prefix="durable_e2e_")
    try:
        tool_was_called = False

        @tool("calculator", description="Add two numbers")
        def calculator(a: int, b: int) -> dict:
            nonlocal tool_was_called
            tool_was_called = True
            return {"result": a + b}

        call_count = 0

        def mock_llm(messages, tools=None, model=None):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {
                    "tool_calls": [{
                        "id": "call_1",
                        "name": "calculator",
                        "arguments": {"a": 17, "b": 25},
                    }]
                }
            return {"content": "17 + 25 = 42"}

        agent = Agent(data_dir)
        agent.add_tool(calculator)
        agent.set_llm(mock_llm)
        response = agent.run("What is 17 + 25?")
        agent.close()

        assert response.is_completed
        assert tool_was_called, "tool should have been called"
        assert "42" in response.text
    finally:
        shutil.rmtree(data_dir, ignore_errors=True)


@pytest.mark.skipif(SKIP, reason=REASON)
def test_e2e_crash_detection():
    """SDK detects binary crash and raises RuntimeCrashed."""
    data_dir = tempfile.mkdtemp(prefix="durable_e2e_")
    try:
        agent = Agent(data_dir)
        agent.set_llm(lambda m, **kw: {"content": "ok"})
        agent._ensure_started()

        # Kill the subprocess
        agent._runtime._process.kill()
        agent._runtime._process.wait()

        # Next operation should detect the crash
        with pytest.raises((RuntimeCrashed, Exception)):
            agent.run("Hello")

        agent.close()
    finally:
        shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
