#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
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
  for (const required of ["tasks", "responses", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function paths(value) {
  return value.split(",").map((entry) => path.resolve(entry.trim())).filter(Boolean);
}

function readJsonl(file) {
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map((line, index) => {
    try { return JSON.parse(line); } catch (error) {
      throw new Error(`${file}:${index + 1}: ${error.message}`);
    }
  });
}

function ratio(numerator, denominator) {
  return denominator === 0 ? null : numerator / denominator;
}

function aggregate(rows) {
  const totals = rows.reduce((result, row) => ({
    matched: result.matched + row.matched_followup_count,
    missing: result.missing + row.missing_followup_count,
    defining_matches: result.defining_matches + row.defining_followup_count,
    supporting_matches: result.supporting_matches + row.supporting_followup_count,
  }), { matched: 0, missing: 0, defining_matches: 0, supporting_matches: 0 });
  return {
    ...totals,
    recall: ratio(totals.matched, totals.matched + totals.missing),
  };
}

function participantLocation(database, anchor) {
  return database.prepare(
    `SELECT f.path AS file, g.display_name AS symbol
     FROM graph_nodes g LEFT JOIN files f ON f.id=g.file_id
     WHERE g.node_key=?`,
  ).get(anchor) ?? { file: null, symbol: null };
}

export function buildWorkflowScopeReport({ taskSet, responseFiles, artifactDirectories }) {
  validateTaskSet(taskSet);
  if (responseFiles.length !== artifactDirectories.length) {
    throw new Error("responses and artifact directories must have the same number of paths");
  }
  const pairById = new Map(taskSet.pairs.map((pair) => [pair.id, pair]));
  const sources = [];
  for (let index = 0; index < responseFiles.length; index += 1) {
    for (const row of readJsonl(responseFiles[index])) {
      if (row.phase !== "session1" || row.arm !== "warm") continue;
      if (!pairById.has(row.pair_id)) throw new Error(`unknown memory pair: ${row.pair_id}`);
      sources.push({ row, artifacts: artifactDirectories[index] });
    }
  }
  const unique = new Set(sources.map(({ row }) => `${row.pair_id}\0${row.trial}`));
  if (unique.size !== sources.length) throw new Error("duplicate session-1 pair/trial rows");

  const runs = sources.sort((left, right) =>
    left.row.trial.localeCompare(right.row.trial) ||
    left.row.pair_id.localeCompare(right.row.pair_id)).map(({ row, artifacts }) => {
    const pair = pairById.get(row.pair_id);
    const snapshot = path.join(
      artifacts,
      "memory-snapshots",
      `${row.pair_id}-${row.trial}-after-session1.db`,
    );
    if (!fs.existsSync(snapshot)) throw new Error(`missing session-1 snapshot: ${snapshot}`);
    const database = new DatabaseSync(snapshot, { readOnly: true });
    try {
      const workflows = database.prepare(
        `SELECT a.id, a.prompt_version, a.body_json
         FROM semantic_artifacts a
         WHERE a.artifact_type='workflow' AND NOT EXISTS(
           SELECT 1 FROM semantic_artifacts newer WHERE newer.supersedes_artifact_id=a.id
         ) ORDER BY a.id DESC`,
      ).all();
      const workflow = workflows[0];
      const body = workflow ? JSON.parse(workflow.body_json) : {};
      const participants = Array.isArray(body.participants) ? body.participants : [];
      const resolved = participants.map((participant) => {
        const anchor = participant.anchor ?? null;
        return {
          anchor,
          role: participant.role ?? null,
          scope: participant.scope ?? null,
          ...(anchor ? participantLocation(database, anchor) : { file: null, symbol: null }),
        };
      });
      const defining = resolved.filter((participant) => participant.scope === "defining");
      const supporting = resolved.filter((participant) => participant.scope === "supporting");
      const goldFiles = new Set(pair.session2.gold.files);
      const goldSymbols = new Set(pair.session2.gold.symbols);
      const goldBoundaries = database.prepare(
        `SELECT g.node_key AS anchor, f.path AS file, g.display_name AS symbol
         FROM graph_nodes g JOIN files f ON f.id=g.file_id`,
      ).all().filter((boundary) =>
        goldFiles.has(boundary.file) && goldSymbols.has(boundary.symbol));
      const goldAnchors = new Set(goldBoundaries.map((boundary) => boundary.anchor));
      const resolvedGoldSymbols = new Set(goldBoundaries.map((boundary) => boundary.symbol));
      const unresolvedGoldSymbols = [...goldSymbols].filter((symbol) =>
        !resolvedGoldSymbols.has(symbol));
      const participantAnchors = new Set(resolved.map((participant) => participant.anchor));
      const matchedAnchors = new Set(
        [...participantAnchors].filter((anchor) => goldAnchors.has(anchor)),
      );
      const expected = goldSymbols.size;
      const missingFollowup = [
        ...goldBoundaries.filter((boundary) => !matchedAnchors.has(boundary.anchor)),
        ...unresolvedGoldSymbols.map((symbol) => ({ anchor: null, file: null, symbol })),
      ];
      const matchedFollowup = resolved.filter((participant) => goldAnchors.has(participant.anchor));
      const definingFollowup = matchedFollowup.filter((participant) =>
        participant.scope === "defining");
      const supportingFollowup = matchedFollowup.filter((participant) =>
        participant.scope === "supporting");
      return {
        pair_id: row.pair_id,
        trial: row.trial,
        session: row.session,
        workflow_count: workflows.length,
        prompt_version: workflow?.prompt_version ?? null,
        participant_count: resolved.length,
        defining_count: defining.length,
        supporting_count: supporting.length,
        invalid_scope_count: resolved.length - defining.length - supporting.length,
        gold_resolution_count: goldBoundaries.length,
        unresolved_gold_symbols: unresolvedGoldSymbols,
        matched_followup_count: matchedAnchors.size,
        missing_followup_count: expected - matchedAnchors.size,
        defining_followup_count: definingFollowup.length,
        supporting_followup_count: supportingFollowup.length,
        followup_recall: ratio(matchedAnchors.size, expected),
        matched_followup: matchedFollowup,
        non_followup_participants: resolved.filter((participant) =>
          !goldAnchors.has(participant.anchor)),
        missing_followup: missingFollowup,
      };
    } finally {
      database.close();
    }
  });

  const grouped = Map.groupBy(runs, (row) => row.pair_id);
  const byPair = Object.fromEntries(
    [...grouped.entries()].map(([pairId, rows]) => [pairId, aggregate(rows)]),
  );
  return {
    schema_version: 2,
    runs: runs.length,
    annotate_v2_runs: runs.filter((row) => row.prompt_version === "annotate/v2").length,
    runs_with_workflow: runs.filter((row) => row.workflow_count > 0).length,
    runs_with_defining_participant: runs.filter((row) => row.defining_count > 0).length,
    runs_without_supporting_participant: runs.filter((row) => row.supporting_count === 0).length,
    invalid_scope_participants: runs.reduce((sum, row) => sum + row.invalid_scope_count, 0),
    micro: aggregate(runs),
    by_pair: byPair,
    details: runs,
  };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const responseFiles = paths(options.responses);
  const artifactDirectories = paths(options.artifacts);
  const report = buildWorkflowScopeReport({
    taskSet: JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8")),
    responseFiles,
    artifactDirectories,
  });
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
