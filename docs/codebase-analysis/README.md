# jscout codebase analysis

A structural analysis of the jscout codebase as of **2026-08-17**, commit `a102597` on `main`. It describes what the code does and how. The source is the authority throughout: where these documents and `PLAN.md` disagree, they describe the code as it stands, not the plan.

Scope: 57,769 lines of Rust across 48 files, two Node sidecars, one Python inference service, and a Node evaluation harness.

## Reading order

Start with the overview. After that the documents are independent — read whichever subsystem you need.

| # | Document | What it covers |
|---|---|---|
| 01 | [System overview](01-system-overview.md) | The whole system at a glance: processes, the three planes, type erasure, the confidence lattice |
| 02 | [Ingestion](02-ingestion.md) | File discovery, oxc parsing, chunking, workspace and dependency resolution, the index drive loop |
| 03 | [Structural extraction](03-structural-extraction.md) | AST visitors, the entity and symbol model, graph node and edge kinds, projection |
| 04 | [Call graph and surface](04-call-graph-and-surface.md) | Call-site detection, what resolution is possible without types, the agent read surface |
| 05 | [Storage schema](05-storage-schema.md) | Every SQLite table, indexes, FTS5, sqlite-vec, migration policy |
| 06 | [Semantic layer](06-semantic-layer.md) | What gets embedded, the content-addressed cache, providers, the Python inference contract |
| 07 | [Retrieval](07-retrieval.md) | BM25 plus vector fusion, reranking, role policy, graph expansion, byte budgets |
| 08 | [Scouting](08-scouting.md) | LLM-generated cards, concepts, summaries, workflows, and repository classification |
| 09 | [Sidecars](09-sidecars.md) | The TypeScript checker and LLM gateway wire protocols, message by message |
| 10 | [CLI and MCP](10-cli-and-mcp.md) | Complete command inventory and complete MCP tool inventory |
| 11 | [Incremental and watch](11-incremental-and-watch.md) | The generation coordinator, cancellation semantics, debouncing, degraded states |
| 12 | [Evaluation](12-evaluation.md) | Pre-registration, arms, adjudication, contamination probes, PR replay |
| 13 | [Build, config, CI](13-build-config-ci.md) | Dependencies, environment variables, CI gates, test inventory, release packaging |
| 14 | [Cross-cutting concerns](14-cross-cutting.md) | Telemetry, the threading model, signal handling, error conventions, configuration precedence |
| 15 | [End-to-end traces](15-end-to-end-traces.md) | Four annotated traces: cold index, search, MCP call, incremental update |
| 16 | [History and roadmap](16-history-and-roadmap.md) | How it was built, the gate model, what is planned and what is not |
| 17 | [Sharp edges](17-sharp-edges.md) | Complexity hotspots, fragile invariants, approximations, testing gaps |

The roadmap document [16](16-history-and-roadmap.md) carries a dated amendment for 2026-08-18: G15 was measured and parked, and G17 and G18 replaced it as the next milestones. Everything else describes `a102597` as stated.

## Conventions used in these documents

- Source citations are repo-relative paths with a line number, `src/file.rs:123`. Line numbers are accurate as of commit `a102597` and will drift.
- "Snapshot" has two senses, both from the code: the disposable structural plane rebuilt on every index run, and the blake3 digest of that plane stored in `meta.snapshot` and used as a version stamp. [12](12-evaluation.md) adds a third, unrelated sense — a frozen repository export used as an evaluation workspace — and always qualifies it.
- "Artifact" means a durable LLM-generated semantic object: a card, workflow, summary, or concept. Where [12](12-evaluation.md) and [13](13-build-config-ci.md) mean a build output or a retained evaluation file, they say so.
- "Plane" also has two senses from the code: a storage lifecycle (disposable snapshot, durable cache, durable semantic memory, checker), and the `runtime` / `contract` / `general` partition stored in `entity_sites.plane`. Neither means a physical database.
- "Entity" is a canonical cross-file identity grouped from per-file `entity_sites` — a route, job, DI token, config key. "Chunk" is a symbol-aligned span of one source file, the unit both FTS5 and the embedding cache are keyed on.
- Diagrams are mermaid. They show verified relationships from the code, not idealized architecture.
- Confidence values (`certain`, `likely`, `possible`) are the system's own vocabulary. They mean the graph edge confidence lattice described in [01](01-system-overview.md) unless attached to an entity site ([03](03-structural-extraction.md)) or a semantic artifact ([08](08-scouting.md)), which carry their own confidence on the same three-value scale.

## Related documents in this repository

- `PLAN.md` — the normative architecture and roadmap document, organized by numbered gates
- `README.md` — user-facing command documentation
- [`docs/affine-architecture-diagrams-2026-08-14.md`](../affine-architecture-diagrams-2026-08-14.md) — earlier architecture diagrams from the AFFiNE experiment
- [`docs/next-pr-replay-evaluation-plan-2026-08-15.md`](../next-pr-replay-evaluation-plan-2026-08-15.md) — the evaluation campaign design
