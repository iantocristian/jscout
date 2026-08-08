# jscout core plan — corrected baseline and forward contract

> Independent Codex revision, 2026-08-07. This file does not replace
> [PLAN.md](PLAN.md); it records the implementation that actually exists and
> defines the boundary that the next architecture should build on.

## What jscout is

`jscout` is persistent, verifiable repository memory for coding agents. It
stores deterministic repository facts, refreshable semantic claims, and
agent-reported findings so later sessions do not have to rediscover them. It
complements rather than replaces the agent's reasoning: its job is to make
repository evidence cheap to retrieve, compact to consume, and honest about
provenance, confidence, and freshness.

The serving contract is:

> Given an agent's current query or focus, return the smallest trustworthy
> slice of repository evidence that materially improves its next action.

The first implementation establishes a fast runtime-oriented JS/TS index. The
next architecture adds graph traversal, scouting representations, deterministic
entities, and refreshable semantic memory; see
[PLAN-KG-REVISED-CODEX.md](PLAN-KG-REVISED-CODEX.md).

## Current implementation status

The original M0–M6 sequence was useful as a build plan, but it is not an
accurate status report anymore. The implementation is best described by
capability rather than by declaring every original milestone complete.

### Implemented

| Area | Current implementation |
|---|---|
| Repository walk | JS/JSX/TS/TSX/MJS/CJS/MTS/CTS discovery via `ignore`; common build/output directories and declaration files skipped |
| Parsing | `oxc_parser` + `oxc_semantic`; runtime bindings and references extracted without the TypeScript checker |
| Chunking | AST-aware function/class/method/component/module chunks with source spans, scope, JSDoc inclusion, file imports, and BLAKE3 content hashes |
| Runtime graph | Files, declaration-scoped symbols, imports, exports, re-exports, resolved module edges, local/imported references, JSX renders, calls, construction, inheritance |
| Heuristics | CommonJS `require`/exports, literal dynamic imports, string-keyed event sites, and name-matched member calls |
| Storage | One embedded SQLite database containing canonical extraction tables, FTS5, event/member-call sites, embedding blobs, and a disposable materialized traversal projection |
| Retrieval | FTS5 BM25; optional embedding search; reciprocal-rank fusion; optional HTTP reranker; compact call/usage hints and snapshot-scoped graph anchors on hits; opt-in separately labelled structural expansion under global budgets |
| Incremental indexing | File and chunk content hashes; unchanged files skipped; deleted files removed; module resolution rebuilt; optional filesystem watcher |
| Agent interface | CLI plus MCP tools: `semantic_search`, `neighborhood`, `who_uses`, `definition`, `file_outline`, and `events`; opt-in privacy-minimal MCP call telemetry |
| Snapshot safety | BLAKE3 repository snapshots; returned graph anchors are snapshot-scoped; stale symbol anchors re-resolve by path/scope/name or fail with candidates |
| Evaluation artifacts | Chunk/structural/search unit tests, local retrieval benchmarks, and a paired baseline/structural agent-task protocol with telemetry-aware grading |

### Partial or deferred

| Original promise | Actual state / destination |
|---|---|
| Golden-file and integration tests across several OSS repositories | Partial; synthetic structural fixtures exist, but multi-repository integration coverage does not |
| Git-aware Merkle state via `gix` | Not implemented; content hashing provides current incremental behavior |
| Patch graph edges for changed files and their dependency closure | References are replaced per changed file and module edges are rebuilt globally; no dependency-closure invalidator |
| Route-table and DI-token heuristics | Not implemented; routes move to deterministic entity extraction; DI tokens remain unscheduled |
| Constant-prefix dynamic imports | Not implemented; only literal strings and expression-free templates are indexed |
| Expanded search acceptance | Search-to-anchor projection, ranked opt-in expansion, whole-response budgets, deterministic file roles, and pre-budget role filtering/penalties are implemented; the pre-registered n8n/Twenty re-run reduced structural irrelevant inspection from +6.38 to +1.08 with an interval including zero, while correctness/cost still showed no win over grep, so expansion remains opt-in and L1 investment closes |
| Repo map, paths, graph export | Not implemented; later structural-retrieval phase |
| Real-provider embedding verification | Provider plumbing and mock-compatible verification exist; repeatable real-provider acceptance remains environment-dependent |
| Runtime trace tier | Not implemented and not on the near-term critical path |

## Decisions that now match the code

| Decision | Current choice | Consequence |
|---|---|---|
| Language | Rust | One local binary, low indexing and query latency |
| Parser | OXC syntax + semantic analysis | Fast binding graph without checker-level type resolution |
| Module resolution | `oxc_resolver` | Node/TS path and package resolution without invoking `tsc` |
| Primary retrieval unit | AST-derived chunk | Search results align with useful code units, but chunk identity is not ontology identity |
| Runtime graph unit | File and symbol | Structural relationships remain independent of retrieval chunk boundaries |
| Storage | SQLite only | Simple deployment and transactional derived-state refresh |
| Sparse search | SQLite FTS5 | No Tantivy sidecar |
| Dense search | Optional API-compatible embeddings stored as blobs | Search remains functional without an embedding provider |
| Incremental policy | Content hashes + full module-edge rebuild | Simple and correct at current repository scale |
| Confidence | `certain`, `likely`, `possible` | Unresolved relationships remain candidates instead of becoming silent false certainty |
| Interfaces | CLI and MCP | Humans can inspect behavior; agents receive structured tools |

## Two semantic planes

The original phrase “TypeScript is for humans” should lead to two different
representations rather than universal type erasure:

1. **Runtime plane:** functions, classes, values, calls, renders, imports,
   side effects. Type-space does not create runtime edges.
2. **Contract plane:** exported interfaces/types, parameter and return types,
   schemas, enums, decorators, validation contracts, and documentation. These
   are high-value semantic evidence for agents even when they do not exist at
   runtime.

The current index implements the runtime plane. The revised scouting plan adds
the contract plane without pretending that checker-less type references are
runtime-resolved facts.

## Core invariants going forward

- Raw source remains the source of truth; compressed, scout-generated, and
  agent-authored views are derived artifacts.
- Existing typed tables remain canonical. A generic graph projection may
  accelerate traversal but does not replace source-specific provenance.
- Chunks remain retrieval artifacts. Search hits project to graph anchors;
  chunk boundaries do not define repository ontology.
- Every inferred edge or semantic claim carries provenance, confidence, and a
  freshness state.
- Deterministic derived data is rebuilt automatically with indexing.
- Semantic data may be stale, but it must never look fresh when its evidence
  changed.
- Output budgets are global per response, measured in rendered tokens/bytes,
  and deduplicated across seeds.
- Agent tools expose evidence and expansion handles; they do not conceal source
  locations behind prose-only answers.

## Stabilization work before or alongside the next phase

1. Add schema-versioned migrations for every new derived table.
2. Add fixture repositories covering ESM, CommonJS, barrels, same-named
   methods, events, member calls, JSX, and file deletion/re-indexing.
3. Add end-to-end tests for `index`, `search`, `who-uses`, and MCP tool schemas.
4. Record benchmark corpus fingerprints with performance results so measurements
   can be reproduced against the same repository state.
5. Keep the CLI and MCP result schemas versioned as graph/scouting fields land.

These are not a separate “quality milestone.” Each new phase owns the fixtures,
latency gates, and retrieval checks necessary to trust it.

## Historical note

[PLAN.md](PLAN.md) should be read as the original build proposal. It contains
decisions that were superseded by implementation (Tantivy/LanceDB, `gix`, and
the original M5/M6 completion criteria). This revision is the operative core
baseline; the separate revised graph/scouting plan is the forward roadmap.
