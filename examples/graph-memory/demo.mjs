/**
 * Minimal knowledge-graph memory for a multi-agent workflow.
 *
 * The worker outputs below stand in for schema-constrained LLM extraction.
 * Keeping them as fixtures makes the example deterministic and runnable
 * without an API key. In production, replace RAW_WORKER_OUTPUTS with the
 * structured result of one extraction call per document.
 */

export const RAW_WORKER_OUTPUTS = [
  {
    worker: "pricing-worker",
    document: "pricing-q3.html",
    entities: [
      {
        name: "acme",
        type: "ORGANIZATION",
        description: "Competitor selling an AI platform.",
      },
      {
        name: "Acme Core",
        type: "PRODUCT",
        description: "Acme's core AI platform plan.",
      },
      {
        name: "$85/month",
        type: "PRICE",
        description: "Published monthly price for Acme Core.",
      },
    ],
    relations: [
      { source: "acme", predicate: "offers", target: "Acme Core" },
      { source: "Acme Core", predicate: "priced at", target: "$85/month" },
    ],
  },
  {
    worker: "product-worker",
    document: "patent-px9.txt",
    entities: [
      {
        name: "ACME Corporation",
        type: "ORGANIZATION",
        description: "Applicant for the PX-9 patent.",
      },
      {
        name: "PX-9 patent",
        type: "DOCUMENT",
        description: "Patent filing for an edge inference system.",
      },
      {
        name: "edge inference engine",
        type: "FEATURE",
        description: "Technology described by the PX-9 patent.",
      },
    ],
    relations: [
      { source: "ACME Corporation", predicate: "filed", target: "PX-9 patent" },
      { source: "PX-9 patent", predicate: "covers", target: "edge inference engine" },
    ],
  },
  {
    worker: "finance-worker",
    document: "q3-filing.txt",
    entities: [
      {
        name: "Acme Corp",
        type: "ORGANIZATION",
        description: "Competitor reporting quarterly financial results.",
      },
      {
        name: "R&D spending",
        type: "METRIC",
        description: "Quarterly research and development expenditure.",
      },
    ],
    relations: [
      { source: "Acme Corp", predicate: "doubled", target: "R&D spending" },
    ],
  },
];

/**
 * This fixture stands in for the playbook's reasoning-heavy resolution step.
 * Every alias appears once, and unmatched names later fall back to themselves.
 */
export const ALIAS_GROUPS = [
  {
    canonical: "Acme Corp",
    aliases: ["acme", "ACME Corporation", "Acme Corp"],
  },
];

export function buildAliasMap(groups) {
  const aliasMap = new Map();

  for (const group of groups) {
    for (const alias of group.aliases) {
      if (aliasMap.has(alias)) {
        throw new Error(`Alias appears in more than one group: ${alias}`);
      }
      aliasMap.set(alias, group.canonical);
    }
  }

  return aliasMap;
}

export class GraphMemory {
  constructor(aliasMap = new Map()) {
    this.aliasMap = aliasMap;
    this.nodes = new Map();
    this.edges = [];
  }

  canonical(name) {
    return this.aliasMap.get(name) ?? name;
  }

  addNode(entity, provenance) {
    const name = this.canonical(entity.name);
    const current = this.nodes.get(name) ?? {
      name,
      type: entity.type,
      descriptions: new Set(),
      sources: new Set(),
    };

    current.descriptions.add(entity.description);
    current.sources.add(provenance.document);
    this.nodes.set(name, current);
  }

  addEdge(relation, provenance) {
    const source = this.canonical(relation.source);
    const target = this.canonical(relation.target);

    if (!this.nodes.has(source) || !this.nodes.has(target)) {
      throw new Error(`Relation endpoint was not extracted: ${source} -> ${target}`);
    }

    this.edges.push({
      source,
      predicate: relation.predicate,
      target,
      provenance: {
        worker: provenance.worker,
        document: provenance.document,
      },
    });
  }

  neighbors(name) {
    const canonical = this.canonical(name);
    const result = new Set();

    for (const edge of this.edges) {
      if (edge.source === canonical) result.add(edge.target);
      if (edge.target === canonical) result.add(edge.source);
    }

    return result;
  }

  subgraph(center, hops = 2) {
    const seed = this.canonical(center);
    if (!this.nodes.has(seed)) throw new Error(`Unknown entity: ${center}`);

    const nodes = new Set([seed]);
    let frontier = new Set([seed]);

    for (let hop = 0; hop < hops; hop += 1) {
      const next = new Set();
      for (const node of frontier) {
        for (const neighbor of this.neighbors(node)) {
          if (!nodes.has(neighbor)) next.add(neighbor);
        }
      }
      for (const node of next) nodes.add(node);
      frontier = next;
    }

    return {
      center: seed,
      hops,
      nodes: [...nodes],
      edges: this.edges.filter(
        (edge) => nodes.has(edge.source) && nodes.has(edge.target),
      ),
    };
  }

  serializeSubgraph(center, hops = 2) {
    return this.subgraph(center, hops).edges
      .map(
        (edge) =>
          `(${edge.source}) --[${edge.predicate}]--> (${edge.target}) ` +
          `[source: ${edge.provenance.document}]`,
      )
      .sort()
      .join("\n");
  }

  checkClaim({ source, predicate, target }) {
    const canonicalSource = this.canonical(source);
    const canonicalTarget = this.canonical(target);
    const exact = this.edges.find(
      (edge) =>
        edge.source === canonicalSource &&
        edge.predicate === predicate &&
        edge.target === canonicalTarget,
    );

    if (exact) {
      return { status: "supported", evidence: [exact] };
    }

    const conflicting = this.edges.filter(
      (edge) =>
        edge.source === canonicalSource && edge.target === canonicalTarget,
    );

    if (conflicting.length > 0) {
      return { status: "contradicted", evidence: conflicting };
    }

    return { status: "unsupported", evidence: [] };
  }

  stats() {
    return {
      nodes: this.nodes.size,
      edges: this.edges.length,
      provenanceCoverage:
        this.edges.filter((edge) => edge.provenance.document).length /
        Math.max(this.edges.length, 1),
    };
  }
}

export function assembleGraph(workerOutputs, aliasGroups = ALIAS_GROUPS) {
  const graph = new GraphMemory(buildAliasMap(aliasGroups));

  for (const output of workerOutputs) {
    for (const entity of output.entities) graph.addNode(entity, output);
  }
  for (const output of workerOutputs) {
    for (const relation of output.relations) graph.addEdge(relation, output);
  }

  return graph;
}

export function synthesizeAcmeAnswer(graph) {
  const evidence = graph.subgraph("Acme Corp", 2).edges;
  const cite = (predicate) => {
    const edge = evidence.find((candidate) => candidate.predicate === predicate);
    return edge
      ? `${edge.source} --[${edge.predicate}]--> ${edge.target} ` +
          `[${edge.provenance.document}]`
      : null;
  };

  return [
    "The graph supports three coordinated signals:",
    `1. ${cite("priced at")}`,
    `2. ${cite("filed")} and ${cite("covers")}`,
    `3. ${cite("doubled")}`,
    "Inference: lower product pricing, a related patent, and higher R&D spending " +
      "are consistent with product expansion. The inference is not stored as a fact.",
  ].join("\n");
}

export function runDemo() {
  const graph = assembleGraph(RAW_WORKER_OUTPUTS);
  const result = {
    stats: graph.stats(),
    subgraph: graph.serializeSubgraph("Acme Corp", 2),
    answer: synthesizeAcmeAnswer(graph),
    checks: {
      supported: graph.checkClaim({
        source: "ACME Corporation",
        predicate: "filed",
        target: "PX-9 patent",
      }),
      contradicted: graph.checkClaim({
        source: "acme",
        predicate: "withdrew",
        target: "PX-9 patent",
      }),
      unsupported: graph.checkClaim({
        source: "Acme Corp",
        predicate: "acquired",
        target: "Northstar Labs",
      }),
    },
  };

  return { graph, result };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { result } = runDemo();
  console.log("GRAPH STATS");
  console.log(JSON.stringify(result.stats, null, 2));
  console.log("\nTWO-HOP SUBGRAPH");
  console.log(result.subgraph);
  console.log("\nGROUNDED SYNTHESIS");
  console.log(result.answer);
  console.log("\nCLAIM CHECKS");
  console.log(
    JSON.stringify(
      Object.fromEntries(
        Object.entries(result.checks).map(([name, check]) => [
          name,
          {
            status: check.status,
            evidence: check.evidence.map(
              (edge) =>
                `${edge.source} --[${edge.predicate}]--> ${edge.target} ` +
                `[${edge.provenance.document}]`,
            ),
          },
        ]),
      ),
      null,
      2,
    ),
  );
}
