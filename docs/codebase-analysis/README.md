# jscout codebase analysis

A structural analysis of the jscout codebase as of **2026-08-24**, commit `4de5622`. It describes what the code does and how, from reading the source. Where these documents and `PLAN.md` disagree, they describe the code as it stands.

Scope: 82 Rust files and 79,963 lines (474 tests), two Node sidecars, one Python inference service, an npm distribution, and a Node evaluation harness. Crate 0.4.0, schema v29.

> **Replaces `docs/codebase-analysis/`.** Roughly 48 commits and +9,925 lines landed in three days: G22 exhaustive lexical search, a bounded value-flow analysis, and a checker package-admission gate. If you read the earlier set, start at [20](20-delta-since-2026-08-22.md).

## Reading order

Start with the overview. After that the documents are independent. The evaluation harness (`eval/`, `scripts/eval-*.mjs`) is byte-identical to its state at `854bff1` and is not re-covered here; the standing account is `14-evaluation.md` in the previous set.

| # | Document | What it covers |
|---|---|---|
| 01 | [System overview](01-system-overview.md) | Processes, the two planes, dispatch resolution, the two retrieval modes |
| 02 | [Ingestion](02-ingestion.md) | Discovery, oxc parsing, chunking, workspace and dependency resolution |
| 03 | [Structural extraction](03-structural-extraction.md) | Visitors, the entity and symbol model, node and edge kinds, projection |
| 04 | [Value flow](04-value-flow.md) | Bounded receiver resolution without a type checker |
| 05 | [Call graph and surface](05-call-graph-and-surface.md) | Call-site matching, export chains, who_uses tiers, the read surface |
| 06 | [Storage schema](06-storage-schema.md) | All 43 tables, FTS5, sqlite-vec, the durable floor |
| 07 | [Semantic layer](07-semantic-layer.md) | Embedding inputs, the content-addressed cache, providers, inference |
| 08 | [Retrieval](08-retrieval.md) | Exhaustive mode, the exact-identifier tier, hybrid ranking, budgets |
| 09 | [Scouting](09-scouting.md) | Cards, workflows, summaries, concepts, repository classification |
| 10 | [Sidecars](10-sidecars.md) | The checker protocol and package gate; the LLM gateway |
| 11 | [CLI and commands](11-cli-and-commands.md) | The clap surface, dispatch, the complete command inventory |
| 12 | [MCP surface](12-mcp-surface.md) | Transport, the twelve tools, profile gating, budgeting |
| 13 | [Configuration](13-configuration.md) | `.jscout.toml`, the precedence chain, and the I/O seam |
| 14 | [Incremental and watch](14-incremental-and-watch.md) | Generations, incremental refresh, cancellation, checker retention |
| 15 | [Build, CI, distribution](15-build-ci-distribution.md) | Dependencies, clippy policy, CI, the npm channel, test strategy |
| 16 | [Cross-cutting concerns](16-cross-cutting.md) | Telemetry, threading, signals, error conventions |
| 17 | [End-to-end traces](17-end-to-end-traces.md) | Cold index, exhaustive query, identifier search, incremental update |
| 18 | [History and roadmap](18-history-and-roadmap.md) | How it was built, the gate model, what is planned |
| 19 | [Sharp edges](19-sharp-edges.md) | Complexity hotspots, fragile invariants, testing gaps |
| 20 | [Delta since 2026-08-22](20-delta-since-2026-08-22.md) | What changed, and what the previous set got wrong |

## Conventions

- Citations are repo-relative with a line number, `src/file.rs:123`, valid at `4de5622`. They drift quickly; this codebase moved 9,900 lines in three days.
- Most modules are `foo.rs` plus a sibling `foo/tests.rs`. A citation into test coverage points at the sibling.
- "Mode" means the retrieval mode in [08](08-retrieval.md) — ranked or exhaustive. "Tier" means an intent tier within ranked mode — exact or hybrid. Neither means confidence. [09](09-scouting.md) uses "boundary tier" for a scouting seed rank, following the code's own word; configuration precedence is described in [13](13-configuration.md) as levels, not tiers.
- "Snapshot" means the disposable structural plane, and also its blake3 digest in `meta.snapshot`. Frozen repository checkouts used by the evaluation harness are a third, unrelated sense and are always qualified.
- "Artifact" means a durable LLM-generated semantic object. Where [15](15-build-ci-distribution.md) means a build output, it says so.
- "Plane" means a storage lifecycle, and separately the `runtime` / `contract` / `general` partition in `entity_sites.plane`.
- Confidence values (`certain`, `likely`, `possible`) mean the edge lattice in [01](01-system-overview.md) unless attached to an entity site or semantic artifact, which use the same three-value scale for their own purposes.
- `src/stats.rs` is not storage code despite its name — it is an oxc AST visitor backing `jscout stats`.
- Diagrams are mermaid, showing verified relationships from the code.

## Related documents

- `PLAN.md` — the normative architecture and roadmap, organized by numbered gates
- `README.md` — user-facing command documentation
- [`docs/publication-2026-08-17/`](../publication-2026-08-17/README.md) — research write-ups and evaluation results
- `docs/codebase-analysis/` and `docs/codebase-analysis/` — superseded, kept as dated snapshots
