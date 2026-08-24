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
and observation ledger, all inside `.jscout-docs.db`.

BM25 always builds. Vector retrieval reuses the repository's existing
`[embedding]` provider and model; when that provider is absent, documentation
search degrades to lexical retrieval rather than disabling. Freshness is a
bounded reordering applied after relevance, only between candidates whose
provenance is comparable, and never through the model reranker.

History is an observation ledger, not reconstructed source history. Matching
is conservative: exact content first, block alignment second, and no
succession claim from ordinal position alone. Baseline content with no Git
provenance has unknown authorship time.

## Product boundary

- code search answers from structurally indexed JS/TS chunks;
- semantic memory answers from evidence-backed semantic artifacts;
- documentation search answers from current repository Markdown;
- callers opt in through `jscout docs ...` and a separate MCP
  `documentation_search` tool; documentation hits never masquerade as
  structural hits, seed graph expansion, or satisfy semantic support anchors.

Normal documentation search indexes only the current checkout. Historical
chunk bodies never enter BM25 or vector candidate generation.

## Separate documentation database

The first review round showed the shared store cannot host an independent
plane: it has one global schema version whose upgrade path rebuilds
source-derived tables, and every read-only open requires a published
structural snapshot. Documentation therefore lives in `.jscout-docs.db`,
stored beside the configured main database, with:

- its own schema version, migration lifecycle, and readiness gate (a
  published documentation snapshot only);
- its own last-good snapshot publication and retry state;
- its own embedding-profile row, resolved from the shared `[embedding]`
  settings;
- no participation in structural snapshots, code config fingerprints, watch
  generations, or semantic freshness.

A code reindex cannot make documentation search unavailable, and a
documentation migration failure cannot affect the main database. Deleting
`.jscout-docs.db` removes jscout's complete local documentation state,
including observation history; the history is local and is never a
replacement for repository version control.

## Configuration

Documentation settings live in the existing repository configuration:

```toml
[docs]
include = ["**/*.md"]
exclude = []
freshness = true
max_rank_movement = 2

[docs.search]
vector = true
limit = 10
response_bytes = 24000
```

There is no `[docs.embedding]` section: vectors use the `[embedding]`
provider, model, and service as configured for the repository. Compatibility
of a committed `[docs]` section with pre-docs jscout binaries is explicitly
not a requirement. The feature activates only through a `docs` command, never
through the mere presence of Markdown.

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
file in v1. `docs status` reports the deciding rule per encountered file
(`indexed`, `excluded`, `not-included`, `oversized`, `non-utf8`) and per
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
| Body               | exact chunk text                             | base weight    | yes           | exact source |
| Path               | repository-relative                          | lowest weight  | no            | yes          |

Embedding identity is exactly `hash(format_version, nearest_heading, body)`.
Nothing else enters that hash, so a file rename reuses every vector, an
ancestor-heading or title edit is metadata-only, an H1 rename re-embeds only
chunks whose nearest heading is that H1, and timestamps never affect vector
identity. The low-weight path column serves directory vocabulary such as
`adr` or `api/v2` without coupling renames to the vector cache. Column
weights are evaluation hypotheses, not compatibility constants.

### Front matter

- Recognized only when the file begins with `---`, has a valid closing
  delimiter, and parses as YAML.
- Only `title`, `description`, and `tags` are used; other keys are ignored;
  front-matter dates do not affect freshness in v1.
- Valid front matter is never emitted as a body chunk.
- Malformed or unterminated front matter is ordinary Markdown body text,
  reported by `docs status` as `front_matter=malformed_as_body`, not a
  rejection.

### Blocks and chunks

Markdown parses into source-backed blocks before size-based merging:
paragraphs, headings, lists, tables, block quotes, fenced/indented code,
visible HTML blocks, and thematic separators. HTML comments are excluded from
retrieval text. Chunks never cross heading boundaries. History alignment
operates on the underlying blocks before target-size merging, so inserting a
paragraph does not shift every later chunk's identity.

Initial deterministic bounds, all evaluation hypotheses:

```text
target:      ~600 tokens
normal max:  ~1,000 tokens
hard max:    24,000 UTF-8 bytes
estimate:    bytes / 4
```

Oversized atomic blocks split at the last block-native boundary before the
hard bound: newline for code, row for tables, top-level item for lists; a
single oversized line splits at the last UTF-8 boundary. Fragments repeat
required synthetic context (fence info string, table header) in their
FTS/embedded representation without altering the exact source span. Fragment
ordinals establish order only, never historical succession.

### Documents without body chunks

A document producing no body chunks emits exactly one document-stub row:
title, description, tags, breadcrumb, and path searchable; empty body; span
covering the file; embedded only when its representation is non-empty. There
are no empty per-section chunks.

## Storage model

Within `.jscout-docs.db`, responsibilities (names may change during
implementation):

- `doc_snapshots`: one immutable row per successfully published scan —
  monotonic local sequence, observation timestamp, corpus fingerprint,
  optional Git worktree/commit identity with author and committer times,
  chunk-format version, inventory and rejection counts. A failed scan never
  publishes or advances the sequence.
- `doc_chunk_contents`: content-addressed representations — raw-body hash,
  embedding-identity hash, body text while a current occurrence references
  it, token/byte accounting, format version.
- `doc_chunk_occurrences`: the current searchable projection — path,
  breadcrumb, same-heading ordinal, exact source spans, content identity,
  freshness provenance. FTS rows and vector occurrence entries reference
  these.
- `doc_chunk_observations`: append-only validity intervals per logical
  occurrence — content identity, first/last active snapshot, transition kind
  and confidence, Git provenance when available. Unchanged chunks add no rows.
- retrieval projections: `doc_chunks_fts`, content-addressed
  `doc_embeddings` keyed by embedding-identity hash and profile,
  `doc_embedding_index_entries`, and `vec_doc_embeddings_{dimensions}`.

Publication is transactional within the docs database: the new projection is
built, then the current-snapshot pointer swaps atomically; failure at any
earlier point leaves the previous snapshot active and searchable.

## History and continuity

Transition kinds, stored per observation:

- `body_changed`: body blocks changed;
- `context_changed`: nearest heading or occurrence metadata changed;
- `moved`: identical content changed location;
- `added`; and
- `removed` — recorded only for confirmed inventory removal; a file excluded
  by a read or parse failure does not close its intervals as `removed`.

Only `body_changed` affects freshness initially: a heading rename changes
embedding context without making the underlying claim newly authored.

Matching order between two snapshots:

1. exact content at the same location;
2. exact content moved elsewhere;
3. exact underlying-block alignment within the same section;
4. high-confidence edited-block alignment anchored by unchanged neighboring
   blocks;
5. otherwise a new occurrence with no predecessor.

Ordinal position alone never establishes continuity. False succession is
worse than missing succession; ambiguous split/merge/reflow cases create new
occurrences. The first snapshot records occurrences without Git provenance as
`baseline_unknown`: `first_seen_at` is operational metadata, not authorship
time.

## Git provenance

In a Git worktree, documentation indexing records the checked-out `HEAD` and
runs one line-porcelain blame per changed tracked Markdown file, mapping
blamed lines onto already-produced chunks. Rules:

- both author and committer times are stored; "newest" for freshness means
  the latest author time among contributing body lines, because author time
  survives rebase and cherry-pick while committer time is rewritten to the
  integration date;
- shallow-clone boundary commits contribute no timestamp; a chunk whose
  contributing lines all blame to a boundary commit has unknown git age;
- the blame mapping is cached by blob OID plus the newest commit touching the
  file's path, so unrelated commits do not invalidate it; history rewriting
  changes the path-tip commit and invalidates correctly;
- working-tree modified lines are labelled `working_tree` and order newer
  than any committed line of the same document, without inventing a commit;
- staged-but-never-committed and untracked files have no Git authorship time
  in any mode and carry observed or unknown provenance;
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
`limit` and response-budget shedding.

Reordering rule: run `max_rank_movement` bubble passes (default 2); an
adjacent pair swaps only when the lower candidate is strictly newer under the
partial order below and both sides carry comparable provenance. A candidate
therefore rises or falls at most `max_rank_movement` positions, the bound is
enforced by construction, and base relevance order is otherwise preserved.

Comparable provenance and the partial order:

- within git provenance: `working_tree` lines are newest, then latest author
  time among contributing body lines;
- within observed provenance: later observed transition beats earlier, by
  snapshot sequence;
- git-basis and observed-basis candidates are not comparable in v1 — their
  clocks differ — and never reorder against each other;
- unknown provenance participates in no reordering and receives no advantage
  or penalty.

Every hit exposes its freshness basis (`git`, `working_tree`, `observed`,
`unknown`), timestamps or observation interval, base rank, and movement.
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
sequence, freshness basis and value, base rank and movement. Result budgets
follow the same complete-response accounting as code search.

## Failure semantics

- Markdown read/parse failure: reject that file visibly; permanent per-file
  failures are corpus exclusions, retryable corpus failures leave the
  previous documentation snapshot active.
- Embedding provider absent: BM25 remains active.
- Provider failure during `docs embed`: completed cached batches are kept;
  search reports degraded vector status and uses BM25 — a previous vector
  projection is cache, never queryable as current.
- Docs database migration failure: the main database is unaffected.
- History matching ambiguity: new occurrence, never a guessed successor.

Further failure-state machinery is deferred until the base feature is in use.

## Retention and privacy

Minimal in v1; there is no configurable retention subsystem, privacy mode, or
purge command:

- the active index stores current raw Markdown only;
- the observation ledger retains retired hashes and temporal/transition
  metadata, never retired raw bodies or rendered embedding text — an author's
  deletion does not leave a recoverable copy in jscout;
- content-addressed vectors remain durable for branch/revert reuse; vectors
  carry reduced but non-zero information and this is documented rather than
  mitigated in v1;
- deleting `.jscout-docs.db` purges jscout's complete local documentation
  state.

## Delivery and acceptance

The normative delivery phases and acceptance gate live in the PLAN.md G24
entry. This document intentionally does not duplicate them.

## Validation

Deterministic tests:

- membership precedence is deterministic and visible: exclude beats include,
  ignore beats both, the hidden allowlist admits `.github`, `.claude`, and
  `.agents`, and `docs status` names the deciding rule;
- front-matter recognition, fallback-to-body, and title derivation follow the
  specified order; a rename changes no embedding identity;
- block-aligned matching: inserting one paragraph yields exactly one `added`
  occurrence and no succession rows for untouched text;
- ordinal-only continuity never occurs; ambiguous split/merge produces no
  predecessor;
- heading renames produce `context_changed` only and refresh no body
  freshness;
- oversized fence/table/list splitting is deterministic and spans are exact;
- a document with no body chunks yields exactly one searchable stub row;
- BM25 works with no provider; `--vector` errors without one; hybrid RRF is
  deterministic across insertion order;
- freshness movement never exceeds `max_rank_movement`; git and observed
  candidates never reorder against each other; unknown provenance never
  moves;
- shallow boundary commits yield unknown git age; rebase preserves author
  time and freshness ordering; the blame cache survives unrelated commits and
  invalidates on path-history rewrite;
- a code reindex leaves documentation search available and vice versa; a
  failed docs scan leaves the prior snapshot searchable;
- code and semantic snapshots, counts, and fingerprints are byte-identical
  after any docs operation.

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

- FTS column weights, chunk-size bounds, and `max_rank_movement` default are
  evaluation hypotheses.
- Whether `docs watch` ships with phase 4 or later.
- Historical search, contradiction detection, MDX, remote documentation
  sources, and author-declared supersession remain out of scope.
