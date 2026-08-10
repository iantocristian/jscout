import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { buildSnapshot } from "./eval-pr-snapshot.mjs";

function git(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

// End-to-end: fixture repo -> snapshot -> stubbed agent edits one gold file
// -> runner grades the workspace and records patched vs pending adjudication.
test("replay runner drives workspace, stub agent, and grading end to end", () => {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-replay-runner-"));
  const repo = path.join(base, "repo");
  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  git(base, ["init", "-q", "-b", "main", repo]);
  git(repo, ["config", "user.email", "eval@test"]);
  git(repo, ["config", "user.name", "eval"]);
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = () => 1;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = () => 2;\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "base"]);
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = (x) => x + 1;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = (x) => x + 2;\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "feat: increment"]);
  const sha = git(repo, ["rev-parse", "HEAD"]).trim();

  const runsRoot = path.join(base, "runs");
  buildSnapshot({
    repository: repo,
    sha,
    workspace: path.join(runsRoot, "replay-fixture", "pristine"),
    gold: path.join(runsRoot, "replay-fixture", "gold"),
  });

  const tasksFile = path.join(base, "tasks.json");
  fs.writeFileSync(
    tasksFile,
    JSON.stringify({
      tasks: [{ id: "replay-fixture", sha, story: "Inputs should be incremented." }],
    }),
  );

  // Stub agent: edits src/a.js in the workspace (--cd) and writes the final
  // message. It never sees the gold directory.
  const stub = path.join(base, "stub-agent.mjs");
  fs.writeFileSync(
    stub,
    `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
const argv = process.argv.slice(2);
const value = (flag) => argv[argv.indexOf(flag) + 1];
const workspace = value("--cd");
if (fs.existsSync(path.join(workspace, "gold"))) throw new Error("gold leaked into workspace");
fs.appendFileSync(path.join(workspace, "src/a.js"), "// stub edit\\n");
fs.writeFileSync(value("--output-last-message"), JSON.stringify({
  answer: "incremented a", files: ["src/a.js", "src/b.js"], symbols: ["a"], inspected_files: ["src/a.js"],
}));
console.log(JSON.stringify({ usage: { input_tokens: 10, output_tokens: 5 } }));
`,
  );
  fs.chmodSync(stub, 0o755);

  const responses = path.join(base, "responses.jsonl");
  const runner = path.resolve(path.dirname(new URL(import.meta.url).pathname), "eval-run-replay.mjs");
  execFileSync(process.execPath, [
    runner,
    "--tasks", tasksFile,
    "--runs-root", runsRoot,
    "--jscout", "/bin/true",
    "--responses", responses,
    "--telemetry", path.join(base, "telemetry.jsonl"),
    "--artifacts", path.join(base, "artifacts"),
    "--codex", stub,
    "--profiles", "grep",
    "--trial", "t1",
  ], { encoding: "utf8" });

  const rows = fs.readFileSync(responses, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
  assert.equal(rows.length, 1);
  const row = rows[0];
  assert.equal(row.task_id, "replay-fixture");
  assert.equal(row.model, "gpt-5.6-terra");
  assert.deepEqual(row.patched_files, ["src/a.js"]);
  assert.equal(row.gold_matched, 1);
  assert.equal(row.gold_pending_adjudication, 1, "src/b.js must be pending adjudication");
  assert.equal(row.total_tokens, 15);
  assert.equal(row.runner_error, undefined);

  const grade = JSON.parse(
    fs.readFileSync(path.join(base, "artifacts", "grep-replay-fixture-t1", "grade.json"), "utf8"),
  );
  // Plan mentioned both files; patched metrics must not be inflated by that.
  assert.deepEqual(grade.coverage.patched.matched, ["src/a.js"]);
  assert.deepEqual(grade.coverage.patched.pending_adjudication, ["src/b.js"]);
  assert.equal(grade.coverage.plan_mentioned.matched.length, 2);
  assert.equal(grade.coverage.patched.confirmed_omission_rate, null);

  fs.rmSync(base, { recursive: true, force: true });
});
