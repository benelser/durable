"""Newline-delimited JSON protocol for communicating with the Rust runtime.

Each message is a single line of JSON followed by a newline.
"""

import json
import sys
from typing import Any, Optional


PROTOCOL_VERSION = "1.0"


class ProtocolMessage:
    """A protocol message with type and payload."""

    def __init__(self, msg_type: str, **kwargs):
        self.type = msg_type
        self.payload = kwargs

    def to_dict(self) -> dict:
        d = {"type": self.type}
        d.update(self.payload)
        return d

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"))

    @staticmethod
    def from_dict(d: dict) -> "ProtocolMessage":
        msg_type = d.pop("type", "unknown")
        # Remove envelope fields
        d.pop("v", None)
        d.pop("id", None)
        d.pop("ts", None)
        return ProtocolMessage(msg_type, **d)

    @staticmethod
    def from_json(line: str) -> "ProtocolMessage":
        return ProtocolMessage.from_dict(json.loads(line.strip()))


def read_message(stream=None) -> Optional[ProtocolMessage]:
    """Read one NDJSON message from a stream (default: stdin)."""
    if stream is None:
        stream = sys.stdin
    line = stream.readline()
    if not line:
        return None
    return ProtocolMessage.from_json(line)


def write_message(msg: ProtocolMessage, stream=None) -> None:
    """Write one NDJSON message to a stream (default: stdout)."""
    if stream is None:
        stream = sys.stdout
    stream.write(msg.to_json() + "\n")
    stream.flush()
