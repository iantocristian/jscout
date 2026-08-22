#!/usr/bin/env node

import { execFileSync, spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

const [output, command, ...args] = process.argv.slice(2);
if (!output || !command) {
  throw new Error("usage: tree-memory.mjs OUTPUT COMMAND [ARG ...]");
}

const child = spawn(command, args, { env: process.env, stdio: "inherit" });
const started = performance.now();
let samples = 0;
let peakRssBytes = 0;
let peakProcesses = [];
let peakTargetRssBytes = 0;
let peakTargetProcesses = [];
let sampleError;
const targetTimeline = [];

function sample() {
  let rows;
  try {
    rows = execFileSync("ps", ["-axo", "pid=,ppid=,rss=,command="], {
      encoding: "utf8",
    }).trim().split("\n").map((line) => {
      const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/u);
      return match ? {
        pid: Number(match[1]),
        ppid: Number(match[2]),
        rss_bytes: Number(match[3]) * 1024,
        command: match[4],
      } : null;
    }).filter(Boolean);
  } catch (error) {
    sampleError ??= error.message;
    return;
  }
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
  const processes = rows.filter((row) => descendants.has(row.pid));
  const rssBytes = processes.reduce((total, row) => total + row.rss_bytes, 0);
  const targets = processes.filter((row) => (
    (row.command.includes("checker/src/main.mjs")
      && !row.command.includes("ai-pipe.mjs")
      && !row.command.includes("tree-memory.mjs"))
    || (/\/jscout(?:\s|$)/u.test(row.command) && !row.command.includes("--binary"))
  ));
  const targetRssBytes = targets.reduce((total, row) => total + row.rss_bytes, 0);
  samples += 1;
  if (rssBytes > peakRssBytes) {
    peakRssBytes = rssBytes;
    peakProcesses = processes.toSorted((left, right) => right.rss_bytes - left.rss_bytes);
  }
  if (targetRssBytes > peakTargetRssBytes) {
    peakTargetRssBytes = targetRssBytes;
    peakTargetProcesses = targets.toSorted((left, right) => right.rss_bytes - left.rss_bytes);
  }
  if (samples % 50 === 0) {
    targetTimeline.push({
      elapsed_ms: performance.now() - started,
      rss_bytes: targetRssBytes,
      checker_rss_bytes: targets
        .filter((row) => row.command.includes("checker/src/main.mjs"))
        .map((row) => row.rss_bytes)
        .toSorted((left, right) => right - left),
    });
  }
}

sample();
const timer = setInterval(sample, 100);
const status = await new Promise((resolve) => {
  child.once("error", (error) => resolve({ error: error.message }));
  child.once("exit", (code, signal) => resolve({ code, signal }));
});
clearInterval(timer);
sample();
writeFileSync(output, `${JSON.stringify({
  elapsed_ms: performance.now() - started,
  samples,
  peak_rss_bytes: peakRssBytes,
  peak_processes: peakProcesses,
  peak_target_rss_bytes: peakTargetRssBytes,
  peak_target_processes: peakTargetProcesses,
  target_timeline: targetTimeline,
  sample_error: sampleError,
  status,
}, null, 2)}\n`);
if (status.error) throw new Error(status.error);
if (status.signal) process.kill(process.pid, status.signal);
process.exitCode = status.code ?? 1;
