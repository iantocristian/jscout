#!/usr/bin/env node

import { execFileSync, spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

const [mode, output, command, ...args] = process.argv.slice(2);
if (!["cancel", "crash", "none"].includes(mode) || !output || !command) {
  throw new Error("usage: checker-fault-runner.mjs cancel|crash|none OUTPUT COMMAND [ARG ...]");
}

const child = spawn(command, args, { env: process.env, stdio: ["ignore", "pipe", "pipe"] });
const started = performance.now();
let stdout = "";
let stderr = "";
let injected;
let candidateSince;
child.stdout.setEncoding("utf8");
child.stderr.setEncoding("utf8");
child.stdout.on("data", (chunk) => { stdout = `${stdout}${chunk}`.slice(-256 * 1024); });
child.stderr.on("data", (chunk) => { stderr = `${stderr}${chunk}`.slice(-256 * 1024); });

function processTree() {
  const rows = execFileSync("ps", ["-axo", "pid=,ppid=,command="], {
    encoding: "utf8",
  }).trim().split("\n").map((line) => {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/u);
    return match ? { pid: Number(match[1]), ppid: Number(match[2]), command: match[3] } : null;
  }).filter(Boolean);
  const descendants = new Set([child.pid]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const row of rows) {
      if (descendants.has(row.ppid) && !descendants.has(row.pid)) {
        descendants.add(row.pid);
        changed = true;
      }
    }
  }
  return rows.filter((row) => descendants.has(row.pid));
}

const timer = setInterval(() => {
  if (mode === "none" || injected) return;
  let tree;
  try {
    tree = processTree();
  } catch {
    return;
  }
  const checkers = tree.filter((row) => (
    row.command.includes("checker/src/main.mjs")
    && !row.command.includes(" --sidecar-path ")
  ));
  if (checkers.length === 0) {
    candidateSince = undefined;
    return;
  }
  candidateSince ??= performance.now();
  if (performance.now() - candidateSince < 1500) return;
  const target = mode === "crash"
    ? checkers[0]
    : tree.find((row) => row.pid === child.pid);
  if (!target) return;
  const signal = mode === "crash" ? "SIGKILL" : "SIGINT";
  process.kill(target.pid, signal);
  injected = {
    elapsed_ms: performance.now() - started,
    pid: target.pid,
    signal,
    command: target.command,
    concurrent_checkers: checkers.length,
  };
}, 50);

const status = await new Promise((resolve) => {
  child.once("error", (error) => resolve({ error: error.message }));
  child.once("exit", (code, signal) => resolve({ code, signal }));
});
clearInterval(timer);
writeFileSync(output, `${JSON.stringify({
  mode,
  elapsed_ms: performance.now() - started,
  injected,
  status,
  stdout,
  stderr,
}, null, 2)}\n`);
if (mode !== "none" && !injected) process.exitCode = 2;
