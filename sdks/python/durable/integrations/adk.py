"""Google ADK integration — durable agent execution with lifecycle callbacks.

Add crash recovery to any ADK agent with one function call::

    from durable.integrations.adk import durable_agent

    agent = durable_agent(
        LlmAgent(name="OrderAgent", tools=[...], model=...),
        data_dir="./data",
    )

    app = AdkApp(agent)
    async for event in app.async_stream_query(user_id="user1", message="..."):
        print(event)
"""

from __future__ import annotations

from typing import Any, Optional

from ._base import DurableBackend


def durable_agent(
    agent: Any,
    data_dir: str = "./durable-data",
) -> Any:
    """Wrap an ADK agent with durable execution callbacks.

    Injects ``before_agent_callback`` and ``after_agent_callback`` that
    persist session state to the durable backend. On restart with the
    same user_id, the agent resumes from the last checkpoint.

    Args:
        agent: An ADK ``LlmAgent`` or any agent with callback support.
        data_dir: Directory for durable state.

    Returns:
        The same agent with durable callbacks injected.
    """
    backend = DurableBackend(data_dir)
    step_counter = {}  # per-session step counter

    # Preserve any existing callbacks
    existing_before = getattr(agent, "before_agent_callback", None)
    existing_after = getattr(agent, "after_agent_callback", None)

    def before_callback(callback_context: Any) -> Optional[Any]:
        """Load checkpoint before agent execution."""
        session_id = _get_session_id(callback_context)
        if session_id not in step_counter:
            step_counter[session_id] = 0

        # Load checkpoint into session state
        checkpoint = backend.load_checkpoint(session_id)
        if checkpoint:
            state = getattr(callback_context, "state", None)
            if state is not None and isinstance(checkpoint, dict):
                for key, value in checkpoint.items():
                    if key.startswith("_durable_"):
                        continue  # Skip our internal keys
                    try:
                        state[key] = value
                    except (TypeError, KeyError):
                        pass

        # Call existing callback if any
        if existing_before:
            return existing_before(callback_context)
        return None

    def after_callback(callback_context: Any) -> Optional[Any]:
        """Save checkpoint after agent execution."""
        session_id = _get_session_id(callback_context)
        step_counter.setdefault(session_id, 0)
        step_counter[session_id] += 1

        # Extract state to checkpoint
        state_dict = _extract_state(callback_context)

        # Save checkpoint
        backend.save_checkpoint(
            thread_id=session_id,
            checkpoint=state_dict,
            metadata={
                "step": step_counter[session_id],
                "agent_name": getattr(agent, "name", "unknown"),
            },
        )

        # Record step
        backend.record_step(
            thread_id=session_id,
            step_name=f"agent_turn_{step_counter[session_id]}",
            step_index=step_counter[session_id],
            result=state_dict,
        )

        # Track token usage if available
        usage = _extract_usage(callback_context)
        if usage:
            backend.record_usage(
                thread_id=session_id,
                input_tokens=usage.get("input_tokens", 0),
                output_tokens=usage.get("output_tokens", 0),
            )

        # Call existing callback if any
        if existing_after:
            return existing_after(callback_context)
        return None

    # Inject callbacks
    agent.before_agent_callback = before_callback
    agent.after_agent_callback = after_callback

    return agent


def get_usage(agent: Any, session_id: str) -> dict:
    """Get cumulative token usage for a session.

    Args:
        agent: The durable-wrapped agent.
        session_id: The session/user ID.

    Returns:
        Dict with input_tokens, output_tokens, total_tokens, cost_dollars, call_count.
    """
    # Find the backend from the agent's callbacks
    backend = _find_backend(agent)
    if backend:
        return backend.get_usage(session_id)
    return {}


def _get_session_id(callback_context: Any) -> str:
    """Extract session ID from ADK callback context."""
    # Try session.id first
    session = getattr(callback_context, "session", None)
    if session:
        sid = getattr(session, "id", None)
        if sid:
            return str(sid)

    # Try user_id
    user_id = getattr(callback_context, "user_id", None)
    if user_id:
        return str(user_id)

    return "default"


def _extract_state(callback_context: Any) -> dict:
    """Extract serializable state from callback context."""
    state = getattr(callback_context, "state", None)
    if state is None:
        return {}

    # Try to convert state to dict
    if isinstance(state, dict):
        return _make_serializable(state)

    # ADK state objects may have a .to_dict() or similar
    if hasattr(state, "to_dict"):
        return _make_serializable(state.to_dict())

    # Try iterating
    try:
        return _make_serializable(dict(state))
    except (TypeError, ValueError):
        return {}


def _extract_usage(callback_context: Any) -> Optional[dict]:
    """Extract token usage from callback context if available."""
    # Try various paths where ADK might put usage info
    for attr in ("usage", "token_usage", "model_usage"):
        obj = getattr(callback_context, attr, None)
        if obj:
            return {
                "input_tokens": getattr(obj, "input_tokens", 0) or getattr(obj, "prompt_tokens", 0),
                "output_tokens": getattr(obj, "output_tokens", 0) or getattr(obj, "completion_tokens", 0),
            }
    return None


def _make_serializable(obj: Any) -> Any:
    """Convert an object to a JSON-serializable form."""
    import json

    try:
        json.dumps(obj)
        return obj
    except (TypeError, ValueError):
        pass

    if isinstance(obj, dict):
        return {str(k): _make_serializable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_make_serializable(item) for item in obj]
    return str(obj)


def _find_backend(agent: Any) -> Optional[DurableBackend]:
    """Find the DurableBackend from an agent's injected callbacks."""
    cb = getattr(agent, "after_agent_callback", None)
    if cb and hasattr(cb, "__closure__") and cb.__closure__:
        for cell in cb.__closure__:
            try:
                val = cell.cell_contents
                if isinstance(val, DurableBackend):
                    return val
            except ValueError:
                continue
    return None
