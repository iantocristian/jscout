// Versioned JSONL framing for the jscout gateway.
//
// stdout carries exactly one JSON object per line; stderr carries sanitized
// human diagnostics. A line that exceeds the byte guard is unrecoverable
// corruption: the reader cannot know where the next message begins, so the
// process reports and exits instead of resynchronizing.

export const PROTOCOL_VERSION = 1;
export const MAX_LINE_BYTES = 16 * 1024 * 1024;
const MAX_ERROR_MESSAGE_BYTES = 2000;

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

/// Defense-in-depth for controlled diagnostics. Provider failures are mapped
/// onto stable messages before they reach this boundary; these replacements
/// keep an accidentally forwarded token or credentialed URL out of the wire
/// protocol and therefore out of terminal logs.
export function sanitizeErrorMessage(message) {
  let text = String(message ?? "").replace(/[\r\n\t]+/gu, " ");
  text = text.replace(/\b(?:bearer|basic)\s+[a-z0-9._~+/=-]+/giu, (match) => {
    const scheme = match.slice(0, match.indexOf(" "));
    return `${scheme} [REDACTED]`;
  });
  text = text.replace(
    /\b(api[_-]?key|access[_-]?token|refresh[_-]?token|authorization|password|secret)\b(\s*[:=]\s*)["']?[^\s,"'}]+/giu,
    "$1$2[REDACTED]",
  );
  text = text.replace(
    /\b(?:[srp]k[-_][a-z0-9_-]{8,}|gh[pousr]_[a-z0-9]{20,}|github_pat_[a-z0-9_]{20,}|xox[baprs]-[a-z0-9-]{8,}|AIza[a-z0-9_-]{20,})\b/giu,
    "[REDACTED]",
  );
  text = text.replace(/https?:\/\/[^\s<>"']+/giu, (candidate) => sanitizeUrl(candidate));
  return text.slice(0, MAX_ERROR_MESSAGE_BYTES);
}

function sanitizeUrl(candidate) {
  try {
    const url = new URL(candidate);
    if (url.username) url.username = "REDACTED";
    if (url.password) url.password = "REDACTED";
    for (const key of url.searchParams.keys()) {
      if (/key|token|secret|password|auth/iu.test(key)) url.searchParams.set(key, "REDACTED");
    }
    if (url.hash) url.hash = "#REDACTED";
    return url.toString();
  } catch {
    return "[REDACTED_URL]";
  }
}

/// Stable, sanitized error payload. Callers should still prefer controlled
/// messages over raw provider/SDK text; truncation alone is not redaction.
export function errorPayload(code, message, { retryable = false, capacity = false } = {}) {
  return {
    code,
    message: sanitizeErrorMessage(message),
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
