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

### Multi-Agent Runtime

```python
from durable import Runtime, Agent

rt = Runtime("./data")
agent_a = Agent("./data", runtime=rt, agent_id="research-bot", ...)
agent_b = Agent("./data", runtime=rt, agent_id="writer-bot", ...)

# Non-blocking spawn — agents run as durable threads
exec_a = rt.go(agent_a, "Research the topic")
exec_b = rt.go(agent_b, "Write the report")

# Lifecycle callbacks
@rt.on_complete
def done(agent_id, exec_id, response):
    print(f"{agent_id} finished: {response[:50]}")
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

## Framework Integrations (experimental)

Basic checkpoint persistence for existing frameworks. These use a
Python file backend — not the Rust engine. They provide task-level
crash recovery (resume from last completed task) but NOT the step-level
exactly-once guarantees of native Durable agents.

For full durable execution guarantees (exactly-once tool calls, WAL
integrity, replay determinism), use the native `Agent` + `Runtime` API.

### LangChain / LangGraph

```python
from langgraph.graph import StateGraph
from durable.integrations.langchain import DurableCheckpointer

compiled = graph.compile(checkpointer=DurableCheckpointer("./data"))

# Same thread_id resumes from last checkpoint
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

The runtime binary supports token-based authentication via CLI flag:

```bash
durable-runtime --sdk-mode --auth-token my-secret
```

> **Note:** The Python SDK does not yet pass auth tokens automatically.
> This is planned for a future release.

### Event Log Compaction

Long-running agents accumulate events. The Rust engine takes periodic
snapshots for fast resume — on resume, it loads the latest snapshot and
replays only subsequent events instead of replaying the full history.

Default interval: every 50 steps. Most agents complete in under 50 steps
and never snapshot. Long-running agents (100+ steps) get snapshots that
keep resume latency under 5ms regardless of history length. Configurable
via `AgentConfig.snapshot_interval` (set to 0 to disable).

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

## How Durable Compares

### vs LangGraph

LangGraph has checkpointing — you can save and restore graph state via
a checkpointer backend (memory, SQLite, Postgres). This gives you:
- Resume a conversation from where it left off
- Branch and replay from any checkpoint
- Human-in-the-loop interrupts via `interrupt_before`/`interrupt_after`

What LangGraph checkpointing does NOT give you:
- **Exactly-once tool execution.** LangGraph's standard `StateGraph`
  checkpoints at node boundaries, not within nodes. If a node calls a
  tool and the process crashes after the tool executes but before the
  next checkpoint is saved, the tool re-executes on resume. LangGraph's
  Functional API offers a `@task` decorator designed to cache sub-node
  results, but it only works in the Functional API (not `StateGraph` or
  `create_react_agent`), has known deployment issues on the LangGraph
  API server, and LangGraph's own docs recommend designing all side
  effects to be idempotent "in case of re-execution."
- **Replay determinism enforcement.** LangGraph doesn't detect if your
  tools or prompts changed between checkpoint and resume.

Durable provides step-level memoization: every step (LLM call and tool
call) is individually persisted to an append-only event log BEFORE the
next step begins. The event log is the source of truth, not in-memory
state. Prompt drift and tool drift are detected and rejected on resume.

### vs Temporal

Temporal is the gold standard for durable execution. It provides
everything Durable does and more: multi-machine clusters, workflow
versioning, schedules, visibility UI, and battle-tested production
hardening.

What Temporal requires that Durable doesn't:
- A Temporal Server cluster (3+ nodes for production)
- Worker processes separate from your application
- Workflow code written in Temporal's SDK patterns
- Infrastructure team to manage the cluster

Durable is for teams that want durable execution guarantees without
operating distributed infrastructure. One process, one binary, files
on disk. If you outgrow single-machine, Temporal is the right next step.

## Zero Dependencies

The Rust engine uses only the standard library. The Python SDK uses only stdlib (`json`, `subprocess`, `urllib`, `threading`, `dataclasses`). No transitive dependency hell.

## License

MIT
