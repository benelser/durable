# Integration Architecture Guide

How to build and maintain delite integrations for agent frameworks.

## Core Principle

Every agent framework has the same fundamental problem: if the process
dies mid-execution, work is lost. They all solve it differently:

| Framework | State Persistence | Crash Recovery |
|-----------|------------------|----------------|
| LangGraph | Checkpointers (pluggable) | Replay from checkpoint |
| CrewAI | Memory system (Chroma + sqlite) | None — restart from scratch |
| Google ADK | Session state (managed) | Partial — session survives, steps don't |
| **delite** | Event-sourced log | Full — replay any execution, exactly-once |

Our integration strategy: **plug into each framework's existing persistence
point and add durability guarantees they don't have natively.**

## The deliteBackend Abstraction

All integrations share `deliteBackend` — a file-based persistence layer:

```
deliteBackend
├── save_checkpoint(thread_id, state) → checkpoint_id
├── load_checkpoint(thread_id) → state
├── record_step(thread_id, name, index, result)
├── get_step(thread_id, name, index) → result
├── record_usage(thread_id, tokens, cost)
├── get_usage(thread_id) → usage_dict
├── save_writes(thread_id, cp_id, writes)
└── load_writes(thread_id, cp_id) → writes
```

**Storage layout:**
```
{data_dir}/
  threads/
    {thread_id}/
      checkpoints/
        latest.json              ← pointer to latest checkpoint
        cp-1712345678000.json    ← checkpoint state
        cp-1712345679000.json
      steps/
        000000_llm_call.json     ← memoized step results
        000001_tool_search.json
      writes/
        cp-xxx.json              ← pending writes (LangGraph)
      usage.json                 ← cumulative token/cost tracking
```

## Integration Patterns

### Pattern 1: Checkpointer (LangGraph)

LangGraph's `BaseCheckpointSaver` is the cleanest integration point.
The graph automatically calls our methods at super-step boundaries.

```
Graph.invoke()
  → node executes
  → checkpointer.put(state)        ← WE SAVE HERE
  → next node executes
  → checkpointer.put(state)        ← WE SAVE HERE
  → ...

Graph.invoke() (after crash)
  → checkpointer.get_tuple()       ← WE RESTORE HERE
  → resume from last checkpoint
```

**Key insight:** LangGraph designed this for exactly our use case.
We're implementing an interface they intended to be extended.

### Pattern 2: Callback Wrapper (CrewAI)

CrewAI doesn't have a checkpointer interface. We wrap `Crew` and
inject callbacks to track task completion.

```
deliteCrew.kickoff()
  → check for existing checkpoint
  → if checkpoint: skip completed tasks
  → Crew.kickoff(remaining_tasks, task_callback=our_callback)
    → task completes
    → our_callback(result)            ← WE SAVE HERE
    → next task
    → ...
```

**Key insight:** CrewAI's task-level granularity is coarser than
LangGraph's step-level. A crash mid-task means the entire task
re-executes. This is a framework limitation, not ours.

### Pattern 3: Lifecycle Hooks (Google ADK)

ADK's `before_agent_callback` / `after_agent_callback` give us
read/write access to session state at each agent turn.

```
AdkApp.stream_query()
  → before_agent_callback(ctx)     ← WE LOAD STATE HERE
  → agent processes (LLM + tools)
  → after_agent_callback(ctx)      ← WE SAVE STATE HERE
  → next turn
  → ...
```

**Key insight:** ADK's callback context includes mutable state.
We can inject checkpoint data directly into the session without
the framework knowing.

## Adding a New Framework Integration

1. **Identify the persistence point:**
   - Does it have a checkpointer interface? → Pattern 1
   - Does it have callbacks? → Pattern 2 or 3
   - Neither? → Wrap the main execution method

2. **Implement using deliteBackend:**
   ```python
   from delite.integrations._base import deliteBackend

   class deliteMyFramework:
       def __init__(self, data_dir="./data"):
           self._backend = deliteBackend(data_dir)

       def run(self, ...):
           # Check for checkpoint
           checkpoint = self._backend.load_checkpoint(thread_id)
           if checkpoint:
               # Resume from checkpoint
               ...

           # Execute normally, recording steps
           result = framework.execute(...)
           self._backend.record_step(thread_id, "step_name", 0, result)
           self._backend.save_checkpoint(thread_id, state)
           return result
   ```

3. **Handle graceful import failure:**
   ```python
   try:
       from myframework import BaseClass
       _HAS_FRAMEWORK = True
   except ImportError:
       _HAS_FRAMEWORK = False
       class BaseClass:  # type: ignore
           pass
   ```

4. **Add to COMPATIBILITY.md:**
   - Upstream API surface we depend on
   - Version compatibility matrix
   - Breaking change risks
   - Source links for tracking

5. **Write an example** that demonstrates the "wow factor":
   - Show the framework without delite (works, but fragile)
   - Show the same workflow with delite (crash-proof)
   - Include side-effect counting to prove exactly-once

## Version Compatibility Strategy

We depend on **abstract interfaces**, not implementations. This minimizes
breaking changes:

- LangGraph: `BaseCheckpointSaver` is a stable abstract class
- CrewAI: `step_callback` / `task_callback` are simple callables
- ADK: `before_agent_callback` / `after_agent_callback` are documented lifecycle hooks

When upstream releases a new version:

1. Check COMPATIBILITY.md for our API surface
2. Run integration tests against the new version
3. If tests pass → update version matrix
4. If tests fail → check what changed, update our integration
5. Document the change in COMPATIBILITY.md

## Testing Without Framework Dependencies

Integration tests should work in two modes:

1. **Unit mode** (no framework installed): Test `deliteBackend` directly
2. **Integration mode** (framework installed): Test the full wrapper

```python
@pytest.mark.skipif(not _HAS_LANGCHAIN, reason="langgraph not installed")
def test_checkpointer_with_real_graph():
    ...
```

The unit tests in `tests/test_integrations.py` validate the backend
without any framework dependencies. Integration tests are in
`tests/test_{framework}_integration.py` and require the framework.
