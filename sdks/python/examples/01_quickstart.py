#!/usr/bin/env python3
"""
01 — Quickstart: A durable agent in 15 lines.

    export OPENAI_API_KEY=sk-...
    python 01_quickstart.py

Or without an API key:

    python 01_quickstart.py --mock
"""

import os
import sys
import shutil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool

DATA_DIR = "./demo-data/quickstart"


@tool("get_weather", description="Get the current weather for a city")
def get_weather(city: str) -> dict:
    """Simulated weather lookup."""
    weather = {
        "San Francisco": {"temp": 62, "conditions": "foggy"},
        "New York": {"temp": 78, "conditions": "sunny"},
        "London": {"temp": 55, "conditions": "rainy"},
    }
    return weather.get(city, {"temp": 70, "conditions": "unknown"})


@tool("get_time", description="Get the current local time for a city")
def get_time(city: str) -> dict:
    """Simulated time lookup."""
    import time
    return {"city": city, "local_time": time.strftime("%I:%M %p"), "timezone": "local"}


def mock_llm(messages, tools=None, model=None):
    """Scripted LLM for testing without an API key."""
    # Count how many assistant messages we've sent
    assistant_count = sum(1 for m in messages if m.get("role") == "assistant")

    if assistant_count == 0:
        return {"tool_calls": [
            {"id": "c1", "name": "get_weather", "arguments": {"city": "San Francisco"}},
            {"id": "c2", "name": "get_time", "arguments": {"city": "San Francisco"}},
        ]}
    else:
        return {"content": "It's currently 62F and foggy in San Francisco. Local time is displayed above."}


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    with Agent(DATA_DIR, system_prompt="You are a helpful assistant with weather and time tools.") as agent:
        agent.add_tool(get_weather)
        agent.add_tool(get_time)

        if "--mock" in sys.argv:
            agent.set_llm(mock_llm)
        else:
            from durable.providers import OpenAI
            agent.set_llm(OpenAI())

        print("Agent: thinking...\n")
        response = agent.run("What's the weather and time in San Francisco?")
        print(f"Agent: {response.text}")
        print(f"\nExecution ID: {response.execution_id}")
        print(f"Status: {response.status.value}")

    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
