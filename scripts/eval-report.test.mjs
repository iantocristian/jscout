import test from "node:test";
import assert from "node:assert/strict";

import { buildReport } from "./eval-report.mjs";

test("evaluation report joins task outcomes to profile/session telemetry", () => {
  const tasks = {
    schema_version: 1,
    repository: "fixture",
    tasks: [
      {
        id: "path",
        category: "multi-hop",
        gold: { files: ["a.ts", "b.ts"], symbols: ["a", "b"] },
      },
    ],
  };
  const responses = [
    {
      task_id: "path",
      profile: "baseline",
      session: "base-1",
      files: ["a.ts"],
      symbols: ["a"],
      correct: false,
      inspected_files: ["a.ts", "noise.ts"],
      total_tokens: 900,
    },
    {
      task_id: "path",
      profile: "structural",
      session: "graph-1",
      files: ["a.ts", "b.ts"],
      symbols: ["a", "b"],
      correct: true,
      inspected_files: ["a.ts", "b.ts"],
      total_tokens: 600,
    },
  ];
  const telemetry = [
    {
      task: "path",
      profile: "baseline",
      session: "base-1",
      tool: "semantic_search",
      ok: true,
      elapsed_ms: 4,
      result_bytes: 100,
    },
    {
      task: "path",
      profile: "structural",
      session: "graph-1",
      tool: "neighborhood",
      ok: true,
      elapsed_ms: 2,
      result_bytes: 80,
    },
  ];

  const report = buildReport(tasks, responses, telemetry);
  assert.equal(report.profiles.baseline.file_recall, 0.5);
  assert.equal(report.profiles.structural.file_recall, 1);
  assert.equal(report.structural_minus_baseline.file_recall, 0.5);
  assert.equal(report.profiles.structural.mean_tool_calls, 1);
  assert.equal(report.profiles.baseline.mean_irrelevant_files, 1);
  assert.equal(report.structural_minus_baseline.mean_total_tokens, -300);
  assert.deepEqual(report.missing, []);
});

test("evaluation report supports a declared grep/baseline/structural comparison", () => {
  const tasks = {
    schema_version: 1,
    profiles: ["grep", "baseline", "structural"],
    tasks: [{ id: "hard", category: "multi-hop", gold: { files: ["a.ts"], symbols: ["a"] } }],
  };
  const responses = [
    {
      task_id: "hard",
      profile: "grep",
      session: "grep-hard-001",
      files: [],
      symbols: [],
      correct: false,
      total_tokens: 1000,
    },
    {
      task_id: "hard",
      profile: "baseline",
      session: "baseline-hard-001",
      files: ["a.ts"],
      symbols: ["a"],
      correct: true,
      total_tokens: 800,
    },
    {
      task_id: "hard",
      profile: "structural",
      session: "structural-hard-001",
      files: ["a.ts"],
      symbols: ["a"],
      correct: true,
      total_tokens: 600,
    },
  ];

  const report = buildReport(tasks, responses);
  assert.equal(report.profile_deltas.baseline_minus_grep.correctness_rate, 1);
  assert.equal(report.profile_deltas.structural_minus_grep.mean_total_tokens, -400);
  assert.equal(report.profile_deltas.structural_minus_baseline.mean_total_tokens, -200);
  assert.deepEqual(report.missing, []);
});
