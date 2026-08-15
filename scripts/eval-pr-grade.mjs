#!/usr/bin/env node

// Grade a PR-replay attempt against the gold bundle.
//
// Layer 1 (optional): apply the admitted fail-to-pass test patch to the
// agent's workspace and run the test command; exit 0 means the agent's
// implementation satisfies the reference behavior.
//
// Layer 2 discipline (adjudicate-first): the real patch is ONE valid
// implementation, not the mandatory affected set. Every gold code file the
// agent did not patch becomes a PENDING adjudication row
// (omission | alternative_covered | not_required). The reported metric is
// confirmed-omission rate, computed only once no rows are pending.
// `patched` (files actually changed on disk) and `plan_mentioned` (files the
// agent's final answer claims to have considered) are reported separately
// and never combined — blending them makes recall gameable by narrating.

import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { overlayHiddenTests } from "./eval-hidden-tests.mjs";
import { cloneTree } from "./eval-tree-clone.mjs";

const IGNORED = new Set(["node_modules", ".git", ".jscout.db", ".jscout.db-wal", ".jscout.db-shm"]);

function walk(root, prefix = "", out = new Map()) {
  for (const entry of fs.readdirSync(path.join(root, prefix), { withFileTypes: true })) {
    if (IGNORED.has(entry.name)) continue;
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      walk(root, rel, out);
    } else if (entry.isFile()) {
      const content = fs.readFileSync(path.join(root, rel));
      out.set(rel, crypto.createHash("sha256").update(content).digest("hex"));
    }
  }
  return out;
}

export function diffTrees(pristineDir, workspaceDir) {
  const pristine = walk(pristineDir);
  const workspace = walk(workspaceDir);
  const changed = [];
  for (const [file, hash] of workspace) {
    const before = pristine.get(file);
    if (before === undefined) changed.push({ file, status: "added" });
    else if (before !== hash) changed.push({ file, status: "modified" });
  }
  for (const file of pristine.keys()) {
    if (!workspace.has(file)) changed.push({ file, status: "deleted" });
  }
  return changed.sort((a, b) => a.file.localeCompare(b.file));
}

export function diffGit(workspaceDir) {
  const rows = execFileSync("git", ["diff", "--name-status", "HEAD"], {
    cwd: workspaceDir,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  }).split(/\r?\n/).filter(Boolean);
  return rows.map((row) => {
    const parts = row.split("\t");
    const status = parts[0][0];
    return {
      file: parts[parts.length - 1],
      status: status === "A" ? "added" : status === "D" ? "deleted" : "modified",
    };
  }).sort((left, right) => left.file.localeCompare(right.file));
}

export function scoreCoverage({ goldFiles, patchedFiles, planMentioned, adjudications }) {
  const patched = new Set(patchedFiles);
  const mentioned = new Set(planMentioned ?? []);
  const verdictByFile = new Map(
    (adjudications ?? []).map((row) => [row.gold_file, row.verdict]),
  );

  const matchedPatched = goldFiles.filter((file) => patched.has(file));
  const unmatched = goldFiles.filter((file) => !patched.has(file));
  const adjudicated = { omission: [], alternative_covered: [], not_required: [] };
  const pending = [];
  for (const file of unmatched) {
    const verdict = verdictByFile.get(file);
    if (verdict === undefined || verdict === null) pending.push(file);
    else if (verdict in adjudicated) adjudicated[verdict].push(file);
    else throw new Error(`unknown adjudication verdict '${verdict}' for ${file}`);
  }

  const required = goldFiles.length - adjudicated.not_required.length;
  const confirmedOmissionRate =
    pending.length > 0 || required === 0 ? null : adjudicated.omission.length / required;

  return {
    gold_total: goldFiles.length,
    patched: {
      matched: matchedPatched,
      extraneous: [...patched].filter((file) => !goldFiles.includes(file)).sort(),
      adjudicated: {
        omission: adjudicated.omission,
        alternative_covered: adjudicated.alternative_covered,
        not_required: adjudicated.not_required,
      },
      pending_adjudication: pending,
      confirmed_omission_rate: confirmedOmissionRate,
    },
    // Reported separately, never blended into patched recall.
    plan_mentioned: {
      matched: goldFiles.filter((file) => mentioned.has(file)),
      count: mentioned.size,
    },
  };
}

function runLayer1(workspaceDir, goldDir, testCommand, inPlace = false, workRoot = "/tmp/jr") {
  const testsPatch = path.join(goldDir, "gold-tests.patch");
  if (!fs.existsSync(testsPatch)) return { status: "no-tests" };
  if (!testCommand) return { status: "skipped" };
  if (!inPlace) fs.mkdirSync(workRoot, { recursive: true });
  const probeRoot = inPlace ? null : fs.mkdtempSync(path.join(workRoot, "g-"));
  const probe = inPlace ? workspaceDir : path.join(probeRoot, "workspace");
  try {
    if (!inPlace) cloneTree(workspaceDir, probe);
    if (!overlayHiddenTests(goldDir, probe)) {
      try {
        execFileSync("git", ["apply", testsPatch], { cwd: probe, encoding: "utf8" });
      } catch (error) {
        return {
          status: "tests-did-not-apply",
          detail: `${error.stderr ?? error.message}`.trim().split("\n").slice(-3).join("\n"),
        };
      }
    }
    try {
      execFileSync("sh", ["-c", testCommand], {
        cwd: probe,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
        maxBuffer: 64 * 1024 * 1024,
      });
      return { status: "pass", exit_code: 0 };
    } catch (error) {
      const output = `${error.stdout ?? ""}${error.stderr ?? ""}`;
      return {
        status: "fail",
        exit_code: error.status ?? 1,
        output_tail: output.split("\n").slice(-40).join("\n"),
      };
    }
  } finally {
    if (probeRoot) fs.rmSync(probeRoot, { recursive: true, force: true });
  }
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
  for (const required of ["workspace", "gold"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const goldDir = path.resolve(options.gold);
  const task = JSON.parse(fs.readFileSync(path.join(goldDir, "task.json"), "utf8"));
  const goldFiles = task.files.code;

  const workspaceDir = path.resolve(options.workspace);
  const changes = options.pristine
    ? diffTrees(path.resolve(options.pristine), workspaceDir)
    : diffGit(workspaceDir);
  const response = options.response
    ? JSON.parse(fs.readFileSync(options.response, "utf8"))
    : {};
  const adjudications = options.adjudications && fs.existsSync(options.adjudications)
    ? fs.readFileSync(options.adjudications, "utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))
    : [];

  const coverage = scoreCoverage({
    goldFiles,
    patchedFiles: changes.map((change) => change.file),
    planMentioned: response.files,
    adjudications: adjudications.filter((row) => row.task_id === undefined || row.task_id === task.sha),
  });

  const layer1 = task.layer1_eligible
    ? runLayer1(
        workspaceDir,
        goldDir,
        options["test-command"],
        options["in-place"] === "true",
        path.resolve(options["work-root"] ?? "/tmp/jr"),
      )
    : { status: "not-eligible" };

  const report = {
    sha: task.sha,
    subject: task.subject,
    changes,
    layer1,
    coverage,
    adjudication_queue: coverage.patched.pending_adjudication.map((file) => ({
      task_id: task.sha,
      gold_file: file,
      verdict: null,
    })),
  };
  const output = `${JSON.stringify(report, null, 2)}\n`;
  if (options.output) fs.writeFileSync(options.output, output);
  else process.stdout.write(output);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
