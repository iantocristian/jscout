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
