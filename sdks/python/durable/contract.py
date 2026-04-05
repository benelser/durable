"""Agent contracts — enforceable invariants at the step boundary."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable


@dataclass
class Contract:
    """A named contract that validates tool calls before execution.

    The check function receives ``(step_name, args)`` and should raise
    ``ValueError`` if the invariant is violated. The agent suspends for
    human review — the tool does not execute.

    Example::

        Contract("max-charge", lambda step, args: (
            None if args.get("amount", 0) <= 100
            else (_ for _ in ()).throw(ValueError("too much"))
        ))

    Or more readably, use the ``@agent.contract`` decorator.
    """

    name: str
    check: Callable[[str, dict], None]
