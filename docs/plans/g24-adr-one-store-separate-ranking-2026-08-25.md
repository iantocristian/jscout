# Documentation in the main index: one store, separate ranking

- Date: 2026-08-25
- Status: accepted decision record; revises the storage half of G24 and introduces Proposed G25. Subordinate to PLAN.md — the G24/G25 entries win on any disagreement.
- One line: docs live in the main index and lifecycle, but rank in their own corpus with their own surface — code search is untouched by construction, not by discipline.

## The question

Should Markdown and MDX (and later YAML, TOML, Groovy) go through the main index pass, or stay a separate product with a separate database?

## Why this shape

Storage and ranking are independent axes. G24 separated the database because it needed separate ranking. The unification argument shared the ranking because it shared the database. Neither coupling is necessary: BM25 statistics are per-FTS-table, so one database can host any number of ranking corpora.

Code and docs also have different truth models. Code's temporal model is already built in — the snapshot means only the current checkout exists, so recency is meaningless. Docs can be stale, obsolete, or contradictory, and staleness is invisible in prose, so docs need an explicit freshness model. Two truth models, two ranking corpora. One project, one store.

## Decision

1. **One database, one lifecycle.** One snapshot, one walker, one watcher, one entity plane. Docs are ordinary rows in `files`/`chunks`, captured by the shared repository inventory and published by the same index pass; Markdown and MDX extraction emit no code graph rows, including no `file:` graph node. Every admitted `files` row records `corpus` (`code` or `docs`) independently from `format` (the parser/format identity). `[docs].enabled` independently disables docs admission without disabling code indexing.

2. **Ranking corpus is explicit file identity, not inferred metadata.** `files.corpus` determines ranking-corpus membership. Code keeps `chunks_fts` exactly as it is — same table, same term statistics, same pipeline; ranked content and statistics stay byte-identical modulo the shared snapshot identifier. Docs get their own FTS table and their own candidate pipeline. Vector candidates are materialized per corpus over the shared content-addressed embedding cache. `files.format` selects parser/format behavior, while `chunks.kind` describes structure within a parsed file; neither substitutes for `files.corpus`.

3. **Docs are their own surface.** The `documentation_search` tool and CLI from G24, with its hit shape and ranking. Disabled docs expose neither retrieval surface. If a combined view is ever wanted, it is an explicit interleave of two ranked lists — never statistical fusion. Documentation vector generation is also separate: only `jscout docs embed` creates missing docs vectors; indexing and the code embedding path do not.

4. **Temporal ranking belongs to the docs corpus only, and it splits into three pieces with different homes.** (a) Git author time and working-tree state — the primary basis — are recomputed from the checkout by blame at index time: disposable columns on `doc_chunk_meta`, shipped in the freshness phase. (b) The bounded reorder is a query-time stage that only `documentation_search` runs; it cannot affect code search. (c) The block observation ledger is not a freshness feature: its reasons are supersession lineage ("this passage replaced that one" — the backbone of the contradiction story), an ordering clock that survives git history rewriting, and finer-than-commit resolution under watch. It is append-only and therefore durable-plane, not disposable; it ships with the supersession/contradiction product if that product is built and owes the explicit cache-compatibility decision PLAN.md requires for durable changes. The one genuine conflict between temporal history and one-database-one-snapshot — the main index keeps no snapshot history, and full rebuild wipes the disposable plane — is resolved by gluing two durable pieces onto the existing mechanism rather than changing it: a whole-codebase `snapshot_log` (sequence, digest, published-at; appended in the publication transaction when the digest changed) gives the index an ordered snapshot timeline that observations reference, and a rolling `doc_block_state` baseline (per block: content hash, position, heading context, occurrence ID — no bodies) is replaced at each observing scan so matching always compares last-observed against current, even across a full rebuild. Matching needs only the previous state — accumulated history is its output, never its input. History recording is a per-format registry property, on for Markdown and MDX only. Unchanged blocks add no rows; whole-codebase snapshots tighten observation intervals for free.

5. **One list decides, per format: how much we understand it (plain text → named sections → full AST) and which `files.corpus` value it receives.** The list gives a new format a defined place to plug in, so adding one never reopens the storage or ranking architecture — and that is all it buys. Each format still needs its own parser or scanner and its own chunking rules (Markdown's alone took a full design cycle), and anything joining code search additionally pays the ranking and exact-tier integration that docs deliberately avoid. Text-only formats are cheap; languages are real work. Groovy would be a distinct `files.format` in the `code` corpus; Markdown is the `markdown` format and MDX is the `mdx` format in the `docs` corpus; YAML/TOML stay out until the initial documentation corpus has shipped and been measured.

## Terminology

In PLAN.md's storage-plane table, docs content joins the existing *disposable
structural snapshot* row; `docs_fts` is one more disposable projection inside
it. `files.corpus` is the ranking membership (`code` or `docs`),
`files.format` is parser/format identity, and `chunks.kind` is the structural
role of one chunk within its file. These are independent dimensions. Code and
docs are two corpora in one plane. No new plane is created; if the observation
ledger ever ships, it is a durable-plane addition and owes the explicit
cache-compatibility decision PLAN.md already requires for those.

## Why one store

PLAN.md already decided this: "The database file is an implementation
container, not a shared lifecycle. Physical database splitting adds backup,
transaction, and deployment complexity without improving this contract and is
not planned." The G24 split contradicted that sentence.

The monorepo questions worth having — an env var read in code but declared nowhere, a Helm value naming a service that doesn't exist, a file loaded but never supplied to anything — are joins between code occurrences and non-code occurrences over the string-keyed entity plane that already exists. Two databases make those queries impossible. One database makes them a GROUP BY.

## Storage: which tables

| table | status | holds |
|---|---|---|
| `files` | shared, disposable | every admitted file; `corpus` is `code` or `docs`, and `format` records parser/format identity; a G24 docs row has `corpus='docs'` and `format='markdown'` or `format='mdx'` according to its extension |
| `chunks` | shared, disposable | one row per parsed structural unit: `kind` is its intra-file structural role; a docs section uses `kind='markdown_section'`, `name=NULL`, `symbols=''`, exact source slice, spans/hash |
| `code_files`, `code_chunks` | new disposable views | `code_files` is `files WHERE corpus='code'`; `code_chunks` joins through `code_files`, providing the explicit code-corpus boundary for canonical-row consumers |
| `docs_fts` | new FTS5 | the docs ranking corpus — G24's field table as columns: title, meta, breadcrumb, body (rendered), path; rowid = chunk id |
| `doc_chunk_meta` | new, chunk-id keyed | documentation retrieval metadata: title, description, lossless tags, breadcrumb, nearest heading, same-heading ordinal, embedding identity; later the freshness columns. It does not define corpus membership. |
| `vec_doc_embeddings_{dimensions}` | new | docs vector candidates — separate because sqlite-vec applies KNN's k before any join filter can run, so vector corpus isolation cannot be done with a predicate |
| `embeddings` | shared, durable | content-addressed cache; a docs embedding identity is just another hash in it |

`chunks.content` stays the exact source slice (spans always slice back to the file); `docs_fts.body` holds the rendered text that is actually searched. `name=NULL` makes docs invisible to the exact-definition tier. Broad `files`/`chunks` consumers are made corpus-safe by the explicit `files.corpus` classification and the central `code_files`/`code_chunks` views; they never infer membership from `chunks.kind` or the presence of `doc_chunk_meta`. FTS and vector materialization remain explicit write-point routings. Everything above the cache line is disposable — dropped and recreated on rebuild like `chunks_fts` already is. Deferred with the ledger, all durable: `snapshot_log`, `doc_block_state`, and `doc_block_observations` — the trio described in decision 4.

## What this does to G24

- **Survives**: the corpus spec (membership, chunking, embedding identity), conservative block history, the freshness model, the docs surface and hit contract, retention rules.
- **Moves**: storage — from `.jscout-docs.db` into the tables above.
- **Dies**: the separate database, `[docs.database]` configuration, and the docs-scoped snapshot sequence (replaced by the shared one).
- **Re-scoped**: the observation ledger — from freshness input to the supersession product's backbone, durable-plane, unscheduled.

## Accepted costs

- Docs rows mirror into the docs FTS table (never `chunks_fts`), carry no `name`/`symbols` in shared rows (headings live in the docs corpus tables), and materialize their own vector candidates. Empty structural fields protect exact tiers but do not protect broad canonical-table consumers. The disposable code-corpus views therefore filter explicit `files.corpus` membership for checker inventory, code embeddings, structural support, reconnaissance, and scouting; they replace scattered negative documentation predicates. A future parser such as Groovy adds a `files.format` value and chooses a corpus without adding another membership sidecar.
- Docs edits rotate the shared snapshot: full projection rebuild, checker batch invalidation, held anchors re-resolve. Accepted for v1 and instrumented; if measurement says it hurts, the fix is digest-splitting inside the one index — not a second database.
- One embedding model for both corpora, for now.

## First pass

Markdown and MDX, at the named-sections tier. Both use the same inert pinned CommonMark/GFM-table scanner. Raw MDX JSX, props, expressions, inner text, and non-leading ESM remain authored text, never evaluated or projected into the code corpus or graph. A narrow leading-block classifier suppresses only a contiguous import/export-only preamble, and exact JSX comments are removed outside protected code ranges consistently with HTML comments. The first pass adds `files.corpus` and `files.format`, the central corpus views, and write-point routing in the same PR. Nothing temporal ships in the first pass — docs search launches as pure relevance, and hits may display the git timestamp as inert metadata so decay can be judged before it is built. Measure rebuild time and docs-corpus retrieval quality before any YAML decision. Reversal is cheap by design: disable docs admission and reindex.

## Out of scope

Tree-sitter, Helm template semantics, cross-path rename history, a combined interleaved surface, per-format embedding profiles.
