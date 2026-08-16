#!/usr/bin/env node

// Blindly adjudicate PR-replay reference-file omissions.
//
// eval-pr-grade.mjs leaves every gold code file an arm did not patch in
// `coverage.patched.pending_adjudication`. The reference patch is ONE valid
// implementation, so an untouched reference file is not automatically a
// defect: a different design may satisfy the same behavioral contract. This
// script asks a judge model, once per arm, whether each pending file's
// contribution is present elsewhere in the arm's patch (alternative_covered),
// incidental to the task (not_required), or genuinely absent (omission).
//
// Blindness: the judge sees only the task story, the arm's patch, and the
// reference diffs for the files that arm did not touch. It never sees the
// profile, treatment, trial, execution model, session id, or any other arm.
// It reads the unmodified base snapshot read-only for context.
//
// One model call per arm; calls run sequentially.

import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { jsonLines } from "./eval-contamination-probe.mjs";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-sol",
    reasoning: "high",
    resume: "false",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["gold", "runs", "repository", "output", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return { ...options, resume: options.resume === "true" };
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

// Split a multi-file patch into per-file diffs keyed by the post-image path.
export function splitPatchByFile(patch) {
  const sections = new Map();
  let file = null;
  let lines = [];
  const flush = () => {
    if (file !== null) sections.set(file, lines.join("\n").trimEnd());
  };
  for (const line of patch.split(/\r?\n/)) {
    if (line.startsWith("diff --git ")) {
      flush();
      file = null;
      lines = [line];
      continue;
    }
    if (lines.length === 0) continue;
    lines.push(line);
    if (file === null && line.startsWith("+++ ")) {
      const target = line.slice(4).trim();
      file = target === "/dev/null" ? null : target.replace(/^[abwic]\//, "");
    }
  }
  flush();
  return sections;
}

// Blind identifiers must not encode profile, treatment, trial, or run order.
export function blindId(session) {
  return `A${crypto.createHash("sha256").update(session).digest("hex").slice(0, 8)}`;
}

export function buildArmCases({ responses, goldSections, pendingBySession, patchBySession }) {
  return responses
    .map((response) => {
      const pending = pendingBySession.get(response.session) ?? [];
      const patch = patchBySession.get(response.session);
      if (pending.length === 0 || !patch) return null;
      const goldDiffs = pending.map((file) => {
        const diff = goldSections.get(file);
        if (!diff) throw new Error(`no reference diff for gold file: ${file}`);
        return { gold_file: file, reference_diff: diff };
      });
      return {
        blind_case: blindId(response.session),
        session: response.session,
        trial: response.trial ?? null,
        profile: response.profile,
        treatment: response.treatment,
        execution_model: response.model,
        patch,
        gold_diffs: goldDiffs,
      };
    })
    .filter(Boolean)
    .sort((left, right) => left.blind_case.localeCompare(right.blind_case));
}

export function promptForOmissionAdjudication({ story, patch, goldDiffs }) {
  return [
    "Blindly adjudicate reference-file omissions for ONE candidate implementation of a repository task.",
    "",
    "The working directory holds the unmodified base snapshot the candidate started from; neither the candidate patch nor the reference change is applied to it. Read it read-only for context.",
    "Do not use web search. Do not read anything outside this repository, and do not look for evaluation directories, other candidates, run artifacts, or upstream project history.",
    "You are not told which retrieval configuration, execution model, or trial produced this patch. Do not speculate about it.",
    "",
    "## Task story given to the implementer",
    story,
    "",
    "## Candidate patch (its complete change)",
    "```diff",
    patch.trimEnd(),
    "```",
    "",
    "## Reference files the candidate did not modify",
    "The reference implementation is one valid solution, not the mandatory affected set. For each file below, its reference diff is shown. Decide whether the candidate's own design makes that change unnecessary, or whether the behavior it contributes is simply missing.",
    "",
    ...goldDiffs.flatMap(({ gold_file, reference_diff }) => [
      `### ${gold_file}`,
      "```diff",
      reference_diff.trimEnd(),
      "```",
      "",
    ]),
    "## Verdicts",
    "Return exactly one verdict for every listed reference file, using its exact path:",
    "- `alternative_covered`: the candidate patch already delivers the behavior this reference change contributes, by other means. Name the specific part of the candidate patch that does so.",
    "- `not_required`: the reference change is incidental plumbing that exists only to support the reference's own internal choice (an export, a field initialization, a signature widening), and its absence costs the candidate nothing.",
    "- `omission`: the behavior this reference change contributes is absent from the candidate's implementation. This is a real defect.",
    "Judge the candidate's actual code, not its comments or its resemblance to the reference. A different structure is fine; missing behavior is not.",
    "Keep each reason to one sentence.",
    "Then return an overall verdict: `genuine_alternative` if the candidate solves the same contract by a different route with no missing behavior, `partial_alternative` if part of the contract is covered differently but some behavior is missing, `not_alternative` if the candidate simply does less than the reference.",
  ].join("\n");
}

export function validateJudgment(goldDiffs, judged) {
  const expected = new Set(goldDiffs.map((entry) => entry.gold_file));
  const rows = judged?.gold_files;
  if (!Array.isArray(rows) || rows.length !== expected.size) {
    throw new Error(`adjudicator returned ${rows?.length ?? 0} files; expected ${expected.size}`);
  }
  const seen = new Set();
  for (const row of rows) {
    if (!expected.has(row.gold_file) || seen.has(row.gold_file)) {
      throw new Error(`unexpected or duplicate gold file: ${row.gold_file}`);
    }
    seen.add(row.gold_file);
  }
  return rows;
}

function readRun(runDir) {
  const responsesFile = path.join(runDir, "responses.jsonl");
  const responses = jsonLines(fs.readFileSync(responsesFile, "utf8"))
    .map((row) => ({ ...row, trial: path.basename(runDir) }));
  const pendingBySession = new Map();
  const patchBySession = new Map();
  for (const response of responses) {
    const artifactDir = path.join(runDir, "artifacts", response.session);
    const gradeFile = path.join(artifactDir, "grade.json");
    const patchFile = path.join(artifactDir, "agent.patch");
    if (!fs.existsSync(gradeFile) || !fs.existsSync(patchFile)) continue;
    const grade = JSON.parse(fs.readFileSync(gradeFile, "utf8"));
    pendingBySession.set(response.session, grade.coverage.patched.pending_adjudication);
    patchBySession.set(response.session, fs.readFileSync(patchFile, "utf8"));
  }
  return { responses, pendingBySession, patchBySession };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const goldDir = path.resolve(options.gold);
  const task = JSON.parse(fs.readFileSync(path.join(goldDir, "task.json"), "utf8"));
  const goldSections = splitPatchByFile(fs.readFileSync(path.join(goldDir, "gold-code.patch"), "utf8"));
  const story = options.story ?? task.story;
  if (!story) throw new Error("no task story available; pass --story");

  const cases = [];
  for (const runDir of options.runs.split(",").map((entry) => path.resolve(entry.trim()))) {
    cases.push(...buildArmCases({ ...readRun(runDir), goldSections }));
  }
  cases.sort((left, right) => left.blind_case.localeCompare(right.blind_case));

  const output = path.resolve(options.output);
  const artifacts = path.resolve(options.artifacts);
  const repository = path.resolve(options.repository);
  const done = new Set();
  if (fs.existsSync(output) && fs.statSync(output).size > 0) {
    if (!options.resume) throw new Error(`refusing to append to non-empty output: ${output}`);
    for (const row of jsonLines(fs.readFileSync(output, "utf8"))) done.add(row.session);
  }
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.mkdirSync(artifacts, { recursive: true });
  fs.mkdirSync(path.join(path.dirname(output), "verdicts"), { recursive: true });
  const schema = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../eval/pr-omission-adjudication.schema.json",
  );

  for (const [index, entry] of cases.entries()) {
    const label = `${entry.blind_case}`;
    if (done.has(entry.session)) {
      process.stderr.write(`[${index + 1}/${cases.length}] ${label} already adjudicated; skipping\n`);
      continue;
    }
    const lastMessage = path.join(os.tmpdir(), `jscout-pr-omission-${label}-${process.pid}.json`);
    const prompt = promptForOmissionAdjudication({
      story,
      patch: entry.patch,
      goldDiffs: entry.gold_diffs,
    });
    fs.writeFileSync(path.join(artifacts, `${label}.prompt.txt`), prompt);
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
      prompt,
    ];
    process.stderr.write(
      `[${index + 1}/${cases.length}] ${label}: ${entry.gold_diffs.length} pending reference files\n`,
    );
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
    const verdicts = validateJudgment(entry.gold_diffs, judged);
    const row = {
      blind_case: entry.blind_case,
      session: entry.session,
      trial: entry.trial,
      profile: entry.profile,
      treatment: entry.treatment,
      execution_model: entry.execution_model,
      task_id: task.sha,
      judge_model: options.model,
      judge_reasoning: options.reasoning,
      gold_files: verdicts,
      overall_verdict: judged.overall_verdict,
      overall_reason: judged.overall_reason,
      duration_ms: result.duration_ms,
    };
    fs.appendFileSync(output, `${JSON.stringify(row)}\n`);
    // Flat per-file rows in the shape eval-pr-grade.mjs consumes on a regrade.
    fs.writeFileSync(
      path.join(path.dirname(output), "verdicts", `${entry.session}.jsonl`),
      verdicts.map((verdict) => `${JSON.stringify({
        task_id: task.sha,
        gold_file: verdict.gold_file,
        verdict: verdict.verdict,
      })}\n`).join(""),
    );
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
