import assert from "node:assert/strict";
import test from "node:test";

import { certifyTask, extractAnchors } from "./eval-anchor-certify.mjs";

test("identifier-like prompt tokens are extracted as strong anchors", () => {
  const { identifiers, words } = extractAnchors(
    "Where does `enqueueSlackAssistantRequest` gate empty requests before order_events dispatch?",
  );
  assert.ok(identifiers.includes("enqueueSlackAssistantRequest"));
  assert.ok(identifiers.includes("order_events"));
  assert.ok(!words.includes("where"));
  assert.ok(!words.includes("before"));
});

test("task whose prompt identifier appears in gold is anchored", () => {
  const gold = new Map([
    ["src/slack.ts", "export function enqueueSlackAssistantRequest(input) { return input; }"],
  ]);
  const certificate = certifyTask(
    "Which function owns enqueueSlackAssistantRequest gating?",
    gold,
  );
  assert.equal(certificate.status, "anchored");
  assert.equal(certificate.identifierHits[0].anchor, "enqueueSlackAssistantRequest");
});

test("behavioral prompt with no lexical overlap is anchor-free", () => {
  const gold = new Map([
    ["src/redact.ts", "export class Sweeper { run(rows) { return rows.filter(Boolean); } }"],
  ]);
  const certificate = certifyTask(
    "After a crashed execution, what marks leftover records as finished during startup?",
    gold,
  );
  assert.equal(certificate.status, "anchor-free");
});

test("prose-only overlap is weak, not anchored", () => {
  const gold = new Map([
    ["src/cleanup.ts", "// startup cleanup pass\nexport const sweep = () => {};"],
  ]);
  const certificate = certifyTask(
    "What performs the startup cleanup of stale rows?",
    gold,
  );
  assert.equal(certificate.status, "weak");
  assert.equal(certificate.identifierHits.length, 0);
  assert.ok(certificate.wordHits.some((hit) => hit.anchor === "startup"));
});

test("substring matches do not count as whole-token anchors", () => {
  const gold = new Map([["src/a.ts", "const retryable = 1;"]]);
  const certificate = certifyTask("Where is the retry policy?", gold);
  assert.equal(certificate.status, "anchor-free");
});

test("sentence punctuation does not turn a prose word into an identifier anchor", () => {
  const { identifiers, words } = extractAnchors("Update the record. Return its owner.");
  assert.ok(!identifiers.includes("record."));
  assert.ok(words.includes("record"));
});
