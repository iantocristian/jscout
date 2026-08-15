#!/usr/bin/env node

// Materialize and prepare every task in a PR-replay suite. Preparation is
// intentionally separate from execution: install/build once, freeze the
// prepared parent snapshot, and prove the hidden regression tests fail before
// any model sees the task.

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { buildSnapshot } from "./eval-pr-snapshot.mjs";

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
  for (const required of ["tasks", "repository", "runs-root"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

export function prepareSuite({ tasksFile, repository, runsRoot, workRoot }) {
  const taskSet = JSON.parse(fs.readFileSync(path.resolve(tasksFile), "utf8"));
  const root = path.resolve(runsRoot);
  const results = [];

  for (const task of taskSet.tasks) {
    const caseRoot = path.join(root, task.id);
    const pristine = path.join(caseRoot, "pristine");
    const gold = path.join(caseRoot, "gold");
    if (fs.existsSync(caseRoot) && fs.readdirSync(caseRoot).length > 0) {
      throw new Error(`refusing to overwrite prepared case: ${caseRoot}`);
    }

    const prepared = buildSnapshot({
      repository,
      sha: task.sha,
      parent: task.parent,
      members: (task.members ?? []).join(","),
      workspace: pristine,
      gold,
      setupCommand: task.setup_command ?? taskSet.setup_command,
      testCommand: task.test_command ?? taskSet.test_command,
      story: task.story,
      storySource: task.story_source,
      workRoot,
    });
    results.push({
      id: task.id,
      parent: prepared.parent,
      setup: prepared.setup_verification.status,
      hidden_tests: prepared.tests_verification.status,
      layer1_eligible: prepared.layer1_eligible,
    });
  }
  return results;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const results = prepareSuite({
    tasksFile: options.tasks,
    repository: options.repository,
    runsRoot: options["runs-root"],
    workRoot: options["work-root"],
  });
  process.stdout.write(`${JSON.stringify(results, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
