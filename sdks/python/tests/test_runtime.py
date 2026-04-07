"""Runtime integration tests — verify core durable guarantees.

These tests use mock LLMs (no API key needed) and exercise the full
pipeline: Python SDK → Rust binary → event log → replay.

Run with: pytest tests/test_runtime.py -v
"""

import os
import shutil
import signal
import time

import pytest

from durable import Agent, Runtime, tool, Budget
from durable.errors import DurableError


DATA_BASE = os.path.join(os.path.dirname(__file__), "..", ".test-data")


@pytest.fixture(autouse=True)
def cleanup():
    """Clean test data before and after each test."""
    shutil.rmtree(DATA_BASE, ignore_errors=True)
    yield
    shutil.rmtree(DATA_BASE, ignore_errors=True)


def data_dir(name):
    return os.path.join(DATA_BASE, name)


# ---- Shared tools ----

COUNTERS = {}


def reset():
    global COUNTERS
    COUNTERS = {}


def count(name):
    COUNTERS[name] = COUNTERS.get(name, 0) + 1
    return COUNTERS[name]


@tool("check_inventory", description="Check item stock")
def check_inventory(item_id: str, quantity: int) -> dict:
    count("check_inventory")
    return {"in_stock": True, "price": 29.99}


@tool("charge_payment", description="Charge payment", requires_confirmation=True)
def charge_payment(customer_id: str, amount: float) -> dict:
    count("charge_payment")
    return {"txn": f"txn_{COUNTERS['charge_payment']}", "status": "charged"}


@tool("send_email", description="Send email")
def send_email(to: str, subject: str) -> dict:
    count("send_email")
    return {"sent": True}


@tool("dangerous_action", description="Do something dangerous")
def dangerous_action(target: str) -> dict:
    count("dangerous_action")
    return {"done": True}


# ---- Mock LLMs ----

def make_order_llm():
    step = {"n": 0}

    def llm(messages, tools=None, model=None):
        step["n"] += 1
        if step["n"] == 1:
            return {"tool_calls": [{"id": "c1", "name": "check_inventory",
                    "arguments": {"item_id": "W-1", "quantity": 1}}]}
        elif step["n"] == 2:
            return {"tool_calls": [{"id": "c2", "name": "charge_payment",
                    "arguments": {"customer_id": "cust-1", "amount": 29.99}}]}
        elif step["n"] == 3:
            return {"tool_calls": [{"id": "c3", "name": "send_email",
                    "arguments": {"to": "a@b.com", "subject": "Order confirmed"}}]}
        return {"content": "Order processed."}

    return llm


def make_simple_llm():
    step = {"n": 0}

    def llm(messages, tools=None, model=None):
        step["n"] += 1
        if step["n"] == 1:
            return {"tool_calls": [{"id": "c1", "name": "check_inventory",
                    "arguments": {"item_id": "X-1", "quantity": 2}}]}
        return {"content": "Done."}

    return llm


def make_dangerous_llm():
    def llm(messages, tools=None, model=None):
        return {"tool_calls": [{"id": "c1", "name": "dangerous_action",
                "arguments": {"target": "everything"}}]}
    return llm


# =========================================================================
# TEST: Basic execution
# =========================================================================

class TestBasicExecution:
    def test_agent_completes(self):
        reset()
        agent = Agent(data_dir("basic"), system_prompt="test")
        agent.add_tool(check_inventory)
        agent.set_llm(make_simple_llm())

        r = agent.run("check inventory")
        assert r.status.value == "completed"
        assert r.text == "Done."
        assert COUNTERS.get("check_inventory") == 1
        agent.close()

    def test_execution_id_returned(self):
        reset()
        agent = Agent(data_dir("exec-id"), system_prompt="test")
        agent.add_tool(check_inventory)
        agent.set_llm(make_simple_llm())

        r = agent.run("check")
        assert r.execution_id, "execution_id should be non-empty"
        assert len(r.execution_id) > 10
        agent.close()


# =========================================================================
# TEST: Exactly-once (crash recovery)
# =========================================================================

class TestExactlyOnce:
    def test_tool_not_reexecuted_on_resume(self):
        """The killer test: run twice with same exec_id, tool runs only once."""
        reset()
        dd = data_dir("exactly-once")

        # Run 1
        agent1 = Agent(dd, system_prompt="test")
        agent1.add_tool(check_inventory)
        agent1.add_tool(send_email)
        agent1.set_llm(make_simple_llm())
        r1 = agent1.run("do it")
        exec_id = r1.execution_id
        agent1.close()

        run1_inv = COUNTERS.get("check_inventory", 0)
        assert run1_inv == 1

        # Run 2 — same exec_id
        agent2 = Agent(dd, system_prompt="test")
        agent2.add_tool(check_inventory)
        agent2.add_tool(send_email)
        agent2.set_llm(make_simple_llm())
        r2 = agent2.run("do it", execution_id=exec_id)
        agent2.close()

        # Tool should NOT have been called again
        assert COUNTERS["check_inventory"] == run1_inv, \
            f"Tool re-executed! Expected {run1_inv}, got {COUNTERS['check_inventory']}"

    def test_side_by_side_crash_demo(self):
        """
        Side-by-side: without durable, a crash means double execution.
        With durable, exactly-once is guaranteed.

        This IS the demo that no other framework can show.
        """
        reset()
        dd = data_dir("crash-demo")

        # Use a payment tool WITHOUT confirmation gate for this test
        @tool("pay", description="Charge payment (no confirmation)")
        def pay(customer_id: str, amount: float) -> dict:
            count("pay")
            return {"txn": f"txn_{COUNTERS['pay']}", "status": "charged"}

        def order_llm():
            step = {"n": 0}
            def llm(messages, tools=None, model=None):
                step["n"] += 1
                if step["n"] == 1:
                    return {"tool_calls": [{"id": "c1", "name": "check_inventory",
                            "arguments": {"item_id": "W-1", "quantity": 1}}]}
                elif step["n"] == 2:
                    return {"tool_calls": [{"id": "c2", "name": "pay",
                            "arguments": {"customer_id": "cust-1", "amount": 29.99}}]}
                elif step["n"] == 3:
                    return {"tool_calls": [{"id": "c3", "name": "send_email",
                            "arguments": {"to": "a@b.com", "subject": "Confirmed"}}]}
                return {"content": "Order processed."}
            return llm

        # Run 1: full execution
        agent = Agent(dd, system_prompt="process order")
        agent.add_tool(check_inventory)
        agent.add_tool(pay)
        agent.add_tool(send_email)
        agent.set_llm(order_llm())
        r1 = agent.run("process")
        exec_id = r1.execution_id
        agent.close()

        assert COUNTERS.get("check_inventory") == 1
        assert COUNTERS.get("pay") == 1
        assert COUNTERS.get("send_email") == 1

        # "CRASH" — restart with same exec_id
        agent2 = Agent(dd, system_prompt="process order")
        agent2.add_tool(check_inventory)
        agent2.add_tool(pay)
        agent2.add_tool(send_email)
        agent2.set_llm(order_llm())
        r2 = agent2.run("process", execution_id=exec_id)
        agent2.close()

        # EXACTLY ONCE: no tools re-executed
        assert COUNTERS["check_inventory"] == 1, "inventory re-checked!"
        assert COUNTERS["pay"] == 1, "PAYMENT DOUBLE-CHARGED!"
        assert COUNTERS["send_email"] == 1, "email re-sent!"


# =========================================================================
# TEST: Confirmation gates
# =========================================================================

class TestConfirmationGates:
    def test_agent_suspends_for_confirmation(self):
        reset()
        dd = data_dir("confirm")

        agent = Agent(dd, system_prompt="process")
        agent.add_tool(check_inventory)
        agent.add_tool(charge_payment)
        agent.set_llm(make_order_llm())

        r = agent.run("process order")
        assert r.is_suspended, "should suspend for confirmation"
        assert r.suspend_reason.type == "waiting_for_confirmation"
        assert COUNTERS.get("charge_payment", 0) == 0, "payment should NOT execute before approval"
        agent.close()

    def test_resume_after_approval(self):
        reset()
        dd = data_dir("confirm-resume")

        agent = Agent(dd, system_prompt="process")
        agent.add_tool(check_inventory)
        agent.add_tool(charge_payment)
        agent.add_tool(send_email)
        agent.set_llm(make_order_llm())

        r = agent.run("process")
        assert r.is_suspended
        conf_id = r.suspend_reason.confirmation_id

        agent.approve(r.execution_id, conf_id)
        r2 = agent.resume(r.execution_id)

        assert r2.status.value == "completed"
        assert COUNTERS["charge_payment"] == 1
        agent.close()


# =========================================================================
# TEST: Contract enforcement
# =========================================================================

class TestContracts:
    def test_contract_blocks_tool(self):
        reset()
        dd = data_dir("contract")

        agent = Agent(dd, system_prompt="do it")
        agent.add_tool(dangerous_action)
        agent.set_llm(make_dangerous_llm())

        @agent.contract("no-danger")
        def block(step_name, args):
            if "dangerous" in step_name:
                raise ValueError("blocked by contract")

        r = agent.run("do the dangerous thing")
        assert r.is_suspended
        assert "contract" in r.suspend_reason.type.lower()
        assert COUNTERS.get("dangerous_action", 0) == 0
        agent.close()


# =========================================================================
# TEST: Multi-agent runtime
# =========================================================================

class TestMultiAgent:
    def test_two_agents_one_runtime(self):
        reset()
        dd = data_dir("multi")

        @tool("get_weather", description="Get weather")
        def get_weather(city: str) -> dict:
            count("get_weather")
            return {"temp": 72}

        @tool("get_stock", description="Get stock")
        def get_stock(ticker: str) -> dict:
            count("get_stock")
            return {"price": 150}

        def weather_llm(messages, tools=None, model=None):
            ac = sum(1 for m in messages if m.get("role") == "assistant")
            if ac == 0:
                return {"tool_calls": [{"id": "c1", "name": "get_weather", "arguments": {"city": "NYC"}}]}
            return {"content": "72F"}

        def stock_llm(messages, tools=None, model=None):
            ac = sum(1 for m in messages if m.get("role") == "assistant")
            if ac == 0:
                return {"tool_calls": [{"id": "c1", "name": "get_stock", "arguments": {"ticker": "AAPL"}}]}
            return {"content": "$150"}

        with Runtime(dd) as rt:
            a1 = Agent(dd, runtime=rt, agent_id="weather")
            a1.add_tool(get_weather)
            a1.set_llm(weather_llm)

            a2 = Agent(dd, runtime=rt, agent_id="stock")
            a2.add_tool(get_stock)
            a2.set_llm(stock_llm)

            r1 = a1.run("weather NYC")
            r2 = a2.run("AAPL price")

            assert r1.text and "72" in r1.text
            assert r2.text and "150" in r2.text
            assert r1.execution_id != r2.execution_id


# =========================================================================
# TEST: Health check
# =========================================================================

class TestHealthCheck:
    def test_ping(self):
        dd = data_dir("health")
        with Runtime(dd) as rt:
            pong = rt.ping()
            assert pong["type"] == "pong"
            assert "engine_version" in pong

    def test_status(self):
        dd = data_dir("health-status")
        with Runtime(dd) as rt:
            agent = Agent(dd, runtime=rt, agent_id="test-bot")
            agent.add_tool(check_inventory)
            agent.set_llm(make_simple_llm())
            agent._ensure_started()

            status = rt.status()
            agents = status.get("agents", [])
            assert len(agents) == 1
            assert agents[0]["agent_id"] == "test-bot"


# =========================================================================
# TEST: Structured logging
# =========================================================================

class TestStructuredLogging:
    def test_log_events_captured(self):
        reset()
        dd = data_dir("logging")
        logs = []

        with Runtime(dd) as rt:
            @rt.on_log
            def capture(entry):
                logs.append(entry)

            agent = Agent(dd, runtime=rt, agent_id="log-test")
            agent.add_tool(check_inventory)
            agent.set_llm(make_simple_llm())
            agent.run("check")

        assert len(logs) > 0, "should have captured log entries"
        assert any(e.get("level") == "INFO" for e in logs)
