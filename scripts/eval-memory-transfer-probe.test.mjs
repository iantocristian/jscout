import assert from "node:assert/strict";
import test from "node:test";

import { classifyTransferProbe, promptForTransfer } from "./eval-memory-transfer-probe.mjs";

const gold = { files: ["b.ts"], symbols: ["beta"] };

test("transfer probe fails only when the previous answer yields the complete follow-up key", () => {
  assert.equal(classifyTransferProbe({
    runnerError: null,
    tools: [],
    gold,
    answer: { files: ["b.ts"], symbols: ["beta"] },
  }), "fail");
  assert.equal(classifyTransferProbe({
    runnerError: null,
    tools: [],
    gold,
    answer: { files: ["b.ts"], symbols: [] },
  }), "pass");
  assert.equal(classifyTransferProbe({
    runnerError: null,
    tools: ["command_execution"],
    gold,
    answer: { files: [], symbols: [] },
  }), "invalid");
});

test("transfer prompt quotes the previous answer and forbids outside knowledge", () => {
  const prompt = promptForTransfer(
    { session2: { prompt: "Where is beta?" } },
    { answer: "alpha calls beta", files: ["a.ts"], symbols: ["alpha"] },
  );
  assert.match(prompt, /<previous-session-answer>/);
  assert.match(prompt, /alpha calls beta/);
  assert.match(prompt, /Do not use tools/);
});
