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
