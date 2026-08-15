import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { buildSnapshot } from "./eval-pr-snapshot.mjs";
import {
  REPLAY_EXECUTION_POLICY,
  profilePlan,
  promptFor,
  workspaceProcessGroups,
} from "./eval-run-replay.mjs";

function git(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

test("replay profiles and forced-search contract are explicit", () => {
  assert.deepEqual(profilePlan("structural"), { usesJscout: true, stages: [] });
  assert.deepEqual(profilePlan("checker"), { usesJscout: true, stages: ["enrich"] });
  assert.deepEqual(profilePlan("checker-embed"), {
    usesJscout: true,
    stages: ["enrich", "embed"],
  });
  assert.deepEqual(profilePlan("checker-scout"), {
    usesJscout: true,
    stages: ["enrich", "scout"],
  });
  assert.deepEqual(profilePlan("checker-scout-embed"), {
    usesJscout: true,
    stages: ["enrich", "scout", "embed-product"],
  });
  assert.deepEqual(profilePlan("production-order"), {
    usesJscout: true,
    stages: ["scout", "enrich", "embed-product"],
  });
  assert.throws(() => profilePlan("invented"), /unknown profile/);

  const natural = promptFor({ story: "Fix it." }, "skill");
  const forced = promptFor({ story: "Fix it." }, "forced");
  assert.ok(!natural.includes("Do not use grep"));
  assert.ok(natural.includes("Dependencies are already installed"));
  assert.ok(natural.includes("localhost test servers"));
  assert.ok(natural.includes("do not access external network services"));
  assert.ok(forced.includes("Use jscout exclusively"));
  assert.ok(forced.includes("directly read files and line ranges identified by jscout"));

  const constrained = promptFor({
    story: "Fix it.",
    execution_notes: ["Do not start the persistent package watcher."],
  }, "skill");
  assert.ok(constrained.includes("Task-specific execution constraints"));
  assert.ok(constrained.includes("Do not start the persistent package watcher"));
  assert.deepEqual(REPLAY_EXECUTION_POLICY, {
    networkAccess: true,
    networkPolicy: "prompt-restricted-external; loopback-required",
    webTools: false,
  });
});

test("workspace process cleanup selects only exact workspace-rooted groups", () => {
  const rows = [
    "  101   90 node /private/tmp/jr/r-one/node_modules/jest/bin/jest.js",
    "  102   90 next-server",
    "  201  200 node /private/tmp/jr/r-two/node_modules/jest/bin/jest.js",
    "  301  300 node /private/tmp/jr/r-one-other/tool.js",
  ].join("\n");
  assert.deepEqual(workspaceProcessGroups(rows, "/private/tmp/jr/r-one"), [{
    pgid: 90,
    pids: [101],
    commands: ["node /private/tmp/jr/r-one/node_modules/jest/bin/jest.js"],
  }]);
});

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
fs.writeFileSync(path.join(path.dirname(value("--output-last-message")), "agent-argv.json"), JSON.stringify(argv));
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
    "--repository", repo,
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
  assert.equal(row.treatment, "control");
  assert.equal(row.model, "gpt-5.6-terra");
  assert.deepEqual(row.patched_files, ["src/a.js"]);
  assert.equal(row.gold_matched, 1);
  assert.equal(row.gold_pending_adjudication, 1, "src/b.js must be pending adjudication");
  assert.equal(row.total_tokens, 15);
  assert.equal(row.input_tokens, 10);
  assert.equal(row.cached_input_tokens, 0);
  assert.equal(row.noncached_input_tokens, 10);
  assert.equal(row.output_tokens, 5);
  assert.equal(row.command_calls, 0);
  assert.equal(row.failed_command_calls, 0);
  assert.equal(row.command_output_bytes, 0);
  assert.equal(row.max_command_output_bytes, 0);
  assert.equal(row.execution_network_access, true);
  assert.equal(
    row.execution_network_policy,
    "prompt-restricted-external; loopback-required",
  );
  assert.equal(row.runner_error, undefined);

  const agentArgv = JSON.parse(
    fs.readFileSync(path.join(base, "artifacts", "grep-control-replay-fixture-t1", "agent-argv.json"), "utf8"),
  );
  assert.ok(agentArgv.includes("sandbox_workspace_write.network_access=true"));

  const grade = JSON.parse(
    fs.readFileSync(path.join(base, "artifacts", "grep-control-replay-fixture-t1", "grade.json"), "utf8"),
  );
  // Plan mentioned both files; patched metrics must not be inflated by that.
  assert.deepEqual(grade.coverage.patched.matched, ["src/a.js"]);
  assert.deepEqual(grade.coverage.patched.pending_adjudication, ["src/b.js"]);
  assert.equal(grade.coverage.plan_mentioned.matched.length, 2);
  assert.equal(grade.coverage.patched.confirmed_omission_rate, null);

  fs.rmSync(base, { recursive: true, force: true });
});
