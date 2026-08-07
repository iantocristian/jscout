import assert from "node:assert/strict";
import test from "node:test";

import {
  RAW_WORKER_OUTPUTS,
  assembleGraph,
  runDemo,
} from "./demo.mjs";

test("entity resolution merges organization aliases", () => {
  const graph = assembleGraph(RAW_WORKER_OUTPUTS);

  assert.equal(graph.canonical("acme"), "Acme Corp");
  assert.equal(graph.canonical("ACME Corporation"), "Acme Corp");
  assert.equal(graph.nodes.has("acme"), false);
  assert.equal(graph.nodes.has("Acme Corp"), true);
});

test("two-hop traversal reaches facts from different workers", () => {
  const graph = assembleGraph(RAW_WORKER_OUTPUTS);
  const subgraph = graph.subgraph("Acme Corp", 2);

  assert.ok(subgraph.nodes.includes("$85/month"));
  assert.ok(subgraph.nodes.includes("edge inference engine"));
  assert.ok(subgraph.nodes.includes("R&D spending"));
});

test("every edge carries document and worker provenance", () => {
  const graph = assembleGraph(RAW_WORKER_OUTPUTS);

  for (const edge of graph.edges) {
    assert.ok(edge.provenance.document);
    assert.ok(edge.provenance.worker);
  }
});

test("claim checks distinguish supported, contradicted, and unsupported", () => {
  const { result } = runDemo();

  assert.equal(result.checks.supported.status, "supported");
  assert.equal(result.checks.contradicted.status, "contradicted");
  assert.equal(result.checks.unsupported.status, "unsupported");
});
