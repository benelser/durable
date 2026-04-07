# Tutorial: Zero to Production

This walks you from first install to a production-ready agent service.

## 1. Install

```bash
pip install durable-runtime
export OPENAI_API_KEY=sk-...
```

## 2. Hello World (30 seconds)

```python
from durable import Agent, tool
from durable.providers import OpenAI

@tool("greet", description="Greet someone by name")
def greet(name: str) -> dict:
    return {"message": f"Hello, {name}!"}

with Agent("./data") as agent:
    agent.add_tool(greet)
    agent.set_llm(OpenAI())
    print(agent.run("Say hello to Alice"))
```

That's it. The agent calls the LLM, the LLM calls the tool, and the
result is returned. If the process crashes mid-execution and you restart,
the agent replays from its event log — the `greet` tool is not called again.

## 3. Crash Recovery (the killer feature)

```python
# Run 1: everything executes
response = agent.run("Process the order")
exec_id = response.execution_id
# Process crashes here

# Run 2: on restart, pass the same execution_id
response = agent.run("Process the order", execution_id=exec_id)
# All completed steps return cached results
# Tools are NOT re-executed
# Payment is NOT double-charged
```

Why this matters: every other framework (LangChain, CrewAI, AutoGen)
loses state on crash. Payment charged but email never sent? Tool called
twice? Conversation history gone? Those are the default behavior.
Durable makes them impossible.

## 4. Confirmation Gates (human-in-the-loop)

Mark any tool as requiring human approval:

```python
@tool("transfer", description="Transfer funds", requires_confirmation=True)
def transfer(from_acct: str, to_acct: str, amount: float) -> dict:
    return {"status": "transferred", "amount": amount}
```

When the LLM tries to call this tool, execution suspends:

```python
response = agent.run("Transfer $5000 from checking to savings")
# response.is_suspended == True
# response.suspend_reason.type == "waiting_for_confirmation"
# response.suspend_reason.confirmation_id == "confirm_transfer_..."

# The tool has NOT executed yet. Human reviews and approves:
agent.approve(response.execution_id, response.suspend_reason.confirmation_id)

# Resume — the tool executes now
response = agent.resume(response.execution_id)
```

The suspension is durable. The process can crash between "agent asks
for approval" and "human approves." On restart, the agent resumes
exactly where it left off.

## 5. Contracts (code-level guardrails)

Contracts are invariant checks that run before a tool executes.
They are not prompt engineering — they are code that the LLM cannot
circumvent through hallucination, jailbreaking, or prompt injection.

```python
@agent.contract("max-charge")
def check_charge(step_name, args):
    if "charge" in step_name:
        amount = args.get("amount", 0)
        if amount > 1000:
            raise ValueError(f"${amount} exceeds $1000 limit")
```

If the contract fails, execution suspends with `contract_violation`.
The tool never executes.

## 6. Budget Limits

```python
from durable import Budget

agent.budget = Budget(
    max_dollars=2.00,      # LLM cost limit
    max_llm_calls=10,      # Call count limit
    max_tool_calls=50,     # Tool call limit
)
```

When the budget is exhausted, the agent suspends (not crashes).
All completed work is preserved. Increase the budget and resume.

## 7. Idempotency Keys

Every tool callback includes an `idempotency_key`. Forward it to
payment providers to prevent double-charges in the narrow window
between "tool executed" and "result persisted":

```python
from durable import tool
from durable.agent import current_idempotency_key

@tool("charge", description="Charge payment")
def charge(customer_id: str, amount: float) -> dict:
    stripe.PaymentIntent.create(
        amount=int(amount * 100),
        currency="usd",
        customer=customer_id,
        idempotency_key=current_idempotency_key(),
    )
    return {"status": "charged"}
```

## 8. Embedded Runtime (production)

For production services, use the `Runtime` class. One runtime,
multiple agents, running as durable threads inside your process:

```python
from durable import Runtime, Agent, tool
from durable.providers import OpenAI

rt = Runtime("./data")

order_agent = Agent(
    "./data",
    runtime=rt,
    agent_id="order-processor",
    system_prompt="You process orders...",
)
order_agent.add_tool(check_inventory)
order_agent.add_tool(charge_payment)
order_agent.add_tool(send_email)
order_agent.set_llm(OpenAI())

# Non-blocking spawn (like go func())
exec_id = rt.go(order_agent, "Process order #456")

# Send signals (from webhooks, other services, etc.)
rt.signal(exec_id, confirmation_id, True)
# The runtime auto-resumes — no explicit resume() call needed

# Lifecycle callbacks
@rt.on_complete
def done(agent_id, exec_id, response):
    update_database(exec_id, status="done")

@rt.on_suspend
def paused(agent_id, exec_id, reason):
    send_slack_notification(f"Approval needed: {reason}")

@rt.on_log
def log(entry):
    logger.info(entry["msg"], extra=entry)

# Health check
pong = rt.ping()  # engine_version, agents_registered, agents_active
```

## 9. FastAPI Integration

```python
from fastapi import FastAPI
from durable import Runtime, Agent, tool
from durable.providers import OpenAI

app = FastAPI()
rt = Runtime("./data")

order_agent = Agent("./data", runtime=rt, agent_id="orders", ...)

@app.post("/orders")
def create_order(order: OrderRequest):
    exec_id = rt.go(order_agent, f"Process: {order.json()}")
    return {"execution_id": exec_id}

@app.post("/orders/{exec_id}/approve")
def approve(exec_id: str, confirmation_id: str):
    rt.signal(exec_id, confirmation_id, True)
    return {"status": "approved"}  # agent auto-resumes

@app.get("/health")
def health():
    return rt.ping()
```

## 10. CLI Inspection

After any agent runs, inspect everything:

```bash
# See all executions
durable status --data-dir ./data

# Step-by-step timeline
durable steps <execution-id> --data-dir ./data

# Detailed inspection
durable inspect <execution-id> --data-dir ./data

# Cost breakdown
durable cost <execution-id> --data-dir ./data

# Animated replay
durable replay <execution-id> --data-dir ./data

# Export as JSON
durable export <execution-id> --data-dir ./data
```

## 11. Testing

```python
from durable.testing import MockAgent

def test_order_flow():
    agent = MockAgent(responses=["Order #123 processed."])
    response = agent.run("Process order #123")
    assert response.text == "Order #123 processed."
    assert agent.last_prompt == "Process order #123"
```

For integration tests that exercise the full pipeline (Rust binary + event log):

```bash
cd sdks/python
pytest tests/test_runtime.py -v
```

## Architecture

```
┌─────────────────────────────────────────────┐
│  Your App (Flask, FastAPI, CLI, cron)       │
│  rt = Runtime("./data")                    │
│  rt.go(agent, prompt)                      │
├─────────────────────────────────────────────┤
│  Python SDK (zero dependencies)             │
│  Callback handlers for tools + LLM         │
├─────────────────────────────────────────────┤
│  Rust Engine (invisible subprocess)         │
│  Event log, replay, crash recovery          │
│  Multiplexed: N agents, 1 process          │
├─────────────────────────────────────────────┤
│  File Storage (./data/)                     │
│  WAL with CRC-64, atomic writes            │
└─────────────────────────────────────────────┘
```

The Rust engine is managed automatically. You never interact with it
directly. The Python SDK handles all communication via NDJSON protocol
over stdin/stdout.

## Scaling

Durable is a single-machine runtime. It scales vertically:

- 100 concurrent agents on a single 8-core machine
- Suspended agents cost zero threads (just files on disk)
- The LLM API is the bottleneck, not the runtime
- A team spending $10K/month on OpenAI needs ~1 machine

For horizontal scaling, put a load balancer in front with sticky
sessions (route by execution_id). Each machine runs its own Runtime
with its own data directory.
