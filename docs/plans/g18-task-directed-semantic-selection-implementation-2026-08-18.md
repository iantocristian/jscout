# G18 task-directed semantic selection: implementation plan

Date: 2026-08-18  
Status: implementation-ready; begins after G17  
Normative parent: `PLAN.md`, “Planned G18 — task-directed semantic coverage and selection”

## Problem statement

Card generation currently sorts all automatic subjects by a single repository-wide structural weight and then applies one global cap. A large high-degree area can consume the entire budget while another reconnaissance scope receives no card. The Next.js root-layout campaign spent 448 calls without generating memory for the relevant type-generation surface.

Direct `semantic_memory` retrieval has the inverse problem: it can rank a large corpus of generic lexical/vector analogs even when the caller supplied localized code evidence. Broad responses return full artifact bodies, so a weak candidate pool consumes the 24 KB budget before the agent can decide which artifact deserves inspection.

G18 separates three concerns:

- fair, observable batch coverage;
- bounded targeted generation for a localized code surface;
- support-aware compact discovery followed by exact artifact drill-down.

It remains optional semantic enrichment. It does not run during index, search, or watch; it does not feed generated prose into code ranking; it does not introduce a task-design agent.

## Part A: scope-stratified automatic card coverage

### Scope identity

Every automatic card subject receives one deterministic selection scope:

1. use the current, fresh G13 repository-policy subject that owns the subject’s file;
2. if no actionable policy row exists, use the current reconnaissance subject membership, including neutral `mixed` or `unknown` classifications;
3. otherwise fall back to a deterministic structural scope: origin plus top-level repository area.

Scope identity is selection metadata, not artifact truth. Stale or absent reconnaissance never hides a subject.

### Allocation algorithm

Automatic candidates retain their existing confidence-weighted structural score. Selection changes from one global sort to deterministic stratification:

1. group candidates by selection scope;
2. sort each group by the current subject comparator;
3. order scopes deterministically by their best candidate score, then scope key;
4. allocate one candidate per non-empty scope in round-robin passes;
5. stop at the configured subject limit;
6. do not make more model calls than the existing call budget permits.

If the budget is smaller than the number of scopes, the report must make the uncovered scopes explicit. “Planned 1,024 subjects” must never be presented as whole-repository coverage.

### Coverage report

The dry-run plan and executed batch report expose, per scope:

- candidates discovered;
- subjects selected;
- subjects omitted by selection limit;
- selected subjects reused;
- selected subjects requiring a model call;
- calls completed, failed, or skipped by call budget.

The global report retains current totals and adds the number of discovered, covered, and uncovered scopes. This is accounting only; it does not change confidence or freshness semantics.

## Part B: targeted card generation

Extend `jscout scout cards` with repeatable selectors:

```text
--anchor <exact-anchor>
--file <repository-relative-path>
--subject <reconnaissance-subject-key>
```

Selectors may be combined. Their union is deduplicated by canonical card anchor.

### Exact anchor selector

Preserve the existing exact-anchor behavior: resolve the current anchor and build one card subject from its bounded structural evidence.

### File selector

The path must resolve to one indexed repository or selected-dependency file. Select bounded card-worthy symbols in that exact file using the existing subject eligibility rules and deterministic priority. If the file contains no eligible symbol, return an explicit empty selection rather than widening to neighboring files.

### Reconnaissance subject selector

Resolve the current classification’s exact selector and membership. Select eligible card subjects only from member files, then apply the same scope-local weight ordering and configured subject limit. A missing, stale, or ambiguous subject key is an error with a command showing how to list current subjects; it must not fall back to repository-wide scouting.

### Bounds

Targeted generation is bounded by:

- the existing card subject limit;
- the existing per-evidence-pack byte and row limits;
- the command call budget;
- deterministic selector membership.

No target selector implicitly increases any bound. Dry-run remains available and must show reuse versus new-call decisions before model execution.

## Part C: support-aware direct semantic retrieval

### Localized inputs

Extend `semantic_memory` with localized selectors while retaining current exact artifact retrieval:

- one or more exact code anchors;
- an exact indexed file path;
- one current reconnaissance subject key;
- exact `artifact` ID for drill-down.

Anchor/file/subject selectors form an evidence scope. An artifact is supported when a current support row points to an allowed anchor or member file. Related artifacts are not direct matches; they remain available during exact drill-down or as a clearly lower relation-connected tier if explicitly requested.

### Selection order

When localized selectors are supplied:

1. filter to artifacts with direct current support inside the evidence scope;
2. order exact anchor support before same-file support, then same reconnaissance-scope support;
3. use exact artifact/name match, lexical score, vector score, freshness, and stable ID only within the same support tier;
4. never backfill from unsupported lexical/vector analogs.

If the supported set is empty, return status `no_supported_memory` with the resolved selectors and zero artifacts. This is a successful, informative query—not an exception and not an excuse to return CMS examples.

When no localized selector is supplied, broad lexical/vector discovery remains available and is explicitly labelled as discovery rather than evidence-connected memory.

## Part D: compact discovery and exact drill-down

### Discovery handle

Broad and localized multi-result queries return compact handles containing:

- artifact ID, type, name, freshness, confidence, and current state;
- support count and a bounded support summary;
- selection tier/reason;
- retrieval diagnostics when present;
- a copy-safe follow-up object for `semantic_memory({ artifact: <id> })`.

They do not return the full artifact body, model/prompt audit fields, all supports, relations, concept tags, or source excerpts.

### Exact artifact response

An exact artifact-ID request is drill-down and may return:

- the full body;
- bounded support rows;
- requested relations;
- requested source evidence;
- model, prompt, source snapshot, and creation audit fields.

Existing freshness, origin, source-byte, relation-depth, and complete response-byte budgets still apply. The default budget stays 24 KB. If one full artifact cannot fit, the request fails with the existing minimum-response error rather than silently corrupting the body.

### Compatibility

The MCP schema retains current arguments where possible. New fields are additive. The serialized result adds an explicit mode/status and compact handle collection. During the transition, exact artifact responses keep the current detailed `semantic_artifacts` shape; discovery responses use `artifact_handles` and leave detailed artifacts empty. This avoids representing an omitted body as a real JSON `null` body.

## Code changes

### Scouting

- `src/scouting/plan.rs`
  - selection-scope resolution;
  - stratified candidate allocation;
  - targeted anchor/file/subject planning;
  - per-scope plan accounting;
- `src/scouting/mod.rs`
  - propagate selection scope through prepared/reused/executed reports;
  - aggregate per-scope execution results;
- `src/recon.rs`
  - expose read-only current subject membership for exact subject targeting;
- `src/main.rs`
  - add repeatable `--file` and `--subject` card selectors;
- CLI/MCP documentation and skill guidance
  - show targeted scouting as an explicit enrichment step after localization.

### Semantic retrieval

- `src/semantic_query.rs`
  - localized selector validation and resolution;
  - direct-support candidate tiers;
  - explicit retrieval mode/status;
  - compact artifact handles and exact-ID detail mode;
  - budget accounting for both shapes;
- `src/mcp.rs`
  - additive selector schema and descriptions;
  - copy-safe exact artifact follow-up;
- `src/semantic.rs`
  - retain lexical/vector ranking as an intra-tier ordering signal only.

The first implementation should not require a database migration. It derives scopes from current reconnaissance projections and existing semantic support rows. If query performance later requires a projection/index, that optimization must preserve the same observable selection semantics.

## Test plan

### Scope-stratified planning

- a high-degree scope cannot consume every slot while another scope has candidates;
- selection is deterministic across insertion order;
- budget smaller than scope count reports uncovered scopes exactly;
- stale/missing reconnaissance falls back to structural scopes without hiding candidates;
- per-scope discovered/selected/omitted totals reconcile with global totals.

### Targeted scouting

- exact anchor selects only its canonical subject;
- exact file selects only eligible symbols from that file;
- reconnaissance subject selects only current member files;
- combined selectors deduplicate subjects;
- missing file/subject returns an error and never widens globally;
- dry-run accurately distinguishes reusable subjects from calls;
- call and subject limits still terminate deterministically.

### Semantic selection

- exact anchor support outranks a higher-scoring unrelated vector result;
- file support outranks unsupported lexical analogs;
- subject selection includes supported member-file artifacts only;
- no direct support returns `no_supported_memory` and no weak fallback;
- origin and freshness filters still apply;
- multiple supported artifacts retain deterministic intra-tier ranking.

### Transport and budgeting

- broad discovery emits compact handles without bodies;
- each follow-up object round-trips to exact artifact detail;
- exact detail retains body, supports, relations, and optional evidence;
- compact and detailed responses each obey one complete byte budget;
- increasing result limit does not silently increase response bytes;
- existing source and relation truncation counters remain correct.

### Full regression

Run Rust formatting, Clippy, the complete Rust test suite, CLI/MCP schema tests, and script tests locally. CI is confirmation, not the first test run.

## Delivery sequence

1. Add selection-scope metadata and stratified automatic card planning.
2. Add per-scope dry-run/execution accounting.
3. Add exact file and reconnaissance-subject targeted card selectors.
4. Add localized support filters and `no_supported_memory` status.
5. Add compact discovery handles and exact-ID drill-down.
6. Update CLI/MCP/skill documentation.
7. Run full local validation and a bounded Next.js dry-run proving target selection without model calls.

Commits should keep scouting coverage, targeted generation, support-aware retrieval, and compact transport independently reviewable where practical.

## Failure handling and rollback

Scouting selection is stateless until the existing run ledger claims work. Dry-run must therefore expose the entire decision before calls begin. A targeted selector resolution failure aborts before any model call. Per-subject remote timeouts keep current subject-local failure behavior; gateway protocol or publication invalidation remains batch-fatal.

Semantic selection is read-only apart from the repository’s pre-existing database-open behavior. The new compact discovery shape can be reverted independently of stored artifacts. Existing artifacts, supports, embeddings, and recon classifications require no rewrite.

## Completion gate

G18 is complete only when fixed call budgets yield auditable per-scope coverage, targeted selectors remain inside their exact evidence surface, localized memory never backfills unsupported analogs, broad queries return handles rather than bodies, exact drill-down preserves the detailed surface, the 24 KB default remains unchanged, and all local tests pass.
