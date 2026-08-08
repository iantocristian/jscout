#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const RESPONSE_METRICS = [
  "exact",
  "adjudicated_correct",
  "file_precision",
  "file_recall",
  "symbol_precision",
  "symbol_recall",
  "total_tokens",
  "duration_ms",
  "inspected_files",
  "irrelevant_files",
];

function parseArgs(argv) {
  const options = { bootstrap: "10000", seed: "20260808" };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "responses"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function splitPaths(value = "") {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => path.resolve(entry));
}

function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map((line, index) => {
    try { return JSON.parse(line); } catch (error) {
      throw new Error(`${file}:${index + 1}: ${error.message}`);
    }
  });
}

function mean(values) {
  return values.length === 0 ? null : values.reduce((sum, value) => sum + value, 0) / values.length;
}

function median(values) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle];
}

function setScore(expected = [], actual = []) {
  const gold = new Set(expected);
  const predicted = new Set(actual);
  const matches = [...predicted].filter((value) => gold.has(value)).length;
  return {
    precision: predicted.size === 0 ? (gold.size === 0 ? 1 : 0) : matches / predicted.size,
    recall: gold.size === 0 ? 1 : matches / gold.size,
  };
}

function sameSet(left = [], right = []) {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

function trialOf(response) {
  const prefix = `${response.profile}-${response.task_id}-`;
  if (!response.session?.startsWith(prefix)) {
    throw new Error(`session ${response.session} does not start with ${prefix}`);
  }
  return response.session.slice(prefix.length);
}

function percentile(sorted, probability) {
  if (sorted.length === 0) return null;
  return sorted[Math.floor(probability * (sorted.length - 1))];
}

function makeRandom(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 0x1_0000_0000;
  };
}

export function clusterBootstrapMean(valuesByCluster, iterations = 10000, seed = 20260808) {
  const clusters = [...valuesByCluster.values()];
  if (clusters.length === 0 || iterations <= 0) return { low: null, high: null };
  const random = makeRandom(seed);
  const estimates = [];
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    const sample = [];
    for (let index = 0; index < clusters.length; index += 1) {
      sample.push(...clusters[Math.floor(random() * clusters.length)]);
    }
    estimates.push(mean(sample));
  }
  estimates.sort((left, right) => left - right);
  return {
    low: percentile(estimates, 0.025),
    high: percentile(estimates, 0.975),
  };
}

export function buildPooledReport({
  taskSets,
  responses,
  telemetry,
  adjudications = [],
  bootstrap,
  seed,
  allowCrossModel = false,
}) {
  // Pooling runs from different models (or reasoning levels) silently blends
  // incomparable cost/behavior distributions. Refuse unless explicitly allowed.
  const executionModels = [...new Set(
    responses.map((response) => `${response.model ?? "unknown"}/${response.reasoning ?? "unknown"}`),
  )].sort();
  if (executionModels.length > 1 && !allowCrossModel) {
    throw new Error(
      `refusing to pool across execution models: ${executionModels.join(", ")} ` +
        "(pass --allow-cross-model true to override; label the result accordingly)",
    );
  }

  const taskById = new Map();
  const repositoryByTask = new Map();
  for (const taskSet of taskSets) {
    for (const task of taskSet.tasks) {
      if (taskById.has(task.id)) throw new Error(`duplicate task id: ${task.id}`);
      taskById.set(task.id, task);
      repositoryByTask.set(task.id, taskSet.repository?.name ?? "unknown");
    }
  }

  const adjudicationBySession = new Map(
    adjudications.map((adjudication) => [adjudication.session, adjudication]),
  );
  const rows = responses.map((response) => {
    const task = taskById.get(response.task_id);
    if (!task) throw new Error(`unknown task id: ${response.task_id}`);
    const fileScore = setScore(task.gold?.files, response.files);
    const symbolScore = setScore(task.gold?.symbols, response.symbols);
    const inspected = new Set(response.inspected_files ?? []);
    const goldFiles = new Set(task.gold?.files ?? []);
    const exact = typeof response.correct === "boolean"
      ? Number(response.correct)
      : Number(sameSet(task.gold?.files, response.files) && sameSet(task.gold?.symbols, response.symbols));
    const adjudication = adjudicationBySession.get(response.session);
    const adjudicatedCorrect = exact === 1 ||
      adjudication?.verdict === "correct" ||
      adjudication?.verdict === "acceptable_alternative";
    return {
      task_id: response.task_id,
      repository: repositoryByTask.get(response.task_id),
      trial: trialOf(response),
      profile: response.profile,
      exact,
      adjudicated_correct: Number(adjudicatedCorrect),
      file_precision: fileScore.precision,
      file_recall: fileScore.recall,
      symbol_precision: symbolScore.precision,
      symbol_recall: symbolScore.recall,
      total_tokens: Number(response.total_tokens),
      duration_ms: Number(response.duration_ms),
      inspected_files: inspected.size,
      irrelevant_files: [...inspected].filter((file) => !goldFiles.has(file)).length,
      runner_error: response.runner_error,
    };
  });

  const byProfile = Map.groupBy(rows, (row) => row.profile);
  const profiles = {};
  for (const [profile, profileRows] of byProfile) {
    profiles[profile] = { runs: profileRows.length };
    for (const metric of RESPONSE_METRICS) {
      profiles[profile][metric] = mean(profileRows.map((row) => row[metric]));
    }
    profiles[profile].runner_errors = profileRows.filter((row) => row.runner_error).length;
  }

  const byRepository = {};
  for (const [repository, repositoryRows] of Map.groupBy(rows, (row) => row.repository)) {
    byRepository[repository] = {};
    for (const [profile, profileRows] of Map.groupBy(repositoryRows, (row) => row.profile)) {
      byRepository[repository][profile] = {
        runs: profileRows.length,
        exact: profileRows.reduce((sum, row) => sum + row.exact, 0),
      };
    }
  }

  const rowsByRun = Map.groupBy(rows, (row) => `${row.task_id}\0${row.trial}`);
  const paired = {};
  for (const profile of ["baseline", "structural"]) {
    paired[`${profile}_minus_grep`] = {};
    for (const metric of RESPONSE_METRICS) {
      const differences = [];
      const byTask = new Map();
      for (const runRows of rowsByRun.values()) {
        const indexed = runRows.find((row) => row.profile === profile);
        const grep = runRows.find((row) => row.profile === "grep");
        if (!indexed || !grep) continue;
        const difference = indexed[metric] - grep[metric];
        differences.push(difference);
        const taskValues = byTask.get(indexed.task_id) ?? [];
        taskValues.push(difference);
        byTask.set(indexed.task_id, taskValues);
      }
      paired[`${profile}_minus_grep`][metric] = {
        pairs: differences.length,
        mean: mean(differences),
        median: median(differences),
        ci95_clustered_by_task: clusterBootstrapMean(byTask, bootstrap, seed),
      };
    }
  }

  const toolProfiles = {};
  for (const [profile, entries] of Map.groupBy(telemetry, (entry) => entry.profile)) {
    const byTool = {};
    const expansionRoleCounts = {};
    for (const [tool, toolEntries] of Map.groupBy(entries, (entry) => entry.tool)) {
      byTool[tool] = toolEntries.length;
    }
    for (const entry of entries) {
      for (const [role, count] of Object.entries(entry.expansion_role_counts ?? {})) {
        expansionRoleCounts[role] = (expansionRoleCounts[role] ?? 0) + Number(count ?? 0);
      }
    }
    const expansionFileNodes = entries.reduce(
      (sum, entry) => sum + Number(entry.expansion_file_nodes ?? 0),
      0,
    );
    const expansionTestFixtureGeneratedNodes = entries.reduce(
      (sum, entry) => sum + Number(entry.expansion_test_fixture_generated_nodes ?? 0),
      0,
    );
    toolProfiles[profile] = {
      calls: entries.length,
      failed_calls: entries.filter((entry) => entry.ok === false).length,
      result_bytes: entries.reduce((sum, entry) => sum + Number(entry.result_bytes ?? 0), 0),
      max_result_bytes: Math.max(0, ...entries.map((entry) => Number(entry.result_bytes ?? 0))),
      source_budget_truncations: entries.reduce(
        (sum, entry) => sum + Number(entry.source_budget_truncations ?? 0),
        0,
      ),
      expansion_nodes: entries.reduce(
        (sum, entry) => sum + Number(entry.expansion_nodes ?? 0),
        0,
      ),
      expansion_file_nodes: expansionFileNodes,
      expansion_role_counts: expansionRoleCounts,
      expansion_test_fixture_generated_nodes: expansionTestFixtureGeneratedNodes,
      expansion_test_fixture_generated_share:
        expansionFileNodes === 0
          ? null
          : expansionTestFixtureGeneratedNodes / expansionFileNodes,
      tools: byTool,
    };
  }

  return {
    schema_version: 1,
    execution_models: executionModels,
    tasks: taskById.size,
    trials: new Set(rows.map((row) => row.trial)).size,
    runs: rows.length,
    profiles,
    repositories: byRepository,
    paired_deltas: paired,
    tool_profiles: toolProfiles,
  };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSets = splitPaths(options.tasks).map((file) => JSON.parse(fs.readFileSync(file, "utf8")));
  const responses = splitPaths(options.responses).flatMap(readJsonl);
  const telemetry = splitPaths(options.telemetry).flatMap(readJsonl);
  const adjudications = splitPaths(options.adjudications).flatMap(readJsonl);
  const report = buildPooledReport({
    taskSets,
    responses,
    telemetry,
    adjudications,
    bootstrap: Number(options.bootstrap),
    seed: Number(options.seed),
    allowCrossModel: options["allow-cross-model"] === "true",
  });
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
