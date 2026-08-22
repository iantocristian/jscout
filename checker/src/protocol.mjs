import readline from "node:readline";

export const PROTOCOL_VERSION = 4;
export const MAX_LINE_BYTES = 4 * 1024 * 1024;
export const PLAN_FRAME_MAX_BYTES = 1024 * 1024;
const PLAN_PAYLOAD_BYTES = 900 * 1024;

export function encodeMessage(message) {
  return `${JSON.stringify({ protocol: PROTOCOL_VERSION, ...message })}\n`;
}

export function messageBytes(message) {
  return Buffer.byteLength(encodeMessage(message));
}

export function writeMessage(stream, message) {
  const line = encodeMessage(message);
  if (Buffer.byteLength(line) > MAX_LINE_BYTES) {
    throw new RangeError("checker protocol line exceeds 4 MiB");
  }
  stream.write(line);
}

export function chunkPlanMemberFiles(files) {
  const chunks = [];
  let chunk = [];
  let chunkBytes = 0;
  for (const file of files) {
    if (typeof file !== "string") throw new TypeError("plan_members files must be strings");
    const encodedBytes = Buffer.byteLength(JSON.stringify(file));
    if (encodedBytes > PLAN_PAYLOAD_BYTES) {
      throw new RangeError("plan_members path exceeds the per-frame payload limit");
    }
    const separator = chunk.length === 0 ? 0 : 1;
    if (chunk.length > 0 && chunkBytes + separator + encodedBytes > PLAN_PAYLOAD_BYTES) {
      chunks.push(chunk);
      chunk = [];
      chunkBytes = 0;
    }
    chunkBytes += (chunk.length === 0 ? 0 : 1) + encodedBytes;
    chunk.push(file);
  }
  if (chunk.length > 0) chunks.push(chunk);
  return chunks;
}

const RESULT_SECTIONS = ["files", "projects", "configuration_problems"];

export function createPlanMemberResultPager(result) {
  if (!result || typeof result !== "object") throw new TypeError("plan_members result is required");
  if (!result.typescript || typeof result.typescript !== "object") {
    throw new TypeError("plan_members result requires TypeScript identity");
  }
  for (const section of RESULT_SECTIONS) {
    if (!Array.isArray(result[section])) {
      throw new TypeError(`plan_members result requires ${section}`);
    }
  }
  return {
    result,
    first: true,
    section: 0,
    index: 0,
    nextCursor: undefined,
  };
}

export function takePlanMemberResultPage(pager, id) {
  const page = {
    files: [],
    projects: [],
    configuration_problems: [],
  };
  if (pager.first) {
    page.typescript = pager.result.typescript;
    page.totals = {
      files: pager.result.files.length,
      projects: pager.result.projects.length,
      configuration_problems: pager.result.configuration_problems.length,
    };
  }

  let payloadBytes = 0;
  while (pager.section < RESULT_SECTIONS.length) {
    const section = RESULT_SECTIONS[pager.section];
    const items = pager.result[section];
    if (pager.index >= items.length) {
      pager.section += 1;
      pager.index = 0;
      continue;
    }
    const item = items[pager.index];
    const itemBytes = Buffer.byteLength(JSON.stringify(item));
    if (itemBytes > PLAN_PAYLOAD_BYTES) {
      throw new RangeError(`plan_members ${section} item exceeds the per-frame payload limit`);
    }
    const separator = page[section].length === 0 ? 0 : 1;
    if (payloadBytes > 0 && payloadBytes + separator + itemBytes > PLAN_PAYLOAD_BYTES) break;
    page[section].push(item);
    payloadBytes += separator + itemBytes;
    pager.index += 1;
  }

  const done = pager.section >= RESULT_SECTIONS.length;
  page.next_cursor = done ? null : `${pager.section}:${pager.index}`;
  const message = { id, kind: "plan_members_page", page };
  const bytes = messageBytes(message);
  if (bytes > PLAN_FRAME_MAX_BYTES) {
    throw new RangeError(`plan_members result page is ${bytes} bytes; limit is ${PLAN_FRAME_MAX_BYTES}`);
  }
  pager.first = false;
  pager.nextCursor = page.next_cursor ?? undefined;
  return message;
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
