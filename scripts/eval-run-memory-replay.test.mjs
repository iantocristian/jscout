import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";

import { databaseState } from "./eval-run-memory.mjs";
import { replaySources } from "./eval-run-memory-replay.mjs";

test("replay source admission verifies the archived semantic fingerprint", () => {
  const directory = fs.mkdtempSync(path.join(process.env.TMPDIR ?? "/tmp", "jscout-replay-test-"));
  const artifacts = path.join(directory, "artifacts");
  const snapshots = path.join(artifacts, "memory-snapshots");
  fs.mkdirSync(snapshots, { recursive: true });
  const databasePath = path.join(snapshots, "flow-001-after-session1.db");
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
  const state = databaseState(databasePath);
  const responses = path.join(directory, "responses.jsonl");
  fs.writeFileSync(responses, `${JSON.stringify({
    pair_id: "flow",
    trial: "001",
    arm: "warm",
    phase: "session1",
    session: "memory-flow-001-warm-session1",
    model: "gpt-5.6-terra",
    reasoning: "high",
    semantic_state: state,
  })}\n`);
  const taskSet = { pairs: [{ id: "flow" }] };
  const admitted = replaySources(
    taskSet,
    [responses],
    [artifacts],
    "gpt-5.6-terra",
    "high",
  );
  assert.equal(admitted.length, 1);
  assert.deepEqual(admitted[0].semantic_state, state);

  const rows = fs.readFileSync(responses, "utf8").trim().split(/\n/).map(JSON.parse);
  rows[0].semantic_state.semantic_sha256 = "0".repeat(64);
  fs.writeFileSync(responses, `${JSON.stringify(rows[0])}\n`);
  assert.throws(
    () => replaySources(taskSet, [responses], [artifacts], "gpt-5.6-terra", "high"),
    /fingerprint mismatch/,
  );
});
