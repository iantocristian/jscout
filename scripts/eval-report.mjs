#!/usr/bin/env node

import fs from "node:fs";
import process from "node:process";
import { pathToFileURL } from "node:url";

function readJson(path) {
  return JSON.parse(fs.readFileSync(path, "utf8"));
}

function readJsonl(path) {
  if (!path || !fs.existsSync(path)) return [];
  return fs
    .readFileSync(path, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim())
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${path}:${index + 1}: ${error.message}`);
      }
    });
}

function parseArgs(argv) {
  const out = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("usage: eval-report --tasks FILE --responses FILE [--telemetry FILE]");
    }
    out[flag.slice(2)] = value;
  }
  if (!out.tasks || !out.responses) {
    throw new Error("--tasks and --responses are required");
  }
  return out;
}

function setScore(expected = [], actual = []) {
  const gold = new Set(expected);
  const predicted = new Set(actual);
  let matches = 0;
  for (const value of predicted) {
    if (gold.has(value)) matches += 1;
  }
  return {
    precision: predicted.size === 0 ? (gold.size === 0 ? 1 : 0) : matches / predicted.size,
    recall: gold.size === 0 ? 1 : matches / gold.size,
  };
}

function mean(values) {
  return values.length === 0 ? null : values.reduce((sum, value) => sum + value, 0) / values.length;
}

function meanPresent(values) {
  return mean(values.filter((value) => typeof value === "number" && Number.isFinite(value)));
}

const DELTA_METRICS = [
  "file_precision",
  "file_recall",
  "symbol_precision",
  "symbol_recall",
  "correctness_rate",
  "mean_tool_calls",
  "mean_failed_tool_calls",
  "mean_tool_latency_ms",
  "mean_result_bytes",
  "mean_source_rendered_bytes",
  "mean_source_original_bytes",
  "mean_source_budget_truncations",
  "mean_jscout_requests",
  "mean_command_calls",
  "mean_failed_command_calls",
  "mean_command_output_bytes",
  "mean_max_command_output_bytes",
  "mean_inspected_files",
  "mean_irrelevant_files",
  "mean_total_tokens",
  "mean_run_duration_ms",
];

function profileDelta(summaries, minuend, subtrahend) {
  if (!summaries[minuend] || !summaries[subtrahend]) return {};
  const deltas = {};
  for (const metric of DELTA_METRICS) {
    const left = summaries[minuend][metric];
    const right = summaries[subtrahend][metric];
    deltas[metric] = left === null || right === null ? null : left - right;
  }
  return deltas;
}

export function buildReport(taskSet, responses, telemetry = []) {
  if (taskSet.schema_version !== 1 || !Array.isArray(taskSet.tasks)) {
    throw new Error("unsupported task-set schema");
  }
  const taskById = new Map(taskSet.tasks.map((task) => [task.id, task]));
  const profiles = {};
  const profileTreatments = {};
  const taskResults = [];

  for (const response of responses) {
    const task = taskById.get(response.task_id);
    if (!task) throw new Error(`unknown task_id: ${response.task_id}`);
    if (!response.profile || !response.session) {
      throw new Error(`response ${response.task_id} requires profile and session`);
    }
    const fileScore = setScore(task.gold?.files, response.files);
    const symbolScore = setScore(task.gold?.symbols, response.symbols);
    const calls = telemetry.filter(
      (entry) =>
        entry.task === response.task_id &&
        entry.profile === response.profile &&
        entry.session === response.session,
    );
    const inspectedFiles = Array.isArray(response.inspected_files)
      ? new Set(response.inspected_files)
      : null;
    const goldFiles = new Set(task.gold?.files || []);
    const result = {
      task_id: response.task_id,
      category: task.category,
      profile: response.profile,
      treatment: response.treatment ?? "unspecified",
      session: response.session,
      file_precision: fileScore.precision,
      file_recall: fileScore.recall,
      symbol_precision: symbolScore.precision,
      symbol_recall: symbolScore.recall,
      correct: typeof response.correct === "boolean"
        ? response.correct
        : typeof response.layer1 === "string"
          ? response.layer1 === "pass"
          : null,
      tool_calls: calls.length,
      failed_tool_calls: calls.filter((call) => call.ok === false).length,
      tool_latency_ms: calls.reduce((sum, call) => sum + Number(call.elapsed_ms || 0), 0),
      result_bytes: calls.reduce((sum, call) => sum + Number(call.result_bytes || 0), 0),
      source_rendered_bytes: calls.reduce(
        (sum, call) => sum + Number(call.source_rendered_bytes || 0),
        0,
      ),
      source_original_bytes: calls.reduce(
        (sum, call) => sum + Number(call.source_original_bytes || 0),
        0,
      ),
      source_budget_truncations: calls.reduce(
        (sum, call) => sum + Number(call.source_budget_truncations || 0),
        0,
      ),
      jscout_requests: Number.isFinite(response.jscout_requests)
        ? response.jscout_requests
        : null,
      command_calls: Number.isFinite(response.command_calls) ? response.command_calls : null,
      failed_command_calls: Number.isFinite(response.failed_command_calls)
        ? response.failed_command_calls
        : null,
      command_output_bytes: Number.isFinite(response.command_output_bytes)
        ? response.command_output_bytes
        : null,
      max_command_output_bytes: Number.isFinite(response.max_command_output_bytes)
        ? response.max_command_output_bytes
        : null,
      inspected_files: inspectedFiles?.size ?? null,
      irrelevant_files: inspectedFiles
        ? [...inspectedFiles].filter((file) => !goldFiles.has(file)).length
        : null,
      total_tokens: Number.isFinite(response.total_tokens) ? response.total_tokens : null,
      run_duration_ms: Number.isFinite(response.duration_ms) ? response.duration_ms : null,
    };
    taskResults.push(result);
    (profiles[response.profile] ??= []).push(result);
    (profileTreatments[`${response.profile}/${result.treatment}`] ??= []).push(result);
  }

  const summarize = (results) => {
    const judged = results.filter((result) => result.correct !== null);
    return {
      runs: results.length,
      file_precision: mean(results.map((result) => result.file_precision)),
      file_recall: mean(results.map((result) => result.file_recall)),
      symbol_precision: mean(results.map((result) => result.symbol_precision)),
      symbol_recall: mean(results.map((result) => result.symbol_recall)),
      correctness_rate: mean(judged.map((result) => (result.correct ? 1 : 0))),
      mean_tool_calls: mean(results.map((result) => result.tool_calls)),
      mean_failed_tool_calls: mean(results.map((result) => result.failed_tool_calls)),
      mean_tool_latency_ms: mean(results.map((result) => result.tool_latency_ms)),
      mean_result_bytes: mean(results.map((result) => result.result_bytes)),
      mean_source_rendered_bytes: mean(results.map((result) => result.source_rendered_bytes)),
      mean_source_original_bytes: mean(results.map((result) => result.source_original_bytes)),
      mean_source_budget_truncations: mean(
        results.map((result) => result.source_budget_truncations),
      ),
      mean_jscout_requests: meanPresent(results.map((result) => result.jscout_requests)),
      mean_command_calls: meanPresent(results.map((result) => result.command_calls)),
      mean_failed_command_calls: meanPresent(
        results.map((result) => result.failed_command_calls),
      ),
      mean_command_output_bytes: meanPresent(
        results.map((result) => result.command_output_bytes),
      ),
      mean_max_command_output_bytes: meanPresent(
        results.map((result) => result.max_command_output_bytes),
      ),
      mean_inspected_files: meanPresent(results.map((result) => result.inspected_files)),
      mean_irrelevant_files: meanPresent(results.map((result) => result.irrelevant_files)),
      mean_total_tokens: meanPresent(results.map((result) => result.total_tokens)),
      mean_run_duration_ms: meanPresent(results.map((result) => result.run_duration_ms)),
    };
  };

  const summaries = {};
  for (const [profile, results] of Object.entries(profiles)) {
    summaries[profile] = summarize(results);
  }
  const treatmentSummaries = {};
  for (const [profileTreatment, results] of Object.entries(profileTreatments)) {
    treatmentSummaries[profileTreatment] = summarize(results);
  }

  const expectedProfiles = taskSet.profiles ?? ["baseline", "structural"];
  if (!Array.isArray(expectedProfiles) || expectedProfiles.length === 0) {
    throw new Error("task-set profiles must be a non-empty array");
  }
  const completed = new Set(responses.map((response) => `${response.task_id}\0${response.profile}`));
  const missing = [];
  for (const task of taskSet.tasks) {
    for (const profile of expectedProfiles) {
      if (!completed.has(`${task.id}\0${profile}`)) missing.push({ task_id: task.id, profile });
    }
  }

  const structuralMinusBaseline = profileDelta(summaries, "structural", "baseline");
  const profileDeltas = {
    structural_minus_baseline: structuralMinusBaseline,
    baseline_minus_grep: profileDelta(summaries, "baseline", "grep"),
    structural_minus_grep: profileDelta(summaries, "structural", "grep"),
    structural_elided_minus_full: profileDelta(
      summaries,
      "structural-elided",
      "structural-full",
    ),
  };

  taskResults.sort((a, b) =>
    `${a.task_id}\0${a.profile}\0${a.session}`.localeCompare(
      `${b.task_id}\0${b.profile}\0${b.session}`,
    ),
  );
  return {
    schema_version: 1,
    repository: taskSet.repository,
    profiles: summaries,
    profile_treatments: treatmentSummaries,
    structural_minus_baseline: structuralMinusBaseline,
    profile_deltas: profileDeltas,
    missing,
    tasks: taskResults,
  };
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const report = buildReport(readJson(args.tasks), readJsonl(args.responses), readJsonl(args.telemetry));
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
