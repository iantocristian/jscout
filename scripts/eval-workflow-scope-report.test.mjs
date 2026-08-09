import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";

import { buildWorkflowScopeReport } from "./eval-workflow-scope-report.mjs";

test("workflow scope report scores all-participant coverage against follow-up gold", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-scope-report-"));
  const artifacts = path.join(root, "artifacts");
  const snapshots = path.join(artifacts, "memory-snapshots");
  fs.mkdirSync(snapshots, { recursive: true });
  const databasePath = path.join(snapshots, "flow-001-after-session1.db");
  const database = new DatabaseSync(databasePath);
  database.exec(`
    CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT NOT NULL);
    CREATE TABLE graph_nodes(node_key TEXT PRIMARY KEY, display_name TEXT NOT NULL, file_id INTEGER);
    CREATE TABLE semantic_artifacts(
      id INTEGER PRIMARY KEY, supersedes_artifact_id INTEGER, artifact_type TEXT,
      prompt_version TEXT, body_json TEXT
    );
  `);
  database.prepare("INSERT INTO files(id,path) VALUES(?,?)").run(1, "src/entry.ts");
  database.prepare("INSERT INTO files(id,path) VALUES(?,?)").run(2, "src/helper.ts");
  database.prepare("INSERT INTO graph_nodes VALUES(?,?,?)")
    .run("sym:src/entry.ts#::entry@1", "entry", 1);
  database.prepare("INSERT INTO graph_nodes VALUES(?,?,?)")
    .run("sym:src/helper.ts#::helper@1", "helper", 2);
  database.prepare(
    "INSERT INTO semantic_artifacts VALUES(1,NULL,'workflow','annotate/v2',?)",
  ).run(JSON.stringify({ participants: [
    { anchor: "sym:src/entry.ts#::entry@1", role: "entry", scope: "defining" },
    { anchor: "sym:src/helper.ts#::helper@1", role: "helper", scope: "supporting" },
  ] }));
  database.close();
  const responses = path.join(root, "responses.jsonl");
  fs.writeFileSync(responses, `${JSON.stringify({
    pair_id: "flow", trial: "001", phase: "session1", arm: "warm", session: "s1",
  })}\n`);
  const taskSet = {
    schema_version: 1,
    pairs: [{
      id: "flow",
      admission: {
        anchor_class: "anchor-free",
        transfer_triviality: { status: "pass", model: "gpt-5.6-terra", reasoning: "high" },
      },
      session1: { prompt: "A", gold: { files: ["src/entry.ts"], symbols: ["entry"] } },
      session2: {
        prompt: "B",
        gold: {
          files: ["src/entry.ts", "src/helper.ts"],
          symbols: ["entry", "helper"],
        },
      },
    }],
  };

  const report = buildWorkflowScopeReport({
    taskSet,
    responseFiles: [responses],
    artifactDirectories: [artifacts],
  });
  assert.equal(report.runs, 1);
  assert.equal(report.annotate_v2_runs, 1);
  assert.equal(report.micro.recall, 1);
  assert.equal(report.micro.defining_matches, 1);
  assert.equal(report.micro.supporting_matches, 1);
  assert.equal(report.details[0].supporting_count, 1);
});
