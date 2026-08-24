# G24 Markdown retrieval and temporal history — design detail

- Date: 2026-08-24, revised the same day after review.
- Status: subordinate, non-normative detail for the Proposed G24 entry in
  [PLAN.md](../../PLAN.md). PLAN.md is the only normative document; where this
  file and the G24 entry disagree, the G24 entry wins. Review findings and
  decisions are recorded on PR #96.
- Scope: a standalone repository-documentation corpus with lexical/vector
  retrieval and bounded, order-based freshness.

## Decision summary

Jscout treats repository Markdown as a separate retrieval product with its own
database. It is not a source extension of the structural index and not
semantic memory. The surface is `jscout docs`: it owns its inventory,
Markdown-aware chunks, BM25 index, vector index, search results, snapshot,
and observation ledger inside the configured documentation database, which
defaults to `<root>/.jscout-docs.db`.

BM25 always builds. Vector retrieval reuses the repository's existing
`[embedding]` provider and model; when that provider is absent, documentation
search degrades to lexical retrieval rather than disabling. Freshness is a
bounded reordering applied after relevance, only between candidates whose
provenance is comparable, and never through the model reranker.

History is a block-observation ledger, not reconstructed source history.
Matching is conservative and one-to-one: exact content first, unique
neighbor-anchored block alignment second, and no succession claim from ordinal
position alone. Baseline content with no Git provenance has unknown authorship
time.

## Product boundary

- code search answers from structurally indexed JS/TS chunks;
- semantic memory answers from evidence-backed semantic artifacts;
- documentation search answers from current repository Markdown;
- callers opt in through `jscout docs ...` and a separate MCP
  `documentation_search` tool; documentation hits never masquerade as
  structural hits, seed graph expansion, or satisfy semantic support anchors.

Normal documentation search indexes only the current checkout. Retired block
content and retired cached vectors never enter BM25 or vector candidate
generation.

## Separate documentation database

The first review round showed the shared store cannot host an independent
plane: it has one global schema version whose upgrade path rebuilds
source-derived tables, and every read-only open requires a published
structural snapshot. Documentation therefore lives in a separately configured
database, defaulting to `<root>/.jscout-docs.db`, with:

- its own schema version, migration lifecycle, and readiness gate (a
  published documentation snapshot only);
- its own last-good snapshot publication and retry state;
- its own embedding-profile row, resolved from the shared `[embedding]`
  settings;
- no participation in structural snapshots, code config fingerprints, watch
  generations, or semantic freshness.

A code reindex cannot disrupt documentation search, and a
documentation migration failure cannot affect the main database. The history
is local and is never a replacement for repository version control.

## Configuration

Documentation settings live in the existing repository configuration:

```toml
[docs]
include = ["**/*.md"]
exclude = []
freshness = true
max_rank_movement = 2

[docs.database]
path = ".jscout-docs.db"

[docs.search]
vector = true
rerank = true
limit = 10
response_bytes = 24000
```

`[docs.database]` is optional; its path defaults to `.jscout-docs.db` and is
resolved relative to the indexed root. It is independent of `[database].path`.
There is no `[docs.embedding]` or `[docs.reranker]` section: vectors use the
repository `[embedding]` provider, model, and service, and reranking uses the
repository `[reranker]` profile. Compatibility of a committed `[docs]` section
with pre-docs jscout binaries is explicitly not a requirement. The feature
activates only through a `docs` command, never through the mere presence of
Markdown.

## Markdown corpus specification

### Membership

The first rule that applies decides membership:

1. Deterministic skips and repository ignore files prune traversal with the
   same ignore semantics as the code plane; `.git` is always a hard skip.
2. The docs walker additionally descends into the fixed root-level hidden
   directory allowlist `.github`, `.claude`, and `.agents`. All other hidden
   paths remain excluded.
3. `exclude` globs, anchored at the indexed root, matching files.
4. `include` globs, anchored at the indexed root, matching files; default
   `**/*.md`.

Exclude beats include; ignore beats both; include cannot resurrect an ignored
file in v1. Files larger than 4 MiB (4,194,304 bytes, an evaluation
hypothesis) are excluded from admission. `docs status` reports the deciding
rule per encountered file (`indexed`, `excluded`, `not-included`,
`hidden-not-allowlisted`, `oversized`, `non-utf8`) and per
pruned directory (`hard-skip`, `ignored`, `hidden-not-allowlisted`), without
enumerating descendants beneath pruned directories. Version one admits `.md`
only; MDX requires a separate parsing and safety decision.

### Field composition

| Field              | Source, in fallback order                    | FTS            | Embedded text | Metadata     |
| ------------------ | -------------------------------------------- | -------------- | ------------- | ------------ |
| Document title     | front-matter `title` → first H1 → file stem  | highest weight | no            | yes          |
| Description, tags  | front matter                                 | medium weight  | no            | yes          |
| Heading breadcrumb | full heading path                            | medium weight  | no            | yes          |
| Nearest heading    | closest enclosing heading                    | via breadcrumb | yes           | yes          |
| Rendered body      | deterministic retrieval rendering of source  | base weight    | yes           | no           |
| Source             | snapshot-relative byte/line spans + file hash | no             | no            | yes          |
| Path               | repository-relative                          | lowest weight  | no            | yes          |

`rendered_body` is the final deterministic body string sent to FTS and the
embedder after comment removal and any bounded synthetic fence/table context
has been applied. Embedding identity is exactly
`hash(format_version, nearest_heading, rendered_body)`, using the same
versioned serialization sent to the embedder. Nothing else enters that hash,
so a file rename reuses every vector, an ancestor-heading or title edit is
metadata-only, an H1 rename re-embeds only chunks whose nearest heading is
that H1, and timestamps never affect vector identity. The low-weight path
column serves directory vocabulary such as `adr` or `api/v2` without coupling
renames to the vector cache. Column weights are evaluation hypotheses, not
compatibility constants.

### Front matter

- Recognized only when the file begins with `---`, has a valid closing
  delimiter, parses as YAML, and produces a top-level mapping. A valid YAML
  scalar or sequence is not front matter and remains ordinary body text.
- Only scalar-string `title` and `description` values and a scalar string or
  sequence of scalar strings for `tags` are used; other keys or value types are
  ignored. Front-matter dates do not affect freshness in v1.
- Valid front matter is never emitted as a body chunk.
- Malformed or unterminated front matter is ordinary Markdown body text,
  reported by `docs status` as `front_matter=malformed_as_body`, not a
  rejection.

### Blocks and chunks

Markdown parses into source-backed blocks before size-based merging:
paragraphs, headings, lists, tables, block quotes, fenced/indented code,
visible HTML blocks, and thematic separators. HTML comments are excluded from
retrieval text. Headings establish structure and metadata but are not
independent history occurrences; the ledger tracks retrieval-bearing body
blocks. Chunks never cross heading boundaries. History alignment operates on
the underlying body blocks independently of target-size merging.
Retrieval chunks may regroup when blocks are inserted or removed; that may
rebuild vectors, but it does not fabricate history transitions for unchanged
blocks.

Initial deterministic bounds, all evaluation hypotheses:

```text
target:                2,400 rendered UTF-8 bytes (~600 tokens)
normal max:            4,000 rendered UTF-8 bytes (~1,000 tokens)
hard max:             24,000 final embedding-input UTF-8 bytes
heading context max:   1,024 rendered UTF-8 bytes
synthetic body max:    1,024 rendered UTF-8 bytes
token estimate:        rendered bytes / 4
```

The hard bound applies after nearest-heading serialization and synthetic
context are added. If the nearest heading exceeds its context bound, its
largest UTF-8 prefix that leaves room for the literal `\n[heading truncated]`
is retained and the marker is appended; that bounded value is the
`nearest_heading` used by the embedding identity. Fence info or table-header
context is serialized as a prefix of `rendered_body` and truncated the same
way within the synthetic-body bound using `\n[context truncated]`. The
remaining total byte budget is used for source text.

Oversized atomic blocks first split at the last block-native boundary before
that remaining bound: newline for code, row for tables, and top-level item for
lists. If no non-empty native fragment fits, every block type falls back to the
last newline before the bound and then to the last UTF-8 boundary. This covers
multiline paragraphs, quotes, HTML blocks, and oversized individual list
items. Fragments repeat the bounded synthetic context in their rendered body
without altering exact source spans. Fragment ordinals establish order only,
never historical succession.

### Documents without body chunks

A document producing no body chunks emits exactly one lexical-only
document-stub row: title, description, tags, path, and all document headings
are searchable, the headings carried in source order by the stub's breadcrumb
column at its weight; empty rendered body; no nearest heading; span
covering the file. Stubs are not embedded. There are no empty per-section
chunks.

## Storage model

Within the configured documentation database, responsibilities (names may
change during implementation):

- `doc_snapshots`: one immutable row per successfully published scan —
  monotonic local sequence, observation timestamp, corpus fingerprint,
  optional Git worktree/commit identity with author and committer times,
  chunk-format version, inventory and rejection counts. A failed scan never
  publishes or advances the sequence.
- `doc_files`: current admitted documents — repository-relative path, indexed
  full-file content hash, byte and line accounting, and document metadata.
- `doc_block_contents`: content-addressed source blocks — raw-body hash and
  body text only while a current block occurrence references it.
- `doc_block_occurrences`: current source-backed history units — path,
  stable logical-occurrence ID, current-observation ID, structural order,
  exact source span, content hash, heading context, and Git provenance when
  available.
- `doc_block_observations`: immutable append-only block events — lifecycle
  event, logical-occurrence ID, predecessor-observation ID when uniquely
  established, zero or more change flags, snapshot sequence, content hash,
  match confidence, and provenance. Unchanged blocks add no rows and retain
  their last current-observation reference. The current occurrence projection,
  not the event ledger, defines what is active in the published snapshot.
- `doc_chunks`: the current searchable projection built from ordered current
  blocks — path, breadcrumb, source spans, rendered body, embedding identity,
  and aggregated freshness provenance. FTS rows and vector index entries
  reference these chunks.
- retrieval projections: `doc_chunks_fts`, content-addressed
  `doc_embeddings` keyed by embedding-identity hash and profile,
  `doc_embedding_index_entries`, and `vec_doc_embeddings_{dimensions}`.

Publication is transactional within the docs database: the new projection is
built, then the current-snapshot pointer swaps atomically; failure at any
earlier point leaves the previous snapshot active and searchable.

## History and continuity

Each block observation stores one lifecycle event — `baseline`, `added`,
`continued`, or `removed` — plus zero or more orthogonal change flags:

- `body_changed`: a uniquely matched predecessor has different body content;
- `context_changed`: nearest heading or other retrieval context changed;
  source-offset and ordinal changes alone do not count; and
- `moved`: the matched block changed path or reordered relative to matched
  neighboring blocks. Line, byte-span, or ordinal shifts caused only by an
  insertion or deletion are not movement.

A transition can therefore be both `body_changed` and `context_changed`, or
both `body_changed` and `moved`. A successful scan emits `removed` when a
previous block is confirmed absent from the current corpus. A permanent
per-file read, parse, or inventory failure is recorded as a visible rejection,
emits no block lifecycle event, and removes the file from the current
projection. Matching never crosses that failure gap. If the file later parses,
its blocks receive new logical-occurrence IDs with lifecycle `baseline`; their
observation time is not authorship time. A retryable corpus failure publishes
nothing and leaves the complete last-good snapshot active.

For observed provenance, a post-baseline `added` event establishes freshness
at its snapshot sequence, and `body_changed` advances it. `context_changed` or
`moved` alone carries the prior freshness forward: a heading rename does not
make the underlying claim newly authored. An initial or post-gap baseline
without Git provenance has provenance `unknown`; the baseline observation's
snapshot timestamp is operational metadata, not authorship time.

Matching between two successfully parsed snapshots is conservative and
one-to-one:

1. Within each unchanged path, an exact hash that occurs once on each side
   matches directly, independent of heading text or source order. A matched
   block is `moved` when its relative order against other matched blocks
   changed.
2. Repeated exact hashes within one path match only when already matched
   neighboring blocks leave exactly one one-to-one monotone pairing. Otherwise
   every ambiguous copy remains unmatched.
3. Version one never creates predecessor edges across repository paths. Git
   rename detection is heuristic and therefore cannot prove succession; a
   rename may be reported as snapshot metadata, but it does not change block
   matching. Unmatched old and new blocks receive no cross-path predecessor
   even when their exact hash is globally unique.
4. An edited block receives a predecessor only when exactly one unmatched old
   block and one unmatched new block occur between the same immediately
   adjacent matched neighbors in one document. The pair is `body_changed` and
   also receives any applicable context or movement flag.
5. Every other unmatched new block is `added`; every other confirmed unmatched
   old block is `removed`.

Ordinal position alone never establishes continuity. If duplicate content,
multiple valid monotone pairings, document-edge edits, split/merge/reflow, or
any other case leaves more than one predecessor or successor possible, no
predecessor is recorded. False succession is worse than missing succession.

## Git provenance

In a Git worktree, documentation indexing records the checked-out `HEAD` and
runs one line-porcelain blame per changed tracked Markdown file, mapping
blamed lines onto already-produced blocks and then aggregating them into
retrieval chunks. Rules:

- both author and committer times are stored; "newest" for freshness means
  the latest author time among contributing body lines, because author time
  survives rebase and cherry-pick while committer time is rewritten to the
  integration date;
- shallow-clone boundary commits contribute no timestamp; a chunk whose
  contributing lines all blame to a boundary commit has unknown git age;
- provenance Git commands disable replacement objects with
  `--no-replace-objects`, and blame clears repository `blame.ignoreRevsFile`
  configuration with `-c blame.ignoreRevsFile=`. Ambient replace refs and
  ignore-revs configuration cannot alter attribution;
- the blame mapping cache key includes the repository-relative path, a hash of
  the exact file bytes being blamed, the newest commit touching that path, and
  the shallow boundary fingerprint. Worktree edits, path-history rewriting,
  clone deepening, and same-content files with different histories therefore
  resolve correctly; unrelated commits and staging an unchanged worktree file
  do not invalidate it;
- modified lines in an already tracked file are labelled `working_tree`
  whether staged or unstaged and order newer than committed lines, without
  inventing a commit;
- newly added staged files and untracked files have no Git authorship time and
  carry observed or unknown provenance;
- filesystem modification time is never a fallback;
- Git absence or a per-file blame failure emits a diagnostic and degrades
  that file to observed/unknown provenance without failing the scan.

Git history is metadata for current chunks only; previous file revisions are
never ingested.

## Freshness ordering

Freshness is a bounded reordering, not a score. The pipeline is: BM25 and
vector retrieval, reciprocal-rank fusion with deterministic tie-breaks, the
optional model reranker — which receives path, breadcrumb, and content, and
never temporal metadata — then freshness reordering, then truncation to
`limit` and response-budget shedding. Fusion and reranking retain at least
`limit + max_rank_movement` candidates through freshness reordering whenever
that many candidates exist.

Reordering rule:

1. Record every candidate's one-based relevance rank as `base_rank`.
2. Scan adjacent pairs from rank 1 downward. A pair may swap only when the
   lower candidate is strictly newer under the partial order below and both
   candidates' resulting positions remain within `max_rank_movement` of their
   own `base_rank`.
3. Repeat the same top-to-bottom scan until a complete scan makes no swap.

Every swap removes one comparable freshness inversion, so the procedure
terminates. The original-rank guard, rather than the number of scans, enforces
that each candidate rises or falls by at most `max_rank_movement`. Base
relevance order is otherwise preserved.

Comparable provenance and the partial order:

- within git provenance: `working_tree` is newest, then latest author time
  among contributing body lines;
- within observed provenance: the later post-baseline `added` or
  `body_changed` event wins by snapshot sequence; context-only and move-only
  events retain the preceding observed value;
- git-basis and observed-basis candidates are not comparable in v1 — their
  clocks differ — and never reorder against each other;
- unknown provenance participates in no reordering and receives no advantage
  or penalty.

A retrieval chunk has one basis, chosen deterministically: `working_tree` when
any contributing body line has that label; otherwise `git` with the latest
usable author time among its contributing lines; otherwise `observed` with the
latest freshness-bearing block event whenever usable Git authorship is absent,
including non-Git repositories, blame failures, untracked files,
and newly staged files; and otherwise `unknown`. A chunk whose lines all map to
shallow boundary commits is `unknown` unless a later local observed body event
exists.

Every hit exposes its freshness basis (`git`, `working_tree`, `observed`,
`unknown`), the basis value, base rank, and movement. The basis value is the
Git author timestamp for `git`; the literal `uncommitted` for `working_tree`,
with the latest committed author time as secondary metadata when one exists;
the freshness-bearing observation snapshot sequence and timestamp for
`observed`; and absent for `unknown`.
Compact agent output retains path, heading, lines, basis, and a
human-readable changed/observed value. `--no-freshness` disables reordering
for comparison while still reporting bases.

## CLI and MCP surface

```text
jscout docs index <root>     local, deterministic; no provider request
jscout docs embed <root>     embeds missing current representations
jscout docs search <root> <query>
jscout docs status <root>
```

Search contract, defined directly rather than by analogy with code search:

```text
default          BM25, plus vector fusion when the profile has a usable index
--lexical-only   BM25 only; skips vector retrieval and the model reranker
--vector         require vector participation: error when no [embedding]
                 provider is configured or no usable docs index exists;
                 retrieval remains hybrid — there is no vector-only mode
--no-vector      BM25 only; reranker unaffected
--rerank / --no-rerank   override the configured reranker
--no-freshness   skip freshness reordering
```

The MCP `documentation_search` tool returns documentation-specific hits:
path, heading breadcrumb, line range, content, documentation snapshot
sequence, indexed file hash, freshness basis and value, base rank and movement.
Byte and line spans are relative to that indexed file hash. To return raw
checkout source, jscout reads the file once into an immutable buffer, hashes
that buffer, and slices only that same buffer when the hash matches. On a
mismatch or missing file, it returns the stored hit content and a
source-mismatch state, never bytes from the wrong revision. Result budgets
follow the same complete-response accounting as code search.

## Failure semantics

- Markdown read/parse failure: use the code-plane I/O classification; a
  permanent per-file failure is a visible corpus rejection, emits no block
  lifecycle event, removes the file from the current projection, and prevents
  matching across the gap; a retryable corpus failure publishes nothing and
  leaves the previous documentation snapshot active.
- Embedding provider absent: BM25 remains active.
- Provider failure during `docs embed`: completed cached batches are kept;
  search reports degraded vector status and uses BM25 — a previous vector
  projection is cache, never queryable as current.
- Docs database migration failure: the main database is unaffected.
- History matching ambiguity: new occurrence, never a guessed successor.

Further failure-state machinery is deferred until the base feature is in use.

## Retention

- Hit content is served from stored current rendered bodies and block text.
  Exact source spans are snapshot-relative and paired with the indexed full-
  file hash. Checkout source is read once into an immutable buffer; that same
  buffer is hashed and, only on a match, sliced. No full raw Markdown copy is
  stored.
- After a successful replacement snapshot, retired block bodies are removed
  from logical storage. The ledger keeps hashes and transition metadata, and
  the content-addressed vector cache may keep retired vectors.
- Version one adds no retention configuration or purge command.

## Delivery and acceptance

The normative delivery phases and acceptance gate live in the PLAN.md G24
entry. This document intentionally does not duplicate them.

## Validation

Deterministic tests:

- membership precedence is deterministic and visible: exclude beats include,
  ignore beats both, the hidden allowlist admits `.github`, `.claude`, and
  `.agents`, and `docs status` names the deciding rule;
- front-matter recognition requires the specified top-level mapping and value
  types; fallback-to-body and title derivation follow the specified order;
- the embedding key hashes the exact versioned text sent to the embedder; a
  file rename changes no embedding identity;
- block-aligned matching: inserting one uniquely distinguishable paragraph
  yields exactly one `added` block observation and no events for untouched
  blocks, even when retrieval chunks regroup;
- duplicate, ordinal-only, and ambiguous split/merge matches produce no
  predecessor; combined edits retain every applicable change flag;
- globally unique copied content and Git-detected renames receive no cross-path
  predecessor in version one;
- heading renames produce `context_changed` only; post-baseline additions and
  body changes advance observed freshness, while context-only and move-only
  changes do not;
- every oversized block type splits deterministically, final rendered
  representations stay within the hard byte bound, and source spans are exact;
- a document with no body chunks yields exactly one searchable lexical-only
  stub row;
- BM25 works with no provider; `--vector` errors without one; hybrid RRF is
  deterministic across insertion order;
- freshness movement from each candidate's recorded base rank never exceeds
  `max_rank_movement`; git and observed candidates never reorder against each
  other; unknown provenance never moves; a rank just outside `limit` can enter
  the result when the bound permits;
- shallow boundary commits yield unknown git age; rebase preserves author
  time and freshness ordering; the blame cache survives unrelated commits and
  invalidates on worktree edits, path-history rewrite, and clone deepening;
  two identical files with distinct path histories use distinct cache entries;
  configured ignore-revs and replacement refs do not alter attribution;
- a code reindex leaves documentation search available and vice versa; a
  retryable docs scan failure leaves the prior snapshot searchable;
- a permanent per-file failure is visibly rejected without emitting `removed`
  or another block lifecycle event; a later successful parse starts new
  baseline logical occurrences and cannot inherit freshness across the gap;
- checkout edits after publication, including a concurrent edit during source
  resolution, never make snapshot spans resolve to wrong bytes: raw source is
  hashed and sliced from one captured immutable buffer;
- code and semantic snapshots, counts, and fingerprints are byte-identical
  after any docs operation, and the documentation database is byte-identical
  after any code or semantic operation.

Retrieval evaluation, before freshness defaults are accepted: a fixed corpus
with conflicting versioned instructions of known order, old evergreen
specifications plus recent irrelevant mentions, changelog sections beside
canonical guides, formatting-only and heading-only edits, renamed and split
sections, dirty/staged/untracked documents, and clean, shallow, and non-Git
copies. Measure current-answer top-k recall, older-conflict visibility,
evergreen regressions, BM25-only parity, vector/hybrid lift, and how many
result orders change solely due to freshness, comparing `--no-freshness`
against movement bounds of 1–3.

## Open items

- FTS column weights, chunk-size bounds, the 4 MiB file-admission bound, and
  the `max_rank_movement` default are evaluation hypotheses.
- Historical search, contradiction detection, MDX, remote documentation
  sources, and author-declared supersession remain out of scope.
