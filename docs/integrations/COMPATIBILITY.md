# Framework Integration Compatibility Tracker

This document tracks the upstream APIs our integrations depend on.
When upstream frameworks release new versions, check these surfaces
for breaking changes.

**Last verified:** 2026-04-05

---

## LangChain / LangGraph

### Versions

| Package | Min Version | Tested Version | Notes |
|---------|------------|----------------|-------|
| `langgraph` | 0.2.0 | 0.2.x | Checkpointer interface stable since 0.2 |
| `langchain-core` | 0.3.0 | 0.3.x | `BaseCheckpointSaver` lives here |

### API Surface We Depend On

**`langgraph.checkpoint.base.BaseCheckpointSaver`** — Abstract base class

```python
class BaseCheckpointSaver:
    def put(self, config: RunnableConfig, checkpoint: Checkpoint,
            metadata: CheckpointMetadata, new_versions: ChannelVersions) -> RunnableConfig
    def put_writes(self, config: RunnableConfig,
                   writes: Sequence[Tuple[str, Any]], task_id: str) -> None
    def get_tuple(self, config: RunnableConfig) -> Optional[CheckpointTuple]
    def list(self, config: Optional[RunnableConfig], *, filter: Optional[Dict],
             before: Optional[RunnableConfig], limit: Optional[int]) -> Iterator[CheckpointTuple]
```

**Key types:**
- `RunnableConfig` — dict with `configurable: {"thread_id": str, "checkpoint_id": str, "checkpoint_ns": str}`
- `Checkpoint` — dict with `id: str`, channel states, version vectors
- `CheckpointMetadata` — dict with `source: str`, `step: int`, `writes: dict`
- `CheckpointTuple` — namedtuple `(config, checkpoint, metadata, parent_config, pending_writes)`

**How we use it:**
Our `DurableCheckpointer` implements all four abstract methods. The graph calls `put()` after each super-step and `get_tuple()` on resume. We serialize checkpoints as JSON files keyed by `thread_id/checkpoint_id`.

**Breaking change risks:**
- `CheckpointTuple` fields added/removed → update our constructor call
- `put()` signature changes → update `DurableCheckpointer.put()`
- New required abstract methods → implement them

**Tracking:** https://github.com/langchain-ai/langgraph/blob/main/libs/checkpoint/langgraph/checkpoint/base/__init__.py

---

## CrewAI

### Versions

| Package | Min Version | Tested Version | Notes |
|---------|------------|----------------|-------|
| `crewai` | 0.80.0 | 0.80.x | Callback API stable |

### API Surface We Depend On

**`crewai.Crew`** — Main orchestration class

```python
class Crew:
    def __init__(self,
        agents: List[Agent],
        tasks: List[Task],
        process: Process = Process.sequential,
        step_callback: Optional[Callable] = None,    # ← We use this
        task_callback: Optional[Callable] = None,    # ← We use this
        memory: bool = False,
        **kwargs
    )
    def kickoff(self, inputs: Optional[Dict] = None) -> CrewOutput
```

**Callback signatures:**
```python
# step_callback receives the step output after each agent reasoning step
step_callback(step_output: AgentAction | AgentFinish) -> None

# task_callback receives the task output after each task completes
task_callback(task_output: TaskOutput) -> None
```

**Key types:**
- `TaskOutput` — has `.raw` (str), `.pydantic` (Optional), `.json_dict` (Optional)
- `AgentAction` — has `.tool`, `.tool_input`, `.log`
- `CrewOutput` — has `.raw` (str), `.tasks_output` (list), `.token_usage` (dict)
- Token usage dict: `{"prompt_tokens": int, "completion_tokens": int, "total_tokens": int}`

**How we use it:**
`DurableCrew` wraps `Crew`, injects `step_callback` and `task_callback` to record completed tasks. On restart, we construct a new `Crew` with only the remaining (uncompleted) tasks.

**Breaking change risks:**
- `Crew.__init__` signature changes → update `DurableCrew`
- Callback signatures change → update our callbacks
- `Process` types change → our sequential assumption may break
- `TaskOutput` fields change → update how we serialize results

**Tracking:** https://github.com/crewAIInc/crewAI/blob/main/src/crewai/crew.py

---

## Google ADK (Agent Development Kit)

### Versions

| Package | Min Version | Tested Version | Notes |
|---------|------------|----------------|-------|
| `google-adk` | 1.0.0 | 1.x | Callback API is first-class |

### API Surface We Depend On

**`google.adk.agents.LlmAgent`** — Agent with LLM

```python
class LlmAgent:
    def __init__(self,
        name: str,
        model: BaseLlm,
        tools: List[Tool] = [],
        before_agent_callback: Optional[Callable] = None,   # ← We use this
        after_agent_callback: Optional[Callable] = None,     # ← We use this
        before_model_callback: Optional[Callable] = None,
        after_model_callback: Optional[Callable] = None,
        **kwargs
    )
```

**Callback signature:**
```python
# Both before and after callbacks receive CallbackContext
def callback(ctx: CallbackContext) -> Optional[types.Content]:
    ctx.session          # Session object with .id, .state
    ctx.state            # Mutable dict-like state (read/write)
    ctx.user_content     # The user's message
    ctx.agent_name       # Name of the current agent
```

**Key types:**
- `CallbackContext` — has `.session`, `.state`, `.user_content`, `.agent_name`
- `Session` — has `.id` (str), `.state` (dict), `.events` (list)
- State is a `dict[str, Any]` that's persisted by the session service

**How we use it:**
`durable_agent()` injects `before_agent_callback` (load checkpoint → state) and `after_agent_callback` (state → save checkpoint). The agent runs normally; our callbacks intercept state transitions.

**Breaking change risks:**
- `CallbackContext` fields added/removed → update our extraction logic
- Callback return type changes → update our handlers
- Session state API changes → update how we read/write state
- Agent constructor signature changes → update `durable_agent()`

**Tracking:** https://github.com/google/adk-python/blob/main/src/google/adk/agents/llm_agent.py

---

## Integration Test Matrix

Run these when upgrading upstream dependencies:

| Test | What it validates | Command |
|------|-------------------|---------|
| Backend unit tests | Shared persistence layer | `python3 -m pytest tests/test_integrations.py` |
| LangChain integration | Checkpointer put/get/list | `pip install langgraph && python3 -m pytest tests/test_langchain_integration.py` |
| CrewAI integration | Crew wrapper + task callback | `pip install crewai && python3 -m pytest tests/test_crewai_integration.py` |
| ADK integration | Agent callback injection | `pip install google-adk && python3 -m pytest tests/test_adk_integration.py` |
| E2E (all frameworks) | Full round-trip with real LLM | `python3 tests/run_e2e.py` |

## Adding a New Integration

To add support for a new framework:

1. Identify the **hook point** — checkpoint API, callback system, or execution wrapper
2. Implement using `DurableBackend` — all integrations share the same backend
3. Add to this compatibility document — track the upstream API surface
4. Write an example in `examples/integrations/`
5. Add optional dependency in `pyproject.toml`
6. Test without the framework installed (graceful import failure)

The key question for any framework: **"Where does state live between steps, and how do we intercept it?"**
