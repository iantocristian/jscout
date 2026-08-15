#!/usr/bin/env node

// Build a leakage-safe PR-replay workspace and gold bundle.
//
// The agent workspace is a `git archive` export of the parent commit — it
// contains NO `.git` directory, because a parent *checkout* still holds the
// real commit in its object store, one `git log` away from the answer key.
// All gold material (full patch, code-only patch, test-only patch, task
// metadata) is written to a separate directory that must live outside the
// workspace; the runner must never mount it into the agent sandbox.
//
// `--test-command` runs the fail-to-pass admission gate: the test-only patch
// must apply cleanly to the parent snapshot on its own and the command must
// FAIL there (the tests encode the intended behavior, which the parent does
// not yet have). Tests that pass on the parent — or that cannot apply
// without the production patch — are excluded from layer-1 grading.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { classifyFiles } from "./eval-pr-mine.mjs";
import { overlayHiddenTests } from "./eval-hidden-tests.mjs";
import { cloneTree } from "./eval-tree-clone.mjs";

function git(repository, args, opts = {}) {
  return execFileSync("git", ["-C", repository, ...args], {
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
    ...opts,
  });
}

function gitBytes(repository, args) {
  return execFileSync("git", ["-C", repository, ...args], {
    maxBuffer: 256 * 1024 * 1024,
  });
}

function run(command, args, cwd) {
  const result = { code: 0, output: "" };
  try {
    result.output = execFileSync(command, args, {
      cwd,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    result.code = error.status ?? 1;
    result.output = `${error.stdout ?? ""}${error.stderr ?? ""}`;
  }
  return result;
}

function assertDisjoint(workspace, gold) {
  const a = path.resolve(workspace);
  const b = path.resolve(gold);
  const rel = path.relative(a, b);
  const relBack = path.relative(b, a);
  if (rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel))) {
    throw new Error("gold directory must not be inside the workspace");
  }
  if (relBack === "" || (!relBack.startsWith("..") && !path.isAbsolute(relBack))) {
    throw new Error("workspace must not be inside the gold directory");
  }
}

function changedFiles(repository, parent, sha) {
  const rows = git(repository, ["diff", "--name-status", "-M", parent, sha])
    .split("\n")
    .filter(Boolean);
  const files = [];
  for (const row of rows) {
    const parts = row.split("\t");
    const status = parts[0];
    // For renames the post-image path is the last column.
    files.push({ status: status[0], file: parts[parts.length - 1] });
  }
  return files;
}

export function buildSnapshot(options) {
  const repository = path.resolve(options.repository);
  const workspace = path.resolve(options.workspace);
  const gold = path.resolve(options.gold);
  assertDisjoint(workspace, gold);
  const workRoot = path.resolve(options.workRoot ?? "/tmp/jr");
  fs.mkdirSync(workRoot, { recursive: true });
  for (const target of [workspace, gold]) {
    if (fs.existsSync(target) && fs.readdirSync(target).length > 0) {
      throw new Error(`refusing to write into non-empty directory: ${target}`);
    }
    fs.mkdirSync(target, { recursive: true });
  }

  const sha = git(repository, ["rev-parse", options.sha]).trim();
  const parent = options.parent
    ? git(repository, ["rev-parse", options.parent]).trim()
    : git(repository, ["rev-parse", `${sha}^`]).trim();
  const subject = git(repository, ["log", "-1", "--pretty=%s", sha]).trim();

  // Arc mode: the change spans several commits (seed + follow-ups). `sha` is
  // the LAST arc member, `parent` the parent of the FIRST. On a busy repo,
  // diff(parent, sha) would sweep in every unrelated interleaved commit, so
  // gold is restricted to the union of the arc members' own files.
  const members = (options.members ?? "")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean)
    .map((member) => git(repository, ["rev-parse", member]).trim());
  const arcScope = new Set();
  for (const member of members) {
    const rows = git(repository, ["diff", "--name-only", "-M", `${member}^`, member])
      .split("\n")
      .filter(Boolean);
    for (const file of rows) arcScope.add(file);
  }
  const scopeArgs = members.length > 0 ? ["--", ...arcScope] : [];

  // History-free export of the parent state.
  const tarPath = path.join(os.tmpdir(), `jscout-replay-${sha.slice(0, 12)}.tar`);
  git(repository, ["archive", "--format=tar", "-o", tarPath, parent]);
  execFileSync("tar", ["-xf", tarPath, "-C", workspace]);
  fs.unlinkSync(tarPath);
  if (fs.existsSync(path.join(workspace, ".git"))) {
    throw new Error("workspace export unexpectedly contains .git");
  }

  // Preparation is part of the frozen base, not part of an agent run. This is
  // where repository-specific dependency installation and builds happen.
  // The resulting node_modules/build outputs are cloned into every arm.
  let setupVerification = { status: "skipped" };
  if (options.setupCommand) {
    const setupRun = run("sh", ["-c", options.setupCommand], workspace);
    setupVerification = {
      status: setupRun.code === 0 ? "pass" : "failed",
      exit_code: setupRun.code,
      command: options.setupCommand,
      output_tail: setupRun.output.split("\n").slice(-25).join("\n"),
    };
    if (setupRun.code !== 0) {
      throw new Error(
        `setup command failed (${setupRun.code}):\n${setupVerification.output_tail}`,
      );
    }
    if (fs.existsSync(path.join(workspace, ".git"))) {
      throw new Error("setup command introduced .git into the history-free workspace");
    }
  }

  const changes = changedFiles(repository, parent, sha).filter(
    (change) => members.length === 0 || arcScope.has(change.file),
  );
  const classes = classifyFiles(changes.map((change) => change.file));

  fs.writeFileSync(
    path.join(gold, "gold.patch"),
    git(repository, ["diff", parent, sha, ...scopeArgs]),
  );
  if (members.length > 0) {
    const membersDir = path.join(gold, "members");
    fs.mkdirSync(membersDir, { recursive: true });
    for (const member of members) {
      fs.writeFileSync(
        path.join(membersDir, `${member.slice(0, 12)}.patch`),
        git(repository, ["show", "--format=%H%n%s%n%b%n---", member]),
      );
    }
  }
  if (classes.tests.length > 0) {
    fs.writeFileSync(
      path.join(gold, "gold-tests.patch"),
      git(repository, ["diff", parent, sha, "--", ...classes.tests]),
    );
    const oracleRoot = path.join(gold, "gold-tests");
    for (const file of classes.tests) {
      const change = changes.find((entry) => entry.file === file);
      if (change?.status === "D") continue;
      const destination = path.join(oracleRoot, file);
      fs.mkdirSync(path.dirname(destination), { recursive: true });
      fs.writeFileSync(destination, gitBytes(repository, ["show", `${sha}:${file}`]));
    }
  }
  const nonTest = [...classes.code, ...classes.other];
  if (nonTest.length > 0) {
    fs.writeFileSync(
      path.join(gold, "gold-code.patch"),
      git(repository, ["diff", parent, sha, "--", ...nonTest]),
    );
  }

  // Fail-to-pass admission gate.
  let testsVerification = { status: "skipped" };
  if (classes.tests.length === 0) {
    testsVerification = { status: "no-tests" };
  } else if (options.testCommand) {
    const probeRoot = fs.mkdtempSync(path.join(workRoot, "v-"));
    const parentProbe = path.join(probeRoot, "parent");
    const referenceProbe = path.join(probeRoot, "reference");
    try {
      cloneTree(workspace, parentProbe);
      overlayHiddenTests(gold, parentProbe);
      const parentRun = run("sh", ["-c", options.testCommand], parentProbe);
      if (parentRun.code === 0) {
        testsVerification = {
          status: "rejected",
          reason: "reference tests pass on the parent; they do not encode the new behavior",
          parent_exit_code: parentRun.code,
          parent_output_tail: parentRun.output.split("\n").slice(-15).join("\n"),
        };
      } else {
        cloneTree(workspace, referenceProbe);
        const codePatch = path.join(gold, "gold-code.patch");
        const applyCode = fs.existsSync(codePatch)
          ? run("git", ["apply", codePatch], referenceProbe)
          : { code: 0, output: "" };
        overlayHiddenTests(gold, referenceProbe);
        const referenceRun = applyCode.code === 0
          ? run("sh", ["-c", options.testCommand], referenceProbe)
          : applyCode;
        testsVerification = referenceRun.code === 0
          ? {
              status: "fail-to-pass",
              reason: null,
              parent_exit_code: parentRun.code,
              parent_output_tail: parentRun.output.split("\n").slice(-15).join("\n"),
              reference_exit_code: referenceRun.code,
              reference_output_tail: referenceRun.output.split("\n").slice(-15).join("\n"),
            }
          : {
              status: "rejected",
              reason: "reference tests do not pass with the reference production patch",
              parent_exit_code: parentRun.code,
              parent_output_tail: parentRun.output.split("\n").slice(-15).join("\n"),
              reference_exit_code: referenceRun.code,
              reference_output_tail: referenceRun.output.split("\n").slice(-15).join("\n"),
            };
      }
    } finally {
      fs.rmSync(probeRoot, { recursive: true, force: true });
    }
  }

  const task = {
    sha,
    parent,
    subject,
    arc_members: members,
    files: classes,
    changes,
    setup_verification: setupVerification,
    tests_verification: testsVerification,
    layer1_eligible: testsVerification.status === "fail-to-pass",
    story: options.story ?? null,
    story_source: options.storySource ?? null,
  };
  fs.writeFileSync(path.join(gold, "task.json"), `${JSON.stringify(task, null, 2)}\n`);
  return task;
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["repository", "sha", "workspace", "gold"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const task = buildSnapshot({
    repository: options.repository,
    sha: options.sha,
    parent: options.parent,
    members: options.members,
    workspace: options.workspace,
    gold: options.gold,
    setupCommand: options["setup-command"],
    testCommand: options["test-command"],
    story: options.story,
    storySource: options["story-source"],
    workRoot: options["work-root"],
  });
  process.stdout.write(`${JSON.stringify(task, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
