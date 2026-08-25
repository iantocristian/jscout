# jscout codebase analysis

A structural analysis of the jscout codebase on `main`, current as of the repository-walk refactors `b92485c` and `03d5b50`. It describes what the code does and how, from reading the source. Where these documents and `PLAN.md` disagree, they describe the code as it stands — and in one case ([20](20-history-and-roadmap.md)) they say so explicitly, because a `PLAN.md` gate heading is stale.

Scope: 88 Rust files and 91,282 lines (570 tests), a documentation corpus subsystem, two Node sidecars, one Python inference service, an npm distribution, and a Node evaluation harness. Crate 0.4.0, schema v31, 47 tables.

> **Replaces `docs/codebase-analysis/`.** Twenty-seven commits and +12,161 lines landed in a day: an entire `src/docs/` subsystem for unified Markdown retrieval, scout model concurrency, and a schema jump from v29 to v31. If you read the earlier set, start at [22](22-delta-since-2026-08-24.md).

## Reading order

Start with the overview. After that the documents are independent.

| # | Document | What it covers |
|---|---|---|
| 01 | [System overview](01-system-overview.md) | Two corpora, one traversal, where the planes couple, retrieval, gate status |
| 02 | [Code ingestion](02-ingestion.md) | Parsing, chunking, roles, workspace and dependency resolution |
| 03 | [Documentation corpus](03-documentation-corpus.md) | The shared traversal, Markdown admission, prose chunking, embedding identity |
| 04 | [Documentation retrieval](04-documentation-retrieval.md) | The `doc_*` tables, `docs_fts`, doc vectors, ranking |
| 05 | [Indexing pipeline](05-indexing-pipeline.md) | One run, two planes; entry points, transactions, publication |
| 06 | [Structural extraction](06-structural-extraction.md) | Visitors, entity and edge kinds, projection, value flow |
| 07 | [Call graph and surface](07-call-graph-and-surface.md) | Call-site matching, export chains, who_uses tiers, the read surface |
| 08 | [Storage schema](08-storage-schema.md) | All 47 tables, two FTS5 indexes, two vec0 families |
| 09 | [Semantic layer](09-semantic-layer.md) | Embedding inputs, the cache, providers, the inference contract |
| 10 | [Code retrieval](10-code-retrieval.md) | Exhaustive mode, the exact-identifier tier, hybrid ranking |
| 11 | [Scouting](11-scouting.md) | Artifacts, the candidate-closed design, wave concurrency |
| 12 | [Sidecars](12-sidecars.md) | The checker protocol and package gate; the concurrent LLM client |
| 13 | [CLI and commands](13-cli-and-commands.md) | Dispatch, the complete command inventory, the agent guide |
| 14 | [MCP surface](14-mcp-surface.md) | Transport, the tool inventory, gating, budgeting |
| 15 | [Configuration](15-configuration.md) | `.jscout.toml`, the precedence chain, the I/O seam |
| 16 | [Incremental and watch](16-incremental-and-watch.md) | Generations, incremental refresh, and the documentation gap |
| 17 | [Build, CI, distribution](17-build-ci-distribution.md) | Dependencies, clippy policy, CI, npm, test strategy |
| 18 | [Cross-cutting concerns](18-cross-cutting.md) | Telemetry, threading, signals, error conventions |
| 19 | [End-to-end traces](19-end-to-end-traces.md) | Cold index, doc query, code query, scouting wave |
| 20 | [History and roadmap](20-history-and-roadmap.md) | The gate model and its actual status |
| 21 | [Sharp edges](21-sharp-edges.md) | Complexity hotspots, fragile invariants, testing gaps |
| 22 | [Delta since 2026-08-24](22-delta-since-2026-08-24.md) | What changed, and what the previous set got wrong |

## Conventions

- Citations are repo-relative with a line number, `src/file.rs:123`, valid at the tip of `main` when this version was written. They drift fast — this codebase has moved 12,000 lines in a day.
- Earlier versions of this document set live at this same path in git history; `git log --follow -- docs/codebase-analysis/README.md` lists them.
- Most modules are `foo.rs` plus a sibling `foo/tests.rs`. A citation into test coverage points at the sibling.
- **"Corpus"** means `code` or `documentation`, the partition stored in `files.corpus`. `code_files` and `code_chunks` are views over `files` and `chunks` filtered to the code corpus, not tables.
- **"Mode"** means ranked or exhaustive code retrieval. **"Tier"** means the exact or hybrid lane inside ranked mode. Neither means confidence.
- "Snapshot" means the disposable structural plane and also its digest in `meta.snapshot`. Note that the digest covers documentation rows too — see [01](01-system-overview.md).
- "Artifact" means a durable LLM-generated semantic object. Where [17](17-build-ci-distribution.md) means a build output, it says so.
- "Plane" means a storage lifecycle, and separately the `runtime` / `contract` / `general` partition in `entity_sites.plane`.
- Confidence values (`certain`, `likely`, `possible`) mean the edge lattice in [01](01-system-overview.md) unless attached to an entity site or semantic artifact.
- `src/stats.rs` is not storage code — it is an oxc AST visitor backing `jscout stats`.
- Diagrams are mermaid, showing verified relationships from the code.

## Related documents

- `PLAN.md` — the normative roadmap, organized by numbered gates. [20](20-history-and-roadmap.md) records where its headings lag the code.
- `README.md` — user-facing command documentation
- [`docs/publication-2026-08-17/`](../publication-2026-08-17/README.md) — research write-ups and evaluation results
- Superseded snapshots: `2026-08-24`, `2026-08-22`, `2026-08-17`
