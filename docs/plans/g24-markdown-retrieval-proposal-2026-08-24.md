# G24 Markdown retrieval and temporal history — design detail

- Date: 2026-08-24, revised the same day after review.
- Status: implementation contract incorporated by reference from the Proposed
  G24 entry in [PLAN.md](../../PLAN.md). It has authority only through that
  entry; PLAN.md remains the roadmap and wins any explicit conflict. Review
  findings and decisions are recorded on PR #96.
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
freshness = false
max_rank_movement = 2

[docs.database]
path = ".jscout-docs.db"

[docs.search]
vector = true
rerank = true
limit = 10
response_bytes = 24000
```

Freshness remains disabled by default until the phase-3 Git/observed
evaluation gate reports and the default is explicitly accepted. Setting it to
`true` is an evaluation override; the observation ledger is still recorded
while rank movement is disabled.

`[docs.database]` is optional; its path defaults to `.jscout-docs.db` and is
resolved relative to the indexed root. It is independent of `[database].path`.
The docs store carries SQLite application ID `0x4A53444F` (`JSDO`) and a
canonical indexed-root binding. Opening rejects a non-docs database, a docs
database bound to another root, or a docs path that resolves to the main
database by normalized path or same-file identity; literal, `..`, symlink, and
hard-link aliases do not bypass separation.
There is no `[docs.embedding]` or `[docs.reranker]` section: vectors use the
repository `[embedding]` provider, model, and service, and reranking uses the
repository `[reranker]` profile. Compatibility of a committed `[docs]` section
with pre-docs jscout binaries is explicitly not a requirement. The feature
activates only through a `docs` command, never through the mere presence of
Markdown.

## Markdown corpus specification

### Membership

The first applicable rule below is the one visible in `docs status`:

| Order | Subject | Decision |
| ----- | ------- | -------- |
| 1 | directory | `.git` or another deterministic code-plane hard skip → `hard-skip` and prune |
| 2 | entry | repository ignore match → `ignored`; a directory is pruned |
| 3 | entry | symlink → `symlink-not-followed`; a directory symlink is pruned |
| 4 | directory or file | hidden path not admitted by the exception below → `hidden-not-allowlisted`; a directory is pruned |
| 5 | file | non-UTF-8 repository-relative path → `non-utf8-path` |
| 6 | file | path does not end in exact lowercase `.md` → `unsupported-extension` |
| 7 | file | `exclude` match → `excluded` |
| 8 | file | no `include` match → `not-included` |
| 9 | file | classified permanent open/read failure → `read-error` |
| 10 | file | captured length greater than 4,194,304 bytes → `oversized` |
| 11 | file | captured contents are not UTF-8 → `non-utf8` |
| 12 | file | otherwise → `indexed` |

Retryable I/O and all configuration, discovery, and inventory failures are
corpus-level failures, not published per-file decisions. A non-UTF-8 path is
reported losslessly as base64 of its platform-native path units, accompanied
by `path_encoding=unix-bytes` or `windows-wtf16le`; any display-only escaped
path is non-authoritative.

After rule 8, the scanner opens the regular file and captures at most 4,194,305
bytes into one immutable buffer. That operation decides rules 9–11 in order;
the same buffer is then hashed and parsed. CommonMark parsing is total, and
malformed front matter becomes body text as specified below, so v1 has no
per-file parser-rejection status. An internal parser failure is corpus-level.

Exclude beats include; ignore beats both; include cannot resurrect an ignored
file in v1. Config globs are compiled with the implementation's pinned
`globset` version using `GlobBuilder::literal_separator(true)`,
`case_insensitive(false)`, and `backslash_escape(true)`, against the complete
slash-normalized UTF-8 path relative to the indexed root. `*`, `?`, and
character classes do not cross `/`, while `**` may cross path segments and may
match zero segments, so `**/*.md` includes root-level files. Brace alternation,
leading `!`, and a trailing `/` are rejected. Patterns match files only, never
directory ancestors, so a subtree exclusion is written `drafts/**`, not
`drafts/`. Invalid patterns fail configuration validation. Changing parser or
glob versions/options requires a chunk-format version bump.

"Hidden" means a repository-relative component whose first byte is ASCII `.`;
platform hidden attributes do not participate. The allowlist excuses only an
exact first component beneath the indexed root. It does not admit
`packages/app/.github` or hidden descendants such as `.github/.private`.

The walker canonicalizes the indexed root once and never follows file or
directory symlinks. Symlink entries are reported but not admitted, so they
cannot escape the root, duplicate content, or form traversal cycles. Non-UTF-8
repository-relative paths are likewise not admitted. Files larger than 4 MiB
(4,194,304 bytes, an evaluation hypothesis) are excluded from admission.
`docs status` reports the deciding rule per encountered file (`indexed`,
`ignored`, `unsupported-extension`, `excluded`, `not-included`,
`hidden-not-allowlisted`, `oversized`, `non-utf8`, `non-utf8-path`,
`symlink-not-followed`, `read-error`) and per pruned directory (`hard-skip`, `ignored`,
`hidden-not-allowlisted`, `symlink-not-followed`), without enumerating
descendants beneath pruned directories. Version one admits `.md` only; MDX
requires a separate parsing and safety decision.

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

`rendered_body` is the final deterministic body string sent to FTS after
comment removal and any bounded synthetic fence/table context has been
applied. The exact UTF-8 provider text is `rendered_body` when there is no
nearest heading, otherwise `nearest_heading + "\n\n" + rendered_body`; no
labels, format header, path, or trailing newline are added.

Embedding identity is BLAKE3 over the byte sequence
`"jscout-doc-embedding-v1\0"`, a one-byte heading-present tag (`0x00` absent,
`0x01` present), `u64::to_be_bytes(heading_utf8.len() as u64)`, the heading
UTF-8 bytes, `u64::to_be_bytes(rendered_body_utf8.len() as u64)`, and the
rendered-body UTF-8 bytes. The absent-heading case has length zero and no
heading bytes. The provider text and this hash preimage are golden-test fixtures.
Nothing else enters the
identity, so a file rename reuses every vector, an ancestor-heading or title
edit is metadata-only, an H1 rename re-embeds only chunks whose nearest heading
is that H1, and timestamps never affect vector identity. The low-weight path
column serves directory vocabulary such as `adr` or `api/v2` without coupling
renames to the vector cache. Column weights are evaluation hypotheses, not
compatibility constants.

### Front matter

- Exactly one UTF-8 BOM (`EF BB BF`) at byte zero is excluded from Markdown and
  front-matter parsing and from retrieval rendering. Full-file hashes and
  source spans remain relative to the original bytes, so parser offsets are
  translated by three bytes. U+FEFF anywhere else is ordinary content.
- After that optional BOM, front matter is recognized only when the first
  logical line is exactly `---` with no leading or trailing whitespace. The
  first later logical line exactly equal to `---` closes it; `...`, `----`,
  indented delimiters, and delimiters with trailing text do not. LF and CRLF
  are both accepted as line endings. The enclosed text must parse as YAML and
  produce a top-level mapping. A valid YAML scalar or sequence is not front
  matter and remains ordinary body text.
- Only scalar-string `title` and `description` values and a scalar string or
  sequence of scalar strings for `tags` are used; other keys or value types are
  ignored. Front-matter dates do not affect freshness in v1.
- Front matter uses YAML 1.2 Core semantics; for example, plain `yes` is a
  string while plain `true` is a boolean. Duplicate mapping keys make front
  matter malformed. The implementation pins the YAML parser version, and a
  parser/schema change requires a chunk-format version bump.
- Valid front matter is never emitted as a body chunk.
- Malformed or unterminated front matter is ordinary Markdown body text,
  reported by `docs status` as `front_matter=malformed_as_body`, not a
  rejection.

### Blocks and chunks

Markdown uses the implementation's pinned CommonMark parser with only the GFM
table extension enabled; footnotes, task-list markers, strikethrough, smart
punctuation, and other extensions are disabled. A parser or option change
requires a chunk-format version bump. Markdown parses into source-backed blocks
before size-based merging:
paragraphs, headings, lists, tables, block quotes, fenced/indented code,
visible HTML blocks, and thematic separators. Outside parser-identified fenced,
indented, and inline-code ranges, rendering removes each byte range beginning
with exact ASCII `<!--` through the first subsequent exact ASCII `-->`,
inclusive. An opener without a closer is retained as ordinary text. This
deterministic non-nesting scan also removes comments contained inside an opaque
raw-HTML block; comment-looking text in every code range remains literal.
Headings establish structure and metadata but are not independent history
occurrences; thematic separators carry no retrieval text or history occurrence
and force a chunk boundary. The ledger tracks retrieval-bearing body blocks.
Chunks never cross heading or thematic-separator boundaries. History alignment
operates on the underlying body blocks independently of target-size merging.
Retrieval chunks may regroup when blocks are inserted or removed; that may
rebuild vectors, but it does not fabricate history transitions for unchanged
blocks.

Retrieval rendering is byte-deterministic:

1. Each source-backed body block starts as its exact original source slice,
   with CRLF and lone CR normalized to LF. Only ranges emitted by the Markdown
   comment scanner above removes ranges outside fenced, indented, or inline
   code. Leading and trailing LF bytes are removed and every other byte is
   retained.
2. A merged chunk joins rendered blocks with exactly two LF bytes (`\n\n`),
   with no separator before the first or after the last block.
3. Heading text concatenates parser `Text` and inline `Code` event bytes,
   substitutes one ASCII space for each soft or hard break, ignores markup
   events, and trims leading/trailing ASCII space or tab. Thus `**API**` and
   `API` both render as `API`. Breadcrumb components and `nearest_heading` use
   this same rendering.
4. Only split oversized fenced-code and table blocks receive synthetic context.
   Every fragment, including the first, prefixes its normalized source slice.
   A fence prefix is `[fence]\n` for empty info or
   `[fence <info>]\n`, where `<info>` is the opening fence's raw info text with
   leading/trailing ASCII space or tab removed. A table prefix is `[table]\n`
   for an empty header or `[table <cell1> | <cell2> ...]\n`, with each header
   cell rendered by the heading-text rule. The prefix does not remove or alter
   syntax present in the fragment's exact source slice.

Initial deterministic bounds, all evaluation hypotheses:

```text
target:                2,400 rendered UTF-8 bytes (~600 tokens)
merge max:             4,000 rendered UTF-8 bytes (~1,000 tokens)
hard max:             24,000 final embedding-input UTF-8 bytes
heading context max:   1,024 rendered UTF-8 bytes
synthetic body max:    1,024 rendered UTF-8 bytes
token estimate:        rendered bytes / 4
```

Adjacent source blocks under the same heading are appended while the current
chunk is below `target` and the combined rendered body does not exceed the
`merge max`; otherwise a new chunk begins. A single atomic block may exceed the
`merge max` and remains whole unless its final provider text exceeds the
24,000-byte hard bound.

The hard bound applies after nearest-heading serialization and synthetic
context are added. If the nearest heading exceeds its context bound, its
largest UTF-8 prefix that leaves room for the literal `\n[heading truncated]`
is retained and the marker is appended; that bounded value is the
`nearest_heading` used by the embedding identity. The exact synthetic prefix
defined above is truncated within the synthetic-body bound using the literal
suffix `\n[context truncated]`: retain the largest UTF-8 prefix that leaves
room for that suffix, then append the suffix. The
remaining total byte budget is used for source text.

Oversized atomic blocks first split at the last block-native boundary before
that remaining bound: newline for code, row for tables, and top-level item for
lists. If no non-empty native fragment fits, every block type falls back to the
last newline before the bound and then to the last UTF-8 boundary. This covers
multiline paragraphs, quotes, HTML blocks, and oversized individual list
items. Every fragment, including the first, repeats the bounded synthetic
context in its rendered body without altering exact source spans. Fragment
spans are half-open and partition the original block with no gaps or overlap.
When a native or newline delimiter supplies the split point, the delimiter
belongs to the preceding fragment's source span; the rendering rule may remove
its terminal LF from rendered text but never from the source span. Fragment
ordinals establish order only, never historical succession.

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

- `doc_store_meta`: the docs application ID, schema version, and canonical
  indexed-root binding used to enforce store identity and repository ownership.
- `doc_snapshots`: one immutable row per successfully published scan —
  monotonic local sequence, observation timestamp, corpus fingerprint,
  optional Git worktree/commit identity with author and committer times,
  chunk-format version, inventory and rejection counts. Any corpus-level
  failure — including retryable I/O, database, transaction, configuration,
  discovery, inventory, or cancellation failure — publishes no replacement
  and does not advance the sequence. Only a classified permanent subject-local
  open/read rejection may be recorded in a successfully published
  replacement with that file omitted.
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
- `doc_vector_generations`: readiness keyed by documentation snapshot,
  embedding profile, dimensions, and chunk-format version. A readiness row is
  inserted only after every current embeddable chunk has both a cached vector
  and matching vector-index entry. Search may query vectors only through that
  exact ready generation; otherwise it reports degraded vector status and uses
  BM25.

Publication is transactional within the docs database: snapshot insertion,
replacement of every current table and FTS projection, and mutation of the
current-snapshot pointer occur in one SQLite transaction. A failure or
failpoint strictly before commit leaves the previous snapshot active. After a
crash at the commit boundary, recovery may expose either the complete previous
snapshot or the complete replacement snapshot, never a partial mixture.

Before acquisition, inventory candidates are sorted by their
slash-normalized repository-relative UTF-8 path in ascending raw UTF-8 byte
order. The fingerprint `file_count` and entries include only final `indexed`
`doc_files`, in that same order; rejected candidates have no fingerprint entry.
Each full-file hash is BLAKE3 over the exact original file bytes,
including any BOM. The corpus fingerprint is BLAKE3 over
`"jscout-doc-corpus-v1\0"`, `u64::to_be_bytes(file_count)`, then for each sorted
file `u64::to_be_bytes(path_utf8.len() as u64)`, its path bytes, and its 32-byte
full-file BLAKE3 digest. Filesystem enumeration and database insertion order
therefore cannot change the fingerprint or published ordering.

## History and continuity

Each block observation stores one lifecycle event — `baseline`, `added`,
`continued`, or `removed` — plus zero or more orthogonal change flags:

- `body_changed`: a uniquely matched predecessor has different body content;
- `context_changed`: nearest heading or other retrieval context changed;
  source-offset, ordinal, and order changes alone do not count.

A transition can therefore be both `body_changed` and `context_changed`. A
successful scan emits `removed` when a previous block is confirmed absent from
the current corpus. A classified permanent per-file open/read failure is
recorded as a visible rejection,
emits no block lifecycle event, and removes the file from the current
projection. Matching never crosses that failure gap. If the file later parses,
its blocks receive new logical-occurrence IDs with lifecycle `baseline`; their
observation time is not authorship time. A retryable corpus failure publishes
nothing and leaves the complete last-good snapshot active.

For observed provenance, a post-baseline `added` event establishes freshness
at its snapshot sequence, and `body_changed` advances it. Context-only changes
and pure reordering carry the prior freshness forward: a heading rename does
not make the underlying claim newly authored. An initial or post-gap baseline
without Git provenance has provenance `unknown`; the baseline observation's
snapshot timestamp is operational metadata, not authorship time.

Matching between two successfully parsed snapshots is conservative and
one-to-one:

1. Within each unchanged path, an exact hash that occurs once on each side
   matches directly, independent of heading text or source order.
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
   also receives `context_changed` when applicable.
5. Every other unmatched new block is `added`; every other confirmed unmatched
   old block is `removed`.

Consequently, without usable Git provenance, a pure file rename is recorded as
`removed` plus `added` and restarts observed freshness for every block at the
new path; version one accepts this false-recency trade-off because its ranking
effect is bounded by `max_rank_movement`, and the renamed-file evaluation arm
measures it.

Pure reordering updates the structural order in the current occurrence
projection but emits no ledger event in version one. Ordinal position and
document boundaries never establish continuity. An edited block receives a
predecessor only under rule 4; therefore a singleton edge edit has no
predecessor and becomes `removed` plus `added`. Duplicate content, multiple
valid monotone pairings, split/merge/reflow, or any other ambiguous case
likewise receives no predecessor. False succession is worse than missing
succession.

## Git provenance

In a Git worktree, documentation indexing records the checked-out `HEAD` and
runs one line-porcelain blame per changed tracked Markdown file against the
same immutable bytes already hashed and parsed, mapping
blamed lines onto already-produced blocks and then aggregating them into
retrieval chunks. Rules:

- both author and committer times are stored; "newest" for freshness means
  the latest author time among contributing body lines, because author time
  survives rebase and cherry-pick while committer time is rewritten to the
  integration date;
- the shallow set is read from the path returned by `git rev-parse --git-path shallow`.
  Only a blamed commit whose OID occurs in that file is a shallow
  boundary and contributes no timestamp; blame porcelain's `boundary` marker
  is not used because it also marks root commits in complete repositories;
- blame receives the captured file bytes on standard input and uses this exact
  argument order:
  `git --no-replace-objects blame --line-porcelain --no-ignore-revs-file --contents - <recorded-head> -- <path>`.
  The Git-global
  `--no-replace-objects` precedes the subcommand and the blame-specific
  `--no-ignore-revs-file` follows it. Other provenance commands also use
  `git --no-replace-objects <subcommand> ...`; ambient replace refs and
  ignore-revs configuration cannot alter attribution;
- the blame mapping cache key includes the repository-relative path, a hash of
  the exact file bytes being blamed, the newest commit touching that path as
  resolved from the recorded head, and the shallow-set fingerprint. Worktree
  edits, path-history rewriting,
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

Immediately before publication, the attempt re-reads `HEAD` and the resolved
shallow file and compares them with the recorded head and shallow-set
fingerprint. Drift from a concurrent checkout or clone deepening aborts that
attempt and retries from a new immutable corpus capture; no mixed provenance
snapshot is published.

Git history is metadata for current chunks only; previous file revisions are
never ingested.

## Freshness ordering

Freshness is a bounded reordering, not a score. The pipeline is: BM25 and
vector retrieval, reciprocal-rank fusion using the shared code-search constant
`k = 60`, the optional model reranker — which receives path, breadcrumb, and
content, and never temporal metadata — then freshness reordering, then
truncation to `limit` and response-budget shedding. The lexical component score
is `-FTS5 bm25()`, making larger values better like vector similarity. Every
component ranking and the fused ranking therefore sort descending by score and
break exact ties by normalized path in ascending raw UTF-8 byte order, then
source-byte start and end as ascending unsigned offsets, then the 32-byte
BLAKE3 digest of the exact `rendered_body` UTF-8 bytes in ascending
lexicographic byte order; reranker
score ties retain the incoming fused order. Fusion and reranking retain at
least `limit + max_rank_movement` candidates through freshness reordering
whenever that many candidates exist.

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
  `body_changed` event wins by snapshot sequence; context-only events and pure
  reordering retain the preceding observed value;
- git-basis and observed-basis candidates are not comparable in v1 — their
  clocks differ — and never reorder against each other;
- unknown provenance participates in no reordering and receives no advantage
  or penalty.

A retrieval chunk has one basis, chosen deterministically: `working_tree` when
any contributing body line has that label; otherwise `git` with the latest
usable author time among its contributing lines; otherwise `observed` with the
latest freshness-bearing block event whenever usable Git authorship is absent,
including non-Git repositories, blame failures, untracked files,
and newly staged files; and otherwise `unknown`. A chunk whose contributing
lines all map to OIDs in the resolved shallow set is `unknown` unless a later
local observed body event exists.

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

- Corpus-level failure: retryable I/O and every database, transaction,
  configuration, discovery, inventory, cancellation, or consistency-drift
  failure publish no replacement, do not advance the snapshot sequence, and
  leave the complete last-good snapshot active. Only a code-plane-classified
  permanent subject-local open/read failure may publish a visible rejection
  with that file omitted; it emits no block lifecycle event and prevents
  matching across the resulting gap.
- Embedding provider absent: BM25 remains active.
- Provider failure during `docs embed`: completed cached batches are kept;
  no readiness row is published, and search reports degraded vector status and
  uses BM25. A previous vector projection is cache, never queryable as current.
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

The delivery phases and acceptance gate live in the PLAN.md G24 entry, which
incorporates this implementation contract by reference and wins any explicit
conflict.

## Validation

Deterministic tests:

- membership precedence is deterministic and visible: exclude beats include,
  ignore beats both, the hidden allowlist admits `.github`, `.claude`, and
  `.agents`, `README.md` and `drafts/**` pin the config-glob dialect while
  `drafts/` and brace alternation are rejected, symlinks are not followed,
  a broad include still rejects non-`.md` files, non-UTF-8 paths have a lossless
  status representation, and every pairwise overlap in the ordered decision
  table reports the first deciding rule;
- front-matter recognition requires the specified top-level mapping and value
  types and exact delimiter grammar; BOM-prefixed H1/front matter is recognized
  while hashes and spans still reference original bytes; fallback-to-body and
  title derivation follow the specified order;
- golden fixtures pin the exact provider text and length-prefixed embedding-key
  preimage; a file rename changes no embedding identity;
- block-aligned matching: inserting one uniquely distinguishable paragraph
  yields exactly one `added` block observation and no events for untouched
  blocks, even when retrieval chunks regroup;
- duplicate, ordinal-only, edge, and ambiguous split/merge matches produce no
  predecessor; pure reordering emits no ledger event; combined body/context
  edits retain both applicable change flags;
- globally unique copied content and Git-detected renames receive no cross-path
  predecessor in version one;
- heading renames produce `context_changed` only; post-baseline additions and
  body changes advance observed freshness, while context-only changes and pure
  reordering do not;
- golden rendering fixtures pin CRLF normalization, `**API**` heading text,
  the two-LF merge separator, exact non-code HTML-comment scanning (including
  comments inside raw HTML), inline-code comment preservation, and exact
  fence/table prefixes on every oversized
  fragment including the first;
- every oversized block type splits deterministically, final rendered
  representations stay within the hard byte bound, and source spans are exact;
- thematic separators flush but do not enter chunks or history, a 5,000-byte
  atomic paragraph remains whole under the default bounds, and shuffled
  filesystem enumeration produces the same sorted rows and corpus fingerprint;
  permanent read rejections are absent from fingerprint entries;
- a document with no body chunks yields exactly one searchable lexical-only
  stub row;
- BM25 works with no provider; `--vector` errors without one; hybrid RRF uses
  `k = 60`, produces the expected `1 / (60 + rank)` contributions, and is
  deterministic across insertion order and exact-score ties;
- freshness movement from each candidate's recorded base rank never exceeds
  `max_rank_movement`; git and observed candidates never reorder against each
  other; unknown provenance never moves; a rank just outside `limit` can enter
  the result when the bound permits;
- OIDs listed in the resolved shallow file yield unknown git age while a
  full-clone root commit retains author time even when porcelain marks it
  `boundary`; rebase preserves author time and freshness ordering; the blame
  cache survives unrelated commits and invalidates on worktree edits,
  path-history rewrite, and clone deepening; two identical files with distinct
  path histories use distinct cache entries; configured ignore-revs and
  replacement refs do not alter attribution;
- literal, `..`, symlink, and hard-link attempts to alias the docs store with
  the main database are rejected, as is opening a docs store bound to another
  canonical root; a code reindex leaves documentation search available and
  vice versa; a retryable docs scan failure leaves the prior snapshot searchable;
- a failpoint or process kill during documentation projection construction or
  before commit leaves the complete previous snapshot searchable; a kill at
  the commit boundary recovers exactly one complete previous or replacement
  snapshot and never a partial mixture;
- a permanent per-file open/read failure is visibly rejected without emitting
  `removed` or another block lifecycle event; a later successful read starts new
  baseline logical occurrences and cannot inherit freshness across the gap;
- checkout edits after publication, including a concurrent edit during source
  resolution, never make snapshot spans resolve to wrong bytes: raw source is
  hashed and sliced from one captured immutable buffer;
- concurrent file edits are blamed from that captured buffer; concurrent
  checkout and clone-deepening drift abort and retry before publication;
- embedding failure after every possible batch, and after index materialization
  but before readiness publication, leaves cached vectors reusable but makes
  the current snapshot BM25-only;
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

- FTS column weights, target/merge/hard chunk-size bounds, the 4 MiB
  file-admission bound, and
  the `max_rank_movement` default are evaluation hypotheses.
- Historical search, contradiction detection, MDX, remote documentation
  sources, and author-declared supersession remain out of scope.
