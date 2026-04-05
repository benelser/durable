/**
 * LLM adapter that wraps any provider to speak the durable protocol.
 * Zero dependencies — Node.js built-ins only.
 */

import { createMessageReader, writeMessage, ProtocolMessage } from "./protocol";
import { Message } from "./types";

type LlmHandler = (
  messages: Message[],
  tools?: unknown,
  model?: string
) => Promise<{ content?: string; text?: string; tool_calls?: unknown[] }>;

/**
 * Wrap an LLM provider to speak the durable NDJSON protocol on stdio.
 *
 * @example
 * ```ts
 * const adapter = new DurableLlmAdapter(async (messages, tools, model) => {
 *   const response = await openai.chat.completions.create({ messages, tools, model });
 *   return response.choices[0].message;
 * });
 * adapter.run();
 * ```
 */
export class DurableLlmAdapter {
  constructor(private handler: LlmHandler) {}

  async run(): Promise<void> {
    for await (const msg of createMessageReader()) {
      if (msg.type === "chat") {
        const messages = (msg.messages as Message[]) || [];
        const tools = msg.tools;
        const model = msg.model as string | undefined;
        await this.handleChat(messages, tools, model);
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

  private async handleChat(
    messages: Message[],
    tools: unknown,
    model?: string
  ): Promise<void> {
    try {
      const result = await this.handler(messages, tools, model);

      if (result.tool_calls && result.tool_calls.length > 0) {
        writeMessage({ type: "tool_calls", calls: result.tool_calls });
      } else {
        const content = result.content || result.text || "";
        writeMessage({ type: "text", content });
      }
    } catch (err: unknown) {
      writeMessage({
        type: "error",
        message: err instanceof Error ? err.message : String(err),
        retryable: true,
      });
    }
  }
}
