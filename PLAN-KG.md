# js-rag — agent memory architecture (plan, revision 2)

Supersedes revision 1 of this file after design review (nine findings folded in)
and the product-framing discussion.

## What js-rag is

**Persistent, verifiable memory for coding agents working on a repository.**
Agents start every session amnesiac; they re-derive the same structural facts
and the same semantic conclusions (what calls what, what participates in which
workflow) hundreds of times, then discard them. js-rag is where that knowledge
persists between sessions — with the discipline that every stored claim carries
provenance, confidence, and freshness, so accumulated memory never masquerades
as current truth.

The serving contract: *given an agent's query or focus, return the smallest
trustworthy slice of repository evidence that improves its next action.*
The agent remains the reasoner; js-rag supplies evidence and relationships.

## Trust tiers

| Tier | Contents | Freshness model | Storage |
|---|---|---|---|
| **T1 — deterministic facts** | symbols, resolved refs, module edges, entities (routes/tables/env/services), confidence-labeled (certain / likely / possible) | always fresh (rebuilt incrementally at index time, ms-scale) | tables (exists + KG-1) |
| **T2 — derived views** | skeletons, neighborhood renderings, repo map, paths | **rendered on demand, never stored** — a deterministic projection of T1 cannot be stale | none (code only) |
| **T3 — semantic memory** | workflows, annotations written by LLM passes or by agents themselves | fingerprinted; served with explicit `fresh | stale` label; manual rebuild | tables (KG-3) |

Design rule carried through every tier: when resolution or meaning is
uncertain, change the *structure* (hub nodes, candidate lists, stale labels)
rather than emitting confident-looking output.

A clarification the discussion forced: **"TypeScript is for humans" means types
are documentation.** The resolver never uses type-space (that stays); but type
text lives on in chunks, embeddings, and skeleton signatures. Erasure is about
*edges*, not text.

## Research notes (inputs, with corrections from review)

- **LARGER** (arXiv:2605.16352): expansion from search seeds is the largest
  single retrieval factor — ablation −13.5% *relative* (7.5 pts absolute
  Acc@5); confidence-filtered edges −4.7%. Caveat: their confidence weights are
  learned and provenance-specific. Our 1.0/0.6/0.3 tier weights are a starting
  heuristic that the eval harness (KG-1) exists to validate or tune — the plan
  does not claim LARGER validates the formula.
- **GraphRAG-Bench** (ICLR'26): graph retrieval often *underperforms* vanilla
  RAG on simple lookups; gains concentrate in multi-hop/structural questions.
  → expansion is a per-query parameter, default off.
- **Aider repo map** (repomap.py): identifier multipliers (×10 mentioned, etc.)
  apply to **edge weights** during graph construction; the PageRank
  `personalization` vector is **file-based** (chat files). Our `--focus`
  (personalization mass on focus nodes + edge-weight boost for focus-matching
  identifiers) is an aider-*inspired* heuristic of our own, not a reproduction.
- **Compression research** (HCP arXiv:2406.18294; ProConSuL; hierarchical
  summarization arXiv:2503.10737): preserving signatures + topology while
  dropping dependent bodies retains task performance; callee context reduces
  hallucination. → the T2 skeleton keeps identifiers and behavioral shape,
  drops bodies.
- **LLM-built-KG failure mode**: uncontrolled vocabulary growth. → T3 dedups
  against existing vocabulary per batch; concept-to-concept edges are banned
  until the vocabulary is stable; LLM assertions cap at `likely`.
- **SQLite scale**: indexed recursive CTEs are ms-level at depth 2–3 up to
  ~100k+ nodes. ai-pipe ≈ 10k symbols / 30k refs → two orders of magnitude of
  headroom. No graph DB.

---

## KG-1 — T1 completion: identity, resolution, traversal

The prerequisite for everything: trustworthy anchors. This phase looked like
"views over existing tables" in revision 1; review showed the real work is
identity and materialization semantics.

**Node identity.** Stable string ids:
- `file:<path>` · `pkg:<name>` · `ent:<type>:<canonical-name>` (KG-4)
- `sym:<path>#<scope-chain>.<name>:<ordinal>` — scope chain disambiguates
  same-named methods of different classes; ordinal (file-order among same-keyed
  duplicates) breaks remaining ties.
- **IDs are snapshot-deterministic only**: same index state → same ids. They
  are NOT stable across edits/moves; consumers must not persist them across
  re-indexes. (Cross-edit symbol identity is explicitly a non-goal.)

**Chunk→graph projection** (chunks are retrieval artifacts, not ontology —
they never become nodes): `seed(chunk) = symbols whose declaration span
overlaps the chunk span; else the file node` (covers imports/module chunks).
Deterministic, unit-tested. Search hits (chunk ids) enter the graph through
this projection; responses still attribute neighborhoods to the hit that
seeded them.

**`resolved_refs` materialization.** Refs currently store `(request, name)`
resolved at query time. KG-1 materializes resolution at index time —
`resolved_refs(ref_id, src_node, dst_node, kind, confidence, line)`.
Invalidation policy: **full rebuild after every index pass** (same policy as
`module_edges`), because a barrel edit reroutes references in unchanged
importers and transitive invalidation tracking is a bug farm. Perf gate:
rebuild < 100ms on ai-pipe (expected ~10–30ms: in-memory map hops × ~30k
refs). Only if a 10× repo breaks the gate do we earn the right to write the
clever version.

**Edge semantics — events become hubs.** Never pair emit×listen by name
(complete-bipartite blowup on `error`/`data`/`close`; cross-process false
edges on signals). Instead: `ent:event:<name>` hub node; `emit→hub` and
`hub→listen` edges, cardinality O(sites). Direct emitter↔listener edges only
when receiver identity is recoverable (both sides resolve to the same object
symbol — e.g. a shared imported bus) → `likely`. Hub membership alone →
`possible` (correctly excluded by the default expansion filter). Names with
pathological fan-out get `generic:true` in meta.

**`kg_nodes` / `kg_edges`** as views over files ∪ symbols ∪ packages ∪
entities and resolved_refs ∪ module_edges ∪ reexports ∪ event edges ∪ member
calls, each row carrying `(kind, confidence, source: ast|heuristic|llm)`.

**`neighborhood <spec>`** (CLI + MCP): params `depth` (default 2), `budget`
(max nodes, default 20), `min_confidence` (default `likely`), `kinds`.
Expansion is budgeted and ranked, never a raw k-hop flood: per-anchor score =
tier weight (certain 1.0 / likely 0.6 / possible 0.3) × edge-kind weight
(call/render > import > use) × 1/log(2+degree) hub damping; top-k per anchor.

**`search --expand`** (MCP `semantic_search {expand:true}`): after the
existing RRF + rerank pipeline (never counted against the rerank pool),
attach `neighborhood(depth=1, budget=8)` of the top 3 hits, rendered as
skeletons (KG-2 format). Default off, per GraphRAG-Bench.

**Eval harness (`js-rag eval`)** — part of KG-1's definition of done, not an
afterthought:
1. **JSDoc holdout** (automatic): sample N documented functions, query with
   the doc text, gold = its chunk; Recall@5/@10 for BM25 / +vector / +rerank.
   Validates search components. *It cannot validate expansion* (expansion
   attaches neighbors after ranking; the gold hit's rank is unchanged).
2. **Structural suite** (small, curated, per test repo): questions whose
   answer is 1–2 hops from the lexical seed ("what renders X", "what breaks
   if Y changes signature", "which handler writes table Z"), scored on
   whether the expanded payload contains the gold node. This is what
   validates expansion and the tier-weight heuristic.
3. **Fixture repo** (synthetic, in tests/): neighborhood precision assertions
   per tier — must-contain / must-NOT-contain lists; extractor fixtures.

**DoD**: identity tests (collision fixtures), projection tests, resolved_refs
rebuild < 100ms on ai-pipe, neighborhood < 50ms, structural-suite baseline
recorded, JSDoc holdout wired into CI.
**Estimate: 1.5–2 days** (honest revision of rev-1's "~1 day").

## KG-2 — T2 renderers: skeleton, map, paths

All deterministic projections of T1. **Rendered on demand, never stored** —
stored skeletons would be a cache-invalidation liability; regeneration is
sub-ms from data already in SQLite.

**Skeleton renderer** — the compact form for every graph-shaped output
(neighborhood, map, expand payloads). Per symbol, from AST + resolved edges:
signature line (with its type text — types are documentation), scope, guards
and significant branch predicates, calls/renders/constructs with resolved
targets, writes (tables when KG-4 lands), emits, throws/returns, async
markers, JSDoc first line, source span for drill-down. No LLM pseudocode —
LLM rewriting silently drops callee names, failure paths, mutation. Format is
deterministic IR, roughly:

```
sym: server/checkout.mjs#checkout:1  [function, exported]
  sig: async function checkout(cart, user) -> Promise<Order>
  guard: cart.items.length > 0 else throw EmptyCartError
  calls: inventory.reserve [certain], payments.authorize [certain]
  emits: event:order.created
  spans: 12-58
```

**`map [--focus ...] [--tokens N]`** (MCP `repo_map`): personalized PageRank
over kg_edges (power iteration, ~20 rounds, in-process, no new deps).
Personalization mass on `--focus` nodes; edge-weight boost for focus-matching
identifiers (our heuristic — see research note). Output: top symbols as
skeleton signature lines grouped by file, binary-searched to the token budget.

**`paths <from> <to>`**: BFS over call/import/render edges, max depth 6,
skeleton-rendered.

**`graph export --format dot|jsonl`**.

**Estimate: 1 day** (skeleton renderer is most of it).

## KG-3 — T3 semantic memory: workflows + write-back

One T3 record type to start — **workflows** — because "which workflows does
this code participate in" is the concrete query this layer exists to answer.
The annotation/capability/domain-concept taxonomy from the design discussion
is explicitly deferred: ontology before evidence is the documented LLM-KG
failure mode. A second record type gets admitted when a real query needs it.

**Schema** (also serves KG-4 entities — one generic shape):

```sql
sem_records(id, type,            -- 'workflow' (only value for now)
            name, description,
            model, prompt_version, created_at,
            corpus_fingerprint)  -- XOR of file hashes at generation time
sem_supports(record_id, node_id, role,        -- e.g. 'orchestrator'
             evidence_span,                   -- file + line range
             node_content_hash,               -- supporting symbol's hash then
             confidence)                      -- likely | possible, never certain
```

**Generation (`js-rag scout [--budget N]`)**: seeds = entry-point
neighborhoods ordered by PageRank importance (routes and event hubs when
KG-4 lands make seeds much better — that's L2's job description). Per seed:
one LLM call, input = skeleton rendering of the neighborhood (KG-2 — cheap,
exact), output = `{workflow, participants: [{node_id, role}]}` validated
against existing vocabulary (dedup per batch). Model self-report maps to
likely/possible; **never certain**.

**Write-back (`annotate` MCP tool)**: agents can store their own findings as
sem_records with `model = agent-reported`, same evidence/fingerprint rules.
The most natural author of "this is the checkout flow" is the agent that just
spent a session proving it. This turns js-rag from a read-only index into
shared memory across agent sessions — it is the cheapest high-leverage
feature in this plan.

**Staleness (short-term contract — deliberately simple)**: every response
containing T3 data compares stored fingerprints/hashes against current index
state and labels each record `fresh` or `stale` (per-support `degraded` when
only some supports moved). Stale records are still served, visibly marked.
`scout --rebuild` regenerates. Granular invalidation (per-symbol annotation
refresh, transitive workflow invalidation, GC of unsupported records) is
designed but deferred — the fingerprint check makes "not caring yet" safe.

**Estimate: 1–1.5 days** (excluding prompt iteration).

## KG-4 — T1 enrichers: entities (routes, env, tables, services)

Recast per discussion: extractors exist to (a) answer blast-radius queries
directly and (b) make T3 seeds and skeletons better (routes/tables are where
workflows start). They compete for implementation order on that basis.

**Schema — canonical entity vs. occurrences** (review finding: one row can't
carry both canonical identity and per-site provenance):

```sql
entities(id, type, name, meta_json)            -- canonical; keyed by normalized name
entity_occurrences(entity_id, file_id, chunk_id, line,
                   extractor, confidence)      -- provenance per site
entity_edges(occurrence_id, target_kind, target_id, kind, confidence)
```

Occurrences FK-cascade with files; after re-index, entities with zero
occurrences are GC'd; entity_edges hang off occurrences so a file re-index
cleanly removes exactly its contribution.

**Extractors** (one visitor pass alongside heur.rs; literal = certain,
constant-prefix template = likely):
1. **Routes** — Express/fastify registration patterns; Next.js file
   conventions (`app/**/page.tsx`, `pages/**`) from paths alone. Frameworks
   beyond these: detect from package.json deps; config escape hatch later.
2. **Env vars** — `process.env.X`, `import.meta.env.X`. Answers "what config
   does this subtree need".
3. **DB tables** — SQL-verb scan over string literals (>12 chars containing
   FROM/JOIN/INSERT INTO/UPDATE) + ORM patterns (`prisma.<model>.`,
   `knex('t')`, `.from('t')`, `db.collection('c')`). Unlocks the blast-radius
   query: table → touching functions → callers.
4. **External services** — fetch/axios/ky with literal URLs → host nodes;
   external packages already in module_edges.

Existing events migrate into this model as `ent:event:<name>` (KG-1 hub
design). `neighborhood` accepts entity specs (`route:POST /checkout`,
`table:orders`, `env:STRIPE_KEY`).

**Estimate: 1 day.**

## Sequencing

| Phase | Delivers | Est. |
|---|---|---|
| KG-1 | identity, projection, resolved_refs, event hubs, neighborhood, search --expand, eval harness | 1.5–2 d |
| KG-2 | skeleton renderer, map, paths, export | 1 d |
| KG-3 | workflows (scout), annotate write-back, staleness labels | 1–1.5 d |
| KG-4 | entities: routes, env, tables, services | 1 d |

KG-3 before KG-4 is deliberate (per discussion): the semantic-memory
hypothesis gets tested early with graph-connectivity seeds; KG-4 then
upgrades seed quality rather than gating the experiment. If scout shows no
retrieval value on the structural suite + agent metrics, T3 stays opt-in and
KG-4's blast-radius value stands alone.

## Success metrics

Component metrics (automatic): JSDoc holdout Recall@k; structural-suite
hit-rate with/without expansion; neighborhood precision per tier on fixtures;
latency gates (neighborhood < 50ms, map < 300ms, resolved_refs < 100ms).

**Product metric (the one that matters)**: on repeated agent sessions over
the same repo (bvb / ai-pipe), with js-rag MCP vs. without — searches per
task, files read per task, tokens to first correct edit, task completion.
"Does the second session start warmer than the first." Recall@k is a
component metric, not the product metric.

## Deferred / parked

- DI-token heuristics, dynamic-import constant-prefix resolution (parked from
  M5; no current query needs them).
- Embedding-cluster "themes": if built, exposed as `themes`, never as the
  concept layer (clusters are regions, not meanings). Contract if revived:
  model = active provider's else max-coverage; refuse < 80% chunk coverage;
  L2-normalized cosine (spherical k-means); k-means++ seeded from corpus
  fingerprint (deterministic); recluster at >20% chunk churn or --force;
  excluded chunks reported.
- Concept-to-concept edges; annotation/capability/domain-concept record
  types; granular T3 invalidation; cross-edit stable symbol identity.
