# durable-runtime

The sqlite of durable agent execution. Zero dependencies. Crash-recoverable. Exactly-once semantics.

```python
from durable import Agent, tool
from durable.providers import OpenAI

@tool("get_weather", description="Get weather for a location")
def get_weather(location: str) -> dict:
    return {"temp": 72, "conditions": "sunny", "location": location}

with Agent("./my-agent") as agent:
    agent.add_tool(get_weather)
    agent.set_llm(OpenAI())  # reads OPENAI_API_KEY from env
    response = agent.run("What's the weather in San Francisco?")
    print(response)
```

## Features

- **Crash recovery** — completed steps are never re-executed
- **Budget limits** — suspend when cost/call/time limits are hit
- **Agent contracts** — enforceable invariants at the step boundary
- **Lifecycle hooks** — intercept before/after tool and LLM calls
- **Multi-agent coordination** — durable DAG of workers with dependency tracking
- **Streaming** — token-by-token LLM responses
- **Authentication** — token-based protocol auth via `DURABLE_AUTH_TOKEN`
- **Testing** — `MockAgent` for unit tests without the runtime binary

## Providers

| Provider | Import | Env Variable |
|----------|--------|-------------|
| OpenAI | `from durable.providers import OpenAI` | `OPENAI_API_KEY` |
| Anthropic | `from durable.providers import Anthropic` | `ANTHROPIC_API_KEY` |
| Custom | Any callable `(messages, tools, model) -> dict` | — |

See the [full documentation](https://github.com/durable-runtime/durable-runtime) for details.
