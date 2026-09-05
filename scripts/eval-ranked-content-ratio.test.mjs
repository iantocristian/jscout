import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { measureRankedResponse } from "./eval-ranked-content-ratio.mjs";

const response = (...hits) => ({ default_match: "hybrid", hits });

test("only escaped UTF-8 snippet bytes count as content", () => {
  const hit = { snippet: 'é\n"x"\\', anchor: "a", at: "a.ts:1" };
  const result = measureRankedResponse(response(hit));
  assert.equal(result.snippet_bytes, 11);
  assert.equal(result.hit_bytes, Buffer.byteLength(JSON.stringify(hit)));
  assert.equal(result.content_ratio, 11 / result.hit_bytes);
  assert.equal(result.majority_content_hits, 0);
});

test("hit metadata reduces the ratio; response metadata never enters it", () => {
  const hit = { snippet: "x".repeat(100) };
  const before = measureRankedResponse(response(hit));
  const after = measureRankedResponse(response({ ...hit, anchor: "a".repeat(200) }));
  assert.equal(before.majority_content_hits, 1);
  assert.equal(after.majority_content_hits, 0);
  assert.equal(after.snippet_bytes, before.snippet_bytes);
  assert.ok(after.content_ratio < before.content_ratio);
  assert.deepEqual(measureRankedResponse({
    ...response(hit), snapshot: "s".repeat(100), publication_snapshot: "p".repeat(100),
    graph: { nodes: ["metadata"] }, semantic_memory: { summary: "not source" },
  }), before);
});

test("aggregate ratio is byte-weighted and per-hit failures remain visible", () => {
  const result = measureRankedResponse(response(
    { snippet: "x".repeat(1000) },
    { snippet: "", snippet_truncated: true },
  ));
  assert.equal(result.hit_count, 2);
  assert.equal(result.majority_content_hits, 1);
  assert.equal(result.content_ratio, result.snippet_bytes / result.hit_bytes);
  assert.notEqual(result.content_ratio,
    (result.hits[0].content_ratio + result.hits[1].content_ratio) / 2);
  assert.equal(result.hits[1].snippet_truncated, true);
  const tie = measureRankedResponse(response({ snippet: "x".repeat(14) }));
  assert.equal(tie.content_ratio, 0.5);
  assert.equal(tie.majority_content_hits, 0);
});

test("empty results have no ratio; exhaustive, debug, and malformed results are rejected", () => {
  assert.deepEqual(measureRankedResponse(response()), {
    hit_count: 0, snippet_bytes: 0, hit_bytes: 0, content_ratio: null,
    majority_content_hits: 0, hits: [],
  });
  for (const invalid of [null, {}, { hits: [] }, { default_match: "lexical", hits: [] },
    { ...response(), effective: {} }, response({ snippet: null }), response({ content: "code" })]) {
    assert.throws(() => measureRankedResponse(invalid));
  }
});

test("CLI reads compact JSON from stdin and reports invalid input as failure", () => {
  const script = new URL("./eval-ranked-content-ratio.mjs", import.meta.url);
  const input = response({ snippet: "export const value = 1;" });
  const run = (value) => spawnSync(process.execPath, [fileURLToPath(script)], {
    encoding: "utf8", input: JSON.stringify(value),
  });
  const valid = run(input);
  assert.equal(valid.status, 0, valid.stderr);
  assert.deepEqual(JSON.parse(valid.stdout), [{ file: null, ...measureRankedResponse(input) }]);
  assert.notEqual(run({ hits: [] }).status, 0);
});
