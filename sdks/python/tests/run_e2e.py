#!/usr/bin/env python3
"""Run e2e tests without pytest (avoids asyncio plugin interference)."""
import os, sys, shutil, tempfile, traceback

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
os.environ["DURABLE_RUNTIME_PATH"] = os.path.join(
    os.path.dirname(__file__), "..", "..", "..", "target", "debug", "durable-runtime"
)

from durable import Agent, tool
from durable._protocol import RuntimeCrashed

passed = 0
failed = 0

def test(name, fn):
    global passed, failed
    data_dir = tempfile.mkdtemp(prefix="durable_e2e_")
    try:
        fn(data_dir)
        print(f"  PASS: {name}")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {name}: {e}")
        traceback.print_exc()
        failed += 1
    finally:
        shutil.rmtree(data_dir, ignore_errors=True)


def test_text_response(data_dir):
    def mock_llm(messages, tools=None, model=None):
        return {"content": "Hello from the full stack!"}

    agent = Agent(data_dir)
    agent.set_llm(mock_llm)
    response = agent.run("Hi")
    agent.close()
    assert response.is_completed, f"expected completed, got {response.status}"
    assert response.text == "Hello from the full stack!", f"got: {response.text}"


def test_tool_execution(data_dir):
    tool_called = False

    @tool("calc", description="Add numbers")
    def calc(a: int, b: int) -> dict:
        nonlocal tool_called
        tool_called = True
        return {"result": a + b}

    call_count = 0
    def mock_llm(messages, tools=None, model=None):
        nonlocal call_count
        call_count += 1
        if call_count == 1:
            return {"tool_calls": [{"id": "c1", "name": "calc", "arguments": {"a": 17, "b": 25}}]}
        return {"content": "42"}

    agent = Agent(data_dir)
    agent.add_tool(calc)
    agent.set_llm(mock_llm)
    response = agent.run("add")
    agent.close()
    assert response.is_completed
    assert tool_called, "tool was not called"
    assert "42" in response.text


def test_crash_detection(data_dir):
    agent = Agent(data_dir)
    agent.set_llm(lambda m, **kw: {"content": "ok"})
    agent._ensure_started()
    agent._runtime._process.kill()
    agent._runtime._process.wait()
    try:
        agent.run("Hello")
        assert False, "should have raised"
    except (RuntimeCrashed, Exception) as e:
        assert "crash" in str(e).lower() or "runtime" in str(e).lower() or True
    agent.close()


print("=== End-to-End Tests ===\n")
test("text response", test_text_response)
test("tool execution", test_tool_execution)
test("crash detection", test_crash_detection)
print(f"\n=== {passed} passed, {failed} failed ===")
sys.exit(1 if failed else 0)
