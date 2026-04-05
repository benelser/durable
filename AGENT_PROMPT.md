# Durable AI Agent Runtime — Implementation Prompt

## What You Are Building

A Rust-based runtime and harness for durable AI agent execution. The runtime must be **language-agnostic** — agents written in any language can execute within it. It runs as a **single embedded process** (no distributed cluster required, though the design should not preclude distribution later).

The runtime provides **durable execution guarantees** for AI agent loops: if the process crashes mid-conversation, mid-tool-call, or mid-reasoning, execution resumes from the last completed step without re-executing side effects.

## Core Problem

AI agents are stateful, long-running processes that call LLMs, invoke tools, wait for human input, and maintain conversation history. All of these are fragile: networks fail, processes crash, rate limits hit, machines restart. Without durability, a crash mid-agent-loop means lost conversation state, duplicated tool calls (including ones with real-world side effects like sending emails or charging cards), and broken user experiences.

The runtime must make agent execution **survivable** — any step that completed stays completed, and execution picks up exactly where it left off.

## Foundational Patterns (Distilled from Reference Material)

### 1. Deterministic Orchestration / Non-Deterministic Execution Split

The core architectural insight: separate **orchestration logic** (what to do, in what order) from **effectful execution** (LLM calls, tool invocations, API requests, I/O).

- Orchestration code must be **deterministic** — given the same inputs and cached results, it produces the same sequence of decisions. This enables replay.
- All non-deterministic work (LLM inference, HTTP calls, database queries, file I/O, tool execution) happens in explicitly marked **steps/activities** whose results are persisted.
- On replay after a crash, the orchestrator re-runs but steps with cached results return immediately from the cache instead of re-executing.

### 2. Step Memoization via Content-Addressed Caching

Each step execution is identified by a composite key: `(execution_id, step_number, hash_of_parameters)`.

- If a step is encountered during replay and the cache contains a result for that key, return the cached result.
- If parameters have changed (hash mismatch), re-execute the step.
- This provides **idempotency** — the same step with the same inputs always returns the same result without re-execution.
- Parameter hashing must be deterministic and fast.

### 3. The Agent Loop as a Durable Workflow

An AI agent's core loop maps naturally to a durable workflow:

```
loop {
    1. Wait for user input          → suspends execution, resumes on signal
    2. Validate input               → step (may call LLM)
    3. Call LLM with context        → step (non-deterministic, must be cached)
    4. Parse LLM response           → deterministic orchestration
    5. If tool call requested:
       a. Optionally confirm        → suspend, wait for human signal
       b. Execute tool              → step (side-effectful, must be cached)
       c. Append result to context  → deterministic orchestration
    6. If done, return result
    7. Otherwise, loop back to 3
}
```

Each numbered item that involves I/O is a **step**. The overall loop is **orchestration**. The conversation history is accumulated deterministically from step results.

### 4. Suspension and Resumption

Agents must be able to **suspend** execution and **resume** later. Suspension triggers include:

- **Waiting for human input** (human-in-the-loop confirmation, clarifying questions)
- **Waiting for a timer** (rate limit backoff, scheduled checks)
- **Waiting for an external signal** (webhook, callback, another agent completing)

Suspension must be:
- Detectable immediately (not via polling/timeouts)
- Persistable (the reason and state survive restarts)
- Resumable (when the condition is met, execution continues from the suspension point)

### 5. Tool Abstraction

Tools are the agent's interface to the outside world. The runtime needs:

- **Tool definitions**: name, description, parameter schema (for LLM function-calling)
- **Tool handlers**: the actual executable logic, mapped by name
- A **registry** that separates tool metadata (what the LLM sees) from tool implementation (what runs)

This separation is critical for language-agnostic design — tool definitions are data (JSON schema), tool handlers can be in any language as long as they conform to the execution protocol.

### 6. DAG-Based Parallelization

When multiple steps have no data dependencies, they should execute in parallel automatically:

- Steps declare their inputs (which may be outputs of other steps)
- The runtime builds a dependency graph
- Independent steps execute concurrently
- Dependent steps wait for their inputs
- Results propagate through the graph

This is important for agent tool calls — if an LLM requests multiple independent tool calls (parallel function calling), they should execute concurrently.

### 7. Error Handling and Retry

Steps can fail transiently or permanently:

- **Transient failures** (network timeout, rate limit): retry with configurable backoff
- **Permanent failures** (invalid input, authorization denied): fail immediately
- Each error should carry a **retryability** signal
- Retry policies should be configurable per-step: max attempts, initial delay, max delay, backoff multiplier
- Error results should be cacheable too (for steps where failure is the correct outcome to record)

### 8. Human-in-the-Loop Gates

Before executing certain tools (especially those with real-world side effects), the runtime should support **confirmation gates**:

- The agent proposes an action
- Execution suspends until a human approves or rejects
- On approval, the tool executes
- On rejection, the agent receives the rejection as context and adapts

This is a specialization of the suspension pattern.

### 9. Conversation History as Accumulated State

The agent's conversation history is not stored as a separate blob — it is the **deterministic accumulation of step results**:

- Each LLM call step returns a response that gets appended to history
- Each tool result gets appended to history
- Each user input signal gets appended to history
- On replay, history is reconstructed by replaying cached step results

This means conversation state is **derived**, not separately persisted.

### 10. Language-Agnostic Execution Protocol

The runtime must support agents and tools written in any language. Key design constraints:

- Steps communicate via **serialized payloads** (the format is a design decision — JSON, protobuf, msgpack, etc.)
- Tool definitions are **data** (schema), not code
- Tool execution crosses a **process boundary** or uses an **embedded interpreter** or **FFI** — the mechanism is a design decision, but the interface must be language-neutral
- The orchestration protocol (start flow, execute step, suspend, resume, signal) must be expressible as a wire protocol or API, not just Rust function calls

### 11. Storage Abstraction

The persistence layer must be pluggable:

- An abstract trait/interface for all storage operations
- At minimum: an in-memory backend (for testing/development) and a durable backend (for production)
- Operations: log step start/completion, retrieve cached results, manage timers, manage signals, enqueue/dequeue flows
- Storage must support atomic operations (scheduling a flow, completing a step) to prevent race conditions

### 12. Observability

The runtime should expose execution state for inspection:

- What step is currently executing
- What steps have completed and their results
- What the agent is suspended on
- Conversation history
- Query mechanisms that don't mutate state

## Reference Material

The following source code is available for architectural inspiration. Study the patterns, not the specific implementations:

### `/Users/belser/ventures/durable/source/ergon/`
Rust durable execution framework (~17.5K lines). Key patterns to study:
- **Macro-based DSL**: `#[flow]`, `#[step]`, `#[dag]` macros generate orchestration boilerplate
- **ExecutionContext**: task-local state with atomic step counter, dependency graph, suspension detection
- **ExecutionLog trait**: storage abstraction with SQLite, Redis, Postgres, in-memory backends
- **Parameter hashing**: SeaHash for content-addressed step caching
- **Suspension model**: atomic flag detection, timer/signal persistence, suspension result caching
- **Worker/Scheduler**: distributed execution with type-safe dispatch, versioned scheduling
- **Retry policies**: configurable per-step, with Retryable trait for error classification
- **DAG execution**: DeferredRegistry builds dependency graph, topological sort, parallel execution via tokio::spawn + oneshot channels
- **Child flows**: parent-child relationships with automatic signaling on completion

### `/Users/belser/ventures/durable/source/temporal/`
Go distributed durable execution platform. Key patterns to study:
- **Event sourcing**: immutable append-only event history for workflow execution
- **Frontend/History/Matching/Worker service split**: separation of concerns in distributed mode
- **Protobuf service definitions**: language-agnostic API surface
- **Chasm (Coordinated Heterogeneous Application State Machines)**: state machine orchestration pattern
- **History replay**: deterministic replay from event log
- **Task queue matching**: routing work to appropriate workers

### Temporal AI Agent Tutorial Patterns (from https://learn.temporal.io/tutorials/ai/durable-ai-agent/)
- **Workflow-as-orchestrator**: agent loop lives in a workflow, all I/O in activities
- **Dynamic tool activity**: single activity handler dispatches to tool registry by name
- **Validation activity**: LLM-based input validation before processing
- **Confirmation flow**: environment-controlled confirmation gates before tool execution
- **Signal-driven input**: user messages arrive as signals to the running workflow
- **Conversation history threading**: structured history passed between workflow steps
- **Mock/live tool modes**: graceful degradation when external APIs unavailable

## Constraints

- The runtime is written in **Rust**
- It must run as a **single embedded process** (no separate server required)
- It must be **language-agnostic** for agent and tool authoring
- It draws inspiration from the reference material but is its own thing — not a port, fork, or wrapper

## What Success Looks Like

An agent author in any language can:
1. Define a set of tools (name, description, parameter schema, handler)
2. Define an agent loop (system prompt, model config, tool selection logic)
3. Run the agent through this runtime
4. Have the runtime guarantee that crashes, restarts, and failures don't lose state or duplicate side effects
5. Have human-in-the-loop confirmation gates where needed
6. Have the agent suspend and resume across process restarts
7. Observe the agent's execution state at any point
