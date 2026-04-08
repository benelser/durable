# delite

The SQLite of durable agent execution. Crash-recoverable AI agents with exactly-once semantics.

## Install

### Binary

```bash
brew install benelser/tap/delite          # macOS
cargo install delite-core                  # Rust toolchain
pip install delite-core                    # pip
npm install @bjelser/delite-core           # npm
curl -sSL https://raw.githubusercontent.com/benelser/durable/main/install.sh | sh
```

### Python SDK

```bash
pip install durable                        # pip
uv add durable                             # uv
poetry add durable                         # poetry
```

### TypeScript SDK (coming soon)

```bash
npm install durable                        # npm
bun add durable                            # bun
```

Set your LLM provider key:

```bash
export OPENAI_API_KEY=sk-...
# or
export ANTHROPIC_API_KEY=sk-ant-...
```

## Quick Start

```python
from durable import Agent, tool
from durable.providers import OpenAI

@tool("get_weather", description="Get weather for a location")
def get_weather(location: str) -> dict:
    return {"temp": 72, "conditions": "sunny", "location": location}

with Agent("./data") as agent:
    agent.add_tool(get_weather)
    agent.set_llm(OpenAI())
    response = agent.run("What's the weather in San Francisco?")
    print(response)
```

## What It Does

Every LLM call and tool execution is a **durable step**. Results are persisted to an append-only event log before the next step begins. If the process crashes — between the payment charge and the confirmation email, between retry 3 and retry 4, between hour 1 and hour 47 — execution resumes exactly where it left off.

```
Without delite:
  charge_payment()  →  crash  →  restart  →  charge_payment() again
  Result: customer charged twice ($299.94)

With delite:
  charge_payment()  →  crash  →  restart  →  cached result returned
  Result: customer charged once ($149.97)
```

## Crash Recovery

```python
# First run — everything executes and is persisted
response = agent.run("Process order #123")
execution_id = response.execution_id

# Process crashes. On restart, pass the same execution_id:
response = agent.run("Process order #123", execution_id=execution_id)
# All completed steps return cached results. No re-execution.
```

## Human-in-the-Loop

Mark any tool as requiring human approval before execution:

```python
@tool("transfer_funds", description="Transfer money", requires_confirmation=True)
def transfer_funds(from_acct: str, to_acct: str, amount: float) -> dict:
    return {"status": "transferred", "amount": amount}

response = agent.run("Transfer $5000 from checking to savings")
# response.is_suspended == True
# The tool has NOT executed. Human reviews and approves:
agent.approve(response.execution_id, response.suspend_reason.confirmation_id)
response = agent.resume(response.execution_id)
# Now the transfer executes
```

## Contracts

Contracts are checks that run before a tool executes. They are code, not prompt engineering — the LLM cannot circumvent them.

```python
@agent.contract("max-charge")
def check_charge(step_name, args):
    if "charge" in step_name and args.get("amount", 0) > 1000:
        raise ValueError("Charges over $1000 need VP approval")

response = agent.run("Charge $5000")
# response.is_suspended == True — the tool never executed
```

## Budget Limits

```python
from durable import Budget

agent.budget = Budget(max_dollars=2.00, max_llm_calls=10)
response = agent.run("Research this topic thoroughly")

if response.is_suspended:
    print(f"Budget exhausted: {response.suspend_reason}")
    # All completed work is preserved. Increase budget and resume.
```

## Multi-Agent Runtime

One runtime, N agents, running as durable threads inside your process:

```python
from durable import Runtime, Agent

rt = Runtime("./data")
researcher = Agent("./data", runtime=rt, agent_id="researcher", ...)
writer = Agent("./data", runtime=rt, agent_id="writer", ...)

# Non-blocking spawn
rt.go(researcher, "Research the topic")
rt.go(writer, "Write the report")

# Lifecycle callbacks
@rt.on_complete
def done(agent_id, exec_id, response):
    print(f"{agent_id} finished")

@rt.on_suspend
def paused(agent_id, exec_id, reason):
    send_slack_notification(f"Approval needed: {reason}")

# Signals trigger auto-resume
rt.signal(exec_id, confirmation_id, True)
```

## Idempotency Keys

Every tool callback includes a unique idempotency key. Forward it to payment providers to prevent double-charges:

```python
from durable.agent import current_idempotency_key

@tool("charge", description="Charge payment")
def charge(customer_id: str, amount: float) -> dict:
    stripe.PaymentIntent.create(
        amount=int(amount * 100),
        customer=customer_id,
        idempotency_key=current_idempotency_key(),
    )
    return {"status": "charged"}
```

## LLM Providers

| Provider | Python | Env Variable |
|----------|--------|-------------|
| OpenAI | `from durable.providers import OpenAI` | `OPENAI_API_KEY` |
| Anthropic | `from durable.providers import Anthropic` | `ANTHROPIC_API_KEY` |
| Custom | Any callable `(messages, tools, model) -> dict` | — |

## Streaming

```python
for chunk in agent.stream("Tell me a story"):
    print(chunk, end="", flush=True)
```

## CLI

Inspect any execution after it runs:

```bash
delite status --data-dir ./data                    # list all executions
delite inspect <execution-id> --data-dir ./data    # detailed view
delite steps <execution-id> --data-dir ./data      # step timeline
delite events <execution-id> --data-dir ./data     # raw event log
delite export <execution-id> --data-dir ./data     # JSON export
delite health --data-dir ./data                    # storage health
```

## How delite Compares

### vs LangGraph

LangGraph has checkpointing (save/restore graph state via SQLite, Postgres, etc). This gives you conversation resume and human-in-the-loop interrupts.

What it does NOT give you: **exactly-once tool execution.** LangGraph checkpoints at node boundaries, not within nodes. If a tool executes and the process crashes before the next checkpoint, the tool re-executes on resume.

delite persists every step individually before the next begins. Prompt and tool drift are detected on resume.

### vs Temporal

Temporal is the gold standard: multi-machine clusters, workflow versioning, visibility UI, battle-tested in production. It requires a Temporal Server cluster, separate worker processes, and an infrastructure team.

delite is for teams that want those guarantees without operating distributed infrastructure. One process, one binary, files on disk. If you outgrow single-machine, Temporal is the right next step.

## Architecture

```
┌─────────────────────────────────────┐
│  Your code (Python, TypeScript)     │
├─────────────────────────────────────┤
│  SDK (zero dependencies)            │
├─────────────────────────────────────┤
│  Rust Engine (invisible subprocess) │
├─────────────────────────────────────┤
│  Event Log (append-only files)      │
└─────────────────────────────────────┘
```

Single-process. The Rust engine runs as an invisible subprocess. The LLM API is the bottleneck, not the runtime — a single machine handles more concurrent agents than most teams can afford in API costs.

## Zero Dependencies

The Rust engine uses only the standard library. The Python SDK uses only stdlib (`json`, `subprocess`, `urllib`, `threading`). No transitive dependency hell.

## License

MIT
