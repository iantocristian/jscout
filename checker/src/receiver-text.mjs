import path from "node:path";

// Display metadata only; declaration targets and confidence never use this text.
export const MAX_RECEIVER_TYPE_BYTES = 4 * 1024;
export const RECEIVER_TYPE_TRUNCATION = "... [truncated]";

export function boundedReceiverType(text) {
  if (Buffer.byteLength(text) <= MAX_RECEIVER_TYPE_BYTES) return text;
  const budget = MAX_RECEIVER_TYPE_BYTES - Buffer.byteLength(RECEIVER_TYPE_TRUNCATION);
  let bytes = 0;
  let end = 0;
  for (const character of text) {
    const size = Buffer.byteLength(character);
    if (bytes + size > budget) break;
    bytes += size;
    end += character.length;
  }
  // Copy the bounded prefix: a substring can otherwise retain the huge backing string.
  return Buffer.from(text.slice(0, end)).toString() + RECEIVER_TYPE_TRUNCATION;
}

// Normalize even a trailing import specifier cut before its closing quote.
// Repair lone UTF-16 surrogates first so Rust can decode the JSON response.
export function normalizeReceiverType(root, text) {
  const normalized = text.toWellFormed().replace(/import\("([^"]*)("|$)/gu, (_, spec, closingQuote) => {
    let display = spec;
    if (path.isAbsolute(spec)) {
      const relative = path.relative(root, spec);
      const inside = relative === "" || (relative !== ".."
        && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
      display = inside ? (relative.split(path.sep).join("/") || ".")
        : `outside:${path.basename(spec) || "unknown"}`;
    }
    return `import("${display}${closingQuote}`;
  });
  return boundedReceiverType(normalized);
}
