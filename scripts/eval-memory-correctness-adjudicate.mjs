#!/usr/bin/env node

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { jsonLines } from "./eval-contamination-probe.mjs";
import { validateTaskSet } from "./eval-run-memory.mjs";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-sol",
    reasoning: "high",
    "batch-size": "4",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "repository", "responses", "output", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  const batchSize = Number(options["batch-size"]);
  if (!Number.isInteger(batchSize) || batchSize < 1 || batchSize > 12) {
    throw new Error("--batch-size must be an integer from 1 through 12");
  }
  return { ...options, batchSize };
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

function chunks(entries, size) {
  const output = [];
  for (let index = 0; index < entries.length; index += size) {
    output.push(entries.slice(index, index + size));
  }
  return output;
}

export function buildBlindCases(taskSet, responses) {
  const pairById = new Map(taskSet.pairs.map((pair) => [pair.id, pair]));
  return responses
    .filter((row) => row.correct === false && !row.runner_error)
    .sort((left, right) => left.session.localeCompare(right.session))
    .map((response, index) => {
      const pair = pairById.get(response.pair_id);
      if (!pair) throw new Error(`unknown memory pair: ${response.pair_id}`);
      const task = pair[response.phase];
      if (!task) throw new Error(`${response.session}: unknown phase ${response.phase}`);
      return {
        blind_case: `C${String(index + 1).padStart(3, "0")}`,
        session: response.session,
        prompt: task.prompt,
        reference: task.gold,
        candidate: { files: response.files, symbols: response.symbols },
      };
    });
}

export function promptForCorrectnessAdjudication(cases) {
  return [
    "Blindly adjudicate repository-localization disagreements against the current frozen source.",
    "Use shell/file inspection as needed. Do not use jscout, web search, or prior run artifacts.",
    "You are not told which retrieval arm produced a candidate. Judge only the task, source, registered reference, and candidate sets.",
    "Return `correct` if the candidate is the best supported answer and the reference is wrong; `acceptable_alternative` if deviations are substantively equivalent; otherwise return `incorrect`.",
    "An explicitly excluded boundary, a missing required boundary, or a helper/constant substituted for a requested defining boundary is incorrect.",
    "Return exactly one result for every blind_case and preserve each blind_case identifier.",
    "",
    JSON.stringify(cases.map(({ blind_case, prompt, reference, candidate }) => ({
      blind_case,
      prompt,
      reference,
      candidate,
    })), null, 2),
  ].join("\n");
}

export function validateJudgments(cases, judged) {
  const expected = new Set(cases.map((entry) => entry.blind_case));
  const rows = judged?.cases;
  if (!Array.isArray(rows) || rows.length !== cases.length) {
    throw new Error(`adjudicator returned ${rows?.length ?? 0} cases; expected ${cases.length}`);
  }
  const seen = new Set();
  for (const row of rows) {
    if (!expected.has(row.blind_case) || seen.has(row.blind_case)) {
      throw new Error(`unexpected or duplicate blind case: ${row.blind_case}`);
    }
    seen.add(row.blind_case);
  }
  return rows;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = validateTaskSet(JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8")));
  const responses = options.responses.split(",")
    .map((file) => path.resolve(file.trim()))
    .flatMap((file) => jsonLines(fs.readFileSync(file, "utf8")));
  const cases = buildBlindCases(taskSet, responses);
  const caseById = new Map(cases.map((entry) => [entry.blind_case, entry]));
  const output = path.resolve(options.output);
  const artifacts = path.resolve(options.artifacts);
  const repository = path.resolve(options.repository);
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
    "../eval/memory-correctness-adjudication.schema.json",
  );
  const rows = [];

  for (const [batchIndex, batch] of chunks(cases, options.batchSize).entries()) {
    const label = `batch-${String(batchIndex + 1).padStart(2, "0")}`;
    const lastMessage = path.join(os.tmpdir(), `jscout-memory-judge-${label}-${process.pid}.json`);
    const args = [
      "exec",
      "--ignore-user-config",
      "--ephemeral",
      "--skip-git-repo-check",
      "--sandbox", "read-only",
      "--cd", repository,
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
      promptForCorrectnessAdjudication(batch),
    ];
    process.stderr.write(`[${label}] correctness adjudication (${batch.length} cases)\n`);
    const result = await run(options.codex, args, repository);
    fs.writeFileSync(path.join(artifacts, `${label}.jsonl`), result.stdout);
    fs.writeFileSync(path.join(artifacts, `${label}.stderr.log`), result.stderr);
    if (result.error) throw result.error;
    if (result.code !== 0) throw new Error(`codex exited ${result.code}: ${result.stderr.trim()}`);
    let judged;
    try {
      judged = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
    } finally {
      if (fs.existsSync(lastMessage)) fs.unlinkSync(lastMessage);
    }
    for (const judgment of validateJudgments(batch, judged)) {
      const source = caseById.get(judgment.blind_case);
      const row = {
        blind_case: judgment.blind_case,
        session: source.session,
        model: options.model,
        reasoning: options.reasoning,
        verdict: judgment.verdict,
        reason: judgment.reason,
        missing_boundaries: judgment.missing_boundaries,
        unjustified_boundaries: judgment.unjustified_boundaries,
        duration_ms: result.duration_ms,
      };
      rows.push(row);
      fs.appendFileSync(output, `${JSON.stringify(row)}\n`);
    }
  }
  process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
