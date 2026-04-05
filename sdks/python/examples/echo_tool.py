#!/usr/bin/env python3
"""Minimal example: a tool that echoes its input.

Run from the Rust runtime:
    ProcessToolHandler::new("python3")
        .with_args(vec!["sdks/python/examples/echo_tool.py"])
"""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable_sdk import DurableToolServer

server = DurableToolServer()


@server.tool("echo", "Echo back the input text")
def echo(text=""):
    return {"echoed": text}


@server.tool("uppercase", "Convert text to uppercase")
def uppercase(text=""):
    return {"result": text.upper()}


if __name__ == "__main__":
    server.run()
