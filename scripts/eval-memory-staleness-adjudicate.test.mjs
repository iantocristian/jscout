import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyStalenessAdjudication,
  promptForStalenessAdjudication,
} from "./eval-memory-staleness-adjudicate.mjs";

test("staleness adjudication requires both a passing judgment and direct file inspection", () => {
  assert.deepEqual(classifyStalenessAdjudication({
    runnerError: null,
    tools: [],
    judged: { stale_reverified: true },
    editedFileInspected: true,
  }), { status: "pass", stale_reverified: true });
  assert.deepEqual(classifyStalenessAdjudication({
    runnerError: null,
    tools: [],
    judged: { stale_reverified: true },
    editedFileInspected: false,
  }), { status: "fail", stale_reverified: false });
  assert.deepEqual(classifyStalenessAdjudication({
    runnerError: "boom",
    tools: [],
    judged: null,
    editedFileInspected: true,
  }), { status: "invalid", stale_reverified: null });
});

test("staleness prompt exposes only the recorded answer and controlled edit", () => {
  const prompt = promptForStalenessAdjudication(
    {
      session2: { prompt: "What happens now?" },
      staleness: { edit: { file: "a.ts", find: "old", replace: "new" } },
    },
    { answer: "new", files: ["a.ts"], symbols: ["a"], inspected_files: ["a.ts"] },
  );
  assert.match(prompt, /consistent with current behavior/);
  assert.match(prompt, /"find": "old"/);
  assert.match(prompt, /"replace": "new"/);
  assert.match(prompt, /Do not use tools/);
});
