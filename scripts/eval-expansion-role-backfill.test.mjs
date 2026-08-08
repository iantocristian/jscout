import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";

import { backfillExpansionRoles } from "./eval-expansion-role-backfill.mjs";

test("backfill classifies file-backed nodes in recorded expanded searches", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-role-backfill-"));
  const artifacts = path.join(root, "artifacts");
  fs.mkdirSync(artifacts);
  const database = new DatabaseSync(path.join(root, ".jscout.db"));
  database.exec("CREATE TABLE files(path TEXT PRIMARY KEY, role TEXT NOT NULL)");
  database.prepare("INSERT INTO files VALUES(?, ?)").run("src/a.ts", "production");
  database.prepare("INSERT INTO files VALUES(?, ?)").run("tests/a.test.ts", "test");
  database.close();

  const resultText = JSON.stringify({
    expansion: {
      nodes: [
        { file: "src/a.ts" },
        { file: "tests/a.test.ts" },
        { key: "event:ready", file: null },
      ],
    },
  });
  const event = {
    type: "item.completed",
    item: {
      type: "mcp_tool_call",
      server: "jscout",
      tool: "semantic_search",
      arguments: { expand: true },
      result: { content: [{ type: "text", text: resultText }] },
    },
  };
  fs.writeFileSync(
    path.join(artifacts, "structural-task.jsonl"),
    `${JSON.stringify(event)}\n`,
  );

  const result = backfillExpansionRoles({
    artifactDirectories: [artifacts],
    repository: root,
    profilePrefix: "structural-",
  });
  assert.equal(result.expanded_search_calls, 1);
  assert.equal(result.expansion_nodes, 3);
  assert.equal(result.expansion_file_nodes, 2);
  assert.deepEqual(result.expansion_role_counts, { production: 1, test: 1 });
  assert.equal(result.expansion_test_fixture_generated_share, 0.5);
  assert.deepEqual(result.unclassified_paths, []);
});
