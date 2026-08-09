#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  certifyMemoryAnchors,
  cloneDatabase,
  databaseState,
  runSession,
  validateTaskSet,
} from "./eval-run-memory.mjs";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
    "session-prefix": "memory-r1",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of [
    "tasks",
    "repository",
    "jscout",
    "source-responses",
    "source-artifacts",
    "responses",
    "telemetry",
    "artifacts",
  ]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  if (!/^[A-Za-z0-9._-]+$/.test(options["session-prefix"])) {
    throw new Error("--session-prefix may contain only letters, numbers, dot, underscore, and hyphen");
  }
  return options;
}

function splitPaths(value) {
  return value.split(",").map((entry) => path.resolve(entry.trim())).filter(Boolean);
}

function jsonLines(text) {
  return text.split(/\r?\n/).filter(Boolean).map((line, index) => {
    try { return JSON.parse(line); } catch (error) {
      throw new Error(`invalid JSONL row ${index + 1}: ${error.message}`);
    }
  });
}

function sameSemanticState(left, right) {
  return left?.schema_version === right?.schema_version &&
    left?.artifacts === right?.artifacts &&
    left?.supports === right?.supports &&
    left?.semantic_sha256 === right?.semantic_sha256;
}

export function replaySources(taskSet, responseFiles, artifactDirectories, model, reasoning) {
  if (responseFiles.length !== artifactDirectories.length) {
    throw new Error("--source-responses and --source-artifacts must have the same number of paths");
  }
  const pairIds = new Set(taskSet.pairs.map((pair) => pair.id));
  const sources = [];
  for (let index = 0; index < responseFiles.length; index += 1) {
    const rows = jsonLines(fs.readFileSync(responseFiles[index], "utf8"));
    const firstRows = rows.filter((row) => row.arm === "warm" && row.phase === "session1");
    const trials = new Set(firstRows.map((row) => row.trial));
    if (trials.size !== 1) {
      throw new Error(`${responseFiles[index]} must contain session-1 rows for exactly one trial`);
    }
    for (const row of firstRows) {
      if (!pairIds.has(row.pair_id)) throw new Error(`unknown replay pair: ${row.pair_id}`);
      if (row.model !== model || row.reasoning !== reasoning) {
        throw new Error(
          `${row.session}: source ${row.model}/${row.reasoning} does not match replay ${model}/${reasoning}`,
        );
      }
      if (row.runner_error) throw new Error(`${row.session}: source session has a runner error`);
      const snapshot = path.join(
        artifactDirectories[index],
        "memory-snapshots",
        `${row.pair_id}-${row.trial}-after-session1.db`,
      );
      if (!fs.existsSync(snapshot)) throw new Error(`missing replay snapshot: ${snapshot}`);
      const actual = databaseState(snapshot);
      if (!sameSemanticState(actual, row.semantic_state)) {
        throw new Error(`${row.session}: replay snapshot semantic fingerprint mismatch`);
      }
      sources.push({ row, snapshot, semantic_state: actual });
    }
  }
  const expected = taskSet.pairs.length * responseFiles.length;
  if (sources.length !== expected) {
    throw new Error(`found ${sources.length} replay sources; expected ${expected}`);
  }
  return sources.sort((left, right) =>
    left.row.trial.localeCompare(right.row.trial) ||
    left.row.pair_id.localeCompare(right.row.pair_id));
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = validateTaskSet(JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8")));
  const repository = path.resolve(options.repository);
  certifyMemoryAnchors(taskSet, repository);
  const pairById = new Map(taskSet.pairs.map((pair) => [pair.id, pair]));
  const sourceResponseFiles = splitPaths(options["source-responses"]);
  const sourceArtifactDirectories = splitPaths(options["source-artifacts"]);
  const sources = replaySources(
    taskSet,
    sourceResponseFiles,
    sourceArtifactDirectories,
    options.model,
    options.reasoning,
  );
  const responses = path.resolve(options.responses);
  const telemetry = path.resolve(options.telemetry);
  const artifacts = path.resolve(options.artifacts);
  const schema = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../eval/agent-response.schema.json",
  );
  const lockPath = `${responses}.lock`;
  let lockFd;
  try {
    lockFd = fs.openSync(lockPath, "wx");
  } catch (error) {
    if (error.code === "EEXIST") throw new Error(`another evaluation writer holds ${lockPath}`);
    throw error;
  }
  const releaseLock = () => {
    if (lockFd === undefined) return;
    fs.closeSync(lockFd);
    lockFd = undefined;
    try { fs.unlinkSync(lockPath); } catch {}
  };
  process.once("exit", releaseLock);

  const resume = options.resume === "true";
  for (const output of [responses, telemetry]) {
    if (!resume && fs.existsSync(output) && fs.statSync(output).size > 0) {
      throw new Error(`refusing to append to non-empty output: ${output}`);
    }
  }
  if (!resume && fs.existsSync(artifacts) && fs.readdirSync(artifacts).length > 0) {
    throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifacts}`);
  }
  fs.mkdirSync(artifacts, { recursive: true });
  const databaseDirectory = path.join(artifacts, "databases");
  fs.mkdirSync(databaseDirectory, { recursive: true });
  const completedRows = resume && fs.existsSync(responses)
    ? jsonLines(fs.readFileSync(responses, "utf8"))
    : [];
  const completed = new Set(completedRows.map((row) => `${row.pair_id}\0${row.trial}`));
  const emitted = [];

  for (const source of sources) {
    const pair = pairById.get(source.row.pair_id);
    const key = `${pair.id}\0${source.row.trial}`;
    if (completed.has(key)) {
      process.stderr.write(`[${pair.id}] ${source.row.trial} warm replay (already complete)\n`);
      continue;
    }
    const database = path.join(
      databaseDirectory,
      `${pair.id}-${source.row.trial}-warm-r1.db`,
    );
    if (fs.existsSync(database)) {
      throw new Error(`replay database exists without a completed response: ${database}`);
    }
    cloneDatabase(source.snapshot, database, options["allow-full-copy"] === "true");
    const row = await runSession({
      options: { ...options, trial: source.row.trial },
      pair,
      task: pair.session2,
      phase: "session2",
      arm: "warm",
      database,
      repository,
      schema,
      telemetry,
      artifacts,
      seedSha256: source.row.structural_seed_sha256,
    });
    row.revision = "memory-budget-r1";
    row.replay_source_session = source.row.session;
    row.replay_source_semantic_state = source.semantic_state;
    fs.appendFileSync(responses, `${JSON.stringify(row)}\n`);
    emitted.push(row);
    completed.add(key);
  }
  process.stdout.write(`${JSON.stringify(emitted, null, 2)}\n`);
  releaseLock();
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
