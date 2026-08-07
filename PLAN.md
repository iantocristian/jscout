# jscout — a runtime-level JS/TS codebase indexer

> **Status (2026-08-06, retrospective): M0–M4 fully implemented; M5–M6
> implemented with deferred scope.** Verified on bvb (86 files), raggazzi (235),
> ai-pipe (690).
>
> - **M5 shipped**: CommonJS require/module.exports, dynamic-import *literals*,
>   emit/on event sites, member-call candidates, class methods as query targets.
>   **M5 deferred**: route tables (→ PLAN-KG KG-4), DI tokens and
>   dynamic-import constant prefixes (→ parked, no current query needs them).
> - **M6 shipped**: MCP stdio server with semantic_search, who_uses, definition,
>   file_outline, events. **M6 deferred**: `neighborhood` (→ PLAN-KG KG-1).
> - Deviations: single-storage SQLite (FTS5 + blob vectors) instead of
>   tantivy/LanceDB; no gix — content hashing subsumes git integration for
>   incremental correctness.
> - Measured: ai-pipe full index ~220ms, single-file re-index ~28ms, watch-mode
>   re-index ~11ms. Embedding path verified against a mock OpenAI-compatible
>   server only; reranking + asymmetric query prefixes added post-M6 (see
>   README).
>
> **Next: [PLAN-KG.md](PLAN-KG.md) (revision 2)** — jscout reframed as
> persistent agent memory: T1 deterministic facts (KG-1 identity/resolution/
> traversal + KG-4 entities), T2 on-demand renderers (skeletons/map/paths),
> T3 fingerprinted semantic memory (workflows + agent write-back).

A fast indexer for JavaScript/TypeScript codebases built for RAG and agent retrieval.
Core philosophy: **TypeScript is for humans** — parse TS syntax, but index the *runtime
value graph* (functions, classes, components, calls, modules), erasing type-level
constructs the same way esbuild/swc do.

## Decisions made

| Decision | Choice | Why |
|---|---|---|
| Language | Rust | Speed is the product; ecosystem has everything needed |
| Parser | oxc (`oxc_parser` + `oxc_semantic`) | Fastest JS/TS parser; symbol tables + scope trees included; TS parsed then type-nodes skipped |
| Module resolution | `oxc_resolver` | Full Node resolution (package.json `exports`, tsconfig `paths`, workspaces) without the TS checker; battle-tested (Knip uses it) |
| Chunking | cAST-style split-then-merge on the AST | Chunks align with syntactic units (function/class/component); ~500–1500 token budget |
| Reference graph | Tiered edges with confidence | 1: binding-resolved (certain) · 2: heuristic string-literal (likely) · 3: name-match (possible) · 4: optional runtime traces |
| Retrieval | Hybrid: BM25 (tantivy) + code embeddings, RRF fusion, graph expansion of top hits | Dense misses exact identifiers; sparse misses semantic queries |
| Embeddings | Pluggable: Voyage/Codestral API or local ONNX (Qwen3-embedding via `ort`) | Cost/latency/privacy trade-off stays user-side |
| Incremental | blake3 content hashes per file+chunk, `gix` for git state, `notify` watcher | Cursor-style Merkle diff; re-embed only changed chunks |
| Storage | SQLite (rusqlite) for graph + metadata; tantivy dir for text; LanceDB or sqlite-vec for vectors | Embedded, no server, single tool |
| Interface | CLI first, then MCP server | Drops into any editor/agent |

## What we deliberately give up (and the mitigation)

Without the TS checker: no resolution through type annotations (`foo: UserService` →
`foo.getUser()`) and no interface→implementation edges. These become tier-3
name-match candidates instead of certain edges. In exchange the indexer works equally
well on plain JS. Dynamic imports/requires with computed strings, string-keyed events,
DI tokens, route tables → tier-2 heuristic extractors, extensible per framework.

## Milestones

### M0 — Skeleton (a day)
Cargo workspace. CLI (`clap`) that walks a repo (respecting .gitignore via `ignore`
crate), parses every JS/TS/JSX/TSX file with oxc, prints stats (files, functions,
classes, components, parse errors). Proves the parse layer end to end.

### M1 — Chunker
Split-then-merge walker over the oxc AST. Erase type-only nodes (interfaces, type
aliases, annotations). Emit chunks with: content, blake3 hash, byte/line span, kind
(function/class/method/component/module-top), enclosing scope chain, imports in
scope, JSDoc. `jscout chunks <repo>` dumps JSONL. Golden-file tests against 2–3 real
OSS repos (e.g. excalidraw, cal.com).

### M2 — Module + symbol graph
Per-file symbols/scopes from oxc_semantic; cross-file binding by joining export maps
to imports via oxc_resolver (must see through barrel files / re-export chains). JSX
element → component binding edges. Store nodes+edges in SQLite.
`jscout who-uses <file>:<symbol>` works, with confidence labels.

### M3 — Retrieval
tantivy index over chunk text + identifiers (BM25). Embedding pipeline with the
pluggable provider trait; vectors in LanceDB/sqlite-vec, cached by chunk hash.
`jscout search "<query>"` → RRF-fused results, each expandable with graph neighbors
(callers/callees/renders). Optional reranker pass.

### M4 — Incremental + git
Hash tree over the repo; on invocation or via `notify` watcher, diff hashes → re-parse
changed files only, re-embed changed chunks only, patch graph edges for changed files
plus their direct dependents. Record indexed commit via `gix`; handle working-tree
drift. Target: sub-second re-index for a single-file edit on a large monorepo.

### M5 — Heuristic edge extractors (tier 2/3)
String-literal matching: `emit('x')`→`on('x')`, route tables, DI tokens, dynamic
imports with constant prefixes. Name-match fallback edges. Confidence surfaces in all
query output.

### M6 — Agent interface
MCP server exposing: semantic_search, who_uses, definition, neighborhood(symbol),
file_outline. This is the point where the tool becomes usable from Claude Code /
Cursor / anything.

## Open questions

- Embedding default: local ONNX (zero-config, slower) vs API-key (better, faster)?
- One storage engine or three? (SQLite-only with FTS5+sqlite-vec is simpler; tantivy+LanceDB is faster at scale.)
- Product, crate, binary, MCP server, database, and environment-variable prefix
  use `jscout` consistently.

## Reference material

- cAST paper (chunking algorithm): https://arxiv.org/html/2506.15655v1
- Cursor's Merkle-tree incremental indexing: https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast
- Knip source (real-world JS reference-resolution edge cases): https://github.com/webpro-nl/knip
- oxc docs: https://oxc.rs · scip-typescript (checker-based alternative): https://github.com/sourcegraph/scip-typescript
- Stack graphs (ambiguity-as-ranked-candidates model): https://github.blog/open-source/introducing-stack-graphs/
