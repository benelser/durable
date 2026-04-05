#!/usr/bin/env python3
"""
CrewAI + Durable: Resilient Multi-Agent Research

A research crew that survives crashes. Three agents — researcher,
analyst, writer — work sequentially. If the process dies after the
researcher finishes (2 hours of work), the analyst picks up from
the research results. No work is lost.

The flow:
  1. Researcher agent gathers information (expensive, slow)
  2. Analyst agent processes and structures findings
  3. Writer agent produces the final report

The "wow":
  - Kill the process after the researcher finishes
  - Restart → analyst starts immediately with cached research
  - No API calls wasted. No work repeated.

Run it:
  pip install durable-runtime[crewai]
  python crewai_resilient_research.py

What to watch for:
  - Research runs only ONCE across both executions
  - Analysis and writing resume from where they left off
  - Token usage is tracked across the entire crew
"""

import os
import sys
import shutil
import time

try:
    from crewai import Agent, Task, Crew
except ImportError:
    print("Install required packages:")
    print("  pip install crewai")
    sys.exit(1)

from durable.integrations.crewai import DurableCrew

DATA_DIR = "./research-crew-data"

# Track which tasks actually ran
TASKS_EXECUTED = []


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 60)
    print("  CrewAI + Durable: Resilient Multi-Agent Research")
    print("=" * 60)

    # -- Define agents --
    researcher = Agent(
        role="Senior Research Analyst",
        goal="Find comprehensive information on the given topic",
        backstory="You are an expert researcher with 20 years of experience.",
        verbose=False,
    )

    analyst = Agent(
        role="Data Analyst",
        goal="Structure and analyze research findings",
        backstory="You excel at finding patterns in complex data.",
        verbose=False,
    )

    writer = Agent(
        role="Technical Writer",
        goal="Produce a clear, well-structured report",
        backstory="You write reports that executives actually read.",
        verbose=False,
    )

    # -- Define tasks --
    research_task = Task(
        description="Research the current state of durable execution in AI agent frameworks. "
                    "Focus on: crash recovery, exactly-once semantics, and state persistence.",
        expected_output="A comprehensive list of findings with sources.",
        agent=researcher,
    )

    analysis_task = Task(
        description="Analyze the research findings. Identify the top 3 gaps in existing "
                    "frameworks and propose solutions.",
        expected_output="A structured analysis with gap identification and recommendations.",
        agent=analyst,
    )

    writing_task = Task(
        description="Write a 500-word executive summary based on the analysis.",
        expected_output="A polished executive summary ready for stakeholders.",
        agent=writer,
    )

    # -- FIRST RUN --
    print("\n--- First Run: Full crew execution ---\n")

    crew = DurableCrew(
        agents=[researcher, analyst, writer],
        tasks=[research_task, analysis_task, writing_task],
        data_dir=DATA_DIR,
        thread_id="research-001",
        verbose=False,
    )

    start = time.time()
    result = crew.kickoff(inputs={"topic": "durable AI agent execution"})
    elapsed = time.time() - start

    print(f"\n  Completed in {elapsed:.1f}s")
    print(f"  Result preview: {str(result)[:200]}...")

    # -- Check usage --
    usage = crew.get_usage()
    print(f"\n  Token usage:")
    print(f"    Input:  {usage['input_tokens']}")
    print(f"    Output: {usage['output_tokens']}")
    print(f"    Total:  {usage['total_tokens']}")
    print(f"    Calls:  {usage['call_count']}")

    # -- SECOND RUN (simulates restart) --
    print("\n--- Second Run: Resume (all tasks cached) ---\n")

    crew2 = DurableCrew(
        agents=[researcher, analyst, writer],
        tasks=[research_task, analysis_task, writing_task],
        data_dir=DATA_DIR,
        thread_id="research-001",
        verbose=False,
    )

    start2 = time.time()
    result2 = crew2.kickoff(inputs={"topic": "durable AI agent execution"})
    elapsed2 = time.time() - start2

    print(f"  Completed in {elapsed2:.1f}s (should be instant — all cached)")
    print(f"  Same result: {str(result2)[:100]}...")

    print(f"\n{'=' * 60}")
    print(f"  SUMMARY:")
    print(f"    First run:  {elapsed:.1f}s (all tasks executed)")
    print(f"    Second run: {elapsed2:.1f}s (all tasks cached)")
    print(f"    Speedup:    {elapsed/max(elapsed2, 0.001):.0f}x")
    print(f"{'=' * 60}")

    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
