# Core Agent Runtime Invariants

A specification for evaluating and constructing systems that execute
long-running, stateful, failure-recovering agent workflows.

This document is implementation-agnostic. It defines the properties a
correct system must hold, not the mechanisms by which it holds them.
Any system claiming to solve this problem space can be graded against
these invariants. Any system being designed for this problem space
should treat them as non-negotiable constraints.

---

## Scope

An **agent runtime** is a system that accepts a declared workflow
(a graph of steps with side effects), executes it, persists enough
state to survive failures, and resumes execution after recovery
such that the workflow's observable behavior is indistinguishable
from an uninterrupted run.

The runtime may operate at any point on the concurrency spectrum:

```
Sequential ──── Concurrent ──── Parallel ──── Distributed
single-thread   single-thread   multi-thread   multi-process
one task at     interleaved     simultaneous   network boundary
a time          tasks           tasks          between workers
```

The invariants apply at every point. The cost of enforcement changes.
The algebra does not.

---

## Definitions

**Flow**: A declared composition of steps forming the unit of durable
execution. A flow is a pure function of its inputs and the history of
its completed steps. It contains no side effects of its own.

**Step**: A unit of work within a flow that may perform side effects
(I/O, network calls, sensor reads, API calls). A step's result, once
completed, is recorded and never re-executed on replay.

**History**: The append-only, ordered log of step completions for a
given flow instance. Each entry records the step's identity, its
serialized inputs, and its serialized output.

**Replay**: Re-execution of a flow from its initial state, using
recorded step results from history instead of re-executing steps.
Replay is the mechanism by which the runtime recovers from failure.

**Suspension**: A flow's voluntary yield of control while waiting for
an external condition (time, signal, child completion). The flow
resumes when the condition is met, at the exact point it yielded.

**Step Handle**: A deferred reference to a step's future result. The
handle carries the step's identity and dependency metadata. It is
resolved by the runtime according to the dependency graph, not by
the caller.

---

## Invariant I — Replay Determinism

> Given identical inputs and identical history, re-execution of a
> flow produces identical commands in identical order.

```
for all histories H, inputs I, times t1 t2:
  execute(I, H, t1) = execute(I, H, t2)
```

### What this requires

- Flow logic must be a pure function of its declared inputs and the
  results of its declared steps. No implicit inputs (wall-clock time,
  random values, environment variables, mutable global state).

- Any operation that reads external state or produces non-deterministic
  output must be isolated within a step, never within the flow
  orchestration.

- The boundary between deterministic orchestration and effectful steps
  must be enforced, not merely documented. The earlier the enforcement
  (compile time > load time > runtime), the stronger the guarantee.

### What violates it

- Reading wall-clock time in flow logic (not in a step).
- Generating random values in flow logic.
- Branching on mutable shared state not captured in history.
- Iterating over unordered collections (HashMap) to determine step
  execution order.
- Any implicit dependency on execution environment that may differ
  between the original run and a replay.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Non-determinism rejected at compile time via static analysis |
| Strong | Non-determinism rejected at load/registration time |
| Adequate | Non-determinism detected at replay time via command comparison |
| Weak | Non-determinism possible; detected only by symptom (wrong result) |
| Absent | No detection; replay silently diverges |

---

## Invariant II — History Sufficiency

> The history log is a complete and faithful record from which any
> prior execution state can be reconstructed by replay.

```
for all flows F, points in execution P:
  state(F, P) = replay(history(F)[0..P])
```

### What this requires

- Every step completion must be recorded with enough information to
  reconstruct its result: step identity, serialized input fingerprint,
  serialized output, and completion status.

- History is append-only. A completed entry is never modified or
  deleted while the flow is active. Compaction of completed flows is
  permitted after the flow reaches a terminal state.

- Each step completion must be **atomically durable** before the
  runtime proceeds to the next step. A crash between "step finished"
  and "result persisted" must not leave the history in a state where
  the step appears complete but its result is missing or corrupt.

- The identity of a step must be **stable across replays**. If step
  identity is derived from names, counters, or hashes, that derivation
  must produce the same identity on every replay of the same flow.

### What violates it

- Partial writes to the history log (crash between write and fsync).
- Storing step results in volatile memory without flushing to durable
  storage before proceeding.
- Using unstable identifiers (memory addresses, runtime-generated IDs)
  as step keys.
- Allowing history entries to be mutated after creation.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Atomic durable writes with stable identity and crash recovery proof |
| Strong | Durable writes with stable identity; crash recovery tested |
| Adequate | Durable writes; identity stable in practice but not proven |
| Weak | Volatile history with periodic checkpointing |
| Absent | No persistence; process death loses all state |

---

## Invariant III — Dependency Completeness

> A step executes only after every step it depends on has completed
> successfully. Steps with no dependency relationship may execute
> in any order, including concurrently.

```
for all steps S with declared predecessors P1..Pn:
  started(S) implies for all i: completed(Pi)
```

### What this requires

- The runtime must accept dependency declarations between steps
  (explicit edges, data-flow wiring, or both).

- The runtime must compute a valid execution order that respects all
  declared dependencies. For acyclic graphs, this is a topological
  ordering (Kahn's algorithm, DFS-based sort, or equivalent).

- The runtime must reject cycles at declaration time, not at
  execution time. A cycle means the flow is unsatisfiable.

- On replay after a crash, the runtime must determine which steps
  completed (from history) and resume from the correct point in the
  topological order. Completed steps are skipped; their results are
  read from history.

- Steps without a dependency relationship are independent. The
  runtime is not required to execute them concurrently, but it must
  not impose a false ordering. False ordering is a correctness
  property, not a performance property: it means the system would
  break if concurrency were introduced later.

### What violates it

- Executing a step before its predecessor completes.
- Imposing sequential execution where no dependency exists (unless
  this is an explicit, documented simplification with a clear
  upgrade path).
- Failing to detect cycles in the dependency graph.
- On replay, re-executing a step whose result is already in history.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | DAG with topological execution, cycle detection, concurrent independent steps, correct replay resume |
| Strong | DAG with topological execution and correct replay; concurrency optional but ordering correct |
| Adequate | Linear step chain with correct replay resume |
| Weak | Steps execute in declaration order; no dependency graph; replay restarts from beginning |
| Absent | No dependency tracking; no replay awareness |

---

## Invariant IV — Suspension Transparency

> A workflow that suspends and later resumes is observationally
> indistinguishable from one that never suspended. The workflow
> author's code sees a function call that returned a value; it has
> no knowledge of the elapsed time or intervening events.

```
for all flows F suspended at step S:
  resume(F, S, data) is equivalent to
  execute(F) where S returns data without suspension
```

### What this requires

- The runtime must support at least three suspension triggers:
  **time** (durable timer), **signal** (external event), and
  **child completion** (hierarchical flow).

- Suspension state must be persisted durably before the runtime
  yields control. A crash between "decided to suspend" and "recorded
  suspension" must not produce a flow that is neither running nor
  suspended.

- The wakeup condition (timer expiry, signal name, child flow ID)
  must be recorded with the suspension, so that recovery after a
  crash can re-register the wakeup without the flow re-executing.

- Resumption must deliver the result to the exact point in the flow
  where suspension occurred. The flow's continuation must not observe
  any side effects of the suspension mechanism.

- If the runtime supports concurrency, a signal or timer firing for
  one step must not interfere with concurrent execution of other
  steps in the same or different flows.

### What violates it

- Requiring the workflow author to handle suspension explicitly
  (polling loops, callback registration, manual state serialization).
- Losing suspension state on crash (flow becomes permanently stuck).
- Resuming at the wrong point (re-executing steps before the
  suspension point).
- Signal or timer handlers that mutate shared flow state without
  synchronization.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Transparent suspension with durable state for all three triggers; invisible to workflow author |
| Strong | Durable suspension for timers and signals; child completion handled but with some author awareness |
| Adequate | Durable suspension for one trigger type; others require explicit author handling |
| Weak | Suspension supported but not durable (lost on crash) |
| Absent | No suspension; flows must run to completion without yielding |

---

## Invariant V — Execution Mutual Exclusion

> At most one execution context is advancing a given flow instance
> at any point in time.

```
for all flows F, times t:
  |{context C : advancing(C, F, t)}| <= 1
```

### What this requires

- Before advancing a flow (executing a step, processing a signal,
  firing a timer), the runtime must ensure exclusive access to that
  flow's execution state.

- The mechanism depends on the concurrency model:
  - **Sequential**: Invariant is free. One task at a time.
  - **Concurrent (single-thread async)**: Async lock per flow, or
    serialized access via channel/queue.
  - **Parallel (multi-thread)**: OS-level or atomic synchronization
    per flow.
  - **Distributed (multi-process)**: Lease-based lock with TTL and
    stale lock recovery.

- If the runtime uses leases or time-bounded locks, it must handle
  **lease expiry during execution**. A step that takes longer than
  the lease TTL must not result in two contexts executing the same
  flow. Either: extend the lease, abort the step, or make step
  completion idempotent.

- Violation of this invariant corrupts history (Invariant II) by
  producing conflicting step records, which in turn breaks replay
  (Invariant I). This is a cascade failure.

### What violates it

- Two threads/tasks/workers executing steps in the same flow
  simultaneously.
- A signal handler advancing a flow while the executor is also
  advancing it.
- Lease expiry allowing a second worker to claim a flow while the
  first is still executing.
- Interrupt handlers modifying flow state without proper
  synchronization.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Mutual exclusion enforced at all concurrency levels the system supports, with lease/lock recovery |
| Strong | Mutual exclusion enforced; edge cases (lease expiry, interrupt reentry) documented and handled |
| Adequate | Mutual exclusion enforced for the common case; known edge cases with documented workarounds |
| Weak | Mutual exclusion assumed but not enforced (single-thread assumption in multi-thread runtime) |
| Absent | No exclusion; concurrent access to flow state is possible |

---

## Invariant VI — Error Classification Decidability

> For every error a step can produce, the runtime can determine a
> stable classification — retry, fail permanently, or escalate —
> and this classification does not change between the original
> execution and any replay.

```
for all errors E:
  classify(E) in {Retryable, Permanent, Escalate}
  and classify(E) at t1 = classify(E) at t2
```

### What this requires

- The runtime must provide a mechanism for error types to declare
  their classification. This may be a trait/interface, a registration
  table, or a policy object — but it must exist and must be consulted
  for every step failure.

- Three classification concerns must be **orthogonal** and
  independently configurable:
  1. **Policy**: How many times to retry and with what delay
     (max attempts, backoff strategy, delay bounds).
  2. **Classification**: Which errors are retryable and which are
     permanent (per error type or per error instance).
  3. **Override**: Per-step escape hatches that override the default
     classification (e.g., cache all errors, never retry this step).

- The default classification must be **safe**. If no classification
  is provided, the system must assume the error is retryable
  (transient). The dangerous case — incorrectly treating a permanent
  error as retryable — wastes retries but does not corrupt state. The
  opposite — treating a transient error as permanent — causes
  premature failure.

- Classification must be a **pure function of the error value**, not
  of external state. If classification depends on wall-clock time,
  random values, or mutable state, it becomes non-deterministic and
  breaks Invariant I during replay.

### What violates it

- No error classification mechanism (all errors treated identically).
- Classification that depends on runtime state (retry if under load,
  fail if over load).
- Retry logic embedded in step code (if/else around retry count)
  instead of declared as metadata.
- Classification that changes between execution and replay.

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Three-axis classification (policy, type, override), safe default, pure classification function |
| Strong | Policy and classification separated; safe default; override mechanism exists |
| Adequate | Retry policy exists; classification is per-step, not per-error-type |
| Weak | Retry exists but classification is ad-hoc or embedded in step logic |
| Absent | No retry; all errors are terminal |

---

## Invariant VII — Configuration Completeness

> The runtime cannot begin executing work in a state where any
> required configuration is missing or invalid. Incomplete
> configuration is rejected before any flow is accepted.

```
for all runtime instances R with required config K1..Kn:
  accepting_work(R) implies for all i: valid(Ki)
```

### What this requires

- The set of required configuration must be **enumerable**. The
  runtime must know what it needs before it starts, not discover
  missing configuration at arbitrary points during execution.

- Missing configuration must be **distinguishable from default
  configuration**. "Not set" and "set to the default value" are
  different states. The runtime must refuse to proceed on "not set."

- Validation must occur **before the first flow is accepted**. If
  the runtime accepts a flow and later discovers it cannot execute it
  due to missing configuration, recovery may be impossible (the flow
  is now in the history but cannot proceed).

- The strongest form of this invariant is **compile-time
  enforcement**: the type system prevents construction of a runtime
  instance that is missing required configuration. The runtime
  literally cannot exist in an unconfigured state.

### What violates it

- Runtime starts and accepts flows before storage is initialized.
- Missing configuration discovered mid-execution (lazy validation).
- Default values silently applied for configuration that has no
  sensible default (storage path, version identifier, worker identity).
- Configuration validated at a boundary that can be bypassed
  (checked in the CLI wrapper but not in the library API).

### Grading criteria

| Grade | Condition |
|-------|-----------|
| Full | Compile-time enforcement; unconfigured runtime cannot be constructed |
| Strong | Startup-time validation; runtime refuses to accept work until all config is valid |
| Adequate | Configuration validated but some defaults silently applied |
| Weak | Partial validation; some required config discovered lazily |
| Absent | No validation; missing config causes runtime errors during execution |

---

## Invariant Dependency Structure

The seven invariants form a directed acyclic graph of their own.
Violating a lower invariant breaks all invariants above it.

```
            ┌────────────────────────┐
            │  I. Replay Determinism │
            └───────────┬────────────┘
                        │ replay requires faithful history
            ┌───────────┴────────────┐
            │ II. History Sufficiency│
            └──┬─────────────────┬───┘
               │                 │
  ┌────────────┴───┐   ┌────────┴────────────┐
  │III. Dependency │   │IV. Suspension        │
  │  Completeness  │   │  Transparency        │
  └────────────────┘   └─────────────────────┘
        both read from and write to history
               │                 │
            ┌──┴─────────────────┴───┐
            │ V. Execution Mutual    │
            │    Exclusion           │
            └───────────┬────────────┘
              prevents conflicting writes
            ┌───────────┴────────────┐
            │VI. Error Classification│
            │   Decidability         │
            └───────────┬────────────┘
              must be stable for replay
            ┌───────────┴────────────┐
            │VII. Configuration      │
            │    Completeness        │
            └────────────────────────┘
              ensures the stack is wired
```

**Reading upward**: each invariant depends on the ones below it.
Mutual exclusion protects history from corruption. History
sufficiency enables replay. Replay determinism is the root guarantee.

**Reading downward**: configuration completeness is the foundation.
If the runtime is misconfigured, none of the above invariants can be
trusted.

---

## Applying This Specification

### For evaluating an existing system

1. For each invariant, determine the **mechanism** the system uses
   to enforce it (compile-time check, runtime check, convention,
   or nothing).

2. Assign a grade using the grading table.

3. Identify the **lowest-graded invariant**. Due to the dependency
   structure, this is the effective ceiling for the entire system.
   A system with Full marks on I-IV but Weak on V is effectively
   Weak — concurrent history corruption will eventually break replay.

4. For invariants graded Adequate or below, determine whether the
   gap is **architectural** (requires redesign) or **incremental**
   (can be improved without restructuring).

### For designing a new system

1. Start with the concurrency model. Determine where on the spectrum
   (sequential, concurrent, parallel, distributed) the system will
   operate. This determines the enforcement cost for each invariant.

2. Design the history format and storage first (Invariant II). Every
   other invariant depends on it. Get crash atomicity right before
   writing any execution logic.

3. Define the step boundary (Invariant I). Decide what constitutes
   a step, what constitutes flow logic, and how the boundary is
   enforced. The enforcement mechanism chosen here determines the
   system's long-term reliability ceiling.

4. Build the dependency graph (Invariant III) and suspension protocol
   (Invariant IV) on top of the history layer. These two are
   independent of each other and can be developed in parallel.

5. Layer mutual exclusion (Invariant V) appropriate to the chosen
   concurrency model. Do not over-engineer: if the system is
   single-threaded, a simple sequencing mechanism suffices. Do not
   under-engineer: if the system is concurrent, test for reentrant
   access to flow state.

6. Add error classification (Invariant VI) and configuration
   validation (Invariant VII) as cross-cutting concerns. These
   interact with all other invariants but do not depend on
   specific implementation choices in the layers above.

### For understanding a failure

When a system exhibits incorrect behavior, map the symptom to the
invariant it violates:

| Symptom | Likely violated invariant |
|---------|-------------------------|
| Different result after crash recovery | I. Replay Determinism |
| Step re-executes after crash despite prior completion | II. History Sufficiency |
| Step executes before its input is available | III. Dependency Completeness |
| Flow stuck after signal/timer should have resumed | IV. Suspension Transparency |
| Duplicate or conflicting step results | V. Mutual Exclusion |
| Permanent error retried indefinitely, or transient error fails immediately | VI. Error Classification |
| Runtime crashes on startup or accepts work it cannot complete | VII. Configuration Completeness |

Then trace the dependency graph downward. A symptom at level I may
have a root cause at level II or V. Fix the lowest violated invariant
first.

---

## Concurrency Cost Matrix

The enforcement cost for each invariant varies by concurrency model.
Use this matrix to calibrate design effort.

| Invariant | Sequential | Concurrent | Parallel | Distributed |
|-----------|-----------|------------|----------|-------------|
| I | Identical across all models — determinism is independent of concurrency |
| II | Append in order | Serialized async writes | Concurrent write with ordering | Distributed transaction or consensus |
| III | Trivial ordering | DAG + futures | DAG + spawn + channels | DAG + spawn + remote coordination |
| IV | Persist and return | Persist and yield to event loop | Persist and yield to thread pool | Persist and release worker |
| V | Free | Async lock per flow | OS lock or atomic per flow | Lease with TTL and recovery |
| VI | Identical across all models — classification is independent of concurrency |
| VII | Identical across all models — configuration is independent of concurrency |

Invariants I, VI, and VII are **concurrency-invariant**: their cost
does not change regardless of the execution model. Invariants II,
III, IV, and V are **concurrency-sensitive**: their enforcement
mechanism and cost scale with the concurrency model.

When moving a system along the concurrency spectrum (e.g., from
concurrent to parallel, or from parallel to distributed), only the
concurrency-sensitive invariants require redesign. The
concurrency-invariant properties carry over unchanged.
