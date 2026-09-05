#!/usr/bin/env node
// Protocol host only. TypeScript program construction and checker calls run in
// a worker so cancel/shutdown remain responsive even while the checker blocks.

import path from "node:path";
import process from "node:process";
import { Worker } from "node:worker_threads";

import {
  PLAN_FRAME_MAX_BYTES,
  PROTOCOL_VERSION,
  createPlanMemberResultPager,
  errorPayload,
  parseMessage,
  readLines,
  takePlanMemberResultPage,
  writeMessage,
} from "./protocol.mjs";

const SIDECAR_VERSION = "0.5.0";
const root = path.resolve(process.argv[2] ?? ".");
const workerUrl = new URL("./worker.mjs", import.meta.url);
let worker;
let active;
let planSession;
let canceledPlanId;
let shuttingDown = false;

const send = (message) => writeMessage(process.stdout, message);

function terminateWorker() {
  const old = worker;
  worker = undefined;
  if (old) void old.terminate();
}

function clearPlanSession() {
  planSession = undefined;
  active = undefined;
}

function failPlanSession(code, message) {
  const id = planSession?.id ?? active?.id ?? "";
  if (planSession?.phase === "processing") terminateWorker();
  clearPlanSession();
  send({ id, kind: "error", error: errorPayload(code, message) });
}

function sendPlanPage() {
  try {
    const message = takePlanMemberResultPage(planSession.pager, planSession.id);
    planSession.nextCursor = message.page.next_cursor ?? undefined;
    send(message);
    if (planSession.nextCursor === undefined) clearPlanSession();
  } catch (error) {
    failPlanSession("protocol", error instanceof Error ? error.message : String(error));
  }
}

function reportWorkerError(error) {
  const request = active ? ` during ${active.kind} (${active.id})` : "";
  const detail = error instanceof Error
    ? error.stack ?? `${error.name}: ${error.message}`
    : String(error);
  process.stderr.write(`jscout-checker: worker error${request}:\n${detail}\n`);
}

function workerFailureMessage(error) {
  const detail = error instanceof Error
    ? error.stack ?? `${error.name}: ${error.message}`
    : String(error);
  return detail.replaceAll(root, "<repository>").slice(0, 64 * 1024);
}

function ensureWorker() {
  if (worker) return worker;
  const created = new Worker(workerUrl, { workerData: { root } });
  created.on("message", (message) => {
    if (!active || message.id !== active.id) return;
    if (planSession?.phase === "processing") {
      if (message.payload?.kind === "error") {
        const completed = message.payload;
        clearPlanSession();
        send({ id: message.id, ...completed });
        return;
      }
      if (message.payload?.kind !== "plan_members_result") {
        failPlanSession("protocol", "checker worker returned an invalid plan_members result");
        return;
      }
      try {
        planSession.pager = createPlanMemberResultPager(message.payload.result);
        planSession.phase = "result";
        sendPlanPage();
      } catch (error) {
        failPlanSession("protocol", error instanceof Error ? error.message : String(error));
      }
      return;
    }
    const completed = active;
    active = undefined;
    send({ id: completed.id, ...message.payload });
  });
  created.on("error", (error) => {
    reportWorkerError(error);
    if (active) {
      const id = active.id;
      clearPlanSession();
      send({
        id,
        kind: "error",
        error: errorPayload("checker_crash", workerFailureMessage(error)),
      });
    }
    terminateWorker();
  });
  created.on("exit", (code) => {
    if (worker !== created) return;
    worker = undefined;
    if (!shuttingDown && active) {
      const id = active.id;
      clearPlanSession();
      send({
        id,
        kind: "error",
        error: errorPayload("checker_exit", `checker worker exited with code ${code}`),
      });
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

function beginPlanMembers(message) {
  if (active) {
    send({ id: message.id, kind: "error", error: errorPayload("busy", "one checker request may run at a time") });
    return;
  }
  if (!Number.isSafeInteger(message.total_files) || message.total_files < 0) {
    send({ id: message.id, kind: "error", error: errorPayload("protocol", "plan_members_begin requires total_files") });
    return;
  }
  // A begin frame is also the reset boundary. No discovery or ownership work
  // happens until the complete, globally grouped membership reaches finish.
  canceledPlanId = undefined;
  planSession = {
    id: message.id,
    phase: "upload",
    totalFiles: message.total_files,
    refreshConfig: message.refresh_config !== false,
    files: [],
    seen: new Set(),
  };
  active = { id: message.id, kind: "plan_members" };
  send({ id: message.id, kind: "plan_members_ready", total_files: message.total_files });
}

function addPlanMembers(message) {
  if (!planSession) {
    send({ id: message.id, kind: "error", error: errorPayload("protocol", "no matching plan_members upload") });
    return;
  }
  if (planSession.id !== message.id || planSession.phase !== "upload") {
    failPlanSession("protocol", "plan_members upload frame is out of sequence");
    return;
  }
  if (!Array.isArray(message.files) || message.files.some((file) => typeof file !== "string")) {
    failPlanSession("protocol", "plan_members_add requires string files");
    return;
  }
  if (planSession.files.length + message.files.length > planSession.totalFiles) {
    failPlanSession("protocol", "plan_members upload exceeds declared file count");
    return;
  }
  for (const file of message.files) {
    if (planSession.seen.has(file)) {
      failPlanSession("protocol", `plan_members upload repeats file: ${file.slice(0, 256)}`);
      return;
    }
    planSession.seen.add(file);
    planSession.files.push(file);
  }
  send({
    id: message.id,
    kind: "plan_members_add_result",
    received_files: planSession.files.length,
  });
}

function finishPlanMembers(message) {
  if (!planSession) {
    send({ id: message.id, kind: "error", error: errorPayload("protocol", "no matching plan_members upload") });
    return;
  }
  if (planSession.id !== message.id || planSession.phase !== "upload") {
    failPlanSession("protocol", "plan_members finish frame is out of sequence");
    return;
  }
  if (planSession.files.length !== planSession.totalFiles) {
    failPlanSession(
      "protocol",
      `plan_members upload received ${planSession.files.length} files; expected ${planSession.totalFiles}`,
    );
    return;
  }
  planSession.phase = "processing";
  planSession.seen = undefined;
  ensureWorker().postMessage({
    id: message.id,
    kind: "plan_members",
    files: planSession.files,
    refresh_config: planSession.refreshConfig,
  });
  planSession.files = undefined;
}

function nextPlanMembersPage(message) {
  if (!planSession) {
    send({ id: message.id, kind: "error", error: errorPayload("protocol", "no plan_members result page is pending") });
    return;
  }
  if (planSession.id !== message.id || planSession.phase !== "result") {
    failPlanSession("protocol", "plan_members result frame is out of sequence");
    return;
  }
  if (typeof message.cursor !== "string" || message.cursor !== planSession.nextCursor) {
    failPlanSession("protocol", "plan_members result cursor does not match");
    return;
  }
  sendPlanPage();
}

function handle(message) {
  // Cancellation can arrive between an acknowledged upload/page frame and
  // the client's next frame. The canceled response is already queued for the
  // logical session id, so discard that one trailing frame instead of adding
  // a second terminal response that would poison a reused Rust client.
  if (message.id === canceledPlanId && message.kind.startsWith("plan_members_")) return;
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
    case "resolve_members":
    case "validate_inputs":
    case "validate_project":
      dispatch(message);
      break;
    case "plan_members_begin":
      beginPlanMembers(message);
      break;
    case "plan_members_add":
      addPlanMembers(message);
      break;
    case "plan_members_finish":
      finishPlanMembers(message);
      break;
    case "plan_members_next":
      nextPlanMembersPage(message);
      break;
    case "cancel": {
      const canceled = active?.id === message.target_id;
      if (canceled) {
        const target = active.id;
        if (planSession) canceledPlanId = target;
        clearPlanSession();
        terminateWorker();
        send({ id: target, kind: "canceled", reason: "requested" });
      }
      send({ id: message.id, kind: "cancel_result", target_id: message.target_id, active: canceled });
      break;
    }
    case "shutdown":
      shuttingDown = true;
      clearPlanSession();
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
      if (planSession) {
        failPlanSession(parsed.error.code, parsed.error.message);
      } else {
        send({ id: "", kind: "error", error: parsed.error });
      }
      return;
    }
    if (parsed.message.kind.startsWith("plan_members_")
      && Buffer.byteLength(line) + 1 > PLAN_FRAME_MAX_BYTES) {
      if (planSession) {
        failPlanSession("oversized_plan_frame", "plan_members protocol frame exceeds 1 MiB");
      } else {
        send({
          id: parsed.message.id,
          kind: "error",
          error: errorPayload("oversized_plan_frame", "plan_members protocol frame exceeds 1 MiB"),
        });
      }
      return;
    }
    handle(parsed.message);
  },
  onEnd: () => {
    shuttingDown = true;
    clearPlanSession();
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
