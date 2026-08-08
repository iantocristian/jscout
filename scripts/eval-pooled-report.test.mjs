import assert from "node:assert/strict";
import test from "node:test";

import { buildPooledReport, clusterBootstrapMean } from "./eval-pooled-report.mjs";

test("cluster bootstrap is deterministic and brackets a constant mean", () => {
  const values = new Map([
    ["a", [2, 2]],
    ["b", [2]],
  ]);
  assert.deepEqual(clusterBootstrapMean(values, 100, 7), { low: 2, high: 2 });
});

test("pooled report computes paired profile deltas", () => {
  const taskSets = [{
    repository: { name: "fixture" },
    tasks: [{ id: "task", gold: { files: ["a.ts"], symbols: ["a"] } }],
  }];
  const response = (profile, correct, tokens) => ({
    task_id: "task",
    profile,
    session: `${profile}-task-seed1`,
    files: correct ? ["a.ts"] : ["b.ts"],
    symbols: correct ? ["a"] : ["b"],
    inspected_files: correct ? ["a.ts"] : ["b.ts"],
    total_tokens: tokens,
    duration_ms: tokens,
    correct,
  });
  const report = buildPooledReport({
    taskSets,
    responses: [response("grep", true, 10), response("baseline", false, 20), response("structural", true, 5)],
    telemetry: [],
    adjudications: [{ session: "baseline-task-seed1", verdict: "acceptable_alternative" }],
    bootstrap: 100,
    seed: 1,
  });
  assert.equal(report.profiles.grep.exact, 1);
  assert.equal(report.profiles.baseline.exact, 0);
  assert.equal(report.profiles.baseline.adjudicated_correct, 1);
  assert.equal(report.paired_deltas.baseline_minus_grep.exact.mean, -1);
  assert.equal(report.paired_deltas.structural_minus_grep.total_tokens.mean, -5);
});

test("pooling refuses mixed execution models unless overridden", () => {
  const taskSets = [{
    repository: { name: "fixture" },
    tasks: [{ id: "task", gold: { files: ["a.ts"], symbols: ["a"] } }],
  }];
  const response = (profile, model) => ({
    task_id: "task",
    profile,
    session: `${profile}-task-seed1`,
    model,
    reasoning: "high",
    files: ["a.ts"],
    symbols: ["a"],
    inspected_files: ["a.ts"],
    total_tokens: 1,
    duration_ms: 1,
    correct: true,
  });
  const mixed = {
    taskSets,
    responses: [response("grep", "gpt-5.6-terra"), response("baseline", "gpt-5.4")],
    telemetry: [],
    bootstrap: 10,
    seed: 1,
  };
  assert.throws(() => buildPooledReport(mixed), /refusing to pool across execution models/);
  const overridden = buildPooledReport({ ...mixed, allowCrossModel: true });
  assert.deepEqual(overridden.execution_models, ["gpt-5.4/high", "gpt-5.6-terra/high"]);
});

test("pooled telemetry reports the pre-registered expansion role share", () => {
  const taskSets = [{
    repository: { name: "fixture" },
    tasks: [{ id: "task", gold: { files: ["a.ts"], symbols: ["a"] } }],
  }];
  const responses = [{
    task_id: "task",
    profile: "structural",
    session: "structural-task-seed1",
    model: "gpt-5.6-terra",
    reasoning: "high",
    files: ["a.ts"],
    symbols: ["a"],
    inspected_files: ["a.ts"],
    total_tokens: 1,
    duration_ms: 1,
    correct: true,
  }];
  const telemetry = [{
    task: "task",
    profile: "structural",
    session: "structural-task-seed1",
    tool: "semantic_search",
    ok: true,
    expansion_nodes: 5,
    expansion_file_nodes: 4,
    expansion_role_counts: { production: 2, test: 1, fixture: 1 },
    expansion_test_fixture_generated_nodes: 2,
  }];
  const report = buildPooledReport({
    taskSets,
    responses,
    telemetry,
    bootstrap: 10,
    seed: 1,
  });
  assert.equal(report.tool_profiles.structural.expansion_nodes, 5);
  assert.deepEqual(report.tool_profiles.structural.expansion_role_counts, {
    production: 2,
    test: 1,
    fixture: 1,
  });
  assert.equal(report.tool_profiles.structural.expansion_test_fixture_generated_share, 0.5);
});
