// Versioned JSONL framing for the jscout gateway.
//
// stdout carries exactly one JSON object per line; stderr carries sanitized
// human diagnostics. A line that exceeds the byte guard is unrecoverable
// corruption: the reader cannot know where the next message begins, so the
// process reports and exits instead of resynchronizing.

export const PROTOCOL_VERSION = 1;
export const MAX_LINE_BYTES = 16 * 1024 * 1024;

export class LineOverflowError extends Error {
  constructor(bytes) {
    super(`protocol line exceeds ${MAX_LINE_BYTES} bytes (${bytes})`);
    this.name = "LineOverflowError";
  }
}

/// Split a byte stream into UTF-8 lines with a hard per-line byte cap.
/// `onLine` may be async; lines are dispatched without awaiting so a
/// `cancel` can be handled while a `complete` promise is pending.
export function readLines(stream, { onLine, onOverflow, onEnd }) {
  let buffered = Buffer.alloc(0);
  let overflowed = false;
  stream.on("data", (chunk) => {
    if (overflowed) return;
    buffered = buffered.length === 0 ? chunk : Buffer.concat([buffered, chunk]);
    let newline;
    while ((newline = buffered.indexOf(0x0a)) !== -1) {
      const line = buffered.subarray(0, newline);
      buffered = buffered.subarray(newline + 1);
      if (line.length > MAX_LINE_BYTES) {
        overflowed = true;
        onOverflow(new LineOverflowError(line.length));
        return;
      }
      const text = line.toString("utf8").trim();
      if (text.length > 0) onLine(text);
    }
    if (buffered.length > MAX_LINE_BYTES) {
      overflowed = true;
      onOverflow(new LineOverflowError(buffered.length));
    }
  });
  stream.on("end", () => {
    if (!overflowed && onEnd) onEnd();
  });
}

export function writeMessage(stream, message) {
  stream.write(`${JSON.stringify({ protocol: PROTOCOL_VERSION, ...message })}\n`);
}

/// Stable, sanitized error payload. `message` comes from a controlled set of
/// template strings or a provider error message that is length-capped; it must
/// never include request payloads, environment values, or credentials.
export function errorPayload(code, message, { retryable = false, capacity = false } = {}) {
  return {
    code,
    message: String(message ?? "").slice(0, 2000),
    retryable,
    capacity,
  };
}

export function parseMessage(text) {
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    return { error: "line is not valid JSON" };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { error: "message must be a JSON object" };
  }
  if (value.protocol !== PROTOCOL_VERSION) {
    return { error: `unsupported protocol ${JSON.stringify(value.protocol)}; expected ${PROTOCOL_VERSION}` };
  }
  if (typeof value.id !== "string" || value.id.length === 0) {
    return { error: "message id must be a non-empty string" };
  }
  if (typeof value.kind !== "string") {
    return { error: "message kind must be a string" };
  }
  return { message: value };
}
