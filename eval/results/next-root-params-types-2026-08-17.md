# Next.js PR-replay: root-layout parameter types (feature)

Execution: 2026-08-17/18
Task: `next-root-params-types` (plan's feature candidate; feature #91019)
Base `1d8e326` (direct parent) · Reference `46e2114` · 10 production files
(9 code + `root-params.d.ts`), 4 test files
Agent: `gpt-5.6-sol`, high reasoning · jscout at origin/main `3517653`
(binary sha256 `9894cf36…2584`)
Design: reduced matrix — grep control, `checker-scout-embed`/skill,
`memory-embed`/skill; no forced arms; two counterbalanced trials sharing one
prepared-database set. Raised generative budgets: scout 96, workflows 96,
cards 448, summaries 64.

## Outcome

- **First oracle-level treatment separation in this program.** Across two
  counterbalanced trials on byte-shared substrates:
  `checker-scout-embed`/skill **passed layer1 in 2/2 trials**; the grep
  control **failed 2/2 with the identical signature** (simple fixture
  passes; the multi-root union/optional fixture dies at its validation
  build). One execution model, one seed per arm per trial — descriptive,
  not a treatment estimate, but the direction repeated under order
  reversal.
- **memory-embed split 1/2**, and the split inverts the naive reading: the
  passing arm (trial A) received **zero** semantic artifacts; the failing
  arm (trial B) is the only arm of six that received memory content — six
  fresh cards — and the only one that mirrored the reference's file
  architecture. The blind influence adjudication found the delivery was
  followed (details below).
- The control's stable failure is the feature's hard half: union across
  multiple root layouts with optional marking for params absent from some
  roots.
- All six arms converged on the same alternative implementation site —
  extending the existing `typegen.ts`/types-plugin path rather than
  creating the reference's `root-params-type-utils.ts` (B1 excepted) — and
  the blind omission adjudication scores that alternative as behaviorally
  legitimate: one arm graded `genuine_alternative`, five
  `partial_alternative`, and the reference's `build/index.ts` hook scored
  `alternative_covered` in every arm that was judged on it.

## Task selection: contamination burned the bug slate

The middleware request-stream task (#95607) was fully admitted first —
bounded two-suite oracle proven fail-to-pass, browserless e2e — and then
died at the pre-registered contamination gate: with no tools and no
repository, sol reproduced `packages/next/src/server/body-streams.ts` plus
`getCloneableBody`/`cloneBodyStream` from memory; a terra probe then failed
the same way (exact file, `cloneBodyStream`, and a confabulated near-name
for `getCloneableBody`). Probes of the remaining slate: tag invalidation —
contaminated by rule (single-file hit on `use-cache-wrapper.ts`, the
subsystem entry point; zero fix-symbol overlap; caveat recorded);
PPR-fallback actions — clearly contaminated (2/3 fix files plus
`handleAction`). **Only the root-params feature probed clean** ("I do not
remember", empty lists, against a stop-list it never approached).

Program lesson: famous bugs with public reproductions are memorized;
PR-replay bug tasks age out as training data catches up with repository
history. Future slates need post-cutoff bugs; features survive longer.
Records: `contamination/` in both experiment folders and
`candidate-probes-2026-08-17/`; the middleware folder retains its full
admission evidence as the double-contamination record.

## Admission

Story: the plan's feature story verbatim; anchor-certified `weak`, zero
identifier anchors. Oracle: the reference's e2e typecheck suite (start
mode, `readFile` + `tsc --noEmit` assertions — no browser, no dev
watchers), bounded both ways: parent fails in 37.8s at the oracle
mechanism, reference passes 2/2 in 64.8s. The reference's unit suite
passes on the parent (its change is signature-compat only), so the e2e
carries the behavioral contract; the unit suite stays in the grade command
and out of the agent-visible one (its path names a fix file). Two harness
backports required by the parent's age, both evidenced in `gates/`: pnpm
pinned to 10.33 (pnpm 11's `ERR_PNPM_IGNORED_BUILDS` breaks
`createNextInstall` on both sides) and typescript 6.0.3/@types/node 26.1.0
pinned (the two-line backport of upstream #95619; TS7 crashes both sides
identically without it).

## Preparation and the power note

One chain, runner-driven, local embeddings: index 7s → enrich 28m16s
(572/572 projects) → scout 8m56s (96 calls, 385 subjects, 289 skipped) →
product embed 13m00s (11,657 vectors) → workflows ~24m (96 calls, 93
artifacts) → cards 1h07m (448 calls, 3 tolerated failures, 445 artifacts,
576 subjects skipped) → summaries 5m12s (64) → semantic embed 20s
(602/602). 704 billed calls. One external interruption mid-workflows was
recovered losslessly through the runner's own `--resume`. Both databases
manifested; re-hashed byte-identical after each trial.

**Power note, measured before any arm: the corpus is blind on the fix
surface.** Of 602 artifacts, exactly one cites any gold production file
(peripheral `setup-dev-bundler.ts#propagateServerField`); none cites
`typegen.ts`, `route-types-utils.ts`, `writeAppTypeDeclarations.ts`, or
`type-check.ts`; the broad type-generation surface totals 6/602. The
fix-surface subjects rank top-10 by weight in their area
(`writeAppTypeDeclarations` 7, `extractRouteParams` 6) and still fell
below the 448-card cutoff. Second consecutive task with this shape:
whole-repository weight-ranked scouting exhausts its budget before
reaching the fix surface even at +40% budgets — a scout-selection finding
independent of any arm outcome. Nearest analog in the plane: card #421,
`generateCacheLifeTypes` (the same generate-types pattern for a different
feature).

## Trials

Layer1 is the agent-visible e2e; the full grade command (build + unit +
e2e) confirmed every verdict offline (one flaky offline re-grade traced to
machine contention and resolved by a clean re-run; both runs preserved).
Reuse was verified in all four jscout arms (`reused_prepared: true`, zero
stages); vector and reranker active on every retrieval call; skill hash
intact.

| Arm | Trial | layer1 | gold/9 | jscout calls | artifacts delivered | time | total tokens |
|---|---|---|---:|---:|---:|---:|---:|
| grep | A | fail | 2 | 0 | — | 24.6m | 14.04M |
| grep | B | fail | 4 | 0 | — | 13.7m | 8.50M |
| checker-scout-embed | A | **pass** | 2 | 9 | 0 | 17.3m | 11.88M |
| checker-scout-embed | B | **pass** | 2 | 8 | 0 | 19.3m | 12.45M |
| memory-embed | A | **pass** | 4 | 8 | 0 | 14.1m | 9.65M |
| memory-embed | B | fail | 2 | 6 | **6 cards** | 17.4m | 10.09M |

B1's failure mode is distinct from the control's: both fixtures timed out
waiting for `.next/types/root-params.d.ts` — the generated declarations
file never appeared at build. It authored the reference's
`root-params-type-utils.ts` (alone among six arms) but never wired
emission and registration.

## Blind omission adjudication (6 arms)

One blind sol judgment per arm (story + patch + untouched reference diffs;
no identity labels). Overall: five `partial_alternative`, one
`genuine_alternative` (checker-scout-embed/trial A, whose generated
`routes.d.ts` conditionally imports `./root-params.d.ts`, covering
registration transitively). Per-file: `build/index.ts` is nobody's
omission (`alternative_covered` 4/4 judged — everyone hooked
`writeRouteTypesManifest`); `cli/next-test.ts` splits 3/3
omission/not_required. The real shared miss, confirmed in 5 of 6 arms:
**TypeScript registration of the generated declarations (the
`next-env.d.ts` import path) plus experimental feature gating** — carried
behavior, not file layout. The reference's structural choices are not
themselves the bar.

## Causal-chain adjudication (memory)

Corpus judge (blind, mechanical 16-candidate prefilter recorded):
**"none"** — no artifact describes discovery, collection,
union/optionality, or declaration emission for root-layout parameters;
#421 is analogous, not part of the chain. This independently confirms the
power note through a judge rather than SQL.

B1 influence judge (blind to outcomes; delivered cards + patch + all 44
cache-life transcript excerpts): **delivery was followed** — the
`generateCacheLifeTypes` card arrived in excerpt 14, and excerpts 38/21/31
show the agent `sed`-reading `cache-life-type-utils.ts` and inspecting its
build/dev/typegen call sites; **design similarity strong** (paired
`generate<X>Types`/`write<X>Types` naming, router-utils placement, ambient
module declarations, `.next/types` emission); **attribution supported**
("reproduces several specific structural choices beyond the generic fact
that both emit declarations"). Combined with the corpus verdict: the one
delivery event in six arms steered *form* — toward the analog's (and
coincidentally the reference's) heavier file architecture — while the
missing *behavior* the omission layer identified (registration wiring)
went unfilled, and that arm failed. One seed. Recorded as the
architectural-anchoring hypothesis, not a conclusion: retrieved pattern
analogs may bias design toward heavier structures without supplying the
behavior that makes them work.

## What this result supports

1. On a mid-band feature task, full retrieval separated from the control
   at the oracle level in both counterbalanced trials. Worth a third seed
   and a second mid-band task before any stronger claim; the direction is
   the first of its kind in this program.
2. The control's repeated failure on multi-root union/optional — while
   both retrieval arms cleared it — is the concrete mechanism to study in
   the event streams: what did retrieval surface that grep didn't reach?
3. Automatic scouting missed the fix surface twice in a row despite raised
   budgets and correct weight-ordering. If the memory plane is to matter
   on tasks like these, the lever is selection policy (or targeted
   scouting surfaces), not budget.
4. The single memory-delivery event influenced architecture without
   supplying behavior. Before building more delivery machinery, test
   whether analog-shaped artifacts help or anchor: a paired trial where
   the analog card is present vs absent on an otherwise identical
   substrate would answer it directly.
5. Slate construction: probe contamination *before* admission work (the
   middleware task cost a full admission before its probe), and prefer
   post-cutoff bugs; three of four slate bugs were burned.

## Artifacts

`~/git/jscout-replay-runs/next-root-params-types-2026-08-17/`:
`task-set.json`, `prepared/next-root-params-types/{pristine,gold}` +
`reference.patch` + `harness-pins.patch`, `gates/` (admission + retrieval
gates + power note), `contamination/` (ported clean sol probe),
`trial-a/`, `trial-b/` (responses, telemetry, per-arm artifacts, prepared
databases + manifests), `adjudication/` (unit-overlay, omission,
causal-chain: prompts, streams, verdicts). Candidate-selection probes:
`~/git/jscout-replay-runs/candidate-probes-2026-08-17/`. The dead
middleware admission: `~/git/jscout-replay-runs/next-middleware-stream-2026-08-17/`.
Scripts pinned byte-for-byte at js-rag origin/main `3517653` with recorded
additions only (`codex-prep-stub.sh`, staged schemas, `gold-code.patch`).
