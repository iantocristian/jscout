# Value hypotheses after the localization null — 2026-08-09

> Scope note: this document deliberately does not touch the SC-2a workstream
> (workflow synthesis, `annotate`, semantic artifacts, or
> [protocols/two-session-memory.md](protocols/two-session-memory.md), which
> remains that workstream's authoritative gate). It defines the *other*
> surviving value hypotheses and sketches their suites. No `src/` changes are
> implied by this document.

## What the recorded results killed, and what they didn't

The n8n+Twenty post-cutoff suite killed **"find X faster than grep for a
capable agent on lexically anchored questions."** It did not test the failure
modes actually observed in this project's own repositories, which motivated
the tool in the first place:

1. agents get lost digging through files (rabbitholes);
2. agents make wrong assumptions instead of verifying;
3. agents miss the big picture of an unfamiliar repository;
4. agents miss places affected by a change.

These are navigation discipline, verification, orientation, and completeness
— none of which localization scoring measures. The **PR-replay suite** below
is the primary instrument: it measures all four at once on real work, judged
against the real implementation. The H-sections after it define the
individual hypotheses; H2 is fully absorbed by the replay suite's grading.

## The distribution mismatch

The null was measured on n8n and Twenty: huge, popular, convention-heavy
monorepos with strong naming and (in n8n's case) a hand-written architectural
`AGENTS.md` served to every arm. Grep thrives there. The deployment target is
the opposite distribution: solo/small-team repositories with home-grown
patterns, mixed module styles, idiosyncratic naming, and no orientation docs
(ai-pipe, bvb, raggazzi). Private repositories are also uncontaminated by
construction. **Primary corpus for the suites below: the private repos.
n8n/Twenty demote to generalization checks.**

## Evidence already on file (mislabeled, not missing)

The ai-pipe discriminating run recorded, per task: grep inspected 10.25 files
(6.00 irrelevant); jscout-baseline inspected 4.75 (**0.50 irrelevant**);
structural 6.00 (1.75). A 12x reduction in wrong-file reads on this project's
own repository is the anti-rabbithole effect (H1), already measured, buried
under a correctness-ceiling headline. Single trial per cell — direction, not
proof — but it says H1 is live exactly where the pain is.

## Primary suite — PR replay (implement real changes, judge against the real implementation)

**Design.** Mine merged PRs / feature commits / bug fixes from the private
repositories. Turn each into a *story* describing the symptom or desired
behavior. Export the change's merge-base parent as a **history-free snapshot**
(`git archive` — no `.git` directory: a parent *checkout* still contains the
real commit in its object store, one `git log` away from the answer key). The
agent implements the story in that snapshot, with and without jscout; the
gold patch, gold tests, and task metadata live **outside the agent sandbox**.
Grade against the real implementation: what did it get wrong, what did it
miss, what did it cost.
This is the SWE-bench shape with two corrections: private repos
(uncontaminated by construction, the actual deployment distribution) and
reference-based gap analysis rather than test-pass alone.

Why it outranks the H-suites below: it measures the whole failure chain in
the currency that matters. Rabbitholes appear as exploration cost, wrong
assumptions and missed blast radius as divergence from the real patch,
big-picture blindness as wrong-shaped solutions. And the real change solves
gold construction for free: **the real patch's touched files/symbols are the
gold affected-set; its added tests are an executable spec.**

**The unit is a change *arc*, not a PR.** A feature or fix rarely lands in
one commit: follow-up fixes, regression patches, and edge-case corrections
arrive later — sometimes weeks later. The task boundary is therefore the
**complete arc**: seed commit + every semantically related follow-up until
the feature stabilized. This buys two reference points per task:
- **human-first-attempt** (the seed commit alone) — the fair single-session
  comparison: the human's own first implementation, whose omissions the
  follow-ups document;
- **full arc** (the stabilized end state) — the completeness ceiling. The
  follow-up content is *ground truth for what a first implementation
  misses*: the missed-edge-case metric scores whether the agent misses the
  same things the human initially missed, or fewer.

Arc discovery is agent-assisted (mechanical mining → LLM seed filtering →
LLM arc tracing → adversarial arc verification: membership is semantic, not
file-overlap — hot files accrue unrelated commits), and arc snapshots
restrict gold to the union of member-commit files so interleaved unrelated
commits never leak in (`eval-pr-snapshot.mjs --members`).

**Corpus for the replay suite.** Primary: **n8n and Twenty arcs**
(human-authored code, post-cutoff, contamination-probed) — ai-pipe is mostly
AI-generated code with unnaturally uniform structure, so its pilot is
retained for **harness validation only**, not value claims. bvb/raggazzi
remain candidates for later arcs if human-authored history suffices.

**Depth requirement (added after the guided sessions).** Lexically shallow,
single-package tasks are rg-saturated: all four guided arms hit zero
omissions. The discriminating class is **deep tasks** — changes whose
understanding requires descending a multi-package dependency hierarchy
(policy propagation, dispatch/indirection, shared-abstraction changes)
until a grep-only agent loses the thread. Admission gains a depth
dimension: v1 proxy = packages spanned by gold (require ≥3) plus directory
breadth; v2 (planned) = graph distance from lexical seeds to gold sites and
distractor counts, computed from the jscout index itself.

**Mining and admission.**
- Candidates: single-purpose changes, roughly 50–800 changed lines, from
  `git log --first-parent` on the target repos.
- Record per task: story, merge-base parent SHA (the working state), merged
  SHA (the gold), the gold file/symbol sets derived from the diff, whether
  the change carries runnable tests.
- Admission gates: the repo builds (and its tests pass) at the parent SHA;
  the story certifies not-`anchored` against the gold patch files via
  `eval-anchor-certify.mjs`.
- **Gold-test admission (fail-to-pass discipline)**: the change's reference
  tests are extracted as a test-only patch that must (a) apply cleanly to the
  parent snapshot on its own, (b) compile/load without the production patch,
  and (c) **fail on the parent for the intended behavior**. Tests that can't
  meet this encode reference implementation details rather than behavior and
  are excluded from layer-1 grading (the task then grades on layers 2–3
  only, recorded as such).
- **Story provenance rule: stories are written from the issue text, bug
  report, or user-visible behavior — never from the diff.** A story that
  names the symbols the patch touched is a leaked answer key and gets
  rewritten.

**Grading — three layers, most objective first.**
1. **Behavior**: run the real change's added/modified tests against the
   agent's patch, when they exist and run at the parent commit. Pass/fail
   per test, no judgment involved.
2. **Coverage recall — adjudicate first, score second.** The real patch is
   one valid implementation, not the mandatory affected set. So: every
   unmatched gold site goes to blind adjudication *before* scoring
   (`omission` | `alternative_covered` | `not_required`), and the reported
   metric is **confirmed-omission rate**, not raw set recall. Two site
   populations stay **separate and are never combined**: `patched` (files
   the agent's diff actually touched) and `plan_mentioned` (sites its final
   answer claims to have considered) — blending them makes recall gameable
   by narrating. Extraneous-edit rate reported alongside, not blended.
3. **Divergence adjudication**: blind (labels hidden, Sol-style) review of
   every divergence — valid alternative vs. miss — including the edge cases
   the real patch handled (empty inputs, error paths, the second consumer).
   PR review comments, where they exist, are ready-made adjudication
   material. Navigation metrics (reads, tool calls, tokens, wall time) are
   recorded from the same runs, subject to the H1 instrumentation caveat
   below.

**Arms and scale.** grep vs. structural, ≥2–3 trials, profile order
counterbalanced, paired per (task, trial), task-clustered bootstrap.
Implementation runs cost 5–10x a localization run: expect 6–10 tasks per
repo, not 24. When the H3 overview renderer exists, with/without-overview
rides as an additional arm on the same tasks.

**Pre-registration sketch.** Primary: gold-site recall and
missed-edge-case rate (adjudicated), indexed arm vs grep. MIE: +15 points
gold-site recall, or halving of missed edge cases. Explicitly *not*
expected: a tests-pass win (strong models may ceiling there exactly as they
did on localization) or a token win. Failure: the completeness thesis dies
on real work, and with it most of the retrieval product surface.

**Honest caveats.** Solo-authored repos may lack issue text, review
threads, and thick tests — story writing and adjudication get more manual,
and layer 1 may be unavailable for some tasks (fall back to layers 2–3 and
say so per task in the result doc).

**Composition with memory (hand-off note, no implementation here).** PR
*pairs* in the same subsystem are the natural two-session experiment:
session one implements change A, session two implements related change B.
This grounds SC-2a's warm-start protocol in real work; it belongs to that
workstream's gate when it gets scheduled.

## H1 — Navigation discipline ("stop the rabbitholes")

**Claim.** On convention-light repositories, jscout reduces wrong-file
exploration even when correctness ties.

**Instrumentation caveat (H1 is exploratory until this is fixed).**
`inspected_files` is currently the model's *self-report*, not measured
tracing — an agent can under- or over-report what it opened. Before H1
carries any registered claim, either (a) audit self-reports against the Codex
event artifacts (which record actual shell/tool activity) and demonstrate
acceptable agreement, or (b) derive inspected-file counts from the artifacts
directly. Until then, all navigation metrics — including the 0.50-vs-6.00
figure above — are labeled exploratory.

**Suite.** Reuse the existing three-arm harness on ai-pipe + bvb + raggazzi
localization/structure tasks; ≥3 trials; navigation metrics from audited
tracing per the caveat above.

**Pre-registration sketch.** Primary: irrelevant-inspected-files, indexed arms
vs grep, task-clustered CI excluding zero in jscout's favor on the private
corpus. Secondary: tokens-to-answer not worse than +25%. MIE: ≥50% reduction
in irrelevant reads. Failure: H1 is n8n-null-consistent (grep is disciplined
everywhere), and navigation value is dead.

## H2 — Edit-impact completeness ("miss fewer affected places")

> **Absorbed into the PR-replay suite** (grading layers 2–3): the real
> patch's touched set is the gold affected-set, replacing hand-built gold.
> This section is retained for the hypothesis definition and the dynamic-tail
> rationale; it no longer implies a separate suite.

Pains 2 and 4 converge here: wrong assumptions and missed blast radius both
surface when an agent *edits*, not when it searches. This is also the first
suite where the dynamic tail (events, registry dispatch, member-call
candidates, re-export chains) is in the gold sets — the sites grep and
checker-based tools miss *silently*, and jscout returns as labeled
candidates. That difference has never been scored anywhere.

**Suite.** 8–10 tasks per private repo: "you must change X's
behavior/signature — list every site that needs attention before editing."
Gold = complete affected-site sets, hand-built, including ≥1 dynamic-tail
site per task where the repo genuinely has one; independently verified
(Sol-style) before execution.

**Task admission.** The *hidden* affected sites must certify `anchor-free` or
`weak` against the task prompt with `eval-anchor-certify.mjs` (the changed
symbol itself may be named; the sites the agent tends to miss must not be
greppable from the prompt). Contamination probes unnecessary on private
repos; keep frozen-commit + fingerprint discipline.

**Pre-registration sketch.** Primary: **affected-site recall** (per task,
sites found / gold sites), indexed arms vs grep, clustered CI excluding zero.
Secondary: precision (indexed arms must not bury the gold in candidates —
report candidate-list sizes); no correctness regression on the named symbol
itself. MIE: +20 points recall on dynamic-tail sites. Failure: completeness
value is not demonstrable and `possible`-tier surfacing needs rethink, not
expansion.

## H3 — Generated orientation ("big picture through the channel agents already read")

**Insight from the recorded runs.** Adoption: agents ignored the MCP server
unprompted (16 runs, zero calls) — but every arm consumed n8n's `AGENTS.md`
automatically, because project docs are the native channel. Orientation
context is valuable enough that big repos hand-write it; the private repos
have none.

**Claim.** A jscout-*generated*, fingerprinted orientation document
(subsystems, entry points, key symbols by importance, where-things-live)
served as a project doc improves cold-start behavior with **zero adoption
friction** — no MCP call required.

**Suite.** Cold-start tasks (unfamiliar-subsystem questions, small feature
plans) on private repos; arm A = repo with generated overview doc present,
arm B = without; same model, ≥3 trials. The overview is deterministic
(structural graph + importance ranking); it is not an SC-2a semantic
artifact and must not claim beyond T1 facts.

**Pre-registration sketch.** Primary: tokens and tool/read calls to first
gold-relevant action. Secondary: plan-completeness judgment (blind
adjudication) for planning tasks; overview staleness handling (doc carries
its snapshot; a stale doc must be regenerated by `index`, never served
current-looking). MIE: ≥20% median reduction in cold-start cost. Failure:
orientation moves to the memory layer (SC-2a territory) or dies.

**Product note.** If H3 passes, the deliverable is `jscout overview --write`
maintaining the doc on re-index — the first jscout output that meets agents
where they already are. (Any extension of the overview with semantic/workflow
content belongs to SC-2a and its freshness rules; out of scope here.)

## H4 — Memory

Owned end-to-end by the SC-2a workstream and its
[two-session gate](protocols/two-session-memory.md). Listed here only for
completeness of the value map. If H3 passes, its generated overview becomes a
natural warm-arm component in that protocol.

## Sequencing and ownership

| Order | Suite | Depends on | Conflicts with SC-2a? |
|---|---|---|---|
| 1 | H1 re-run on private repos | nothing (harness exists) | no — reuses recorded harness |
| 2 | **PR-replay pilot** (primary; absorbs H2, records H1 metrics) | PR mining + story authoring + patch-grading additions to the harness (apply/build/test + diff-vs-gold scoring) | no — eval/ + scripts/ only |
| 3 | H3 generated overview | small `overview --write` renderer over existing T1 tables; then rides as an arm on PR-replay tasks | no `src/` overlap with semantic artifacts; schedule the renderer after SC-2a's current changes land to avoid tree churn |
| 4 | H4 | SC-2a; PR pairs from the replay corpus feed its two-session gate | — |

Full pre-registrations are written immediately before each suite runs (same
rule as [prereg/file-roles-2026-08-09.md](prereg/file-roles-2026-08-09.md));
the sketches above fix the primary metric and MIE now so task design cannot
drift toward a metric the tool happens to win.
