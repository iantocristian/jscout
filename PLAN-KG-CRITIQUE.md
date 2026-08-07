# Critique of the converged plan (PLAN-KG-REVISED-CODEX, 2026-08-07)

> Adversarial review of the converged roadmap. Context: two plans
> ([PLAN-KG.md](PLAN-KG.md) rev 2 and [PLAN-KG-REVISED-CODEX.md](PLAN-KG-REVISED-CODEX.md))
> were produced independently from the same design discussion and have now
> converged. Convergence between documents with a shared parent is not
> validation — it is shared blind spots. This document attacks the
> foundations both share. Five challenges, no nitpicks. Each ends with a
> concrete counter-proposal and what evidence would retire the challenge.

## C1 — The plan builds ~8–10 days of infrastructure for a consumer nobody has observed

**Claim.** The deepest unexamined assumption is that agents will call these
tools at all. Agents given search indexes reliably fall back to grep: it is
predictable, composable, and they were trained on it. Five MCP tools are live
today (`semantic_search`, `who_uses`, `definition`, `file_outline`, `events`)
and there is zero observational data: whether real sessions call them, whether
their output is *used* once returned, where they lose to `grep -r`, whether
tool descriptions even trigger selection.

Meanwhile the plan's own "product metric" (agent utility) is scheduled last,
as an evaluation of finished layers, and no phase owns building the harness.
If agents don't call `who_uses` today, they will not call `neighborhood`
tomorrow — and the real fix will be tool descriptions, output legibility, and
latency-of-decision, not additional representation layers.

Secondary defect: the evaluation section proposes comparing agents "with and
without each representation layer" across six metrics — a combinatorial
experiment that will never actually run.

**Counter-proposal.** Add **phase 0, before RI-1**: a scripted agent A/B
harness — fixed task list over bvb/ai-pipe, N repetitions, one headline
metric (tool-calls-to-first-relevant-file or tokens-to-correct-edit) —
run against the *existing* tools vs. grep-only. ~1 day. Every later phase
gate becomes a single A/B against the previous phase on the same harness.
Phase-0 findings are explicitly allowed to reorder the roadmap.

**Retired when.** The harness exists, baseline numbers are recorded, and the
data shows the existing tools get selected and consumed by agents (or the
roadmap has been re-prioritized in response to finding they don't).

## C2 — The custom skeleton IR never competes against the cheap baseline: elided source

**Claim.** SC-1 invents a language (`guard X else throw Y`, `ir_json`,
`ir_version`, golden fixtures for every JS construct, preservation checks) and
takes on permanent format-maintenance tax. But the cited research does not
support *custom IR* — it supports **real source with bodies pruned**: HCP
prunes dependent implementations while keeping actual signatures/topology;
the hierarchical-summarization study finds full code strongest and *reduced
code* the cost-efficient fallback; aider ships elided verbatim source. LLMs
are trained on oceans of JavaScript and zero lines of this IR; the bespoke
format may be *worse per token* than the source lines it paraphrases.

Elided source is nearly free to produce: guard/call/throw/return spans are
known from the AST, so "signature + selected verbatim lines + `⋯` elision
markers + resolved-target trailing comments" is span surgery over content
already stored in SQLite.

Consequence for storage: the concession that R2 must be stored rested on
`rendered_text` being a distinct, searchable representation. Elided source is
a *subset of chunk content* — if it wins, `scout_units` shrinks to span lists
(or disappears; render from spans + stored chunk content), and the "skeletons
cannot be reconstructed from graph tables" premise mostly dissolves. SC-1
could drop from 2–3 days plus permanent tax to ~1 day.

**Counter-proposal.** Make **skeleton-IR vs. elided-source an A/B gate inside
SC-1**, judged on the curated structural question set at equal token budgets.
Elided source is the default; the IR ships only if it wins at equal fidelity.

**Retired when.** The A/B has been run and the IR demonstrably compresses
better at equal answer quality — or the plan has switched to elided source.

## C3 — Snapshot-scoped anchors + watch mode + agents that cache = silent wrong answers

**Claim.** The identity model is formally clean and practically hazardous.
An agent holds `sym:api.ts#UserService::modify@1` in its context window
across a session *while editing the repo*. Watch mode rebuilds the projection
on every save; ordinals shift. The plan gives responses a snapshot
fingerprint — detection — but never specifies what any tool *does* when
handed a stale anchor. The failure mode is not an error: **ordinal reuse
means a stale key can resolve to a different declaration with no signal.**
That is precisely the category this project promised never to ship —
confident-looking wrongness — sitting inside its most-demoed workflow
(live editing under watch).

**Counter-proposal.** RI-1 owns **stale-anchor semantics as a first-class
contract**: anchor references travel with the snapshot id they came from; on
snapshot mismatch the server re-resolves by (path, scope, name) and labels
the result `re-resolved`; ambiguous re-resolution fails loudly with
candidates instead of guessing. Fixtures simulate the mid-session-edit
sequence explicitly.

**Retired when.** The contract is specified in RI-1's DoD, implemented, and a
fixture proves a stale anchor after an ordinal-shifting edit either
re-resolves correctly or errors — never silently misresolves.

## C4 — SC-2a bets the first LLM tokens on the artifact with the weakest query

**Claim.** Cards-first is backwards. Bounded to ~1,000 PageRank-selected
symbols, SC-2a is ~1,000 multi-kilotoken LLM calls — per model, per prompt
version, re-run on staleness — to produce purpose/role/invariants that the
consuming agent derives nearly free for any symbol already in its context,
and that embeddings-over-code already partially retrieve for symbols that
aren't. **Cards lack a crisp query that search cannot answer.** Workflows
have exactly that query ("which workflows does this code participate in" —
relational, multi-file, named by the user in their own words), and a workflow
experiment needs **~20–30 LLM calls on connectivity seeds** while proving the
identical storage/validation/freshness machinery SC-2a exists to prove.
Plumbing can be proven at 3% of the cost against the artifact with a known
query.

Related gap: "semantic search can retrieve a workflow" has no fusion story.
Prose artifacts and code chunks have different length and register
distributions; a code-specialized embedder (e.g. nomic-embed-code) ingesting
JSON cards is out-of-distribution. Semantic artifacts likely need their own
index and their own result section, not a seat in the code ranking.

**Counter-proposal.** Swap the order: **SC-2a = bounded workflow experiment**
(connectivity seeds, ~dozens of calls, freshness labels, `annotate`
validation piggybacks here); **symbol cards move behind the same value gate
as summaries** (SC-2c tier) and must name, in advance, the query set they are
supposed to win. Specify separate indexing/routing for semantic artifacts.

**Retired when.** Either the swap is adopted, or cards-first is defended with
a concrete query set that cards answer and search-plus-workflows cannot, with
cost accounting.

## C5 — The plan never says why not tsserver

**Claim.** An agent can already get checker-backed find-references and
definitions from tsserver via LSP MCP wrappers (Serena and similar) —
strictly more precise than a checker-less graph on typed code, with no index
build. The plan records no comparison, concedes nothing, and parks nothing.
The real answers exist and are good: tsserver cold-start and monorepo weight
vs. ~30ms warm re-index; parity on untyped JS; **confidence-labeled
candidates for the dynamic tail that tsserver silently misses**; entities;
cross-session semantic memory; one static binary. But unstated positioning is
a strategic hole: the first sophisticated reader asks "why not Serena?", and
each phase gate loses a sharpening question it should have — *does this beat
what an LSP wrapper gives for free?*

**Counter-proposal.** Add a positioning section to the operative plan:
what jscout concedes (precise typed call hierarchy, rename/refactor
tooling), what it wins on (speed, JS parity, honest uncertainty, entities,
memory, ops), and park "optional tsserver enrichment pass for
interface→implementation edges" as an explicitly deferred idea with a
revisit trigger.

**Retired when.** The section exists and phase gates reference it.

## The pattern

The plan is strongest where it is furthest from the user — storage schemas,
freshness lattices, IR fixtures — and weakest where the product actually
lives: does an agent pick the tool over grep, read its output correctly, and
survive an edit mid-session. All five challenges are instances of one rule:
**validate the consumer before building for it.**

## Proposed re-sequencing (if all five are accepted)

| Phase | Change from converged plan |
|---|---|
| **P0 (new)** | Agent A/B harness against existing tools; baseline recorded; findings may reorder everything below |
| **RI-1** | + stale-anchor contract (C3) in DoD; structural core otherwise unchanged |
| **SC-1** | Elided-source default; skeleton-IR only via A/B gate (C2); storage shrinks accordingly |
| **SC-2a** | Workflow experiment (~dozens of calls) + `annotate` validation (C4) |
| **EN-1** | Unchanged; still feeds workflow seed quality |
| **SC-2c gate** | Symbol cards join summaries behind the value gate, with a pre-registered query set (C4) |
| **Doc** | Positioning-vs-LSP section (C5) |
