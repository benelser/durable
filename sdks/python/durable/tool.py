"""Tool decorator with automatic JSON Schema inference from type hints."""

from __future__ import annotations

import inspect
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Union, get_type_hints


# Type-hint to JSON Schema mapping
_TYPE_MAP = {
    str: {"type": "string"},
    int: {"type": "integer"},
    float: {"type": "number"},
    bool: {"type": "boolean"},
    list: {"type": "array"},
    dict: {"type": "object"},
}


def _type_to_schema(hint: Any) -> dict:
    """Convert a Python type hint to a JSON Schema fragment."""
    # Handle basic types
    if hint in _TYPE_MAP:
        return dict(_TYPE_MAP[hint])

    # Handle Optional[T] (Union[T, None])
    origin = getattr(hint, "__origin__", None)
    args = getattr(hint, "__args__", ())

    if origin is Union:
        # Optional[T] is Union[T, NoneType]
        non_none = [a for a in args if a is not type(None)]
        if len(non_none) == 1:
            return _type_to_schema(non_none[0])
        return {}

    if origin is list or origin is List:
        schema: dict = {"type": "array"}
        if args:
            schema["items"] = _type_to_schema(args[0])
        return schema

    if origin is dict or origin is Dict:
        return {"type": "object"}

    return {}


def _infer_schema(fn: Callable) -> dict:
    """Infer JSON Schema from function signature and type hints."""
    try:
        hints = get_type_hints(fn)
    except Exception:
        hints = {}

    sig = inspect.signature(fn)
    properties: Dict[str, dict] = {}
    required: List[str] = []

    for name, param in sig.parameters.items():
        if name in ("self", "cls", "return"):
            continue

        if name in hints:
            prop = _type_to_schema(hints[name])
        else:
            prop = {"type": "string"}  # Default to string

        properties[name] = prop

        if param.default is inspect.Parameter.empty:
            required.append(name)

    schema: dict = {"type": "object", "properties": properties}
    if required:
        schema["required"] = required
    return schema


@dataclass
class ToolDefinition:
    """Metadata for a registered tool."""

    name: str
    description: str
    parameters: dict
    requires_confirmation: bool = False
    handler: Optional[Callable] = None

    def to_dict(self) -> dict:
        d: dict = {
            "name": self.name,
            "description": self.description,
            "parameters": self.parameters,
        }
        if self.requires_confirmation:
            d["requires_confirmation"] = True
        return d


class ToolWrapper:
    """Wraps a function as a tool. Can be called directly or registered with an Agent."""

    def __init__(self, definition: ToolDefinition) -> None:
        self.definition = definition
        self._fn = definition.handler

    def __call__(self, **kwargs: Any) -> Any:
        if self._fn is None:
            raise RuntimeError(f"tool '{self.definition.name}' has no handler")
        return self._fn(**kwargs)

    @property
    def name(self) -> str:
        return self.definition.name


def tool(
    name: str,
    *,
    description: str = "",
    parameters: Optional[dict] = None,
    requires_confirmation: bool = False,
) -> Callable[[Callable], ToolWrapper]:
    """Decorator to create a tool from a function.

    Type hints are inspected to generate JSON Schema parameters.
    The function's first-line docstring is used as description if not provided.

    Example::

        @tool("get_weather", description="Get weather for a location")
        def get_weather(location: str) -> dict:
            return {"temp": 72, "conditions": "sunny"}

        agent.add_tool(get_weather)
    """

    def decorator(fn: Callable) -> ToolWrapper:
        schema = parameters if parameters is not None else _infer_schema(fn)
        desc = description or (fn.__doc__ or "").strip().split("\n")[0]

        defn = ToolDefinition(
            name=name,
            description=desc,
            parameters=schema,
            requires_confirmation=requires_confirmation,
            handler=fn,
        )
        return ToolWrapper(defn)

    return decorator
