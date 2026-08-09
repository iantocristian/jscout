import assert from "node:assert/strict";
import test from "node:test";

import {
  buildBlindCases,
  promptForCorrectnessAdjudication,
  validateJudgments,
} from "./eval-memory-correctness-adjudicate.mjs";

const taskSet = {
  pairs: [{
    id: "flow",
    session2: { prompt: "Find it", gold: { files: ["a.ts"], symbols: ["a"] } },
  }],
};

test("correctness adjudication includes only exact disagreements and hides arm labels", () => {
  const cases = buildBlindCases(taskSet, [{
    pair_id: "flow",
    phase: "session2",
    arm: "warm",
    session: "session-a",
    correct: false,
    files: ["b.ts"],
    symbols: ["b"],
  }, {
    pair_id: "flow",
    phase: "session2",
    arm: "cold",
    session: "session-b",
    correct: true,
    files: ["a.ts"],
    symbols: ["a"],
  }]);
  assert.equal(cases.length, 1);
  const prompt = promptForCorrectnessAdjudication(cases);
  assert.doesNotMatch(prompt, /warm|cold|session-a/);
  assert.match(prompt, /C001/);
});

test("correctness adjudication requires one unique judgment per blind case", () => {
  const cases = [{ blind_case: "C001" }, { blind_case: "C002" }];
  assert.equal(validateJudgments(cases, { cases: [
    { blind_case: "C001" },
    { blind_case: "C002" },
  ] }).length, 2);
  assert.throws(
    () => validateJudgments(cases, { cases: [{ blind_case: "C001" }, { blind_case: "C001" }] }),
    /unexpected or duplicate/,
  );
});
