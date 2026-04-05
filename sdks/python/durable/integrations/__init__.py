"""Framework integrations — add durability to existing agent frameworks.

Each integration wraps a popular framework with crash recovery,
step memoization, and cost tracking. One-line code change.

Usage::

    # LangGraph
    from durable.integrations.langchain import DurableCheckpointer
    compiled = graph.compile(checkpointer=DurableCheckpointer("./data"))

    # CrewAI
    from durable.integrations.crewai import DurableCrew
    crew = DurableCrew(agents=[...], tasks=[...], data_dir="./data")

    # Google ADK
    from durable.integrations.adk import durable_agent
    agent = durable_agent(my_agent, data_dir="./data")
"""
