# Durable Runtime

The SQLite of durable agent execution. Zero dependencies. Crash-recoverable. Exactly-once semantics.

```python
from durable import Agent, tool
from durable.providers import OpenAI

@tool("get_weather", description="Get weather for a location")
def get_weather(location: str) -> dict:
    return {"temp": 72, "conditions": "sunny", "location": location}

with Agent("./my-agent") as agent:
    agent.add_tool(get_weather)
    agent.set_llm(OpenAI())
    response = agent.run("What's the weather in San Francisco?")
    print(response)
```

## What It Does

Every LLM call and tool execution is a **durable step**. Results are memoized in an append-only event log. If the process crashes — between the payment charge and the confirmation email, between retry 3 and retry 4, between hour 1 and hour 47 of a long-running workflow — execution resumes exactly where it left off. No duplicate charges. No lost state. No re-execution of completed steps.

## Install

```bash
pip install durable-runtime
```

## Quick Start

### Ephemeral (in-memory, for testing)

```python
from durable import Agent, tool
from durable.providers import OpenAI

@tool("greet", description="Greet someone by name")
def greet(name: str) -> dict:
    return {"message": f"Hello, {name}!"}

with Agent("./data") as agent:
    agent.add_tool(greet)
    agent.set_llm(OpenAI())  # reads OPENAI_API_KEY from env
    print(agent.run("Greet Alice"))
```

### With Anthropic

```python
from durable.providers import Anthropic

agent.set_llm(Anthropic())  # reads ANTHROPIC_API_KEY from env
agent.set_llm(Anthropic(model="claude-opus-4-20250514"))
```

### Budget Limits

```python
from durable import Budget

with Agent("./data") as agent:
    agent.budget = Budget(max_dollars=2.00, max_llm_calls=10)
    agent.set_llm(OpenAI())
    response = agent.run("Process this batch")

    if response.is_suspended:
        print(f"Budget exhausted: {response.suspend_reason}")
        # Approve more budget and resume
```

### Agent Contracts

```python
with Agent("./data") as agent:
    @agent.contract("spending-limit")
    def check_spending(step_name, args):
        if args.get("amount", 0) > 100:
            raise ValueError("Charges over $100 need approval")

    agent.set_llm(OpenAI())
    agent.add_tool(charge_payment)
    response = agent.run("Charge $500")
    # response.is_suspended == True
    # response.suspend_reason.type == "contract_violation"
```

### Human-in-the-Loop

```python
@tool("transfer_funds", description="Transfer money", requires_confirmation=True)
def transfer_funds(from_acct: str, to_acct: str, amount: float) -> dict:
    return {"status": "transferred", "amount": amount}

with Agent("./data") as agent:
    agent.add_tool(transfer_funds)
    agent.set_llm(OpenAI())
    response = agent.run("Transfer $5000 from checking to savings")

    if response.is_suspended:
        # Agent paused before executing the transfer
        agent.approve(response.execution_id, response.suspend_reason.confirmation_id)
        response = agent.resume(response.execution_id)
        # Now the transfer executes
```

### Crash Recovery

```python
# First run — charges payment, crashes before sending email
response = agent.run("Process order #123")
# Process dies here

# Second run — payment step returns cached result (no double charge)
response = agent.resume(execution_id)
# Email sends, order completes
```

### Multi-Agent Coordination

```python
from durable import AgentCoordinator

coord = AgentCoordinator(event_store, storage)
coord.add_worker("research", [], lambda deps: research_task())
coord.add_worker("implement", ["research"], lambda deps: implement(deps["research"]))
coord.add_worker("verify", ["implement"], lambda deps: verify(deps["implement"]))

results = coord.execute(execution_id)
# On crash: completed workers skip, incomplete workers re-run
```

### Streaming

```python
for chunk in agent.stream("Tell me a story"):
    print(chunk, end="", flush=True)
```

### Testing

```python
from durable.testing import MockAgent

def test_my_agent():
    agent = MockAgent(responses=["The answer is 42."])
    response = agent.run("What is the answer?")
    assert response.text == "The answer is 42."
    assert agent.last_prompt == "What is the answer?"
```

## LLM Providers

| Provider | Import | Env Variable |
|----------|--------|-------------|
| OpenAI | `from durable.providers import OpenAI` | `OPENAI_API_KEY` |
| Anthropic | `from durable.providers import Anthropic` | `ANTHROPIC_API_KEY` |
| Custom | Any callable `(messages, tools, model) -> dict` | — |

### Custom Provider

```python
def my_llm(messages, tools=None, model=None):
    # Call any API
    return {"content": "response text"}
    # Or for tool calls:
    return {"tool_calls": [{"id": "1", "name": "tool", "arguments": {}}]}

agent.set_llm(my_llm)
```

## Framework Integrations

Add durability to your existing agents — one line, any framework.

### LangChain / LangGraph

```python
from langgraph.graph import StateGraph
from durable.integrations.langchain import DurableCheckpointer

compiled = graph.compile(checkpointer=DurableCheckpointer("./data"))

# Crash recovery is automatic — same thread_id resumes from checkpoint
result = compiled.invoke(
    {"messages": [HumanMessage(content="Process order #123")]},
    config={"configurable": {"thread_id": "order-123"}}
)
```

### CrewAI

```python
from crewai import Agent, Task
from durable.integrations.crewai import DurableCrew

crew = DurableCrew(
    agents=[researcher, writer],
    tasks=[research_task, write_task],
    data_dir="./data",
)

result = crew.kickoff(inputs={"topic": "AI safety"})
# Crash mid-task → next kickoff() resumes from last completed task
```

### Google ADK

```python
from google.adk.agents import LlmAgent
from durable.integrations.adk import durable_agent

agent = durable_agent(
    LlmAgent(name="OrderAgent", tools=[...], model=...),
    data_dir="./data",
)
# Durability is automatic — session state persists across crashes
```

Install with: `pip install durable-runtime[langchain]`, `durable-runtime[crewai]`, or `durable-runtime[adk]`.

## Architecture

```
┌─────────────────────────────────┐
│  Python SDK (your code)         │  pip install durable-runtime
├─────────────────────────────────┤
│  Protocol Client                │  Invisible — managed subprocess
├─────────────────────────────────┤
│  Rust Engine (durable-runtime)  │  Event log, replay, crash recovery
├─────────────────────────────────┤
│  File Storage                   │  ./my-agent/events/*.ndjson
└─────────────────────────────────┘
```

The Rust engine is a single binary managed as an invisible subprocess. The Python SDK communicates via NDJSON protocol over stdio. All durable state (event log, step memoization, crash recovery) lives in the Rust engine. Tools execute in your Python process.

## Production Hardening

### Authentication

The runtime binary supports token-based authentication. Unauthenticated commands are rejected.

```bash
# Binary with auth
durable-runtime --sdk-mode --auth-token my-secret

# Or via environment variable
export DURABLE_AUTH_TOKEN=my-secret
durable-runtime --sdk-mode
```

The Python SDK passes the token automatically when `DURABLE_AUTH_TOKEN` is set.

### Event Log Compaction

Long-running agents accumulate thousands of events. Compaction snapshots the current state and truncates old events, bounding file size and resume latency.

```python
# Compaction happens automatically via snapshot_interval (default: every 50 steps)
# Manual compaction is also available via the event store
```

### Safety Limits

The JSON parser enforces hard limits to prevent DoS:

| Limit | Default | Protects Against |
|-------|---------|-----------------|
| Max nesting depth | 128 | Stack overflow from `[[[[...]]]]` |
| Max string length | 10 MB | OOM from giant string values |
| Max elements | 100,000 | OOM from massive arrays/objects |

### Concurrency Protection

- File storage uses PID-unique temp files to prevent write races between processes
- Lease-based fencing ensures one worker per execution (TTL with automatic expiry)
- Multi-threaded SDK mode: concurrent agent executions don't block each other

### Structured Errors

Errors include execution context for production debugging:

```
step 'tool_charge' (#3) in exec abc-123 failed: connection timeout
```

## Engine Invariants

The runtime enforces seven invariants from the [durable execution specification](engine.md):

| Invariant | What It Guarantees |
|-----------|-------------------|
| Replay Determinism | Same inputs + same history = same execution |
| History Sufficiency | Any state can be reconstructed from the event log |
| Dependency Completeness | Steps run in correct order, independent steps parallelize |
| Suspension Transparency | Suspend/resume is invisible to workflow logic |
| Mutual Exclusion | One worker per execution at a time (lease-based fencing) |
| Error Classification | Every error classified as retry/fail/escalate |
| Configuration Completeness | Unconfigured runtime cannot be constructed |

## Zero Dependencies

The Rust engine uses only the standard library. The Python SDK uses only stdlib (`json`, `subprocess`, `urllib`, `threading`, `dataclasses`). No transitive dependency hell.

## License

MIT
