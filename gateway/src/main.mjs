#!/usr/bin/env node
// jscout pi-ai gateway: a transport adapter between Rust and pi-ai.
//
// stdin/stdout carry the versioned JSONL protocol; stderr carries sanitized
// diagnostics. The gateway owns provider registration, auth, request
// execution, and cancellation. It is not an agent: no tool execution, no
// repository access, no SQLite.

import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);

/// The pi-ai exports map does not expose ./package.json; walk up from the
/// resolved ESM entry point to read the installed version.
function piAiVersion() {
  let dir = path.dirname(fileURLToPath(import.meta.resolve("@earendil-works/pi-ai")));
  while (dir !== path.dirname(dir)) {
    const candidate = path.join(dir, "package.json");
    if (fs.existsSync(candidate)) {
      const pkg = JSON.parse(fs.readFileSync(candidate, "utf8"));
      if (pkg.name === "@earendil-works/pi-ai") return pkg.version;
    }
    dir = path.dirname(dir);
  }
  return "unknown";
}

const MINIMUM_NODE = [22, 19, 0];

function nodeVersionSupported() {
  const parts = process.versions.node.split(".").map((part) => Number.parseInt(part, 10));
  for (let index = 0; index < MINIMUM_NODE.length; index += 1) {
    const actual = parts[index] ?? 0;
    if (actual !== MINIMUM_NODE[index]) return actual > MINIMUM_NODE[index];
  }
  return true;
}

async function main() {
  if (!nodeVersionSupported()) {
    process.stderr.write(
      `jscout-pi-ai-gateway requires Node >= ${MINIMUM_NODE.join(".")}; running ${process.versions.node}\n`,
    );
    process.exit(1);
    return;
  }

  // Keep the version gate ahead of pi-ai imports: an unsupported runtime
  // should fail with one controlled diagnostic, not an adapter syntax error.
  const [{ createGatewayState, handleMessage }, protocol] = await Promise.all([
    import("./server.mjs"),
    import("./protocol.mjs"),
  ]);
  const { errorPayload, parseMessage, readLines, writeMessage } = protocol;

  const state = createGatewayState({
    env: process.env,
    versions: {
      gateway: require("../package.json").version,
      pi_ai: piAiVersion(),
      node: process.versions.node,
    },
  });
  const send = (message) => writeMessage(process.stdout, message);

  readLines(process.stdin, {
    onLine: (line) => {
      const { message, error } = parseMessage(line);
      if (error) {
        send({ id: "", kind: "error", error: errorPayload("protocol", error) });
        return;
      }
      // Dispatched without awaiting so cancel can interleave with an active
      // completion; per-request ordering is preserved inside handleMessage.
      handleMessage(state, message, send).catch((failure) => {
        send({
          id: message.id,
          kind: "error",
          error: errorPayload("internal", "internal gateway failure"),
        });
      });
    },
    onOverflow: (overflow) => {
      // Framing is unrecoverable after an oversized line; report and exit.
      send({ id: "", kind: "error", error: errorPayload("oversized_line", overflow.message) });
      process.stderr.write(`${overflow.message}\n`);
      process.exit(1);
    },
    onEnd: () => {
      state.abortActive("stdin closed");
      process.exit(0);
    },
  });
}

main().catch((failure) => {
  const message = failure?.code === "ERR_MODULE_NOT_FOUND"
    ? "gateway dependencies are missing; reinstall the bundled gateway"
    : "gateway failed to initialize";
  process.stderr.write(`jscout-pi-ai-gateway: ${message}\n`);
  process.exitCode = 1;
});
