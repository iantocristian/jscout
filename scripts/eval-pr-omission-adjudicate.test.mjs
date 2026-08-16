import assert from "node:assert/strict";
import test from "node:test";

import {
  blindId,
  buildArmCases,
  promptForOmissionAdjudication,
  splitPatchByFile,
  validateJudgment,
} from "./eval-pr-omission-adjudicate.mjs";

const goldPatch = [
  "diff --git a/src/a.ts b/src/a.ts",
  "--- a/src/a.ts",
  "+++ b/src/a.ts",
  "@@ -1 +1 @@",
  "-const a = 1",
  "+const a = 2",
  "diff --git a/src/b.ts b/src/b.ts",
  "--- a/src/b.ts",
  "+++ b/src/b.ts",
  "@@ -1 +1 @@",
  "-const b = 1",
  "+const b = 2",
  "",
].join("\n");

test("gold patches split into per-file diffs keyed by post-image path", () => {
  const sections = splitPatchByFile(goldPatch);
  assert.deepEqual([...sections.keys()], ["src/a.ts", "src/b.ts"]);
  assert.match(sections.get("src/b.ts"), /const b = 2/);
  assert.doesNotMatch(sections.get("src/b.ts"), /const a = 2/);
});

test("arm cases skip fully-covered arms and hide arm labels from the prompt", () => {
  const goldSections = splitPatchByFile(goldPatch);
  const cases = buildArmCases({
    goldSections,
    responses: [
      { session: "checker-forced-t-001", profile: "checker", treatment: "forced", model: "m" },
      { session: "grep-control-t-001", profile: "grep", treatment: "control", model: "m" },
    ],
    pendingBySession: new Map([
      ["checker-forced-t-001", ["src/b.ts"]],
      ["grep-control-t-001", []],
    ]),
    patchBySession: new Map([
      ["checker-forced-t-001", "diff --git a/src/a.ts b/src/a.ts\n"],
      ["grep-control-t-001", "diff --git a/src/a.ts b/src/a.ts\n"],
    ]),
  });
  assert.equal(cases.length, 1);
  assert.equal(cases[0].blind_case, blindId("checker-forced-t-001"));
  const prompt = promptForOmissionAdjudication({
    story: "make it terminate",
    patch: cases[0].patch,
    goldDiffs: cases[0].gold_diffs,
  });
  assert.doesNotMatch(prompt, /checker|forced|grep|control|001/);
  assert.match(prompt, /src\/b\.ts/);
  // The reference diff for the file the arm did patch stays hidden.
  assert.doesNotMatch(prompt, /const a = 2/);
});

test("adjudication requires one unique verdict per pending gold file", () => {
  const goldDiffs = [{ gold_file: "src/a.ts" }, { gold_file: "src/b.ts" }];
  assert.equal(validateJudgment(goldDiffs, {
    gold_files: [{ gold_file: "src/a.ts" }, { gold_file: "src/b.ts" }],
  }).length, 2);
  assert.throws(
    () => validateJudgment(goldDiffs, {
      gold_files: [{ gold_file: "src/a.ts" }, { gold_file: "src/a.ts" }],
    }),
    /unexpected or duplicate/,
  );
  assert.throws(
    () => validateJudgment(goldDiffs, { gold_files: [{ gold_file: "src/a.ts" }] }),
    /expected 2/,
  );
});
