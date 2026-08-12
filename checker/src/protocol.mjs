import readline from "node:readline";

export const PROTOCOL_VERSION = 1;
export const MAX_LINE_BYTES = 4 * 1024 * 1024;

export function writeMessage(stream, message) {
  stream.write(`${JSON.stringify({ protocol: PROTOCOL_VERSION, ...message })}\n`);
}

export function errorPayload(code, message) {
  return { code, message };
}

export function parseMessage(line) {
  if (Buffer.byteLength(line) > MAX_LINE_BYTES) {
    return { error: errorPayload("oversized_line", "checker protocol line exceeds 4 MiB") };
  }
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return { error: errorPayload("protocol", "checker protocol line is not valid JSON") };
  }
  if (message?.protocol !== PROTOCOL_VERSION) {
    return { error: errorPayload("protocol_version", `checker requires protocol ${PROTOCOL_VERSION}`) };
  }
  if (typeof message.id !== "string" || typeof message.kind !== "string") {
    return { error: errorPayload("protocol", "checker message requires string id and kind") };
  }
  return { message };
}

export function readLines(stream, handlers) {
  const lines = readline.createInterface({ input: stream, crlfDelay: Infinity });
  lines.on("line", (line) => handlers.onLine(line));
  lines.on("close", () => handlers.onEnd());
}
