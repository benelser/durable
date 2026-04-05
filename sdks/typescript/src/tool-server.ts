/**
 * Tool server that speaks the durable NDJSON protocol on stdio.
 * Zero dependencies — Node.js built-ins only.
 */

import { createMessageReader, writeMessage, ProtocolMessage } from "./protocol";

type ToolHandler = (args: Record<string, unknown>) => unknown | Promise<unknown>;

interface RegisteredTool {
  name: string;
  description: string;
  handler: ToolHandler;
  parameters?: Record<string, unknown>;
}

/**
 * Register tool handlers and run the stdin/stdout dispatch loop.
 *
 * @example
 * ```ts
 * const server = new DurableToolServer();
 * server.register("greet", "Greet someone", (args) => `Hello, ${args.name}!`);
 * server.run();
 * ```
 */
export class DurableToolServer {
  private tools = new Map<string, RegisteredTool>();

  register(
    name: string,
    description: string,
    handler: ToolHandler,
    parameters?: Record<string, unknown>
  ): void {
    this.tools.set(name, { name, description, handler, parameters });
  }

  async run(): Promise<void> {
    for await (const msg of createMessageReader()) {
      if (msg.type === "execute") {
        const toolName = msg.tool_name as string;
        const args = (msg.arguments as Record<string, unknown>) || {};
        await this.handleExecute(toolName, args);
      } else if (msg.type === "heartbeat") {
        writeMessage({ type: "heartbeat_ack", timestamp: msg.timestamp });
      } else {
        writeMessage({
          type: "error",
          message: `unknown message type: ${msg.type}`,
          retryable: false,
        });
      }
    }
  }

  private async handleExecute(
    toolName: string,
    args: Record<string, unknown>
  ): Promise<void> {
    const tool = this.tools.get(toolName);
    if (!tool) {
      writeMessage({
        type: "error",
        message: `unknown tool: ${toolName}`,
        retryable: false,
      });
      return;
    }

    try {
      const result = await tool.handler(args);
      writeMessage({ type: "result", output: result });
    } catch (err: unknown) {
      writeMessage({
        type: "error",
        message: err instanceof Error ? err.message : String(err),
        retryable: false,
      });
    }
  }
}
