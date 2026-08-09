import assert from "node:assert/strict";
import test from "node:test";

import { scoreWorkflowCandidateSets } from "./eval-workflow-candidate-gate.mjs";

const boundary = (anchor) => ({ anchor, file: `${anchor}.ts`, symbol: anchor });
const candidateSet = (anchors, overrides = {}) => ({
  fingerprint: "f".repeat(64),
  traversal_truncated: false,
  candidate_truncated: false,
  candidates: anchors.map((anchor) => ({ anchor })),
  ...overrides,
});

test("candidate gate separates recall and truncation failures", () => {
  const passing = scoreWorkflowCandidateSets([{
    pair_id: "flow",
    seeds: ["entry"],
    gold: [boundary("entry"), boundary("leaf")],
    candidate_set: candidateSet(["entry", "leaf", "adjacent"]),
  }]);
  assert.equal(passing.micro_recall, 1);
  assert.equal(passing.decision, "pass");

  const missing = scoreWorkflowCandidateSets([{
    pair_id: "flow",
    seeds: ["entry"],
    gold: [boundary("entry"), boundary("leaf")],
    candidate_set: candidateSet(["entry"]),
  }]);
  assert.equal(missing.micro_recall, 0.5);
  assert.equal(missing.details[0].missing_boundaries[0].anchor, "leaf");
  assert.equal(missing.decision, "fail");

  const truncated = scoreWorkflowCandidateSets([{
    pair_id: "flow",
    seeds: ["entry"],
    gold: [boundary("entry")],
    candidate_set: candidateSet(["entry"], { candidate_truncated: true }),
  }]);
  assert.equal(truncated.micro_recall, 1);
  assert.equal(truncated.no_truncation, false);
  assert.equal(truncated.decision, "fail");
});
