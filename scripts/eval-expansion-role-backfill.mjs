#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { DatabaseSync } from "node:sqlite";
import { pathToFileURL } from "node:url";

function parseArgs(argv) {
  const options = { "profile-prefix": "structural-" };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  if (!options.artifacts || !options.repository) {
    throw new Error("--artifacts and --repository are required");
  }
  return options;
}

function expansionNodes(event) {
  const item = event?.item;
  if (
    event?.type !== "item.completed" ||
    item?.type !== "mcp_tool_call" ||
    item?.server !== "jscout" ||
    item?.tool !== "semantic_search" ||
    item?.arguments?.expand !== true
  ) {
    return null;
  }
  for (const content of item?.result?.content ?? []) {
    if (content?.type !== "text" || typeof content.text !== "string") continue;
    try {
      const result = JSON.parse(content.text);
      return Array.isArray(result?.expansion?.nodes) ? result.expansion.nodes : [];
    } catch {
      return [];
    }
  }
  return [];
}

export function backfillExpansionRoles({ artifactDirectories, repository, profilePrefix }) {
  const database = new DatabaseSync(path.join(repository, ".jscout.db"), { readOnly: true });
  const roleForPath = database.prepare("SELECT role FROM files WHERE path = ?");
  const roleCounts = {};
  const unclassifiedPaths = new Set();
  let artifactFiles = 0;
  let expandedSearchCalls = 0;
  let expansionNodesTotal = 0;
  let expansionFileNodes = 0;

  try {
    for (const directory of artifactDirectories) {
      const files = fs
        .readdirSync(directory)
        .filter((file) => file.startsWith(profilePrefix) && file.endsWith(".jsonl"))
        .sort();
      for (const file of files) {
        artifactFiles += 1;
        const lines = fs.readFileSync(path.join(directory, file), "utf8").split(/\r?\n/);
        for (const line of lines) {
          if (!line.trim()) continue;
          let event;
          try {
            event = JSON.parse(line);
          } catch {
            continue;
          }
          const nodes = expansionNodes(event);
          if (nodes === null) continue;
          expandedSearchCalls += 1;
          expansionNodesTotal += nodes.length;
          for (const node of nodes) {
            if (typeof node?.file !== "string") continue;
            const row = roleForPath.get(node.file);
            if (!row?.role) {
              unclassifiedPaths.add(node.file);
              continue;
            }
            expansionFileNodes += 1;
            roleCounts[row.role] = (roleCounts[row.role] ?? 0) + 1;
          }
        }
      }
    }
  } finally {
    database.close();
  }

  const testFixtureGenerated = ["test", "fixture", "generated"].reduce(
    (sum, role) => sum + (roleCounts[role] ?? 0),
    0,
  );
  return {
    schema_version: 1,
    artifact_files: artifactFiles,
    expanded_search_calls: expandedSearchCalls,
    expansion_nodes: expansionNodesTotal,
    expansion_file_nodes: expansionFileNodes,
    expansion_role_counts: roleCounts,
    expansion_test_fixture_generated_nodes: testFixtureGenerated,
    expansion_test_fixture_generated_share:
      expansionFileNodes === 0 ? null : testFixtureGenerated / expansionFileNodes,
    unclassified_paths: [...unclassifiedPaths].sort(),
  };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const result = backfillExpansionRoles({
    artifactDirectories: options.artifacts.split(",").map((value) => path.resolve(value)),
    repository: path.resolve(options.repository),
    profilePrefix: options["profile-prefix"],
  });
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
