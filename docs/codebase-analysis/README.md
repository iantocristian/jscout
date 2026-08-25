# jscout codebase analysis

A structural analysis of the jscout codebase as of **2026-08-21**, commit `854bff1` on branch `codex/performance-budget-freshness-sync`. It describes what the code does and how. Where these documents and `PLAN.md` disagree, they describe the code as it stands.

Scope: 78 Rust files and 70,445 lines (409 tests), two Node sidecars, one Python inference service, an npm distribution, and a Node evaluation harness. Crate version 0.4.0.

> **Replaces `docs/codebase-analysis/`.** The codebase was restructured in the intervening four days — `src/main.rs` went from 2,403 lines to 79, tests moved into sibling files, and three new subsystems landed. If you read the earlier set, start at [20](20-delta-since-2026-08-17.md), which lists per-document exactly what is now wrong.

## Reading order

Start with the overview. After that the documents are independent.

| # | Document | What it covers |
|---|---|---|
| 01 | [System overview](01-system-overview.md) | Processes, the three planes, type erasure, the confidence lattice, the tiered query path |
| 02 | [Ingestion](02-ingestion.md) | Discovery, oxc parsing, chunking, workspace and dependency resolution |
| 03 | [Structural extraction](03-structural-extraction.md) | Visitors, the entity and symbol model, node and edge kinds, projection |
| 04 | [Call graph and surface](04-call-graph-and-surface.md) | Call-site matching, export-chain resolution, the agent read surface |
| 05 | [Storage schema](05-storage-schema.md) | Every table, FTS5, sqlite-vec, the durable floor |
| 06 | [Semantic layer](06-semantic-layer.md) | Embedding inputs, the content-addressed cache, providers, the inference contract |
| 07 | [Retrieval](07-retrieval.md) | The exact-identifier tier, fusion, reranking, expansion, byte budgets |
| 08 | [Scouting](08-scouting.md) | Cards, workflows, summaries, concepts, repository classification |
| 09 | [Sidecars](09-sidecars.md) | The checker and gateway wire protocols, message by message |
| 10 | [CLI and commands](10-cli-and-commands.md) | The clap surface, dispatch, and the complete command inventory |
| 11 | [MCP surface](11-mcp-surface.md) | Transport, the complete tool inventory, profile gating, budgeting |
| 12 | [Configuration](12-configuration.md) | `.jscout.toml`, the precedence chain, and the I/O seam |
| 13 | [Incremental and watch](13-incremental-and-watch.md) | Generations, the real incremental path, cancellation, checker retention |
| 14 | [Evaluation](14-evaluation.md) | Pre-registration, contamination probes, arms, adjudication |
| 15 | [Build, CI, distribution](15-build-config-ci.md) | Dependencies, clippy policy, CI, the npm channel, test strategy |
| 16 | [Cross-cutting concerns](16-cross-cutting.md) | Telemetry, threading, signals, error conventions, conventions |
| 17 | [End-to-end traces](17-end-to-end-traces.md) | Cold index, identifier search, MCP call, incremental update |
| 18 | [History and roadmap](18-history-and-roadmap.md) | How it was built, the gate model, what is planned |
| 19 | [Sharp edges](19-sharp-edges.md) | Complexity hotspots, fragile invariants, testing gaps |
| 20 | [Delta since 2026-08-17](20-delta-since-2026-08-17.md) | What changed, and what the previous analysis got wrong |

## Conventions

- Source citations are repo-relative with a line number, `src/file.rs:123`, valid at `854bff1`. They will drift.
- Most modules are now `foo.rs` plus a sibling `foo/tests.rs`. A citation into a module's test coverage points at the sibling.
- "Snapshot" means the disposable structural plane, and also the blake3 digest of it stored in `meta.snapshot`. [14](14-evaluation.md) uses a third, unrelated sense — a frozen repository export used as an evaluation workspace — and qualifies it.
- "Artifact" means a durable LLM-generated semantic object: a card, workflow, summary, or concept. [14](14-evaluation.md) and [15](15-build-config-ci.md) use it for build outputs and say so.
- "Plane" means a storage lifecycle (disposable, durable cache, semantic memory, checker), and separately the `runtime` / `contract` / `general` partition in `entity_sites.plane`. Neither is a physical database.
- "Tier" refers to the retrieval intent tiers in [07](07-retrieval.md) — the exact lane versus the hybrid lane — not to confidence.
- Confidence values (`certain`, `likely`, `possible`) mean the edge lattice in [01](01-system-overview.md) unless attached to an entity site or a semantic artifact, which carry their own on the same scale.
- Diagrams are mermaid, showing verified relationships from the code.

## Related documents

- `PLAN.md` — the normative architecture and roadmap document, organized by numbered gates
- `README.md` — user-facing command documentation
- [`docs/publication-2026-08-17/`](../publication-2026-08-17/README.md) — the research write-ups and evaluation results
- `docs/codebase-analysis/` — the superseded analysis, kept as a dated snapshot
