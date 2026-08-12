# jscout architecture and implementation plan

> Status: authoritative plan as of 2026-08-12.
>
> G1–G9 are implemented. Semantic v1 has reached its implementation boundary;
> product-value testing remains paused until the engineering verification gate
> below is green.

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
| Storage | One versioned SQLite database; schema v16; FTS5, provenance-keyed embedding caches, dimension-specific sqlite-vec `vec0` indexes, canonical extraction tables, graph projection, semantic artifacts, run ledger, and freshness metadata |
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

- Rust compile, formatting, lint, unit, migration, and existing regression
  tests;
- fake-provider gateway protocol/config/auth tests;
- schema rejection, cancellation, timeout, child-crash, snapshot-race, and
  no-partial-write tests;
- deterministic evidence-pack and freshness-transition fixtures;
- no paid or plan-backed model calls in the default test suite.

After semantic v1, run real Sol or Terra scouting on the installed n8n and
Twenty repositories, inspect generated memory, repair implementation defects,
and only then compare real agent work with and without it.

## Post-v1: checker enrichment sidecar (G10)

The original plan deferred checker-backed enrichment behind a revisit
trigger. That trigger is now pulled deliberately: the call-site query work
showed that receiver identity (`dbs.wave.card` → which table class) is the
recurring gap between candidate-set answers and behavioral ones, and the
owner has accepted the cost. This section replaces the deferral; it does not
extend the semantic-v1 completion boundary, and nothing in v1 depends on it.

### Shape

A companion Node sidecar hosting the TypeScript checker (LanguageService or
tsserver — an implementation decision, not a plan commitment) behind the
same versioned JSONL-over-stdio pattern as the pi-ai gateway: `hello`,
capabilities, query, cancellation, shutdown, request IDs, stable error
codes. The sidecar answers exactly one bounded question in its first version:
resolve a statically named member at one indexed call occurrence. The request
contains the repository-relative file, indexed file hash, exact call,
receiver-expression, and property spans. The response contains the receiver's
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

Diagnostics are never enumerated, used as a gate, or surfaced. A broken or
non-compiling project still attempts the requested member query rather than
turning enrichment into a compile check. When the answer is an error type or
`any`-degraded, the sidecar reports `unknown` and jscout records nothing —
fail-closed, no guessed edge.

The sidecar prefers the repository's own `typescript` installation so
answers match the project's language version; a bundled fallback is
permitted but its version is always recorded in provenance. Absent Node,
absent TypeScript, or an unhealthy sidecar leaves every existing surface
working exactly as today.

Checker program construction and queries may block the Node event loop. The
protocol host must keep cancellation responsive by isolating checker work in a
terminable worker or child process. Rust also enforces a hard per-request
deadline and terminates an unresponsive sidecar; writing a cancel message alone
is not considered cancellation support.

### Consumption

Enrichment is an explicit pass (`jscout enrich`), never part of `index` or
`watch`; deterministic indexing remains Node-free. The pass takes indexed
member-call occurrences whose receivers currently reach property-hub
candidates, asks the sidecar to resolve the called property on that receiver,
and maps returned declaration sites to indexed symbol anchors.

Enrichment is occurrence-specific. A single `dbs.wave.card.insert()` result may
add an edge from that call's enclosing file/symbol to `CardTable.insert`; it
must never promote or replace the shared `member:insert` hub edge, which would
leak the answer into unrelated `.insert()` calls. One unambiguous mapped target
is `likely` with provenance `checker`. Multiple valid declaration targets —
from unions, overload ownership, inheritance, or disagreeing projects — remain
separate `possible` candidates. Existing hubs are retained for unexplained
dynamic calls. Contract-plane consumers may attach the receiver's declared type
as documentary evidence under the same provenance.

Checker results are canonical typed facts, not writes made directly to the
disposable graph projection. A dedicated enrichment table records at least the
caller/file identity and hash, exact occurrence spans, project ID, resolved
target anchor and fingerprint, confidence/provenance, and checker-input
fingerprint. Projection rebuilds include only fresh facts and recreate their
targeted edges; they neither erase the canonical facts nor make stale facts
traversable.

The checker-input fingerprint covers the exact TypeScript package/version,
normalized config inheritance, effective compiler options, project selection,
and identities/hashes of the config, source, and declaration inputs loaded by
the checker. A lockfile hash may contribute but is not a freshness certificate
for ambient/generated declarations or effective compiler settings. The pass is
planned against one structural snapshot and publishes a completed batch in one
transaction after rechecking that snapshot, occurrence source hashes, target
anchors, and checker inputs; drift publishes nothing from the raced batch.

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

Diagnostics, rename/refactor safety, call hierarchy, emit, watch-mode
daemons, and any checker influence over deterministic structural facts.
Agents wanting full typed navigation should use an LSP; G10 only closes
the receiver-identity gap inside jscout's own evidence model.

## Evaluation decisions already made

The dated evidence remains under `eval/`; this section records only the design
consequences that still govern implementation.

| Finding | Current consequence |
|---|---|
| Unassisted Codex sessions made zero jscout calls; MCP metadata alone did not create adoption | Ship explicit project-local agent guidance; do not generalize the adoption result to every client/model |
| Grep, baseline, and structural arms reached the same correctness ceiling while structural retrieval initially read more irrelevant files | L1 retrieval investment is closed; expansion stays opt-in and file-role/origin policy applies before budgets |
| Whole search responses grew materially when structural context was attached | Complete rendered-response byte budgets are a permanent contract |
| The preregistered file-role revision reduced structural irrelevant inspection to an interval including zero without creating a correctness win | Keep deterministic role classification/filtering; do not claim graph value from the result |
| Full versus elided source retained answer quality but did not reduce selected-artifact bytes/calls | Full source remains the default; custom behavioral IR is not earned |
| Fixed-snapshot workflow memory replay delivered artifacts in every correct warm token win and reduced median session-2 tokens | Keep evidence-backed workflow memory opt-in and proceed with the shared semantic engine |
| Free-form workflow participant synthesis omitted deterministic continuations | Candidate closure and exhaustive classification are mandatory |
| Standalone `neighborhood` had no natural selection while expanded search used the same machinery | Treat neighborhood as drill-down plumbing; prioritize agent-reached surfaces |

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

Do not reimplement checker machinery. Optional occurrence-scoped
receiver/member enrichment through a checker sidecar is planned as G10
(post-v1) — a deliberate pull of the original deferral trigger. Unambiguous
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
