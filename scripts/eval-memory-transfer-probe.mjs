#!/usr/bin/env node

import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { jsonLines, setOverlap, toolKinds } from "./eval-contamination-probe.mjs";
import { validateTaskSet } from "./eval-run-memory.mjs";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
    trial: "admission",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "session1-responses", "output", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function run(command, args, cwd) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => resolve({
      code: null, stdout, stderr, error, duration_ms: Date.now() - started,
    }));
    child.on("close", (code) => resolve({
      code, stdout, stderr, error: null, duration_ms: Date.now() - started,
    }));
  });
}

function sameSet(left = [], right = []) {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

export function classifyTransferProbe({ runnerError, tools, gold, answer }) {
  if (runnerError || tools.length > 0) return "invalid";
  return sameSet(gold.files, answer.files) && sameSet(gold.symbols, answer.symbols)
    ? "fail"
    : "pass";
}

export function promptForTransfer(pair, first) {
  return [
    "This is a transfer-triviality probe. Do not use tools, shell, filesystem, repositories, web search, or prior model knowledge.",
    "Use only the previous session's final answer quoted below.",
    "Determine the production files and symbols requested by the follow-up only when that answer itself contains enough information.",
    "Do not infer paths or symbols from naming conventions. When the answer is insufficient, return empty files/symbols and confidence `unknown`.",
    "",
    "<previous-session-answer>",
    JSON.stringify({ answer: first.answer, files: first.files, symbols: first.symbols }),
    "</previous-session-answer>",
    "",
    `<follow-up>${pair.session2.prompt}</follow-up>`,
  ].join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskPath = path.resolve(options.tasks);
  const taskSet = validateTaskSet(
    JSON.parse(fs.readFileSync(taskPath, "utf8")),
    { requireTransfer: false },
  );
  const session1Responses = options["session1-responses"]
    .split(",")
    .map((file) => path.resolve(file.trim()))
    .flatMap((file) => jsonLines(fs.readFileSync(file, "utf8")))
    .filter((row) => row.phase === "session1" && row.arm === "warm");
  const firstByPair = new Map(session1Responses.map((row) => [row.pair_id, row]));
  const output = path.resolve(options.output);
  const artifacts = path.resolve(options.artifacts);
  if (fs.existsSync(output) && fs.statSync(output).size > 0) {
    throw new Error(`refusing to append to non-empty output: ${output}`);
  }
  if (fs.existsSync(artifacts) && fs.readdirSync(artifacts).length > 0) {
    throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifacts}`);
  }
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.mkdirSync(artifacts, { recursive: true });
  const schema = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../eval/contamination-probe.schema.json",
  );
  const rows = [];

  for (const pair of taskSet.pairs) {
    const first = firstByPair.get(pair.id);
    if (!first || first.runner_error || first.correct !== true) {
      throw new Error(`${pair.id}: transfer probe requires a correct, error-free session-1 response`);
    }
    const emptyWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-transfer-probe-"));
    const session = `memory-transfer-${pair.id}-${options.trial}`;
    const lastMessage = path.join(os.tmpdir(), `jscout-${session}-${process.pid}.json`);
    const args = [
      "exec",
      "--ignore-user-config",
      "--ephemeral",
      "--skip-git-repo-check",
      "--sandbox", "read-only",
      "--cd", emptyWorkspace,
      "--model", options.model,
      "--json",
      "--output-schema", schema,
      "--output-last-message", lastMessage,
      "--config", `model_reasoning_effort=${JSON.stringify(options.reasoning)}`,
      "--config", "approval_policy=\"never\"",
      "--config", "features.multi_agent=false",
      "--config", "features.apps=false",
      "--config", "features.browser_use=false",
      "--config", "features.computer_use=false",
      "--config", "features.plugins=false",
      "--config", "tools.web_search=false",
      promptForTransfer(pair, first),
    ];
    process.stderr.write(`[${pair.id}] transfer probe\n`);
    let result;
    try {
      result = await run(options.codex, args, emptyWorkspace);
    } finally {
      fs.rmSync(emptyWorkspace, { recursive: true, force: true });
    }
    const events = jsonLines(result.stdout);
    fs.writeFileSync(path.join(artifacts, `${session}.jsonl`), result.stdout);
    fs.writeFileSync(path.join(artifacts, `${session}.stderr.log`), result.stderr);
    let answer = { answer: "", files: [], symbols: [], confidence: "unknown" };
    let runnerError = null;
    try {
      answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
    } catch (error) {
      runnerError = `unable to parse final response: ${error.message}`;
    }
    if (result.error) runnerError = result.error.message;
    if (result.code !== 0) runnerError = `codex exited ${result.code}: ${result.stderr.trim()}`;
    if (fs.existsSync(lastMessage)) fs.unlinkSync(lastMessage);
    const tools = toolKinds(events);
    const status = classifyTransferProbe({
      runnerError,
      tools,
      gold: pair.session2.gold,
      answer,
    });
    const firstPayload = JSON.stringify({
      answer: first.answer,
      files: first.files,
      symbols: first.symbols,
    });
    const row = {
      pair_id: pair.id,
      status,
      model: options.model,
      reasoning: options.reasoning,
      source_session: first.session,
      source_answer_sha256: crypto.createHash("sha256").update(firstPayload).digest("hex"),
      probe_session: session,
      confidence: answer.confidence ?? "unknown",
      files: answer.files ?? [],
      symbols: answer.symbols ?? [],
      file_overlap: setOverlap(pair.session2.gold.files, answer.files),
      symbol_overlap: setOverlap(pair.session2.gold.symbols, answer.symbols),
      tool_activity: tools,
      duration_ms: result.duration_ms,
    };
    if (runnerError) row.runner_error = runnerError;
    rows.push(row);
    fs.appendFileSync(output, `${JSON.stringify(row)}\n`);
  }

  if (options["certified-tasks"]) {
    const target = path.resolve(options["certified-tasks"]);
    if (fs.existsSync(target)) throw new Error(`refusing to overwrite certified task set: ${target}`);
    const rowByPair = new Map(rows.map((row) => [row.pair_id, row]));
    for (const pair of taskSet.pairs) {
      pair.admission.transfer_triviality = rowByPair.get(pair.id);
    }
    fs.writeFileSync(target, `${JSON.stringify(taskSet, null, 2)}\n`, { flag: "wx" });
  }
  process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
