# Durable Runtime Examples

Crash-recoverable AI agents with exactly-once semantics. Every example
runs with a real LLM (OpenAI by default) and demonstrates a failure mode
that no other framework handles correctly.

## Setup

```bash
# Install the SDK
pip install durable-runtime

# Set your API key
export OPENAI_API_KEY=sk-...

# Run any example
python examples/01_quickstart.py
```

To use Anthropic instead of OpenAI:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

Then swap `OpenAI()` for `Anthropic()` in any example.

## Examples

| # | File | What it proves | Time |
|---|------|---------------|------|
| 01 | `01_quickstart.py` | Agent with tools, 15 lines of code | 10s |
| 02 | `02_crash_recovery.py` | Kill mid-execution, resume without double-charging | 20s |
| 03 | `03_human_in_the_loop.py` | Agent suspends for approval, resumes after | 15s |
| 04 | `04_budget_guardrails.py` | Agent suspends when cost limit is hit | 15s |
| 05 | `05_contract_enforcement.py` | Business rules that the LLM cannot violate | 15s |
| 06 | `06_cli_observability.py` | Run an agent, then inspect it with the CLI | 20s |

### Framework Integrations

| File | Framework | What it shows |
|------|-----------|---------------|
| `integrations/langchain_crash_recovery.py` | LangGraph | Crash-proof LangGraph with `DurableCheckpointer` |
| `integrations/crewai_resilient_research.py` | CrewAI | Multi-agent crew with memoized task results |
| `integrations/adk_persistent_assistant.py` | Google ADK | Persistent session state across restarts |

## What Makes This Different

**Every other agent framework loses state on crash.** Payment charged
but email never sent? Tool called twice? Conversation history gone?
These are not edge cases -- they are the default behavior of LangChain,
CrewAI, AutoGen, and every other framework.

Durable solves this with an append-only event log (a custom WAL store
with CRC-64 integrity checking). Every step result is persisted before
the next step begins. On crash recovery, completed steps return their
cached result -- the tool function is never called again.

### The five-second pitch

```
Without durable:
  charge_payment()  -->  crash  -->  restart  -->  charge_payment() again
  Result: customer charged twice

With durable:
  charge_payment()  -->  crash  -->  restart  -->  cached result returned
  Result: customer charged exactly once
```

## Running Without an API Key

Every example supports `--mock` mode for testing without an API key:

```bash
python examples/02_crash_recovery.py --mock
```

Mock mode uses a scripted LLM that returns predetermined tool calls.
The crash recovery and exactly-once guarantees work identically.

## After Running: Inspect With the CLI

```bash
# See all executions
durable status --data-dir ./demo-data

# Inspect a specific execution
durable inspect <execution-id> --data-dir ./demo-data

# Step-by-step timeline with timing
durable steps <execution-id> --data-dir ./demo-data

# Animated replay
durable replay <execution-id> --data-dir ./demo-data

# Cost breakdown
durable cost <execution-id> --data-dir ./demo-data
```
