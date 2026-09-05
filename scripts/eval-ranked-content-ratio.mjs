#!/usr/bin/env node

// Measure compact ranked semantic_search JSON, not debug JSON or the MCP envelope.
// Usage: node scripts/eval-ranked-content-ratio.mjs [response.json ...]
// With no files, read one response from stdin. Empty results are not a pass.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const jsonBytes = (value) => Buffer.byteLength(JSON.stringify(value), "utf8");
const ratio = (content, total) => total === 0 ? null : content / total;

export function measureRankedResponse(response) {
  if (response?.default_match !== "hybrid" || !Array.isArray(response.hits)
      || Object.hasOwn(response, "effective")) {
    throw new Error("expected compact ranked semantic_search JSON");
  }
  const hits = response.hits.map((hit, index) => {
    if (typeof hit?.snippet !== "string") {
      throw new Error(`hit ${index + 1} has no string snippet`);
    }
    // Count escaped source bytes only; quotes and the field name are metadata.
    const snippetBytes = jsonBytes(hit.snippet) - 2;
    const hitBytes = jsonBytes(hit);
    return {
      rank: index + 1,
      at: hit.at ?? null,
      snippet_bytes: snippetBytes,
      hit_bytes: hitBytes,
      content_ratio: ratio(snippetBytes, hitBytes),
      majority_content: snippetBytes * 2 > hitBytes,
      snippet_truncated: hit.snippet_truncated === true,
    };
  });
  const snippetBytes = hits.reduce((sum, hit) => sum + hit.snippet_bytes, 0);
  const hitBytes = hits.reduce((sum, hit) => sum + hit.hit_bytes, 0);
  return {
    hit_count: hits.length,
    snippet_bytes: snippetBytes,
    hit_bytes: hitBytes,
    content_ratio: ratio(snippetBytes, hitBytes),
    majority_content_hits: hits.filter((hit) => hit.majority_content).length,
    hits,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const files = process.argv.slice(2);
  const reports = (files.length ? files : [null]).map((file) => ({
    file,
    ...measureRankedResponse(JSON.parse(readFileSync(file ?? 0, "utf8"))),
  }));
  process.stdout.write(`${JSON.stringify(reports, null, 2)}\n`);
}
