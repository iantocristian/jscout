# G26 Rust code indexing — design detail

- Date: 2026-08-25
- Status: subordinate, non-normative detail for the G26 entry in
  [PLAN.md](../../PLAN.md); that entry wins any explicit disagreement.
- Current milestone: phases 0 and 1.

## Motivation and boundary

jscout cannot currently index its own Rust source. G26 adds Rust as the first
second-language member of the code corpus, but only after implementing the G25
format registry that prevents one kind of admission from silently enabling
unrelated consumers.

Rust phase 1 is deliberately lexical. It publishes repository files and exact
source-backed text chunks to the ordinary code FTS projection. It does not
publish vectors, definitions, occurrences, graph facts, checker inputs,
dependency inputs, resolver inputs, reconnaissance subjects, or disposable
repository file-policy rows. Later phases may enable those
capabilities independently after their own tests and evaluations pass.

## Phase 0 — implement the G25 registry

One static registry is the sole authority for:

- persisted format identity and recognized extensions;
- corpus (`code` or `docs`) and understanding tier;
- repository and dependency admission, independently;
- format-specific directory exclusion;
- parser/chunker identity and extraction-contract version;
- lexical and vector projection eligibility;
- exact-definition and exact-occurrence eligibility, independently;
- exact-occurrence scanner identity;
- graph, checker, resolver, and watch/checker-affinity eligibility.
- repository-reconnaissance membership and file-policy eligibility.

Inventory, dependency discovery, extraction dispatch, ranked-projection
routing, exact-tier queries, checker inventory, watch classification, and
resolver dispatch consume the descriptor. They may not infer a capability
from `files.corpus`, `files.format`, a filename extension, a chunk name, or an
empty projection. A repository-admission helper must not be reused as a
dependency, checker, exact-tier, or resolver predicate.

The initial capability matrix is:

| Format | Corpus | Repository | Dependency | Ranked projection | Exact definition | Exact occurrence | Checker | Checker watch affinity | Recon policy | Structural projection | Resolver |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| JavaScript | code | yes | yes | existing code lexical/vector | yes | JavaScript scanner | yes | yes | yes | existing | existing |
| TypeScript | code | yes | yes | existing code lexical/vector | yes | JavaScript scanner | yes | yes | yes | existing | existing |
| Markdown | docs | docs policy | no | existing docs lexical/vector | no | no | no | no | no | docs metadata only | none |
| MDX | docs | docs policy | no | existing docs lexical/vector | no | no | no | no | no | docs metadata only | none |
| Rust, phase 1 | code | yes | no | code lexical only | no | no | no | no | no | none | none |

The registry contains typed policies or behavior identifiers, not booleans
that callers subsequently override. JavaScript and TypeScript may share a
policy implementation while retaining distinct persisted format identities.
Markdown and MDX keep their existing include/exclude and hidden-directory
membership policy; the registry identifies them and routes them to that
policy rather than replacing it.

Extraction contracts are versioned and persisted per format. Changing the Rust
parser or chunk contract invalidates Rust extraction without invalidating
unchanged JavaScript, TypeScript, Markdown, or MDX rows. Tests must prove that
one format-version change alters the published contract identity and schedules
only files of that format for re-extraction.

### Phase 0 acceptance

A fixed JavaScript/TypeScript/Markdown/MDX fixture is indexed before and after
the registry refactor. Inventories, pre-existing canonical columns, FTS rows, vector
candidates, exact-tier results, checker membership, watch signals, graph rows,
and public query responses must be byte-identical, except for newly introduced
format-contract metadata and the phase-1 `files.parse_error_count` column. Its
zero default is asserted separately. Table-driven tests pin every descriptor and reject
duplicate format identities or extensions. Consumer tests must demonstrate
that repository admission does not imply dependency, exact-tier, checker, or
resolver admission.

## Phase 1 — Rust lexical retrieval

Exact-lowercase `.rs` is registered as `files.format='rust'` and
`files.corpus='code'` for repository inventory. Rust dependency admission is
false: dependency-origin inventories, including selected npm packages, never
admit `.rs`, and Cargo caches are outside the repository inventory. A checked-in
directory is not classified as vendored from its name alone; Cargo vendor/source
replacement discovery belongs to the phase-3 Cargo input contract. Rust chunks
enter `chunks_fts`; code-vector materialization remains disabled.

The pinned `ra_ap_syntax` parser supplies error-tolerant, lossless syntax
ranges. The phase-1 projection is limited to:

- `files`;
- unnamed `chunks` with `kind='rust_text'`;
- the corresponding `chunks_fts` rows.

Each chunk has `name=NULL`, empty symbols and scope, and an exact source-backed
byte span. Rust emits no symbols, imports, exports, refs, member calls, events,
entities, contracts, graph nodes or edges, semantic artifacts, checker inputs,
resolver inputs, reconnaissance members, or repository file-policy rows.

### Chunk contract

The chunks are sorted, non-overlapping, and form a gap-free partition of every
non-empty source file. Top-level syntax ranges are preferred boundaries;
interstitial comments, whitespace, malformed regions, and other residual text
are retained. Adjacent ranges may be coalesced toward a 4,800-byte target.
No chunk may exceed 8,000 bytes. Oversized ranges split at the last newline
before the hard bound, or at the last UTF-8 boundary when no newline exists.
CRLF is never split. Empty files emit no chunks.

For every chunk, `content.as_bytes()` equals the source slice at its stored
byte range. Line coordinates are derived from the same source and range.
Contract tests cover LF, CRLF, multibyte UTF-8, raw and byte strings, nested
block comments, lifetimes, and mid-edit syntax errors.

Parser errors do not reject the file. Recoverable errors publish all
source-backed chunks and contribute to explicit
`rust_files_with_parse_errors` and `rust_parse_error_count` diagnostics in the
index/watch outcome and human status. An extractor panic, invalid UTF-8 input,
or lossless-span invariant failure aborts publication rather than publishing a
partial snapshot. The diagnostics describe the current indexed snapshot, not
only work performed by the latest incremental run.

### Exact-tier isolation

`exact_definition_chunks` and every branch of `exact_occurrence_chunks`,
including the `chunks_fts` fallback, filter by registry eligibility. Rust is
ineligible for both in phase 1, so the JavaScript-oriented occurrence scanner
never examines Rust chunks. The absence of Rust names is not relied on as the
filter.

### Format-scoped retrieval

Code search accepts an optional plural `formats` allowlist containing registry
ids (`javascript`, `typescript`, `rust`); omission spans every registered code
format. The normalized selection intersects each producer's capability and is
applied before limits to BM25, exact definitions, exact occurrences, vectors,
reranker candidates, and exhaustive traversal. Exhaustive responses echo it in
`scope.formats`, cursor fingerprints bind it, and completeness claims state
`corpus`, `file_roles`, `origins`, `formats`, and `snapshot`. A continuation or
compatible follow-up copies an explicitly supplied formats allowlist unchanged;
it never synthesizes request filters from echoed scope.

The phase keeps one `chunks_fts` table for the code corpus. Candidate filtering
therefore does not isolate FTS5 document-frequency or average-document-length
statistics: a filtered JS/TS ranking need not be byte-identical to a Rust-free
database. The first inspected treatment showed a small residual statistics
effect after JS/TS projection, which remains pilot evidence only. Split
per-format statistics only if a prospectively judged mixed-language evaluation
shows persistent irrelevant cross-language domination on single-language-
intent queries; relevant results from another language are not domination.

### Repository, dependency, and `target` policy

`target` is not a global skipped directory. A directory named `target` whose
parent contains a repository-visible `Cargo.toml` is a Cargo-output root for
Rust admission only. Rust files below that root are excluded, while registered
JavaScript, TypeScript, Markdown, and MDX files below the same directory keep
their existing policies. A different directory component named `target`, such
as `src/target/`, does not exclude authored Rust merely because of its name.

The shared inventory may carry per-format descent or admission state to make
this deterministic without pruning traversal for every format. Repository and
dependency admission remain separate registry capabilities.

### Watch and checker isolation

An admitted `.rs` create, edit, rename, or delete schedules the same
incremental shared-inventory refresh as another repository source. Its dirty
signal has no checker affinity. Watch state therefore distinguishes paths that
require an index refresh from paths that may enter the TypeScript checker
backlog. A Rust-only generation cannot create a checker plan or provider call.
Directory and unknown events remain conservative.

Incremental Rust changes must converge to the same canonical database state as
a full refresh. JavaScript and TypeScript events retain their current refresh
and checker behavior exactly.

### Phase 1 acceptance

The committed suite must prove:

1. exact source slices and chunk invariants for LF, CRLF, multibyte UTF-8, raw
   strings, byte strings, nested comments, lifetimes, and malformed edits;
2. zero Rust rows in every projection except `files`, `chunks`, and
   `chunks_fts`;
3. zero Rust paths in dependency and checker inventories;
4. identical JavaScript/TypeScript exact-definition and exact-occurrence
   candidates and ordering before and after Rust admission;
5. `.rs` watch parity with full refresh and an empty checker-dirty set for a
   Rust-only generation;
6. Rust below a Cargo-output `target` excluded while other formats there keep
   their existing policies, and authored Rust below an unrelated `target`
   remains admitted;
7. malformed Rust searchable with current-snapshot parse diagnostics; and
8. no behavior change in existing `chunks`, `stats`, MCP, or CLI surfaces when
   their input contains no Rust.

The prospectively committed v4 protocol used clean baseline and treatment
arms. Filtered parity compares the Rust-free
baseline against the mixed index with
`formats=['javascript','typescript']` using the previously inspected v3 JS/TS
cohort. That reused cohort is a regression guard, not fresh confirmatory
evidence: file Recall@10 may not decrease, MRR may drop by at most 0.02, and
baseline top-five gold remains top-ten. Mixed relevance uses a fresh
source-only holdout, searches the default combined corpus with formats omitted,
and uses blinded pooled cross-language judgments. Each query's pool unions the
baseline and treatment top-ten files plus authored positive recall sentinels;
every pooled query-file pair must receive an explicit `0`–`3` qrel. Baseline
and treatment nDCG@10 use the same complete pool with gain `2^grade-1`.
Treatment mean nDCG@10 must be at least `0.70` and may trail baseline by no
more than `0.02`. Missing qrels invalidate v4 scoring rather than receiving
zero gain. Language representation and known-positive Recall@10 are reported
but not gated. Both arms retain 100 raw ranked chunks, deduplicate files by
first occurrence, and truncate to 10 files. Every filtered-parity raw hit must
be JavaScript or TypeScript. Each arm's query responses share one nonempty
snapshot; the arms need not share a snapshot because their indexed memberships
differ. V4 arm reports record indexing duration, database bytes, index
stdout/stderr diagnostics, and raw query results. Performance remains separate
from retrieval acceptance.

The first treatment formally failed that frozen gate because relevant new Rust
files displaced a legacy JS/TS gold file in the shared ranking. Projecting the
same inspected result to JS/TS paths stayed within the old thresholds, but that
post-hoc projection is diagnostic only. At that point, phase 1 remained
unaccepted pending a prospectively frozen replacement control and fresh
holdout queries; the preserved failure report was the governing evidence.

The v3 replacement's filtered regression arm passed: Recall@10 stayed
`1.0000`, mean MRR moved from `0.8833` to `0.8854`, and no baseline top-five
gold file left the top ten. The mixed arm formally failed at `0.5084` against
the frozen `0.70` nDCG gate. Its qrels were not actually a retrieval pool—only
68 of 240 returned top-ten slots had judgments, every judgment was positive,
and unjudged direct tests were scored zero—so the score cannot choose a storage
or fusion design. Real authored positives were also missed, so the failure
remains preserved and cannot be regraded after inspection.

V4 completed that blind pool and explicitly judged every pooled candidate, so
its formal failure is decision-grade. Filtered Recall@10 stayed `1.000000`,
mean MRR improved from `0.883333` to `0.885417`, and no baseline top-five
positive left the top ten. Format-scoped JS/TS retrieval therefore remains
validated. Mixed treatment nDCG@10 improved from the Rust-free baseline's
`0.310561` to `0.597555`, a `+0.286994` change, and authored-positive Recall@10
improved from `0.319697` to `0.550000`. Both arms had complete judgment
coverage at ten. Relevant Rust improved the combined ranking, so this result
does not support per-language FTS statistics, language quotas, or weights.

Treatment nDCG@10 nevertheless missed the frozen absolute `0.70` gate. G26
therefore advances to Phase 2a named, item-local Rust chunks. Exact definitions,
exact occurrences, Rust vectors, and module edges remain disabled. Do not add
identifier aliases to the broad phase-1 chunks; any alias experiment belongs
to the item-local projection and requires its own prospective test. The full
decision and immutable artifact hashes are recorded in
[eval/results/g26-format-scope-v4-failed-2026-08-26.md](../../eval/results/g26-format-scope-v4-failed-2026-08-26.md).

### Later vector enablement

Rust stays lexical-only until phase 2a publishes named semantic chunks;
embedding the phase-1 lossless partitions would spend provider calls on
identities that the scheduled rechunk invalidates. Phase 1 adds `format` as a
sqlite-vec partition key alongside profile and origin because KNN applies `k`
before a relational format filter. Search queries each requested origin/format
partition and merges same-profile cosine scores. Later Rust vectors reuse the
configured code embedding model and content-addressed cache; different models
require rank fusion. Only after named chunks and a Rust retrieval evaluation
pass does the registry flip Rust to `CodeLexicalAndVector`.

## Phase 2a — named Rust chunks

Phase 2a replaces the text projection with a non-overlapping partition of named
item chunks and residual unnamed chunks. It adds names for functions, structs,
enums, traits, modules, constants, statics, `macro_rules!`, and associated
items. An associated-item chunk includes bounded enclosing `impl` or `trait`
header context; doc comments attach to their item; scope is the lexical
module/type path; macro bodies remain unexpanded.

Exact definitions, exact occurrences, and Rust vectors remain disabled in
Phase 2a. Because the v4 holdout and ranking are now inspected, item-local
retrieval acceptance requires its own prospectively committed evaluation; v4
cannot be reused as confirmatory evidence. Identifier aliases are not part of
the initial projection and must never be appended to the broad phase-1 chunks.

## Phase 2b — Rust exact tiers

Rust exact definitions and a Rust-specific exact-occurrence scanner are
separate registry capabilities. Neither turns on merely because named chunks
exist. The occurrence scanner must understand Rust comments, nested block
comments, normal/raw/byte strings, character literals, raw identifiers, and
lifetimes. The JavaScript scanner remains JavaScript/TypeScript-only.

Before either exact capability ships, a committed collision fixture contains
at least eight Rust and eight JavaScript/TypeScript definitions for each of
`new`, `from`, and `default`. When both formats are eligible, each must appear
within the first four definition candidates and neither may consume the whole
definition allowance. A Rust-absent fixture must retain byte-identical existing
exact results. The same fixture proves that `formats=['javascript','typescript']`
and `formats=['rust']` independently constrain exact candidates before their
limits. The phase-1 retrieval thresholds remain gates.

## Phase 3 — Rust module edges and Cargo lifecycle

Local Rust resolution covers declared module structure and explicit `use`
paths only. Trait selection, method dispatch, macro expansion, generated code,
build-script output, and rust-analyzer semantics remain out of scope.

Cargo discovery runs once per discovered workspace root with the equivalent
of:

```text
cargo metadata --format-version 1 --no-deps --offline --locked \\
  --manifest-path <absolute Cargo.toml>
```

It may not access the network, update a lockfile, write repository files, or
index dependency sources. Missing Cargo, a missing or outdated lockfile, a
non-zero exit, timeout, or malformed JSON is a visible degraded result: Rust
lexical/named rows still publish, metadata-derived edges for that workspace
are empty, and status records the workspace and reason. Failed metadata from
an earlier snapshot is never carried forward.

The Cargo input fingerprint contains sorted path-and-byte hashes for every
observed workspace and member `Cargo.toml`, `Cargo.lock`, `.cargo/config`,
`.cargo/config.toml`, `rust-toolchain`, and `rust-toolchain.toml`, plus the Cargo
executable version and normalized metadata output used for projection. It
participates in projection reuse and published snapshot identity.

Creating, modifying, renaming, or deleting any listed input is a watch refresh
boundary. A successful refresh recomputes all Rust module edges atomically. A
Rust source edit may remain incremental but must rebuild its derived module
projection before publication.

Committed single-crate, nested-module, workspace, renamed-module,
path-attribute, broken-manifest, unavailable-Cargo, and locked/offline fixtures
pin the exact expected edge sets and degraded states. Incremental and full
refreshes must produce identical canonical rows, module edges, status, and
snapshot identity.

## Out of scope

Dependency crates, checker facts, entities, events, member calls, macro
expansion, build-script output, trait or method resolution, procedural-macro
output, and a rust-analyzer sidecar are separate goals. Inline `#[cfg(test)]`
modules retain their containing file role and are reported as a known
role-granularity limitation.
