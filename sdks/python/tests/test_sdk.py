"""Tests for the durable Python SDK."""

import pytest
from durable import Agent, tool, Budget, AgentResponse, ExecutionStatus
from durable.tool import _infer_schema
from durable.testing import MockAgent
from typing import Optional, List


# ===========================================================================
# Tool decorator and schema inference
# ===========================================================================

def test_tool_decorator_creates_wrapper():
    @tool("greet", description="Greet someone")
    def greet(name: str) -> dict:
        return {"greeting": f"Hello, {name}!"}

    assert greet.name == "greet"
    assert greet.definition.description == "Greet someone"
    assert greet(name="World") == {"greeting": "Hello, World!"}


def test_tool_schema_inference_basic():
    def fn(name: str, age: int, active: bool = True):
        pass

    schema = _infer_schema(fn)
    assert schema["type"] == "object"
    assert schema["properties"]["name"] == {"type": "string"}
    assert schema["properties"]["age"] == {"type": "integer"}
    assert schema["properties"]["active"] == {"type": "boolean"}
    assert "name" in schema["required"]
    assert "age" in schema["required"]
    assert "active" not in schema["required"]  # has default


def test_tool_schema_inference_optional():
    def fn(name: str, label: Optional[str] = None):
        pass

    schema = _infer_schema(fn)
    assert schema["properties"]["name"] == {"type": "string"}
    assert schema["properties"]["label"] == {"type": "string"}
    assert "label" not in schema.get("required", [])


def test_tool_schema_inference_list():
    def fn(items: List[str]):
        pass

    schema = _infer_schema(fn)
    assert schema["properties"]["items"]["type"] == "array"
    assert schema["properties"]["items"]["items"] == {"type": "string"}


def test_tool_docstring_as_description():
    @tool("echo")
    def echo(text: str) -> str:
        """Echo the input text back."""
        return text

    assert echo.definition.description == "Echo the input text back."


# ===========================================================================
# Budget
# ===========================================================================

def test_budget_to_dict():
    b = Budget(max_dollars=2.0, max_llm_calls=10)
    d = b.to_dict()
    assert d["max_dollars"] == 2.0
    assert d["max_llm_calls"] == 10
    assert "max_tool_calls" not in d  # None values omitted


def test_budget_wall_time_conversion():
    b = Budget(max_wall_time_secs=300.0)
    d = b.to_dict()
    assert d["max_wall_time_millis"] == 300_000


# ===========================================================================
# MockAgent
# ===========================================================================

def test_mock_agent_returns_scripted_responses():
    agent = MockAgent(responses=["Hello!", "Goodbye!"])
    r1 = agent.run("Hi")
    r2 = agent.run("Bye")
    assert r1.text == "Hello!"
    assert r2.text == "Goodbye!"
    assert r1.is_completed
    assert agent.prompts == ["Hi", "Bye"]


def test_mock_agent_suspended():
    from durable.response import SuspendReason

    agent = MockAgent(
        suspended=True,
        suspend_reason=SuspendReason(type="waiting_for_input", details={}),
    )
    response = agent.run("Do something")
    assert response.is_suspended
    assert response.suspend_reason.type == "waiting_for_input"


def test_mock_agent_stream():
    agent = MockAgent(responses=["Hello world"])
    chunks = list(agent.stream("Hi"))
    assert len(chunks) == 3  # "Hello", " ", "world", final
    text = "".join(c.text for c in chunks)
    assert "Hello" in text
    assert "world" in text


def test_mock_agent_context_manager():
    with MockAgent(responses=["OK"]) as agent:
        response = agent.run("test")
        assert response.text == "OK"


def test_mock_agent_contract_decorator():
    agent = MockAgent(responses=["OK"])

    @agent.contract("test-contract")
    def check(step_name, args):
        pass

    assert len(agent._contracts) == 1


# ===========================================================================
# AgentResponse
# ===========================================================================

def test_response_display_completed():
    r = AgentResponse(text="Hello!", status=ExecutionStatus.COMPLETED)
    assert str(r) == "Hello!"


def test_response_display_suspended():
    from durable.response import SuspendReason

    r = AgentResponse(
        status=ExecutionStatus.SUSPENDED,
        suspend_reason=SuspendReason(type="waiting_for_input", details={}),
    )
    assert "[suspended" in str(r)


def test_response_properties():
    r = AgentResponse(text="Done", execution_id="abc-123", status=ExecutionStatus.COMPLETED)
    assert r.is_completed
    assert not r.is_suspended
    assert r.execution_id == "abc-123"


# ===========================================================================
# Agent (unit tests — no binary needed)
# ===========================================================================

def test_agent_tool_registration():
    @tool("echo", description="Echo input")
    def echo(text: str) -> dict:
        return {"echoed": text}

    agent = Agent.__new__(Agent)
    agent._tools = {}
    agent._tool_definitions = []
    agent.add_tool(echo)

    assert "echo" in agent._tools
    assert len(agent._tool_definitions) == 1
    assert agent._tool_definitions[0].name == "echo"


def test_agent_contract_decorator():
    agent = Agent.__new__(Agent)
    agent._contracts = []

    @agent.contract("max-charge")
    def check(step_name: str, args: dict) -> None:
        if args.get("amount", 0) > 100:
            raise ValueError("too much")

    assert len(agent._contracts) == 1
    assert agent._contracts[0].name == "max-charge"

    # Contract passes
    check("tool_pay", {"amount": 50})

    # Contract fails
    with pytest.raises(ValueError, match="too much"):
        check("tool_pay", {"amount": 200})
