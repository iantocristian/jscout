# jscout architecture and implementation plan

> Status: authoritative plan as of 2026-08-25.
>
> G1–G10 have functional implementations, but G10 is not accepted for
> large-repository operation until its required scale correction passes. G11
> snapshot simplification, G12 watcher coordination and incremental source
> refresh, G13 repository reconnaissance, and G14 retrieval handoff are
> implemented. G15 design-before-edit task memory is parked after its harness
> treatment preserved wrong design contracts on a real task. G16 remains an
> independent conditional correction to G14 attached-memory delivery. G17
> exact-identifier dominance and G18 task-directed semantic coverage and
> selection are implemented. G13 has one planned extension: evidence-backed
> generated-output boundary reconnaissance for unignored build artifacts.
> Real-monorepo use has also registered syntax-aware and pure-identifier G17
> corrections plus staged-use guidance for the existing G14/G18 surfaces. A
> problem-solving investigation then confirmed that exact definitions are the
> efficient drill-down surface while repeated expansion dominates response
> volume. G19 is reserved for opt-in quiet-window scouting in watch, while G20
> is the compact-transport and path-projection pass. G21 repository-local
> runtime configuration and retrieval observability are implemented. G22
> exhaustive lexical search is implemented. G23 investigation/inquiry guidance
> is implemented, with its production replay still pending. Neither triggers
> G16 or widens the semantic product surface, and no retrieval default changes
> without a same-binary, same-snapshot comparison. G24 repository documentation
> retrieval phases 1, 2, and 4 are implemented; git-basis freshness remains
> gated on a retrieval evaluation corpus. G25 multi-format admission is
> scheduled as G26 phase 0. G26 Rust code indexing is the current implementation
> goal and the first additional code-corpus format, motivated by self-indexing this
> repository.

## Document policy

This is the only normative architecture and roadmap document.

- [README.md](README.md) documents current installation, commands, and
  operator-facing behavior.
- `eval/` contains dated protocols, preregistrations, and results. Those files
  are historical evidence, not live plans, and should not be rewritten to match
  later decisions.
- `presentations/` contains dated explanatory artifacts. Presentations are not
  implementation specifications or review material and should not be revised
  unless Cristian explicitly requests presentation work.
- Superseded plans and critiques remain available through Git history; keeping
  copies in the working tree created contradictory status and requirements.

When code and this document disagree about current behavior, fix the document
or code explicitly. Do not add another competing plan.

## Product

jscout is persistent, verifiable repository memory for coding agents. It is a
complementary tool: agents still reason, inspect source, and make changes;
jscout makes repository evidence and previously established meaning cheaper to
retrieve across files and sessions.

The serving contract is:

> Given an agent's current query or focus, return the smallest trustworthy
> slice of repository evidence that improves its next action.

The graph is structural infrastructure, not the product by itself. Scouting
uses deterministic repository structure to bound model interpretation, then
stores only evidence-backed semantic claims with visible confidence and
freshness.

## Trust model

| Layer | Contents | Authority and freshness |
|---|---|---|
| **R0 — source** | Repository files and exact indexed content | Final authority |
| **R1 — deterministic facts** | Files, symbols, resolved references, modules, contracts, runtime entities, occurrences, and structural edges | Parser/resolver/extractor output; refreshed by indexing |
| **R2 — derived views** | Search hits, definitions, outlines, neighborhoods, paths, repository overview, and optional elided source | Rendered from one current SQLite snapshot; not semantic truth |
| **R3 — semantic memory** | Workflows, annotations, selected symbol cards, summaries, and concepts | Model- or agent-authored claims; fingerprinted, evidence-backed, confidence-limited, and freshness-labelled |

Only deterministic resolution may be `certain`. Model- and agent-authored
claims are capped at `likely`; incomplete interpretations are `possible`.
Semantic artifacts are returned as `fresh`, `degraded`, `stale`, or
`superseded` rather than silently presented as current truth.

## Non-negotiable invariants

1. Raw source is never replaced by a compressed or generated representation.
2. Rust owns repository access, deterministic candidates, evidence packs,
   schemas, validation, persistence, freshness, and public CLI/MCP behavior.
3. Generative providers are reached only through the companion pi-ai gateway;
   the JavaScript process never receives repository or SQLite access.
4. Existing typed extraction tables are canonical. Generic graph tables are
   disposable traversal projections.
5. Chunks are retrieval units, not ontology nodes. Search hits project onto
   snapshot-scoped file and symbol anchors.
6. Ambiguity produces candidates, hubs, or a loud error—never silent omission
   or fabricated certainty.
7. Every semantic claim reaches exact source evidence directly or through
   fingerprinted child artifacts.
8. Indexing and watch mode never make model calls or spend model budget.
9. Complete response budgets include hits, semantic memory, expansion,
   metadata, and serialization overhead.
10. Dependency internals, possible-confidence expansion, and structural
    expansion remain explicit scope choices.

## Snapshot lifecycle and logical storage planes

**Amendment — disposable snapshot boundary (2026-08-13), watcher carry-forward
(2026-08-20).** This section is authoritative over older text. Manual
`jscout index` always clears checker facts, including on an identical rebuild.
Only `watch --enrich` may retain the prior active batch plus the newest reusable
superseded staging source as hidden inputs while it constructs a newly
validated batch for the current snapshot. jscout uses
one SQLite database with three logical lifecycles, not three physical databases:

| Plane | Contents | Lifecycle |
|---|---|---|
| **Disposable structural snapshot** | Files, chunks/FTS, symbols, imports/exports, references, events, member calls, contracts, entities, package instances, checker batches/facts, graph projection, and materialized vector occurrences | Rebuilt from the current checkout. Manual indexing clears checker batches. Watch may retain the active publication and newest reusable superseded staging source non-public long enough to carry validated completed projects/facts into a new current-snapshot batch. |
| **Durable content cache** | Embedding profiles and content-hash-keyed embedding vectors | Preserved across snapshot rebuilds. Current chunk occurrences are rematerialized from cached vectors; only unseen content is embedded. |
| **Durable semantic memory** | Scout runs/classifications and `semantic_*` artifacts, relations, and evidence supports | Preserved across snapshot rebuilds. Evidence hashes and current anchors determine whether a claim is fresh, degraded, stale, or superseded. |

The database file is an implementation container, not a shared lifecycle.
Physical database splitting adds backup, transaction, and deployment
complexity without improving this contract and is not planned.

`jscout index` is the reliable fixed-snapshot path. Its target behavior is a
full disposable-plane rebuild followed by resolution, projection, and vector
occurrence rematerialization, with publication of a current snapshot only
after those required phases succeed. Rebuilding source-derived state is cheap;
recomputing embeddings and discarding reviewed semantic memory are not.

Checker enrichment is also snapshot-bound: projection never reads a batch from
another snapshot. Manual indexing clears the whole checker plane and
`jscout enrich` repopulates it. Watch-only carry first matches a project's
configuration-chain and membership fingerprints plus checker/protocol identity,
then rebinds facts only when the exact occurrence source/hash/spans and target
fingerprint still match. Every owner of a multi-project occurrence must carry,
or every owner is re-queried. The published result is a new batch bound to the
new snapshot; the old batch is never traversable there.

This optimization deliberately does not treat every source hash loaded by a
TypeScript Program as a project-wide invalidator. Such a rule would turn any
edit into full re-enrichment. Exact external inputs remain watched and copied
for carried projects, projection performs per-fact validation, and an
independent daily-scale carry-free enrichment bounds ambient drift. Manual
`jscout enrich --full` performs the same carry-free recomputation immediately.

Watch mode is a separate coordination problem, not the correctness model for
indexing. It may use incremental work as an optimization, but branch switches,
submodule changes, lockfile/configuration changes, missed events, or uncertain
ownership must converge through the same full snapshot refresh. Watcher
optimization must not make the non-watcher index path stateful or fragile.

Query surfaces should open an existing published snapshot without silently
creating or migrating structural state. Commands that refresh the snapshot,
materialize embeddings, publish checker enrichment, or write semantic memory
retain only the write authority their operation requires.

The historical v1→v18 in-place migration ladder is retired. A database at the
current schema opens normally. Schemas at or above the v16 durable-format floor
keep embedding and semantic-memory tables while dropping and recreating the
entire disposable plane once. Older or future-incompatible durable formats fail
with instructions to preserve the old file. Future durable-plane changes need
an explicit export/import or cache-compatibility decision, not migrations for
source-derived tables.

## System architecture

```text
agent / CLI / MCP
        |
        v
Rust jscout
  walk + parse + resolve + extract
        |
        v
SQLite R0 references + R1 facts + graph projection
        |
        +--> deterministic R2 retrieval and agent surfaces
        |         ^
        |         +-- optional Python BGE embed/rerank service
        |
        +--> candidate/evidence pack --> Node pi-ai gateway --> model
                                            |
                                            v
                              Rust validation + atomic R3 publication
```

Rust remains the application. The gateway is a transport adapter, not an
agent, indexer, or semantic authority.

The optional Python inference service is a retrieval accelerator, not the
generative gateway. It owns only Hugging Face/PyTorch model execution behind
bounded `/embed` and `/rerank` requests. Rust retains provider selection,
credentials, cache identity, vector storage, fusion, fallback, and ranking.

## Implemented baseline

| Area | Current implementation |
|---|---|
| Parsing and chunking | OXC syntax and semantic analysis; AST-aware JS/JSX/TS/TSX/MJS/CJS/MTS/CTS chunks with scopes, declarations, imports, JSDoc, source spans, and BLAKE3 hashes |
| Storage | One versioned SQLite database; schema v29; three explicit logical lifecycles; FTS5, provenance-keyed embedding caches, dimension-specific sqlite-vec `vec0` indexes, canonical extraction tables, graph projection, durable reconnaissance policy, semantic artifacts, run ledger, and freshness metadata |
| Runtime graph | Files, symbols, imports/exports/re-exports, module resolution, local/imported references, calls, construction, JSX renders, inheritance, event/property hubs, and ranked bounded traversal |
| Runtime boundaries | Registry handlers/dispatch, lifecycle operations/listeners, jobs/queues/crons, DI tokens/providers, and logical workflow handoffs |
| Contract plane | Interfaces, aliases, enums, decorators, DTO/schema evidence, exported parameter/return contracts, referenced contract names, and type-only barrel resolution; documentary edges remain separate from runtime edges |
| General entities | Routes, GraphQL operations, environment/configuration keys, database resources, feature flags, and external-service hosts with canonical identity plus evidence-bearing occurrences |
| Dependency scope | Opt-in named packages; realpath-normalized workspace/dependency identity, pnpm layout/version handling, source-over-dist preference, bundle/minification limits, and dependency origin excluded from retrieval by default |
| Retrieval | BM25 plus optional explicit-provider embeddings/RRF/reranking; local BGE-M3 and BGE reranker share one Python/PyTorch service; native exact cosine KNN runs through sqlite-vec rather than a Rust full-table loop; snapshot-scoped anchors, file roles, definitions, who-uses, events, entity lookup, ranked paths, filtered semantic-memory queries, exact source drill-down, fresh-only overview overlays, and opt-in structural expansion |
| Agent integration | CLI, MCP profiles, project-local agent guide, whole-response budgets, privacy-minimal telemetry, packaged companion gateway, and isolated evaluation database support |
| Semantic memory | Validated agent write-back; candidate-closed generated workflows; evidence-backed selected symbol cards; bottom-up child-cited file/module/repository summaries; exact-vocabulary concepts with derived file/chunk tags; fingerprint-pinned child relations and upward freshness propagation; automatic deterministic discovery; run reuse; explicit refresh; immutable successors; fresh/degraded/stale status |
| Model gateway | Pinned `@earendil-works/pi-ai` 0.84.1 sidecar, protocol-v1 JSONL over stdio, provider/auth registry, bounded same-model retries, cancellation, controlled/redacted errors, installed-layout packaging, Node-version enforcement, and auth-aware `llm doctor` |

## Deterministic repository plane

### Identity and resolution

Canonical anchors are snapshot-scoped:

- `file:<repo-relative-path>`
- `pkg:<package-name>`
- `sym:<path>#<scope>::<name>@<ordinal>`
- `contract:<entity-type>:<path>#<name>` for contract declarations;
  name-only contract references collapse to hashed
  `contract:<entity-type>:ref-<digest>` hubs, and requests that leave the
  indexed tree use the `contract:<entity-type>:external:<request>#<name>`
  and `contract:<entity-type>:unresolved:<request>#<name>` variants
- `entity:<type>:<normalized-name>` for canonical entities, with hashed
  `entity:<type>:ref-<digest>` reference hubs
- receiver-qualified or unknown event/property hubs

They are deterministic for one indexed snapshot, not stable identities across
arbitrary edits or moves. Responses carry the repository snapshot. A stale
symbol anchor is re-resolved by path, scope, and name; ambiguity fails with
candidates rather than binding to a reused ordinal.

After changed files are extracted, jscout rebuilds the disposable graph
projection in one transaction. Full projection rebuild remains the policy
because a barrel edit can reroute unchanged importers. Selective invalidation
is not justified until measured scale requires it.

Chunks project onto overlapping declaration anchors and fall back to the file
for module-level behavior. The originating hit remains attached so traversal
never loses its exact source context.

### Uncertain runtime relationships

- Ambiguous root references fan out as `possible` candidate edges.
- Unknown-receiver member calls use bounded property hubs rather than a
  call-site × symbol cross-product.
- A binding-aware value-flow pass projects supported instance-context `this`,
  direct/const-bound `new`, imported/exported const values, and closed
  synchronous module-scope factory receivers at `likely`, with at
  most three targets and factory recursion capped at depth two. Exact root or
  imported binding identity is required. Awaited values, implicit fallthrough,
  ambiguous or heuristic module resolution, unsupported returns or receiver
  shapes, and method-shadowing fields, accessors, or direct property writes give
  up to the existing property hub.
- Events use receiver-qualified or unknown event hubs; jscout does not connect
  every emitter to every listener sharing a common string.
- General-association edges are terminal and degree-bounded in workflow
  traversal so common helpers do not consume the candidate budget.

Every projected edge retains kind, confidence, provenance, source location,
and detail needed to explain why it exists.

### Runtime, contract, and entity planes

The runtime plane models executable values and effects. The contract plane
retains TypeScript and schema information as documentary evidence without
pretending it is a runtime call edge. Canonical entities remain separate from
their source occurrences so identity, provenance, evidence, and file lifecycle
have the correct update semantics.

Runtime-boundary and general entities are projected into the graph. This lets
an agent traverse relationships such as route → handler → service → queue →
worker or producer → data resource → lifecycle listener, including across
repository/dependency boundaries when dependency origin is explicitly enabled.

### Dependency boundary

`jscout index --deps <package,...>` indexes named installed packages only.
Blanket `node_modules` indexing is intentionally unsupported because it
amplifies irrelevant-file noise and bundled/minified content.

Physical identity wins over traversal spelling:

- realpaths inside the repository are workspace code even when reached through
  `node_modules` symlinks;
- pnpm store realpaths are deduplicated and versioned from their package
  instance;
- dependency files carry `origin=dependency` plus package identity;
- dependency chunks and backing files stay excluded from default retrieval;
- Yarn Plug'n'Play zip indexing is out of scope.

### Retrieval and traversal

Search ranking is independent of graph expansion. Expansion attaches a
separately labelled structural context pack after retrieval and remains off by
default. Matching semantic memory is attached by default with explicit
freshness and can be disabled or separately budgeted.

Traversal uses a simple, explicit ranking hypothesis:

```text
path score = minimum confidence on the path
           × relation weight
           × distance decay
           × hub damping
           × file-role/origin policy
```

The weights are deterministic heuristics, not learned relevance. Nodes and
edges are deduplicated before global node, edge, and byte budgets are applied.
Standalone neighborhood UX is secondary; expanded search and bounded paths are
the primary graph delivery surfaces agents have actually used.

## Semantic scouting plane

### Gateway boundary

The gateway is a companion Node package using pi-ai. It owns provider
registration, model lookup, credentials, reasoning/service-tier translation,
request execution, cancellation, and normalized provider errors/usage. It does
not own prompts, schemas, tools, repository reads, validation, or persistence.

Resolution order is:

1. CLI override;
2. `JSCOUT_*` environment override;
3. bundled gateway and the plan-backed model default
   `openai-codex:gpt-5.6-terra`.

There is no automatic provider, model, or billing-path fallback. Diagnostics
and every scout run record provider, model, billing path, reasoning policy,
gateway protocol, prompt version, usage, and stable error classification.
Credentials and hidden reasoning are never stored.

The operator configuration surface — `JSCOUT_LLM_MODEL`,
`JSCOUT_LLM_REASONING`, `JSCOUT_PI_AI_GATEWAY`, `JSCOUT_NODE`, and the
gateway-side `JSCOUT_PI_AI_AUTH_FILE` and
`JSCOUT_PI_AI_OPENAI_BASE_URL` and `JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS`
— is documented in the README
configuration section, with `.env.example` as the safe template. This
document defines the boundary; the README owns the operating instructions.

The versioned JSONL protocol uses request IDs and supports `hello`,
capabilities, completion, cancellation, and shutdown. Rust submits one JSON
Schema tool per semantic artifact type. Text-only output, unknown/multiple tool
calls, malformed arguments, incomplete candidate classification, timeouts, and
unexpected gateway exit fail the run without publishing an artifact.

### Storage and publication

`semantic_artifacts` and `semantic_supports` are canonical semantic memory.
Generated rows also reference `scout_runs`, input/artifact fingerprints, and
exhaustive `scout_classifications`. `semantic_relations` links parent artifacts
to fingerprinted children for hierarchy and concepts.

A generated artifact is published only when one transaction can commit the
completed run, classifications, artifact, supports, and relations after
rechecking the current snapshot and evidence hashes. Failed, incomplete,
canceled, or snapshot-raced runs publish no artifact.

Agent `annotate` writes use the same support validation and freshness rules but
cannot modify structural tables. Agent-reported claims and generated claims
remain separately attributable and corrections create immutable successors.

### Implemented workflow scouting (G1–G5)

1. Resolve explicit seeds or deterministically select entry surfaces from
   routes, GraphQL, runtime handlers/producers, lifecycle/job/DI boundaries,
   and bounded package/application exports.
2. Construct a ranked, bounded workflow candidate set from the current graph.
   Truncated sets are refused rather than interpreted as complete.
3. Build a line-numbered evidence pack from full source plus relevant
   deterministic entities. Contract-only relationships stay out of runtime
   workflow topology.
4. Fingerprint the snapshot, seeds, candidates, evidence, schema, prompt,
   gateway protocol, model, and request policy. Reuse a matching completed run
   unless rebuild is explicit.
5. Require the model to classify every candidate exactly once as `defining`,
   `supporting`, or `excluded`, with roles and exact evidence for included
   candidates.
6. Validate closure, anchors, line ranges, hashes, body limits, confidence, and
   at least one defining participant in Rust.
7. Recheck all deterministic inputs and publish atomically. Refresh selects
   stale/degraded current workflows and writes immutable successors using their
   recorded configuration.

The model cannot add anchors outside the deterministic candidate set. It may
declare the pack incomplete; that records an incomplete run and publishes no
workflow.

### Implemented selected symbol cards (G6)

Cards reuse the same gateway, run ledger, evidence pack, support validator,
freshness engine, and immutable supersession as workflows.

1. Select subjects deterministically: exported production symbols, runtime
   boundary endpoints, and participants of current published workflows,
   deduped by anchor and capped with a reported discovered count; or resolve
   explicitly requested anchors. One run per subject.
2. Build evidence from the subject's declaring file plus its depth-1 resolved
   edges, rendered deterministically. Those edges and entity annotations are
   deterministic facts the prompt forbids restating as claims: a card that
   repeats signatures or call lists spends tokens on what the index already
   knows.
3. Fingerprint the snapshot, subject, evidence, schema, prompt, gateway
   protocol, model, and request policy, and reuse a matching completed run
   unless rebuild is explicit.
4. Require `purpose` with exact evidence; accept `architectural_role`,
   `domain_terms`, `side_effects`, `invariants`, and `failure_modes` only when
   each individual claim carries its own line ranges. Unsupported optional
   fields are omitted, never filled speculatively, and an unsupported claim
   fails the run rather than downgrading the card.
5. Publish one artifact per subject anchor with one JSON-pointer support per
   claim per range at `likely` confidence, under the same atomic recheck.

The model may declare the evidence insufficient; that records an incomplete
run and publishes no card. Refresh selects stale/degraded current cards and
replays their recorded subject and model into immutable successors.

### Implemented hierarchical summaries (G7)

The governing specification, unchanged — build bottom-up rather than prompting
over the repository at once:

- file summaries from validated cards/workflows plus deterministic topology;
- module/package summaries from selected child claims;
- repository summary from package/module artifacts.

Every parent claim links through `semantic_relations` to child fingerprints and
ultimately to exact source support. A changed child degrades or stales its
parents even when the parent's own text is unchanged. Prose without a support
chain is not indexable memory.

As implemented, summaries reuse the same gateway, run ledger, support
validator, freshness engine, and immutable supersession as workflows and cards.

1. Discover scopes deterministically per level from the index and the workspace
   manifest set, never from the model: `file:<path>` from the files current
   cards and workflows cite, `module:<package>` from the file summaries a
   workspace package owns, and `repo` from module summaries plus the file
   summaries no package owns. A scope with no current children is not a summary
   subject; a child set that outgrows one bounded prompt is refused or skipped,
   never silently truncated.
2. Enumerate the children as `C1..Cn` with their bodies quoted as data and their
   artifact fingerprints pinned inline, so the prompt pack itself participates
   in the input fingerprint. That fingerprint is deliberately snapshot-free:
   an unrelated repository change reuses the completed run.
3. Require every claim — the one mandatory `overview` and each optional key
   point — to cite the child references supporting it. Uncited prose fails
   validation, an unknown reference fails validation, and a refusal
   (`incomplete_reason`) is mutually exclusive with any claim.
4. Publish one artifact per scope key, with one `summarizes` relation per cited
   child per claim plus a whole-summary input dependency for every planned
   child, all pinned to the fingerprints the summary was grounded on, at
   `likely` confidence. Inside the publication transaction, recheck the
   structural snapshot, exact current child set, and every child's pinned
   fingerprint; any mismatch refuses the write whole.
5. Run levels staged bottom-up under one `--max-calls` budget when no `--level`
   is given, so each level is planned only after the previous level's artifacts
   exist and a module summary sees the file summaries the same invocation just
   published.

Freshness propagates upward on read: a missing, superseded, or changed child
stales its parent, and a current-but-not-fresh child degrades it, bounded by
the three-level hierarchy. Refresh therefore needs no summary-specific
selection rule — a summary whose child drifted is already non-fresh — and
replans the recorded scope against the children current at refresh time.

## Semantic-v1 final layers

### Implemented G8 — concepts

Concept scouting operates on a deliberately narrow, evidence-backed
vocabulary rather than embedding clusters or arbitrary generated prose. The
only admitted inputs are supported claims on current fingerprinted artifacts:
a workflow's canonical `/name` and a card's string-valued
`/domain_terms/<index>`. Unsupported values and every other body field are
invisible to concept discovery.

1. Group terms by the versioned `concept-normalizer/nfkc-lower-ws-v1`
   identity: Unicode NFKC, Unicode lowercase, and trimmed/collapsed whitespace.
   Punctuation is preserved, so `invoice-id` and `invoice id` remain different
   identities.
2. Plan one bounded model call per exact normalized group. Automatic discovery
   requires an explicit command-level call budget; repeatable `--term` values
   select existing groups through the same normalizer and default the budget to
   their count. Oversized groups are refused rather than truncated.
3. Let the model define the repository-specific concept, but not its identity,
   aliases, or children. The alias list is the exhaustive set of observed
   NFKC/whitespace-normalized display spellings, every claim cites enumerated
   child artifacts, and every child is classified exactly once.
4. Publish the normalized name, aliases, and definition with claim-level
   `related_to` relations and whole-input dependencies. Do not copy a child's
   source span onto generated prose: the fingerprinted child hop leads to its
   exact supports without overstating what the span proves.
   Child fingerprints, the normalizer version, prompt/schema/model policy, and
   rendered input all participate in the run contract. Confidence is capped at
   `likely`, or at `possible` when any child artifact or vocabulary support is
   only possible.
5. Recheck the structural snapshot, every child fingerprint, the exact current
   vocabulary child set, and the current concept lineage inside publication.
   Child drift makes the concept stale/degraded through the shared freshness
   engine; `scout refresh` replans it and publishes an immutable successor.
   Operationally, scout concepts after workflow/card sweeps. A newly published
   child carrying an existing normalized term intentionally stales that concept;
   mixed refresh orders children first, and every concept planning surface
   refuses reuse or model spend until those dependencies are fresh.
6. `memory`/`semantic_memory` derives bounded `concept_tags` only for selected
   current, fresh concepts. Exact supports reached through claim-level child
   relations project into deduplicated file associations and associations with
   every overlapping indexed chunk. Tags are an R2 response view, not stored
   semantic claims, and are dropped first when the complete response-byte
   budget binds.

Exact normalized spellings share one lineage. Fuzzy, stemming, punctuation,
or embedding-based near-duplicate merging is deferred. The current schema has
one predecessor per successor, so jscout also refuses ambiguous many-lineage
merges instead of pretending a many-to-one merge occurred.

Concept-to-child provenance uses `related_to` because its direction is
concept → evidence-bearing workflow/card. The schema's `names_concept` value is
reserved for a future explicit source-artifact → concept assertion; it does not
describe the current generated provenance direction.

This layer enables questions such as “which workflows touch invoice
reconciliation?” without replacing the source evidence used to answer them.

### Implemented G9 — retrieval, packaging, and operations

- `memory`/`semantic_memory` filter current or historical workflows, cards,
  summaries, concepts, and annotations by text, type, freshness, exact evidence
  anchor, or direct artifact relation. Results expose successors, bounded
  relations, and pinned evidence paths to hash-verified source.
- `overview`/`repository_overview` read deterministic inventory and optional
  semantic overlays from one SQLite snapshot. Generated overlays are opt-in,
  current/fresh only, separately labelled untrusted data, and are sacrificed
  before deterministic inventory when the whole-response budget binds.
- Semantic sections remain separate from BM25/vector ranking. CLI text search
  no longer hides semantic matches when there are no code hits; neighborhood,
  semantic memory, overview, and the existing code surfaces enforce complete
  rendered-byte budgets.
- Agent-facing search and neighborhood transport is compact by default. The
  full canonical representation remains available only through explicit debug
  mode. Whole-response shedding preserves the top code hit, removes optional
  semantic memory and low-ranked relations first, and caps search-attached
  evidence at eight supports globally rather than eight per artifact.
- Embedding documents are content-only because durable vectors are keyed by
  content hash. The document-text format is versioned in the profile
  fingerprint, and embedding selection groups missing work by hash so duplicate
  chunk occurrences cause one provider request and one cached vector.
- Every search response reports lexical/vector/reranker retrieval status. A
  configured vector stage that cannot use its requested profile is `degraded`, not
  indistinguishable from an active hybrid search. The `content-v2` document
  format intentionally requires existing embedded repositories to create one
  new profile with `jscout embed`; prior profiles remain stored and are never
  mixed into the new vector space.
- File-role filters apply before fusion and reranker pool construction.
  Cross-encoder inputs carry occurrence-specific path, scope, symbol, kind,
  role, origin, and span context, and reranking preserves the untouched RRF
  tail. This repairs the context-starved input without changing the reranker
  default; a real-query remeasurement must precede any default flip.
- Release packaging places the installed gateway and pinned dependencies beside
  the Rust binary. Startup and doctor enforce the supported Node version and
  produce controlled missing-runtime/dependency/auth diagnostics; deterministic
  indexing and retrieval remain Node-free.
- The gateway retries at most twice and only for classified transient/capacity
  failures, retaining the exact provider, model, service tier, and billing
  path. Auth/schema/context/quota/billing errors are terminal. Cancellation
  interrupts requests and retry backoff; provider errors and credentials are
  redacted from normal output.
- Operator documentation covers ChatGPT-plan auth, API-key providers, custom
  compatible endpoints, proxy/TLS boundaries, redaction, cancellation,
  retries, packaging, and the non-network scope of `llm doctor`.

## Semantic-v1 completion boundary

G1–G9 now satisfy this design boundary. The verification policy below remains
the release gate before product-value evaluation.

Semantic v1 is complete when:

- one supported installation can call both a ChatGPT-plan model and an API-key
  model through the same gateway protocol;
- workflows, selected cards, hierarchy, and concepts all use the common
  run/evidence/freshness engine;
- malformed, partial, unsupported, or snapshot-raced output never publishes;
- stale artifacts are visibly stale and refresh into immutable successors;
- semantic-specific queries and repository overview drill down to exact source;
- the gateway is bundled, diagnosable, cancellable, and redacts credentials and
  prompt contents from normal logs;
- deterministic indexing and retrieval still work with Node absent.

## Verification policy

No further product-value evaluation is required during semantic-v1
implementation. Before real repository testing, complete engineering
verification:

- Rust compile, formatting, lint, unit, schema-compatibility, and existing
  regression tests;
- fake-provider gateway protocol/config/auth tests;
- schema rejection, cancellation, timeout, child-crash, snapshot-race, and
  no-partial-write tests;
- deterministic evidence-pack and freshness-transition fixtures;
- no paid or plan-backed model calls in the default test suite.

After semantic v1, run real Sol or Terra scouting on the installed n8n and
Twenty repositories, inspect generated memory, repair implementation defects,
and only then compare real agent work with and without it.

## Implemented post-v1 checker enrichment sidecar (G10)

As implemented, schema v29 stores exact call/receiver/property byte spans and
canonical checker batches. `jscout enrich` drives a pinned Node/TypeScript
sidecar explicitly; `jscout checker doctor` reports project/configuration
readiness. The protocol host isolates compiler work in a terminable worker,
and the Rust client enforces a hard deadline. Under projection v12 the checker
stage recreates only fresh occurrence-specific `checker` edges and retains the
shared possible member hubs.

**Amendment — bounded receiver value flow (2026-08-22).** Deterministic
indexing now records closed syntax-and-binding summaries for `this.m()` in
instance methods and supported non-static initializers, direct or const-bound
`new C()` receivers, imported/exported const values, and immutable module-scope
factory receivers. Every factory return must be a construct, a const binding
to one, or another summarized factory call, and block-bodied factories must
terminate without implicit fallthrough. Awaited values and async factories are
left to the checker because thenable assimilation can change receiver identity.
Factory resolution is capped at depth two and requires one exact module root or
imported binding at each hop; local immutable aliases are followed. It rejects
heuristic workspace edges, unresolved or ambiguous exports, mutable
declarations, destructuring, optional factory results, decorators, constructors
with explicit returns, `eval` references, dynamic `with` scope, unresolved or
dynamically computed base/member shapes, and TypeScript parameter properties.
One-hop inheritance is used only when the receiver class has no own runtime
method and no field, accessor, or direct `this.property` write anywhere in the
exact superclass chain shadows the requested member, and the full superclass
construction chain resolves exactly.

The pass emits one to three occurrence-specific targets at `likely` with
`receiver-value-flow` provenance and `candidateCount`. Optional member
invocation is retained because it changes execution, not the target of an
executed call. Parameters, unsupported/conditional expressions, `this.field`,
unresolved branches, deeper factories, and larger target sets emit nothing
beyond the existing property hub. Direct dot/bracket member assignments,
updates, deletes, destructuring targets, and `for` targets block a binding.
Alias-mediated writes, global-object rebinding, `Object.assign`/`defineProperty`,
and prototype mutation remain outside this bounded proof, so the result is
deliberately not `certain`.
Resolved occurrences are excluded from checker planning even under `--all`.

The original plan deferred checker-backed enrichment behind a revisit
trigger. That trigger is now pulled deliberately: the call-site query work
showed that receiver identity (`dbs.wave.card` → which table class) is the
recurring gap between candidate-set answers and behavioral ones, and the
owner has accepted the cost. This section replaces the deferral; it does not
extend the semantic-v1 completion boundary, and nothing in v1 depends on it.

**Amendment — watch replenishment (2026-08-13).** The original G10 rule that
enrichment never runs during `watch` is replaced by the explicit
`jscout watch --enrich` option. Each refresh launches one explicitly configured
sidecar pass after deterministic indexing has suppressed stale checker edges;
an identical refresh may resolve as a no-op against the already active exact-
snapshot batch. The sidecar then exits. This does not authorize a persistent
watch-resident TypeScript daemon, hidden checker execution in plain `watch`, or
checker work in `index`.

**Amendment — large-repository execution (2026-08-13).** A real repository run
planned 150,213 eligible member-call occurrences, progressed slowly through
10,900 one-at-a-time queries, then lost the run when the checker worker crashed.
This falsifies the current implementation's claim to be bounded at repository
scale. Raising the V8 heap only postpones the failure; exhaustive `enrich` and
`watch --enrich` are not considered operational on large repositories until
the scale correction below passes its acceptance checks.

**Amendment — tooling-project ownership (2026-08-14).** Configuration-only
planning now classifies only high-confidence lint projects as `tooling` from
explicit filename/extends evidence or a lint script corroborated by `noEmit`.
For each file, tooling owners are excluded only when at least one non-tooling
owner remains; otherwise they remain fallback owners. Doctor output records
project purpose/evidence, while enrichment dry runs record selected, excluded,
and fallback occurrence counts per affected project. Generic `noEmit` and broad
include patterns never classify a project by themselves. G13 remains
responsible for ambiguous project purpose rather than extending this bootstrap
with repository-specific exceptions.

**Amendment — builtin-receiver and runtime-namesake selection (2026-08-15).**
A Next.js diagnostic showed ~93% of returned declarations failing to anchor:
62.8% in the TypeScript standard library, 30.2% in `node_modules/@types`, and
receiver classes told the story (`this` receivers mapped at 88%, ECMAScript
globals at 0.05%, Node core namespaces at 0%). Discovery now computes two
per-occurrence facts: a builtin-looking receiver (a file-local unbound
ECMAScript/host global name or a receiver sharing a file-level Node-core import
name) and a runtime namesake (some indexed symbol with the member's name in an
effective-runtime file). Runtime namesakes remain a necessary default
eligibility gate and `--all` bypasses it. Builtin detection is advisory only:
project-wide ambient declarations, lexical import shadows, and tsconfig path
aliases make file-local classification insufficient for hard exclusion.
Builtin-looking occurrences are scheduled after ordinary receivers within the
same structural tier, reported separately, and still all consumed by an
uncapped run. An empty post-filter plan is a successful no-op that does not
launch the checker. The sidecar labels every declaration's provenance (`repo`,
`types`, `lib`, `vendored`, `outside`); non-`repo` declarations skip the
anchoring lookup but still count as unmapped, so the per-occurrence ambiguity rule
(`unmapped == 0`) and every published confidence are byte-identical — the
change avoids runtime-unanchorable names and attributes refusals without
relaxing fail-closed anchoring.

### Shape

A companion Node sidecar hosting the TypeScript checker (LanguageService or
tsserver — an implementation decision, not a plan commitment) behind the
same versioned JSONL-over-stdio pattern as the pi-ai gateway: `hello`,
capabilities, query, cancellation, shutdown, request IDs, stable error
codes. The sidecar answers exactly one semantic question in its first version:
resolve a statically named member at an indexed call occurrence. The scalable
protocol batches that question over bounded groups of occurrences. Each item
contains the repository-relative file, indexed file hash, exact call,
receiver-expression, and property spans. Results contain the receiver's
declared type and the called property's declaration site(s), grouped by the
configured or inferred TypeScript project that produced the answer.

This requires an extraction/schema change before the sidecar is useful.
`member_calls.start` currently identifies the start of the whole call, and the
stored `object` covers only shallow identifier/`this` receivers; neither can
identify the receiver in `dbs.wave.card.insert()`. G10 must persist exact
`[start,end)` spans for the call, receiver expression, and property, including
nested, `this.`-qualified, and optional-chain forms, and force affected files
through re-extraction. Every request is bound to those spans and the indexed
source hash. The sidecar accepts only indexed repository-relative query paths
and rejects traversal, hash mismatches, and query locations outside the
configured repository root.

A file can belong to multiple `tsconfig` projects with different ambient types
or path mappings. Project discovery and selection must therefore be
deterministic: enumerate every owning configured project (or one explicitly
identified inferred project), attach a stable project ID and effective compiler
options to each answer, and never choose a target by server load order. Equal
answers may coalesce. Fresh owning projects that disagree contribute to one
configuration-conditioned candidate set: a fully mapped closed union of at most
three targets remains visible as separate `likely` candidates; larger or
incompletely mapped sets are `possible`, and an answer that cannot be mapped
safely is `unknown`. Conflicts never collapse into one arbitrary edge.

An owning project that returns `unknown` is incomplete coverage, not evidence
against a clean resolution produced by another owning project. It therefore
does not demote otherwise agreeing resolved answers. Canonical occurrence
coverage retains its project ID, status, and input fingerprint; projected
checker edges expose those IDs as `unknownProjects`. Projected edges also expose
the closed set's `candidateCount`. One to three distinct mapped targets with no
unmappable declaration remain `likely`; four or more targets, or any unmappable
declaration from a resolved answer, make every survivor `possible`. The complete
answer for the selected plan, including explicit
omitted/failed coverage, is published as one batch bound to the current
structural snapshot; it is never freshened project by project.

Diagnostics are never enumerated, used as a gate, or surfaced. A broken or
non-compiling project still attempts the requested member query rather than
turning enrichment into a compile check. When the answer is an error type or
`any`-degraded, the sidecar reports `unknown` and jscout records no target fact
for that project — no guessed edge. Its owning-project coverage is still
recorded and surfaced as described above.

The sidecar prefers the repository's own `typescript` installation so
answers match the project's language version; a bundled fallback is
permitted but its version is always recorded in provenance. Absent Node,
absent TypeScript, or an unhealthy sidecar leaves every existing surface
working exactly as today.

Checker program construction and queries may block the Node event loop. The
protocol host must keep cancellation responsive by isolating checker work in a
terminable worker or child process. Rust also enforces a hard per-request
deadline and terminates an unresponsive sidecar; writing a cancel message alone
is not considered cancellation support. Worker lifetime is a memory boundary,
not merely a cancellation mechanism: a completed or failed project worker is
terminated so its complete TypeScript `Program` is reclaimed before another
large project is admitted.

### Consumption

Enrichment is an explicit pass (`jscout enrich`) and an explicit watch option
(`jscout watch --enrich`); deterministic indexing remains Node-free. The pass
takes a bounded, reported plan of indexed member-call occurrences whose
receivers currently reach property-hub candidates, asks the sidecar to resolve
the called property on each receiver, and maps returned declaration sites to
indexed symbol anchors.

Enrichment is occurrence-specific. A single `dbs.wave.card.insert()` result may
add an edge from that call's enclosing file/symbol to `CardTable.insert`; it
must never promote or replace the shared `member:insert` hub edge, which would
leak the answer into unrelated `.insert()` calls. A fully mapped closed set of
one to three targets is `likely` with provenance `checker`; each edge records
the set's `candidateCount`. Four or more targets, or any incompletely mapped
answer, remain separate `possible` candidates. Existing hubs are retained for
unexplained dynamic calls. Contract-plane consumers may attach the receiver's
declared type as documentary evidence under the same provenance.

Checker results are typed facts in the disposable snapshot plane, not writes
made directly to the graph projection. A dedicated enrichment table records the
caller/file identity and hash, exact occurrence spans, project ID, resolved
target anchor and fingerprint, confidence/provenance, and checker-input
fingerprint. Projection rebuilds include only the active batch whose
`source_snapshot` exactly matches the snapshot being projected. Inactive staging
runs may coexist temporarily so bounded work can be committed and resumed, but
they are never traversable and do not weaken the one-active-batch rule.

The checker-input fingerprint covers the exact TypeScript package/version,
normalized config inheritance, effective compiler options, project selection,
and identities/hashes of the config, source, and declaration inputs loaded by
the checker. It is retained as provenance and used to detect an input race
during a checked enrichment command. Watch carry uses the separate coarse
project-planning fingerprint described in G12.2. The pass
publishes only after rechecking its structural snapshot, occurrence source
hashes, target anchors, and current checker inputs. A structural snapshot race
publishes nothing; the scale-corrected path withholds and reports a project
whose external inputs drift while allowing explicitly covered unaffected
projects to assemble one partial batch. Only one batch is active; manual
`jscout index` deletes active and staging checker state unconditionally.

`jscout watch --enrich` makes replenishment automatic. Each relevant event is
debounced, indexed first, then enriched. A checker failure leaves the current
snapshot without checker edges unless the scale-corrected planner reaches a
controlled partial activation with explicit coverage. Transient failures remain
phase-retryable. A partial activation containing only deterministic project
failures completes that watch generation as partial and is attempted again on
the next structural generation or periodic reconciliation. Worker and whole
sidecar process crashes/exits use this project-terminal path; recognized
launch/request/transport/resource failures remain phase-retryable.
External-input watching, bounded carry validation, and generation cancellation
belong to the watcher coordinator; the fixed-snapshot path remains stateless.

### Required G10 scale correction

**Implementation status (2026-08-14, amended 2026-08-22).** The correction
below is implemented in checker protocol v4 and schema v29: complete
configured-project coverage, package-policy admission of runtime orphan scopes
by default, exhaustive inferred-project coverage under `--all`, manual planning,
configuration-only ownership discovery, package/file spread ordering, bounded
per-project batches, grouped inferred scopes by nearest package/compiler family
with deterministic 150-root subdivision, per-file inferred failure attribution,
one disposable Program worker per project, once-per-project source mapping,
durable batch staging/resume, controlled partial activation,
input/target/snapshot rechecks, resource progress, and synchronous worker-crash
details. Unit, protocol, projection, and small end-to-end gates pass. The
n8n full-plan dry run selects all 121,060 eligible occurrences from 284,183
discovered across 234 owning/inferred projects in 5.6 seconds after excluding
645 exact namespace-member calls already answered by the structural resolver; a real bounded
100-occurrence/three-project slice completes 300 project answers in three
protocol requests while reclaiming each worker between projects. The full real
n8n/Twenty runs and sustained-churn G12 coordinator gate remain open, so the
top-level operational qualification is intentionally unchanged.

The TypeScript semantic operation stays in Node. OXC and Rust do not attempt to
reimplement TypeScript's version-specific type system, configured-project
semantics, ambient declarations, aliases, unions, overloads, or declaration
ownership. The correction moves planning, resource policy, durable progress,
and publication into Rust while making the Node boundary coarse-grained and
memory-bounded.

The current exhaustive path has five specific defects that must be removed:

1. Candidate selection accepts every repository/workspace member call whose
   property name appears on any indexed symbol. Common names therefore create
   large low-value plans even when deterministic resolution already explains
   the call or the file is a test/fixture.
2. Rust and Node reread and hash the same file for individual occurrences, and
   the protocol makes one round trip per occurrence.
3. Node searches project ownership repeatedly, constructs a complete
   TypeScript `Program` for each encountered `tsconfig`, and retains every
   program/checker/input manifest in an unbounded process-wide cache. Overlapping
   monorepo projects retain duplicate source graphs.
4. Rust holds all pending facts until the final transaction. A crash loses the
   complete run and leaves no resumable progress.
5. Final validation clears the program cache and rebuilds every used project,
   repeating the most expensive operation before publication.

#### Rust-owned plan and budgets

`jscout enrich --dry-run` must produce a deterministic plan before TypeScript
program construction or type queries. Planning has a Rust candidate phase and
one full-inventory, configuration-only ownership observation followed by an
admitted-scope plan that reuses the same configured-project discovery; neither
builds a project `Program`. The plan reports discovered, eligible,
selected, and skipped occurrences by file role, package/area, property, file,
and planned project ownership. It pins the structural snapshot, selection
policy, and ordered occurrence IDs in a plan fingerprint.

The default plan:

- includes `repository`/`workspace` production and unknown-role files;
- excludes test, fixture, generated, and documentation roles unless selected;
  explicit role selection can include configured files, while orphan files in
  those roles still require `--all`;
- excludes occurrences already explained by a direct, occurrence-bound
  `certain` or `likely` structural edge (including namespace-member calls and
  bounded receiver value-flow answers resolved through the module/export
  graph); line or name coincidence is never sufficient. Receiver value-flow
  answers remain excluded under `--all`; other deterministic answers may be
  included for audit;
- admits unowned production/unknown-role files package by package: a strict
  unowned majority makes the package JS-first, while a TS-first package admits
  only files reachable over non-type module edges from package runtime targets;
  other unowned files remain fully available to deterministic structure, FTS,
  embeddings, and retrieval;
- requires at least one current property-hub target candidate;
- ranks exported/entity/workflow boundaries and watcher-supplied changed files
  ahead of unanchored internal calls;
- spreads selection within each rank tier by deterministic round-robin across
  packages, then across files within each package, with occurrence ID as the
  final in-file order; lexicographic package, file, anchor, or property order
  must not let one prefix monopolize early staged progress or an explicitly
  capped run;
- selects every eligible occurrence with a configured project owner; batching,
  project-worker disposal, and durable staging bound resources rather than
  discarding configured-project coverage.

Repeatable `--file`, `--package`, `--member`, and `--role` selectors narrow the
plan. `--max-occurrences N` is an operator-requested runtime cap applied after
the deterministic spread order; without it, manual `jscout enrich` has no
occurrence-count cap. `--all` broadens eligibility to normally excluded roles,
already `certain`/`likely` calls, and every synthetic inferred project for audit
or diagnostic runs. The package-policy gate precedes this cap. Hitting an
explicit cap is successful partial enrichment only when the report and stored
batch coverage expose the omitted count. An occurrence without a checker fact
keeps the existing `possible` property-hub path, so bounded coverage cannot
fabricate certainty or create a false negative.

Rust owns source-hash verification and caches it once per distinct file for the
run. It also owns declaration-to-anchor mapping, selection coverage, budgets,
staging, resume, final source/target/snapshot checks, and projection activation.
Node never receives database access or repository source contents over the
protocol; it receives repository-relative paths, indexed hashes, and spans.

#### Project scheduling and batched protocol

Project discovery builds one reverse file-to-owning-project index. Ownership is
enumerated once for the planned file set, not rediscovered for each occurrence.
Conflicting owners remain visible under the existing ambiguity rules.

Configuration-only planning uses a protocol session: Rust uploads
repository-relative paths in byte-bounded frames, the worker performs one
finish-time configuration discovery and global inferred-scope grouping, and
Rust consumes byte-bounded result pages. Upload or result boundaries cannot
change ownership, scope IDs, membership fingerprints, or the 150-root cap.

Rust schedules one selected project at a time, with configured projects before
inferred projects inside each dirty/clean priority tier. A project worker
constructs one TypeScript `Program`, resolves bounded batches grouped by source
file, returns the results plus one project input manifest/fingerprint, and then
exits. The host must not retain an unbounded `programCache`; any future
parallelism is explicit and capped, with one as the default until measurement
justifies more.

`resolve_members` replaces per-occurrence `resolve_member` in the hot path.
Each frame contains at most 512 occurrences and at most 1 MiB of serialized
request data; responses obey the same byte bound and split before exceeding it.
Within a project, each source file is read, hashed, converted from byte to UTF-16
coordinates, and walked into an occurrence map once. Configuration problems,
TypeScript identity, effective options, and input manifests are emitted once
per project rather than copied into every occurrence response.

The project fingerprint is computed during the one program construction. After
its final query batch, the worker rehashes the exact input manifest without
rebuilding the `Program`; Rust rechecks the returned files before activation.
The current destroy-everything-and-rebuild validation pass is removed.

#### Inferred-project coverage amendment

Files without a configured TypeScript-project owner are not missing from the
repository index. They retain chunks, symbols, imports/references, heuristic
member-call edges, FTS, embeddings, and every retrieval surface. The checker
adds only occurrence-specific receiver-type evidence. Default enrichment groups
admitted orphans instead of constructing one synthetic Program per file. A
package is JS-first only when more than half of its production/unknown-role
indexed sources are unowned; all of its unowned default-role sources are then
admitted. In a TS-first package, only unowned default-role sources reachable by
non-type module edges from `main`, every non-type `exports` condition, `bin`, or
script file targets are admitted. Package boundaries are independent, runtime
reachability may cross them, and test/fixture/generated/documentation roles stay
excluded by default. `--all` admits every orphan role.

Admission and final scope planning share one complete configured-ownership
snapshot. Its fingerprint records every source role and package, the selected
and excluded configured-owner sets, tooling fallback, package majorities,
resolved runtime seeds, admitted paths, manifest content, and each absent
`package.json` probe between a source and its boundary. A fresh complete
inventory is compared immediately before exact reuse or activation, while
manifest content and absence are rechecked inside the activation transaction.
Any ownership, boundary, or policy drift retains staging and requires a retry.
Reports expose files and eligible occurrences outside configured projects plus
the occurrences skipped by this gate.

The follow-up lands through five independent cache and measurement boundaries,
in this order:

1. **Implemented 2026-08-22.** Keep a fully mapped closed checker set of one to
   three declarations at `likely`, record `candidateCount`, and version both
   batch reuse and per-project watch carry so rows produced by the former
   single-target rule cannot be resumed or carried.
2. **Implemented 2026-08-22.** Replace per-file inferred projects with scopes
   grouped by nearest `package.json`, compiler family, and deterministic
   directory bins capped at 150 roots. Keep the existing ESNext + Bundler
   options in this layer, include full scope membership and the nearest manifest
   in freshness identity, schedule dirty scopes first by earliest pending
   occurrence rank, and retain per-file failure attribution so one bad root
   cannot fail its whole scope. Logical checker facts must remain unchanged on
   the pinned parity corpus.
   On ai-pipe, all 1,412 mapped repository fact payloads remained identical.
   Coverage did not: 587 occurrences moved from `unknown` to external
   `@types/node` declarations because a typed sibling's transitive declaration
   graph became visible across the shared Program.
3. **Implemented 2026-08-22.** Replace the binary inferred-project gate with a
   per-package decision. A package whose non-test indexed source is mostly
   unowned is JS-first and admits its unowned non-test scopes by default. A
   TS-first package admits an unowned file only when the import graph reaches it
   from a `main`, `exports`, `bin`, or `scripts` manifest target. Tests remain
   role-excluded, `--all` remains exhaustive, and watch uses the same decision.
4. **Implemented and measured 2026-08-22.** `node-esm` uses NodeNext module and
   resolution semantics. `node-cjs` uses the paired NodeNext mode so file and
   package context supplies CommonJS semantics without losing modern package
   `exports`/`imports` resolution. `bundler-jsx` retains ESNext plus Bundler.
   Normalized effective options are part of inferred configuration fingerprints
   without changing configured-project fingerprint identity, so pre-change
   facts cannot be reused after the switch. On ai-pipe, the default 116-edge
   plane and exhaustive 1,382-edge
   inferred plane both had zero bidirectional logical-fact delta. On n8n, all
   31 inferred scopes (2,524 occurrences) retained the same 39 stored and
   projected facts. Coverage-only status changes were bounded to three
   occurrences on exhaustive ai-pipe and two on n8n; SQLite integrity,
   snapshots, selections, confidence, multiplicity, spans, receiver types, and
   candidate counts matched.
5. **Implemented and measured 2026-08-22.** Add bounded structural receiver
   value flow for `this`, direct construction, immutable aliases, and closed
   factory-return sets to depth two. Schema v28, extraction v6, and projection
   v12 persist exact binding/reference shape and hard-exclude the resulting
   occurrences from checker selection, including under `--all`. ai-pipe
   produced 557 answered occurrences and 1,025 edges. All 557 are retained from
   a 669/669 bidirectionally exact pre-pass checker oracle; awaited values and
   other unsafe cases were subsequently removed without changing any retained
   target set. Its exhaustive checker plan fell from 5,158 to 4,601 occurrences
   exactly, and the full provider-free run completed 4,601 queries in 42
   batches, published 387 checker facts (1,412 combined with value-flow facts),
   then reused all 4,601 with zero checker requests. A normalized digest ratchet
   covers the final 557 occurrences and 1,025 edges after indexing, enrichment,
   and unchanged reuse. n8n produced 14,414 answered occurrences and 14,456
   edges; exhaustive selection fell from 284,184 to 269,770 exactly. A fresh
   stratified n8n sample covered 61 occurrences across every emitted
   flow/cardinality class: all 27 closed mapped checker facts matched
   bidirectionally, while the remaining 34 occurrences had no mapped checker
   fact. Candidate, checker, and sample databases passed integrity and
   foreign-key checks. Merging two full-AST scans and caching the exact-ref SQL
   statement cut the isolated n8n receiver-flow projection from 906 ms to
   380–385 ms without changing its normalized target-set digest. Three paired
   release-mode n8n cold-index runs then averaged 19.150 seconds before and
   19.536 seconds after (+2.0%, with noisy individual deltas from -3.2% to
   +9.7%). Five direct ai-pipe runs averaged 0.466 seconds before and 0.497
   seconds after (+6.6%, or 31 ms). The first fresh restricted n8n enrichment
   exposed an unindexed correlated receiver-flow lookup during checker
   projection: 3,836 facts against 869,952 edges took 374.62 seconds end to end.
   Preloading the 14,414 resolved occurrence IDs once reduced the identical
   run to 62.36 seconds while retaining 6,554 selected occurrences, 14,265
   queries, 114 request batches, and 3,836 canonical facts exactly; unchanged
   reuse completed with zero requests in 11.03 seconds. Unsupported cases
   remain on the property hub.

#### Real-repository validation and remaining items (2026-08-23)

The stack at `6a93b0d` (#76–#80 before the projection-scan fix `3fb30ef`)
was run against the pre-stack binary (`1e9acac`) on ai-pipe `ea13166` and
n8n `9d9e9bf` with scratch databases and no billed calls; the full record is
`docs/checker-stack-validation-2026-08-23.md`. Its n8n restricted-enrichment
timing is therefore pre-fix; the post-fix figures recorded in item 5 above
supersede it. Headline measurements:

- ai-pipe `enrich --all`: 355.06 s (458 one-file Programs, 1,412 facts) →
  29.30 s (12 projects, 387 checker facts, 1,412 combined with value flow).
  Default gate: 17.16 s cold, 0.30 s unchanged reuse. Occurrences with a
  `likely` member-call edge: 108 → 694 (557 value flow + 137 checker), none
  lost.
- n8n restricted enrichment (`n8n-workflow` + `@n8n/db`, 6,554 selected
  occurrences, 3,836 facts): 374.62 s cold at `6a93b0d`; 62.36 s cold and
  11.03 s unchanged reuse after `3fb30ef`, identical facts.
- n8n index: 21.39 s → 22.09 s (+3%, +26 MB). Value flow: ai-pipe 1,025
  edges over 557 occurrences; n8n 14,456 over 14,414. Hand check of 24
  sampled occurrences at source (13 ai-pipe, 11 n8n; spread over files and
  flow kinds rather than seeded random): 24 correct, 4 over-approximate
  (`openDatabase(path, { driver })` with an explicit driver keeps the dead
  second adapter at `likely`). The parameter-property limit held: zero
  `this.x.y()` edges on n8n.
- Watch on an ai-pipe copy: a server edit re-indexed in 279 ms and
  re-checked only the dirty `inferred:.#node-esm/server~1` scope in 4.6 s,
  and the new call received a value-flow edge; a test edit re-checked no
  scope.
- A schema-6 database is refused by read-only commands and by `index`
  (below the durable floor, file untouched); v26 → v28 migrates in place.

Findings, in product order, with the decision taken:

1. **Where the edges land.** Value-flow edges sit mostly in tests (ai-pipe
   499/557, n8n 9,071/14,414); only 9 are in ai-pipe `server/`. Production
   code receives its instances as parameters — 170 `db.*` call sites in the
   server are on parameters — and the checker answers none of those either:
   the five inferred server/scripts scopes published 0 facts by default (833
   unknown, the rest lib or vendored). Construction-site resolution is solved
   and nearly free; the remaining lever for JS-first code is
   argument→parameter flow, not more checker work. Design, not yet scheduled;
   see the decision record below.
2. **CLI `who-uses` ignores enriched edges.** `commands/core.rs` name-matches
   only, so its output for `SqliteAdapter.query` is identical before and
   after the stack while the database holds 11 `likely` in-edges. The MCP
   `who_uses` tool already uses the anchor query. Fix: route the CLI through
   `who_uses_anchor_in_origins` when the target resolves to one anchor. Small
   PR.
3. **Overload signatures.** TypeScript overloads map to one anchor per
   signature (`@1..@4`), so a four-signature method demotes to `possible`
   with `candidateCount 4` (three n8n occurrences). Fix: collapse signature
   declarations onto the implementation before counting targets. Small PR.
4. **Multi-owner querying.** An occurrence is queried once per owning
   tsconfig: n8n selected 6,554 and queried 14,265, storing facts two to
   three times. By design today. Two concepts replace it:
   - a per-owner **freshness manifest** — config, package, compiler, and
     source inputs plus the negative discovery probes needed to prove a
     cached result current. It decides whether a cached answer may be
     reused and is never a comparison key between owners;
   - a **semantic Program signature** — TypeScript runtime identity,
     normalized effective options, normalized roots and references, and
     the complete resolved source-input identities and hashes, excluding
     project and config labels. `tsconfig.json` and `tsconfig.build.json`
     owners of the same files compare equal under it when their effective
     Programs are identical, which is the case behind the 14,265 queries.
     A positive-only manifest cannot prove that automatic `@types` or
     ambient discovery gained no input, so the signature is computed from a
     built Program, and reusing a recorded signature additionally
     fingerprints the discovery directories (or negative probes) it
     depended on.

   Cold path: the worker computes the signature when it builds each
   owner's Program, before `resolve_members` (the `validateInputs` path
   already builds without resolving and can carry it). Owners with equal
   signatures form a group; one representative is queried and its answers
   are attributed to every owner in the group. Every owner still builds
   once on the cold path; the saving is the queries and the duplicated
   facts. Warm path: a recorded signature with a fresh manifest lets equal
   owners skip the query without a build. Acceptance is a fresh n8n run,
   not a reuse run: query count below 14,265 with facts identical to the
   single-owner baseline. Cost only, and the strongest measured efficiency
   item in this record.
5. **Zero-fact narrowed runs.** A plan-scoped run on a snapshot that already
   has an active batch and yields zero facts (`--file scripts` after a
   package run) exits 1 without a summary and cannot reuse, because the
   retain-previous-batch safeguard treats it as a failed replacement. Manual
   only — watch opens a new snapshot per generation. Treat it as a
   successful no-op: record a completed, inactive zero-fact batch that exact
   reuse can match — reuse currently considers only active batches — while
   the prior active fact batch stays active.

**Decision record: argument→parameter flow.** This is the first
interprocedural step and therefore the first that rests on an assumption the
code cannot prove: that every caller of a function is visible. The choices
and the recommended rule:

- Non-escape is the admission condition: every in-scope
  production/unknown-role runtime reference to the function binding, local
  and through imports, must be an indexed direct call. An alias, a callback or property value, a re-export to an
  unresolved consumer, a dynamic import, or any reference the index cannot
  see (files outside the walk such as `.github/`, extensionless executables)
  leaves the set open, and the occurrence keeps the property-hub/checker
  path. "Unpublished package" and "every import resolves in-repo" are
  necessary, not sufficient.
- Closure is over production/unknown-role references only — the same role
  set the checker's default eligibility uses. Test, fixture, generated, and
  documentation-role references are ignored entirely: they are neither
  closure blockers nor candidate contributors, and the emitted edge records
  that role scope in its detail. The set is therefore closed over
  production callers, not over every caller in the repository; that is
  the stated meaning of `likely` here, and a misclassified role is a
  known limit shared with checker eligibility. The closure scope is a
  first-class edge attribute — `scope: production` against
  `scope: repository` — stored beside the confidence, not inside free-text
  detail: exact `who_uses` reads only `detail_json.$.detail` as optional
  text today, so a structured field there would be dropped while `likely`
  surfaced globally. Every confidence consumer — `who_uses`, neighborhood
  expansion, search follow-ups and `used_by` counts, checker eligibility
  and filtering — propagates it, with an acceptance test per consumer.
  Without that propagation the rule falls back to repository-wide
  closure. The alternative — test
  references as blockers — would leave ai-pipe's sets open for no
  production reason: 443 tests pass a real `SqliteAdapter`, 156 pass
  awaited helpers, and some pass object-literal fakes.
- Disagreeing call sites union into one closed set under the ≤3 rule, as
  factory returns do.
- Depth 2. `api.mjs` → route-handler parameter → `db.mjs` parameter is the
  real shape; depth 1 leaves the set open on `saveWorkspace` itself.
- `likely` under its own provenance (`parameter-flow`) so it can be audited
  or disabled separately; resolved occurrences are excluded from the checker
  by edge, as value flow is.
- Measure before building, with a bounded AST probe rather than SQL: the
  index stores call spans, references, and chunks but no argument→parameter
  relation (the existing `calls` query reparses candidate files). The probe
  answers how many of ai-pipe's 170 parameter-bound `db.*` sites close under
  the rule above; that number decides whether to build.
- TypeScript annotations yield the same edges from a syntactic read of the
  parameter type. That is the erasure line and stays a separate decision.
- Sequencing: this is graph coverage, not a performance item. A measured
  prototype follows the user-facing completeness work in G22–G23 (#83) and
  the bounded correctness items above.

Sequence agreed in review: close G20b through its pending reproducible
replay; G22 then G23 (#83); the bounded correctness items (2, 3, 5);
multi-owner deduplication under the semantic signature (4); then the
parameter-flow probe (1) and sidecar parallelism (6), each only on its own
measurement.

Remaining items from the optimization sequence:

6. **Bounded sidecar pool.** Productize `codex/checker-sidecar-experiment`:
   typed configuration for worker count and RSS recycle threshold, an
   aggregate memory budget, one fresh-worker retry on crash, and the five
   test classes the experiment listed. Premature after grouped scopes,
   which removed most of the duplicated Programs the experiment
   parallelized. Re-measure after multi-owner deduplication and productize
   only if checker execution remains dominant under a bounded aggregate RSS
   budget; the right default may be two workers or none.
7. **TypeScript backend, later.** Move the pinned 5.9.3 to 6.0 when
   convenient (the API baseline TypeScript 7 targets). When 7.1 ships its
   API, prototype a native worker behind the same sidecar protocol with
   digest parity as the gate; a one-day typescript-native-bridge spike only
   if the checker is still the bottleneck on TS-heavy corpora after items 2
   and 6.

Deferred with item 7, no evidence either is the next bottleneck: `node-esm`
and `node-cjs` now carry identical options and could merge into one `node`
family (a scope-ID change), and the NodeNext extensionless trade-off is
unmeasured on bundler-style orphan `.ts`/`.js` files in `type: module`
packages.

#### Durable staging, resume, and partial coverage

An enrichment run is keyed by structural snapshot, plan fingerprint, checker
protocol/version, TypeScript identity, and execution policy. Rust commits
bounded inactive staging rows after each successful query batch or project.
Restarting the same command resumes the matching run. An already published
partial batch is immutable: its reusable rows are cloned into a new inactive
batch before retry, so interruption or a fingerprint reset cannot split
canonical facts from `resolved_edges`. An occurrence with any failed owner is
not cloned; every owner is re-queried so a repaired closed set can regain
`likely`. A changed snapshot or plan starts a new run and makes old inactive
staging rows collectible. Staging has a bounded retention policy and never
enters `resolved_edges`.

One failed project does not erase successful work from unrelated projects. Its
occurrences publish no targeted edge, its coverage is recorded as failed, and
the command reports partial failure. Partial activation requires at least one
completed project; an all-failed run exits non-zero and cannot retire a
previously active batch. A zero-fact partial run likewise cannot replace an
existing active batch for the same snapshot. An occurrence owned by a failed,
cancelled, or unprocessed project cannot receive a `likely` checker edge from a
different owner; it remains on the `possible` fallback unless every owning
project needed by the confidence decision completed. Malformed or internally
inconsistent answers still reject the affected project atomically.

Grouped inferred scopes additionally isolate a failure after source-hash
validation to the affected source file. Every selected occurrence from that
file receives explicit failed coverage; previously staged facts from the same
file are removed, while sibling-file progress and the validated Program input
manifest remain staged under a `partial` project run. Exact reuse and
cross-snapshot carry accept only `completed` runs, so the next matching command
retries the failed files. If the resumed worker reports a different Program
fingerprint, Rust discards the whole scope staging and reruns it coherently.
Discarded rows are not reported as resumed, and a second fingerprint drift in
the clean rerun fails the project instead of entering a restart loop.
Freshness includes the nearest manifest plus negative `package.json` probes
between every root and that boundary, so a newly created closer manifest
invalidates planning and execution even when no manifest existed originally.
Missing or changed inputs, Program construction, protocol/ownership failures,
and configured-project failures remain project-atomic. A scope in which every
file fails is `failed`, not `partial`, and retains the all-failed publication
safeguard.

Final activation is one short transaction. It rechecks the exact structural
snapshot, selected occurrence/file hashes, mapped target fingerprints, and
project input manifests, then marks one assembled batch active and rebuilds its
checker projection. Drifted project results are withheld and reported; a
structural snapshot race activates nothing. Only active rows are public.

#### Operations and watcher interaction

Progress output names the current project and file and reports projects,
files, occurrences, staged facts, elapsed phase time, and Node RSS/heap usage.
A worker crash must carry the actual Node error/stack through the synchronous
protocol error as well as stderr, together with the active project, file, batch,
and progress counters; independent stdout/stderr drain timing must not erase the
diagnostic. Reports separate program-build, type-query, hashing, IPC, mapping,
and publication time.

`checker doctor` reports overlapping-project counts and largest configured
projects so an operator can see likely cost before execution. Heap overrides
remain diagnostic escape hatches, not the scalability mechanism.

One Ctrl-C cancels the active project worker and aborts the complete enrichment
operation; it is never converted into failed-project coverage followed by work
on later projects or partial activation. Already staged batches stay inactive
and resumable. A second Ctrl-C remains the forced-exit path.

`watch --enrich` uses the same planner, batching, and staging machinery.
Unchanged projects and individually validated facts may carry into a new
snapshot batch; dirty or invalidated projects and occurrences are ranked first,
and only the remaining delta constructs Programs. A run without a compatible
predecessor, `enrich --full`, and the daily drift flush are complete carry-free
passes, and the watcher never implies `--all`. A
newer structural generation cancels between batches, and staged work may resume
only when its exact snapshot and plan still match. After
structural indexing, checker program construction waits for a configurable
enrichment quiet period, defaulting to the G12 two-second trailing quiet period;
any newer event resets that wait. Sustained churn may therefore starve checker
enrichment by design while deterministic indexing continues to converge. A
cancelled enrichment is not immediately relaunched: the coordinator waits for
the next quiet point, then resumes only exact matching staged work or starts one
new plan.

#### Scale-correction acceptance checks

- a 150,000-eligible-occurrence synthetic plan selects all 150,000 by default
  and completes through bounded batches, project-worker disposal, and durable
  staging without an occurrence-count override;
- the same plan with `--max-occurrences 10000` selects exactly 10,000 in spread
  order and reports exact omitted coverage, while `--all` is tested separately
  as an eligibility override rather than a completeness switch;
- a skewed plan spreads each rank tier across packages and files instead of
  spending its budget on one lexicographic prefix, while repeated planning
  produces byte-identical ordering and fingerprints;
- direct `certain`/`likely` structural resolutions are excluded from the
  default plan while unresolved and `possible` property-hub occurrences remain
  eligible and are counted explicitly;
- protocol request count scales with bounded batches/projects rather than one
  request per occurrence, with frame byte/item limits tested;
- peak Node memory is bounded by the largest admitted project plus one response
  batch, not the sum of every project encountered; completed project workers
  are observed exiting before the next large project is admitted;
- the same source file is read/hashed/walked once per owning project, not once
  per occurrence;
- overlapping projects preserve ambiguity and a failed owner prevents an
  unjustified `likely` edge, while repairing that owner re-queries the closed set
  and can promote it back to `likely`;
- an all-failed run and a zero-fact partial run preserve the previously active
  batch, while one Ctrl-C stops the project loop without activating partial
  coverage;
- killing the checker after staged progress and rerunning resumes the exact
  snapshot/plan without redoing committed batches or exposing staging rows;
- a source, target, config, ambient declaration, TypeScript-runtime change, or
  newly appearing package boundary during the run cannot activate raced facts;
- a project failure can publish explicitly incomplete coverage for unaffected
  projects while returning a non-zero/partial status;
- an actual Node exception/OOM is visible in the command's final error even if
  stderr forwarding loses a race;
- sustained filesystem churn starts no checker program before the enrichment
  quiet period, cancels active work at a batch boundary, does not immediately
  restart it, and produces one resumable/new plan after the next quiet point;
- n8n and Twenty full-plan dry runs plus bounded real runs record wall time,
  throughput, peak RSS/heap, selected/omitted coverage, and resume behavior
  before full-repository `enrich` or `watch --enrich` is recommended.

Verification follows the gateway precedent: fake-sidecar protocol,
unknown-type, crash, enforced-timeout, cancellation, and outside-root tests in
the Rust suite. The sidecar's own suite uses a pinned TypeScript library and
small fixtures for nested/`this`/optional receivers, two same-named methods on
different receiver types, inheritance/overrides, multiple declaration
candidates, overlapping `tsconfig` ownership, broken projects, and changed
ambient declarations. Default tests do not launch a repository-sized checker
process. A doctor command reports the resolved TypeScript version, discovered
projects, configuration problems, and sidecar readiness — not diagnostic or
compilation health — before enrichment runs.

### Why checker answers are `likely`, never `certain`

`certain` in jscout means provable from the value graph and module
resolution — a runtime-grade guarantee reproducible from indexed content.
A checker answer is authoritative about what the type system *claims*, and
types are declarations about runtime, not observations of it: `as any`,
stale or wrong `.d.ts`, `@ts-ignore`, unsound generics, and Proxy-backed
dynamic APIs all let declared types diverge from what executes, with no
warning the sidecar could see (it deliberately reads no diagnostics).

The marginal edges make this concrete: where the checker merely confirms
what binding analysis proves, jscout already emits `certain`. The edges
the checker *adds* are precisely the annotation-mediated ones — interface
receivers, DI tokens, generic table maps — that is, exactly the edges
resting on assertions runtime may not honor. Checker answers also depend
on inputs outside the indexed snapshot (lib versions, ambient
declarations, node_modules state), and degrade silently near broken
regions in the explicitly tolerated non-compiling mode. `likely` with
`checker` provenance states all of this honestly: almost always right,
worth verifying when load-bearing, never silently trusted the way
`certain` is. Generated model claims cap at `likely` for a different
reason (non-determinism); checker claims cap there because they are
type-level assertions, not runtime facts. Neither cap is revisited by
observing agreement.

### Out of scope for G10

Diagnostics, rename/refactor safety, call hierarchy, emit, persistent
watch-resident checker daemons, and any checker influence over deterministic
structural facts.
Agents wanting full typed navigation should use an LSP; G10 only closes
the receiver-identity gap inside jscout's own evidence model.

## G11 — fixed-snapshot simplification

G11 makes the normal, non-watcher index path the primary correctness surface
and removes lifecycle machinery that only attempted to preserve cheap derived
facts. It does not change extraction semantics or add another storage file.

Implementation order:

1. Include occurrence-specific checker `member_call` edges in deterministic
   workflow traversal. **Complete.**
2. Bound semantic supports and discard optional semantic overlays before exact
   source hits when a response budget binds. **Complete.**
3. Make the three logical storage lifecycles executable in the existing
   database. `jscout index` clears and rebuilds the disposable plane while
   retaining embeddings and semantic memory, then rematerializes cached vector
   occurrences. **Complete.**
4. Remove checker retention from the fixed-snapshot path. Manual rebuild clears
   every checker batch and enrichment republishes explicitly. Watch-only carry
   is specified separately in G12.2 and does not alter this contract.
   **Complete; watcher behavior amended in G12.2.**
5. Separate query-only database opening from schema creation/migration and
   snapshot publication. Retire the historical in-place migration ladder;
   durable-compatible schemas preserve cache/memory while recreating the
   disposable plane, and incompatible durable schemas fail explicitly.
   **Complete for retrieval surfaces.** `scout --dry-run` still opens a writer
   before planning and is a known command-authority cleanup; `embed` is
   intentionally a writer because it materializes the durable vector cache.
6. Treat watch as a later coordinator over the same refresh/enrich operations,
   with a full-refresh fallback for branch, submodule, configuration, and event
   uncertainty. **Specified separately as G12 below.**

Acceptance checks:

- rebuilding an unchanged checkout reparses the disposable plane but reuses
  content-hash embedding cache rows;
- materialized vector occurrences exactly match chunks in the new snapshot;
- semantic artifacts and their run ledger survive, while evidence freshness is
  recalculated against the new snapshot;
- manual rebuild retains neither checker facts nor old package-instance
  ownership;
- a genuine v15 embedding layout is rejected without mutation, while the v16
  durable floor preserves compatible embedding and semantic-memory rows;
- a fatal required-phase failure never publishes a snapshot marker describing
  new or partially rebuilt structural rows; non-retryable file reads and
  deterministic extraction rejections are reported and excluded from the
  successfully published indexable corpus;
- retrieval-only commands do not create or migrate a missing database;
  semantic dry-run planners should follow the same rule after the noted
  command-authority cleanup.

## G12 — watcher coordinator

**Implementation complete (2026-08-17); sustained-churn validation on a large
real repository remains pending.** The production watcher uses a pure
generation coordinator, a typed full/incremental refresh scope, fresh per-phase
connections, explicit optional embedding/checker phases, supersession and
cancellation, uncapped phase-error retries with a capped exponential delay,
exact self-output exclusions, dynamic external coverage, and periodic
reconciliation. Unit and fixture coverage passes; the next operational step is
to run it through branch switches and ordinary edits on the user's target
repository.

G12 brings `jscout watch` under the fixed-snapshot architecture. The watcher
is an in-process coordinator over the same explicit operations used outside
watch mode; it is not an independent indexing implementation and does not
make incremental state the product's correctness model.

### Contract

The watcher guarantees eventual convergence to the current checkout while it
is running. A successful generation executes:

```text
full or incremental structural refresh
  -> rematerialize vectors already present in the content cache
  -> optionally embed unseen content (`--embed`)
  -> optionally enrich the exact published snapshot (`--enrich`)
  -> optionally embed/sync current semantic artifacts (`--embed`)
```

Startup and structural-boundary generations run a full disposable-plane
refresh. An ordinary bounded batch of JavaScript/TypeScript source paths, an
admitted Markdown/MDX event, or periodic reconciliation uses incremental
extraction: it still walks and hashes the complete current shared code-and-doc
inventory, but preserves unchanged rows and parses/replaces only changed or
missing files. Documentation events do not enter checker dirty affinity.
Dependency discovery, module resolution, snapshot
calculation, hidden old-checker-batch retention/retirement, vector occurrence
rematerialization, and projection publication still run against the complete
resulting snapshot. `jscout index` remains a full rebuild and always clears
checker state; the incremental path and checker carry are watcher latency
optimizations, not a second correctness model.

A source batch is promoted to full refresh when it contains more than 256
distinct paths. Git HEAD or submodule controls, source-inventory ignore files,
package/workspace manifests, lockfiles, tsconfig/jsconfig and declaration
inputs, selected dependency roots, external checker inputs, pathless events,
and backend errors also require full refresh. Non-boundary directory and
uncertain missing-path events select complete-inventory incremental refresh;
the full inventory, not the event path, remains authoritative. Full scope is
sticky within a generation, so a mixed event cannot be downgraded by later
source notifications. A changed file with a non-retryable read or deterministic
extraction failure is reported and excluded rather than leaving its previous
structural row live. The operation still publishes the indexable corpus
successfully. A recognized transient read failure instead rolls back the
transaction and fails the refresh for retry.

G12 does not promise uninterrupted queries during refresh. Publish-then-swap,
database generations, or a second structural database would add lifecycle
machinery that the fixed-snapshot design intentionally removed. Existing
n8n/Twenty reports put repository indexing between roughly 7 and 50 seconds,
depending on the enabled planes and checkout; these are scale observations,
not a latency target. A query may report that no snapshot is published for the
entire structural-refresh interval, and every cycle logs its actual phase
durations.

`--embed` and `--enrich` remain explicit. `--product` is subordinate to
`--embed` and applies the same effective-product selection as manual
`jscout embed --product`; a product-only vector cache must not be silently
widened by the watcher. Plain watch performs no model calls, does not start the
TypeScript checker, and never serves checker facts from a different structural
snapshot. It may retain the active publication and newest reusable superseded
staging source hidden as future carry inputs, but only `watch --enrich` can
publish a replacement bound to the current snapshot. An
exact-snapshot batch remains a no-op reuse. Dependency selectors remain
authoritative and must be supplied to watch exactly as they are to index.

Watcher startup telemetry records the jscout version, executable-byte
fingerprint, non-secret loaded runtime-config fingerprint, config-loaded and
restart-required semantics, checker-policy fingerprint derived from the actual
watcher enrichment selection, effective-watch-policy fingerprint after CLI
overrides, and effective phase flags. Repository snapshots and dirty paths
remain per-generation state rather than part of those runtime identities.
Executable fingerprint acquisition is best-effort: failure is logged and
rendered as `unavailable`, never promoted into a watcher or MCP startup error.

Code embedding remains ahead of checker enrichment because its document and
selection inputs are chunk content plus current repository policy; checker
tables are not embedding inputs. When `--embed` is enabled, a separate semantic
tail runs after enrichment, or immediately after code embedding when enrichment
is disabled. It embeds missing current semantic documents through the durable
cache and repairs the semantic vector index. Both embedding phases report
missing, embedded, cached-reused, and current synced-occurrence counts.
Repository scouting remains an explicit generative operation and is not hidden
inside G12 watch.

### Generation state machine

Filesystem notifications are wake-up hints, not proof that the index is
current. Except for the immediate startup refresh, the transition from dirty
to refreshing waits for a configurable trailing quiet period, default two
seconds. Each relevant event resets the quiet period. The previously published
snapshot remains queryable while waiting; continuous editing delays refresh
rather than causing back-to-back full-refresh outages. Events received during
a refresh begin a new quiet period after that refresh finishes.

The coordinator owns a monotonically increasing desired generation and
retains dirty state until every required phase for that generation has
finished:

```text
clean
  -> dirty(generation, reasons, full|incremental)
  -> refreshing(generation)
  -> embedding-code(generation, snapshot)   [only with --embed]
  -> enriching(generation, snapshot)   [only with --enrich]
  -> embedding-semantic(generation, snapshot)   [only with --embed]
  -> clean

any phase + newer event -> dirty(newer generation)
any failed phase        -> retry-wait(same generation, phase) -> retry
```

Events received during a phase are not consumed by that phase. They advance
the desired generation and force another structural refresh before the
watcher can become clean. Structural work is allowed to finish rather than be
cancelled mid-transaction; optional embedding work stops between batches and
checker work terminates its bounded sidecar when superseded. Before starting
each optional phase, the coordinator drains pending events and skips that
phase if a newer structural generation is already required.

A structural refresh may return individual file rejections. `jscout index`
reports every rejected path/stage/error. The watcher reports full details once
per distinct rejection set, reports once when the entire set clears, and keeps
`rejected=N` in every refresh summary. Both publish the indexable corpus as a
successful, clean generation. Non-retryable read failures and deterministic
parse rejections are subject-local: a whole-repository retry cannot repair
binary media with a source-looking extension or a permanently protected file.
A later file event or periodic reconciliation naturally tries the path again.

Read-error disposition is one explicit rule. Descriptor exhaustion,
interrupted or timed-out I/O, connection/network failures, stale handles,
and temporary resource pressure are retryable phase errors. Unknown errors and
permission denial are rejected inputs so a single permanently inaccessible
file or subtree cannot wedge watch forever. A path that disappears or changes
between file and directory after inventory is checkout churn: its old row is
removed, and a later event or reconciliation converges on the next state. The
walker applies the same classification to directory and ignore-file errors;
retryable I/O aborts while permanent subtree failures remain visible
rejections.
Retryable reads roll back the active transaction and return `Err`; watch
remains dirty and retries even when periodic reconciliation is disabled.
Other database, transaction, discovery, and phase-level failures follow the
same retry path.
Selected-dependency traversal errors are phase failures rather than partial
inventories. One classified workspace map is built before mutation by expanding
declared globs against the filesystem. Package manifests establish identity;
the indexed source inventory only prefers alias targets, with classified
manifest-entry fallback for source-less members. First-party extraction,
dependency discovery from the newly extracted importers, and every selected-
dependency source read are then prepared in the same rollbackable transaction
before the old snapshot publication is invalidated. A retryable acquisition
failure therefore leaves the previous snapshot queryable instead of exposing
an unpublished gap.

Phase-level failures retry without an attempt limit, using exponential delay
capped at 30 seconds. A parked retry gates fresh work for that generation and
is consumed when it starts; delay resets on new input or a successful phase.
Retry state lives in memory. Restarting watch always subscribes first and then
performs a full refresh, so no persistent watcher journal or recovery schema is
required.

### Trigger and reconciliation policy

Relevant events carry a typed refresh scope. Indexed source-file and admitted
Markdown/MDX create, update, delete, and rename paths select incremental
extraction while all resolution, ownership, checkout, dependency, and
uncertain boundaries select full refresh. Scopes coalesce during debounce,
full scope dominates and remains sticky for the generation, and more than 256
distinct source paths promotes the generation to full refresh. The incremental
executor still scans the complete shared code-and-doc inventory and runs
complete resolution and publication, so event paths are optimization hints
rather than the correctness inventory.

Jscout-owned output paths are excluded before relevance classification and
before the unknown-event escalation rule. The exclusion set is exact, not a
broad `.jscout*` pattern: the active database path, its SQLite `-wal`, `-shm`,
and journal sidecars, and explicitly configured telemetry or temporary output
paths. A future watch `--database` option must register the selected path in
this set whether it is inside or outside the repository. An event containing
only excluded paths does not advance the desired generation. This also keeps
two watchers on one database from triggering each other with SQLite writes;
database locking remains a separate concern handled below.

Triggers include:

- indexed source create, update, delete, and rename events;
- `package.json`, `pnpm-workspace.yaml`, source-inventory ignore files,
  supported lockfiles, tsconfig/jsconfig files, declaration files, and other
  resolver configuration;
- resolved Git worktree `HEAD` control paths, including branch switches and
  worktree-specific Git directories; source notifications cover checkout
  changes without treating routine `.git/index` writes as rebuild triggers;
- `.gitmodules`, submodule control paths, and submodule worktree changes;
- selected dependency locator entries and canonical package roots, including
  pnpm/Yarn symlink replacement and package installation changes;
- exact checker inputs reported by the most recent enrichment pass, including
  TypeScript, ambient declarations, configs, and inputs under `node_modules`
  or an external package store;
- notification overflow, backend errors, or unknown non-excluded event shapes.

Existing regular files that fail every relevance rule are ignored rather than
escalated: documentation excluded by the configured corpus policy, editor
metadata, and other unindexed files therefore do not rebuild the repository.
Pathless/rescan events remain conservative full-refresh triggers. A directory
or uncertain missing path that is not already a recognized boundary schedules
complete-inventory incremental refresh, which discovers all descendant changes
without resetting unrelated canonical rows.

After each refresh or enrichment, the coordinator reconciles its narrow
external watches with the newly resolved package instances and checker input
set. These paths are ephemeral coordinator state, not a cross-snapshot
freshness manifest stored in SQLite.

Failure to register a narrow external watch marks that path as `degraded`
coverage immediately. Registration is attempted again whenever targets are
reconciled: after a successful refresh or enrichment, on the next periodic
reconciliation, or when the target set changes. It has no independent retry
timer. Persistent registration failure does not itself keep the structural
generation dirty or cause a full-refresh loop.

Notification backends can miss events, so a configurable reconciliation timer
(default ten minutes) schedules a complete-inventory incremental refresh even
when no event arrived.
The interval starts when a generation completes, not when its timer fires, so
a slow refresh cannot cause back-to-back cycles. A nonzero interval must exceed
the debounce period, and reconciliation starts immediately when due rather
than entering the edit debounce path.
Setting the interval to zero explicitly gives up bounded recovery from missed
events and degraded watch coverage. The default deliberately spends cheap
structural work instead of building another durable fingerprint system. The
timer, watcher errors, and uncertain ownership all enter the same dirty
generation path after the self-output exclusion has run.

### Phase and database ownership

Only one phase runs at a time. Each phase opens its own SQLite connection and
closes it on completion; watch does not retain a writer connection that one
failed transaction can wedge forever. Writer connections use a finite
`busy_timeout`, and lock contention enters the normal retry path.

Every manually managed transaction must roll back on every error, including a
failed commit. The coordinator serializes its refresh, embedding, and
enrichment phases, while SQLite remains the arbiter when another jscout
process writes concurrently. G12 does not introduce an application-level
lease until a demonstrated concurrent-writer failure requires one.

A watcher structural refresh bounds inactive checker staging to the newest
reusable superseded source and may retain it beside the prior active
publication as hidden carry sources. An empty newer destination left by a crash
cannot displace a completed source. The successor prefers a valid fully
completed staging project, falls back to the active project, and never carries
incomplete project rows. Validated copying and predecessor retirement are
atomic. Projection still requires an exact source-snapshot match:

- plain `watch` starts no checker work and never projects a retained old batch;
- checker-dirty code paths accumulate across supersession, cancellation,
  retries, and terminal partial publication, and clear only after a
  non-superseded successful checker publication; documentation paths never
  enter this backlog;
- enrichment telemetry separates exact-batch reuse, staging resume/reset,
  unique occurrence carry, owner-occurrence carry, and active-versus-staging
  carry sources;
- `watch --enrich` reuses an exact-snapshot batch as a no-op, or carries only
  projects whose config-chain/membership/checker fingerprint is unchanged and
  facts whose source occurrence and target fingerprint still validate;
- any multi-project occurrence that cannot carry in every owner is re-queried
  in every owner; changed/dirty projects and occurrences execute first;
- manual `jscout index` clears every checker batch, while manual
  `jscout enrich --full` and the daily watcher drift flush recompute without
  reuse or carry;
- a failure before controlled activation leaves checker facts absent; a
  scale-planner partial activation exposes only its explicit coverage and keeps
  failed project coverage pending for retry;
- a newer structural generation always takes precedence over retrying
  enrichment for an older snapshot.

Embedding failure does not invalidate an otherwise current structural
snapshot. It leaves the embedding phase pending and retries missing hashes;
the durable cache makes completed vectors reusable. No watcher failure may
delete semantic memory or content-hash embedding rows.

### Implementation order

1. Extract a coordinator with injectable event input, clock, and phase
   executor. Track desired/completed generations, dirty reasons, per-phase
   retry state, debounce, degraded external-watch coverage, and structured
   cycle telemetry without timing-dependent tests.
2. Replace the pre-G12 watch loop with the normal full-refresh operation. Open
   a fresh connection per phase, configure `busy_timeout`, audit rollback
   paths, report and skip file-local rejections, and make fatal phase failures
   retry automatically.
3. Prevent cross-snapshot checker projection; sequence optional embedding and
   exact-snapshot enrichment, and add generation checks plus cancellation
   between/within optional work. **Amended by G12.2 carry validation.**
4. Add exact self-output exclusion plus Git/worktree, submodule,
   selected-dependency, and dynamically reported checker-input watches. Treat
   notification backend errors as full-refresh uncertainty and persistent
   registration failures as degraded timer-backed coverage.
5. Add periodic reconciliation, uncapped retry with a capped exponential
   delay, concise generation and phase logging, then remove assumptions that
   another repository event is required to recover from failure.
6. Update README operational guidance after the coordinator acceptance suite
   passes.
7. **G12.1 amendment (2026-08-17):** promote the already parity-tested
   incremental extractor to a production watcher operation. Add typed event
   scope, sticky full fallbacks, a 256-path promotion bound, fail-closed stale
   row removal, hidden checker-source retention, and refresh-scope telemetry.
   Keep manual `index` full-refresh-only.
8. **G12.2 amendment (2026-08-20):** add watch-only per-project carry-forward.
   First require current `member_calls.rowid` in projection; then fingerprint
   config chains, membership, checker identity, and protocol during planning;
   rebind individually validated facts into a new snapshot batch; execute the
   remaining delta with dirty affinity; and schedule an independent daily
   carry-free flush. Manual indexing remains carry-free with no retention flag.

G12.2 has no rollout flag. Before merge, development validation compares the
canonical carried result with a carry-free `enrich --full` result (byte-equal
after excluding projects intentionally rechecked) and records enrichment wall
time on a pristine Next.js checkout, one of n8n/Twenty, and the user's
monorepo. These are implementation measurements, not a runtime gate or a
permanent dual-execution mode.

Validation is recorded in
[`docs/checker-watch-carry-validation-2026-08-20.md`](docs/checker-watch-carry-validation-2026-08-20.md).
Next.js, n8n, and the available AFFiNE monorepo produced zero canonical fact or
coverage differences; checker-phase wall time fell by 72.6–98.4%. The
production monorepo checkout was unavailable and remains an explicit corpus
substitution rather than a completed measurement of that checkout.

### Acceptance checks

- startup subscribes before work begins, then runs a full refresh;
- ordinary generations wait for the default two-second quiet period, and the
  previous snapshot remains published during that debounce;
- an event arriving during refresh, embedding, or enrichment causes a later
  generation and cannot be lost when the current cycle completes;
- refresh, commit, lock, embedding, and enrichment failures retry without a
  new filesystem event and cannot wedge subsequent cycles;
- notification overflow or an unclassifiable event forces a full refresh;
- database/WAL/SHM, configured telemetry, and other registered jscout-output
  writes alone never create a generation, including with two watchers;
- three persistent registration failures degrade that path to timer-backed
  coverage without causing a refresh loop, and registration is retried later;
- branch switches replace the complete file set without serving old files,
  projections, package ownership, vector occurrences, or checker facts from a
  different snapshot;
- submodule, manifest, lockfile, selected dependency, symlink-target, tsconfig,
  TypeScript runtime, and ambient declaration changes converge;
- edit -> enrich -> revert cannot reactivate a checker batch created before
  intervening external checker-input changes;
- bounded source/doc generations, non-boundary directory or missing-path
  generations, and periodic reconciliation parse only changed files and report
  unchanged-file reuse, while startup, branch/config/package, large-batch, and
  pathless/backend-uncertain generations use full refresh;
- no refresh mode can project a checker batch from a different snapshot;
- plain watch never serves checker edges from an older generation;
- `watch --enrich` publishes checker facts only for the current exact snapshot,
  and superseded checker work is cancelled or discarded;
- unchanged projects avoid Program construction after an ordinary changed
  snapshot, changed occurrences are queried first, and carried-vs-from-scratch
  canonical results are equal modulo projects deliberately re-enriched;
- the daily-scale checker flush has its own deadline, is not the ten-minute
  reconciliation tick, and a superseding source event cannot erase the
  carry-free requirement;
- a deterministically unindexable file reports the exact path/stage/error, is
  excluded without failing or degrading the refresh, and remains covered by
  later file events and periodic reconciliation;
- a recognized transient read failure rolls back and retries without
  publishing a reduced corpus;
- the default ten-minute reconciliation repairs a deliberately dropped
  notification, while explicitly disabling it reports the lost guarantee;
- repeated full generations reuse cached embeddings, embed only unseen
  content when requested, and preserve semantic artifacts and run history;
- no path through plain watch invokes pi-ai, the checker sidecar, embedding, or
  other optional spending without its explicit flag.

### Out of scope for G12

- publish-then-swap or uninterrupted query availability during refresh;
- a persistent daemon/service manager or background watch installation;
- a durable watcher event journal or unbounded transitive checker-input
  freshness graph;
- per-file incremental correctness logic before a measured latency need;
- blanket `node_modules` watching or dependency indexing;
- hidden scouting, summarization, or other generative work.

## Implemented G13 — repository reconnaissance scout

Path nouns and configuration filenames are not structural truth. `doc`,
`docs`, and a broadly owning `tsconfig` can each describe either production
behavior or auxiliary material depending on the repository. G13 replaces
accumulating path-specific exceptions with an evidence-backed semantic policy
overlay immediately after the neutral L1 index.

Reconnaissance is an explicit generative command, never an implicit part of
`index` or plain `watch`:

```text
jscout index
  -> jscout scout repository
  -> optional embedding selection
  -> optional checker enrichment
  -> workflow/card/summary/concept scouting
```

The deterministic planner discovers subjects without consulting current file
roles:

- workspace packages and bounded directory areas. A `mixed` area may subdivide
  at most three directory levels below its initial subject, the complete plan
  may contain at most 256 subjects, subdivision and classification share the
  command-wide `--max-calls` limit, and each serialized evidence pack must fit
  `--context-bytes`. Reaching any bound leaves the unresolved area `mixed`,
  which has neutral downstream policy, rather than silently guessing a child
  role. Subdivision includes a direct-file residual when a mixed scope contains
  both direct files and child directories, so root-level implementation files
  are not stranded under the mixed parent. Descendant classifications remain
  immutable history, but a current definite parent suppresses their projected
  policy; they may reactivate if the parent later returns to `mixed`;
- TypeScript/JavaScript project configurations discovered by the checker
  configuration-only pass;
- repository-owned files outside a workspace package, grouped by stable path
  boundaries rather than one model call per file.

Within each subject tier (packages, then areas, then projects), planning
orders subjects by indexed member count descending, then subject key. A
bounded `--max-calls` budget therefore reaches the corpus's weight centers
before its periphery; the Next.js evaluation showed alphabetical ordering
spending a 64-call budget on `apps/`/`bench/`/`crates/`/`evals/` while
skipping 308 subjects including the main package. Member count is a neutral
structural fact, consistent with the no-path-noun doctrine.
When a subject is subdivided because it is mixed or exceeds the context
budget, its children use the same descending-member-count ordering and run
immediately after their parent. This prevents an identified weight center's
refinement from falling behind the unrelated remainder of the initial plan.

Each bounded evidence pack includes manifests and scripts, config
`extends`/`references`/`include`/`exclude`, file-kind and language counts,
representative outlines, imports/exports, and deterministic entity/relation
summaries. Ambiguous `production`/`documentation`/`unknown` labels are excluded
from the prompt so the scout cannot merely repeat the heuristic under review.
Whole-scope counts expose only the high-precision surfaces `handwritten`,
`test`, `fixture`, and `generated`; representative content remains bounded.

The scout publishes immutable, fingerprinted scope/project classifications:

- role: `runtime`, `tooling`, `documentation`, `test`, `generated`, `mixed`, or
  `unknown`;
- exact scope or config identity;
- `likely`/`possible` confidence, model provenance, and cited evidence spans or
  config fields;
- a reusable classification fingerprint over the exact subject identity,
  ordered repository-relative membership, evidence-pack input hashes, the
  deterministic evidence-selection algorithm, and prompt/schema/model policy.
  Evidence inputs include manifest/config contents, aggregate file-kind and
  language counts, and the exact selected outline/import/export/entity rows.

The repository structural snapshot is recorded on the scouting run for audit
and response labelling, but it is deliberately excluded from classification
identity and freshness. Planning and publication recheck the subject
fingerprint, not global snapshot equality. An unrelated edit, reindex, or
branch change elsewhere therefore reuses the classification; membership or
evidence changes inside the subject stale only that subject and its ancestors.
A removed subject has no current identity to which its old classification can
apply. Returning to a branch with the same subject fingerprint may reuse the
immutable prior result without another model call.

Classifications are policy metadata, not graph facts. Files are never deleted
from L1. Only current, fresh classifications affect downstream defaults:

- deterministic artifact role, scouted scope role, and derived effective role
  are separate. `test`, `fixture`, and `generated` are protected facts; a
  runtime scope may rescue ambiguous documentation/unknown files but cannot
  promote protected artifacts into the product corpus;
- primary search applies a penalty to auxiliary scopes but retains recall and
  supports explicit inclusion. The penalty is applied once to the final fused
  or reranked order, not compounded independently in each retrieval arm;
- workflow candidates exclude fresh, likely auxiliary scopes by default;
- embedding can explicitly select the product corpus before expensive vector
  generation;
- checker ownership remains deterministic, while project purpose controls
  scheduling. A tooling project is skipped for a file only when a non-tooling
  owning project remains; a sole owner is retained as a fallback;
- stale, possible, mixed, or missing classifications fall back to neutral
  inclusion rather than silently hiding code. Index-time policy reconciliation
  is fail-neutral: it warns and clears the disposable policy projection rather
  than allowing optional semantic metadata to block L1 publication.

For package/area subjects, whole-scope artifact counts guide rather than
constrain classification. `unknown`/`possible` remains legal, and co-located
tests, fixtures, or generated output do not alone require `mixed`; deterministic
effective roles already protect those files. `mixed` represents multiple
semantic purposes worth bounded subdivision. Project classification continues
to describe why the configuration exists and is not forced mixed by member
file kinds. Duplicate valid citations are removed in model order and valid
citations after the first eight are truncated locally; unknown or empty
citations fail closed. The disposable current-classification projection keeps
explanations and bounded cited evidence for `repository_overview`, including
neutral mixed/unknown results and protected-role conflicts. When durable
history exists but no scope classification matches current evidence, overview
reports the stale/upgrade state and the explicit re-scout command; a zero
reconnaissance limit omits the overlay entirely.

The implementation includes a dry-run showing every planned subject,
classification input, subdivision depth/budget decision, reuse/freshness
decision, and downstream inclusion decision. Acceptance requires fixtures
where the same `doc`/`docs` and `tsconfig` names receive different roles from
different repository evidence; an unrelated reindex and an outside-scope
branch edit preserve freshness; in-scope membership/evidence drift restores
neutral inclusion only for the affected subject/ancestors; returning to an
identical fingerprint reuses the prior classification; and depth, subject,
call, and context limits terminate mixed subdivision deterministically.

### Planned G13 extension — generated-output boundary reconnaissance

The neutral walker must not treat an ambiguous directory noun such as `build`,
`lib`, or `generated` as structural truth. A directory named `build` may contain
authored product code, as it does in Next.js and Twenty, while an unignored
directory with another name may be reproducible compiler output. L1 therefore
continues to index every otherwise-indexable, non-ignored file. Repository
scouting, not the walker, determines whether an indexed scope belongs in the
default product corpus.

The current reconnaissance input is insufficient for this case. It can classify
packages, areas, and projects from bounded representatives, but a definite
runtime parent need not subdivide. An unmarked output subtree can consequently
inherit runtime treatment when its files have neither a high-precision
generated path nor a generated header. The follow-up adds exact output-candidate
subjects without requiring their parent to be `mixed`.

Candidate discovery remains deterministic and treats path names only as weak
labels. It assembles evidence from bounded, auditable signals such as:

- `tsconfig`/`jsconfig` `outDir`, `declarationDir`, and project-reference output
  relationships;
- `package.json` `main`, `module`, `types`, `typings`, `exports`, `bin`, and
  files/publication boundaries that point into a candidate directory;
- explicit output paths recoverable from package build scripts and tool
  configuration, retained as evidence rather than accepted as a verdict;
- generated headers, source-map links, bundled/minified or transpiled shapes,
  and bounded source/output correspondence;
- indexed membership, import/export direction, and representative symbol and
  entity surfaces for the exact candidate scope.

Gitignored output remains outside L1 and needs no classification. Source-control
tracked/untracked state may be recorded as supporting evidence when available,
but cannot be required because history-free snapshots and exported source trees
must behave coherently.

The model classifies each exact candidate as `runtime`, `tooling`, `generated`,
`mixed`, or `unknown`, with the existing citation, confidence, context, call,
and freshness rules. A directory name alone cannot support `likely generated`.
Fresh `likely generated` output policy is more specific than a runtime parent;
it may set the child files' effective role to `generated` without requiring the
parent to become `mixed`. `possible`, `unknown`, stale, invalid, or failed
classifications remain neutral. A fresh authored/runtime result prevents a weak
path-name suspicion from demoting the scope.

This is product-corpus policy, not destructive compaction:

- files remain in L1 and remain explicitly searchable;
- default search retains them with the existing generated-surface penalty;
- `embed --product` and automatic workflow/card/summary/concept scouting omit
  fresh generated output by default;
- overview and dry-run expose the exact boundary, decision, explanation,
  citations, and downstream policy;
- output evidence and exact membership participate in the subject fingerprint,
  so an in-scope rebuild or branch change restores neutral behavior until the
  matching classification is reused or refreshed.

Acceptance requires at least these paired cases:

1. a Next.js-like authored `src/build/**` scope remains runtime/tooling and in
   the product corpus;
2. an unignored compiler-output `build/**` scope is classified generated and
   omitted from product embedding and automatic semantic scouting;
3. generated output under a neutral name is discovered from explicit compiler
   or package boundaries;
4. a runtime package with a generated child receives the specific child policy
   without classifying the whole package as mixed;
5. a name-only suspicion and insufficient or conflicting evidence stay neutral;
6. gitignored output is never indexed or submitted to the scout;
7. changing only unrelated repository content reuses the classification, while
   changing the candidate membership or its cited producer/output evidence
   invalidates it.

## Implemented G14 — retrieval handoff and relevance discipline

G14 is retrieval hygiene, not a claim that more retrieval will solve complex
implementation tasks. The optimistic-prefetch replays established both sides
of that boundary: compact hybrid search localized the relevant subsystem and
fresh vector-backed semantic artifacts reached the agents, while implementation
still converged on the same incomplete one-file repair. G14 fixes concrete
transport and selection defects exposed by those runs without increasing
default response budgets or generating more semantic material.

### Copy-safe drill-down

Every compact search hit with a resolvable target must include bounded,
ready-to-use follow-up argument objects for the tools that can consume it. For
example:

```json
{
  "at": "packages/app/cache.ts:120-180",
  "anchor": "sym:packages/app/cache.ts#Cache::invalidate@1",
  "followups": {
    "tools": ["definition", "who_uses", "neighborhood"],
    "arguments": {
      "anchor": "sym:packages/app/cache.ts#Cache::invalidate@1",
      "snapshot": "<current-snapshot>",
      "origins": ["repository"]
    }
  }
}
```

G14 extends `definition` and `who_uses` with mutually exclusive exact
`anchor`/optional `snapshot` inputs while retaining their current fuzzy
`symbol` lookup for human-authored queries. Exact-anchor mode resolves the
snapshot node directly; it does not round-trip through the lossy
`path-substring:name` parser, which cannot distinguish same-named methods in one
file. The emitted keys and values must be the exact G14 MCP schema, not
descriptive pseudocode. File-only hits expose only file-compatible follow-ups.
Ambiguous hits expose candidate-specific objects or no object; jscout never
manufactures a convenient unique alias. Follow-ups share the parent response
budget and may be shed from lower-ranked hits before source locators or primary
hit identity. The compact shared `arguments` object is valid for every named
tool; file-only follow-ups use per-tool call objects where schemas differ.
Agents remain free to widen tool budgets, but should never need to retype or
shorten an opaque anchor.

### Compact overview drill-down

`repository_overview` currently spends roughly 20 KB on a large repository
before task localization. Its default form must retain deterministic totals,
top-level areas, current reconnaissance roles, freshness state, conflicts, and
the explicit re-scout action while reducing repeated explanations and evidence
objects. A scope row carries a one-line reason and citation count by default.
Exact explanations and cited evidence remain available through an explicit
scope/detail request on the same tool; an agent may raise `response_bytes` when
it needs a broader inventory. Budget shedding removes detailed reconnaissance
before deterministic repository identity and always reports omissions.

### Evidence-connected attached memory

Search-attached memory is a preview of memory relevant to the returned code,
not a second unconstrained semantic search. Selection therefore proceeds in
this order:

1. artifacts with exact support in a returned hit or its enclosing file;
2. artifacts whose support is within a bounded structural path of a top hit;
3. artifacts directly related to an artifact selected by the first two rules.

Text/vector similarity orders candidates inside those evidence-connected
tiers; it does not promote a generic semantically similar card over a directly
supported artifact. Artifact type and support-anchor diversity break ties so
four previews do not restate one symbol. If no artifact is connected, attached
memory is omitted with an explicit `no_connected_memory` status. The separate
`semantic_memory` tool remains the place for broad lexical/vector discovery
and continues to report its candidate pool and retrieval modes. The structural
join defaults to depth 2 and 2,000 visited nodes. Both bounds are explicit and
widenable per search, with depth capped at 8 and nodes at 20,000; truncation is
reported in the attachment status.

### G14 acceptance

- every emitted follow-up object round-trips through its named MCP tool on
  symbols, methods, modules, and file-only hits; ambiguous targets remain
  visible candidates;
- the anchor shorthand invented in the optimistic-prefetch replay is never
  emitted by jscout, while the exact returned object resolves without the agent
  interpreting it;
- overview honors one complete response-byte budget, preserves deterministic
  identity under truncation, and retrieves a scope's full explanation only on
  explicit request;
- attached memory prefers direct support and bounded graph proximity over a
  higher vector score from an unrelated area, and honestly returns none when
  no connected artifact exists;
- compact/debug representations remain fact-equivalent, and all tests use
  deterministic fixtures without model calls.

### Operational interaction amendment (2026-08-19)

Real use on a 7,000-plus-file monorepo confirmed that jscout's primary value is
cross-package localization and copy-safe source drill-down. Broad conceptual
ranking, concurrent expansions, and parallel full-artifact reads created noise,
latency, and truncated client output without improving the verified workflow.
The shipped skill and structural MCP instructions therefore standardize this
staged interaction:

1. use `repository_overview` once only when the repository is genuinely cold;
   skip it when the task already supplies a package, exact symbols, files, or
   stable anchors, and never use overview to rank semantic artifacts;
2. split the task into distinct behaviors and issue narrow, unexpanded searches
   sequentially, normally with limits of 4–6;
3. refine with learned symbols and use the returned opaque `definition` or
   `who_uses` arguments before broad source reads;
4. enable expansion only after localizing an exact entry point, expand one
   query at a time, and widen its independently reported bounds only when the
   omitted context is relevant;
5. retrieve one exact semantic artifact at a time; never fetch several full
   bodies concurrently through one client output channel;
6. after the useful artifacts are known, set `include_memory: false` on later
   code searches rather than repeatedly attaching the same previews; and
7. once a stable working set of files and symbols is known, stop broad
   expansion. Re-enable it only for one named unresolved boundary; otherwise
   use exact definitions/usages and unexpanded searches.

This observation does not justify larger global response, expansion, or memory
traversal budgets. Telemetry and reports must distinguish the returned
`expand_nodes` context bound from the internal `memory_nodes` evidence-join
bound before attributing a 2,000-node truncation to graph expansion. A degraded
reranker with useful lexical/structural fallback remains a visible provider
condition, not a reason to weaken deterministic retrieval.

The later TargetsQueue problem-solving investigation validates the drill-down
half of this contract: four naturally selected exact `definition` calls cost
11.6 KB total and carried the decisive mechanism, while nine expanded searches
cost 162.9 KB. G20 may make attached memory explicit and expansion path-shaped,
but it must not collapse exact definition back into the discovery payload.

MCP `tools/list` necessarily returns complete schemas; a client may present a
names-only inventory, but jscout will not add a parallel discovery protocol or
remove schema guidance solely to compensate for a client that prints every
definition. Optional payload limits use zero consistently to mean omission when
that interpretation is unambiguous. In particular, `source_limit: 0` is
equivalent to `include_source: false` rather than an error; the architecture
inquiry otherwise spent six parallel calls learning a constraint that provided
no safety or retrieval value. Positive limits retain their declared bounds and
invalid contradictory combinations still fail explicitly.

## Parked G15 — design-before-edit task memory

**Decision amendment (2026-08-18):** do not ship G15 as a jscout product
surface. PR #45 is blocked. In the Next.js root-layout replay, two-phase arms
cost more, passed less often, and twice preserved a coherent but wrong output
contract through implementation. Two-phase execution remains an optional
evaluation-harness treatment. No design artifact, command, MCP tool,
semantic-plane write, or agent-guide change is added to jscout. The design
below is retained only as a bounded proposal to reconsider if repeated tasks
show that a read-only design phase finds a correct mechanism that
implementation-only agents miss and that the handoff improves implementation
rather than merely anchoring it.

The proposed G15 addresses a different bottleneck from retrieval. In the
optimistic-prefetch campaign, one read-only Sol architecture probe received the
same anchor-free task story as the implementation agents and produced the
missing mechanism, detection axes, cure semantics, and cross-file intervention
site. Across 46 implementation arms, two models, multiple retrieval profiles,
forced and unforced use, and live semantic memory, that mechanism was never
generated. Agents localized relevant code and sometimes received `cache.ts`
memory, then collapsed into an edit/verify loop around
`optimistic-routes.ts`.

The experiment consequence is a first-class design phase whose result survives
implementation pressure. Whether persistence belongs in jscout is explicitly
unresolved. The parked G15 proposal would make a task-specific hypothesis
artifact before editing, store it in the semantic plane, and provide a compact,
explicit handoff during implementation.

### Parked product proposal

The initial surface is an explicit command and two structural-profile MCP
tools:

```text
jscout scout design ROOT --task TEXT [--seed ANCHOR ...] [--dry-run]

design_task(task, seeds?, budgets?) -> validated design artifact
implementation_brief(design_id, response_bytes?) -> compact pinned handoff
```

The command is opt-in generative work through the existing pi-ai gateway. It
never runs during `index`, plain search, or plain watch. Seeds are optional
because hard tasks may begin anchor-free; when supplied, they must be exact
current anchors. Dry-run prints the deterministic localization/evidence plan
and estimated calls without invoking a model.

### Bounded design evidence pack

The design scout starts from the task statement, not from a proposed patch. It
builds one fingerprinted, bounded evidence pack from:

- deterministic and hybrid search hits for the task language;
- exact supplied seeds and their enclosing symbols/files;
- bounded callers, callees, entities, runtime boundaries, paths, and contract
  edges around the strongest anchors;
- current checker facts and evidence-connected semantic artifacts when
  available;
- exact source spans and hashes for every cited candidate.

File roles and origins apply before budgets. Production evidence is retained
before tests/fixtures, while relevant tests remain labelled evidence of the
oracle rather than implementation truth. Every configured node, edge, source,
subject, model-call, and response-byte limit is reported and can be widened by
the requestor. Reaching a limit produces visible omissions, never an implied
complete repository model.

### Design-mode schema

The model is asked for a design, not a patch. The prompt and output schema have
no diff or replacement-code field and require reasoning in this order:

1. candidate defect/feature mechanisms and evidence that would distinguish or
   falsify them;
2. selected mechanism, or an explicit unresolved candidate set;
3. runtime detection signals and the observation channel carrying each signal;
4. cure semantics and invariants, including why retry, invalidation, backoff,
   fallback, or propagation behavior is correct where applicable;
5. affected files/symbols and the responsibility of each touchpoint;
6. cross-file state/control/data propagation required by the design;
7. validation oracle, regression risks, and unresolved questions.

Mechanism, detection, cure, and touchpoint claims require exact evidence
supports. Test evidence may establish required behavior but cannot alone prove
an implementation claim. Unsupported certainty, missing citations, unknown
anchors, an over-budget response, or an evidence/snapshot race fails
publication. Model-authored designs remain `likely` or `possible`, never
`certain`.

### Task-design artifact and lifecycle

`design_task` publishes a new `design` semantic artifact through the existing
run/support/fingerprint engine. Its identity includes the normalized task,
ordered seed anchors, deterministic evidence-pack hashes, prompt/schema/model
policy, and localization algorithm version. Repeating the same task on the
same evidence reuses the artifact without another model call. Evidence drift
marks it degraded or stale; refresh creates an immutable successor.

Design artifacts do not enter ordinary search-attached memory or repository
overview by default. They are task-scoped and retrieved by exact artifact ID,
an explicitly activated task, or a direct relation. This prevents one-off
debugging hypotheses from polluting general repository memory. A stale design
remains readable with its original evidence and a visible label so the
implementation agent can understand the historical decision rather than lose
it as soon as it edits a cited file.

### Implementation handoff and gating boundary

`implementation_brief` preserves, in order, the selected mechanism, detection
signals, cure semantics, touchpoints, invariants, and validation oracle. It
then includes copy-safe G14 follow-ups to current source. Under byte pressure it
sheds alternatives, prose, and source excerpts before those fields. Reading a
brief is recorded in request telemetry, and a later design revision is an
immutable successor rather than an in-place mutation.

The shipped agent guide establishes the workflow contract for non-trivial,
cross-file behavioral work:

```text
localize -> design_task -> inspect/approve design -> implementation_brief
         -> edit/verify -> optionally publish a design successor
```

jscout cannot prevent an arbitrary coding client from invoking shell or edit
tools. It therefore must not claim universal edit locking. Clients and
evaluation harnesses that can gate mutation may require a valid design ID
before enabling edit tools; other clients receive guidance plus telemetry that
shows whether design and handoff occurred before the first observed jscout
implementation query. The jscout product primitive is the validated persistent
design and explicit handoff, not a false cross-client sandbox guarantee.

This surface supports three later orchestration policies without baking them
into the gateway: one agent designs then implements; a read-only designer hands
off to a separate implementer; or an implementation session activates a design
already stored in jscout. G15 implements the third as the durable common layer.
The pi-ai gateway remains a bounded schema-call adapter, not an autonomous
tool-using coding agent.

### Acceptance if unparked

- identical task/evidence inputs reuse one immutable design; evidence changes
  visibly degrade/stale it and refresh publishes one successor;
- every design claim and touchpoint traces to hash-verified source or labelled
  behavioral test evidence, with no partial publication on model, validation,
  cancellation, or snapshot failure;
- an anchor-free fixture can localize a bounded evidence pack, while exact
  seeds constrain rather than silently replace that pack;
- the brief remains within its complete response budget and retains mechanism,
  detection, cure, touchpoints, and oracle before optional material;
- task designs remain absent from unrelated search/overview responses and are
  retrievable by exact ID or activated task;
- fake-gateway tests cover schema rejection, unresolved mechanisms, reuse,
  refresh, cancellation, and design-to-brief round trips without paid calls;
- after G14, G15, and the remaining engineering roadmap are implemented, the
  product evaluation compares implementation-only, single-agent two-phase,
  and persisted designer-to-implementer handoff. Correctness and mechanism
  retention are primary; token and wall-time cost are secondary.

### Out of scope for G15

- autonomous source edits or test execution by the gateway;
- pretending jscout can lock edit tools in clients it does not control;
- treating a design as deterministic graph truth;
- automatically generating a design for every simple lookup or local edit;
- attaching task-specific designs to unrelated repository search;
- using more global card generation or larger response budgets as a substitute
  for hypothesis formation.

## Conditional G16 — independent G14 attached-memory fallback

G14 deliberately changed search-attached memory from broad similarity previews
to evidence-connected selection: direct support first, then bounded graph
proximity, then relations to an already connected artifact. This prevents a
high-scoring generic card from displacing code evidence. It also creates an
intentional hard boundary: a semantically relevant artifact without a current
code-evidence connection is not attached, and the response redirects the agent
to `semantic_memory`.

G16 is a decision gate for that boundary, not the next automatic milestone.
It is independent of G15 and is considered only when task evidence satisfies
the entry criteria below. The evaluation must retain, per search, the bounded
semantic candidate IDs and ranks, why each
selected artifact connected, the `no_connected_memory` status, the emitted
follow-up, and whether the agent subsequently called `semantic_memory`. Stored
request telemetry records identifiers and decisions, not artifact bodies or
source payloads.

### Entry criteria

G16 enters implementation only when repeated task evidence establishes at
least one of these failures:

- current artifacts independently adjudicated as useful repeatedly enter the
  semantic candidate pool but are rejected because the evidence-connection
  join misses their relevant code relationship; or
- `no_connected_memory` correctly withholds unconnected prose, but agents
  repeatedly ignore the explicit `semantic_memory` handoff and consequently
  miss useful context or mechanisms.

A low attachment count, a high vector score, or one agent declining a
follow-up is not sufficient. If neither entry criterion is met, G14 selection
remains final and G16 is closed without implementation.

### Permitted correction

The correction must separate artifact discovery from body attachment. Likely
options are a bounded set of compact, explicitly unconnected artifact handles
with copy-safe `semantic_memory` arguments, or a narrower repair to a measured
false-negative evidence join. Unconnected artifact bodies do not return to
ordinary search, semantic prose does not enter code ranking, and graph depth or
response budgets do not widen globally to hide a selection defect.

Any implemented design must preserve:

- direct/graph/relation-connected artifacts ahead of discovery-only handles;
- explicit connection reason, freshness, and omission counts;
- one complete response-byte budget, with discovery handles shed before code
  hits or connected memory;
- exact follow-up arguments that require no anchor rewriting by the agent; and
- `semantic_memory` as the only surface for full bodies, relations, and source
  evidence.

### Conditional acceptance

If G16 is triggered, fixtures and the triggering real tasks must show that the
previously missed useful artifacts become discoverable and are deliberately
retrieved, without reattaching unrelated generic memory or reducing code-hit
quality. The report must compare useful-artifact recall, follow-up rate,
correctness/mechanism retention, rendered bytes, and irrelevant artifact reads
against G14. Failure to improve the triggering outcome at the registered byte
and relevance constraints closes the redesign rather than expanding it again.

G16 does not repair a semantic corpus that lacks useful artifacts, broad
`semantic_memory` queries that rank unrelated artifacts, or batch scouting that
never generated memory for the relevant code surface. Those are G18 concerns.
The root-layout replay did not trigger G16: independent adjudication found no
artifact describing the required causal chain, so there was no useful existing
artifact for the attachment boundary to miss.

The configuration-publish review also does not trigger G16. Useful artifacts
were deliberately found through `semantic_memory`, attached previews were
sometimes repeated after discovery, and no useful artifact was shown to be
rejected solely by the G14 evidence-connection boundary. Its actionable issues
are staged tool use, G17 occurrence ordering, possible workflow-overlap
diversity, and consolidated write-back. Adding unconnected bodies or larger
attachment budgets would worsen the observed noise.

## Implemented G17 — exact-identifier dominance

Hybrid retrieval currently lets RRF, the cross-encoder, and repository policy
place semantic or partial-name matches above an exact symbol definition. This
is wrong query intent. BM25 score magnitude is discarded by RRF, and the
reranker can then demote the remaining exact hit. Exact identifier lookup must
be a deterministic tier ahead of learned ranking, not another score feature.

G17 adds an intent lane for identifier-shaped query tokens:

1. Parse case-sensitive identifier tokens without promoting ordinary prose.
2. Retrieve exact chunk-name/symbol definitions and exact whole-token symbol
   occurrences before hybrid candidates. Existing origin and explicit role
   filters still apply. A pure identifier lookup may return its complete
   bounded occurrence tier; a mixed natural-language query admits one exact
   occurrence per identifier before hybrid ranking resumes, so an incidental
   common type cannot consume the whole result limit.
3. For a multi-identifier query, reserve coverage across distinct identifiers
   before returning repeated occurrences of one identifier. Ambiguous exact
   definitions remain visible candidates; exactness does not manufacture
   uniqueness. Exact occurrence peers that survive hybrid retrieval use its
   reranker/repository-policy order without crossing the tier boundary.
4. Run vector fusion, reranking, and repository-policy penalties only inside
   lower intent tiers. They may reorder exact peers but cannot place a partial,
   example, or vector-only match above an exact definition.
5. Return a compact match reason such as `exact_definition`,
   `exact_occurrence`, or `hybrid`, while preserving existing score fields as
   diagnostics rather than calibrated relevance.

### Planned G17 residual — syntax-aware exact occurrences

The initial G17 tier treats every verified whole-token occurrence as the same
kind of exactness. In a multi-identifier behavioral query, an import specifier
can consequently consume the one reserved occurrence for an identifier ahead
of the call, state transition, or implementation that gives the identifier
runtime meaning. Exact text is not sufficient to establish equal navigational
value.

Occurrence ordering becomes deterministic and syntax-aware within the exact
tier:

```text
exact definition
  -> executable occurrence (call, read/write, condition, argument)
  -> contract/type occurrence
  -> import/export occurrence
  -> hybrid candidate
```

Indexed structural occurrence kinds are authoritative where available. The
bounded lexer fallback must at minimum recognize import/export-only lines so
they cannot displace an available executable occurrence. Pure identifier
lookups retain bounded import/export occurrences; mixed and multi-identifier
queries use the strongest available occurrence for each identifier's reserved
slot before returning weaker exact peers. This changes ordering, not recall,
and does not infer runtime behavior from an import alone.

The TargetsQueue investigation adds two bounded corrections. When a query is a
single identifier and at least one exact definition or occurrence is admitted,
the default response ends with the exact tier instead of filling spare result
slots with hybrid analogs. The response exposes an explicit hybrid-widening
action; a zero-exact-result query may still fall back to hybrid retrieval.
Mixed prose/identifier queries keep the existing hybrid path. Exact retrieval
must produce the same admitted tier when vector retrieval is degraded or
disabled.

Repeated import/export occurrences for one `(identifier, file, syntax kind)`
collapse to one locator plus an omitted-occurrence count. Distinct executable
occurrences in the same file remain separate: diversity must not erase the
multiple state transitions or calls an investigation is trying to prove.

### G17 acceptance

- exact definition or occurrence hits for `createRouteTypesManifest`,
  `getRootParamsFromLayouts`, `collectedRootParams`, and `NextTypesPlugin`
  precede unrelated example and Sitecore chunks with embeddings and reranking
  enabled;
- a hostile fake reranker cannot demote an exact definition below a hybrid
  candidate;
- same-named definitions remain separate candidates and multi-identifier
  queries cover each resolvable identifier within the requested limit;
- a multi-identifier behavioral query containing `ExportJobPayload` returns an
  available executable use before its import specifiers, while a pure
  `ExportJobPayload` lookup still exposes those import sites;
- a pure `TargetsQueue` query with exact results returns no hybrid filler unless
  the caller explicitly widens, and its exact tier is unchanged when the vector
  provider is degraded;
- repeated imports collapse without collapsing two distinct executable
  occurrences in the same file; and
- prose-only queries retain the current hybrid path and complete response-byte
  budget.

## Implemented G18 — task-directed semantic coverage and selection

Whole-repository weight ordering is not a workable completeness strategy for
generated cards. In the Next.js root-layout run, 448 card calls still missed
the relevant type-generation surface, while three broad `semantic_memory`
calls returned 26 mostly unrelated artifacts from candidate pools of 93–270.
No larger response budget can recover information that was never generated,
and returning more weak analogs increases anchoring risk.

G18 changes both generation selection and direct semantic retrieval:

1. Keep batch scouting opt-in, but allocate bounded coverage across G13
   reconnaissance scopes before spending the remainder by subject weight.
   Report selected and omitted subjects per scope so a global cutoff cannot
   masquerade as repository coverage.
2. Add bounded targeted card generation from exact anchors, files, or one
   reconnaissance subject. This is semantic enrichment of a localized surface,
   not a task-design agent and not automatic work during index/search/watch.
3. Rank direct semantic-memory results by exact support/anchor and current
   reconnaissance scope before lexical/vector similarity. Generic analogous
   artifacts cannot outrank memory supported by the localized code surface.
4. Separate discovery from payload: broad query results return compact artifact
   handles, type/freshness/support summaries, relevance reason, and copy-safe
   exact-ID follow-ups. Full bodies, relations, and evidence remain an explicit
   artifact drill-down. Do not raise the 24 KB default.
5. Return an explicit `no_supported_memory` result when no artifact is connected
   to supplied anchors/scopes. Do not fill the response with weak analogs merely
   to satisfy `limit`.

### Observed G18 follow-up — overlap and verified consolidation

Broad semantic discovery can still return several apparently duplicate or
overlapping workflows. Before changing retrieval, diagnostics must distinguish
true duplicate artifacts from legitimate narrower workflows that share an
entry point. Any diversity correction uses current support/participant overlap,
artifact relations, freshness, and supersession—not prose similarity alone—and
must preserve separately useful stages.

When an agent verifies a stable end-to-end workflow that existing memory splits
across narrower artifacts, the intended correction is evidence-backed
`annotate` write-back when the operator or task workflow authorizes persistent
memory mutation. A read-only repository question does not implicitly authorize
write-back. When authorized, it publishes the consolidated workflow with exact
source participants rather than launching another broad speculative scouting
batch.
The configuration-publish review is the registered case: analysis/staging,
queue dispatch, publish mutation, completion notification, and UI state form one
verified larger skeleton while remaining individually inspectable stages.

Semantic relevance is not architectural or product importance. Jscout may
provide package boundaries, callers, acceptance/test surfaces, and workflow
participation as evidence, but it does not convert retrieval scores into a
universal importance ranking. The requesting agent must label such a ranking as
judgment against stated criteria.

### G18 acceptance

- fixed budgets provide deterministic per-scope coverage and report every
  omitted scope/subject count;
- targeted scouting of root-layout type-generation anchors selects relevant
  subjects without spending hundreds of calls on unrelated areas;
- a direct query with localized anchors returns supported artifacts before CMS
  examples, or honestly returns `no_supported_memory` when the corpus is blind;
- broad discovery stays compact and requires an explicit artifact-ID request
  before returning a full semantic body;
- useful-artifact precision improves without widening global generation or
  response budgets, and generated prose remains separate from code ranking.

## Planned G19 — quiet-window repository scouting in watch

G12 deliberately keeps semantic-content generation outside watch. A later
opt-in `watch --scout` may close the full enrichment loop without turning
ordinary watch into a background LLM job. Its phase order is fixed:

```text
refresh -> embed(code) -> enrich -> scout(stale delta) -> embed(semantic)
```

The phase is constrained by quiet time, not a hidden monetary throttle. It has
the lowest priority and is superseded first by any relevant filesystem event.
On a continuously changing checkout it may never finish; watch must report the
lag and the exact manual scout command instead of queuing generations or
silently widening work.

G19 must be designed around stale-delta scoping before implementation. It uses
semantic supports and reconnaissance subject fingerprints to select only
subjects invalidated by recent successful generations. Full-repository
scouting remains manual. Deterministic subject failures publish an explicit
partial/terminal outcome and wait for a later generation; gateway transport
failures use the existing retry path. Cancellation must retain completed
subject-local work and never publish evidence against a superseded structural
generation.

Acceptance requires fixtures for quiet completion, continuous supersession,
subject-local resume, partial failures, gateway retry, and semantic-vector tail
convergence. Until those exist, no `--scout` flag is shipped and watch's README
boundary remains structure/checker/vector maintenance only.

## In progress G20b — path transport and measured compatibility

G20a merged in PR #60 and implements the correctness and compact-transport portion: cross-origin
exact follow-ups, one complete top-hit handoff, opt-in search-attached memory,
bounded checker receiver display, exact usage labels, compact/body/full artifact
views, successful-diagnostic gating, and canonical section-byte telemetry.
G20b implements path-shaped expansion first, then runs the structured-content
compatibility experiment and the registered fixed/staged replays. No aggregate
byte claim is made before those replays run.

Real-monorepo use established a second bottleneck after localization quality:
individually budgeted responses can still repeat enough metadata across an
exploratory session to consume more context than the repository evidence. The
[architecture-inquiry call report](eval/results/workflow-architecture-inquiry-2026-08-19.md)
records the exact 42-call inventory. Twenty-seven responses with retained
measurements totalled 358,334 inner JSON bytes. Extrapolation across 15
truncated, omitted, or error responses put total jscout output near 460–510
KiB, or roughly 115k–145k raw tokens before client-side truncation.
Approximately 11.8k additional tool-discovery tokens were a client
orchestration cost and are excluded from the jscout total.

The later
[TargetsQueue problem-solving investigation](eval/results/targets-queue-problem-investigation-2026-08-20.md)
records 19 measured calls from a concrete mechanism/edge-case investigation.
They returned 228.5 KB (223.1 KiB), estimated at 60k–65k tokens. Nine expanded
searches generated 162.9 KB—about 71% of all jscout bytes—while four naturally
selected exact definitions generated only 11.6 KB and carried the decisive
mechanism. The agent judged 55–65% of the session payload avoidable.

This is an architecture-inquiry workload: the agent was explicitly asked to
discover and explain several product workflows. It is a valid primary jscout
use case and a useful stress test for conceptual retrieval, but it is not an
independent coding agent localizing evidence while implementing a story or
fixing a bug. G20 may use it to optimize transport; claims about implementation
behavior require later real-work evidence. The TargetsQueue trace supplies
that missing problem-investigation evidence for natural exact-tool selection
and marginal value after localization. It still does not establish a patch-
outcome effect because it was not a controlled implementation comparison.

The useful payload was much narrower: exact symbols and locations, short source
snippets, opaque anchors, direct uses/call edges, one-sentence workflow meaning,
defining participants, and freshness. G20 reduces repeated transport without
removing those facts, weakening source verification, or raising the existing
24 KB per-response default.

### Confirmed serializer defects

The current compact serializers already remove many diagnostic fields, but the
review and code inspection confirm these remaining defects:

- search emits the snapshot once at response level and repeats it inside every
  symbol follow-up together with tool names and origins;
- a generated follow-up restricts `origins` to the hit's single origin. For a
  workspace hit this can omit root/unowned first-party usages, while a
  dependency-only follow-up can omit the first-party callers that matter. This
  is a correctness defect, not just duplicated bytes;
- compact search still emits normal-path retrieval, candidate-pool, semantic
  score, and successful memory-attachment traversal diagnostics whose primary
  consumer is telemetry rather than the coding agent;
- exact semantic-artifact detail always returns model/prompt/snapshot/timestamp
  provenance and up to eight complete supports containing source/context hashes,
  even when hash-verified source was not requested;
- expanded search serializes a ranked induced neighborhood rather than the
  smallest useful cross-file continuations, retaining unrelated graph nodes and
  high-frequency framework edges;
- compact graph edges copy raw checker `receiverTypes` verbatim, so a single
  generic receiver can serialize an entire nested contract instead of the
  useful bounded type head;
- search labels repository-wide `refs.target_name` counts as `used_by` even
  though they are not resolved to the hit's anchor. Common method names can
  consequently claim hundreds of apparent callers that are only approximate
  same-name occurrences; and
- MCP serializes the JSON result inside `content[].text`. This is compatible
  with existing clients but can appear as escaped JSON inside another captured
  result and must be measured separately from the inner rendered-byte budget.

Short source excerpts are retained: the review found them useful and estimated
that they were less than one fifth of total payload. G20 targets metadata,
diagnostics, repeated defaults, and graph shape before reducing source evidence.

### Compact search and follow-up contract

**Decision amendment to G14:** compact search no longer emits a complete
arguments object for every eligible hit. The architecture-inquiry agent used
none of the objects, but that does not establish that implementation agents
will also ignore them. The highest-ranked uniquely anchored hit therefore keeps
one complete copy-safe follow-up object by default. Lower hits keep their exact
anchor and compatible tool names without repeating snapshot/default arguments.
An explicit debug or widened-follow-up mode may return complete objects for more
hits.

Compact search also keeps one response-level snapshot. Exact-anchor tools accept
an anchor without a snapshot and fail with candidates rather than guessing if
current resolution is ambiguous; a caller that needs strict pinning may use the
complete top-hit object or copy the one response-level snapshot. Later real
implementation work must measure whether the top-hit handoff is selected before
G20 removes or multiplies it.

First-party follow-ups omit `origins`, preserving the normal combined
`repository` plus `workspace` corpus. A dependency target carries an explicit
non-default inclusion that permits the dependency definition and first-party
callers instead of constraining the entire drill-down to `dependency`. Tests
must cover repository-to-workspace, workspace-to-repository, and
first-party-to-dependency usage edges.

**Second decision amendment to G14:** `semantic_search` defaults
`include_memory` to false in its CLI, MCP schema, implementation, and examples.
Callers opt in when an evidence-connected preview is useful; direct
`semantic_memory` remains the discovery surface for causal/workflow memory.
This preserves G14's evidence-connection rules but stops paying their traversal
and payload cost on every search. Both real-use traces repeatedly received weak
or already-known previews, and the TargetsQueue investigation found no
decisive claim in them. This default change does not trigger G16: no useful
artifact was shown to be rejected solely by the evidence-connection boundary.

Compact graph receiver types render a bounded top-level display such as
`Errors`, preserve an explicit `truncated` marker, and keep the complete checker
string only in debug output/telemetry. Compact hit decorations must not label a
name-only count as `used_by`. A uniquely resolved hit may report bounded
anchor-resolved incoming edges; otherwise it reports
`name_occurrences_approx` or omits the count and directs the caller to exact
`who_uses`. Exact usage semantics cannot be traded for a shorter misleading
field.

G20 does not initially add server-side short handles. Handles require session
state, expiry, collision, replay, and reconnect semantics; removing repeated
defaults captures most of the measured waste while anchors remain durable and
copy-safe. A stateful handle enters consideration only if post-G20 measurement
shows anchor strings, rather than bodies or graphs, remain a material share.

The intended default shape is approximately:

```json
{
  "snapshot": "...",
  "hits": [
    {
      "anchor": "...",
      "at": "file:line",
      "symbol": "...",
      "snippet": "...",
      "tools": ["definition", "who_uses", "neighborhood"],
      "followup": {
        "arguments": {"anchor": "...", "snapshot": "..."}
      },
      "key_edges": ["calls X", "used by Y"]
    }
  ]
}
```

Lower-ranked hits may lose snippets before identity, location, and key edges.
Session-aware suppression of previously returned snippets is deferred; staged
limits and `include_memory: false` avoid adding retrieval-session state for the
first correction.

### Progressive drill-down remains separate

G20 does not add a generic `fields` selector or inline
`include_definition`. A field selector multiplies response contracts and asks
the agent to understand serializer internals before it has localized the task.
Named compact/debug/artifact views plus strict byte budgets cover the measured
need. In the problem-solving trace, four separate exact definitions cost only
11.6 KB and were the highest-value responses; inlining them would move useful
progressive disclosure back into the 162.9 KB expanded-search class. The
copy-safe top-hit handoff is the intended round trip unless later latency data,
not byte speculation, shows that it is the bottleneck.

### Compact semantic-artifact views

Exact `semantic_memory` drill-down gains an explicit view with a type-aware
compact default:

- `compact`: identity, freshness, one-sentence description or primary claim,
  and, for workflows, defining participants only;
- `body`: the complete artifact body plus one compact evidence locator by
  default, without model/prompt/timestamp provenance or hashes;
- `full`: the current diagnostic artifact, relations, complete selected
  supports, provenance, and hashes.

`include_source` remains explicit and hash verification remains mandatory
internally. Its default returned evidence count becomes one and stays widenable.
The compact/body response reports support and relation omission counts so a
caller can request `full` deliberately. Supporting leaf helpers and related
summaries do not accompany a defining-workflow request unless selected by the
view or relation request.

Broad semantic discovery continues to return compact handles. G18's
support/participant-overlap investigation owns duplicate or overlapping
workflow diversity; G20 does not deduplicate artifacts by prose similarity.

### Agent diagnostics versus telemetry

Default compact responses retain diagnostics only when they change the next
action:

- degraded/failed lexical, vector, or reranker stages;
- truncation plus actionable omission counts;
- `no_connected_memory` and `no_supported_memory` handoffs; and
- artifact freshness and trust labels.

Candidate-pool size, uncalibrated component scores, successful attachment graph
depth/node counts, full byte accounting, model/prompt provenance, and evidence
hashes move behind `debug: true` or the `full` artifact view. They remain in
per-tool telemetry so G16 and retrieval evaluations do not lose observability.
Search telemetry and debug output split canonical rendered bytes into at least
`hits_bytes`, `graph_bytes`, `memory_bytes`, and `envelope_bytes`; the sum and
accounting method are tested against the complete canonical response. Those
section counters are not added to every normal response, where they would
consume more of the budget they are intended to diagnose.
Telemetry also retains the repository-wide name-only usage-occurrence count
that compact hits no longer present as exact `used_by`. This is an explicitly
approximate diagnostic for detecting recall shifts in dynamic-dispatch-heavy
corpora; it must not be rendered as declaration-resolved caller evidence.
`rendered_bytes` remains visible when a response truncates; normal-path byte
measurements are available in telemetry and debug output rather than repeated
in every agent response.

### Path-shaped expansion

Expanded search gains a path projection optimized for the product question:
"how does this localized entry point reach another package, handler, state
transition, or effect?" The compact default returns a ranked path forest rooted
at the selected hit seeds. It retains the edges required to explain each
cross-file continuation and gives nodes response-local short IDs while keeping
the exact anchor at its first occurrence.

The repository default is `search.expansion.mode = "paths"`, with eight ranked
continuation endpoints (`paths = 8`, widenable to 50) under the existing global
seed/node/edge/byte bounds. Selection is a deterministic multi-source
maximum-bottleneck forest over the already ranked neighborhood: cross-file and
non-symbol boundaries lead same-file leaves, while direct relations between
two search-hit seeds remain first-class paths. This changes only projection;
confidence and relation weights, hub damping, role penalties, and origin policy
remain owned by the structural traversal.

The existing induced neighborhood remains an explicit diagnostic mode.
High-frequency/common calls are suppressed through existing edge-kind weights,
degree/hub damping, and path contribution—not a brittle blacklist of names such
as `default`, `object`, or `string`. An edge on a selected connecting path is
retained even when its display name is common. Omitted path/node/edge counts and
truncation remain visible, and agents may widen every existing bound.
With expansion depth one, the same projection is the requested compact one-hop
caller/callee view; G20 does not add another overlapping expansion tool or mode
solely for that shape.

### MCP structured-content experiment

The serializer first produces one canonical structured value so text and any
future MCP `structuredContent` representation are fact-equivalent. G20 then
tests Codex, Claude Code, and the supported pi/MCP clients before changing the
wire shape. A client that ignores structured content must retain the JSON-text
fallback; a client that exposes both forms must not receive two full copies in
model context. Structured content ships only through a negotiated/profiled
path that demonstrates lower client-visible bytes. It is not assumed to solve
compression merely because the protocol can represent it.

Measurement distinguishes:

1. inner canonical JSON bytes;
2. JSON-RPC wire bytes after escaping/envelopes; and
3. client-visible model-context bytes after the client's MCP rendering.

The 2026-08-21 compatibility experiment is recorded in
[`eval/results/g20b-mcp-structured-content-2026-08-21.md`](eval/results/g20b-mcp-structured-content-2026-08-21.md).
Codex 0.147.0 preserved all 40 probe records and its verified result mapper
reduced the deterministic client-visible representation by 11.31%, while raw
MCP bytes increased by 88.9% because the fallback remains present. Claude Code
2.1.238 also preserved all facts and did not expose two complete model copies,
but showed no context reduction while raw bytes increased by 88.3%. Therefore
`auto` is profiled to verified Codex versions; Claude and unknown clients stay
text-only, with explicit `text` and `structured` overrides. The installed pi
agent has no MCP client surface, while pi-ai is the LLM gateway rather than an
MCP result consumer. This compatibility result does not satisfy the aggregate
fixed-call replay gate.

The path projection also has a separately labelled
[`n8n real-corpus proxy`](eval/results/g20b-n8n-path-transport-proxy-2026-08-21.md).
Across four fixed lexical expanded searches, it preserved the ordered hit
anchors, emitted only edges present in the diagnostic neighborhood, reduced
aggregate response bytes by 62.4%, nodes by 72.5%, and edges by 85.8%. The
first pass exposed and fixed seed starvation by reserving one continuation per
connected seed before global fill. This proxy does not satisfy the registered
historical gate: the original private corpus is absent, raw responses were not
retained, and abbreviated calls cannot be reconstructed honestly.

The prospective
[`Next.js full-posture check`](eval/results/g20b-next-full-posture-2026-08-21.md)
then exercised product code vectors, the local reranker, 599 semantic vectors,
evidence-connected memory attachment, and both projections together. Across
four fixed query pairs, paths preserved ordered hits and delivered artifact IDs,
reduced bytes by 55.1%, nodes by 70.0%, and edges by 85.6%. The first pass found
that multi-seed path selection could retain a real cross-file continuation that
the equally bounded diagnostic neighborhood omitted. Neighborhood now reserves
the selected path forest before filling its remaining fan-out budget; the full
rerun made every path node and edge a subset in all four pairs. This is the
required rich-response corpus point, but its 55.1% result does not clear or
replace the unavailable historical 60% fixed-call gate.

### Implementation order and acceptance

1. Preserve both exact call inventories and any recoverable raw responses before
   changing serializers; label the architecture inquiry's historical 460–510
   KiB total as an estimate and the problem investigation's 228.5 KB as the
   measured inner-response baseline.
2. Fix cross-origin follow-up semantics and remove repeated first-party defaults.
3. Make attached memory opt-in, bound receiver-type display, and replace or
   relabel name-only `used_by` counts.
4. Add compact artifact views, debug-gate routine diagnostics, and record
   per-section byte accounting in telemetry/debug.
5. Add path-shaped expansion while preserving explicit neighborhood mode.
6. Run the structured-content compatibility experiment; ship it only where it
   reduces client-visible context without a fallback regression.
7. For each historical workload, run two separate replays:
   - a fixed-call transport replay with the same valid queries, arguments, call
     count, and ordering, measuring serializer changes only; and
   - a staged-session replay with sequential 4–6-result queries, one artifact at
     a time, delayed expansion, and explicit memory only when needed, measuring
     skill/orchestration behavior.

For every search in both replay forms, report memory attachment requested,
selected, and delivered separately using the existing telemetry fields
`requested_retrieval.memory`, `semantic_selected`, and
`semantic_artifacts_returned`. The opt-in default makes request adoption a
separate causal gate: a zero-delivery arm must not be interpreted as a memory
ranking result when the agent never requested attachment.

The architecture fixed replay excludes the six invalid historical requests
from byte parity but tests their replacement (`source_limit: 0`) separately.
The 42-call architecture and 19-call problem-solving workloads are reported
separately rather than pooled. Each staged replay reports reduced calls and
bytes independently; its savings cannot be credited to the serializer target.

Acceptance requires:

- all previously identified high-value facts—locations, anchors, snippets,
  decisive edges, descriptions, defining participants, and freshness—remain
  available without `debug` or `full`;
- compact/default aggregate inner JSON bytes fall by at least 60% on the
  fixed-call transport replay at equal fact coverage and call count. The two
  workloads are evaluated separately so one cannot hide a regression in the
  other. The reported 65–75% reduction remains a hypothesis until measured;
- the staged-session replay reports call count, inner/wire/client-visible bytes,
  and recovered high-value facts separately, without presenting strategy
  savings as transport savings;
- the prospective real-corpus path-projection workload includes at least one
  full-posture arm with vectors, reranking, and attached semantic memory.
  **Complete:** the Next.js check above supplies that point; the lexical n8n
  proxy remains graph-projection-only evidence;
- strict individual response-byte budgets and explicit omission reporting
  remain intact;
- `source_limit: 0` performs a successful no-source artifact read and a
  positive source limit remains bounded and widenable;
- copy-safe exact drill-down returns cross-origin usages rather than silently
  applying the seed hit's origin as a global filter;
- the top-hit complete follow-up still round-trips, while lower-hit compact
  anchors remain sufficient for deliberate drill-down;
- omitting `include_memory` performs no attachment traversal and returns no
  preview, while `include_memory: true` remains fact-equivalent to G14's
  evidence-connected selection;
- compact receiver types remain bounded for a hostile deeply nested generic and
  expose truncation, while debug retains the complete checker value;
- no name-only occurrence count is labelled as exact `used_by`; exact incoming
  edges and the telemetry-only approximate occurrence count remain
  distinguishable;
- per-section byte counters sum to the canonical response size under complete
  and truncated responses without appearing in normal compact output;
- `compact`, `body`, and `full` artifact views are deterministic, and `full` is
  fact-equivalent to the pre-G20 diagnostic result;
- path mode preserves the verified configuration-publish skeleton with fewer
  returned nodes/edges than neighborhood mode and preserves the TargetsQueue
  feedback-loop continuations, while explicitly requested neighborhood output
  remains available;
- the four exact definition responses in the problem-solving replay retain
  their decisive source facts and remain separate drill-down calls;
- normal compact responses omit successful-stage scores/pools/hashes, while
  degraded/truncated responses and telemetry retain enough information to
  diagnose the event; and
- no client receives both complete text and structured copies in model context.

This milestone is independent of G15 and does not trigger G16. The observed
failure was excessive/repeated delivery of memory and graph metadata, not a
useful artifact hidden solely by G14's evidence-connection boundary.

## Implemented G21 — repository runtime configuration

Jscout's durable operator policy currently spans CLI flags, MCP arguments, and
many `JSCOUT_*` environment variables. This makes one-off experiments possible
but leaves no repository-level answer to basic operational questions: which
database the MCP process opened, whether vector retrieval and reranking are
enabled by default, which gateway/model is selected, and where telemetry is
written.

G21 adds one versioned `<repository>/.jscout.toml` for non-secret stable
configuration. Every command resolves the canonical repository root first;
MCP loads that exact file once at startup and remains one process serving one
root/database. The default database remains `<root>/.jscout.db`, an explicit
`--database` remains authoritative, and no parent-directory search,
multi-repository MCP routing, or hot reload is introduced.

Resolution is explicit invocation/MCP argument, then repository config, then a
legacy environment fallback, then built-in behavior. API keys and tokens stay
outside the file; config may name their environment variable or an auth file.
MCP retrieval booleans become tri-state so omission uses repository policy and
an explicit value can widen or narrow it. CLI keeps negative retrieval flags
and gains corresponding positive overrides.

The implementation must centralize database, search, embedding/reranker,
inference, LLM/gateway, checker, MCP, and telemetry settings in one immutable
resolved object rather than letting subsystems reread process environment.
`jscout config show|validate|init` exposes effective values and their sources
with secrets redacted. MCP initialization and privacy-minimal telemetry record
the binary/build identity, a non-secret runtime-configuration fingerprint, the
effective per-call retrieval posture, and stage-specific retrieval timings.
Exact request arguments remain in the separate privacy-sensitive request log;
client-side batching and outer-message truncation cannot be inferred by the MCP
server unless the client supplies that metadata.

The runtime fingerprint is observational, not a global invalidation key.
Changing reranking, attached memory, expansion, or byte budgets must not alter
the structural snapshot or embedding-profile identity. In particular, an
operator can disable reranking for interactive speed and later re-enable it
without re-embedding the repository.

The current mixed production telemetry is insufficient to change the built-in
reranker default: deployed rows combine different binaries and intentionally
different retrieval postures, every ordinary vector-active row also has the
reranker active, and no relevance labels were recorded. G21 therefore preserves
existing built-in defaults. A repository may explicitly set `rerank = false`;
a product-wide change requires a fixed-query comparison on one binary,
database, snapshot, and embedding profile with only reranking toggled.

The typed loader, command/MCP precedence, provider and sidecar wiring, legacy
fallback warning, non-secret fingerprints, retrieval-stage telemetry, operator
commands, and operating template are implemented. The complete schema,
migration boundary, phased implementation, tests, and acceptance criteria are
in the
[repository runtime configuration implementation plan](docs/repository-configuration-plan-2026-08-20.md).

## Implemented G22 — exhaustive lexical search contract

A production investigation (the links-iteration convention check,
2026-08-22) ran twelve `vector: false` searches at `limit: 10` over a bounded
workspace corpus to establish a repository convention, missed at least one
literal occurrence that `rg` listed, and reported the comparison as complete.
The miss was truncation, not ranking: no limit was ever widened, and
completeness was claimed from a truncated result. For identifier-shaped
lexical queries over the indexed corpus the match set is finite and small;
the tool should state its size and return all of it on request. Ranking's
structural limit belongs to vector similarity only. FTS tokenization cannot
promise regex or substring exactness, which stays with `rg`.

G20b is not a prerequisite. Its transport work shipped in #60 and #62; it
remains open only because the historical 60% fixed-call replay is
unreachable. It closes with a newly registered reproducible fixed-call
workload plus the staged-session replay, after which it is marked implemented
or its historical gate is explicitly replaced. The serializers are not
reopened before G22.

Contract:

1. **A mode with precedence over configuration.** `semantic_search` accepts
   `exhaustive: true`. After repository configuration is resolved, the mode
   forces `vector`, `rerank`, `expand`, and `include_memory` off — the
   built-in defaults turn vector and rerank on, so a bare `exhaustive: true`
   must work on a normal repository. Only an explicitly supplied conflicting
   `true` for one of those fields is rejected. The response echoes the
   effective posture: `effective: { vector: false, rerank: false, expand:
   false, include_memory: false, page_size }`. The cross-encoder runs over the
   fused pool independently of vector retrieval today, and expansion and
   attached memory change both membership and latency; forcing them off gives
   the response one meaning — the FTS content-column match set for the query
   terms over indexed chunks in the requested scope. The ranked-only `name`,
   `symbols`, and `path` columns cannot create exhaustive hits with no source
   line. `vector: false` without `exhaustive` keeps today's ranked behaviour.
2. **Continuation fields.** In exhaustive mode the integer `limit` is the
   page size, bounded by a hard ceiling, and `cursor` carries the opaque
   continuation token from the previous page. There is no `offset`.
3. **The unit of completeness is the chunk.** Search returns one hit per
   chunk, so the completeness fields are `total_chunks` (every chunk whose
   content matches in scope, counted before paging and before byte shedding),
   `returned`, `truncated`, and `next_cursor`. An exhaustive hit carries `match_lines`,
   the unique lines inside the chunk where a query term matches. The claim is
   chunk coverage plus unique matching-line coverage; match multiplicity
   within a line and match spans are not represented, and the contract does
   not say that every literal occurrence is recovered from `match_lines`.
   The disposable FTS mirror replaces embedded NUL bytes with one-byte token
   boundaries so later line offsets remain intact, and highlight delimiters
   are selected against the complete page text so source bytes cannot be
   mistaken for match markers.
4. **Paging.** `next_cursor` is opaque and binds the query, the normalized
   scope, and the snapshot; continuation against a changed snapshot fails
   with a snapshot error rather than skipping or duplicating hits. Whenever
   `next_cursor` is returned it differs from the input cursor and resumes at
   the first unrendered hit. Order is deterministic and unranked — path,
   chunk start, chunk id — so traversal is stable and needs no tie-breaker.
5. **Byte shedding with forward progress.** `response_bytes` still applies:
   `truncated` reflects the rendered prefix, and `next_cursor` advances from
   the last hit actually rendered, never from the pre-budget candidate count.
   The complete top-hit handoff is emitted once per traversal, on its first
   page only. When the budget cannot fit the envelope plus one hit, the
   handoff degrades to a locator first; if one locator hit still does not
   fit, the response is a deterministic `response_budget_too_small` error
   carrying the minimum byte size — never an unchanged cursor.
6. **Scope is echoed as a normalized object, not raw arrays.**
   `scope: { corpus: "indexed_chunks", file_roles: "all" | [...], origins:
   [...], snapshot }`, because an empty roles filter means every indexed role
   and default origins mean both first-party classes. Completeness is a claim
   about indexed chunks in the echoed scope: ignored, hidden (`.github/`),
   unsupported, and extensionless files are never indexed and are outside it;
   dependency files are outside the *default* origin set and inside the claim
   only when `origins` explicitly includes `dependency`, which exhaustive mode
   accepts.
7. **Locator-heavy hits.** Exhaustive pages carry anchor, path, lines, kind,
   and `match_lines`. They do not run the per-hit `used_by` resolution that
   normal hits perform, so a high-frequency identifier does not become
   thousands of exact-reference lookups.
8. No regex or pattern occurrence tool until G22 proves insufficient on a
   real completeness question.

Acceptance: a rare identifier (one page, `truncated: false`,
`returned == total_chunks`); a high-frequency identifier traversed across
pages with no duplicated or missing chunk; two occurrences on one line
appearing as one `match_lines` entry, with the gold compared as unique
`(path, line)` values; role and origin filters reflected in the scope object
and the counts; an explicit `dependency` origin counted and paged; a small
`response_bytes` producing `truncated: true`, a cursor that resumes at the
first unrendered hit, and no repeated handoff; a zero-fit budget producing
the locator degradation or `response_budget_too_small`; a snapshot change
between pages failing the continuation. Replay the links-iteration
investigation against a gold set built with `rg -w` over the indexed files in
the same scope, as unique `(path, line)` values, compared at the
representation the API returns — chunk plus `match_lines` — not at raw `rg`
output.

## G23 — guidance implemented, acceptance replay pending

Three production sessions and the evaluation record show agents discovering
the efficient loop themselves, partially. The TargetsQueue investigation
converged on unexpanded search plus `definition` in its tail; the
links-iteration investigation copied anchors and snapshot verbatim into six
exact definitions but never widened limits; the architecture inquiry was
memory-first with repeated attachments after discovery. The current skill
gives one posture and an initial `limit` of 10 that agents read as a session
ceiling. The evaluation record adds the invented-anchor failure class and one
anchoring event in which a delivered analog shaped architecture without
supplying the missing behavior.

A later production investigation exposed the opposite failure: exhaustive
`history.cache` became the FTS terms `history OR cache`, and the skill's
unqualified cursor-completion rule drove eight pages across 1,496 chunks even
after the agent recognized the query was wrong. Correct exhaustive operation
therefore needs both a completion contract for an intended evidence set and an
explicit abandonment contract for a mis-specified one.

Skill and MCP guidance plus one profile-correctness fix: Baseline now forces
configured expansion off, matching its existing forced-off attached-memory
posture, while still rejecting an explicitly enabled expansion. There are no
schema, retrieval-ranking, or product-default changes.

1. Routing precedence is explicit. A usable code identifier, exact anchor, or
   file localizes through the Investigation loop first, even when the eventual
   question is causal or cross-file. Broad memory leads only for genuinely
   anchor-free architecture or workflow questions.
2. An intended exhaustive evidence set still traverses sequentially until
   `truncated` is false, with query, filters, and cursor preserved. A query the
   agent recognizes as mis-specified may be abandoned immediately; its partial
   pages cannot support a completeness claim, and cursor presence is not a
   reason to continue. The replacement query starts a new traversal.
3. After identifier/file localization, `semantic_memory` is queried with the
   exact returned anchor or file only when a causal or cross-file question
   remains. Simple occurrence and convention questions skip memory. Exact
   identifier follow-ups keep vector retrieval and reranking off unless
   lexical evidence is insufficient.
4. A computed-dispatch conclusion requires current-source inspection of both
   the selection predicate and the selected subject's metadata, registry key,
   or equivalent identity.
5. On the first exhaustive page only, core metadata emits `broad_or_query`
   when tokenization yields at least two distinct effective FTS terms and the
   scoped match set contains at least 200 chunks. The warning reports terms,
   `total_chunks`, and a refine-or-abandon message while preserving every
   result and cursor; no cap is added. MCP telemetry records exhaustive total,
   returned, truncated, and warning fields on every successful exhaustive page.
6. Completeness answers state the scope object and separate convention from
   correctness. Exhaustive cursor traversal, expansion, and artifact detail
   reads stay sequential; independent small lexical queries may run in
   parallel. A changed snapshot requires repeating the affected evidence.
7. The shipped skill and both MCP server instruction strings carry the same
   routing. `agent-guide --install` remains non-overwriting, while explicit
   `agent-guide --update ROOT` replaces only the fixed project-local skill path
   so existing installations can opt into corrected guidance.

Acceptance: the skill ships with the G22 fields; a replay of the
links-iteration investigation following the skill reaches the
`rg -w`-listed occurrences and states scope; recorded before and after:
missed gold chunks, false completeness claims, calls, bytes, and telemetry's
exact-anchor definition success rate. A `history.cache`-shaped query over at
least 200 matching chunks warns only on page one and may be abandoned without
a completeness claim; telemetry retains its exhaustive counts and warning.
Install refuses an existing guide, while update replaces that exact guide and
leaves unrelated agent-specific copies untouched.

## G24 — repository documentation retrieval (phase 3 pending)

Repository Markdown and MDX are authored source material. G24 makes them
retrievable through jscout without treating them as code structural evidence.
They are the wrong shape for the code structural and
ranking corpus — a JavaScript parser cannot manufacture documentation chunks,
and prose must not become structural evidence — while still sharing the same
publication snapshot. It is also the wrong shape for semantic memory, whose
artifacts carry code-evidence chains that authored prose does not acquire by
being indexed. The review rounds on PR #96 established that
the shared database cannot host a second lifecycle behind its gate — one
global schema version and structural-snapshot-gated opens — and that a
multiplicative score decay is not bounded in effect once applied to
rank-fusion scores. Storage and ranking are independent axes, so the
resolution is one lifecycle with a separate ranking corpus, not a second
database, which the storage-planes contract rejects; the decision record is
[docs/plans/g24-adr-one-store-separate-ranking-2026-08-25.md](docs/plans/g24-adr-one-store-separate-ranking-2026-08-25.md).
The revised decisions:

1. Documentation lives in the main database and the disposable structural
   snapshot. Every admitted `files` row carries two independent identities:
   `corpus` is ranking-corpus membership (`code` or `docs`), while `format` is
   the parser/format identity (`markdown` for `.md`, `mdx` for `.mdx`).
   Documentation files are ordinary rows with `corpus='docs'` and the matching
   format, produced by the
   same index pass. Documentation sections are `chunks` rows whose `kind`
   describes their intra-file structural role (`markdown_section` or
   `markdown_document`), not their file's corpus or format; they carry no
   `name` or symbols, so the exact tiers cannot match them. Disposable
   `code_files` filters `files.corpus = 'code'`, and `code_chunks` admits only
   chunks joined through `code_files`; code-plane consumers that enumerate
   canonical rows read through those views. `doc_chunk_meta` contains
   documentation-retrieval metadata keyed by chunk id and is not a corpus
   membership marker. Ranking is a separate corpus: docs rows mirror into a new `docs_fts`
   table with their own BM25 statistics and never into `chunks_fts`; docs
   vectors materialize into
   `vec_doc_embeddings_{dimensions}` because sqlite-vec applies KNN's k
   before any join filter; the durable content-addressed embedding cache is
   shared. Code search keeps the same statistics and pipeline as today and its
   ranked content stays byte-identical modulo the shared snapshot identifier.
   The checker, structural support, reconnaissance, scouting, and embedding
   paths use the same central corpus boundary rather than scattered negative
   documentation predicates. Physical database splitting stays rejected per
   the storage-planes contract. Compatibility of a committed `[docs]`
   configuration section with pre-docs binaries is not a requirement.
   `[docs].enabled` independently controls admission (default `true`); disabling
   it admits no documentation files on the next shared index and does not
   disable or otherwise alter code indexing.
2. Corpus: ignore-aware exact-lowercase `.md` and `.mdx` inventory with a fixed root-level hidden
   allowlist (`.github`, `.claude`, `.agents`); Markdown-compatible block chunking that
   never crosses heading boundaries; one document-stub row for body-empty
   documents; deterministic file-only config globs, BOM handling, symlink
   exclusion, and sorted publication. `docs status` reports file decisions for
   `.md`/`.mdx` candidates and for non-document files explicitly selected by
   an `include` glob, plus the deciding rule for each pruned directory; ordinary
   unmatched code and other non-document files do not create docs-status rows.
   MDX otherwise remains raw authored text, but a contiguous leading
   import/export-only preamble is not retrieval-bearing and exact JSX comments
   are removed outside protected code ranges consistently with HTML comments.
3. Retrieval: BM25 always builds for an enabled docs corpus; vectors reuse the existing `[embedding]`
   provider, model, and service — no second provider section and no second
   local model; reciprocal-rank fusion; embedding identity is exactly
   `hash(format_version, nearest_heading, rendered_body)`, with the exact
   provider text and hash preimage fixed by the incorporated contract, so file
   renames and ancestor-heading edits reuse vectors; vector search participates
   only when the current snapshot/profile has a complete persisted readiness
   generation; RRF reuses `k = 60`, with lexical score defined as `-FTS5
   bm25()` and deterministic source-key tie-breaks; the CLI contract is defined directly,
   with `--vector` meaning required vector participation and no vector-only mode
   existing. Documentation vector generation is separately opt-in: only
   `jscout docs embed` materializes missing docs vectors; the normal index pass
   and code `embed` path make no docs-provider requests. `[docs.search].vector`
   controls whether docs search attempts to use a complete docs-vector
   generation and does not itself create one.
4. History: an append-only block-observation ledger, deferred out of the
   numbered phases. Its reasons are supersession lineage ("this passage
   replaced that one" — the backbone of the contradiction story), an ordering
   clock that survives git history rewriting, and finer-than-commit
   resolution under watch; it ships with the supersession product if that
   product is built. Append-only history is not rebuildable from the
   checkout, so the ledger is durable-plane and owes the explicit
   cache-compatibility decision durable changes require. The unified
   lifecycle resolves its two needs with shared mechanisms rather than a
   private clock: a durable `snapshot_log` (sequence, digest, published-at),
   appended in the publication transaction whenever the published digest
   changes, gives the whole index an ordered snapshot timeline — the
   snapshot itself stays a disposable replaced digest — and observations
   reference that shared sequence; a durable rolling `doc_block_state`
   baseline (per block: content hash, position, heading context,
   logical-occurrence ID; no bodies), replaced at each observing scan, lets
   matching always compare last-observed state against current, even across
   a full rebuild, which wipes the disposable plane by design. Matching
   needs only the previous state; accumulated history is matching's output,
   never its input. History recording is a per-format registry property, on
   for Markdown and MDX only. Unchanged blocks add no observation rows, so code
   churn grows nothing, while whole-codebase snapshots tighten observation
   intervals for free. When built, the ledger stays separate from
   the current size-merged retrieval-chunk projection. Matching
   is conservative and one-to-one — exact content first, then uniquely
   neighbor-anchored edited blocks; ordinal position alone never establishes
   continuity and ambiguous matches receive no predecessor. `removed` is
   recorded only when a successfully parsed current corpus confirms a prior
   block is absent. A classified permanent file open/read failure is a visible corpus gap,
   emits no lifecycle transition, and breaks continuity; when that file later
   parses, its blocks start new baseline occurrences. Baseline content without
   Git provenance has unknown authorship time and is never presented as newly
   written. Retryable I/O and every database, transaction, configuration,
   discovery, inventory, cancellation, or consistency-drift failure publishes
   nothing and leaves the complete last-good snapshot active. Version one never creates cross-path
   predecessor edges: Git rename detection is heuristic, so identical content
   at another path starts a new occurrence even when Git reports a rename.
   Pure within-path reordering updates the current projection but emits no
   history event in version one.
   Without usable Git provenance, a pure rename therefore restarts observed
   freshness for every block; this accepted version-one false-recency trade-off
   is bounded by `max_rank_movement` and measured by the renamed-file
   evaluation arm.
5. Freshness: order-based and bounded, not a score multiplier. After
   relevance fusion and optional reranking, each candidate's final rank differs
   from its base rank by at most `max_rank_movement` (candidate value 2;
   evaluation hypothesis), and swaps
   occur only between candidates with comparable provenance: git orders
   against git by latest author time with working-tree lines newest, observed
   orders post-baseline `added` and `body_changed` events by snapshot sequence,
   git and observed never reorder against each other, and unknown provenance
   never moves and is never advantaged. The model reranker never receives
   temporal metadata. Only commits listed by the repository's resolved shallow
   file count as shallow boundaries and contribute no timestamp; provenance
   uses captured indexed bytes with
   `git --no-replace-objects blame --line-porcelain --no-ignore-revs-file --contents - <recorded-head> -- <path>`;
   blame mappings cache by repository-relative
   path, exact file-byte hash, path-tip commit, and shallow boundary
   fingerprint; filesystem mtime is never a fallback.
   `--no-freshness` preserves the relevance order for comparison.
6. Retention: hit content is served from stored current rendered bodies and
   block text; source spans are snapshot-relative and carry the indexed full-
   file hash. Checkout source is read once into an immutable buffer, and only
   that same buffer may be sliced after its hash matches; a separate check then
   read is forbidden. No full raw Markdown copy is stored. After a successful
   replacement snapshot, retired block bodies are not retained in the ledger;
   retired hashes, transition metadata, and content-addressed vectors may
   remain. Version one adds no retention controls.

Delivery status: phases 1, 2, and 4 are implemented. Phase 1 admits Markdown
and MDX at the named-sections tier through the shared index pass, with the
`files.corpus` and `files.format` classifications, `docs_fts`,
`doc_chunk_meta`, the MCP documentation-search surface, and lexical docs
search. Phase 2 adds docs vectors from the shared `[embedding]` profile. Phase
4 adds documentation-aware watch classification through the shared
incremental watcher. The fixed
[retrieval corpus](eval/fixtures/docs-retrieval/manifest.json),
[pre-registration](eval/prereg/g24-documentation-freshness-2026-08-25.md),
[conflict-arm addendum](eval/prereg/g24-documentation-freshness-addendum-2026-08-25.md), and
Phase 2 [human](eval/results/g24-docs-retrieval-phase2-2026-08-25.md) and
[machine](eval/results/g24-docs-retrieval-phase2-2026-08-25.json) reports
satisfy Phase 3's entry prerequisite. Phase 3 — Git-basis provenance and the
bounded freshness reorder — remains unimplemented. The observation ledger is
unscheduled and belongs only to the supersession product.

Phase 1/2 acceptance: code-search ranking, content, and statistics are
byte-identical after docs admission modulo the necessarily changed shared
snapshot identifier —
every admitted file has an explicit corpus and format; `code_files` is exactly
the `corpus='code'` subset and does not infer membership from a format sidecar;
`chunks_fts` contains no docs rows, its term statistics are unchanged, and
the exact tiers match no docs chunk; the checker file inventory contains no
non-code files; docs publish inside the shared snapshot publication;
a repository with no `[embedding]` provider retains full lexical documentation
search; disabling `[docs].enabled` yields no docs rows or docs-status file
decisions while leaving every code surface unchanged; indexing and code
embedding never generate documentation vectors; and crash recovery exposes exactly one complete old or replacement
shared snapshot, never a partial mixture. Deferred ledger/freshness acceptance:
inserting one uniquely distinguishable paragraph produces one `added` block
observation and no succession rows for untouched blocks; globally unique copied
content and Git-detected renames receive no cross-path predecessor; and
freshness movement never exceeds its configured bound or crosses provenance
bases. The detailed implementation
contract
[docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md](docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md)
and the decision record
[docs/plans/g24-adr-one-store-separate-ranking-2026-08-25.md](docs/plans/g24-adr-one-store-separate-ranking-2026-08-25.md)
are incorporated by reference, this entry winning on any explicit
disagreement.

## G25 — multi-format admission (scheduled through G26 phase 0)

One registry owns every format decision that otherwise leaks into walkers,
watch classification, extraction, ranking, checker selection, and resolution.
For each format it fixes the persisted `files.format`, `files.corpus`,
comprehension tier (plain text → named sections → full AST), repository and
dependency admission, extractor contract identity, lexical/vector projection,
exact-definition and exact-occurrence eligibility and scanner, checker
eligibility and watch affinity, repository-reconnaissance/file-policy
eligibility, and resolver strategy.
`chunks.kind` remains the intra-file structural role emitted by the parser.
Callers consume registry capabilities; they do not infer them from
`corpus='code'`, copy extension lists, or maintain independent format switches.

The registry gives a new format a defined place to plug in and prevents one
capability from silently enabling another; that is all it buys. Each format
still needs its own scanner and chunking contract (Markdown's took a full design
cycle). A format joining the code corpus additionally pays BM25/vector
integration, while exact tiers, checker participation, dependency admission,
repository reconnaissance, and structural projections remain separately gated
capabilities. Text-only formats are cheap; languages are real work.

MDX is already admitted by G24 as `format='mdx'` in the docs corpus, using the
same inert named-sections scanner as Markdown: JSX, props, expressions, inner
text, and non-leading ESM remain authored text, never evaluated or projected
into the code graph. The only MDX-specific retrieval subtractions are a
contiguous leading ESM-only preamble and exact JSX comments outside protected
code ranges.

Positions: Groovy/Jenkinsfiles join the code corpus as plain text first —
identifier-shaped searches want them beside TypeScript hits. YAML and TOML
are current-like-code (the checkout is truth; no freshness) but stay out
until G24's documentation pass has shipped and been measured. Helm templates are
not valid YAML and are treated as text if admitted. Tree-sitter is not
adopted; the named revisit trigger is a kind that genuinely needs call-shape
extraction, such as Groovy at the AST tier, where regex extraction would
poison the entity plane.

The north star is the cross-file string-reference plane: an environment
variable read in code but declared nowhere, a Helm value naming a missing
service, a file loaded but never supplied to anything. The entity plane's
string-keyed identities already make these one-store questions; producers for
non-code kinds are the payoff that justifies every admission above the text
tier. No implementation milestone is assigned and no current goal is
displaced.

## In progress G26 — Rust code indexing

jscout is a Rust program that cannot index itself. G26 makes this repository a
standing evaluation corpus and implements G25 against a real second code
format. The decisions:

1. Phase 0 installs the registry and routes the current JavaScript, TypeScript,
   Markdown, and MDX behavior through it without changing any published row,
   ranking, checker input, dependency input, watch signal, or snapshot. The
   registry is the only extension/capability authority after this phase.
2. Phase 1 registers exact-lowercase `.rs` as `files.format='rust'`,
   `files.corpus='code'`, repository-admitted and lexical-ranking-eligible.
   Rust is not dependency-admitted, exact-definition-eligible,
   exact-occurrence-eligible, checker-eligible, or resolver-enabled. Rust watch
   events schedule shared incremental refresh but carry no checker dirty path.
   `target` is not added to global `walk::SKIP_DIRS`; only a directory named
   `target` whose parent contains `Cargo.toml` is a Cargo-output root in phase 1.
   Rust is also excluded from repository reconnaissance membership and its
   disposable file-policy projection; lexical code-corpus admission does not
   imply semantic-policy admission. Code search accepts an optional plural
   `formats` allowlist of registry ids; omission means every registered code
   format. The same normalized scope applies before limits to ranked lexical,
   exact, vector, reranker, and exhaustive candidate generation, is echoed in
   exhaustive `scope`, and binds its cursor. The original explicit allowlist is
   copied unchanged across compatible follow-ups and is never reconstructed
   from echoed scope.
3. The pinned parser is `ra_ap_syntax`: lossless byte ranges, error tolerance,
   and no C toolchain. Phase-1 chunks form a non-overlapping, gap-free partition
   of the source, carry `kind='rust_text'`, no name/symbol/scope, and an empty
   graph. Top-level syntax ranges are preferred boundaries; residual text is
   retained, and oversized ranges split only at a newline or UTF-8 boundary.
   Parse errors do not reject the file and are counted in index/watch refresh
   diagnostics. The parser edition comes from the nearest visible package
   `Cargo.toml`, including `edition.workspace=true`; absent editions and
   standalone files use Cargo's Rust-2015 default. The effective path-to-edition
   map is a persisted extraction/snapshot input, so an edition-only manifest
   edit reparses only Rust files whose effective edition changed. Invalid
   edition context recovers visibly to the default. Non-UTF-8 source and
   deterministic extraction failures are per-file rejections; retryable I/O
   and panics remain publication-fatal. The Rust extractor contract is
   versioned per format; changing it does not invalidate unchanged JavaScript
   or TypeScript rows.
4. Phase 2a replaces the text projection with a non-overlapping partition of
   named item chunks plus residual unnamed chunks—never duplicate full-text and
   named rows. Exact definitions, exact occurrences, and Rust vectors remain
   disabled throughout 2a. Phase 2b may add exact definitions only after a
   committed mixed-corpus collision protocol passes. Rust-aware exact
   occurrences are a separate capability and do not turn on merely because
   names exist.
5. Phase 3 adds Rust module edges. Before code begins, its contract must fix the
   exact `cargo metadata` invocation, no-network/no-mutation policy, tool and
   input identity, failure behavior, supported path forms, and unresolved-edge
   reporting. Phase 1 observes visible `Cargo.toml` only for output-directory
   membership and parser edition; Cargo configuration and every other declared
   metadata input become watch refresh boundaries in phase 3.
6. Out of scope: entity extraction, events, member calls, checker enrichment,
   macro expansion, dependency-crate indexing, and rust-analyzer semantics.
   Inline `#[cfg(test)]` modules indexing with their containing file's role is a
   recorded role-granularity limitation. Exact `test.rs` and `tests.rs`
   basenames carry the deterministic `test` file role.

Canonical `files` and `chunks` remain shared, and phase 1 keeps one code FTS
ranking corpus. A format filter is a scoping tool, not statistics isolation:
FTS5 document frequency and average document length still include every code
format, so filtered JS/TS ranks need not be byte-identical to a Rust-free
database. The first treatment's post-hoc projection found only a small residual
effect, but that inspected pilot is not confirmatory evidence. Separate
per-format FTS statistics are revisited only if a prospectively judged
mixed-language evaluation shows persistent domination—irrelevant
cross-language hits pushing relevant same-language gold below K on
single-language-intent queries—rather than relevant competition.

Phase 1 adds `format` as a sqlite-vec partition key, searches each requested
origin/format partition, and merges same-profile cosine scores. Rust vectors
remain disabled until named phase-2a chunks exist and a Rust embedding
evaluation passes. Later Rust enablement reuses the existing embedding model
and cache; different models would require rank fusion instead. Only then does
Rust change from `CodeLexical` to `CodeLexicalAndVector`.

Phase 0 acceptance: a differential fixture indexes the same JS/TS/Markdown/MDX
repository before and after the registry refactor and compares every public
code/docs surface and pre-existing canonical column byte-for-byte. The only
normalized additions are newly introduced format-contract metadata and the
phase-1 `files.parse_error_count` diagnostic column, whose zero default is
asserted separately—the refactor must otherwise be a complete no-op.
Phase 1 acceptance: repository `.rs` files
index while selected dependency `.rs` files do not; authored non-Rust content
under an ordinary `target/` remains admitted; Cargo-output `target/` is pruned;
Rust rows never enter exact tiers, checker inventory, checker dirty affinity,
JS-specific fact tables, or module edges; spans slice exactly on multibyte,
raw-string, and CRLF content; malformed Rust remains searchable and reports its
parse-error count; direct/workspace/default Cargo editions select parser
context and edition-only changes re-extract Rust; deterministic Rust read or
extraction failures reject only that file; and a Rust-only change preserves all
JS/TS canonical rows and checker carry inputs. The prospectively committed v4
provider-free protocol has clean baseline and treatment arms. Filtered parity reuses the
previously inspected v3 JS/TS regression cohort; it is a regression guard, not
fresh confirmatory evidence. It compares a Rust-free baseline with the mixed
index searched using `formats=['javascript','typescript']`; JS/TS Recall@10
does not decrease, MRR drops by no more than 0.02, and baseline top-five gold
stays top-ten. Mixed relevance instead uses a fresh source-only holdout and
searches with formats and file roles omitted, therefore admitting every
deterministic role. For each query, one blinded pool unions the
baseline and treatment top-ten files plus authored positive recall sentinels,
and every pooled query-file pair receives an explicit `0`–`3` qrel. Baseline
and treatment nDCG@10 use that same complete pool and gain `2^grade-1`;
treatment mean nDCG@10 must be at least `0.70` and may trail baseline by no more
than `0.02`. Missing qrels invalidate v4 scoring rather than receiving zero
gain. Language representation and known-positive Recall@10 are reported but
not gated. Both arms retain 100 raw ranked chunks, deduplicate files by first
occurrence, and truncate to 10. Every filtered-parity raw hit must be
JavaScript or TypeScript. Each arm's query responses share one nonempty
snapshot; the two arms need not share a snapshot because their indexed
memberships differ. V4 first writes arm reports, the blinded pool, qrels, and
the score report to fresh paths outside every evaluated checkout. Those
artifacts record indexing duration, database bytes, index stdout/stderr
diagnostics, and raw query results. After scoring, a hash-linked result record
and any selected immutable artifact copies are preserved under
`eval/results/`. V4 completed as the decision-grade formal failure recorded in
[eval/results/g26-format-scope-v4-failed-2026-08-26.md](eval/results/g26-format-scope-v4-failed-2026-08-26.md).
The format-scoped JS/TS regression contract passed, but mixed treatment mean
nDCG@10 was `0.597555`, below the frozen `0.70` absolute gate. Phase 2a named,
item-local Rust chunks is therefore the current milestone. Exact definitions,
exact occurrences, Rust vectors, and module edges remain disabled pending their
own prospective acceptance protocols. The detail document
[docs/plans/g26-rust-indexing-proposal-2026-08-25.md](docs/plans/g26-rust-indexing-proposal-2026-08-25.md)
is subordinate and non-normative, this entry winning on any disagreement.
Phases 0 and 1 remain the built substrate for that work.

The first phase-1 treatment formally failed the preregistered mixed-corpus
control gate: relevant Rust implementations displaced one legacy JS/TS gold
file from the combined top ten. A post-hoc JS/TS-only projection stayed within
the numerical thresholds, but it is diagnostic evidence, not a passing result.
The v3 replacement's filtered regression arm passed: JS/TS Recall@10 remained
`1.0000`, mean MRR improved from `0.8833` to `0.8854`, and every baseline
top-five gold file stayed top-ten. Its mixed arm still failed the frozen
`0.70` nDCG gate at `0.5084`, so the result remains a formal failure under
`eval/results/`. That mixed score was not decision-grade,
however: the evaluator called source-authored positive qrels a pool, judged
only 68 of 240 returned top-ten slots, supplied no explicit zeroes, and treated
every unjudged file as zero gain. The same output also contained real misses,
so neither dismissing the failure nor changing its judgments after inspection
is allowed.

V4 then completed the blind pool and assigned explicit qrels to every pooled
candidate, making its formal failure decision-grade. Filtered Recall@10 stayed
`1.000000`, mean MRR improved from `0.883333` to `0.885417`, and no baseline
top-five positive left the top ten, so format-scoped JS/TS retrieval remains
validated. Mixed treatment nDCG@10 improved from the Rust-free baseline's
`0.310561` to `0.597555`, and authored-positive Recall@10 improved from
`0.319697` to `0.550000`; relevant Rust therefore improved the combined
ranking, leaving no evidence for per-language statistics, quotas, or weights.
The treatment nevertheless failed the frozen absolute `0.70` nDCG gate.

G26 consequently advances to Phase 2a named, item-local Rust chunks while
exact definitions, exact occurrences, Rust vectors, and module edges remain
disabled. Identifier aliases must not be appended to the broad phase-1 chunks;
any alias experiment belongs to the item-local projection and requires its own
prospective test. The Phase-2a replacement protocol must freeze `file_roles`
explicitly: default retrieval omits it and includes tests, while a separate
production-only experiment requires a classifier audit and cannot silently
reframe the all-role v4 result. The complete v4 result and immutable artifact
hashes are recorded in
[eval/results/g26-format-scope-v4-failed-2026-08-26.md](eval/results/g26-format-scope-v4-failed-2026-08-26.md).

## Evaluation decisions already made

The dated evidence remains under `eval/`; this section records only the design
consequences that still govern implementation.

| Finding | Current consequence |
|---|---|
| Unassisted Codex sessions made zero jscout calls; MCP metadata alone did not create adoption | Ship explicit project-local agent guidance; do not generalize the adoption result to every client/model |
| Grep, baseline, and structural arms reached the same correctness ceiling while structural retrieval initially read more irrelevant files | L1 retrieval investment is closed; expansion stays opt-in and file-role/origin policy applies before budgets |
| Whole search responses grew materially when structural context was attached | Complete rendered-response byte budgets are a permanent contract |
| The preregistered file-role revision reduced structural irrelevant inspection to an interval including zero without creating a correctness win | Keep high-precision deterministic roles as bootstrap signals; move ambiguous directory and project purpose into the G13 reconnaissance overlay rather than adding repository-specific path rules |
| Full versus elided source retained answer quality but did not reduce selected-artifact bytes/calls | Full source remains the default; custom behavioral IR is not earned |
| Fixed-snapshot workflow memory replay delivered artifacts in every correct warm token win and reduced median session-2 tokens | Keep evidence-backed workflow memory opt-in and proceed with the shared semantic engine |
| Free-form workflow participant synthesis omitted deterministic continuations | Candidate closure and exhaustive classification are mandatory |
| Standalone `neighborhood` had no natural selection while expanded search used the same machinery | Treat neighborhood as drill-down plumbing; prioritize agent-reached surfaces |
| Optimistic-prefetch memory replays delivered fresh vector-backed context but 46 implementation arms never generated the mechanism found by one read-only architecture probe | Treat retrieval polish as G14 hygiene; test a read-only design phase followed by implementation in the eval harness before considering any persistent jscout product surface |
| Root-layout searches ranked partial example and Sitecore chunks above exact identifiers | Implement G17 deterministic exact-identifier tiers before adding retrieval surface |
| Root-layout scouting spent 448 card calls without covering the relevant type-generation surface, while broad semantic-memory calls returned mostly unrelated artifacts | Implement G18 scope-stratified and targeted scouting plus support-aware compact semantic discovery; do not increase default budgets |
| Two-phase root-layout arms cost more, passed less often, and preserved wrong design contracts | Park G15; retain two-phase design only as an optional evaluation treatment |
| Checker/scout/vector passed the root-layout oracle in both counterbalanced trials while grep failed both; those passing arms received no semantic artifacts | Preserve the full deterministic/checker/vector substrate and fix ranking/selection; do not attribute the separation to semantic memory or flip defaults from one task |
| Architecture-inquiry use on a 7,000-plus-file monorepo localized a verified cross-package workflow, while parallel expansion/full-artifact reads caused noise and import occurrences displaced behavior | Preserve jscout's localization/source-verification role; add staged sequential guidance, syntax-aware G17 occurrence ordering, and authorized evidence-backed consolidated workflow write-back; do not generalize tool-selection behavior to implementation tasks, trigger G16, or claim retrieval rank measures importance |
| The same 42-call architecture inquiry produced an estimated 460–510 KiB of jscout output, while only 25–35% was judged decision-relevant | Implement G20 compact artifact views, routine-diagnostic gating, cross-origin-safe tiered follow-ups, and path-shaped expansion; separate fixed-call transport savings from staged-session savings and target at least 60% measured aggregate byte reduction at fact parity before considering larger budgets or session state |
| A 19-call TargetsQueue problem-solving investigation naturally selected four exact definitions that carried the mechanism in 11.6 KB, while nine expansions produced 162.9 KB and weak attached memory | Preserve exact definition as progressive drill-down; make search-attached memory opt-in, stop broad expansion after localization, and replay G20 on this workload separately from the architecture inquiry |
| The same investigation exposed an unbounded generic receiver string and `used_by` counts derived from unresolved repository-wide names | Bound compact receiver displays with full debug fidelity; replace those counts with anchor-resolved edges or label them as approximate name occurrences |
| The first production telemetry window mixes binaries and intentionally different retrieval postures; vector-active rows also rerank and no relevance labels exist | Use it for incident discovery only; use G21 configuration/build fingerprints and stage timings, preserve retrieval defaults, and compare reranking with one variable changed before any global flip |
| Three 1.86 MB `who_uses` responses predate the current compact whole-response ceiling | Replay a high-fanout case on the current binary and measure follow-up value; do not schedule a duplicate cap implementation or treat historical bytes as current behavior |

Relevant result summaries include:

- [ai-pipe P0](eval/results/ai-pipe-p0-2026-08-07.md)
- [discriminating three-arm run](eval/results/ai-pipe-discriminating-2026-08-07.md)
- [n8n/Twenty post-cutoff run](eval/results/n8n-twenty-post-cutoff-2026-08-09.md)
- [file-role rerun](eval/results/file-roles-rerun-2026-08-09.md)
- [memory budget replay](eval/results/twenty-memory-budget-replay-2026-08-09.md)
- [workflow candidate gate](eval/results/twenty-workflow-candidate-gate-2026-08-09.md)
- [runtime-boundary entities](eval/results/runtime-boundary-entities-2026-08-10.md)
- [contract plane](eval/results/contract-plane-2026-08-10.md)
- [general entities](eval/results/general-entities-2026-08-10.md)
- [agent surfaces](eval/results/agent-surfaces-2026-08-10.md)
- [logical workflow routing](eval/results/workflow-logical-routing-2026-08-10.md)
- [dependency indexing](eval/results/dependency-indexing-2026-08-10.md)
- [AFFiNE contextual reranker smoke](eval/results/affine-reranker-context-2026-08-14.md)
- [Next.js optimistic-prefetch campaign](eval/results/next-optimistic-prefetch-2026-08-15.md)
- [Next.js root-layout parameter types](eval/results/next-root-params-types-2026-08-17.md)
- [workflow architecture-inquiry call trace](eval/results/workflow-architecture-inquiry-2026-08-19.md)
- [TargetsQueue problem-solving investigation](eval/results/targets-queue-problem-investigation-2026-08-20.md)
- [cross-trace retrieval synthesis](eval/results/retrieval-cross-trace-synthesis-2026-08-20.md)
- [first production MCP telemetry window](eval/results/mcp-telemetry-first-window-2026-08-20.md)
- [repository runtime configuration implementation plan](docs/repository-configuration-plan-2026-08-20.md)

## Positioning versus tsserver/LSP

jscout does not compete with a configured TypeScript language server on typed
definition/call hierarchy, diagnostics, rename/refactor safety, or precise
interface-to-implementation navigation. Agents should use an LSP for those
operations when it is available and healthy.

jscout provides a different repository-level surface:

- one runtime-oriented model across JavaScript and TypeScript;
- explicit candidates for the dynamic tail an LSP cannot prove;
- entities, dependency boundaries, and cross-file workflow paths;
- bounded snapshot-labelled context for agent consumption;
- persistent evidence-backed semantic and agent memory across sessions.

Do not reimplement general LSP machinery. Optional occurrence-scoped
receiver/member enrichment is implemented as G10 (post-v1) — a deliberate
pull of the original deferral trigger. Closed answers with at most three fully
mapped targets are recorded at `likely` with `checker` provenance; larger or
incomplete answers remain `possible` candidates. Everything else typed
navigation offers remains the LSP's job.

## Deferred or out of scope

- cross-edit stable symbol identity;
- runtime traces;
- checker-backed enrichment beyond G10's occurrence-scoped receiver/member
  resolution
  (diagnostics, rename/refactor safety, call hierarchy — use an LSP);
- learned compression or learned traversal policy;
- LLM-generated pseudocode as source truth;
- one model call per chunk;
- embedding clusters presented as domain concepts;
- semantic claims modifying deterministic structural facts;
- hidden model spending during index/watch;
- autonomous tool-using agents inside the gateway;
- automatic provider/model/billing fallback;
- migrating embeddings through pi-ai;
- blanket dependency indexing;
- Yarn Plug'n'Play dependency archive indexing.

## Retained decision rationale

| Decision | Reason | Revisit trigger |
|---|---|---|
| Keep chunks out of graph identity | Chunk boundaries follow retrieval budgets and churn independently of repository identity | A concrete query needs chunk identity independent of source anchors |
| Rebuild the graph projection after indexing | Barrel edits can reroute unchanged importers; full rebuild is simpler and correct at current scale | A measured repository exceeds the acceptable projection budget |
| Use hubs/candidates for uncertain dynamic relationships | Direct pairing creates false edges and quadratic fan-out | G10 adds occurrence-specific `likely` edges while retaining shared hubs as the `possible` fallback |
| Separate entities from occurrences | Canonical identity and evidence sites have different lifecycle/provenance semantics | No planned revisit |
| Keep structural expansion off by default | Evaluation did not show a default-workload outcome gain and simple lookups do not need graph context | Real agent work shows a reliable default benefit |
| Treat traversal weights as heuristics | Published systems do not validate jscout's edge kinds, confidence mapping, or workload | Repository-specific evidence supports tuning or learning |
| Keep generated semantics separate from structural facts | Model interpretation can be useful without becoming source truth | No planned revisit |
| Prefer workflows before cards/hierarchy/concepts | Workflows answer a cross-file relational query structure alone cannot answer and prove the shared semantic machinery cheaply | Implemented; no revisit needed |

## Research references

- [cAST — code-aware syntax-tree chunking](https://arxiv.org/html/2506.15655v1)
- [Hierarchical Context Pruning](https://arxiv.org/abs/2406.18294)
- [ProConSuL — call-graph-aware summarization](https://aclanthology.org/2024.emnlp-industry.65/)
- [Higher-level code summarization](https://arxiv.org/abs/2503.10737)
- [RepoSummary](https://arxiv.org/abs/2510.11039)
- [CodePromptZip](https://aclanthology.org/2026.findings-acl.1384/)
- [RepoDistill](https://aclanthology.org/2026.findings-acl.217/)
- [LARGER](https://arxiv.org/abs/2605.16352)
- [Aider repository map](https://github.com/Aider-AI/aider/blob/main/aider/repomap.py)
- [OXC](https://oxc.rs)
- [Knip](https://github.com/webpro-nl/knip)
- [Stack graphs](https://github.blog/open-source/introducing-stack-graphs/)
