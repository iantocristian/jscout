# jscout architecture and implementation plan

> Status: authoritative plan as of 2026-08-17.
>
> G1–G10 have functional implementations, but G10 is not accepted for
> large-repository operation until its required scale correction passes. G11
> snapshot simplification, G12 watcher coordination and incremental source
> refresh, G13 repository reconnaissance, and G14 retrieval handoff are
> implemented. G15 design-before-edit task memory is parked as
> product-surface expansion while the same hypothesis is tested entirely in
> the evaluation harness. G16 is also parked; it remains a conditional
> memory-delivery correction, not a committed rewrite.

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

**Amendment — disposable snapshot boundary (2026-08-13).** This section is
authoritative over older text that describes retaining checker facts across
reindexing or maintaining cross-snapshot checker-input freshness. jscout uses
one SQLite database with three logical lifecycles, not three physical
databases:

| Plane | Contents | Lifecycle |
|---|---|---|
| **Disposable structural snapshot** | Files, chunks/FTS, symbols, imports/exports, references, events, member calls, contracts, entities, package instances, checker batches/facts, graph projection, and materialized vector occurrences | Rebuilt from the current checkout. Snapshot-dependent rows are deleted first, except that checker batches may be reused only when the rebuilt structural snapshot is byte-identical to their source snapshot. |
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

Checker enrichment is also snapshot-bound. A full index deletes checker facts
whose source snapshot differs from the rebuilt snapshot; an exact-snapshot
batch remains reusable. `jscout enrich` may populate the changed published
snapshot. No version ladder, transitive input manifest, or cross-snapshot
revalidation is required to carry checker answers through a changed rebuild.
Within one snapshot, publication still validates source hashes, occurrence
spans, project answers, and the snapshot race boundary.

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
| Storage | One versioned SQLite database; schema v23; three explicit logical lifecycles; FTS5, provenance-keyed embedding caches, dimension-specific sqlite-vec `vec0` indexes, canonical extraction tables, graph projection, durable reconnaissance policy, semantic artifacts, run ledger, and freshness metadata |
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

As implemented, schema v18 stores exact call/receiver/property byte spans and
canonical checker batches. `jscout enrich` drives a pinned Node/TypeScript
sidecar explicitly; `jscout checker doctor` reports project/configuration
readiness. The protocol host isolates compiler work in a terminable worker,
and the Rust client enforces a hard deadline. Projection v11 recreates only
fresh occurrence-specific `checker` edges and retains the shared possible
member hubs.

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
answers may coalesce. Conflicting targets remain a visible `possible` candidate
set, or `unknown` when they cannot be mapped safely; they never become one
arbitrary `likely` edge.

An owning project that returns `unknown` is incomplete coverage, not evidence
against a clean resolution produced by another owning project. It therefore
does not demote otherwise agreeing resolved answers. Canonical occurrence
coverage retains its project ID, status, and input fingerprint; projected
checker edges expose those IDs as `unknownProjects`. Multiple mapped targets or
an unmappable declaration from a resolved answer still make every survivor
`possible`. The complete answer for the selected plan, including explicit
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
leak the answer into unrelated `.insert()` calls. One unambiguous mapped target
is `likely` with provenance `checker`. Multiple valid declaration targets —
from unions, overload ownership, inheritance, or disagreeing projects — remain
separate `possible` candidates. Existing hubs are retained for unexplained
dynamic calls. Contract-plane consumers may attach the receiver's declared type
as documentary evidence under the same provenance.

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
during the enrichment command, not as a cross-snapshot freshness join. The pass
publishes only after rechecking its structural snapshot, occurrence source
hashes, target anchors, and current checker inputs. A structural snapshot race
publishes nothing; the scale-corrected path withholds and reports a project
whose external inputs drift while allowing explicitly covered unaffected
projects to assemble one partial batch. Only one batch is active; a full
`jscout index` deletes active and staging checker state only when it belongs to
a different rebuilt structural snapshot.

`jscout watch --enrich` makes replenishment automatic. Each relevant event is
debounced, indexed first, then enriched. A checker failure leaves the current
snapshot without checker edges unless the scale-corrected planner reaches a
controlled partial activation with explicit coverage. Either condition remains
retryable. External-input watching and generation cancellation belong to the
later watcher coordinator; the fixed-snapshot path does not retain a
manifest-rehashing subsystem for them.

### Required G10 scale correction

**Implementation status (2026-08-14).** The correction below is implemented in
checker protocol v2 and schema v19: complete-by-default/manual planning,
configuration-only ownership discovery, package/file spread ordering, bounded
per-project batches, one disposable Program worker per project, once-per-project
source mapping, durable batch staging/resume, controlled partial activation,
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
a configuration-only sidecar phase that resolves file-to-project ownership;
neither builds a project `Program`. The plan reports discovered, eligible,
selected, and skipped occurrences by file role, package/area, property, file,
and planned project ownership. It pins the structural snapshot, selection
policy, and ordered occurrence IDs in a plan fingerprint.

The default plan:

- includes `repository`/`workspace` production and unknown-role files;
- excludes test, fixture, generated, and documentation roles unless selected;
- excludes occurrences already explained by a direct, occurrence-bound
  `certain` or `likely` structural edge (currently including namespace-member
  calls resolved through the module/export graph); line or name coincidence is
  never sufficient;
- requires at least one current property-hub target candidate;
- ranks exported/entity/workflow boundaries and watcher-supplied changed files
  ahead of unanchored internal calls;
- spreads selection within each rank tier by deterministic round-robin across
  packages, then across files within each package, with occurrence ID as the
  final in-file order; lexicographic package, file, anchor, or property order
  must not let one prefix monopolize early staged progress or an explicitly
  capped run;
- selects every eligible occurrence; batching, project-worker disposal, and
  durable staging bound resources rather than discarding repository coverage.

Repeatable `--file`, `--package`, `--member`, and `--role` selectors narrow the
plan. `--max-occurrences N` is an operator-requested runtime cap applied after
the deterministic spread order; without it, manual `jscout enrich` has no
occurrence-count cap. `--all` broadens eligibility to normally excluded roles
and already `certain`/`likely` calls for audit or diagnostic runs; it is not
required for ordinary complete repository enrichment. Hitting an explicit cap
is successful partial enrichment only when the report and stored batch coverage
expose the omitted count. An occurrence without a checker fact keeps the
existing `possible` property-hub path, so bounded coverage cannot fabricate
certainty or create a false negative.

Rust owns source-hash verification and caches it once per distinct file for the
run. It also owns declaration-to-anchor mapping, selection coverage, budgets,
staging, resume, final source/target/snapshot checks, and projection activation.
Node never receives database access or repository source contents over the
protocol; it receives repository-relative paths, indexed hashes, and spans.

#### Project scheduling and batched protocol

Project discovery builds one reverse file-to-owning-project index. Ownership is
enumerated once for the planned file set, not rediscovered for each occurrence.
Conflicting owners remain visible under the existing ambiguity rules.

Rust schedules one configured project at a time by default. A project worker
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

#### Durable staging, resume, and partial coverage

An enrichment run is keyed by structural snapshot, plan fingerprint, checker
protocol/version, TypeScript identity, and execution policy. Rust commits
bounded inactive staging rows after each successful query batch or project.
Restarting the same command resumes the matching run; a changed snapshot or
plan starts a new run and makes the old staging rows collectible. Staging has a
bounded retention policy and never enters `resolved_edges`.

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

`watch --enrich` uses the same planner, batching, staging, and optional explicit
`--max-occurrences` machinery. Changed files in the current generation define
its ordinary incremental scope and are ranked first; startup/full-refresh
generations may cover the complete eligible repository, and the watcher never
implies `--all`. A newer structural generation cancels between batches, and
staged work may resume only when its exact snapshot and plan still match. After
structural indexing, checker program construction waits for a configurable
enrichment quiet period, defaulting to the G12 two-second trailing quiet period;
any newer event resets that wait. Sustained churn may therefore starve checker
enrichment by design while deterministic indexing continues to converge. A
cancelled enrichment is not immediately relaunched: the coordinator waits for
the next quiet point, then resumes only exact matching staged work or starts one
new plan. The G12 coordinator must not be declared operational with `--enrich`
until this correction is implemented.

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
  unjustified `likely` edge;
- an all-failed run and a zero-fact partial run preserve the previously active
  batch, while one Ctrl-C stops the project loop without activating partial
  coverage;
- killing the checker after staged progress and rerunning resumes the exact
  snapshot/plan without redoing committed batches or exposing staging rows;
- a source, target, config, ambient declaration, or TypeScript-runtime change
  during the run cannot activate raced facts;
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
4. Remove checker cross-snapshot retention from the fixed-snapshot path. A
   rebuild drops batches from a different snapshot; an identical rebuilt
   snapshot may reuse its exact-snapshot batch. Enrichment republishes changed
   snapshots explicitly. Per-project manifest rehash/freshness joins are
   removed. **Complete; exact-snapshot reuse amended in G12 review.**
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
- checker facts from a different snapshot and package-instance ownership from
  the old rebuild do not survive; exact-snapshot checker facts may be reused;
- a genuine v15 embedding layout is rejected without mutation, while the v16
  durable floor preserves compatible embedding and semantic-memory rows;
- a fatal required-phase failure never publishes a snapshot marker describing
  new or partially rebuilt structural rows; individual file read/extraction
  failures may publish the same visibly reported partial snapshot as
  `jscout index`;
- retrieval-only commands do not create or migrate a missing database;
  semantic dry-run planners should follow the same rule after the noted
  command-authority cleanup.

## G12 — watcher coordinator

**Implementation complete (2026-08-17); sustained-churn validation on a large
real repository remains pending.** The production watcher uses a pure
generation coordinator, a typed full/incremental refresh scope, fresh per-phase
connections, explicit optional embedding/checker phases, supersession and
cancellation, bounded retry and stable-failure degradation, exact self-output
exclusions, dynamic external coverage, and periodic reconciliation. Unit and
fixture coverage passes; the next operational step is to run it through branch
switches and ordinary edits on the user's target repository.

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
```

Startup and reconciliation generations run a full disposable-plane refresh.
An ordinary bounded batch of JavaScript/TypeScript source paths uses
incremental extraction: it still walks and hashes the complete current source
tree, but preserves unchanged first-party rows and parses/replaces only changed
or missing files. Dependency discovery, module resolution, snapshot
calculation, stale checker-batch retirement, vector occurrence
rematerialization, and projection publication still run against the complete
resulting snapshot. `jscout index` remains a full rebuild; the incremental path
is a watcher latency optimization, not a second correctness model.

A source batch is promoted to full refresh when it contains more than 256
distinct paths. Git HEAD or submodule controls, source-inventory ignore files,
package/workspace manifests, lockfiles, tsconfig/jsconfig and declaration
inputs, selected dependency roots, external checker inputs, directories,
backend errors, and unclassifiable missing paths also require full refresh.
Full scope is sticky within a generation, so a mixed event cannot be downgraded
by later source notifications. A changed file that cannot be read or extracted
is removed from the published partial snapshot rather than leaving its
previous structural row live.

G12 does not promise uninterrupted queries during refresh. Publish-then-swap,
database generations, or a second structural database would add lifecycle
machinery that the fixed-snapshot design intentionally removed. Existing
n8n/Twenty reports put repository indexing between roughly 7 and 50 seconds,
depending on the enabled planes and checkout; these are scale observations,
not a latency target. A query may report that no snapshot is published for the
entire structural-refresh interval, and every cycle logs its actual phase
durations.

`--embed` and `--enrich` remain explicit. Plain watch performs no model calls,
does not start the TypeScript checker, and never serves checker facts from a
different structural snapshot. It may reuse an active exact-snapshot batch
when either refresh mode proves the snapshot unchanged. Dependency selectors
remain authoritative and must be supplied to watch exactly as they are to
index.

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
  -> embedding(generation, snapshot)   [only with --embed]
  -> enriching(generation, snapshot)   [only with --enrich]
  -> clean | degraded(snapshot, warnings)

any phase + newer event -> dirty(newer generation)
any failed phase        -> retry-wait(same generation, phase) -> retry
```

Events received during a phase are not consumed by that phase. They advance
the desired generation and force another structural refresh before the
watcher can become clean. Structural work is allowed to finish rather than be
cancelled mid-transaction; optional embedding work stops between batches and
checker work terminates its bounded sidecar when superseded. Before starting
either optional phase, the coordinator drains pending events and skips that
phase if a newer structural generation is already required.

A structural refresh that returns individual file failures may expose the
same explicitly reported partial snapshot as `jscout index`. The watcher
reports every path/stage/error and retries the generation without requiring
another filesystem event. Three consecutive refreshes with the same failure
fingerprint (path, stage, and error) make the snapshot `degraded` rather than
permanently dirty: optional embedding and enrichment may then run against that
exact partial snapshot, while the failed paths remain on the reconciliation
retry set. The last degraded fingerprint survives later generations, so the
same permanent failure can degrade immediately rather than paying three full
retries every cycle. A changed failure fingerprint or successful read resets
that stability; new input resets only the current retry timer. Fatal refresh
errors never enter this bounded degradation path and remain dirty until a
required phase succeeds.

Failures use bounded exponential backoff. A parked retry gates fresh work for
that generation and is consumed when it starts; attempts reset on new input or
a successful phase. Retry and stable-failure state live in memory. Restarting
watch always subscribes first and then performs a full refresh, so no persistent
watcher journal or recovery schema is required.

### Trigger and reconciliation policy

Relevant events carry a typed refresh scope. Indexed source-file create,
update, delete, and rename paths select incremental extraction while all
resolution, ownership, checkout, dependency, and uncertain boundaries select
full refresh. Scopes coalesce during debounce, full scope dominates and remains
sticky for the generation, and more than 256 distinct source paths promotes
the generation to full refresh. The incremental executor still scans the
complete source tree and runs complete resolution and publication, so event
paths are optimization hints rather than the correctness inventory.

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
escalated: documentation, editor metadata, and other unindexed files therefore
do not rebuild the repository. Pathless/rescan events, directories, and
otherwise uncertain missing paths remain conservative full-refresh triggers.

After each refresh or enrichment, the coordinator reconciles its narrow
external watches with the newly resolved package instances and checker input
set. These paths are ephemeral coordinator state, not a cross-snapshot
freshness manifest stored in SQLite.

Failure to register a narrow external watch retries with backoff. Three
consecutive failures for the same path move that path to `degraded` coverage:
the coordinator logs it, relies on periodic reconciliation for that path, and
retries registration on the next reconciliation tick or when the external
path set changes. Persistent registration failure does not itself keep the
structural generation dirty or cause a full-refresh loop.

Notification backends can miss events, so a configurable reconciliation timer
(default ten minutes) schedules a full refresh even when no event arrived.
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

A structural refresh removes checker batches from every different snapshot
before publishing its graph. An active batch for the exact rebuilt snapshot is
retained and reprojected:

- plain `watch` starts no checker work and may retain only exact-snapshot facts;
- `watch --enrich` reuses an exact-snapshot batch as a no-op or publishes one
  batch bound to the changed snapshot;
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
   retry state, debounce, degraded snapshots/coverage, and structured cycle
   telemetry without timing-dependent tests.
2. Replace the pre-G12 watch loop with the normal full-refresh operation. Open
   a fresh connection per phase, configure `busy_timeout`, audit rollback
   paths, implement bounded stable-file-failure degradation, and make fatal
   failures retry automatically.
3. Invalidate cross-snapshot checker state through the structural refresh while
   retaining a reusable exact-snapshot batch; sequence optional embedding and
   exact-snapshot enrichment, and add generation checks plus cancellation
   between/within optional work. **Amended after implementation review.**
4. Add exact self-output exclusion plus Git/worktree, submodule,
   selected-dependency, and dynamically reported checker-input watches. Treat
   notification backend errors as full-refresh uncertainty and persistent
   registration failures as degraded timer-backed coverage.
5. Add periodic reconciliation, bounded retry/backoff, concise generation and
   phase logging, then remove assumptions that another repository event is
   required to recover from failure.
6. Update README operational guidance after the coordinator acceptance suite
   passes.
7. **G12.1 amendment (2026-08-17):** promote the already parity-tested
   incremental extractor to a production watcher operation. Add typed event
   scope, sticky full fallbacks, a 256-path promotion bound, fail-closed stale
   row removal, exact-snapshot checker retention, and refresh-scope telemetry.
   Keep manual `index` full-refresh-only.

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
- branch switches replace the complete file set without retaining old files,
  projections, package ownership, vector occurrences, or checker facts from a
  different snapshot;
- submodule, manifest, lockfile, selected dependency, symlink-target, tsconfig,
  TypeScript runtime, and ambient declaration changes converge;
- edit -> enrich -> revert cannot reactivate a checker batch created before
  intervening external checker-input changes;
- bounded source-only generations parse only changed files and report
  unchanged-file reuse, while startup, reconciliation, branch/config/package,
  large-batch, and uncertain generations use full refresh;
- no refresh mode can project a checker batch from a different snapshot;
- plain watch never serves checker edges from an older generation;
- `watch --enrich` publishes checker facts only for the current exact snapshot,
  and superseded checker work is cancelled or discarded;
- a transient failed-file result remains dirty initially, reports the exact
  path/stage/error, and converges after the file becomes readable;
- three identical failed-file generations publish a visibly degraded snapshot,
  allow exact-snapshot enrichment, and retain periodic retry coverage;
- the default ten-minute reconciliation repairs a deliberately dropped
  notification, while explicitly disabling it reports the lost guarantee;
- repeated full generations reuse cached embeddings, embed only unseen
  content when requested, and preserve semantic artifacts and run history;
- no path through plain watch invokes pi-ai, the checker sidecar, embedding, or
  other optional spending without its explicit flag.

### Out of scope for G12

- publish-then-swap or uninterrupted query availability during refresh;
- a persistent daemon/service manager or background watch installation;
- a durable watcher event journal or cross-snapshot checker-input manifest;
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

## Parked G15 — design-before-edit task memory

**Decision amendment (2026-08-17):** do not ship or evaluate G15 as a jscout
product surface now. PR #45 is blocked. The next experiment implements the
intervention only in the replay harness: one read-only design call followed by
a separate implementation call that receives the captured design in its
prompt. No design artifact, command, MCP tool, semantic-plane write, or agent
guide change is added to jscout. The design below is retained only as the
bounded proposal to reconsider if the harness experiment establishes value
that cannot be achieved through orchestration alone.

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

## Conditional G16 — adaptive memory delivery

G14 deliberately changed search-attached memory from broad similarity previews
to evidence-connected selection: direct support first, then bounded graph
proximity, then relations to an already connected artifact. This prevents a
high-scoring generic card from displacing code evidence. It also creates an
intentional hard boundary: a semantically relevant artifact without a current
code-evidence connection is not attached, and the response redirects the agent
to `semantic_memory`.

G16 is a decision gate for that boundary, not the next automatic milestone.
It is considered only if G15 is unparked and after the full product evaluation.
The evaluation must retain, per search, the bounded semantic candidate IDs and ranks, why each
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
pull of the original deferral trigger. Unambiguous
answers are recorded at `likely` with `checker` provenance; ambiguous answers
remain candidates. Everything else typed navigation offers remains the LSP's
job.

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
