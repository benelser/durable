# Durable Agent Runtime — Wire Protocol Specification v1.0

## Overview

The durable runtime communicates with external tools and LLM adapters via **newline-delimited JSON (NDJSON)** over **stdin/stdout**. Each message is a single JSON object followed by a newline character (`\n`).

## Envelope

Every message MAY include envelope fields:

| Field | Type   | Description                              |
|-------|--------|------------------------------------------|
| `v`   | string | Protocol version (default: `"1.0"`)      |
| `id`  | string | Request/response correlation ID (UUID)   |
| `ts`  | number | Timestamp in milliseconds since epoch    |

Missing `v` field is treated as `"1.0"` for backward compatibility.

## Message Types

### Runtime → Tool

#### `execute`
Request the tool to execute with given arguments.

```json
{"type":"execute","tool_name":"get_weather","arguments":{"location":"SF"},"v":"1.0","id":"abc-123","ts":1700000000000}
```

### Tool → Runtime

#### `result`
Successful tool execution result.

```json
{"type":"result","output":{"temperature":72,"conditions":"sunny"}}
```

#### `error`
Tool execution failed.

```json
{"type":"error","message":"API rate limited","retryable":true}
```

### Runtime → LLM Adapter

#### `chat`
Send a chat completion request.

```json
{"type":"chat","messages":[{"role":"user","content":"Hello"}],"tools":[...],"model":"gpt-4"}
```

### LLM Adapter → Runtime

#### `text`
Text response (no tool calls).

```json
{"type":"text","content":"Hello! How can I help?"}
```

#### `tool_calls`
LLM wants to call tools.

```json
{"type":"tool_calls","calls":[{"id":"call_1","name":"get_weather","arguments":{"location":"SF"}}]}
```

### Bidirectional

#### `heartbeat` / `heartbeat_ack`
Keepalive mechanism. Runtime sends heartbeats; tools/adapters respond with acks.

```json
{"type":"heartbeat","timestamp":1700000000000}
{"type":"heartbeat_ack","timestamp":1700000000000}
```

## Tool Execution Lifecycle

```
Runtime                          Tool Process
   |                                  |
   |  spawn process                   |
   |  -------- stdin -------->        |
   |  {"type":"execute",...}          |
   |                                  |
   |  <------- stdout --------        |
   |  {"type":"result",...}           |
   |  OR {"type":"error",...}         |
   |                                  |
   |  (process may stay alive         |
   |   for multiple calls)            |
```

## LLM Adapter Lifecycle

```
Runtime                        LLM Adapter
   |                                |
   |  -------- stdin -------->      |
   |  {"type":"chat",...}           |
   |                                |
   |  <------- stdout --------      |
   |  {"type":"text",...}           |
   |  OR {"type":"tool_calls",...}  |
```

## Error Handling

- `retryable: true` — the runtime may retry (transient failures like rate limits)
- `retryable: false` — permanent failure (invalid input, auth denied)

## Implementing a Tool (any language)

1. Read JSON lines from stdin
2. Parse the `type` field
3. For `execute`: call your handler, write `result` or `error` to stdout
4. For `heartbeat`: write `heartbeat_ack` to stdout
5. Flush stdout after every write

## SDKs

- **Python**: `sdks/python/durable_sdk/` — `DurableToolServer`, `DurableLlmAdapter`
- **TypeScript**: `sdks/typescript/src/` — `DurableToolServer`, `DurableLlmAdapter`
