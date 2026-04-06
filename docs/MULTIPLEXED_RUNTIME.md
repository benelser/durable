# Multiplexed Runtime Architecture

One Rust process, N agents. The runtime multiplexes agent execution
onto a thread pool, sharing storage and the protocol pipe.

## Current Architecture (1:1)

```
Python ──stdin/stdout──► Rust Process (1 AgentRuntime)
                           │
                           └── run_agent blocks command loop
                               only one agent can run at a time
```

## Target Architecture (1:N)

```
Python ──stdin/stdout──► Rust Process
                           │
                           ├── Command Loop (main thread)
                           │     reads stdin, dispatches, never blocks
                           │
                           ├── Shared Resources
                           │     FileStorage (Arc, thread-safe)
                           │     FileEventStore (Arc, thread-safe)
                           │     SdkLlmClient (Arc, sends callbacks through pipe)
                           │     ThreadPool (Arc, for parallel tool exec)
                           │
                           └── Agent Registry (BTreeMap<AgentId, AgentSlot>)
                                 │
                                 ├── "order-agent" ─► AgentSlot {
                                 │     config, tools, contracts, budget,
                                 │     active_executions: BTreeMap<ExecId, thread::JoinHandle>
                                 │   }
                                 │
                                 ├── "research-agent" ─► AgentSlot { ... }
                                 │
                                 └── "refund-agent" ─► AgentSlot { ... }
```

## Protocol Changes

### Current: one agent, synchronous

```jsonl
{"type":"create_agent", "config":{...}, "tools":[...]}
{"type":"run_agent", "input":"..."}          ← blocks until done
{"type":"completed", "response":"..."}
```

### Target: named agents, async execution

```jsonl
{"type":"create_agent", "agent_id":"order-agent", "config":{...}, "tools":[...]}
{"type":"create_agent", "agent_id":"research-agent", "config":{...}, "tools":[...]}
{"type":"agent_created", "agent_id":"order-agent"}
{"type":"agent_created", "agent_id":"research-agent"}

{"type":"run", "agent_id":"order-agent", "input":"Process order #456"}
{"type":"run", "agent_id":"research-agent", "input":"Research competitor pricing"}
  ← both return immediately, agents run on separate threads

{"type":"chat_request", "agent_id":"order-agent", "callback_id":"aaa", ...}
{"type":"chat_request", "agent_id":"research-agent", "callback_id":"bbb", ...}
  ← interleaved callbacks from both agents

{"type":"chat_response", "callback_id":"aaa", ...}      ← Python responds to order-agent LLM
{"type":"chat_response", "callback_id":"bbb", ...}      ← Python responds to research-agent LLM

{"type":"completed", "agent_id":"order-agent", "execution_id":"xxx", ...}
{"type":"suspended", "agent_id":"research-agent", "execution_id":"yyy", ...}

{"type":"signal", "execution_id":"yyy", "signal_name":"approved", "data":true}
  ← runtime auto-resumes research-agent
{"type":"completed", "agent_id":"research-agent", "execution_id":"yyy", ...}
```

### Key difference: `run` is non-blocking

The command loop dispatches `run` to a thread and immediately returns
to process more commands. Callbacks are interleaved on the shared pipe,
correlated by `callback_id`.

## Rust Implementation

```
sdk_mode main loop:
  │
  ├── "create_agent" → AgentRegistry.insert(agent_id, AgentSlot::new(config, tools, ...))
  │
  ├── "run" → {
  │     let slot = registry.get(agent_id);
  │     let rt = slot.make_runtime(shared_storage, shared_event_store, shared_llm);
  │     let handle = std::thread::spawn(move || rt.start(input));
  │     slot.active_executions.insert(exec_id, handle);
  │     // returns immediately — does NOT block the command loop
  │   }
  │
  ├── "signal" → {
  │     shared_storage.store_signal(exec_id, name, data);
  │     // event loop will detect and auto-resume
  │   }
  │
  └── callback responses (chat_response, tool_result, contract_result)
        → routed by callback_id to the waiting agent thread
          (existing StdinReader waiter mechanism, unchanged)
```

## Thread Model

```
With 3 agents (A active, B active, C suspended):

Rust Process
├── Thread: command-loop         ← reads stdin, dispatches
│     never blocks, O(1) per command
│
├── Thread: agent-A              ← running agent loop
│     blocked on: LLM callback response (waiter in StdinReader)
│     file access: events/aaa.ndjson (exclusive via lease)
│
├── Thread: agent-B              ← running agent loop
│     blocked on: tool callback response (waiter in StdinReader)
│     file access: events/bbb.ndjson (exclusive via lease)
│
├── Thread: event-loop           ← scans signals, checks timers
│     every 100ms: for each suspended exec, check signals/ dir
│     on signal found: sends internal "resume" to command channel
│
└── (no thread for agent-C — suspended, zero compute)

Total Rust threads: 4  (command + event-loop + 2 active agents)
Total files open: 2 agent event logs + signal dir scans
```

## Lock Analysis (N agents)

```
Resource                 Lock Type        Granularity    Contention
────────                 ─────────        ───────────    ──────────
Agent registry           RwLock           global         LOW: read-heavy
                                                         write only on
                                                         create/remove agent

Event log writes         file append      per-agent      NONE between agents
                         + fsync          (different      (each agent writes
                                          files)          its own file)

Step cache               Mutex            per-execution  NONE between agents
                                                         (each ReplayContext
                                                          is thread-local)

Lease                    event log entry  per-execution  NONE during normal
                                                         ops (one thread
                                                         per execution)

Protocol pipe            StdinReader      per-callback   LOW: waiters keyed
(stdin/stdout)           waiter map       (correlation   by callback_id,
                                          ID)            no head-of-line
                                                         blocking

Signal file write        atomic rename    per-file       NONE: different
                                                         signal files per
                                                         execution
```

Zero cross-agent lock contention. The only global lock is the agent
registry, which is read-heavy (lookup on every command) and write-rare
(only on agent create/remove).

## Scaling Properties

Agents    Rust Threads    Files Open    Memory         CPU (idle agents)
──────    ────────────    ──────────    ──────         ─────────────────
1         3               1             ~2MB           0 (suspended = no thread)
10        3-12            1-10          ~20MB          0 for suspended agents
100       3-102           1-100         ~200MB         0 for suspended agents
1000      3-1002          1-1000        ~2GB           0 for suspended agents

Active agents = threads. Suspended agents = just files. The runtime
scales with active agents, not total agents. 1000 suspended agents
waiting for human approval cost nothing.

## Migration Path

The change is isolated to sdk_mode.rs:
1. Replace `Arc<Mutex<Option<Arc<AgentRuntime>>>>` with `AgentRegistry`
2. Change `run_agent` from synchronous to `thread::spawn`
3. Add `agent_id` to protocol messages
4. Add event-loop thread for signal/timer watching
5. Completion/suspension events emitted from agent threads via shared writer

No changes to: AgentRuntime, ReplayContext, EventStore, FileStorage,
the agent loop, tool execution, LLM callbacks, or the event log format.

The entire change is in the multiplexing layer. The execution engine
is unchanged.
