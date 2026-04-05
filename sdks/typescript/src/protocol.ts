/**
 * Newline-delimited JSON protocol for the durable runtime.
 * Zero dependencies — uses only Node.js built-ins.
 */

import * as readline from "readline";

export const PROTOCOL_VERSION = "1.0";

export interface ProtocolMessage {
  type: string;
  [key: string]: unknown;
}

/** Read one NDJSON message from stdin. */
export function createMessageReader(
  input: NodeJS.ReadableStream = process.stdin
): AsyncIterable<ProtocolMessage> {
  const rl = readline.createInterface({ input });
  return {
    [Symbol.asyncIterator]() {
      const lineIterator = rl[Symbol.asyncIterator]();
      return {
        async next() {
          const result = await lineIterator.next();
          if (result.done) return { done: true, value: undefined };
          try {
            const msg = JSON.parse(result.value as string) as ProtocolMessage;
            return { done: false, value: msg };
          } catch {
            return { done: false, value: { type: "error", message: "parse error" } };
          }
        },
      };
    },
  };
}

/** Write one NDJSON message to stdout. */
export function writeMessage(
  msg: ProtocolMessage,
  output: NodeJS.WritableStream = process.stdout
): void {
  output.write(JSON.stringify(msg) + "\n");
}
