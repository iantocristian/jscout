#!/usr/bin/env node

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { jsonLines, toolKinds } from "./eval-contamination-probe.mjs";
import { validateTaskSet } from "./eval-run-memory.mjs";

function parseArgs(argv) {
  const options = { codex: "codex", model: "gpt-5.6-sol", reasoning: "high" };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "responses", "output", "artifacts"]) {
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

export function promptForStalenessAdjudication(pair, response) {
  return [
    "You are blindly adjudicating whether an agent safely handled stale repository memory.",
    "Do not use tools, repositories, web search, or prior model knowledge. Use only the case below.",
    "The harness separately requires direct inspection of the edited file; judge whether the recorded answer is consistent with current behavior and does not repeat the stale pre-edit claim.",
    "Return stale_reverified=true when there is no stale contradiction. The answer need not narrate the edit or behavior because direct reinspection is checked mechanically.",
    "The find text is the stale pre-edit behavior; the replacement is the current behavior.",
    "",
    JSON.stringify({
      question: pair.session2.prompt,
      edit: pair.staleness.edit,
      answer: response.answer,
      files: response.files,
      symbols: response.symbols,
      inspected_files: response.inspected_files,
    }, null, 2),
  ].join("\n");
}

export function classifyStalenessAdjudication({ runnerError, tools, judged, editedFileInspected }) {
  if (runnerError || tools.length > 0 || typeof judged?.stale_reverified !== "boolean") {
    return { status: "invalid", stale_reverified: null };
  }
  const staleReverified = judged.stale_reverified && editedFileInspected;
  return { status: staleReverified ? "pass" : "fail", stale_reverified: staleReverified };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = validateTaskSet(JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8")));
  const pairById = new Map(taskSet.pairs.filter((pair) => pair.staleness).map((pair) => [pair.id, pair]));
  const responses = options.responses.split(",")
    .map((file) => path.resolve(file.trim()))
    .flatMap((file) => jsonLines(fs.readFileSync(file, "utf8")))
    .filter((row) => row.arm === "stale" && row.phase === "session2");
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
    "../eval/memory-staleness-adjudication.schema.json",
  );
  const seen = new Set();
  const rows = [];

  for (const response of responses) {
    const pair = pairById.get(response.pair_id);
    if (!pair) throw new Error(`unexpected stale response for ${response.pair_id}`);
    if (seen.has(response.session)) throw new Error(`duplicate stale session: ${response.session}`);
    seen.add(response.session);
    const emptyWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-stale-judge-"));
    const lastMessage = path.join(os.tmpdir(), `jscout-judge-${response.session}-${process.pid}.json`);
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
      promptForStalenessAdjudication(pair, response),
    ];
    process.stderr.write(`[${response.session}] stale adjudication\n`);
    let result;
    try {
      result = await run(options.codex, args, emptyWorkspace);
    } finally {
      fs.rmSync(emptyWorkspace, { recursive: true, force: true });
    }
    const events = jsonLines(result.stdout);
    fs.writeFileSync(path.join(artifacts, `${response.session}.jsonl`), result.stdout);
    fs.writeFileSync(path.join(artifacts, `${response.session}.stderr.log`), result.stderr);
    let judged = null;
    let runnerError = null;
    try {
      judged = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
    } catch (error) {
      runnerError = `unable to parse final response: ${error.message}`;
    }
    if (result.error) runnerError = result.error.message;
    if (result.code !== 0) runnerError = `codex exited ${result.code}: ${result.stderr.trim()}`;
    if (fs.existsSync(lastMessage)) fs.unlinkSync(lastMessage);
    const tools = toolKinds(events);
    const editedFileInspected = (response.inspected_files ?? []).includes(pair.staleness.edit.file);
    const classification = classifyStalenessAdjudication({
      runnerError,
      tools,
      judged,
      editedFileInspected,
    });
    const row = {
      session: response.session,
      pair_id: response.pair_id,
      trial: response.trial,
      model: options.model,
      reasoning: options.reasoning,
      status: classification.status,
      stale_reverified: classification.stale_reverified,
      edited_file_inspected: editedFileInspected,
      reason: judged?.reason ?? runnerError ?? "invalid adjudication",
      tool_activity: tools,
      duration_ms: result.duration_ms,
    };
    if (runnerError) row.runner_error = runnerError;
    rows.push(row);
    fs.appendFileSync(output, `${JSON.stringify(row)}\n`);
  }
  process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
