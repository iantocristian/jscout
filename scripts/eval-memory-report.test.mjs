import assert from "node:assert/strict";
import test from "node:test";

import { buildMemoryReport } from "./eval-memory-report.mjs";

const taskSets = [{
  schema_version: 1,
  pairs: [{
    id: "flow",
    admission: {
      anchor_class: "anchor-free",
      transfer_triviality: { status: "pass", model: "gpt-5.6-terra", reasoning: "high" },
    },
    session1: { prompt: "A", gold: { files: ["a.ts"], symbols: ["a"] } },
    session2: { prompt: "B", gold: { files: ["b.ts"], symbols: ["b"] } },
  }],
}];

const response = (arm, phase, tokens) => ({
  pair_id: "flow",
  trial: "001",
  arm,
  phase,
  session: `${arm}-${phase}`,
  model: "gpt-5.6-terra",
  reasoning: "high",
  total_tokens: tokens,
  duration_ms: tokens,
  correct: true,
  semantic_state_before: { artifacts: arm === "warm" && phase === "session2" ? 1 : 0 },
});

test("memory report pairs warm and cold session 2 and requires artifact-backed wins", () => {
  const responses = [
    response("warm", "session1", 100),
    response("warm", "session2", 60),
    response("cold", "session2", 100),
  ];
  const telemetry = [
    { session: "warm-session1", semantic_artifacts_written: 1 },
    { session: "warm-session2", semantic_artifacts_returned: 1, semantic_artifacts_fresh: 1 },
    { session: "cold-session2", semantic_artifacts_returned: 0 },
  ];
  const report = buildMemoryReport({ taskSets, responses, telemetry, bootstrap: 100, seed: 1 });
  assert.equal(report.session2.token_reduction_fraction.median, 0.4);
  assert.equal(report.memory.warm_artifact_usage_rate, 1);
  assert.equal(report.memory.token_wins_without_artifact_reads, 0);
  assert.equal(report.decision, "pass");
});

test("memory report refuses cross-model pooling", () => {
  const responses = [
    response("warm", "session1", 100),
    response("warm", "session2", 60),
    { ...response("cold", "session2", 100), model: "gpt-5.6-sol" },
  ];
  assert.throws(
    () => buildMemoryReport({ taskSets, responses, telemetry: [] }),
    /refusing to pool across execution models/,
  );
});

test("staleness gate requires retrieval, a stale label, and re-verification", () => {
  const staleTaskSets = structuredClone(taskSets);
  staleTaskSets[0].pairs[0].staleness = {
    edit: {
      file: "b.ts",
      find: "old",
      replace: "new",
      base_sha256: "a".repeat(64),
      mutated_sha256: "b".repeat(64),
    },
  };
  const responses = [
    response("warm", "session1", 100),
    response("warm", "session2", 60),
    response("cold", "session2", 100),
    { ...response("stale", "session2", 70), staleness_edit: staleTaskSets[0].pairs[0].staleness.edit },
  ];
  const telemetry = [
    { session: "warm-session1", semantic_artifacts_written: 1 },
    { session: "warm-session2", semantic_artifacts_returned: 1, semantic_artifacts_fresh: 1 },
    { session: "cold-session2", semantic_artifacts_returned: 0 },
    { session: "stale-session2", semantic_artifacts_returned: 1, semantic_artifacts_degraded: 1 },
  ];
  const passing = buildMemoryReport({
    taskSets: staleTaskSets,
    responses,
    telemetry,
    adjudications: [{ session: "stale-session2", stale_reverified: true }],
    bootstrap: 100,
    seed: 1,
  });
  assert.equal(passing.staleness.artifact_not_retrieved, 0);
  assert.equal(passing.staleness.artifact_not_labelled_stale_or_degraded, 0);
  assert.equal(passing.decision, "pass");

  const failed = buildMemoryReport({
    taskSets: staleTaskSets,
    responses,
    telemetry: telemetry.filter((row) => row.session !== "stale-session2"),
    adjudications: [{ session: "stale-session2", stale_reverified: true }],
    bootstrap: 100,
    seed: 1,
  });
  assert.equal(failed.staleness.artifact_not_retrieved, 1);
  assert.equal(failed.decision, "fail-staleness");
});
