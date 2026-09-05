[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# Advanced workflows and architecture

Start with the [quickstart](../README.md#getting-started). This guide retains
operational details, model-assisted workflows, and implementation contracts.
[PLAN.md](../PLAN.md) remains the architecture and roadmap authority.

- [Repository reconnaissance](#repository-reconnaissance)
- [TypeScript checker enrichment](#typescript-checker-enrichment)
- [Watcher lifecycle](#watcher-lifecycle)
- [Semantic scouting](#semantic-scouting)
- [Semantic retrieval](#semantic-retrieval)
- [Call-site queries](#call-site-queries)
- [Search anchors and expansion](#search-anchors-and-expansion)
- [Confidence tiers](#confidence-tiers)
- [Scoped dependency indexing](#scoped-dependency-indexing)
- [Storage](#storage)
- [Gateway execution and indexing failures](#gateway-execution-and-indexing-failures)
- [Building release packages](#building-release-packages)

## Repository reconnaissance

`jscout scout repository <root> --max-calls N|all` is the explicit G13 pass between
the neutral structural index and optional expensive work. It classifies exact
workspace packages, unowned directory areas, and configured TypeScript/JavaScript
projects as `runtime`, `tooling`, `documentation`, `test`, `generated`, `mixed`,
or `unknown`. The model receives manifests/configuration, aggregate file kinds,
and bounded representative outlines/imports/exports/entities. Ambiguous
documentation/unknown path labels remain hidden. Whole-scope counts expose
only the high-precision artifact surfaces `handwritten`, `test`, `fixture`,
and `generated`, so the scout can recognize material outside the representative
sample without treating a directory name as semantic truth.

Run the deterministic inspection first:

```bash
jscout scout repository /path/to/repo --max-calls all --max-subjects all --warn-subjects 512 --dry-run
jscout scout repository /path/to/repo --max-calls all --max-subjects all --warn-subjects 512
jscout embed /path/to/repo --product # with embedding.provider configured
jscout enrich /path/to/repo
```

The dry run prints every initial subject, the exact evidence pack and request
size, exact model-policy reuse status, downstream decision, possible children,
and depth/subject/context bounds. It starts the checker inventory and the local
LLM gateway to resolve the same endpoint/model fingerprint execution will use,
but makes no provider generation calls. `reusable: true` items have
`would_call: false` and do not consume the reported `calls_planned` budget.

`mixed` package/area results subdivide deterministically into immediate child
directories plus a direct-file residual. One command shares `--max-calls`,
`--max-subjects` (default `all`; both limits accept `all`), `--max-depth`
(default 3), and
`--context-bytes` across the entire recursive plan. Reaching a bound leaves the
unresolved subject neutral; it never invents a narrower role.
`--warn-subjects N` (default 512) reports when the initial or subdivided subject
count exceeds `N` without truncating it.
Whole-scope artifact counts guide classification but do not constrain the
answer: `unknown`/`possible` is always legal, and co-located tests, fixtures, or
generated output do not by themselves make a runtime package `mixed`.
`mixed` means that the evidence supports multiple semantic purposes worth
bounded subdivision. Deterministic per-file roles protect auxiliary artifacts
whether or not subdivision is useful.
When a later scout gives a parent scope a definite role, that parent controls
the current projection and suppresses policy from descendants created by an
older `mixed` result. The descendant rows remain immutable history and can
reactivate if the parent later returns to `mixed`.

Classifications are immutable durable policy metadata, not graph facts. Their
freshness covers ordered subject membership, manifests/configs, and the bounded
representative evidence actually shown to the model; the global structural
snapshot is audit metadata only. An unrelated reindex does not stale them, an
evidence or membership change in the subject restores neutral fallback, and a
return to an identical branch fingerprint reuses the prior run without another
model call. Only fresh `likely` classifications affect defaults:

- deterministic file role and scouted scope role remain separate. The derived
  effective role protects `test`, `fixture`, and `generated` from a coarse
  runtime override; runtime may rescue ambiguous `documentation`/`unknown`,
  while auxiliary scope roles may demote otherwise production files;
- search retains every hit but penalizes auxiliary scopes unless an explicit
  `--file-role` filter is supplied;
- workflow/card automatic planning excludes auxiliary scopes and permits a
  fresh runtime decision to override an ambiguous deterministic path role;
- `embed --product` embeds fresh runtime scopes plus unclassified deterministic
  production/unknown files, while excluding fresh auxiliary scopes;
- checker project scheduling follows fresh project purpose but retains an
  auxiliary project when it is a file's sole owner.

`possible`, `mixed`, `unknown`, stale, and missing classifications are neutral.
Each classification stores the exact cited evidence objects, including the
bounded content shown to the model, so historical citations remain auditable
after the source or deterministic pack changes.
Duplicate valid citations are removed in model order and valid citations after
the first eight are truncated without another model call. Empty or unknown
citations still fail validation, and the scouting report records normalization
counts.
If policy reconciliation cannot read or validate its optional inputs during
`index`, jscout warns, clears the disposable policy projection, and keeps the
new L1 snapshot available with neutral defaults.
Scout prompt/evidence upgrades can intentionally invalidate earlier
classifications. When historical scope classifications exist but none match
current evidence, `repository_overview` reports `no_current_classifications`
and points to `jscout scout repository` instead of silently omitting the layer.
Diagnostic search JSON retains deterministic `file_role` and adds the derived
`repository_role` only when an active reconnaissance policy exists; compact
output presents the effective role without adding a second metadata field.
`jscout overview` and MCP `repository_overview` attach a bounded, explicitly
untrusted `reconnaissance` section whenever current classifications exist. It
prioritizes mixed/unknown/conflicting scopes and includes role counts, effective
file counts, scope identity, confidence, policy, conflicts, a one-line reason,
and citation count. Full explanations, citation IDs, and bounded evidence
excerpts are opt-in: pass the exact returned subject to
`--reconnaissance-subject SUBJECT --reconnaissance-detail` (MCP:
`reconnaissance_subject` plus `reconnaissance_detail=true`). Use
`--reconnaissance-limit` or MCP `reconnaissance_limit` to cap the compact
inventory; set the limit to `0` to omit reconnaissance from the response.

## TypeScript checker enrichment

`jscout enrich <root>` resolves every eligible indexed member-call occurrence
by default. The semantic question remains occurrence-specific — which
declaration owns this statically named property on this exact receiver? — but
the protocol sends bounded batches instead of one process round trip per call.
`index` stays deterministic and Node-free; `jscout watch --enrich` explicitly
opts into the same machinery after relevant changes. Enrichment never requests
diagnostics and does not replace deterministic name-matched member hubs.

Eligibility defaults to repository/workspace production and unknown-role calls
that still reach property candidates. Tests, fixtures, generated files,
documentation, and exact calls already explained by a direct deterministic
`certain`/`likely` edge are excluded. Repeat `--file`, `--package`, `--member`,
or `--role` to narrow that set. `--all` broadens other deterministic answers
and includes every synthetic inferred project for audit, but receiver
value-flow answers are never repeated through the checker. By default, a
package with a strict majority of unowned production/unknown-role source is
JS-first and its unowned default-role roots are included. In a TS-first
package, an orphan is
included only when a non-type import path reaches it from a `main`, `exports`,
`bin`, or script target. Tests, fixtures, generated files, and documentation
remain excluded from orphan scopes unless `--all` is used. Inferred roots
are grouped by nearest package and compatible compiler family, then
deterministically subdivided at a 150-root cap; imported dependencies can
therefore share one TypeScript Program without recreating the former
one-Program-per-file cost. Sharing a Program can expose ambient declarations
loaded through one root to its siblings. In the pinned grouping-only ai-pipe
parity run, 587 occurrences moved from unknown to `@types/node` declarations
while all 1,412 mapped repository fact payloads remained unchanged; on that
corpus grouping was fact-neutral, not coverage-neutral.
Node ESM scopes use NodeNext module and resolution
semantics. Node CommonJS scopes use the same paired NodeNext mode, which applies
CommonJS semantics from `.cjs`/`.cts` or `type: commonjs` while retaining modern
package `exports`/`imports` resolution. JSX scopes retain ESNext plus Bundler
resolution. Those effective options are part of each inferred scope's
configuration fingerprint, so a semantics change
cannot reuse facts produced under the previous family options. Skipped files
remain first-class in chunks, symbols, structural edges, FTS, embeddings, and
retrieval.
`--max-occurrences N` is the only occurrence-count cap and deliberately creates
partial coverage. Ordering is deterministic and spread across packages and
files within each priority tier; configured projects execute before inferred
projects within the same dirty/clean tier. The package-policy gate runs before
the operator cap, so skipped files cannot consume a capped selection.
`--dry-run` reports discovered, eligible, selected, omitted, project, and
configuration counts after one full-inventory configuration-only ownership
snapshot and does not construct a TypeScript Program. Its coverage fields
distinguish eligible files and occurrences without configured owners from
occurrences actually skipped by the default package policy.
`--full` bypasses exact-batch reuse and recomputes every selected project. It is
the manual equivalent of the watcher's periodic carry-free checker drift flush.

The default plan excludes calls whose member name has indexed namesakes only
outside effective-runtime files; `--all` bypasses that necessary anchorability
gate. Builtin-looking receivers (`console`, `JSON`, `path`, `fs`, …) are only
scheduled after ordinary receivers within the same structural tier and are
reported in `occurrences_deprioritized_builtin_receiver`. They are never
excluded: file-local scope and import spelling cannot account for project-wide
ambient declarations, lexical import shadows, or tsconfig path aliases. An
uncapped run therefore remains complete. A plan with no eligible occurrences
is a successful no-op and does not launch the checker sidecar. The sidecar
labels every returned declaration's provenance (`repo`, `types`, `lib`,
`vendored`, `outside`); non-repository
declarations skip the anchoring lookup but still count as unmapped, so
confidences are unchanged and the report attributes refusals by provenance in
`unmapped_declaration_contexts`.
When every returned declaration maps, a closed set of one to three targets is
published at `likely`; four or more targets, or any unmapped declaration, keeps
the set at `possible`. Every projected checker edge records `candidateCount`
for the targets that survive current path and target-fingerprint validation.

Configured projects start with a deterministic purpose classification. Explicit
lint configurations such as `tsconfig.eslint.json` are removed from a file's
ownership set when a non-tooling project still owns that file; they remain as
fallback owners for otherwise-unowned files. This avoids re-querying an
aggregate lint program without silently dropping its unique coverage.
Fresh likely repository reconnaissance can override that bootstrap purpose;
membership/config fingerprint drift restores the deterministic fallback.
`--dry-run` reports selected, excluded, and fallback occurrence counts per
affected project. `jscout checker doctor` prints each project's purpose and the
evidence used to classify it. Generic `noEmit` configurations are not treated
as tooling without an independent lint signal.

The sidecar prefers the repository's installed `typescript`; otherwise it uses
the pinned bundled fallback. `jscout checker doctor <root>` reports that choice,
every discovered owning `tsconfig`, and configuration-read problems. Query
paths, indexed BLAKE3 hashes, and exact call/receiver/property byte spans are
verified before checker work. A reverse ownership index assigns planned files
once. A file owned by multiple projects is queried in all of them. Conflicting
declarations remain separate `possible` candidates;
one mapped declaration becomes an occurrence-specific `likely` edge with
`checker` provenance. `any`, error, and unknown receiver types publish no edge.

If another owning project returns `unknown`, that incomplete answer does not
demote an otherwise clean, agreeing resolution. The edge's `detail_json`
reports the incomplete owner under `unknownProjects`, and the enrichment report
lists aggregate `unknown_projects`. Ambiguity from a resolved answer — multiple
targets or a declaration jscout cannot map — still makes every survivor
`possible`.

Rust schedules one selected project at a time, configured projects first. Its
disposable Node worker constructs one TypeScript Program, resolves batches of
at most 128 calls (the protocol accepts at most 512 and enforces 1 MiB
request/response frames), rehashes the exact project input manifest without
rebuilding the Program, and then exits so its heap is reclaimed before the next
project starts.

Results are committed to SQLite staging after every successful batch. The run
key includes the code digest, deterministic plan, TypeScript identity,
and checker protocol. A killed Rust process leaves those rows non-public;
rerunning the same command resumes the missing occurrence/project pairs. A
controlled project failure activates only completed projects, marks the failed
owner in coverage, and forces affected targeted edges to `possible`; that same
batch remains the resume target. After every project completes, Rust rechecks
the code digest, distinct checker inputs, and mapped target
fingerprints, publishes the complete canonical batch, and drops the superseded
batch. A code-publication race remains staged and publishes nothing.
Each request has a hard deadline (`--timeout`, default 300 seconds); timeout
kills only the current project worker. Progress names the project and reports
staged occurrences plus Node RSS and heap usage. Worker crashes return the
actual Node error/stack in the command error as well as stderr.

### Checker code-digest lifecycle

Every checker batch is bound to exactly one code digest. Projection accepts it
only when `source_snapshot` matches. Manual `jscout index` preserves or rebinds
an active publication only when the code digest, checker contracts, canonical
checker inputs, module resolution, and on-disk inputs all still validate;
otherwise it clears the checker plane.

`tsconfig` files are checker invalidation boundaries, not code-digest inputs.
Editing or deleting one can therefore leave the reported code digest unchanged.
In watch mode the boundary event still starts a
generation, and the checker planner's configuration-chain and membership
fingerprints determine which projects must run again; code-digest equality alone
does not imply that checker configuration was unchanged.

`jscout watch --enrich` performs the cycle: reindex first, then run the same
project-batched, resumable checker pass for the resulting snapshot. Across a
changed snapshot it first considers each fully completed project in the newest
reusable superseded staging source, then falls back project-by-project to the
active publication. An empty newer destination left by a crash cannot displace
that source. Pending, partial, failed, or coverage-incomplete staging rows are
never carried. Configuration chain, membership, checker identity, protocol,
external inputs, and exact occurrence/target fingerprints are all revalidated.
If any owner of a multi-project occurrence cannot carry, all owners are
re-queried. Dirty projects and occurrences run first; fully carried projects
construct no TypeScript Program. Validated copies and predecessor-staging
retirement commit atomically, and neither old source is projected for the new
snapshot.

Source dirty affinity is a watcher backlog rather than generation-local state.
It accumulates code paths across supersession, cancellation, retries, and
terminal partial publication, and clears only after a non-superseded checker
publication succeeds. Documentation changes remain refresh-only. Enrichment
reports distinguish exact-batch reuse, durable staging resume, staging resets,
unique carried occurrences, project-occurrence carry, and carry sourced from
superseded staging versus the active publication.

External checker inputs remain watched for carried projects. An independent
daily-scale deadline schedules `enrich --full` semantics inside the watcher;
this is separate from the default ten-minute structural reconciliation. If a
source event supersedes that generation, the carry-free requirement follows
the successor instead of being dropped. Plain `watch` never starts Node and
never projects an old-snapshot batch.

Watch maintains structural state and, when explicitly enabled, checker facts
plus code and semantic vector indexes. It does not generate semantic content:
repository scouting, cards, workflows, summaries, and concepts remain explicit
`jscout scout` operations. With both optional planes enabled, the phase order is
`refresh -> embed(code) -> enrich -> embed(semantic)`; without `--enrich`, the
semantic tail follows code embedding immediately. The tail absorbs artifacts
written by prior manual scout or agent-annotation operations and repairs their
semantic vector index from the durable cache. The complete manual enrichment
sequence remains:

```bash
jscout index .
jscout enrich .
jscout scout repository . --max-calls all
jscout embed . --product --semantic
```

### Watcher lifecycle

`jscout watch` subscribes before its startup pass and begins with the same full
disposable-snapshot refresh as `jscout index`. A bounded batch containing only
indexed JavaScript/TypeScript source paths then uses incremental extraction: it
walks and hashes the complete shared code-and-document inventory, but parses
and replaces only changed or missing files. Admitted Markdown/MDX changes use
the same incremental refresh without entering checker dirty affinity. Periodic
reconciliation also runs this complete-inventory incremental path, so it still
repairs missed create/delete/ignore transitions without rebuilding unchanged
rows. Startup, more than 256 changed source paths, Git/submodule controls,
source-inventory ignore files,
package/workspace manifests, lockfiles, tsconfig/jsconfig or declaration
inputs, selected dependency/checker inputs, pathless events, and backend errors
use full refresh. Non-boundary directory and uncertain missing-path events use
the same complete-inventory incremental path, because their paths are not the
correctness inventory. Full scope is sticky within a coalesced generation.

Both refresh modes rerun dependency ownership, module resolution, plane-digest
calculation, vector occurrence rematerialization, and structural projection as
needed. Either mode retains a checker publication only after exact code-digest
and input validation. Watch may additionally keep the active publication plus
the newest reusable superseded staging source hidden as inputs to the following
validated carry pass. A
deterministic extraction rejection or non-retryable read failure is reported
and excluded; an old row for that path is not served as current. The refresh
still succeeds over the indexable corpus.
The classified workspace map is built first. Workspace globs are expanded
against the filesystem, so a declared package keeps first-party identity even
when it contains only excluded build output or gitignored source. Indexed
sources are an alias-target preference, not a membership gate; when no indexed
source mapping exists, a classified manifest-entry lookup preserves the
declared alias. First-party extraction, current-import dependency discovery,
and every selected-dependency source read then complete in one rollbackable
transaction before the old snapshot publication is invalidated. A retryable
acquisition failure therefore leaves the previous snapshot queryable until a
complete replacement can commit.

Compatibility note: repositories indexed by the brief source-derived
workspace-discovery implementation get a one-time resolution-identity change
when source-less members return to the workspace map. In watch, the resulting
membership-fingerprint change forces the affected checker projects to run once.
The default two-second trailing quiet period coalesces edits; an event received
during any phase advances the desired generation and cannot be consumed by the
phase already running.

Each phase opens and closes its own database connection with a finite SQLite
busy timeout. Fatal refresh, embedding, and checker errors retry indefinitely
without waiting for another edit, with an exponential delay capped at 30
seconds. Recognized transient read failures such as descriptor exhaustion,
interrupted/network I/O, or stale handles are phase errors: the transaction
rolls back and watch retries instead of publishing a reduced corpus. A path
that disappears or changes between file and directory after inventory is
ordinary checkout churn, not evidence of an atomic-snapshot violation; its old
row is removed and later events or reconciliation converge on the next state.
Checker enrichment may publish a partial batch when some projects fail. A
transient project failure uses the phase retry loop; a partial batch containing
only deterministic project failures completes the generation with
`status=partial` and those projects are attempted again after the next source
generation or periodic reconciliation. Published partial batches remain
immutable; retryable rows are cloned into inactive staging before another
checker is launched. A checker worker or whole sidecar
process crash/exit is project-terminal: successful project staging is retained
and the crashed project follows that generation/reconciliation recovery path
instead of an uncapped immediate crash loop. Recognized launch, request,
transport, and resource failures remain immediately retryable.
Repository traversal applies the same classifier at subtree granularity:
retryable I/O aborts the phase, while a permanently inaccessible subtree is
reported and excluded without losing accessible siblings. Attached
`.gitignore`/`.ignore` errors are surfaced rather than discarded. Selected-
dependency traversal is a phase error because that explicitly requested
package inventory is planned as one bounded unit.
Non-retryable file reads and deterministic extraction failures are rejected
inputs. They do not degrade a refresh or trigger whole-repository retries, and
their path, stage, and error remain visible in every manual index report.
The watcher prints the full details once per distinct rejection set, reports
when the set clears, and retains `rejected=N` in every refresh summary. The
default ten-minute reconciliation pass naturally attempts those paths again
while also repairing missed notifications. Its interval is measured from
completion of the previous generation, avoiding back-to-back refreshes when a
cycle itself is slow; a nonzero interval must be greater than the debounce
period.
Set `--reconcile-seconds 0` only when giving up that bounded recovery is
acceptable; it does not disable phase-error retries.

An external dependency/checker path that cannot be registered with the native
filesystem watcher is marked as degraded coverage immediately. Registration is
attempted again on later target reconciliation; it does not have a separate
retry loop.

Database/WAL/SHM writes are excluded by exact path. For long-running watch,
prefer an external `--database` path (or ensure the selected database family is
gitignored) so broad staging commands cannot add it. Git HEAD,
`.gitmodules`, selected dependency roots and locators, and external inputs from
the latest checker batch are watched as additional invalidation boundaries.
Branch switches therefore rebuild the complete current file set. Extraction,
resolution, snapshot calculation, and graph publication commit in one SQLite
transaction. Concurrent WAL readers keep seeing the last committed publication
while the replacement is built; a failed refresh rolls back without exposing a
half-built index. A repository still needs one successful initial index before
read-only queries can start.

## Semantic scouting

`jscout scout repository`, `workflows`, `cards`, `summaries`, and `concepts` make
schema-constrained model calls through the bundled pi-ai gateway. Workflow and
concept runs additionally require exhaustive candidate classification.
Generative calls default to
`openai-codex:gpt-5.6-terra`, which uses the ChatGPT-plan OAuth path; `--model`
is the explicit per-call override and `llm.model` is the repository default.
The old `JSCOUT_LLM_MODEL` remains a warned compatibility fallback. Request
policy is explicit per command: `--timeout` (default 300 s per request),
`--max-calls` (a hard
command-level budget), `--context-bytes` (default 240 000 serialized evidence
bytes, also checked against the selected model's context window),
`--reasoning`, `--service-tier` (rejected where the provider API does not
support tiers), and `--rebuild` (supersede a completed identical run instead
of reusing it). See [Configuration](configuration.md) for the complete
environment surface.

Without `--seed`, scouting derives bounded seeds from routes, GraphQL
operations, runtime handlers/producers, lifecycle/job/DI boundaries, and
exported package/application entry files. Export seeding is deliberately
limited to manifest-resolved entries and conventional `index`, `main`,
`server`, `app`, or `entry` filenames. Routes/GraphQL/handlers rank ahead of
producers and dispatchers, which rank ahead of DI injection sites. Equal
deterministic candidate fingerprints are collapsed before any call. Automatic
mode requires an explicit `--max-calls`; completed matching runs are reused
before that budget is spent, and one over-budget boundary is reported and
skipped without blocking smaller boundaries. `--dry-run` prints the exact
resolved seeds, candidate fingerprints, candidate counts, evidence file
counts, and evidence bytes without starting Node or contacting a model.

`jscout scout cards` writes one evidence-backed card per selected symbol.
Without `--anchor`, subjects are the union of exported production symbols,
runtime boundary endpoints, and participants of current published workflows,
deduped by anchor and capped at 1024. Selection is allocated round-robin across
current reconnaissance scopes (or deterministic top-level structural scopes)
before repeating a scope. The plan reports discovered, selected, and omitted
subjects per scope so the cap cannot be mistaken for repository coverage.
`--anchor` selects subjects explicitly — each resolves uniquely to a symbol,
like a workflow seed, and each becomes its own run. Repeatable `--file` and
`--subject` selectors target symbols only inside an exact indexed file or one
current `repository_overview` reconnaissance subject; they never widen when
the selected surface has no eligible symbol. Automatic and file/subject modes
require `--max-calls`; anchor-only mode defaults it to the number of anchors.
Evidence is the subject's declaring file plus its deterministic depth-1 edges,
which the prompt forbids restating: a card carries purpose, architectural role,
domain terms, side effects, invariants, and failure modes, never signatures or
call lists. Every individual claim cites its own line ranges in that file and
is published as its own support at `likely` confidence; an optional field the
model cannot support is omitted rather than guessed, and a claim without exact
evidence fails the run instead of downgrading it. `--dry-run` prints the
selected subjects, evidence bytes, per-item request bytes, and budget decisions
without starting Node or contacting a model.

`jscout scout summaries` writes one summary per scope, strictly bottom-up over
already-validated artifacts and never over raw source. Levels are `file`
(children: current cards and workflows), `module` (children: the file summaries
of one workspace package), and `repository` (children: module summaries plus
file summaries no package owns); omitting `--level` runs all three in that
order under a single `--max-calls` budget, so a module summary is planned from
the file summaries the same invocation just published. Every claim must cite
the enumerated child references it rests on, and each citation is published as
a `summarizes` relation pinned to that child's artifact fingerprint. Every
planned child also becomes a whole-summary input dependency, whether cited or
not — uncited prose never validates, and a scope with no current children is
not a summary subject at all. Publication rechecks inside the transaction that
the scope still has exactly the planned child set and that every child remains
current with its pinned fingerprint, so a child added, removed, or replaced
mid-flight refuses the write rather than publishing immediately stale prose.
Freshness then propagates upward: a missing, superseded, or changed child
stales its parent, and a current-but-not-fresh child degrades it, even when the
parent's own text never changed. `--scope KEY` selects scopes explicitly and
requires `--level`, since scope keys are level-specific. `--dry-run` prints the
per-level plans, child counts, and request bytes without starting Node or
contacting a model.

`jscout scout concepts` writes one concept per exact normalized vocabulary
group. Discovery considers only current fingerprinted workflows whose `/name`
claim has exact supports and current fingerprinted cards whose string-valued
`/domain_terms/N` claim has exact supports; other body fields and unsupported
prose are excluded. The versioned normalizer applies Unicode NFKC, Unicode
lowercase, and trimmed/collapsed whitespace while preserving punctuation, so
case and compatibility variants group together but `invoice-id` and
`invoice id` do not. The model supplies a repository-specific definition and
cites deterministic child references; it cannot choose the concept identity,
invent aliases, or add children. Published aliases exhaustively reproduce the
observed NFKC/whitespace-normalized display spellings, and the concept copies
none of their spans onto newly generated prose. Instead, claim-level
`related_to` relations pin each child fingerprint and preserve the drill-down
through that child's exact source supports.

Run concept scouting after the intended workflow/card sweep. The expected
vocabulary group is recomputed on every semantic read: publishing another card
or workflow with the same normalized term intentionally stales the existing
concept until it is refreshed against the settled child set. In mixed refresh
runs, jscout enforces this order automatically. All concept planning, including
direct and dry-run commands, refuses reuse or model spend while one of the
group's children is still non-fresh.

Without `--term`, all bounded groups are planned and `--max-calls` is required.
Repeatable `--term TEXT` selects an existing group through the same normalizer
and defaults the call budget to the number of supplied terms. Oversized groups
are skipped in automatic mode and fail explicitly selected runs rather than
being truncated. Publication atomically rechecks the snapshot, child
fingerprints, exact child set, and concept lineage. Near-duplicate matching and
many-lineage merging are deliberately not implemented: punctuation, fuzzy
similarity, stemming, and embedding proximity never cause an implicit merge.
`--dry-run` prints the normalized groups, exact aliases, child/support counts,
input bytes, skips, and budget decisions without starting Node or contacting a
model.

Generated workflows record their resolved seeds, traversal limits, service
tier, model, and reasoning policy in the run ledger; cards record their
subject anchor, summaries their level and scope key, and concepts their
normalized vocabulary group the same way. After
indexing exposes source or structural-context drift, `jscout scout refresh
--max-calls N` selects current stale/degraded generated workflows, cards, and
summaries, and concepts and publishes immutable successors. A summary needs no
rule of its own here: child drift already makes it non-fresh, so it selects
naturally and is replanned against the children that are current now; a concept
is replanned from the currently supported exact vocabulary group. Index and
watch never make model calls. Runs created before replay configuration was
stored remain visible but are reported as non-refreshable; jscout does not
guess their original boundary. A stale target whose recorded seed or scope no
longer resolves is reported and skipped without blocking other refreshes.

## Semantic retrieval

`jscout memory` and the structural-profile MCP `semantic_memory` tool query
persistent memory through a vector-plus-lexical ranking plane separate from
code-chunk ranking. Semantic vectors are content-addressed by the bounded
artifact description and exact support anchors; create or refresh them after
workflow/card/summary/concept generation with
`jscout embed <root> --semantic-only` after configuring an embedding provider.
Without a
materialized semantic index, retrieval reports `vector: degraded` and falls
back to lexical matching. `--no-vector` (MCP: `vector=false`) requests lexical
matching explicitly.

Memory discovery filters by artifact type, computed freshness, direct relation,
or historical artifact id. Broad calls return compact handles with bounded
support summaries and a copy-safe `view=body` exact-artifact follow-up. Exact
artifact reads default to a type-aware compact projection: identity, trust,
freshness, a description/primary claim, and defining workflow participants.
`view=body` returns the complete body plus one compact evidence locator by
default. `view=full` returns diagnostic provenance, relations, complete selected
supports, hashes, and concept tags. Supplying an exact
anchor, indexed file, or current reconnaissance subject creates a hard evidence
scope: direct anchor support ranks before file support and scope-member support,
and unsupported lexical/vector analogies are not used as filler. An empty
localized result reports `no_supported_memory`. Bodies, relations, concept
tags, and source evidence are returned only for an exact artifact-id drill-down.
Current artifacts are the default; an exact historical id reports
`current: false` and its `superseded_by` successor. The default complete response
budget remains 24 KB.

`semantic_search` attaches compact, evidence-connected memory previews only
when requested with `--memory` (MCP: `include_memory=true`):
artifact identity, a short purpose/overview/description/claim, freshness,
and one evidence locator. An artifact is eligible only when
its support is in a returned hit or enclosing file, within the bounded
likely/certain memory graph path, or directly related to an artifact connected
that way. Text/vector similarity ranks candidates inside those tiers; it cannot
promote an unrelated generic card over direct evidence. `memory_depth` and
`memory_nodes` (CLI: `--memory-depth`, `--memory-nodes`) are explicit,
widenable bounds. `attachment.status: "no_connected_memory"` means the broad
semantic candidate pool had no evidence connection to the returned code; use
`semantic_memory` for unconstrained discovery. Full bodies, relations, and
evidence always belong to `semantic_memory`.
Annotations preview their required `claim` field. Normal compact transport omits
successful retrieval stages, candidate-pool sizes, scores, and successful
attachment traversal statistics. Degraded stages, `no_connected_memory`,
truncation, actionable omission counts, and the follow-up tool remain visible.
The complete diagnostics remain available with `debug=true` and in telemetry;
rank, lexical-score, and vector-cosine values are diagnostic signals, not
calibrated probabilities.

Compact code hits carry copy-safe symbol anchors without per-hit follow-up
objects. Pair one anchor with the response's top-level `snapshot` when calling
`definition`, `who_uses`, or `neighborhood`; pass a file hit's path to
`file_outline`. Exact `definition`/`who_uses` anchor mode is mutually exclusive
with their human-authored fuzzy `symbol` mode and preserves same-named methods
instead of round-tripping through a lossy `path:name` shorthand.

When an interactive `annotate` call starts with a healthy semantic vector
index, jscout embeds the new document and incrementally synchronizes that
profile after committing the artifact. A failed refresh does not roll back or
duplicate the durable annotation; the response reports the degraded vector
plane and its repair action. Batch scouts still embed once after publication.

When the selected results include current, fresh concepts, the response also
contains deterministic `concept_tags`: deduplicated file-level associations
and chunk-level associations for each indexed chunk whose line range overlaps
an exact support reached through the concept's claim-level child relations.
These are derived R2 localization hints, not stored claims and not additional
model output. `--concept-tag-limit N` (MCP:
`concept_tag_limit`, default 40, maximum 200) bounds them independently; the
complete response-byte budget removes whole concept tags before source,
relations, or semantic artifacts and reports the omitted count.

Add `--source` (MCP: `include_source=true`) to follow the artifact's pinned
outgoing claim-citation relations to leaf supports. Empty-path whole-input
dependencies remain visible in the relation section but are never presented as
claim evidence. Every returned path identifies the intermediate artifact,
relation, and JSON claim pointer; distinct claims that cite the same child stay
distinct. `--source-depth` bounds relation traversal and the response reports
depth/path truncation and skipped cycles explicitly.

Source resolution uses the indexed file identity—including virtual dependency
paths—then reads disk and compares the current bytes with the indexed hash. A
disk change that has not been indexed returns `source_status: "index-stale"`.
After re-indexing changed source, an older support returns
`source_status: "source-stale"`. Neither case returns a misleading excerpt; an
unavailable file is also explicit. Structural-context drift is reported
separately as support freshness and does not hide hash-verified source bytes.

`jscout overview` and MCP `repository_overview` return the deterministic corpus
inventory from one pinned SQLite read snapshot. Generated memory is absent by
default. `--semantic`/`include_semantic=true` adds a separately labelled,
untrusted overlay containing only current artifacts whose computed freshness is
`fresh`; cards require explicit type selection. Whole-response byte budgets
drop overlay artifacts and detailed reconnaissance before deterministic
inventory and report every omission. Generated prose never changes search
scores.

## Call-site queries

`jscout calls` answers "where is this option passed to this method?" with
exact AST matching instead of line-based text joins:

```bash
jscout calls /path/to/repo insert --arg merge=replace --receiver wave.card --json
```

Candidate files come from the index (member-call names plus full-text
argument tokens); matches re-parse those files, so every hit reports the
complete call span — a multiline call owns every line inside it — the
static receiver chain (`dbs.wave.card`), the matched argument position, and
the innermost enclosing declaration anchor. All `--arg KEY[=VALUE]` filters
must match top-level literal properties of the same object-literal argument;
`--arg-position` pins which argument that is. Candidate files are
hash-verified against disk, and drift fails the query instead of answering
from a stale index. The calls matcher itself does not resolve receiver
identity. Use graph queries or enrichment to inspect occurrence-specific
receiver-value-flow/checker edges and property-hub candidates; the matcher
never silently guesses an implementation. The same query is exposed as the
`calls` MCP tool.

## Search anchors and expansion

Search returns a code snapshot, retrieval-stage status, and ranked hits.
`retrieval.vector` is `active`, `disabled`, or `degraded`; degraded means the
requested vector stage failed and the returned ranking is lexical-only. This
status is present in compact CLI/MCP and full diagnostic JSON, so an agent does
not have to infer vector availability from stderr. Every hit includes a
`file_role`. Structurally eligible JavaScript/TypeScript hits also include a
`file_anchor` and one or more code-snapshot-scoped `anchors` projected from the
chunk's overlapping declarations, falling back to the file anchor. Rust lexical
hits omit structural anchors and graph follow-ups. Roles are deterministic:
`production`, `test`, `fixture`, `generated`, `documentation`, or `unknown`.
Use repeatable `--file-role` flags to restrict primary hits. Chunks remain
retrieval units; they do not become graph identity.

Code search spans JavaScript, TypeScript, and Rust by default. Use repeatable
`--format javascript`, `--format typescript`, or `--format rust` flags to scope
primary retrieval before candidate limits; the MCP `semantic_search` surface
uses the equivalent plural `formats` array. The formats share one lexical FTS
corpus, so the filter isolates candidates but not BM25 corpus statistics. Rust
is lexical-only until its named-chunk phase; a Rust-only search does not invoke
the embedding provider for code retrieval, although vector-enabled attached
semantic memory still uses the shared provider. Rust parsing follows the
package edition declared by the nearest visible `Cargo.toml`, including
workspace-inherited editions.
An explicit `package.workspace` pointer is authoritative; ancestor workspace
discovery is used only when that key is absent. Malformed or missing explicit
targets are reported and recover to the default. Packages without an edition
and standalone files use Rust 2015. Non-UTF-8 Rust and
deterministic extraction failures are reported and rejected per file without
blocking accessible repository siblings.

Structural expansion is off by default and does not alter search scores. Add
`--expand` to attach a separately labelled context pack:

```bash
jscout search /path/to/repo "checkout inventory" --json --expand \
  --response-bytes 24000 \
  --expand-mode paths --expand-depth 1 --expand-seeds 3 --expand-paths 8 \
  --expand-nodes 40 --expand-edges 120 --expand-bytes 24000
```

Compact expansion defaults to a ranked path forest rooted at the selected hit
anchors. It keeps the shared prefixes needed to reach cross-file symbols,
runtime hubs, handlers, state transitions, and effects instead of returning
every incident edge in the induced neighborhood. `--expand-paths` bounds the
number of ranked continuation endpoints. Depth one is the same compact one-hop
caller/callee projection. Use `--expand-mode neighborhood` only when the full
diagnostic neighborhood is actually needed. Both modes report omitted
path/node/edge counts and retain the existing independently widenable limits.
Under identical limits, diagnostic neighborhood reserves the selected compact
path forest before filling the remaining budget with ranked fan-out, so it is a
strict superset of the path projection rather than a competing bounded sample.

Expansion defaults to `production` and `unknown` file-backed nodes while
retaining structural hubs. Use repeatable `--expand-file-role` flags to opt
tests, fixtures, generated files, or documentation back in. Explicitly
included non-production nodes receive deterministic ranking penalties before
the traversal and global node/byte budgets are consumed.

`--json` and the MCP tools use compact, minified agent transport. It retains
source locations, symbols, snippets, anchors, graph direction, confidence,
provenance, and checker receiver types while omitting occurrence IDs, raw
diagnostic metadata, empty fields, and repeated defaults. Search
`--debug-json`, neighborhood `--debug-json`, and MCP `debug: true` retain the
full diagnostic representation.

Ranked code-search snippets are query-aware source excerpts: at most eight
source lines and 1 KiB (1,024 UTF-8 bytes), with ellipses when byte clipping is
needed.
Exact-tier hits prioritize their matched identifiers; other hits select a
window of literal query matches using the index's FTS tokenizer. A hit with
no literal source match keeps a bounded header excerpt. `at` still locates
the whole retrieval chunk; optional `snippet_line` gives the first excerpt
line when it differs from the chunk start. A leading ellipsis can indicate
a mid-line start. The source-byte cap does not replace the complete JSON
response budget, and `snippet_truncated` continues to indicate additional
response-budget truncation. Use the unchanged anchor with `definition` for
complete source. Exhaustive search continues to return match lines, not
snippets.

CLI `--debug-json` is not outer-response-budgeted when `--response-bytes` is
omitted, so inspecting diagnostics cannot silently remove graph nodes or edges.
Pass `--response-bytes` explicitly to test diagnostic truncation. Compact CLI
and MCP responses retain their configured complete-response budgets.

Code search defaults to a 30,000-byte complete JSON response ceiling, covering
hits, attached context, metadata, and JSON overhead. Override it in
`[search].response_bytes`, with CLI `--response-bytes`, or per MCP
`semantic_search` call with `response_bytes`; explicit limits still win.
Documentation search and other tools keep their existing response budgets.

For exact drill-down, construct arguments from a hit's `anchor` and the
response's top-level `snapshot`. Preserve any explicit `origins` and `formats`
from the originating search request for `definition` and `who_uses`;
`neighborhood` does not accept a format filter. Ambiguous chunks expose
`anchors` for the caller to choose from, and file-only hits retain their `at`
locator. Supplying the snapshot makes stale anchors re-resolve by
path/scope/name or fail closed instead of silently binding to a same-named
declaration.

An explicit `--response-bytes` caps whichever complete JSON representation was requested:
hits, expansion, budget metadata, and serialization overhead. The result
reports its actual `rendered_bytes`, original `unbudgeted_bytes`, and omitted
content. Search semantic-memory previews share a global eight-support cap.
When the budget binds, optional memory is shed first, then low-ranked graph
relations and their unused nodes, then lower-ranked code hits; the top code hit
is never silently removed. Expansion admits an edge together with both endpoint
nodes so a node-only context pack cannot consume the relation budget. The
expansion node, edge, and payload limits are subordinate budgets shared across
all search-hit seeds. `--expand-min-confidence` defaults to `likely`;
use `possible` only when explicit unresolved candidates are useful.

Evidence-connected semantic memory is opt-in for CLI and structural-profile
search; use `--memory` when a preview connected to localized code would help,
then use `--memory-limit` and
`--memory-depth`/`--memory-nodes` to widen its reported structural join bounds.
Every artifact carries evidence supports and a computed `fresh`, `degraded`,
or `stale` label. The complete response-byte limit includes semantic artifacts.

`jscout index` reparses the current checkout instead of carrying cheap
structural rows across snapshots. It preserves content-hash embedding cache
rows, semantic memory, and immutable repository-reconnaissance history, then
rematerializes current vector occurrences and exact fresh reconnaissance policy
from those durable planes. Checker enrichment is code-digest-bound: manual
index and watch may reuse an exact batch or validate and rebind unchanged facts
into the newly published code digest; invalid checker state is cleared. Run
`jscout enrich` when no validated occurrence-specific checker edges remain.
`jscout watch` coordinates full convergence and bounded incremental source
refreshes with optional embedding/checker operations, debounce, retries, and
periodic complete-inventory incremental reconciliation. `watch --embed` updates the default corpus;
`watch --embed --product` applies the same fresh reconnaissance policy and
neutral production fallback as `jscout embed --product`, so it does not widen a
product-only vector cache. Each embedding phase reports missing documents,
newly embedded documents, durable-cache reuse, and current vector occurrences;
a fully cached pass therefore reports reuse rather than `embedded=0/0`.
`watch --embed` also repairs and tops up semantic-artifact vectors after the
checker phase. Manual `jscout index` always remains a full disposable-snapshot
rebuild.
The structural nearest file-form `.git` indirection, worktree `HEAD`, and
`.gitmodules` controls remain watched even when documentation freshness is
disabled. Worktree-index, reference/log, shallow, reftable, and Git conversion
controls are provenance-specific and are subscribed only while freshness
indexing is enabled.
The startup line records the jscout version, executable-byte fingerprint,
loaded non-secret runtime-config fingerprint, whether a config file was loaded,
the documentation-indexing-only reload boundary, checker-policy fingerprint
derived from the actual watcher enrichment selection, and a separate non-secret
fingerprint of the effective watch invocation after CLI overrides alongside the
effective watch flags. These identities make logs from different binaries or
policies comparable without exposing credentials. If the running executable
cannot be read, jscout warns and records `binary_fingerprint=unavailable`;
diagnostic identity never prevents watch or MCP startup.
Retrieval-only CLI commands and MCP sessions open an existing published index
read-only: they do not create `.jscout.db` or migrate an old schema. The MCP
server opens a writer lazily only when its `annotate` tool is selected.
The old per-version migration ladder has been removed. Writer commands accept
the v16+ durable format by preserving embedding/semantic-memory/reconnaissance tables and
recreating all disposable snapshot tables once. Older durable formats are
rejected; preserve such a file before creating a fresh current database.

## Confidence tiers

- **certain** — resolved through binding analysis + Node module resolution (incl. package.json `exports`, tsconfig `paths`, barrel/star re-exports, CommonJS `require` with literals, dynamic `import('...')` literals).
- **likely** — bounded occurrence-specific receiver candidate sets from lexical value flow or complete mapped TypeScript-checker declaration sets. Small candidate sets remain explicit and record `candidateCount`.
- **possible** — name-matched member calls (`x.getUser()`): candidates listed, never silently dropped. This is the honest checker-less answer for calls through type annotations.

When an otherwise-certain reference resolves to multiple same-named root
symbols, the traversal projection emits every candidate at `possible`
confidence and includes ambiguity details instead of dropping the edge.

Event wiring (`emit('x')` ↔ `on('x')`) is surfaced by the `events` tool/command.

## Scoped dependency indexing

Dependency internals are opt-in and package-scoped. jscout never walks all of
`node_modules`:

```bash
jscout index /path/to/repo --deps zod,lodash
```

The effective selector list is authoritative: explicit `--deps` overrides
`index.dependencies` for `index`, or the independent `watch.dependencies` for
`watch`. Omitting the flag uses that command's configured list; `--no-deps`
explicitly disables dependencies. Keep both TOML lists consistent, or repeat
the same explicit `--deps` values for both commands. Dependencies disappear when
the effective list is empty. Discovery starts from real indexed importers and Node resolution,
so distinct installed versions become distinct package instances. An unused
root installation may also be selected by its exact package name. Package
roots and resolved modules are canonicalized before identity is assigned:
pnpm store links deduplicate to one physical instance, while declared
workspace links remain first-party code.

Each package is independently bounded to 10,000 files, 100 MiB total source,
and 2 MiB per file. Hidden directories, nested `node_modules`, minified names,
and strongly bundled long-line artifacts are skipped; a package entry point is
retained even when it looks bundled so the boundary does not disappear. When
the manifest exposes a `source` field or `source` export condition, that source
tree is preferred. Otherwise the active `exports`/`module`/`main` runtime tree
is indexed. Source-map reconstruction and Yarn Plug'n'Play zip archives are not
supported yet; PnP is rejected explicitly instead of being treated as a
missing package.

Indexed dependency files remain invisible to normal retrieval. All read
surfaces default to `repository,workspace`; include `dependency` explicitly:

```bash
# Dependency internals only
jscout search /path/to/repo "parse object schema" --origin dependency

# Cross the first-party/package boundary in one expansion
jscout search /path/to/repo "validate credentials" --expand --json \
  --origin repository,workspace,dependency

# Create vectors for dependency chunks (content-hash dedup still applies)
jscout embed /path/to/repo --origin dependency
```

The same `origins` allowlist is available on MCP search, definition,
who-uses, file-outline, events, and neighborhood calls. Search filtering is
applied before BM25/vector candidate ranking. Without dependency visibility,
the structural graph still exposes a versioned package-instance boundary hub;
indexed modules sit behind that hub and enter traversal only when their origin
is allowed.

Canonical runtime-entity nodes are also file-less boundary hubs. Their source
occurrences and symbol endpoints retain file origin, so an entity identity can
join first- and third-party evidence without making dependency-backed endpoint
code visible unless `dependency` is included in the caller's origin filter.

## Storage

Everything lives in one SQLite file, `.jscout.db`: code and Markdown chunks,
separate code/docs FTS5 corpora, symbols, import/export tables, classified
references, event/member-call sites, provenance-keyed embedding caches,
dimension-specific sqlite-vec indexes, and a disposable
`graph_nodes`/`resolved_edges` traversal projection. The projection is rebuilt
after indexing so barrel changes can reroute references in otherwise unchanged
files without leaving stale graph edges behind. Runtime module links use
`import`/`imports_package`; requests found only in type bindings use the
documentary `imports_types`/`imports_package_types` kinds. File roles live on canonical
file rows and are refreshed even when source hashes are unchanged. Files also
carry `repository`, `workspace`, or `dependency` origin plus optional package
instance/path identity. Package instances record canonical root, name, version,
locator, manifest hash, and completeness status.

Repository Markdown uses the shared `files`/`chunks` tables and durable
content-addressed embedding cache, while its documentation digest is independent
of the code digest. `docs_fts` and dimension-specific docs vector tables keep
its ranking statistics and candidates isolated from code search.

Agent-authored and generated `workflow`, `card`, `summary`, `concept`, and
`annotation` records live in separate `semantic_artifacts`/
`semantic_supports` tables; they never become structural edges. Workflow
participants are explicitly `defining` (the minimal stable
cross-file skeleton) or `supporting` (internal helpers and leaf operations), so
evidence does not flatten every related function into an equal boundary.
Supports store source and direct structural-context fingerprints.
Re-indexing preserves memory and makes source/context drift visible instead of
silently deleting or serving the record as current.

Workflow writes use a direct request: `participants` is top-level and every
participant carries its own anchor, role, scope, evidence file/span, and
confidence. jscout constructs the stored body and support pointers. Generic
`body`/`supports` input is reserved for `annotation` records. Writers retain
every distinct stable cross-file production stage as a participant; internal
or leaf stages are `supporting`, not compressed into another participant's
prose role.

## Gateway execution and indexing failures

See [authentication](installation.md#optional-scouting-authentication) before
running model-backed commands, and the [configuration reference](configuration.md)
for all settings.

Scouting model calls are serialized by default. Set
`llm.max_concurrency = N` to allow at most `N` independent subjects to wait on
the provider concurrently. jscout launches one local gateway worker per slot,
claims every run in the ledger before dispatch, and then validates and
publishes results in deterministic plan order. Summary and refresh dependency
levels remain barriers: only independent subjects inside the same level
overlap. `--max-calls` remains the total command budget and is not multiplied
by concurrency. Values must be positive and are not artificially capped; the
operator is responsible for provider rate limits and local process overhead.
Repository reconnaissance overlaps only the current frontier; children of a
`mixed` scope enter the next wave, in parent plan order.

The gateway owns one visible retry layer: at most two retries with 500 ms then
1,000 ms backoff, only for classified connection, timeout, rate-limit, overload, or
capacity failures. Every attempt keeps the exact provider, model, service tier,
and billing path. Auth, schema, context-window, quota, credit, and billing
failures are terminal; there is no hidden provider/model/tier fallback. The
command timeout includes retries and backoff. The first Ctrl-C sends
cancellation to every active gateway request; a second Ctrl-C, or an interrupt
when no request is active, forces exit status 130.

Normal gateway errors use stable controlled messages. Provider exception text,
prompts, tool arguments, and credential values are not written to stderr or the
run ledger; common key/token forms are redacted again at the protocol boundary.
Do not enable third-party HTTP debug logging in a shell that contains secrets.

Indexing continues past non-retryable file reads, permanent subtree/boundary
failures, and deterministic extraction errors. The final summary reports both
`removed=N` and `rejected=N`, followed by every rejected path, its stage
(`walk`, `ignore`, `workspace-manifest`,
`workspace-walk`, `workspace-alias`, `workspace-canonicalize`, `rust-edition`,
`read`, or `extract`), and the underlying error on stderr. `watch` prints those details
once per distinct rejection set, emits one recovery line when they clear, and
keeps the count in every refresh summary. A recognized transient read error
fails the phase instead, so a reduced corpus is not published; watch retries
indefinitely with an exponential delay capped at 30 seconds.

## Building release packages

Build a distributable archive containing the Rust binary, installed Node
sidecars, and optional Python inference service sources:

```bash
scripts/package-release.sh              # host target
scripts/package-release.sh TARGET_TRIPLE
```

The archive is written under `target/release-packages/`. Extract it anywhere,
put its directory on `PATH`, and keep the adjacent `gateway/`, `checker/`, and `inference/`
directories with the binary. Indexing and retrieval do not start Node.
`jscout enrich` starts the checker for typed member-call resolution;
`jscout scout repository` starts both the checker inventory and the pi-ai
gateway because configured projects are explicit reconnaissance subjects.

Build the npm publish tree instead — a `@jscout/cli` wrapper plus one
per-platform binary package, staged under `target/npm/`:

```bash
node scripts/npm-package.mjs                  # host platform + wrapper
node scripts/npm-package.mjs --target TRIPLE
```

The wrapper vendors no `node_modules`: it declares the sidecar dependencies
and lets the installer resolve them. Because the binary lands in a separate
platform package, `current_exe()` sidecar discovery cannot reach the bundled
`gateway/` and `checker/`, so `npm/cli/bin/jscout.mjs` supplies their paths
through private bundled-discovery transport before exec. Repository config and
explicit legacy overrides remain authoritative. Publishing is
driven by `.github/workflows/release-npm.yml` on a `vX.Y.Z` tag; the tag must
match the Cargo.toml version.

That workflow authenticates by OIDC trusted publishing, which cannot perform a
package's *first* publish: the trusted publisher is configured per package at
`npmjs.com/package/<name>/access`, which requires the package to already exist
([npm/cli#8544](https://github.com/npm/cli/issues/8544)). Any newly named
package — a new platform target as much as the initial release — is therefore
published once from a workstation:

```bash
node scripts/npm-bootstrap-publish.mjs --run-id RUN --dry-run
node scripts/npm-bootstrap-publish.mjs --run-id RUN
```

It takes the binaries from a workflow run rather than rebuilding them, and
refuses to publish unless every platform package the wrapper declares is
present at the Cargo.toml version with a binary whose actual architecture
matches its target. npm prompts for the one-time password, so no token is
created. Provenance begins with the first release published by the workflow.
