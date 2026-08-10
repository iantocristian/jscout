# jscout persistent repository memory — structural retrieval and scouting

> Independent Codex revision, 2026-08-07. This file does not replace
> [PLAN-KG.md](PLAN-KG.md). It presents an alternative architecture derived
> from the implementation review, the design discussion, and research on
> repository-level context compression and summarization.

## Objective

`jscout` is persistent, verifiable repository memory for coding agents. It is
a complementary tool, not a replacement for agent reasoning. It should let
knowledge survive across sessions while making the repository easier for an
agent to observe by providing:

- exact structural facts;
- compact, reversible overviews;
- inferred domain meaning and agent-reported findings with evidence;
- explicit confidence and freshness;
- fast drill-down to source.

The central contract is:

> Given an agent's current query or focus, return the smallest trustworthy
> slice of repository evidence that improves its next action.

This is broader than a “knowledge graph” and narrower than autonomous software
comprehension. The graph is the structural substrate. **Scouting** compiles it
into a multi-resolution field guide. LLM enrichment adds meanings the parser
cannot infer, such as capabilities, workflows, and domain concepts. Validated
agent write-back turns discoveries made during development work into the same
fingerprinted semantic memory.

## Inherited decisions from revision 2 and its review

This revision changes storage and sequencing details, but it inherits the
following decisions and rationale from [PLAN-KG.md](PLAN-KG.md), especially its
“Research notes (inputs, with corrections from review).” They are recorded
here so the reasons do not disappear behind the revised specification.

| Decision retained | Why / evidence | Revisit trigger |
|---|---|---|
| Project search chunks onto symbol/file anchors; do not make chunks ontology nodes | Chunk boundaries follow retrieval budgets and churn independently of repository identity | Only if a concrete query needs chunk identity independent of its source anchors |
| Rebuild the resolved traversal projection after indexing | A barrel edit can reroute unchanged importers; full rebuild is simpler and currently below the latency budget | A measured repository exceeds the 100 ms projection target |
| Model uncertain events through receiver-qualified or unknown hubs | Pairing emitters and listeners by event name creates false relationships and quadratic fan-out | Direct edges become available when receiver identity resolves |
| Separate canonical entities from source occurrences | Identity, evidence, confidence, and file-lifecycle behavior have different update semantics | No planned revisit |
| Keep graph expansion off by default | GraphRAG-Bench reports that graph retrieval can underperform vanilla retrieval on simple lookups; expected value is concentrated in structural and multi-hop questions | The code-specific agent-utility evaluation shows expansion improves the default workload |
| Treat expansion weights as an unvalidated heuristic | LARGER reports 55.7 Acc@5 for the full system, 48.2 without graph expansion (7.5 points absolute; 13.5% relative), and 53.1 without confidence (2.6 points; 4.7% relative). Its published confidence values are fixed and provenance-specific, not learned support for this plan's weights | Tune or replace the heuristic using the structural suite |
| Treat focus ranking as Aider-inspired, not an Aider reproduction | Aider's identifier multipliers affect graph edge weights, while PageRank personalization is file-based | Replace when a simpler measured ranking performs better |

The product decision inherited from revision 2 is equally important: this is
memory, not merely a batch-generated repository description. Structural truth
comes from the index; semantic memory can come from scouts or agents, but every
claim must remain attributable, evidence-backed, confidence-limited, and
freshness-aware.

## Implementation status — 2026-08-07

The first RI-1 slice is implemented:

- schema v3 adds declaration spans, scope chains, reference identities, and
  source offsets;
- indexing fully rebuilds disposable `graph_nodes` and `resolved_edges` after
  module resolution and publishes a BLAKE3 repository snapshot;
- file, symbol, package, and unknown-receiver event-hub identities exist;
- ambiguous root references fan out to visible `possible` candidate edges
  instead of disappearing;
- tier-3 member calls project through unknown-receiver property hubs, keeping
  storage linear while making name-matched candidates traversable at
  `possible` confidence; unmatched sites remain canonical but do not create
  dead-end traversal nodes;
- CLI and MCP expose bounded `neighborhood` traversal with direction,
  confidence, edge-kind, node, and edge limits;
- saved anchors can carry a snapshot; stale symbol anchors are re-resolved by
  path/scope/name, and ambiguity is an error with candidates;
- structural fixtures cover call resolution, same-named methods, barrel
  rerouting, stale anchors, and event hubs;
- search hits project chunks onto snapshot-scoped symbol/file anchors, and
  opt-in expansion returns a separately labelled context pack under one global
  seed/node/edge/byte budget without changing retrieval scores;
- multi-statement search and neighborhood reads are pinned to one SQLite
  snapshot so the fingerprint, anchors, nodes, and edges cannot straddle an
  indexing commit;
- baseline and structural MCP profiles plus a vendor-neutral JSONL task/grader
  protocol make controlled agent A/B runs reproducible;
- opt-in privacy-minimal MCP telemetry records tool selection, latency,
  success, result size, task, profile, session, and snapshot without recording
  arguments or results;
- the P0 Codex runner freezes the repository, isolates agent configuration,
  counterbalances profiles, captures structured answers, and joins telemetry;
- the representative `ai-pipe` P0 run is complete; see
  [eval/results/ai-pipe-p0-2026-08-07.md](eval/results/ai-pipe-p0-2026-08-07.md);
- the contamination-probed, post-cutoff n8n/Twenty run is complete; see
  [eval/results/n8n-twenty-post-cutoff-2026-08-09.md](eval/results/n8n-twenty-post-cutoff-2026-08-09.md);
- `jscout agent-guide --install <root>` ships the explicit project-local agent
  integration contract that MCP metadata alone did not deliver in Codex.

P0 and the large-repository follow-up are complete as bounded direction gates.
The first assisted comparison tied on correctness, while the later three-seed
post-cutoff run scored grep 24/24 and both indexed profiles 23/24 after blind
adjudication. Structural retrieval inspected 6.38 more irrelevant files than
grep per paired run, with a task-clustered 95% interval of +1.00 to +12.38.
Token and wall-time intervals crossed zero. Neither run had standalone
`neighborhood` adoption; expanded search is the graph delivery vehicle. The
unassisted pass had zero jscout uptake. Therefore expansion remains opt-in,
explicit agent integration is part of the product contract, production-path
filtering precedes further expansion work, and no general localization gain is
claimed. Whole-response budgeting is complete; broader per-tool schema
coverage remains incremental.

The current release-build measurement on the frozen 690-file `ai-pipe` corpus
is 182 ms for a full traversal-projection rebuild: 103 ms references, 31 ms
candidate-bearing member calls, 6 ms modules, and under 1 ms events. This does
not meet RI-1's 100 ms projection target. Member-call projection is bounded to
properties with at least one indexed symbol candidate; all unmatched sites
remain in the canonical `member_calls` table.

## Architectural conclusion from the research

There is no query-independent compression of arbitrary code that preserves all
semantics needed by every future development task. Logging is noise for one
architecture question and the answer to an observability bug. A retry loop is
boilerplate until the task concerns failure recovery.

Therefore:

1. Raw code is never replaced.
2. Compression is hierarchical and reversible.
3. Exact identifiers and program topology survive every level.
4. More implementation is retained near the current query/focus.
5. Semantic artifacts—whether scout- or agent-authored—are derived claims, not
   source truth.

The research supports this shape:

- Hierarchical Context Pruning reports that preserving repository topology and
  function-level structure while pruning many dependent implementations can
  retain or improve repository-level completion performance.
- ProConSuL reports that call-graph/callee context improves function summaries
  and reduces hallucination compared with isolated summarization.
- Higher-level code summarization experiments find full code strongest for
  individual files, reduced code a cost-efficient alternative, and hierarchical
  aggregation strongest for module summaries.
- RepoSummary finds feature-oriented grouping more useful and traceable than
  directory-only repository summaries.
- CodePromptZip and newer issue-resolution compressors show that generic text
  compression is the wrong abstraction: code roles, structure, and the current
  task determine what is safe to remove.

The implementation below turns those observations into a local, incremental
architecture rather than a one-shot prompt compressor.

---

## Four representation layers

| Layer | Contents | Generated by | Freshness |
|---|---|---|---|
| **R0 — source** | Repository files and exact stored chunk content | User/repository | Source of truth |
| **R1 — structural graph** | Files, symbols, resolved references, modules, events, entities | Parser/resolver/extractors | Rebuilt with index |
| **R2 — scout views** | Contracts, signatures, query-focused elided source, optional behavioral IR, compact maps | AST/source elision + R1 projection | Rendered from the current snapshot; store only representations that win the gate |
| **R3 — semantic memory** | Workflows, agent annotations, and optional symbol/file/module summaries | Scouts or agents using R1+R2 evidence | Fingerprinted; fresh/stale/degraded |

R0–R2 are deterministic. R3 contains semantic assertions rather than
structural facts. The distinction must be visible in storage and in every
response.

## Identity model

### Canonical graph anchors

- `file:<repo-relative-path>`
- `pkg:<package-name>`
- `sym:<path>#<scope>::<name>@<ordinal>`
- `contract:<path>#<scope>::<name>@<ordinal>`
- `entity:<type>:<normalized-name>`
- `event:<receiver-or-unknown>:<name>`

The ordinal is file order among declarations with the same path, scope, and
name. These keys are **snapshot-deterministic**, not stable across arbitrary
edits, renames, or moves. Public APIs must describe them as snapshot-scoped.

The current `symbols.start/end` values describe binding spans, which are not
enough for containment or reference ownership. RI-1 extends symbol extraction
with `decl_start`, `decl_end`, and `scope_chain`. A reference belongs to the
smallest declaration span containing its source offset; if no declaration owns
it, the source anchor is the file. This same declaration-span model powers
chunk projection and prevents calls from merged module chunks being attributed
to every symbol in the chunk.

### Chunks are not graph nodes

Chunks are retrieval artifacts whose boundaries depend on token budgets. A
search hit projects to graph anchors:

1. select declarations whose spans overlap the hit;
2. prefer the chunk's primary named declaration;
3. use all overlapping declarations for merged module chunks;
4. fall back to the file node when the chunk contains imports or module-level
   behavior without a declaration anchor.

The response retains the originating chunk/hit so an agent can request exact
source even though traversal uses symbol/file identity.

---

## RI-1 — trustworthy structural graph and traversal

### Source tables versus traversal projection

The existing typed tables remain canonical because they retain extractor-
specific provenance. Add a derived traversal projection rather than replacing
them with one generic source-of-truth table.

```sql
-- Existing refs need an explicit id for materialized provenance.
refs(id INTEGER PRIMARY KEY, ...);

graph_nodes(
  node_key TEXT PRIMARY KEY,
  node_kind TEXT NOT NULL,
  native_table TEXT,
  native_id INTEGER,
  display_name TEXT NOT NULL,
  file_id INTEGER,
  line INTEGER,
  meta_json TEXT NOT NULL DEFAULT '{}'
);

resolved_edges(
  id INTEGER PRIMARY KEY,
  src_key TEXT NOT NULL,
  dst_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL,
  provenance TEXT NOT NULL,       -- semantic | resolver | heuristic | synthetic
  source_file_id INTEGER,
  source_ref_id INTEGER,
  line INTEGER,
  detail_json TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX resolved_edges_src ON resolved_edges(src_key, confidence, kind);
CREATE INDEX resolved_edges_dst ON resolved_edges(dst_key, confidence, kind);
```

`graph_nodes` and `resolved_edges` are disposable read models. Schema migrations
or indexing can rebuild them from files/symbols/refs/module/entity tables.

### Resolution refresh

After changed files are extracted and `module_edges` are rebuilt:

1. construct the export/module resolver snapshot;
2. rebuild all `graph_nodes` and `resolved_edges` in one transaction;
3. atomically expose the new snapshot;
4. update the index fingerprint only after the rebuild commits.

A full rebuild is the initial policy. Barrel edits can reroute unchanged
importers, and the current graph is small enough that correctness is cheaper
than transitive invalidation logic. Introduce selective invalidation only after
a measured repository violates the latency gate.

### Events

Never generate every emit→listener pair sharing a string. Use a hub:

- emit site → `event:unknown:<name>`;
- event hub → listener site;
- both edges are `possible` when receiver identity is unknown;
- if both receivers resolve to the same object/bus symbol, create a direct
  `likely` relationship through a receiver-qualified event key;
- mark high-fanout hubs `generic` and suppress them from default expansion.

This keeps storage linear in event sites and prevents unrelated `error`,
`data`, `close`, or signal handlers from looking connected.

### Traversal tools

#### `neighborhood`

Inputs: anchor spec, direction, depth, global node/token budget,
`min_confidence`, and edge-kind filters.

Traversal now uses this deliberately simple best-first ranking hypothesis:

```text
path score = minimum edge confidence on path
           × relation weight
           × distance decay
           × hub damping
```

The implementation maps confidence to `1.0 / 0.6 / 0.3`, applies explicit
relation weights, decays each additional hop by `0.75`, and damps hubs by
`1/log2(degree+2)`. These weights are unvalidated product hypotheses, not
learned relevance. Search expansion merges candidates from all seeds by this
score before enforcing global budgets. Do not spend standalone
`neighborhood` UX effort until discriminating evaluations show a need;
expanded search remains the current agent-facing delivery vehicle.

The budget is global across all anchors, not per hit. Nodes and edges are
deduplicated before rendering. Confidence and provenance remain visible.

#### `search --expand`

Search ranking remains BM25/vector/RRF/reranker. Expansion runs after ranking
and returns a separately labelled context pack for the top hits. It does not
silently alter retrieval scores. Initial default: off, with a clear MCP
description explaining that it helps structural/multi-hop questions.

#### `paths`

Bounded shortest paths over selected structural edges, with direction and a
maximum depth. Results include every edge's confidence and evidence location.

#### `repo_overview`

Use a **file-level projection**, not PageRank over the full heterogeneous
graph. Aggregate confident cross-file symbol edges into weighted file edges,
personalize on focused paths/symbols, rank files, then select important symbols
within each file. Event/entity hubs and possible member-call candidates should
not dominate repository importance.

### RI-1 definition of done

- Collision fixtures for same-named methods and duplicate declarations.
- Barrel/re-export fixture proving a changed barrel reroutes unchanged refs.
- Event fixtures proving unrelated receivers do not pair.
- Ambiguous-reference fixtures proving candidates survive as `possible`.
- Member-call fixtures proving unknown-receiver candidates are traversable
  without materializing a call-site × symbol cross-product.
- Full graph projection rebuild under 100 ms on the current `ai-pipe` corpus.
- `neighborhood` under 50 ms at depth two on that corpus.
- Global output-budget tests and deterministic ordering.
- CLI and MCP schemas versioned and covered end to end.

---

## SC-1 — deterministic scouting compression

Scouting begins after RI-1 because the graph identifies exact relationships
and evidence anchors. The first comparison is deliberately thin: full source
versus deterministic elided source at equal context budgets. A custom
behavioral IR is not the default merely because it is more compressed.

### Default: query-focused elided source

Render source with imports, signatures, calls, guards, loops, returns, throws,
and query-relevant bodies retained. Collapse comments, formatting, local
plumbing, and distant implementations behind explicit span-linked elision
markers. The renderer reparses selected files on demand initially. Caching is
earned only when profiling shows parsing/rendering latency matters.

This representation preserves the source language and exact surviving text,
which reduces the new-parser burden imposed on the consuming model. Compare it
against full source on the curated structural task set before designing a new
IR.

### Optional: behavioral IR behind an A/B gate

The richer skeleton below is an experiment. Persist it only if, at equal token
budgets, it improves agent outcomes over elided source and the gain justifies
another representation, renderer, migration path, and preservation suite.

```sql
scout_units(                         -- created only if behavioral IR wins
  id INTEGER PRIMARY KEY,
  anchor_key TEXT NOT NULL,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  unit_kind TEXT NOT NULL,           -- signature | behavior | contract
  source_hash TEXT NOT NULL,
  ir_version INTEGER NOT NULL,
  signature TEXT,
  ir_json TEXT NOT NULL,
  rendered_text TEXT NOT NULL,
  UNIQUE(anchor_key, unit_kind, ir_version)
);
```

If the gate passes, JSON is the durable deterministic form and `rendered_text`
is a replaceable cache. If it does not, no `scout_units` table is needed.

### Runtime skeleton

Lower the AST into a compact, language-like IR that retains:

- exact names, scope, exported status, decorators, and signature;
- `async`, `await`, generators, callbacks, and transaction/error boundaries;
- guards and significant branch predicates;
- loops plus important calls/side effects inside them;
- calls, construction, renders, inheritance, events, and resolved targets;
- writes/reads of deterministic entities when available;
- returns, throws, error types, and externally meaningful literals;
- first-line documentation and exact source span.

Collapse local expression plumbing, repeated assignments, formatting, and
unselected pure calculations behind explicit elision markers. Do not ask an
LLM to produce this pseudocode: free-form paraphrase can silently rename calls,
erase failure paths, or invent sequencing.

Example:

```text
async checkout(cart: Cart, user: User) -> Promise<Order>
  guard cart.items.length > 0 else throw EmptyCartError
  transaction:
    call inventory.reserve(cart.items)
    write table:orders
  try call payments.authorize(order.total)
  catch:
    call orders.markPaymentFailed(order.id)
    rethrow
  emit event:order.created
  return order
```

Every call/entity/event in the rendering is linked to an R1 anchor and can be
expanded back to source.

### Contract plane

The runtime graph may erase type-space for resolution, but scouting must retain
human/agent-facing contracts:

- exported interfaces and type aliases;
- function parameter and return types;
- class/public method signatures;
- enums and discriminated unions;
- validation schemas and decorators;
- referenced type names and import text;
- `.d.ts` declarations when they describe repository-owned public APIs.

These become `contract:` anchors or documentary fields, not runtime-call
targets. Checker-less relationships are labelled documentary/textual rather
than certain runtime facts.

### Multi-resolution context assembly

Given a focus and token budget, assemble context by distance:

| Region | Representation |
|---|---|
| Direct search hits / edit targets | Exact source or full chunk |
| One-hop structural neighbors | Elided source; behavioral IR only if it wins; workflow role when fresh |
| Two-hop neighbors | Signature, one-line purpose, typed edges |
| Remaining important repository regions | Deterministic file/module overview; semantic summary only when available and fresh |

This is adaptive compression without destroying information. An agent can
request the next lower level for any anchor.

### SC-1 definition of done

- Full-source versus elided-source agent A/B at equal rendered budgets.
- Golden fixtures for guards, loops, try/catch/finally, async calls, events,
  JSX, transactions, returns/throws, and public TypeScript contracts.
- Preservation checks: every exported identifier and every certain outgoing
  edge appears in retained source/signatures.
- If behavioral IR is tested, it has a separate preservation suite and ships
  only after outperforming elided source on the pre-registered task set.
- Compression ratio reported per corpus, without making ratio the quality goal.
- `repo_overview --tokens N` stays within its actual rendered budget.

---

## SC-2 — LLM semantic scouting

LLMs add semantic information that syntax and embeddings do not make explicit.
The first useful product is not a free-standing ontology; it is structured,
evidence-backed annotations over R1/R2 anchors.

### Step 1: bounded workflow experiment

Generate candidate subgraphs from entry points, calls, events, routes, tables,
and graph communities. Start with dozens of high-value seeds, not one LLM call
per symbol. Ask the LLM to name the feature/workflow and assign a role to each
participant.

```json
{
  "type": "workflow",
  "name": "checkout",
  "participants": [
    {"anchor": "...checkout...", "role": "orchestrator", "scope": "defining"},
    {"anchor": "...reserveInventory...", "role": "inventory consistency", "scope": "defining"},
    {"anchor": "...retryAuthorization...", "role": "retry helper", "scope": "supporting"}
  ]
}
```

Workflows are first because they answer “which workflows does this code
participate in?” and require cross-file semantics that structural search does
not already provide. Each participant and role needs validated evidence.
`defining` participants form the minimal stable cross-file skeleton;
`supporting` participants retain useful internal detail without presenting a
helper as an equal workflow boundary. Every distinct stable cross-file stage
still gets its own participant anchor; scope prevents flattening without
discarding localization evidence.

### Step 2: validated agent write-back

Add an `annotate` MCP tool over `semantic_artifacts` and
`semantic_supports` in the same phase as the workflow experiment. An agent that
has proved a repository-level fact while doing development work should be able
to preserve it for later sessions.

Write-back follows the same trust contract as scout output:

- every annotation supplies one or more current R1/R2 support anchors and
  exact evidence spans;
- the server validates anchors, spans, source hashes, and the current snapshot
  before committing;
- `model = 'agent-reported'` and `prompt_version = 'annotate/v2'`;
  reporter/session details may be stored inside `body_json` without being
  treated as evidence;
- confidence is `likely` or `possible`, never `certain`;
- changed evidence produces the same stale/degraded states as scout output;
- a write creates a new attributable artifact rather than silently
  overwriting or merging another scout's or agent's assertion; corrections
  point to the prior artifact through `supersedes_artifact_id`;
- competing claims may coexist and remain distinguishable by provenance;
- `body_json` must pass the artifact-type schema and is rendered as quoted
  repository data, never as instructions to a consuming agent;
- write-back can only create semantic artifacts and supports. It cannot write
  `graph_nodes`, `resolved_edges`, entities, or any other structural fact.

### Step 3: optional symbol cards

For each selected symbol, provide the LLM:

- exact implementation for that symbol;
- query-focused elided source and contract evidence;
- signatures for direct callers and callees;
- a deterministic runtime skeleton only if the SC-1 IR experiment won;
- adjacent events/entities;
- repository-level vocabulary accumulated so far.

Require structured output:

```json
{
  "purpose": "Coordinates checkout and payment initiation",
  "architectural_role": "workflow orchestrator",
  "domain_terms": ["checkout", "inventory reservation", "payment"],
  "side_effects": ["creates order", "reserves inventory"],
  "invariants": ["inventory is reserved before payment authorization"],
  "failure_modes": ["empty cart", "payment authorization failure"]
}
```

The LLM should infer intent, role, invariants, and domain language. It should
not spend tokens restating calls or signatures that R2 already knows.

Concept-to-concept edges are deferred until the vocabulary is stable. Embedding
clusters, if implemented, are called `themes`; they are candidate regions, not
domain concepts.

Cards now share the optional-summary gate. They ship only if a pre-registered
query set shows value beyond search, elided source, and workflow artifacts.

### Step 4: optional hierarchical summaries

- File summaries aggregate available workflow/card evidence and file topology.
- Module/package summaries aggregate file summaries, not raw repository code.
- Summaries retain lists of supporting anchors; prose without traceability is
  not indexable memory.

Nothing downstream depends on these summaries. They enter a separate SC-2c
gate and ship only if curated questions or agent-utility evaluation show that
they reduce source reads, tool calls, or tokens beyond the deterministic
overview, symbol cards, and workflows. Otherwise they remain an experiment,
not default memory.

### Semantic storage and freshness

```sql
semantic_artifacts(
  id INTEGER PRIMARY KEY,
  supersedes_artifact_id INTEGER REFERENCES semantic_artifacts(id),
  artifact_type TEXT NOT NULL,     -- symbol_card | workflow | annotation | concept | file_summary | module_summary
  canonical_name TEXT,
  body_json TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  source_snapshot TEXT NOT NULL,
  created_at TEXT NOT NULL
);

semantic_supports(
  artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id) ON DELETE CASCADE,
  claim_path TEXT NOT NULL,       -- JSON pointer to the supported field/claim
  anchor_key TEXT NOT NULL,
  role TEXT,
  evidence_file TEXT NOT NULL,
  evidence_start_line INTEGER NOT NULL CHECK(evidence_start_line > 0),
  evidence_end_line INTEGER NOT NULL CHECK(evidence_end_line >= evidence_start_line),
  source_hash TEXT NOT NULL,
  context_hash TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible'))
);
```

Repository fingerprints use BLAKE3 over sorted `(path, file_hash)` records plus
schema/IR versions. Do not use XOR: duplicate hashes can cancel and paths are
lost.

Short-term freshness contract:

- a semantic run records its source snapshot;
- changed snapshot marks affected artifacts stale;
- stale artifacts may be returned only with an explicit label;
- `scout --rebuild` performs a full refresh.

Long-term incremental contract:

- `source_hash` change → source-stale;
- direct structural-neighborhood fingerprint change → context-stale;
- some stale participants → workflow `degraded`;
- no fresh support → artifact eligible for garbage collection;
- file/module summaries depend on child artifact fingerprints.

No scout- or agent-authored assertion is `certain`. `likely` means supported by
identifiable code; `possible` means useful interpretation with incomplete
evidence.

### SC-2 definition of done

- `scout` can generate workflows for a bounded entry-point/retrieval-selected
  subset before attempting broad repository coverage.
- Every workflow field and optional card field has at least one valid support
  anchor.
- Changed evidence produces a visible stale/degraded state.
- Semantic search can retrieve a workflow and traverse to exact supporting
  code.
- `annotate` round-trips an agent claim with validated evidence and visibly
  marks it stale/degraded after relevant code changes.
- A curated set of repository questions compares base search, R2 scouting, and
  R2+R3 semantic memory.
- Symbol cards and file/module summaries are not prerequisites for SC-2
  completion; SC-2c owns their separate value gate.

---

## EN-1 — deterministic non-code entities

Entities improve both direct blast-radius questions and semantic scouting.
Keep canonical identity separate from occurrences/provenance.

```sql
entities(
  id INTEGER PRIMARY KEY,
  entity_key TEXT UNIQUE NOT NULL,
  type TEXT NOT NULL,
  name TEXT NOT NULL,
  meta_json TEXT NOT NULL DEFAULT '{}'
);

entity_occurrences(
  id INTEGER PRIMARY KEY,
  entity_id INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  line INTEGER NOT NULL,
  extractor TEXT NOT NULL,
  confidence TEXT NOT NULL,
  detail_json TEXT NOT NULL DEFAULT '{}'
);

entity_edges(
  occurrence_id INTEGER NOT NULL REFERENCES entity_occurrences(id) ON DELETE CASCADE,
  target_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL
);
```

Initial extractors:

1. Routes: Express/Fastify plus Next.js file conventions.
2. Environment variables: `process.env` and `import.meta.env`.
3. Database resources: SQL literals and common ORM/client patterns.
4. External services: literal URL hosts in network clients.
5. Events: migrate current sites into receiver-qualified or unknown hubs.

Packages remain `pkg:` nodes. A package dependency is not automatically an
external service.

Occurrences cascade with changed/deleted files. Canonical entities with no
occurrences are garbage-collected after indexing.

---

## Agent-facing surface

Keep the tool surface small and composable:

- `semantic_search(query, limit, expand, context_level, token_budget)`
- `neighborhood(anchor, depth, direction, kinds, min_confidence, token_budget)`
- `repo_overview(focus, token_budget, include_semantics)`
- `paths(from, to, kinds, max_depth)`
- `entities(type, query)`
- `annotate(type, name, body, supports, confidence, supersedes?)`
- existing `definition`, `who_uses`, `file_outline`, and `events`

Every result includes:

- snapshot/index fingerprint;
- anchor keys and source locations;
- deterministic, batch-scout, or agent-reported provenance;
- confidence;
- freshness for semantic artifacts;
- explicit handles for requesting more detail.

`annotate` uses the same semantic-artifact/support validation and freshness
machinery as scout output. Agent reports are memory claims, never structural
truth; callers can inspect their evidence and distinguish them from parser,
resolver, heuristic, and batch-scout provenance.

---

## Positioning versus an LSP

jscout does not compete with `tsserver` on checker-backed operations. An LSP
should remain the first choice for precise typed definition/call hierarchy,
rename/refactor safety, diagnostics, and interface-to-implementation navigation
inside a configured TypeScript project.

jscout's scope is different:

- one fast runtime-oriented view across JavaScript and TypeScript without
  requiring a healthy checker configuration;
- explicit uncertainty when member or event receiver identity is unresolved;
- repository entities and multi-file workflows beyond language-server symbol
  operations;
- bounded, snapshot-labelled retrieval designed for agent context;
- persistent, evidence-backed scout and agent memory across sessions.

Do not reimplement checker machinery speculatively. An optional tsserver
enrichment pass for interface-to-implementation edges is deferred until agent
evaluation shows that missing typed edges are a material failure mode and the
latency/operational cost is justified.

---

## Evaluation

### Structural retrieval

- Fixture precision by confidence tier.
- Curated one- and two-hop questions where expansion should surface a gold
  neighbor that lexical search alone does not return.
- Latency and global output-budget gates.

### Base retrieval

- JSDoc holdout Recall@5/@10 for BM25, embeddings, and reranker.
- This does not evaluate expansion because expansion occurs after ranking.

### Scouting preservation

- Identifier, signature, certain-edge, exception, and entity preservation.
- Compression ratio alongside preservation—not as a substitute for it.
- Exact drill-down validation for every rendered anchor.

### Semantic memory

- Curated questions such as “which workflows does this code participate in?”
  with gold supporting symbols.
- Evidence-support checks for every scout- or agent-authored claim.
- Freshness tests after source, edge, and participant changes.

### Agent utility

On repeated tasks over the same repositories, compare agents with and without
each representation layer:

- searches/tool calls to first relevant file;
- irrelevant files read;
- tokens to a correct implementation plan or edit;
- affected callers/tests/routes/tables discovered;
- task completion and regression rate;
- whether a later session benefits from earlier scouting or agent write-back.

This is the product metric. Retrieval scores are component diagnostics.

The next repository-outcome run has three arms: grep-only, jscout baseline,
and jscout structural. Its task set must create accuracy or cost headroom with
deep barrel indirection, misleading same-name candidates, receiver-ambiguous
events, dynamic registries, and cross-file workflow paths. Repeating the
current 8/8 task set with more seeds measures variance but not discrimination.

---

## Sequencing

| Phase | Deliverable | Dependency | Rough scope |
|---|---|---|---|
| **P0** | Complete: frozen `ai-pipe` task set, isolated Codex runner, paired naturalistic/assisted observations, telemetry join, and recorded decision | Current core | Done 2026-08-07 |
| **RI-1** | Complete: whole-search and outline response budgets, identity, materialized graph, candidate projection, ranked neighborhood, stale anchors, chunk projection, opt-in expansion, and core fixtures | P0 | Done 2026-08-07; broader per-tool envelope coverage remains incremental |
| **SC-1** | First full-source vs deterministic-elided A/B complete: equal correctness, no selected-artifact compression, worse observed calls/bytes; full remains default and custom IR is not earned | RI-1 | Current renderer rejected 2026-08-07; iterate only behind another gate |
| **SC-2a** | Complete: bounded workflow memory, semantic storage/freshness, validated `annotate` write-back, and fixed-snapshot response-budget replay | SC-1 | Passed registered replay 2026-08-09; remains opt-in |
| **RB-1** | Runtime-boundary entities: registry dispatch, data lifecycle, jobs/queues/crons, and DI providers | RI-1 | First deterministic slice implemented 2026-08-10; known Recall/Slack candidate misses are now explicit two-hop paths |
| **EN-1** | Routes, env, tables, services, event migration | RI-1; enriches SC-1/2 | 1–2 days |
| **SC-2b** | Candidate-closed scouting: deterministic bounded graph candidates, exhaustive LLM defining/supporting/excluded classification, and snapshot-bound validation before expanding coverage | SC-2a; EN-1 improves workflow seeds | Free-form producer blocked in preflight 2026-08-09 |
| **SC-2c** | Optional symbol-card and file/module-summary experiments with pre-registered query sets | SC-2a | 1 day plus evaluation, if earned |
| **RI-2** | Paths, graph export, ranking tuning, scale work earned by benchmarks | RI-1/SC-1 | Incremental |

The contamination-probed n8n/Twenty suite is complete: 72 Terra/high runs over
eight post-cutoff tasks and three trials. Grep scored 24/24; baseline and
structural each scored 23/24 after blind Sol adjudication. Structural retrieval
read significantly more irrelevant files, mostly tests, fixtures, generated
files, and adjacent framework code. RI-2 expansion breadth and standalone
`neighborhood` UX have not earned priority. Deterministic file roles plus
search filters and pre-budget expansion filtering/penalties passed their
pre-registered gate: structural irrelevant inspection fell to +1.08 versus
grep with an interval including zero, while all arms scored 24/24. L1 retrieval
investment closes here. The bounded SC-2a workflow/write-back experiment
initially landed inconclusive because response budgeting dropped the matching
artifact in 4/18 warm runs. Its single pre-registered fixed-snapshot revision
delivered memory in 18/18 and passed: 36.40% median session-2 token reduction,
17/18 warm correctness versus 14/18 frozen cold, and artifact reads in every
correct token win. This accepts opt-in workflow memory, not default retrieval
or broad scouting. The remaining Recall regression requires defining
participants to be separated from evidence-only helpers before SC-2b expands
coverage.
The first participant-scope preflight then showed that scope labels alone do
not prevent omissions: two direct-write smokes omitted both later-needed
synchronous operations, including after explicit complete-stage guidance. No
registered Twenty run was started. SC-2b must therefore be candidate-closed:
deterministic graph expansion enumerates a bounded set and the LLM must classify
every candidate as defining, supporting, or excluded. Free-form LLM discovery
is not the next implementation step.
Expansion remains opt-in. The failed first SC-1
gate keeps full source as the default and requires a paired-artifact compression
benchmark before another source-view agent run. Broad workflow coverage
benefits from routes/events/tables, but the bounded experiment does not wait for
every EN-1 extractor. Cards and summaries must earn separate implementation
effort.

## Explicitly deferred

- Cross-edit stable symbol identity.
- Type-checker-backed interface-to-implementation resolution.
- Runtime traces.
- Learned compression models and policy optimization.
- Concept-to-concept ontology edges.
- Embedding clusters presented as concepts.
- Agent assertions modifying deterministic structural facts.
- Selective materialized-edge invalidation before full rebuild misses its
  measured latency target.
- tsserver enrichment until missing checker-backed edges are a measured agent
  failure mode.

## Research references

- HCP — Hierarchical Context Pruning:
  https://arxiv.org/abs/2406.18294
- ProConSuL — call-graph-aware code summarization:
  https://aclanthology.org/2024.emnlp-industry.65/
- Higher-level full/reduced/hierarchical code summarization:
  https://arxiv.org/abs/2503.10737
- RepoSummary — feature-oriented repository summarization:
  https://arxiv.org/abs/2510.11039
- CodePromptZip — code-aware prompt compression:
  https://aclanthology.org/2026.findings-acl.1384/
- RepoDistill — graph retrieval plus learned budget allocation:
  https://aclanthology.org/2026.findings-acl.217/
- LARGER — confidence-filtered expansion from repository search anchors:
  https://arxiv.org/abs/2605.16352
- Aider repository map implementation:
  https://github.com/Aider-AI/aider/blob/main/aider/repomap.py
