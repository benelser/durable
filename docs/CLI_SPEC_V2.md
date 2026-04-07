# durable CLI Specification

One binary. Every feature. Every language.

## `durable help`

```
durable — the SQLite of durable agent execution

Getting started:
  durable init <name> [--lang python|typescript]    Create a new agent project

Runtime:
  durable runtime                                   Start the runtime (SDKs connect via stdio)
  durable runtime --auth-token <token>               Start with authentication

Inspection:
  durable status [--data-dir <path>]                List all executions
  durable inspect <execution-id> [--data-dir]       Detailed execution view
  durable steps <execution-id> [--data-dir]         Step-by-step timeline with timing
  durable events <execution-id> [--data-dir]        Raw event log
  durable export <execution-id> [--data-dir]        Export execution as JSON

Operations:
  durable health [--data-dir <path>]                Storage health and stats
  durable compact [--data-dir <path>]               Compact old event logs
  durable ping                                      Health check a running runtime

Info:
  durable version                                   Show version
  durable help                                      Show this help
  durable help <command>                            Show help for a command

Environment:
  DURABLE_DATA_DIR          Default data directory (default: ./data)
  OPENAI_API_KEY            OpenAI provider
  ANTHROPIC_API_KEY         Anthropic provider
  DURABLE_AUTH_TOKEN        Runtime authentication token
```

## Command Details

### `durable init`

```
durable init <name> [--lang python|typescript]

Create a new agent project with a working example.

Arguments:
  <name>                    Project directory name

Options:
  --lang <language>         Language template (default: python)
                            Supported: python, typescript

Creates:
  <name>/agent.py           Working agent (or agent.ts)
  <name>/.env.example       API key template
  <name>/.gitignore         Ignores data/ and secrets

The generated agent.py is a complete, runnable example.
Install the SDK with your package manager, set your API key, run it.
```

### `durable runtime`

```
durable runtime [--auth-token <token>]

Start the durable execution runtime. Language SDKs connect to this
process via stdin/stdout NDJSON protocol.

Options:
  --auth-token <token>      Require authentication from SDKs

You typically don't run this directly — the SDK starts it automatically
as a managed subprocess. This command is for advanced use:
  - Running the runtime as a standalone service
  - Debugging protocol issues
  - Custom SDK development

The runtime can be started via environment variable instead:
  DURABLE_AUTH_TOKEN=secret durable runtime
```

### `durable status`

```
durable status [--data-dir <path>] [--json]

List all executions in the data directory.

Options:
  --data-dir <path>         Data directory (default: $DURABLE_DATA_DIR or ./data)
  --json                    Output as JSON (for scripting)

Output:
  ID           STATUS      STEPS    CREATED              AGENT
  abc-123...   completed   12       2025-03-15 14:30:00  order-processor
  def-456...   suspended   5        2025-03-15 14:31:00  research-bot
  ghi-789...   running     3        2025-03-15 14:32:00  writer-bot
```

### `durable inspect`

```
durable inspect <execution-id> [--data-dir <path>] [--json]

Detailed view of a single execution.

Output:
  Execution:    abc-123-def-456-ghi-789
  Agent:        order-processor
  Status:       completed
  Created:      2025-03-15 14:30:00
  Duration:     12.4s
  Steps:        12 (4 LLM calls, 8 tool calls)
  Prompt hash:  a1b2c3d4e5f6g7h8
  Tools hash:   f8e7d6c5b4a3...

  Budget:
    LLM calls:  4 / 10
    Dollars:    $0.03 / $2.00

  Events:       42 (3.2KB)
  Snapshot:     at event #30
```

### `durable steps`

```
durable steps <execution-id> [--data-dir <path>] [--json]

Step-by-step timeline with timing.

Output:
  Timeline: abc-123-def-456

  #   STEP                    DURATION   STATUS     REPLAYED
  0   llm_call                2.3s       completed  no
  1   tool_check_inventory    0.1s       completed  no
  2   llm_call                1.8s       completed  no
  3   tool_charge_payment     0.2s       completed  no
  4   llm_call                1.5s       completed  no
  5   tool_send_email         0.1s       completed  no
  6   llm_call                1.2s       completed  no

  Total: 7.2s (4 LLM calls, 3 tool calls)
```

### `durable events`

```
durable events <execution-id> [--data-dir <path>] [--json]

Raw event log for debugging.

Output (default: human-readable summary):
  #    TYPE                  TIMESTAMP            DETAILS
  1    execution_created     14:30:00.123         agent=order-processor prompt_hash=a1b2...
  2    step_started          14:30:00.456         step=llm_call #0
  3    step_completed        14:30:02.789         step=llm_call #0 (2.3s)
  4    step_started          14:30:02.801         step=tool_check_inventory #0
  ...

Output (--json): raw NDJSON events, one per line
```

### `durable export`

```
durable export <execution-id> [--data-dir <path>] [-o <file>]

Export complete execution state as JSON.

Options:
  -o <file>                 Output file (default: stdout)

Output: JSON object with execution metadata, all events, step results,
and current state. Suitable for archival, debugging, or migration.
```

### `durable health`

```
durable health [--data-dir <path>] [--json]

Storage health check and recommendations.

Output:
  Storage: ./data
  ──────────────────────────────
  Executions:    42
  Total events:  1,247
  Storage size:  2.3 MB
  Oldest:        2025-03-01
  Newest:        2025-03-15

  Health:
    [OK]   All event logs valid
    [OK]   No orphaned step files
    [WARN] 12 executions older than 30 days (run durable compact)
```

### `durable compact`

```
durable compact [--data-dir <path>] [--older-than <days>] [--dry-run]

Compact old event logs. Creates snapshots and removes old events.

Options:
  --older-than <days>       Only compact executions older than N days (default: 30)
  --dry-run                 Show what would be compacted without doing it
```

### `durable ping`

```
durable ping [--timeout <seconds>]

Health check a running runtime (via stdin/stdout protocol).

Output:
  Engine:     durable 0.1.0
  Protocol:   1.0
  Agents:     3 registered, 1 active
  Uptime:     2h 34m
```

### `durable version`

```
durable version

Output:
  durable 0.1.0 (rust engine, protocol v1.0)
```

## Design Principles

1. **Every command supports `--json`** for scripting and CI/CD
2. **`--data-dir` defaults to `$DURABLE_DATA_DIR` then `./data`**
3. **Colors auto-disabled** when stdout is not a TTY (piping, CI)
4. **Exit codes**: 0 = success, 1 = error, 2 = usage error
5. **No interactive prompts** — everything is flags and arguments
6. **Help for every command** via `durable help <command>`

## Binary Name

The binary is `durable`. Package names match:

```
brew install durable
apt install durable
pip install durable           # bundles binary in wheel
bun add durable               # bundles binary in package
cargo install durable          # builds from source
```

The binary name in `Cargo.toml` changes from `durable-runtime` to `durable`.
The crate/package name stays `durable-runtime` internally.
