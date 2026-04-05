"""LLM provider implementations.

Each provider translates between the durable runtime's message format
and the provider's native API. No external dependencies — uses stdlib
``urllib.request`` for HTTP.

Usage::

    from durable import Agent
    from durable.providers import OpenAI, Anthropic

    # OpenAI
    agent = Agent("./data")
    agent.set_llm(OpenAI(api_key="sk-..."))

    # Anthropic
    agent = Agent("./data")
    agent.set_llm(Anthropic(api_key="sk-ant-..."))

    # Or use environment variables (auto-detected)
    agent.set_llm(OpenAI())      # reads OPENAI_API_KEY
    agent.set_llm(Anthropic())   # reads ANTHROPIC_API_KEY
"""

from .openai import OpenAI
from .anthropic import Anthropic

__all__ = ["OpenAI", "Anthropic"]
