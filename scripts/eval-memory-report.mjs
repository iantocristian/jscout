#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { clusterBootstrapMean } from "./eval-pooled-report.mjs";
import { validateTaskSet } from "./eval-run-memory.mjs";

function parseArgs(argv) {
  const options = { bootstrap: "10000", seed: "20260809" };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "responses", "telemetry"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function paths(value = "") {
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

function sum(entries, field) {
  return entries.reduce((total, entry) => total + Number(entry[field] ?? 0), 0);
}

export function buildMemoryReport({
  taskSets,
  responses,
  telemetry,
  adjudications = [],
  bootstrap = 10000,
  seed = 20260809,
  allowCrossModel = false,
}) {
  for (const taskSet of taskSets) validateTaskSet(taskSet);
  const pairs = taskSets.flatMap((taskSet) => taskSet.pairs);
  const pairById = new Map();
  for (const pair of pairs) {
    if (pairById.has(pair.id)) throw new Error(`duplicate pair id: ${pair.id}`);
    pairById.set(pair.id, pair);
  }
  const executionModels = [...new Set(
    responses.map((row) => `${row.model ?? "unknown"}/${row.reasoning ?? "unknown"}`),
  )].sort();
  if (executionModels.length > 1 && !allowCrossModel) {
    throw new Error(
      `refusing to pool across execution models: ${executionModels.join(", ")} ` +
      "(pass --allow-cross-model true to override)",
    );
  }
  for (const row of responses) {
    if (!pairById.has(row.pair_id)) throw new Error(`unknown memory pair: ${row.pair_id}`);
  }

  const telemetryBySession = Map.groupBy(telemetry, (entry) => entry.session);
  const responseByKey = new Map(
    responses.map((row) => [`${row.pair_id}\0${row.trial}\0${row.arm}\0${row.phase}`, row]),
  );
  const trials = [...new Set(responses.map((row) => row.trial))].sort();
  const missing = [];
  for (const pair of pairs) {
    for (const trial of trials) {
      for (const [arm, phase] of [["warm", "session1"], ["warm", "session2"], ["cold", "session2"]]) {
        const key = `${pair.id}\0${trial}\0${arm}\0${phase}`;
        if (!responseByKey.has(key)) missing.push({ pair_id: pair.id, trial, arm, phase });
      }
    }
  }

  const paired = [];
  for (const pair of pairs) {
    for (const trial of trials) {
      const warm = responseByKey.get(`${pair.id}\0${trial}\0warm\0session2`);
      const cold = responseByKey.get(`${pair.id}\0${trial}\0cold\0session2`);
      const first = responseByKey.get(`${pair.id}\0${trial}\0warm\0session1`);
      if (!warm || !cold || !first) continue;
      const warmTelemetry = telemetryBySession.get(warm.session) ?? [];
      const coldTelemetry = telemetryBySession.get(cold.session) ?? [];
      const firstTelemetry = telemetryBySession.get(first.session) ?? [];
      const artifactReads = sum(warmTelemetry, "semantic_artifacts_returned");
      paired.push({
        pair_id: pair.id,
        trial,
        warm_correct: Number(Boolean(warm.correct)),
        cold_correct: Number(Boolean(cold.correct)),
        warm_tokens: Number(warm.total_tokens),
        cold_tokens: Number(cold.total_tokens),
        token_delta: Number(warm.total_tokens) - Number(cold.total_tokens),
        token_reduction_fraction: Number(cold.total_tokens) === 0
          ? null
          : (Number(cold.total_tokens) - Number(warm.total_tokens)) / Number(cold.total_tokens),
        warm_duration_ms: Number(warm.duration_ms),
        cold_duration_ms: Number(cold.duration_ms),
        duration_delta_ms: Number(warm.duration_ms) - Number(cold.duration_ms),
        warm_tool_calls: warmTelemetry.length,
        cold_tool_calls: coldTelemetry.length,
        tool_call_delta: warmTelemetry.length - coldTelemetry.length,
        artifact_reads: artifactReads,
        artifact_used: artifactReads > 0,
        session1_tokens: Number(first.total_tokens),
        session1_correct: Number(Boolean(first.correct)),
        session1_tool_calls: firstTelemetry.length,
        session1_artifacts_written: sum(firstTelemetry, "semantic_artifacts_written"),
        session1_failed_annotation_calls: firstTelemetry.filter(
          (entry) => entry.tool === "annotate" && entry.ok === false,
        ).length,
        warm_combined_tokens: Number(first.total_tokens) + Number(warm.total_tokens),
        cold_combined_tokens: Number(first.total_tokens) + Number(cold.total_tokens),
        warm_semantic_state_before: warm.semantic_state_before,
        cold_semantic_state_before: cold.semantic_state_before,
        staleness_case: Boolean(pair.staleness),
        stale_or_degraded_reads:
          sum(warmTelemetry, "semantic_artifacts_stale") +
          sum(warmTelemetry, "semantic_artifacts_degraded"),
        fresh_reads: sum(warmTelemetry, "semantic_artifacts_fresh"),
        runner_error: warm.runner_error ?? cold.runner_error ?? first.runner_error ?? null,
      });
    }
  }

  const byPair = (field) => {
    const grouped = new Map();
    for (const row of paired) {
      const values = grouped.get(row.pair_id) ?? [];
      if (Number.isFinite(row[field])) values.push(row[field]);
      grouped.set(row.pair_id, values);
    }
    return grouped;
  };
  const metric = (field) => {
    const values = paired.map((row) => row[field]).filter(Number.isFinite);
    return {
      pairs: values.length,
      mean: mean(values),
      median: median(values),
      ci95_clustered_by_pair: clusterBootstrapMean(byPair(field), bootstrap, seed),
    };
  };
  const correctness = {
    warm: mean(paired.map((row) => row.warm_correct)),
    cold: mean(paired.map((row) => row.cold_correct)),
    warm_minus_cold: mean(paired.map((row) => row.warm_correct - row.cold_correct)),
    capability_wins: paired.filter((row) => row.warm_correct === 1 && row.cold_correct === 0).length,
    regressions: paired.filter((row) => row.warm_correct === 0 && row.cold_correct === 1).length,
  };
  const tokenWins = paired.filter(
    (row) => row.warm_correct === 1 && row.cold_correct === 1 && row.token_delta < 0,
  );
  const stalenessRows = paired.filter((row) => row.staleness_case);
  const silentStaleFresh = stalenessRows.filter(
    (row) => row.artifact_reads > 0 && row.stale_or_degraded_reads === 0 && row.fresh_reads > 0,
  );
  const adjudicationBySession = new Map(adjudications.map((row) => [row.session, row]));
  const staleAdjudications = stalenessRows.map((row) => {
    const response = responses.find(
      (candidate) => candidate.pair_id === row.pair_id &&
        candidate.trial === row.trial && candidate.arm === "warm" && candidate.phase === "session2",
    );
    return response ? adjudicationBySession.get(response.session) : undefined;
  });
  const staleReverificationFailures = staleAdjudications.filter(
    (row) => row && row.stale_reverified === false,
  ).length;
  const staleAdjudicationMissing = staleAdjudications.filter((row) => !row).length;
  const tokenReduction = metric("token_reduction_fraction");
  const session1Correctness = mean(paired.map((row) => row.session1_correct));
  const artifactsRetrievedInAllWins = tokenWins.length > 0 && tokenWins.every((row) => row.artifact_used);
  const correctnessParity = correctness.warm >= correctness.cold && session1Correctness === 1;
  const hardStalenessFailure = silentStaleFresh.length > 0 || staleReverificationFailures > 0;
  const claimEligible = taskSets.every((taskSet) => taskSet.experiment?.claim_eligible !== false);
  const incomplete = missing.length > 0 || paired.some((row) => row.runner_error) ||
    (stalenessRows.length > 0 && staleAdjudicationMissing > 0);
  const decision = !claimEligible
    ? "not-claim-eligible"
    : incomplete
    ? "incomplete"
    : hardStalenessFailure
    ? "fail-staleness"
    : tokenReduction.median >= 0.2 && correctnessParity && artifactsRetrievedInAllWins
      ? "pass"
      : tokenReduction.median < 0.1 && correctness.capability_wins === 0
        ? "stop-efficiency"
        : "inconclusive";

  return {
    schema_version: 1,
    execution_models: executionModels,
    claim_eligible: claimEligible,
    pair_definitions: pairs.length,
    trials: trials.length,
    paired_runs: paired.length,
    missing,
    runner_errors: paired.filter((row) => row.runner_error).length,
    correctness,
    session1_correctness: session1Correctness,
    session2: {
      token_delta_warm_minus_cold: metric("token_delta"),
      token_reduction_fraction: tokenReduction,
      duration_delta_ms: metric("duration_delta_ms"),
      tool_call_delta: metric("tool_call_delta"),
    },
    memory: {
      warm_artifact_usage_rate: mean(paired.map((row) => Number(row.artifact_used))),
      session1_write_rate: mean(paired.map((row) => Number(row.session1_artifacts_written > 0))),
      failed_annotation_calls: paired.reduce(
        (total, row) => total + row.session1_failed_annotation_calls,
        0,
      ),
      token_wins: tokenWins.length,
      token_wins_without_artifact_reads: tokenWins.filter((row) => !row.artifact_used).length,
    },
    combined_two_session: {
      warm_tokens: metric("warm_combined_tokens"),
      cold_tokens: metric("cold_combined_tokens"),
    },
    staleness: {
      cases: stalenessRows.length,
      silent_stale_served_as_fresh: silentStaleFresh.length,
      revalidation_failures: staleReverificationFailures,
      missing_adjudications: staleAdjudicationMissing,
    },
    decision,
    paired,
  };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const report = buildMemoryReport({
    taskSets: paths(options.tasks).map((file) => JSON.parse(fs.readFileSync(file, "utf8"))),
    responses: paths(options.responses).flatMap(readJsonl),
    telemetry: paths(options.telemetry).flatMap(readJsonl),
    adjudications: paths(options.adjudications).flatMap(readJsonl),
    bootstrap: Number(options.bootstrap),
    seed: Number(options.seed),
    allowCrossModel: options["allow-cross-model"] === "true",
  });
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
