# Durable Execution Validation Rubric

Use this rubric to grade the implementation against established durable execution principles. Each principle has specific tests and acceptance criteria. Grade each A through F.

---

## 1. IMMUTABLE EVENT LOG

### What to check
- [ ] All state changes are recorded as immutable, append-only events
- [ ] No mutation of previously written records (no read-modify-write on events)
- [ ] Current state can be reconstructed by folding over the event sequence
- [ ] Signals are events (appended), not files that get deleted
- [ ] Execution status is derived from the latest event, not a mutable field
- [ ] Every event has: `event_id`, `execution_id`, `timestamp`, `event_type`, `data`
- [ ] Events survive process crashes (fsync after append)

### Tests to run
1. **Append and reconstruct**: Create execution, run 5 steps, reconstruct state from events alone — state matches live state
2. **Crash mid-append**: Kill process after event write but before next operation — events are intact on restart
3. **Point-in-time query**: Replay events up to event N, verify state matches what it was at event N
4. **Signal as event**: Send a signal, verify it appears in the event log, verify consuming the signal appends a new event (not deleting the old one)

### Grading
- **A**: All events immutable, full state reconstructable, crash-safe, point-in-time queries work
- **B**: Events immutable but some metadata still mutable (e.g., tags)
- **C**: Most state changes are events but some are still mutations
- **D**: Partial event log alongside mutable state
- **F**: No event log; mutable state files

---

## 2. DETERMINISTIC REPLAY

### What to check
- [ ] On resume, the runtime replays through the event log step by step
- [ ] Each replayed step is validated: `(step_number, step_name)` must match the event
- [ ] If a mismatch is detected, the runtime fails with `NonDeterminismDetected`
- [ ] Replay produces identical side-effect-free decisions as the original run
- [ ] Code that has non-deterministic operations (random, time, external I/O) encapsulates them in memoized steps
- [ ] Adding/removing/reordering steps in agent code between runs is detected, not silently accepted

### Tests to run
1. **Happy path replay**: Suspend at step 5, resume, verify steps 0-4 are replayed from cache (not re-executed)
2. **Step name mismatch**: Suspend, then resume with code that produces a different step name at the same step number — must fail with `NonDeterminismDetected`
3. **Step count mismatch**: Suspend at step 5, resume with code that produces 6 steps before reaching the suspension point — must fail
4. **Conditional branch stability**: Agent code has `if config { step_a }; step_b` — verify that changing `config` between runs fails replay
5. **Idempotent replay**: Replay 3 times in a row — same result every time, no extra side effects

### Grading
- **A**: Strict validation on replay, loud failure on mismatch, provides versioning/patching API for safe code changes
- **B**: Validates step names but not step count, or vice versa
- **C**: Detects some mismatches but silently re-executes in some cases
- **D**: No replay validation; silently re-executes on mismatch
- **F**: Replay is broken by code changes

---

## 3. EXACTLY-ONCE SEMANTICS

### What to check
- [ ] Each step has a fencing token / generation number
- [ ] Step completion writes are rejected if the generation doesn't match (optimistic concurrency)
- [ ] Two concurrent workers cannot both complete the same step
- [ ] Step results are written atomically (no partial writes visible)
- [ ] Re-executing a step after a crash returns the same result (from cache, not re-running)

### Tests to run
1. **Fencing token**: Start execution with generation 1, complete step. Try to complete same step with generation 0 — must be rejected
2. **Concurrent completion race**: Two threads both try to complete step 5 simultaneously. Exactly one succeeds; the other gets `StaleGeneration`
3. **Crash recovery idempotency**: Run step, simulate crash after completion but before consumer reads result. Resume — step returns cached result, not re-executed
4. **Cross-process fencing**: Process A holds generation 3. Process B (stale) tries to write with generation 2 — rejected

### Grading
- **A**: Full fencing with generation numbers, atomic CAS on step completion, race-proof
- **B**: Fencing exists but some edge cases (e.g., timer steps) aren't fenced
- **C**: Atomic writes but no generation/fencing — relies on single-process assumption
- **D**: Read-check-write without atomicity
- **F**: No protection against duplicate execution

---

## 4. SAGA / COMPENSATION

### What to check
- [ ] Steps can register compensation handlers (undo functions)
- [ ] On failure after N successful steps, compensations run in reverse order
- [ ] Compensation handlers are themselves durable (memoized, retried)
- [ ] Compensation failures are tracked separately
- [ ] The runtime provides a `step_with_compensation(do_fn, undo_fn)` API

### Tests to run
1. **Happy path**: Steps 1-3 succeed, step 4 fails. Compensations for 3, 2, 1 run in that order. Verify each compensation executed exactly once.
2. **Compensation is durable**: Start compensation, crash mid-way, resume. Remaining compensations execute; already-completed ones don't re-run.
3. **Partial compensation failure**: Compensation for step 2 fails (retryable). Verify it's retried. Then compensation for step 1 runs.
4. **No compensation needed**: All steps succeed. No compensations run.

### Grading
- **A**: Full saga with durable compensations, reverse-order execution, retry support
- **B**: Compensation API exists but compensations aren't durable (not memoized)
- **C**: Manual compensation (developer rolls back in catch block, no framework support)
- **D**: Failure tracking but no compensation mechanism
- **F**: No compensation — failed executions leave side effects unreversed

---

## 5. WORKFLOW VERSIONING

### What to check
- [ ] Every execution records the code version that created it
- [ ] On resume, the runtime checks if the current code version matches the execution's version
- [ ] Version mismatch is either handled (migration) or rejected (loud failure)
- [ ] Multiple versions can coexist (old executions finish with old code, new executions use new code)
- [ ] There's a migration path for changing step structure between versions
- [ ] A `get_version("change-id", min, max)` or equivalent patching API exists

### Tests to run
1. **Version recorded**: Create execution, verify metadata contains version string
2. **Version mismatch detection**: Create execution with v1, try to resume with v2 code — fails with `VersionMismatch`
3. **Compatible resume**: Create with v1, resume with v1 — succeeds
4. **Patching API**: Use `get_version` in agent code to branch between old and new behavior. Old executions take the old branch, new executions take the new branch. Both complete correctly.
5. **Version routing**: Two versions registered. New execution goes to v2. Old suspended execution resumes on v1.

### Grading
- **A**: Full version routing, patching API, coexistence of multiple versions, migration helpers
- **B**: Version tracked and checked, but no routing (all code must handle all versions)
- **C**: Version stored but only logged/warned, not enforced
- **D**: Version field exists but unused
- **F**: No versioning

---

## 6. IDEMPOTENCY KEYS

### What to check
- [ ] Step identity uses `(step_number, step_name, full_params)` — not just a hash
- [ ] Hash is used for fast lookup; full parameter comparison for verification
- [ ] Same step with same params always returns the same cached result
- [ ] Same step with different params detects the difference and re-executes
- [ ] Changing JSON serialization format doesn't break cache lookups

### Tests to run
1. **Cache hit**: Execute step with params `{a: 1}`, re-execute with same params — returns cached result, handler not called
2. **Cache miss on param change**: Execute with `{a: 1}`, re-execute with `{a: 2}` — handler called again
3. **Full param verification**: Craft two different param sets that hash to the same u64 (if possible). Verify the runtime distinguishes them via full comparison.
4. **Serialization stability**: Serialize `{b: 2, a: 1}` and `{a: 1, b: 2}` — both produce the same cache key (BTreeMap ordering)

### Grading
- **A**: Full parameter storage + comparison, hash for fast path, deterministic serialization
- **B**: Full parameter storage but no fast-path hash (always compares full bytes)
- **C**: Hash-only with collision detection (store params, check on hit)
- **D**: Hash-only without collision detection
- **F**: No caching or broken caching

---

## 7. BACKPRESSURE & FLOW CONTROL

### What to check
- [ ] Thread pool has a fixed maximum size
- [ ] Worker has max concurrent execution limit
- [ ] A slow tool call doesn't starve other executions
- [ ] Queue depth is bounded (not unbounded growth)
- [ ] Under overload, the system degrades gracefully (rejects new work) rather than OOMing
- [ ] Priority support exists (or at least FIFO fairness)

### Tests to run
1. **Pool saturation**: Submit 100 tasks to a pool of 4. Verify at most 4 run concurrently. All 100 eventually complete.
2. **Worker backpressure**: Set `max_concurrent: 2`. Submit 10 executions. Verify only 2 run at a time.
3. **Slow tool isolation**: One tool sleeps for 5 seconds. Other tools with 100ms latency should complete promptly (not blocked by the slow one).
4. **Graceful degradation**: Fill the system to capacity. Verify new submissions get a clear "at capacity" response, not a hang or crash.

### Grading
- **A**: Bounded pools, per-execution limits, priority queues, graceful rejection under overload
- **B**: Bounded pools and limits but no priority or graceful rejection
- **C**: Bounded pools but no per-execution limits
- **D**: Some limits but unbounded in key paths
- **F**: Unbounded thread/memory growth under load

---

## 8. CONSISTENCY MODEL

### What to check
- [ ] Single-event appends are atomic (fsync + rename or equivalent)
- [ ] Multi-event transactions are atomic (signal + status update in one operation)
- [ ] Read-after-write is guaranteed within the same process
- [ ] Cross-process reads see a consistent snapshot (no torn reads)
- [ ] No partial state is visible to observers during writes

### Tests to run
1. **Atomic event append**: Write an event, crash immediately after. On restart, the event is either fully present or fully absent (no partial JSON).
2. **Transaction atomicity**: Store signal and update status as one transaction. Crash between — either both are visible or neither.
3. **Read-after-write**: Write event, immediately read — see the written event (not stale)
4. **Observer consistency**: While one thread writes, another reads via `ExecutionInspector` — sees a consistent state (no half-written steps)

### Grading
- **A**: All operations atomic (event log), transactions supported, linearizable within process
- **B**: Single operations atomic, multi-operation transactions via journal/WAL
- **C**: Single file operations atomic, multi-file operations not atomic
- **D**: Atomic writes but no consistency across files
- **F**: Non-atomic writes, torn reads possible

---

## OVERALL SCORE CALCULATION

| Principle | Weight | Current Grade |
|-----------|--------|---------------|
| 1. Event Log | 20% | ___ |
| 2. Deterministic Replay | 20% | ___ |
| 3. Exactly-Once | 15% | ___ |
| 4. Saga/Compensation | 10% | ___ |
| 5. Versioning | 10% | ___ |
| 6. Idempotency Keys | 10% | ___ |
| 7. Backpressure | 10% | ___ |
| 8. Consistency | 5% | ___ |

**Target: All A's.**

To convert: A=4.0, B=3.0, C=2.0, D=1.0, F=0.0
Weighted GPA of 3.5+ = production-ready. Below 2.0 = prototype only.

---

## HOW TO USE THIS RUBRIC

After implementing each phase from the remediation plan:

1. Run the specific tests listed for that principle
2. Check off the acceptance criteria
3. Assign the grade
4. If any principle is below B: stop and fix before moving to the next phase

The tests in this rubric should be implemented as actual integration tests in `tests/validation.rs`. Each test name should correspond to a rubric item (e.g., `test_event_log_append_and_reconstruct`, `test_replay_step_name_mismatch`, `test_fencing_stale_generation_rejected`).
