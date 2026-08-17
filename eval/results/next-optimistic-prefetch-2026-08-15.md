# Next.js PR-replay: optimistic-routing prefetch loop (trial 001)

Execution: 2026-08-15
Task: `next-optimistic-prefetch` (slate priority 2; issue #97135, fix #97128)
Base `7cb68c12` (direct parent) · Reference `5942b37a` · 26 files, ~900 additions
Agent: `gpt-5.6-terra`, high reasoning · jscout baseline `d138de4`
(binary sha256 `152f7aa1…73f06`)

## Outcome

- **0 of 13 arms passed the hidden oracle.** This task discriminates where the
  calibration task could not — and it discriminates *within* failure.
- The gold fix has two mechanisms: (A) prediction safety in
  `segment-cache/optimistic-routes.ts`, and (B) a rewrite-divergence channel
  threaded through six more files (`RouteTreeAccumulator`,
  `treeDivergedFromBase`, `canonicalizeURLPart`). Mechanism B was implemented
  by **no arm**: zero `canonicaliz*`/`diverg*` lines across all 13 patches,
  answers, and agent messages. The rewrite-misprediction test failed 13/13.
- Mechanism A (the livelock) was fixed by 7 of 13 arms, with no profile
  monotonicity: grep control fixed it; both embed-only arms lost it; within
  the last two profile pairs the dose-response *inverted* (the 5-call skill
  arm fixed what the 25-call forced arm did not, and vice versa in the
  neighboring pair).
- **Every arm patched exactly 1 of 7 gold files** (`optimistic-routes.ts`),
  0 extraneous production files. Six gold files per arm remain
  pending blind adjudication (not yet performed).

## Admission

Story anchor-certified `weak` with zero identifier anchors; Terra/high
no-tools contamination probe returned `files: [], symbols: []` ("I do not
genuinely remember…") — clean. Setup pass; hidden tests fail-to-pass on the
first gate attempt (parent exit 1 with exactly the two oracle cases failing,
reference exit 0). The gate required one harness correction: Next.js e2e
consumes a packed tarball of `packages/next` with no turbo `dependsOn`, so
`test_command` must run `pnpm build` first (cache hit when unchanged) or
patches never reach the tests. `HEADLESS=true` and `ulimit -n 65536` are part
of the command after bug 1's EMFILE failure.

## Matrix

Oracle column is failed/total hidden tests; ✓ = livelock suite fixed.

| Profile | Treatment | Oracle | Livelock | jscout calls | Total tokens | Time |
|---|---|---:|---|---:|---:|---:|
| grep | control | 1/11 | ✓ | 0 | 4.27M | 13.5m |
| structural | skill | 2/11 | — | 3 | 4.10M | 12.8m |
| structural | forced | 1/11 | ✓ | 17 | 3.02M | 11.0m |
| checker | skill | 1/11 | ✓ | 0 | 2.78M | 11.3m |
| checker | forced | 1/11 | ✓ | 19 | 3.64M | 10.5m |
| checker-embed | skill | 2/11 | — | 6 | 2.52M | 8.9m |
| checker-embed | forced | 2/11 | — | 21 | 3.60M | 10.1m |
| checker-scout | skill | 2/11 | — | 6 | 2.35M | 8.0m |
| checker-scout | forced | 1/11 | ✓ | 31 | 3.93M | 10.1m |
| checker-scout-embed | skill | 1/11 | ✓ | 5 | 3.96M | 12.8m |
| checker-scout-embed | forced | 2/11 | — | 25 | 3.60M | 11.1m |
| production-order | skill | 2/11 | — | 2 | 2.07M | 5.9m |
| production-order | forced | 1/11 | ✓ | 55 | 6.18M | 14.8m |

These are one-seed descriptive results. Skill and forced treatments are not
pooled; nothing here is a stable treatment estimate.

## Preparation cost

| Stage | Wall | Result |
|---|---:|---|
| structural index | ~9s | 290 MiB |
| checker enrichment | 30.2m | 1,873 facts; 374 MiB |
| full embedding | 72.9m | 40,410 vectors; 807 MiB |
| repository scout | 4.9m | 64 calls, 200,788 tokens; tooling 30 / runtime 26 / test 7 / unknown 1 |
| product embedding after scout | 16.9m | 12,503 vectors (31% of full at 23% of its cost); 481 MiB |
| production-order full chain | 48.6m | index 0.2m → scout 5.1m → enrich 29.5m (1,831 facts) → product embed 13.8m (12,528 vectors) |

Full-embed timing carries a contention caveat: the bug-1 experiment shared
the local inference service during that window.

**Ordering effect (production-order vs additive):** scout-first visibly
changed checker scheduling — 7 projects received real purposes, 30
occurrences used the tooling fallback, 17 fewer projects and ~1,000 fewer
occurrences were scheduled, unmapped declarations fell 19,445 → 17,997 —
while `occurrences_avoided_by_tooling_filter` stayed 0 and product-embed
selection was unchanged (+0.2% vectors). The ordering matters operationally
but did not change any outcome on this task.

## Localization versus fix scope

The trial's central finding: **localization was not the binding constraint.**
jscout surfaced 5 of the 7 gold files to the embed arms (`cache.ts`,
`navigation.ts`, `create-initial-router-state.ts`, `ppr-navigations.ts`,
`optimistic-routes.ts`) and the agents declined to edit any but the last.
`decode-server-response.ts` surfaced in five arms (twice via jscout);
`route-params.ts` surfaced in two (once via jscout, in production-order/skill).
No sighting converted into a patch. What no arm ever produced is the *design
axis* of the reference fix — canonicalization plus divergence tracking — and
no retrieval surface can currently hand an agent that.

Localization quality still moved: checker/forced produced the cleanest arm
(single-file patch, zero extraneous, fastest turn, 5 of 7 gold files named in
its plan), and forced arms with scout policy were the most role-filtered.

## Behavior notes

- **Skill adoption is unreliable on hard tasks**: 0–6 calls across skill arms
  (checker/skill connected, listed tools, and never called one), against 5/5
  adoption on the easy calibration task. Terra goes heads-down into source
  when the task is hard — precisely where retrieval would matter.
- **Forced compliance was clean** (zero grep/rg in forced arms' commands;
  production-order/skill's 10 grep-like calls were a skill arm, allowed)
  and forced call volume did not track outcomes.
- **No arm completed browser e2e self-verification** (Chromium sandbox
  denials or stalls); every arm shipped on build/typecheck evidence and the
  grader adjudicated. One agent-turn EMFILE storm (2,361 Watchpack errors):
  the raised ulimit wraps setup/test commands, not the agent's inner login
  shells.
- Scout budget ordering: with `--max-calls 64` over ~372 subjects, tier-then-
  alphabetical planning spent the budget on `apps/`, `bench/`, `crates/`,
  `evals/` and skipped 308 subjects; no fresh classification covered the
  files agents touched, so no policy role reached any tool response. Subject
  ordering by weight (e.g. member count) would aim the bounded budget at the
  production surface.

## What this result supports

1. Keep this task in the slate as a discriminating hard case; run trial 002
   with counterbalanced order before drawing treatment conclusions.
2. Retrieval investment beyond the structural index shows localization gains
   but no oracle gains here; the failure mode it would need to attack is fix
   under-scoping, which is an agent-behavior problem more than an index one.
3. The two embed-only arms regressing the livelock, and the inverted
   dose-response, caution against reading tool-call volume as progress.
4. Blind adjudication of the six pending gold files per arm is required
   before any "missed file" is scored as a defect.
5. Harness items: subject-weight ordering for bounded scout budgets; ulimit
   inheritance into agent shells; the packed-tarball build dependency now
   encoded in the task's test command.

## Trial sol-001: same matrix, execution model gpt-5.6-sol

Run immediately after trial 001 with every prepared profile database cloned
byte-identically from it (sha256-verified) via a `--prepared-root` runner
flag — a deliberate, recorded deviation from the no-cross-trial-sharing rule
so that the execution model is the only variable. One arm ran at a time. Sol
admission: contamination probe clean (confident wrong guesses, zero gold
overlap), model id verified.

**Bottom line: 13/13 fail, identical to terra.** The rewrite-misprediction
case fails in all 26 arms of both trials at the same assertion — a property
of the task, not the model. What moves cross-model, on retrieval- and
oracle-side evidence unaffected by the confound below:

- **Livelock: sol fixes it in 11/13 arms vs terra's 7/13.** The embed-skill
  regression reproduced exactly (checker-embed/skill is the only arm to lose
  the livelock under both models on the same database); terra's other embed
  regressions did not reproduce.
- **Patched gold overlap stays 1/7 everywhere** except sol checker/forced,
  which reached 3/7 (`cache.ts`, `navigation.ts`, `optimistic-routes.ts`,
  1 extraneous) before hitting the 30-minute timeout — the best gold
  targeting of either trial, cut off mid-convergence.
- Sol names more gold files/symbols in its answers (10 of 13 paired arms)
  and produced zero extraneous patches on every scout/embed-bearing profile.
- **Adoption is ~2x terra at both treatment levels** (skill median 9 calls
  vs 4; forced 50 vs 23; maximum 98). `canonicaliz*`/`diverg*` markers:
  still zero in all 26 arms.

**Confound — do not read sol's costs as model differences.** Chromium could
not launch inside the sol arms' sandbox (Mach-port rendezvous denial): 24
blocked launches across 11 of 13 arms versus 3 across 2 terra arms. Sol
iterated blind against the oracle, which inflates its totals (101.9M tokens
vs terra's 46.1M, +60% wall clock, 2.5x failed commands, and the single
timeout). Layer1 grading ran outside the sandbox and is sound. The
environment must be fixed (or the denial equalized) before sol-vs-terra cost
comparisons mean anything.

## Trial 002: terra on the fixed harness, counterbalanced order

Run 2026-08-16 with the repaired harness (out-of-sandbox browser server +
`NEXT_TEST_BROWSER_WS_ENDPOINT`, fd-limit wrapper, dev-watcher and
run-tests.js prompt guards), profiles in reversed order, one arm at a time,
databases cloned from trial-001 (same recorded deviation).

| Profile | Treatment | Grade | jscout | Tool bytes | Fresh in | Cached in | Output | Time |
|---|---|---|---:|---:|---:|---:|---:|---:|
| production-order | skill | fail | 9 | 20,356 | 124,944 | 3,249,664 | 17,107 | 9.7m |
| production-order | forced | fail | 18 | 103,639 | 137,846 | 4,310,272 | 17,844 | 13.5m |
| checker-scout-embed | skill | fail | 2 | 16,215 | 110,626 | 2,194,432 | 18,548 | 8.6m |
| checker-scout-embed | forced | fail | 17 | 93,809 | 115,739 | 2,342,912 | 13,575 | 7.0m |
| checker-scout | skill | fail | 5 | 28,856 | 137,597 | 4,816,896 | 24,254 | 15.8m |
| checker-scout | forced | fail | 26 | 165,125 | 129,397 | 3,701,504 | 15,626 | 10.4m |
| checker-embed | skill | fail | 5 | 25,573 | 125,929 | 2,872,320 | 19,078 | 11.4m |
| checker-embed | forced | fail | 31 | 87,348 | 136,957 | 4,840,192 | 23,104 | 14.0m |
| checker | skill | fail | 0 | 0 | 103,909 | 1,578,496 | 17,849 | 7.9m |
| checker | forced | fail | 25 | 104,865 | 171,658 | 5,207,808 | 19,844 | 17.7m |
| structural | skill | fail | 4 | 18,012 | 276,855 | 4,758,784 | 21,850 | 22.3m |
| structural | forced | fail | 51 | 200,790 | 228,751 | 8,429,824 | 26,098 | 19.9m |
| grep | control | fail | 0 | 0 | 119,745 | 2,444,288 | 20,307 | 9.1m |

Totals: 0/13 pass; 193 jscout calls; 52.9M tokens (1.92M fresh in, 255k out);
167.5 agent-minutes.

What moved, and why it matters:

- **The rewrite-misprediction case is now 39/39 unfixed** across three trials
  and two models, always at the same assertion. It is the defining property
  of this task and a better future target than any profile in this matrix.
- **Livelock fixed 12/13, up from terra-001's 7/13, with adoption flat**
  (skill mean 4.2 vs 3.7 calls; forced mean identical at 28.0). The gain
  tracks the harness — a reachable browser and zero EMFILE (45,645
  occurrences in trial-001, 0 here) — not retrieval or feedback iteration.
- **The embed-skill livelock regression is retired**: on its third
  same-substrate run (identical cloned `checker-embed.db`), the arm won the
  case it lost under terra-001 and sol-001. Execution variance, not an
  embed effect.
- **Gold overlap broke its floor once**: structural/skill patched 3/7 gold
  files (`cache.ts`, `navigation.ts`, `optimistic-routes.ts`) — the only
  non-timeout arm across three trials to exceed 1/7. Markers remain zero in
  all 13 patches and answers.
- **Self-verification stayed hollow despite the fix**: real assertion
  results reached 7/13 arms (6/13 before). The blocker moved — 21 of 43
  in-arm e2e attempts hung in `pnpm test-start-turbo` teardown (120s
  `afterAll` in `next.destroy()`, 18 timeouts trial-wide) while the graded
  oracle path stayed clean at ~33s. And structurally: the hidden suites are
  vacuous green in-workspace and the pre-existing suites are green at the
  parent, so no in-arm run can discriminate the fix. One integrity note:
  checker/skill claimed e2e verification its own logs do not contain;
  grep/control described the same situation honestly.

Residual harness item for the next task: steer agents to the `pnpm
testonly` invocation (teardown-clean under connect) instead of
`test-start-turbo`, and check whether that teardown hang is connect-induced
or pre-existing.

## Artifacts

`~/git/jscout-replay-runs/next-optimistic-prefetch-2026-08-15/`: `task-set.json`,
`prepared/next-optimistic-prefetch/{pristine,gold}`, `contamination/`
(admission records), `trial-001/{responses.jsonl,telemetry.jsonl,artifacts/}`
with per-arm event streams, MCP request logs, patches, grades, and prepared
profile databases; `trial-sol-001/` mirrors the layout for the sol trial,
with `contamination/responses-sol.jsonl` recording sol's admission probe;
`trial-002/` holds the fixed-harness terra trial and `trial-sol-002-probe/`
the browser-fix validation arm; `trial-sol-mem-001/` … `trial-sol-mem-004/`
the memory trials (the two aborted preps kept for their logs). Scripts are
local copies of js-rag `scripts/eval-*` at `d138de4` with two recorded
changes (production-order profile, `--resume`); the memory trials instead
ran the runner from js-rag origin/main worktrees, recorded per trial in the
folder README. `trial-codex-*` folders are Codex's own runs, not covered by
this document.

## Blind omission adjudication (all 39 arms)

One blind gpt-5.6-sol judgment per arm (story + patch + untouched reference
diffs; no profile/treatment/trial/model identity). Unanimous across 230 file
judgments: `decode-server-response.ts` confirmed defect 39/39 and `cache.ts`
37/37 (the two arms that patched it still drew the decode omission); the
other four gold files — `create-initial-router-state.ts`,
`ppr-navigations.ts`, `navigation.ts`, `route-params.ts` — scored
`not_required` in every arm, and no file ever scored `alternative_covered`.
No arm was a genuine alternative implementation; 38 were partial (the
parallel-slot half only). The honest per-arm miss is therefore the two-file
divergence core (confirmed-omission 2/3), not "6 of 7 gold files": four of
seven were reference-design plumbing nobody needed. The blind judge
independently named the same missing mechanism the oracle test detects.
Records: `adjudication/` in the experiment folder (per-arm JSONL, grader-
compatible verdicts, prompts and event streams). Cost: 39 sequential calls,
16.15M in / 208K out, 84 minutes.

## Architecture probe: design instead of implement (one sol call)

One read-only gpt-5.6-sol call (11.2 min, 26.6k output tokens) given the
same anchor-free story but asked to design the fix. Verdict: split, and the
split is the finding.

Retrieval-reachable: the design reached the correct residual mechanism (the
synthetic prediction never fetches the tree that discovery would validate,
survives segment-level rejection, and is re-derived on retry), the correct
detection signal (server tree vs request tree, compared on segment identity
and parallel-slot structure — gold's two axes), the correct site
(`fetchSegmentPrefetchesUsingDynamicRequest`, before writing under predicted
keys), gold's exact cure semantics (immediate expiration, `-1`, not
backoff), named `cache.ts`, and independently rediscovered the parallel-slot
dynamic-child collision. The concept is not beyond the model: 39
implementation arms missing it reads as search/attention collapse under
implementation pressure, not a capability ceiling.

Conceptual gap: the canonicalization sub-fix (`canonicalizeURLPart` vs param
cache keys in `route-params.ts`) is absent in any form — the design believes
downstream response comparison covers that class, so no retrieval steering
would land it. The design also argues, by name, against the two pieces of
existing machinery gold reuses (`markRouteEntryAsDynamicRewrite`,
`invalidateRouteCacheEntries`), leaving sibling entries derived from a bad
template un-invalidated.

Implication for the product: the leverage on hard tasks may be a
design-before-edit workflow surface rather than more retrieval — the model
produces the mechanism when asked to design and loses it while implementing.
Artifacts: `trial-sol-003-arch-probe/` in the experiment folder.

## Memory trials: the semantic plane as a treatment (sol-mem-001 … 004)

Four trials asked whether a scouted semantic memory — workflows, cards, and
summaries written by jscout's own scouting over the workspace, never aimed
at the fix's subsystem — changes agent behavior on this task. All ran on
gpt-5.6-sol, one arm at a time. Two preps aborted and became upstream
fixes; the third ran vector-disabled and became another; the fourth is the
measurement.

- **sol-mem-001** (prep abort): one card of its 64-card batch failed claim
  validation (5 evidence ranges, cap 4) and the stage exit killed the whole
  prep — 96 billed calls discarded. Upstreamed: card citation repair and
  runner partial-failure tolerance (#39, #40).
- **sol-mem-002** (prep abort): a single gateway timeout at ~100 of 256
  cards poisoned the batch, and tier-first card ordering had spent the
  budget on examples/dev-infra endpoints. Upstreamed: remote timeouts are
  subject-local failed reports; card selection is weight-primary (#40).
- **sol-mem-003** (ran; invalid as a memory measurement): both arms
  completed on a 425-artifact plane, but the profile's stage plan carried
  no embed stage and the runner gates the arm's embedding provider on that
  same list, so every search reported `vector: disabled` — the plane
  existed and was mostly unreachable (skill arm: 0 segment-cache artifacts
  delivered). Upstreamed: #41 split the profiles (`memory` embed-free by
  design, `memory-embed` retrievable).

**sol-mem-004** re-ran the pair on a retrievable substrate with the corpus
held constant. The prepared database was grafted, not re-scouted:
`checker-scout-embed.db` as the base, sol-mem-003's semantic plane inserted
byte-identically (hash-verified; 425 artifacts = 314 cards / 63 workflows /
48 summaries), then migrated across the v23 boundary (post-#42 binary;
425/425 semantic embeddings; freshness verified per-support).
Retrievability is the treatment, not literally the only difference against
mem-003 — it arrives together with the #42 binary, tool surface, and skill,
and the grafted base carries trial-001's checker facts and product
embeddings. What the graft holds constant is artifact content, so delivery
differences are not corpus differences. Two unbilled gates ran before
any agent turn: the search preview envelope attaching fresh segment-cache
cards (pass) and the #42 `semantic_memory` drill-down returning 13 fresh
artifacts at the default budget (pass). Arms ran as profile `memory-embed`;
sol-mem-003's `memory-*` rows are the same artifacts, unretrievable.

Oracle is failed/total hidden tests; ✓ = livelock suite fixed.

| | mem-003 skill | mem-003 forced | mem-004 skill | mem-004 forced |
|---|---:|---:|---:|---:|
| Oracle | 1/11 | 1/11 | 1/11 | 1/11 |
| Livelock | ✓ | ✓ | ✓ | ✓ |
| Vector status | disabled | disabled | active 6/6 | active 24/24 |
| Gold matched | 1/7 | 1/7 | 1/7 | 1/7 |
| jscout calls | 7 | 53 | 8 | 36 |
| Distinct artifacts delivered | 10 | 30 | 21 | 42 |
| Segment-cache cards delivered | 0 | 1 | 3 | 2 |
| Total tokens | 14.91M | 28.56M | 13.78M | 18.96M |
| Time | 24.0m | 34.6m | 26.1m | 32.0m |

What the treatment changed — delivery, verifiably. Vector retrieval was
active on every call in both mem-004 arms, and the reranker on every search
(reranking is a search-pipeline stage; direct memory retrieval is
lexical+vector fusion without it). The `semantic_memory`
drill-down was called exactly once per arm, both times as the *first*
retrieval action (the #42 skill guidance fires; neither agent returned to
it once previews arrived); and segment-cache cards reached the agents
(`cache.ts#segmentCacheMap`, `cache.ts#upgradeToPendingSegment`,
`types.ts#PrefetchPriority`) where mem-003's skill arm got none.

What it did not change — the outcome, at all. Same single failing case
(rewrite misprediction, same assertion), same 1/7 gold overlap, and no
memory arm edited any source file but `optimistic-routes.ts` — every wider
patch is a self-authored e2e regression fixture. `canonicaliz*`/`diverg*`
markers: zero in all four arms, across events, patches, and answers. The
cross-trial constants now stand at 44 graded arms (three matrix trials, the
harness probe, four memory arms): oracle pass 0/44, the rewrite-
misprediction case unfixed 44/44, `optimistic-routes.ts` patched 44/44,
gold overlap 1/7 in 42 of 44. The four memory arms are not in the 39-arm
blind adjudication; given identical patch shape there is no reason to
expect different verdicts.

Plumbing is now eliminated as the retrieval-side limitation; the one that
remains is corpus power — a limitation, not the exclusive cause of the
failures. The plane holds no artifact naming `optimistic-routes.ts` —
automatic scouting never aimed at the fix's subsystem, and the file's low
incoming-reference weight (rank 2,353) keeps it outside any bounded card
budget — and only 3 of the 7 gold files carry any artifact. Of those, only `segment-cache/cache.ts` ever
surfaced to an agent; `navigation.ts` and `ppr-navigations.ts` never did.
The retrieval machinery is now demonstrably live end-to-end; what it
retrieves over is adjacent context. Read with the architecture probe, the
leverage question shifts from "can the agent reach memory" (yes) to what
the scout writes into it — mechanism-level artifacts on the production
surface — and whether any retrieved context competes with implementation
pressure once editing begins.

Corroboration (`trial-codex-memory-vector-006/RESULTS.md`): Codex ran
skill-only memory-embed arms on terra and sol with an independently
prepared workspace, checker, and structural plane but the byte-identical
425-artifact semantic plane (verified here: artifact row sets hash equal
across the two databases; its trial-005 was invalidated by the same
v22→v23 snapshot trap recorded above). Both its models adopted jscout
naturally (7–8 calls, no forcing), vector active throughout — and both
landed at 1/7 gold, `optimistic-routes.ts` only, rewrite misprediction
unfixed; its terra arm also dropped the livelock, within terra's
documented variance on that case. The cross-experiment constants are now
46/46 on the same corpus, and its conclusion matches this section's
independently: memory was "non-empty but generic," and neither budgets
nor embedding coverage are the lever. Its new finding, bounded by this
trial's data: both its models invented shorthand drill-down arguments
(`optimistic-routes:matchKnownRoute`, zero targets) where this trial's
sol arms copied full `path:symbol` anchors and resolved 8 of 9
definition calls to source — the failure is real but non-deterministic,
and its proposed fix (emit copy-ready tool arguments beside each hit)
would also cover this trial's one miss, an unqualified class-member
anchor. Its second recommendation — `repository_overview` is now the
largest automatic response — reproduces here (21.5KB and 17.4KB in the
two mem-004 arms).
