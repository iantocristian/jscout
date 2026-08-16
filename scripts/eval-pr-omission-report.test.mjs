import assert from "node:assert/strict";
import test from "node:test";

import { renderMarkdown, summarize } from "./eval-pr-omission-report.mjs";

const rows = [
  {
    session: "arm-a",
    trial: "trial-001",
    profile: "checker",
    treatment: "skill",
    overall_verdict: "not_alternative",
    overall_reason: "does less",
    gold_files: [
      { gold_file: "src/a.ts", verdict: "omission", reason: "absent" },
      { gold_file: "src/b.ts", verdict: "not_required", reason: "export only" },
    ],
  },
  {
    session: "arm-b",
    trial: "trial-002",
    profile: "grep",
    treatment: "control",
    overall_verdict: "genuine_alternative",
    overall_reason: "same contract by another route",
    gold_files: [
      { gold_file: "src/a.ts", verdict: "alternative_covered", reason: "covered inline" },
      { gold_file: "src/b.ts", verdict: "not_required", reason: "export only" },
    ],
  },
];

test("summary counts verdicts per reference file and flags legitimate omissions", () => {
  const summary = summarize(rows);
  assert.equal(summary.arms, 2);
  const a = summary.files.find((file) => file.gold_file === "src/a.ts");
  assert.deepEqual(
    { o: a.omission, alt: a.alternative_covered, nr: a.not_required, ever: a.ever_legitimately_omitted },
    { o: 1, alt: 1, nr: 0, ever: true },
  );
  const b = summary.files.find((file) => file.gold_file === "src/b.ts");
  assert.equal(b.omission, 0);
  assert.equal(summary.overall.genuine_alternative, 1);
  assert.deepEqual(summary.genuine_alternatives.map((arm) => arm.session), ["arm-b"]);
});

test("markdown report lists genuine alternatives and per-arm counts", () => {
  const markdown = renderMarkdown(summarize(rows), rows);
  assert.match(markdown, /Arms judged genuine alternative implementations/);
  assert.match(markdown, /arm-b/);
  assert.match(markdown, /\| `arm-a` \| trial-001 \| checker \| skill \| 1 \| 0 \| 1 \| not_alternative \|/);
});
