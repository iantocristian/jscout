#!/usr/bin/env node

// Mine PR-replay candidates from a repository's first-parent history.
//
// Emits task scaffolds (SHAs, changed-file classes, sizes, subjects) for
// human story-writing. Stories are written from the symptom or issue text,
// never from the diff; the scaffold deliberately excludes patch content so
// the story author is not staring at the answer while writing the question.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

function git(repository, args) {
  return execFileSync("git", ["-C", repository, ...args], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
}

export function isTestPath(file) {
  return (
    /(^|\/)(tests?|__tests__|__mocks__|e2e|spec|fixtures?)\//i.test(file) ||
    /\.(test|spec)\.[cm]?[jt]sx?$/i.test(file)
  );
}

export function isCodePath(file) {
  return (
    (/\.[cm]?[jt]sx?$/i.test(file) || /\.(vue|svelte)$/i.test(file)) &&
    !file.endsWith(".d.ts")
  );
}

export function classifyFiles(files) {
  const code = [];
  const tests = [];
  const other = [];
  for (const file of files) {
    if (isTestPath(file)) tests.push(file);
    else if (isCodePath(file)) code.push(file);
    else other.push(file);
  }
  return { code, tests, other };
}

export function admitCandidate({ classes, linesChanged }, options) {
  if (classes.code.length === 0) return "no code files";
  if (classes.code.length + classes.tests.length + classes.other.length > options.maxFiles) {
    return "too many files";
  }
  if (linesChanged < options.minLines) return "too small";
  if (linesChanged > options.maxLines) return "too large";
  return null;
}

function parseArgs(argv) {
  const options = {
    "min-lines": "20",
    "max-lines": "600",
    "max-files": "25",
    limit: "200",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  if (!options.repository) throw new Error("--repository is required");
  return options;
}

export function mineCandidates(repository, options) {
  const logArgs = [
    "log",
    "--first-parent",
    `--max-count=${options.limit}`,
    "--pretty=format:%H%x00%P%x00%cI%x00%s",
  ];
  if (options.since) logArgs.push(`--since=${options.since}`);
  const lines = git(repository, logArgs).split("\n").filter(Boolean);

  const candidates = [];
  const rejected = {};
  for (const line of lines) {
    const [sha, parents, date, subject] = line.split("\0");
    const parent = (parents ?? "").split(" ").filter(Boolean)[0];
    if (!parent) continue; // root commit
    const numstat = git(repository, ["diff", "--numstat", parent, sha]);
    let linesChanged = 0;
    const files = [];
    for (const row of numstat.split("\n").filter(Boolean)) {
      const [added, deleted, file] = row.split("\t");
      if (!file) continue;
      // binary files report "-"; count them as files but zero lines
      linesChanged += (Number(added) || 0) + (Number(deleted) || 0);
      files.push(file);
    }
    const classes = classifyFiles(files);
    const admission = admitCandidate(
      { classes, linesChanged },
      {
        minLines: Number(options["min-lines"]),
        maxLines: Number(options["max-lines"]),
        maxFiles: Number(options["max-files"]),
      },
    );
    if (admission) {
      rejected[admission] = (rejected[admission] ?? 0) + 1;
      continue;
    }
    candidates.push({
      id: sha.slice(0, 12),
      sha,
      parent,
      date,
      subject,
      lines_changed: linesChanged,
      files: classes,
      has_tests: classes.tests.length > 0,
      // Story authors fill these; the miner never includes diff content.
      story: null,
      story_source: null, // issue | bug-report | observed-behavior
    });
  }
  return { repository: path.resolve(repository), candidates, rejected };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const report = mineCandidates(options.repository, options);
  const output = JSON.stringify(report, null, 2);
  if (options.output) {
    fs.writeFileSync(options.output, `${output}\n`);
    process.stderr.write(
      `${report.candidates.length} candidate(s) written to ${options.output} ` +
        `(rejected: ${JSON.stringify(report.rejected)})\n`,
    );
  } else {
    process.stdout.write(`${output}\n`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
