#!/usr/bin/env python3
"""
04 — Budget Guardrails: Automatic cost control.

The agent has a budget of $0.50 and 3 LLM calls. When the budget is
exhausted, execution SUSPENDS (not crashes). You can inspect what
happened, increase the budget, and resume.

This prevents runaway agents from burning through your API credits.
Unlike a hard kill, suspension preserves all state -- you don't lose
the work that was already done.

    export OPENAI_API_KEY=sk-...
    python 04_budget_guardrails.py

Or:
    python 04_budget_guardrails.py --mock
"""

import os
import sys
import shutil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool, Budget

DATA_DIR = "./demo-data/budget"


@tool("research_topic", description="Research a topic and return findings")
def research_topic(topic: str) -> dict:
    print(f"    [research] Researching: {topic}")
    return {
        "topic": topic,
        "findings": f"Key findings about {topic}: it is widely studied and has significant implications.",
        "sources": 3,
    }


@tool("write_section", description="Write a section of the report")
def write_section(title: str, content: str) -> dict:
    print(f"    [write] Writing section: {title}")
    return {"title": title, "word_count": len(content.split()), "status": "drafted"}


MOCK_STEP = 0

def mock_llm(messages, tools=None, model=None):
    """Mock LLM that simulates token usage to trigger budget exhaustion."""
    global MOCK_STEP
    MOCK_STEP += 1

    if MOCK_STEP == 1:
        return {
            "tool_calls": [{"id": "c1", "name": "research_topic", "arguments": {"topic": "quantum computing"}}],
            "usage": {"input_tokens": 500, "output_tokens": 200},
        }
    elif MOCK_STEP == 2:
        return {
            "tool_calls": [{"id": "c2", "name": "research_topic", "arguments": {"topic": "quantum error correction"}}],
            "usage": {"input_tokens": 800, "output_tokens": 300},
        }
    elif MOCK_STEP == 3:
        # This is the 3rd LLM call -- should hit the budget limit
        return {
            "tool_calls": [{"id": "c3", "name": "write_section", "arguments": {
                "title": "Introduction",
                "content": "Quantum computing represents a paradigm shift in computation.",
            }}],
            "usage": {"input_tokens": 1200, "output_tokens": 500},
        }
    else:
        return {
            "content": "Report complete: Quantum computing research with 3 sources across 2 topics.",
            "usage": {"input_tokens": 400, "output_tokens": 100},
        }


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  Budget Guardrails: Automatic Cost Control")
    print("=" * 64)

    agent = Agent(
        DATA_DIR,
        system_prompt=(
            "You are a research assistant. When asked to write a report:\n"
            "1. Research the main topic\n"
            "2. Research a subtopic\n"
            "3. Write the introduction section\n"
            "4. Summarize the results"
        ),
    )
    agent.add_tool(research_topic)
    agent.add_tool(write_section)

    # Tight budget: only 3 LLM calls allowed
    agent.budget = Budget(max_llm_calls=3, max_dollars=0.50)

    if "--mock" in sys.argv:
        agent.set_llm(mock_llm)
    else:
        from durable.providers import OpenAI
        agent.set_llm(OpenAI())

    # ---- Run 1: Agent hits the budget wall ----
    print("\n--- Run 1: Agent with tight budget ---\n")
    print(f"  Budget: 3 LLM calls, $0.50 max\n")

    response = agent.run("Write a short report on quantum computing")
    exec_id = response.execution_id

    print(f"\n  Status: {response.status.value}")

    if response.is_suspended:
        reason = response.suspend_reason
        print(f"  Suspended: {reason.type}")
        if reason.dimension:
            print(f"    Dimension: {reason.dimension}")
        print()
        print("  The agent ran out of budget but did NOT crash.")
        print("  All completed work is preserved in the event log.")
        print("  You can increase the budget and resume.")

        # ---- Run 2: Increase budget and resume ----
        print("\n--- Run 2: Increase budget and resume ---\n")

        agent.budget = Budget(max_llm_calls=10, max_dollars=5.00)
        response2 = agent.resume(exec_id)

        print(f"  Status: {response2.status.value}")
        if response2.text:
            print(f"  Agent: {response2.text}")

        print()
        print("  The agent completed using the work it had already done.")
        print("  Research steps were NOT re-executed -- cached from run 1.")
    else:
        print(f"  Agent completed within budget: {response.text}")

    print(f"\n{'=' * 64}")
    print("  KEY INSIGHT")
    print(f"{'=' * 64}")
    print("  Other frameworks: budget exceeded = crash = all work lost")
    print("  Durable: budget exceeded = suspend = resume after adjustment")
    print()

    agent.close()
    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
