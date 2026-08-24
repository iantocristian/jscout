# Markdown retrieval and temporal history proposal

- Date: 2026-08-24
- Status: proposal; no implementation milestone assigned
- Scope: a standalone repository-documentation corpus with lexical/vector
  retrieval and bounded freshness ranking

## Decision summary

Jscout should treat repository Markdown as a separate retrieval product, not as
another source extension and not as semantic memory. The proposed surface is
`jscout docs`: it owns its inventory, Markdown-aware chunks, BM25 index,
optional embedding provider, vector index, search results, snapshot, and
history ledger. Existing code indexing, code search, structural snapshots,
reconnaissance, and semantic-artifact freshness remain unchanged.

The documentation corpus should always build BM25. When a documentation
embedding provider is configured, search independently retrieves lexical and
vector candidates and fuses their ranks. A missing or disabled vector provider
therefore degrades to lexical retrieval rather than disabling documentation
search.

Freshness is a bounded ranking prior, not a claim that recency proves truth.
Git line provenance is preferred when available. Jscout also records its own
append-only observation history, which can order changes seen after the first
documentation snapshot, including changes in a non-Git repository. Baseline
content with no Git provenance has unknown authorship time and must not be
labelled as newly written merely because jscout saw it for the first time.

Normal search indexes only the current checkout. Historical chunk bodies do
not enter BM25 or vector candidate generation. Older relevant results are not
silently suppressed: freshness may order a newer result first, while result
metadata lets a caller inspect the basis and uncertainty of that decision.

## Problem and product boundary

The source inventory currently admits JavaScript and TypeScript extensions and
passes each file through the OXC parser and AST-aware chunker. Markdown has
different syntax, retrieval units, authority rules, and change semantics.
Adding `.md` to `walk::EXTENSIONS` would couple prose changes to the structural
snapshot and would ask a JavaScript parser to manufacture documentation chunks.

Semantic artifacts are also the wrong home. Workflows, cards, summaries,
concepts, and annotations are generated or agent-authored claims whose
freshness is derived from explicit code support. Repository Markdown is
authored source material. It may be stale or internally contradictory, but it
does not acquire a code-evidence chain merely by being indexed.

The boundary is therefore:

- code search answers from structurally indexed JS/TS chunks;
- semantic memory answers from evidence-backed semantic artifacts;
- documentation search answers from current repository Markdown;
- callers opt into documentation search through a separate CLI command and
  MCP tool rather than widening ordinary code search implicitly.

One SQLite database may host all three planes, but their source tables,
current-occurrence projections, vector tables, completion markers, and
snapshots remain independent.

## User-facing surface

The initial CLI surface is explicit:

```text
jscout docs index <root>
jscout docs embed <root>
jscout docs search <root> <query>
jscout docs search <root> <query> --lexical-only
jscout docs status <root>
```

`docs index` is local and deterministic. It synchronizes the current Markdown
corpus, publishes one documentation snapshot, builds the current BM25
projection, and records an observation in the history ledger. It makes no
provider request.

`docs embed` requires a documentation embedding provider and embeds only
missing current document representations for that profile. It does not reuse
the code provider implicitly: a code-oriented embedding model may be a poor
prose model, and an implicit inheritance rule could create unexpected remote
spend. Operators may deliberately configure the same endpoint and model in
both sections.

`docs search` uses BM25 unconditionally. It adds vector retrieval when the
documentation provider is configured and the current profile has a usable
index. `--lexical-only`, `--vector`, and `--no-vector` should follow the same
explicit-override rules as code search. The result reports each retrieval
stage independently.

The MCP surface should be a separate `documentation_search` tool. It returns
documentation-specific hits containing path, heading breadcrumb, line range,
content, current-document snapshot, and freshness provenance. Documentation
hits must not masquerade as structural code hits and cannot seed graph
expansion or satisfy a semantic support anchor.

A later `jscout docs watch` may keep the corpus synchronized continuously. The
history model below must work for repeated manual `docs index` calls first;
watch improves observation resolution but is not required for correctness.

## Repository configuration

The proposed repository-local settings are independent of `[embedding]` and
`[search]`:

```toml
[docs]
include = ["**/*.md"]
exclude = []
freshness = "hybrid" # off | git | observed | hybrid
half_life_days = 365
max_decay_penalty = 0.30
retain_retired_content = false

[docs.search]
vector = true
limit = 10
response_bytes = 24000

[docs.embedding]
provider = "local"
model = "BAAI/bge-m3"
revision = "5617a9f61b028005a4858fdac845db406aefb181"
```

The documentation feature is activated by a `docs` command, not by the mere
presence of Markdown in a checkout. Missing `[docs]` settings use built-in
documentation defaults once the command is invoked. Missing
`[docs.embedding]` settings leave vector retrieval unavailable while BM25
continues to work.

Inventory honors repository ignore files, hidden-file policy, deterministic
skipped directories, and configured include/exclude patterns. Version one
admits `.md` only. MDX mixes executable source and prose and requires a
separate parsing and safety decision.

The effective documentation settings and chunk-format version participate in
the documentation snapshot or embedding-document identity only. They must not
invalidate the structural snapshot, code embedding profile, semantic memory,
or reconnaissance policy.

## Markdown inventory and chunk representation

Chunk boundaries come from the current Markdown structure, never from commit
history. History annotates a chunk after parsing; it does not decide where the
chunk starts or ends. This keeps content identity stable across rebases and
history truncation.

The chunker should:

1. parse front matter separately from the document body;
2. maintain the full heading breadcrumb for every block;
3. keep fenced code blocks, tables, lists, and block quotes atomic while they
   fit the hard bound;
4. merge adjacent paragraphs under the same heading to a target size;
5. split oversized sections at block boundaries, then at paragraph/sentence
   boundaries as a bounded fallback;
6. preserve exact source byte and line spans; and
7. emit no empty navigation-only chunk.

The embedded representation is versioned and includes the document title,
heading breadcrumb, and chunk body. The path is occurrence-specific metadata
and should be supplied to an optional reranker, not folded into a
content-addressed vector key. A rename can then reuse the same vector.

```text
markdown-v1
title: Repository configuration
heading: Providers > Documentation embeddings
body:
...
```

The representation hash, rather than the raw body hash alone, keys the
documentation embedding cache. Changing heading context is a semantic change
and intentionally misses the previous vector.

## Storage model

The schema should separate immutable content, current occurrences, temporal
observations, and disposable retrieval projections. Exact names may change
during implementation; the required responsibilities are:

### `doc_snapshots`

One immutable row per successfully published documentation scan:

- monotonic local sequence;
- observation timestamp;
- corpus fingerprint;
- optional Git worktree root, checked-out commit, and commit timestamp;
- parser/chunk-format version;
- inventory and rejection counts; and
- completion state.

A failed scan never publishes a partial snapshot or advances the observation
sequence.

### `doc_chunk_contents`

Content-addressed current or retained representations:

- raw-body hash;
- versioned embedding-document hash;
- body and rendered embedding text while retention permits;
- token/byte accounting; and
- format version.

Identical content can be referenced by multiple occurrences without choosing
one representative path.

### `doc_chunk_occurrences`

The current searchable projection:

- repository-relative path;
- heading breadcrumb and same-heading ordinal;
- source byte and line range;
- current content identity;
- current freshness provenance; and
- active documentation snapshot.

Current FTS rows and vector occurrence entries reference these IDs. A full
documentation refresh may rebuild this projection while preserving immutable
content and observation history.

### `doc_chunk_observations`

Append-only validity intervals for logical occurrences:

- logical occurrence identity;
- content identity and location metadata;
- first and last snapshot in which the version was active;
- first/last observed timestamps;
- predecessor/successor link when continuity is certain or likely;
- transition reason and confidence; and
- Git provenance when available.

Validity intervals grow only when a change occurs; unchanged chunks do not add
one row per scan.

### Retrieval planes

- `doc_chunks_fts` contains current occurrences only;
- `doc_embeddings` caches vectors by documentation representation hash and
  documentation embedding profile;
- `doc_embedding_index_entries` materializes current occurrences; and
- `vec_doc_embeddings_{dimensions}` is separate from code and semantic vector
  tables.

Completion markers and consistency audits are scoped to a documentation
snapshot/profile pair. A code reindex must not make a complete documentation
vector index appear incomplete, or vice versa.

## Building jscout observation history

Jscout history is an observation ledger, not reconstructed source history. It
can prove that version B replaced version A between two successful snapshots.
It cannot prove when baseline content was authored, recover intermediate edits
while jscout was not scanning, or describe commits on branches that were never
checked out.

The first snapshot for an occurrence without Git provenance records it as
`baseline_unknown`. Its `first_seen_at` is operational metadata, not
`changed_at`; search must not call the entire repository newly authored on the
day jscout is installed.

After the baseline, a changed logical occurrence receives an observation
interval:

```text
changed after:  previous successful snapshot time
changed by:     current successful snapshot time
```

The current snapshot sequence establishes reliable local ordering even when
the exact wall-clock edit time is unknown. This supports bounded freshness in
a non-Git repository after jscout has observed at least one transition.

Continuity is recorded only with defensible evidence:

- same path, heading breadcrumb, and ordinal with a changed body: `certain`;
- an explicit watcher rename carrying the same content: `certain`;
- exact content relocated during a full scan: `likely` move;
- heading rename with exact body and unambiguous neighbors: `likely`;
- approximate text or vector similarity alone: no succession claim;
- split, merge, or ambiguous simultaneous path/heading/body changes: new
  occurrences without an asserted predecessor.

False succession is worse than missing succession because it creates a false
temporal claim. Approximate matching may be exposed diagnostically later, but
must not affect freshness until its precision is measured.

Deleting the database deletes jscout's local observation history. That is an
explicit limitation. The history must never be described as portable or as a
replacement for repository version control.

## Optional Git provenance

Git supplements the observation ledger with history from before jscout's
first scan. In `git` or `hybrid` mode, documentation indexing should:

1. detect whether the selected root belongs to a Git worktree;
2. record the checked-out `HEAD` object and its committer timestamp;
3. run one line-porcelain blame operation per changed tracked Markdown file;
4. map blamed lines onto already-produced Markdown chunks; and
5. cache that mapping by file content hash and checked-out revision.

The chunk's Git change time is the maximum committer time among substantive
body lines and heading-breadcrumb lines that contribute to its representation.
This intentionally treats a one-line factual correction as a current chunk
change. Smaller structural chunks limit the blast radius of unrelated edits.

Age is measured relative to the checked-out `HEAD` time, not the host's current
clock. The same clean checkout therefore produces the same freshness ordering
tomorrow, and an old release branch is evaluated relative to its own head
rather than being globally penalized for age.

Modified tracked lines are labelled `working_tree` and treated as newer than
their committed predecessor without inventing a commit. Untracked baseline
files have no Git authorship date and fall back to observed history in
`hybrid` mode. Git absence or a per-file best-effort provenance failure does
not fail `hybrid` indexing; it emits an explicit diagnostic and uses
observed/unknown freshness. A future `git-required` diagnostic posture may
make incomplete provenance fatal for controlled evaluations.

Filesystem modification time is never a fallback. Checkout, archive
extraction, copying, and build tools routinely rewrite it without changing the
knowledge represented by the document.

Git history is used only as metadata for current chunks. Normal indexing does
not ingest previous file revisions. Historical retrieval, if ever added, must
be a separate explicit mode whose results cannot enter current documentation
ranking.

## Retrieval and freshness ranking

Documentation retrieval has no exact-identifier or structural-expansion tier.
It produces bounded BM25 and optional vector rankings over current
occurrences, filters them to the current documentation snapshot, and combines
available rankings with reciprocal-rank fusion. An optional documentation
reranker receives path, heading breadcrumb, line range, freshness metadata,
and content.

Freshness is applied after relevance fusion and optional reranking. Applying
it before candidate retrieval would let recent irrelevant material consume the
candidate pool. Applying it only to vector scores would make lexical-only mode
behave differently.

For a chunk with a usable change time, the proposed bounded prior is:

```text
age_days = max(0, freshness_anchor - changed_at) / 86400
decay = 2 ^ (-age_days / half_life_days)
multiplier = (1 - max_decay_penalty) + max_decay_penalty * decay
final_score = relevance_score * multiplier
```

The starting defaults are a 365-day half-life and a maximum 30 percent
penalty. They are hypotheses to evaluate, not compatibility constants. The cap
prevents a recent but weakly relevant passage from defeating a substantially
better older passage. Equally relevant passages order newest first.

For observed history without an exact timestamp, the upper end of the change
interval is a conservative `changed_at`, while the documentation snapshot time
is the anchor. Baseline occurrences are neutral on the first snapshot. Once a
later transition is observed, snapshot order can establish that the new
version is newer than an unchanged baseline occurrence, but the result remains
labelled `observed` rather than `git`.

Every diagnostic hit should expose:

- base retrieval ranks and fused/reranked position;
- freshness basis: `git`, `working_tree`, `observed`, or `unknown`;
- commit and Git change time when available;
- observation interval and snapshot sequence when available;
- configured half-life, multiplier, and final ordering effect; and
- whether retired content retention is enabled.

Compact agent output may omit score arithmetic but must retain path, heading,
lines, freshness basis, and a human-readable changed/observed value.

## Conflicting information

BM25, embedding similarity, and recency do not establish logical
contradiction. A newer troubleshooting note can be less authoritative than an
older specification, and a formatting-only edit can refresh a chunk without
changing its claim. The feature must not report that it resolved a conflict
unless a future claim-level system actually identifies incompatible claims.

The initial contract is narrower:

- retrieve current passages relevant to the query;
- apply a bounded preference for better-supported recency metadata;
- keep older relevant passages eligible and visible;
- display provenance so the caller can explain why a newer passage ranked
  first; and
- never delete or hide a result merely because another result is newer.

Explicit author-declared supersession may later provide a stronger signal,
for example through validated front matter or a repository sidecar. It should
be designed independently from automatic time decay and must not be inferred
from embedding proximity.

## Snapshot, incremental indexing, and watch

Documentation publication is transactional. Inventory, reads, parsing,
history matching, and optional Git provenance complete before the active
projection changes. Permanent per-file failures are reported as visible corpus
exclusions; retryable corpus failures leave the previous documentation
snapshot active.

Repeated `docs index` calls compare file hashes and reparse only changed
Markdown. A Git revision change may require provenance refresh even when body
content is unchanged, because rebase or history replacement can change blame
metadata. That refresh affects only documentation freshness and snapshot
identity.

An eventual docs watcher classifies `.md` changes as documentation work rather
than structural work. A Markdown-only event must not rebuild code extraction,
module resolution, structural projection, reconnaissance, or checker state.
Source-only events likewise need not rescan unchanged documentation. Unknown
event shapes may conservatively schedule both phases while preserving their
separate publication boundaries.

## Retention, privacy, and deletion

Append-only raw document history can retain credentials, private incident
details, or other text after an author removes it. The default therefore
retains retired hashes, observation intervals, transition metadata, and Git
identifiers, but not retired raw bodies or rendered embedding text.

When `retain_retired_content = false`:

- raw text remains only while at least one current occurrence references it;
- retired FTS rows and vector occurrence rows are deleted at publication;
- unreferenced documentation vectors are eligible for deletion rather than
  becoming a permanent recoverable cache; and
- history diagnostics can show that a version changed without reproducing its
  deleted content.

Retaining historical bodies should require explicit configuration plus a
bounded retention policy before it ships. A purge operation must remove raw
history and orphaned vectors without damaging the current corpus or Git-owned
history outside jscout.

## Failure and fallback semantics

- Markdown parsing/read failure: reject that current input visibly and publish
  according to the same permanent/retryable boundary used by the docs plane.
- Documentation provider absent: BM25 remains active.
- Documentation provider failure during `docs embed`: keep completed cached
  batches and the previous complete vector occurrence projection; BM25 remains
  active.
- Incomplete current vector index during search: report degraded vector status
  and use BM25 rather than silently searching a stale occurrence projection.
- Git unavailable in `hybrid`: use observed/unknown history and report the
  provenance gap.
- Git unavailable in `git`: apply no freshness to affected chunks and report
  the gap; a stricter future mode may fail.
- History matching ambiguity: start a new occurrence; never guess a successor.
- Documentation database migration failure: leave structural and semantic
  planes readable and unchanged.

## Delivery sequence

### Phase 1: current Markdown and BM25

- add independent documentation configuration and schema;
- implement ignore-aware `.md` inventory and Markdown-aware chunking;
- publish documentation snapshots and current FTS projection;
- add `docs index`, `docs status`, `docs search --lexical-only`, and the MCP
  documentation-search surface;
- create the observation ledger even though first-snapshot freshness is
  neutral.

### Phase 2: documentation vectors

- add a separate documentation embedding provider/profile;
- add content-addressed documentation vector cache and current occurrence
  materialization;
- fuse BM25/vector ranks and expose retrieval status;
- add consistency audit and repair behavior scoped to documentation.

### Phase 3: Git and observed freshness

- attach line-level Git provenance to current chunks;
- publish observed transition intervals and conservative continuity links;
- apply bounded post-relevance decay with debug accounting;
- evaluate defaults before enabling freshness outside explicit docs search.

### Phase 4: incremental watch and retention controls

- add documentation-only event classification and retry coordination;
- avoid one history row per unchanged watcher generation;
- add raw-history retention bounds and explicit purge support if historical
  bodies are accepted.

Historical search, contradiction detection, MDX, remote documentation sources,
and generated claim supersession remain outside these phases.

## Validation and evaluation

### Deterministic tests

- `.md` is admitted only by documentation inventory, never source inventory;
- ignore/include/exclude policy is deterministic and visible;
- heading breadcrumbs, repeated headings, paragraphs, lists, tables, quotes,
  and fenced code blocks produce stable spans and hashes;
- path rename reuses content-addressed vectors without changing embedded text;
- BM25 works with no embedding provider;
- vector-only override fails clearly when no documentation provider exists;
- hybrid RRF is deterministic across insertion order;
- code/semantic snapshots and counts do not change after docs indexing;
- a first scan without Git provenance labels baseline authorship unknown;
- an observed body change creates one validity transition;
- unchanged scans add no version interval;
- ambiguous split/merge creates no false predecessor;
- Git edits refresh only chunks containing changed substantive lines;
- clean-checkout freshness is invariant across host date changes;
- missing Git never falls back to filesystem mtime;
- decay never exceeds the configured penalty;
- lexical-only and vector-enabled paths apply the same temporal stage;
- retired content and orphan vectors are removed under default retention; and
- a failed scan leaves the prior documentation snapshot searchable.

### Retrieval evaluation

Before accepting decay defaults, build a fixed corpus containing:

- pairs of current Markdown passages with deliberately conflicting versioned
  instructions and known change order;
- old evergreen specifications plus recent irrelevant mentions;
- changelog/release-note sections alongside canonical guides;
- formatting-only edits;
- renamed and split sections;
- dirty tracked and untracked documents;
- clean Git, shallow Git, and non-Git copies of the same corpus; and
- lexical identifiers, flags, version numbers, and conceptual prose queries.

Measure current-answer top-k recall, older-conflict visibility, evergreen
regressions, BM25-only parity, vector/hybrid lift, and the number of results
whose order changes solely because of freshness. The half-life and cap should
remain configuration hypotheses until these results show a useful operating
point.

## Open decisions

The proposal fixes the architectural boundary and history semantics but leaves
these implementation choices for review or evaluation:

- Markdown parser/library and exact target/max chunk sizes;
- whether the first release exposes `freshness = observed` separately or only
  `off`, `git`, and `hybrid`;
- default half-life and maximum penalty after retrieval evaluation;
- whether a documentation reranker is configured independently in version one;
- exact compact MCP result budget and follow-up shape;
- whether docs watch ships with freshness or in a later PR; and
- retention duration and storage format if retired raw bodies become opt-in.

## Acceptance gate

The feature is ready to implement only after review agrees that Markdown is an
independent corpus, BM25 is always available, vectors are separately
configured, normal search contains only current content, Git and jscout
observation history remain distinguishable, baseline observation is not
misrepresented as authorship time, temporal influence is bounded and
auditable, ambiguous succession creates no false history, and retired content
does not persist by default.
