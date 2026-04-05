"""Integration tests — Python SDK talking to the real Rust binary."""

import os
import shutil
import tempfile
import pytest

from durable import Agent, tool, AgentResponse


# Skip if binary not available
BINARY = os.path.join(
    os.path.dirname(__file__), "..", "..", "..", "target", "release", "durable-runtime"
)
if not os.path.exists(BINARY):
    # Try debug build
    BINARY = os.path.join(
        os.path.dirname(__file__), "..", "..", "..", "target", "debug", "durable-runtime"
    )

SKIP = not os.path.exists(BINARY)


@pytest.fixture
def data_dir():
    d = tempfile.mkdtemp(prefix="durable_test_")
    yield d
    shutil.rmtree(d, ignore_errors=True)


@pytest.mark.skipif(SKIP, reason="durable-runtime binary not found")
def test_agent_create_and_shutdown(data_dir):
    """Agent can start the binary, create an agent, and shut down cleanly."""

    @tool("echo", description="Echo input")
    def echo(text: str) -> dict:
        return {"echoed": text}

    # Set up a mock LLM that returns text
    def mock_llm(messages, tools=None, model=None):
        return {"content": "Hello from mock LLM!"}

    with Agent(data_dir, system_prompt="Test agent") as agent:
        agent.add_tool(echo)
        agent.set_llm(mock_llm)
        response = agent.run("Hi")
        assert response.is_completed
        assert response.text == "Hello from mock LLM!"


@pytest.mark.skipif(SKIP, reason="durable-runtime binary not found")
def test_agent_tool_execution(data_dir):
    """Tools registered in Python are called back from the Rust runtime."""

    call_count = 0

    @tool("counter", description="Count calls")
    def counter() -> dict:
        nonlocal call_count
        call_count += 1
        return {"count": call_count}

    # LLM that requests a tool call then completes
    call_num = 0
    def mock_llm(messages, tools=None, model=None):
        nonlocal call_num
        call_num += 1
        if call_num == 1:
            return {
                "tool_calls": [
                    {"id": "call_1", "name": "counter", "arguments": {}}
                ]
            }
        return {"content": f"Tool was called {call_count} time(s)."}

    with Agent(data_dir) as agent:
        agent.add_tool(counter)
        agent.set_llm(mock_llm)
        response = agent.run("Count")
        assert response.is_completed
        assert call_count == 1
        assert "1" in response.text
