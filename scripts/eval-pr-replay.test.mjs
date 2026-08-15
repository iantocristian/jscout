import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { admitCandidate, classifyFiles, isTestPath, mineCandidates } from "./eval-pr-mine.mjs";
import { buildSnapshot } from "./eval-pr-snapshot.mjs";
import { prepareSuite } from "./eval-pr-prepare.mjs";
import { diffTrees, scoreCoverage } from "./eval-pr-grade.mjs";

function git(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

function makeFixtureRepo() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-replay-fixture-"));
  git(repo, ["init", "-q", "-b", "main"]);
  git(repo, ["config", "user.email", "eval@test"]);
  git(repo, ["config", "user.name", "eval"]);

  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  fs.mkdirSync(path.join(repo, "tests"), { recursive: true });
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = () => 1;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = () => 2;\n");
  fs.writeFileSync(path.join(repo, "tests/a.test.js"), "// existing test\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "base"]);

  // The "real change": touches two code files and adds a test.
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = (x) => x + 1;\nexport const helper = () => 0;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = (x) => x + 2;\n");
  fs.writeFileSync(path.join(repo, "tests/feature.test.js"), "// asserts new behavior\nprocess.exit(0);\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "feat: increment inputs"]);
  return { repo, sha: git(repo, ["rev-parse", "HEAD"]).trim() };
}

test("test-path and admission classification", () => {
  assert.ok(isTestPath("tests/feature.test.js"));
  assert.ok(isTestPath("src/__tests__/x.js"));
  assert.ok(!isTestPath("src/latest.js"));
  const classes = classifyFiles(["src/a.js", "tests/a.test.js", "README.md"]);
  assert.deepEqual(classes.code, ["src/a.js"]);
  assert.deepEqual(classes.tests, ["tests/a.test.js"]);
  assert.equal(
    admitCandidate({ classes: classifyFiles(["README.md"]), linesChanged: 50 }, { minLines: 20, maxLines: 600, maxFiles: 25 }),
    "no code files",
  );
});

test("miner emits scaffolds without diff content", () => {
  const { repo } = makeFixtureRepo();
  const report = mineCandidates(repo, { limit: "10", "min-lines": "1", "max-lines": "600", "max-files": "25" });
  assert.equal(report.candidates.length, 1);
  const candidate = report.candidates[0];
  assert.equal(candidate.subject, "feat: increment inputs");
  assert.deepEqual(candidate.files.code.sort(), ["src/a.js", "src/b.js"]);
  assert.equal(candidate.has_tests, true);
  assert.equal(candidate.story, null);
  assert.ok(!JSON.stringify(candidate).includes("x + 1"), "scaffold must not contain patch content");
  fs.rmSync(repo, { recursive: true, force: true });
});

test("snapshot exports history-free workspace and out-of-sandbox gold", () => {
  const { repo, sha } = makeFixtureRepo();
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-replay-out-"));
  const workspace = path.join(base, "workspace");
  const gold = path.join(base, "gold");
  const task = buildSnapshot({ repository: repo, sha, workspace, gold, testCommand: "sh tests/run.sh 2>/dev/null || node tests/feature.test.js" });

  assert.ok(!fs.existsSync(path.join(workspace, ".git")), "workspace must not contain .git");
  assert.equal(
    fs.readFileSync(path.join(workspace, "src/a.js"), "utf8"),
    "export const a = () => 1;\n",
    "workspace must be the parent state",
  );
  assert.ok(fs.existsSync(path.join(gold, "gold.patch")));
  assert.ok(fs.existsSync(path.join(gold, "gold-tests.patch")));
  assert.deepEqual(task.files.code.sort(), ["src/a.js", "src/b.js"]);
  // The fixture's "test" exits 0 on the parent, so fail-to-pass must REJECT it.
  assert.equal(task.tests_verification.status, "rejected");
  assert.equal(task.layer1_eligible, false);

  assert.throws(
    () => buildSnapshot({ repository: repo, sha, workspace: path.join(base, "ws2"), gold: path.join(base, "ws2", "gold") }),
    /must not be inside the workspace/,
  );
  fs.rmSync(base, { recursive: true, force: true });
  fs.rmSync(repo, { recursive: true, force: true });
});

test("preparation freezes setup outputs and admits fail-to-pass hidden tests", () => {
  const { repo, sha } = makeFixtureRepo();
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-prepare-out-"));
  const tasksFile = path.join(base, "tasks.json");
  const runsRoot = path.join(base, "runs");
  fs.writeFileSync(
    tasksFile,
    JSON.stringify({
      setup_command:
        "node -e \"require('fs').writeFileSync('prepared.txt', 'yes')\"",
      test_command:
        "node -e \"const fs=require('fs'); if(fs.readFileSync('prepared.txt','utf8')!=='yes'||!fs.readFileSync('src/a.js','utf8').includes('x + 1')) process.exit(1)\"",
      tasks: [{
        id: "prepared-fixture",
        sha,
        story: "Increment inputs.",
        story_source: "fixture",
      }],
    }),
  );

  const [result] = prepareSuite({ tasksFile, repository: repo, runsRoot });
  assert.equal(result.setup, "pass");
  assert.equal(result.hidden_tests, "fail-to-pass");
  assert.equal(result.layer1_eligible, true);
  assert.equal(
    fs.readFileSync(path.join(runsRoot, "prepared-fixture", "pristine", "prepared.txt"), "utf8"),
    "yes",
  );
  const task = JSON.parse(
    fs.readFileSync(path.join(runsRoot, "prepared-fixture", "gold", "task.json"), "utf8"),
  );
  assert.equal(task.story, "Increment inputs.");
  assert.equal(task.story_source, "fixture");

  fs.rmSync(base, { recursive: true, force: true });
  fs.rmSync(repo, { recursive: true, force: true });
});

test("arc snapshot restricts gold to member files and excludes interleaved commits", () => {
  const { repo } = makeFixtureRepo();
  const seed = git(repo, ["rev-parse", "HEAD"]).trim();
  const parent = git(repo, ["rev-parse", "HEAD^"]).trim();

  // Unrelated interleaved commit — must NOT appear in arc gold.
  fs.writeFileSync(path.join(repo, "src/unrelated.js"), "export const u = 1;\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "chore: unrelated"]);

  // Follow-up arc member: fixes an edge case the seed missed.
  fs.writeFileSync(
    path.join(repo, "src/a.js"),
    "export const a = (x) => (x ?? 0) + 1;\nexport const helper = () => 0;\n",
  );
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "fix: handle null input"]);
  const followup = git(repo, ["rev-parse", "HEAD"]).trim();

  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-arc-out-"));
  const task = buildSnapshot({
    repository: repo,
    sha: followup,
    parent,
    members: `${seed},${followup}`,
    workspace: path.join(base, "workspace"),
    gold: path.join(base, "gold"),
  });

  assert.deepEqual(task.arc_members, [seed, followup]);
  assert.ok(!task.files.code.includes("src/unrelated.js"), "interleaved commit leaked into gold");
  assert.deepEqual(task.files.code.sort(), ["src/a.js", "src/b.js"]);
  const goldPatch = fs.readFileSync(path.join(base, "gold", "gold.patch"), "utf8");
  assert.ok(!goldPatch.includes("unrelated"), "gold.patch must exclude interleaved changes");
  assert.ok(goldPatch.includes("x ?? 0"), "gold.patch must include the follow-up fix");
  assert.ok(fs.existsSync(path.join(base, "gold", "members", `${seed.slice(0, 12)}.patch`)));
  // Workspace is the state before the FIRST member.
  assert.equal(
    fs.readFileSync(path.join(base, "workspace", "src/a.js"), "utf8"),
    "export const a = () => 1;\n",
  );
  fs.rmSync(base, { recursive: true, force: true });
  fs.rmSync(repo, { recursive: true, force: true });
});

test("tree diff detects modified, added, and deleted files", () => {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-replay-diff-"));
  const pristine = path.join(base, "pristine");
  const workspace = path.join(base, "workspace");
  fs.mkdirSync(path.join(pristine, "src"), { recursive: true });
  fs.writeFileSync(path.join(pristine, "src/a.js"), "1\n");
  fs.writeFileSync(path.join(pristine, "src/gone.js"), "x\n");
  fs.cpSync(pristine, workspace, { recursive: true });
  fs.writeFileSync(path.join(workspace, "src/a.js"), "2\n");
  fs.writeFileSync(path.join(workspace, "src/new.js"), "n\n");
  fs.rmSync(path.join(workspace, "src/gone.js"));
  fs.mkdirSync(path.join(workspace, "node_modules"), { recursive: true });
  fs.writeFileSync(path.join(workspace, "node_modules/x.js"), "ignored\n");

  const changes = diffTrees(pristine, workspace);
  assert.deepEqual(changes, [
    { file: "src/a.js", status: "modified" },
    { file: "src/gone.js", status: "deleted" },
    { file: "src/new.js", status: "added" },
  ]);
  fs.rmSync(base, { recursive: true, force: true });
});

test("coverage scoring adjudicates before scoring and separates patched from mentioned", () => {
  const goldFiles = ["src/a.js", "src/b.js", "src/c.js"];
  const pendingScore = scoreCoverage({
    goldFiles,
    patchedFiles: ["src/a.js", "src/extra.js"],
    planMentioned: ["src/a.js", "src/b.js", "src/c.js"],
  });
  assert.equal(pendingScore.patched.confirmed_omission_rate, null, "no rate while adjudication pending");
  assert.deepEqual(pendingScore.patched.pending_adjudication, ["src/b.js", "src/c.js"]);
  // Mentioning every gold file must not affect the patched metrics.
  assert.deepEqual(pendingScore.patched.matched, ["src/a.js"]);
  assert.equal(pendingScore.plan_mentioned.matched.length, 3);

  const scored = scoreCoverage({
    goldFiles,
    patchedFiles: ["src/a.js", "src/extra.js"],
    planMentioned: [],
    adjudications: [
      { gold_file: "src/b.js", verdict: "omission" },
      { gold_file: "src/c.js", verdict: "not_required" },
    ],
  });
  // required = 3 - 1 not_required = 2; omissions = 1.
  assert.equal(scored.patched.confirmed_omission_rate, 0.5);
  assert.deepEqual(scored.patched.extraneous, ["src/extra.js"]);
});
