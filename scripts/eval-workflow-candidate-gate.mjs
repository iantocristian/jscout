#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { pathToFileURL } from "node:url";

import { validateTaskSet } from "./eval-run-memory.mjs";

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
  for (const required of ["tasks", "repository", "database", "jscout"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function resolveGoldAnchors(database, gold, label) {
  const files = new Set(gold.files);
  const symbols = new Set(gold.symbols);
  const rows = database.prepare(
    `SELECT g.node_key AS anchor, f.path AS file, g.display_name AS symbol
     FROM graph_nodes g JOIN files f ON f.id=g.file_id
     WHERE g.node_kind='symbol'`,
  ).all().filter((row) => files.has(row.file) && symbols.has(row.symbol));
  const resolvedSymbols = new Set(rows.map((row) => row.symbol));
  const unresolved = [...symbols].filter((symbol) => !resolvedSymbols.has(symbol));
  if (unresolved.length > 0) {
    throw new Error(`${label}: unresolved symbols: ${unresolved.join(", ")}`);
  }
  return rows.sort((left, right) => left.anchor.localeCompare(right.anchor));
}

function ratio(numerator, denominator) {
  return denominator === 0 ? null : numerator / denominator;
}

export function scoreWorkflowCandidateSets(rows) {
  const details = rows.map(({ pair_id, seeds, gold, candidate_set }) => {
    const candidateAnchors = new Set(candidate_set.candidates.map((candidate) => candidate.anchor));
    const matched = gold.filter((boundary) => candidateAnchors.has(boundary.anchor));
    const missing = gold.filter((boundary) => !candidateAnchors.has(boundary.anchor));
    return {
      pair_id,
      seeds,
      fingerprint: candidate_set.fingerprint,
      candidates: candidate_set.candidates.length,
      traversal_truncated: candidate_set.traversal_truncated,
      candidate_truncated: candidate_set.candidate_truncated,
      matched: matched.length,
      missing: missing.length,
      recall: ratio(matched.length, gold.length),
      missing_boundaries: missing,
    };
  });
  const matched = details.reduce((sum, row) => sum + row.matched, 0);
  const missing = details.reduce((sum, row) => sum + row.missing, 0);
  const microRecall = ratio(matched, matched + missing);
  const noTruncation = details.every((row) =>
    !row.traversal_truncated && !row.candidate_truncated);
  const everyPair = details.every((row) => row.recall >= 0.6);
  return {
    schema_version: 1,
    pairs: details.length,
    matched,
    missing,
    micro_recall: microRecall,
    no_truncation: noTruncation,
    every_pair_recall_at_least_60_percent: everyPair,
    decision: microRecall >= 0.9 && everyPair && noTruncation ? "pass" : "fail",
    details,
  };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = validateTaskSet(
    JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8")),
  );
  const repository = path.resolve(options.repository);
  const databasePath = path.resolve(options.database);
  const jscout = path.resolve(options.jscout);
  const database = new DatabaseSync(databasePath, { readOnly: true });
  const rows = [];
  try {
    for (const pair of taskSet.pairs) {
      const seedRows = resolveGoldAnchors(database, pair.session1.gold, `${pair.id} session1`);
      const gold = resolveGoldAnchors(database, pair.session2.gold, `${pair.id} session2`);
      const seeds = seedRows.map((row) => row.anchor);
      const result = spawnSync(
        jscout,
        [
          "workflow-candidates",
          repository,
          ...seeds,
          "--database",
          databasePath,
          "--depth",
          "2",
          "--candidate-limit",
          "31",
        ],
        { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
      );
      if (result.status !== 0) {
        throw new Error(`${pair.id}: workflow candidates failed: ${result.stderr.trim()}`);
      }
      rows.push({
        pair_id: pair.id,
        seeds,
        gold,
        candidate_set: JSON.parse(result.stdout),
      });
    }
  } finally {
    database.close();
  }
  const report = scoreWorkflowCandidateSets(rows);
  const rendered = `${JSON.stringify(report, null, 2)}\n`;
  if (options.output) fs.writeFileSync(path.resolve(options.output), rendered);
  process.stdout.write(rendered);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
