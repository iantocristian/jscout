import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";

import { databaseState, promptFor, validateTaskSet } from "./eval-run-memory.mjs";

function taskSet() {
  return {
    schema_version: 1,
    pairs: [{
      id: "flow-1",
      admission: {
        anchor_class: "anchor-free",
        transfer_triviality: { status: "pass", model: "gpt-5.6-terra", reasoning: "high" },
      },
      session1: { prompt: "Trace A", gold: { files: ["a.ts"], symbols: ["a"] } },
      session2: { prompt: "Trace B", gold: { files: ["b.ts"], symbols: ["b"] } },
    }],
  };
}

test("memory task admission requires anchor and transfer certificates", () => {
  assert.equal(validateTaskSet(taskSet()).pairs.length, 1);
  const invalid = taskSet();
  invalid.pairs[0].admission.transfer_triviality.status = "fail";
  assert.throws(() => validateTaskSet(invalid), /transfer-triviality/);
});

test("session prompts isolate write-back to session 1 and freshness handling to session 2", () => {
  const first = promptFor(taskSet().pairs[0].session1, "session1");
  const second = promptFor(taskSet().pairs[0].session2, "session2");
  assert.match(first, /record it with jscout annotate/);
  assert.doesNotMatch(second, /record it with jscout annotate/);
  assert.match(second, /re-verify degraded\/stale claims/);
});

test("database state reports schema and semantic row counts", () => {
  const directory = fs.mkdtempSync(path.join(process.env.TMPDIR ?? "/tmp", "jscout-memory-test-"));
  const databasePath = path.join(directory, "index.db");
  const database = new DatabaseSync(databasePath);
  database.exec(`
    CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
    INSERT INTO meta VALUES('schema_version', '6');
    CREATE TABLE semantic_artifacts(id INTEGER PRIMARY KEY);
    CREATE TABLE semantic_supports(artifact_id INTEGER);
    INSERT INTO semantic_artifacts VALUES(1);
    INSERT INTO semantic_supports VALUES(1);
  `);
  database.close();
  assert.deepEqual(databaseState(databasePath), {
    schema_version: "6",
    artifacts: 1,
    supports: 1,
    semantic_sha256: "9e563e2779636d974e0f41b188e2e244214356724fce62a62fbfabdf7db3f605",
  });
});
