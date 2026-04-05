#!/usr/bin/env npx ts-node
/**
 * Minimal example: a tool that echoes its input.
 *
 * Run from the Rust runtime:
 *     ProcessToolHandler::new("npx")
 *         .with_args(vec!["ts-node", "sdks/typescript/examples/echo-tool.ts"])
 */

import { DurableToolServer } from "../src/tool-server";

const server = new DurableToolServer();

server.register("echo", "Echo back the input text", (args) => ({
  echoed: args.text || "",
}));

server.register("uppercase", "Convert text to uppercase", (args) => ({
  result: String(args.text || "").toUpperCase(),
}));

server.run();
