#!/usr/bin/env python3
"""
06 — CLI Observability: Run an agent, then inspect everything.

This example runs a multi-step agent and then shows you how to use the
CLI to inspect every detail: execution status, step timeline, cost
breakdown, and animated replay.

    export OPENAI_API_KEY=sk-...
    python 06_cli_observability.py

Or:
    python 06_cli_observability.py --mock
"""

import os
import sys
import subprocess

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool

DATA_DIR = "./demo-data/observability"


@tool("search_web", description="Search the web for information")
def search_web(query: str) -> dict:
    print(f"    [search] {query}")
    return {
        "results": [
            {"title": f"Result 1 for '{query}'", "snippet": "Relevant information found."},
            {"title": f"Result 2 for '{query}'", "snippet": "Additional context available."},
        ],
        "total_results": 2,
    }


@tool("analyze_data", description="Analyze data and extract insights")
def analyze_data(data: str, question: str) -> dict:
    print(f"    [analyze] Question: {question}")
    return {
        "insight": f"Analysis of '{question}': the data suggests positive trends.",
        "confidence": 0.85,
    }


@tool("write_report", description="Write a formatted report")
def write_report(title: str, sections: list) -> dict:
    print(f"    [report] Writing: {title}")
    return {
        "title": title,
        "sections": len(sections) if sections else 0,
        "word_count": 450,
        "status": "complete",
    }


MOCK_STEP = 0

def mock_llm(messages, tools=None, model=None):
    global MOCK_STEP
    MOCK_STEP += 1

    if MOCK_STEP == 1:
        return {
            "tool_calls": [
                {"id": "c1", "name": "search_web", "arguments": {"query": "AI agent frameworks 2025"}},
            ],
            "usage": {"input_tokens": 350, "output_tokens": 80},
        }
    elif MOCK_STEP == 2:
        return {
            "tool_calls": [
                {"id": "c2", "name": "search_web", "arguments": {"query": "durable execution patterns"}},
                {"id": "c3", "name": "analyze_data", "arguments": {
                    "data": "search results",
                    "question": "What differentiates durable execution from standard frameworks?",
                }},
            ],
            "usage": {"input_tokens": 600, "output_tokens": 150},
        }
    elif MOCK_STEP == 3:
        return {
            "tool_calls": [
                {"id": "c4", "name": "write_report", "arguments": {
                    "title": "AI Agent Framework Comparison",
                    "sections": ["Overview", "Durable Execution", "Recommendations"],
                }},
            ],
            "usage": {"input_tokens": 900, "output_tokens": 200},
        }
    else:
        return {
            "content": "Report complete: 'AI Agent Framework Comparison' with 3 sections covering framework landscape, durable execution advantages, and recommendations.",
            "usage": {"input_tokens": 300, "output_tokens": 100},
        }


def run_cli(args):
    """Run a durable CLI command and display the output."""
    cmd = f"python -m durable.cli.main {args}"
    print(f"  $ {cmd}\n")
    try:
        result = subprocess.run(
            cmd.split(),
            capture_output=True,
            text=True,
            timeout=10,
            env={**os.environ, "DURABLE_DATA_DIR": DATA_DIR},
        )
        output = result.stdout.strip()
        if output:
            for line in output.split("\n"):
                print(f"    {line}")
        if result.stderr.strip():
            for line in result.stderr.strip().split("\n"):
                print(f"    (stderr) {line}")
    except (subprocess.TimeoutExpired, FileNotFoundError) as e:
        print(f"    (CLI not available: {e})")
    print()


def main():
    import shutil
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  CLI Observability: Full Execution Inspection")
    print("=" * 64)

    # ---- Run the agent ----
    print("\n--- Running a multi-step research agent ---\n")

    agent = Agent(
        DATA_DIR,
        system_prompt=(
            "You are a research agent. When asked to write a report:\n"
            "1. Search the web for the topic\n"
            "2. Search for related subtopics\n"
            "3. Analyze the combined data\n"
            "4. Write a formatted report\n"
            "5. Summarize what you did"
        ),
    )
    agent.add_tool(search_web)
    agent.add_tool(analyze_data)
    agent.add_tool(write_report)

    if "--mock" in sys.argv:
        agent.set_llm(mock_llm)
    else:
        from durable.providers import OpenAI
        agent.set_llm(OpenAI())

    response = agent.run("Write a report comparing AI agent frameworks")
    exec_id = response.execution_id

    print(f"\n  Agent: {response.text}")
    print(f"  Execution ID: {exec_id}")
    print(f"  Status: {response.status.value}")
    agent.close()

    # ---- Show CLI commands ----
    print(f"\n{'=' * 64}")
    print("  CLI Inspection Commands")
    print(f"{'=' * 64}")
    print()
    print("  After any agent run, you can inspect everything with the CLI:")
    print()

    print("--- 1. Execution Status ---\n")
    run_cli(f"status --data-dir {DATA_DIR}")

    print("--- 2. Step Timeline ---\n")
    run_cli(f"steps {exec_id} --data-dir {DATA_DIR}")

    print("--- 3. Detailed Inspection ---\n")
    run_cli(f"inspect {exec_id} --data-dir {DATA_DIR}")

    print("--- 4. Cost Breakdown ---\n")
    run_cli(f"cost {exec_id} --data-dir {DATA_DIR}")

    print("--- 5. Storage Health ---\n")
    run_cli(f"health --data-dir {DATA_DIR}")

    print(f"{'=' * 64}")
    print("  OTHER COMMANDS TO TRY")
    print(f"{'=' * 64}")
    print(f"""
  # Animated step-by-step replay
  durable replay {exec_id} --data-dir {DATA_DIR}

  # Export execution as JSON (for debugging or auditing)
  durable export {exec_id} --data-dir {DATA_DIR}

  # Compact old event logs (production maintenance)
  durable compact --data-dir {DATA_DIR}
""")

    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
