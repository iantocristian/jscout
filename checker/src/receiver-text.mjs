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
