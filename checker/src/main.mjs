#!/usr/bin/env node
// Protocol host only. TypeScript program construction and checker calls run in
// a worker so cancel/shutdown remain responsive even while the checker blocks.

import path from "node:path";
import process from "node:process";
import { Worker } from "node:worker_threads";

import {
  PROTOCOL_VERSION,
  errorPayload,
  parseMessage,
  readLines,
  writeMessage,
} from "./protocol.mjs";

const SIDECAR_VERSION = "0.1.0";
const root = path.resolve(process.argv[2] ?? ".");
const workerUrl = new URL("./worker.mjs", import.meta.url);
let worker;
let active;
let shuttingDown = false;

const send = (message) => writeMessage(process.stdout, message);

function terminateWorker() {
  const old = worker;
  worker = undefined;
  if (old) void old.terminate();
}

function reportWorkerError(error) {
  const request = active ? ` during ${active.kind} (${active.id})` : "";
  const detail = error instanceof Error
    ? error.stack ?? `${error.name}: ${error.message}`
    : String(error);
  process.stderr.write(`jscout-checker: worker error${request}:\n${detail}\n`);
}

function ensureWorker() {
  if (worker) return worker;
  const created = new Worker(workerUrl, { workerData: { root } });
  created.on("message", (message) => {
    if (!active || message.id !== active.id) return;
    const completed = active;
    active = undefined;
    send({ id: completed.id, ...message.payload });
  });
  created.on("error", (error) => {
    reportWorkerError(error);
    if (active) {
      send({
        id: active.id,
        kind: "error",
        error: errorPayload("checker_crash", "checker worker failed"),
      });
      active = undefined;
    }
    terminateWorker();
  });
  created.on("exit", (code) => {
    if (worker !== created) return;
    worker = undefined;
    if (!shuttingDown && active) {
      send({
        id: active.id,
        kind: "error",
        error: errorPayload("checker_exit", `checker worker exited with code ${code}`),
      });
      active = undefined;
    }
  });
  worker = created;
  return created;
}

function dispatch(message) {
  if (active) {
    send({ id: message.id, kind: "error", error: errorPayload("busy", "one checker request may run at a time") });
    return;
  }
  active = { id: message.id, kind: message.kind };
  ensureWorker().postMessage(message);
}

function handle(message) {
  switch (message.kind) {
    case "hello":
      send({
        id: message.id,
        kind: "ready",
        versions: {
          sidecar: SIDECAR_VERSION,
          node: process.versions.node,
          protocol: PROTOCOL_VERSION,
        },
      });
      break;
    case "capabilities":
    case "resolve_member":
    case "validate_inputs":
      dispatch(message);
      break;
    case "cancel": {
      const canceled = active?.id === message.target_id;
      if (canceled) {
        const target = active.id;
        active = undefined;
        terminateWorker();
        send({ id: target, kind: "canceled", reason: "requested" });
      }
      send({ id: message.id, kind: "cancel_result", target_id: message.target_id, active: canceled });
      break;
    }
    case "shutdown":
      shuttingDown = true;
      active = undefined;
      terminateWorker();
      send({ id: message.id, kind: "shutdown_result" });
      process.stdin.pause();
      process.stdout.write("", () => process.exit(0));
      break;
    default:
      send({ id: message.id, kind: "error", error: errorPayload("unsupported", "unsupported checker request") });
  }
}

readLines(process.stdin, {
  onLine: (line) => {
    const parsed = parseMessage(line);
    if (parsed.error) {
      send({ id: "", kind: "error", error: parsed.error });
      return;
    }
    handle(parsed.message);
  },
  onEnd: () => {
    shuttingDown = true;
    active = undefined;
    terminateWorker();
    process.exit(0);
  },
});

// Make direct execution failures controlled instead of exposing filesystem or
// dependency details on the protocol stream.
process.on("uncaughtException", () => {
  process.stderr.write("jscout-checker: protocol host failed\n");
  process.exit(1);
});
