# Next.js Astra instruction comparison

Registered before solver dispatch, 2026-09-06. This follows the six-attempt
[September 5 report](../results/next-astra-full-scout-2026-09-05.md). The user
requested a latest-main build, upgrades of copied databases reusing valid vectors
and scouting, five conditions on the existing cases, and consolidated findings.
New cases and new scouting are outside this batch.

## Fixed design

- Product source: main `9947f4d` (full commit and release SHA-256 in the external
  launch manifest). Only the replay harness and this registration are changed.
- Solver: `gpt-6-astra`, high reasoning; CLI 0.153.4. One fresh single-phase
  attempt per case/condition, 3,600-second allowance each. Ten attempts total;
  no silent solver retries or substitution of another model.
- Parents: prefetch `7cb68c12828a758492ea54251393b4f988aecd6e`; root params
  `1d8e326d1b360da4a439cf440316fe76a359bfd3`.
- Unchanged stories, visible constraints, original hidden oracle and task-native
  dependency pins. Each solver gets a history-free synthetic workspace, no
  external solution lookup, and no prior results, patches or supplemental tests.
- All four indexed conditions get independent copies of the same upgraded
  memory-enabled database for that case, with the same current full guide,
  code vectors, reranker and semantic memory available. Thus instruction
  comparisons do not also add/remove memory data.

| Condition | Runner profile / treatment | Instruction difference |
| --- | --- | --- |
| Control | `grep` / `control` | No jscout server or skill |
| Current | `memory-embed` / `skill` | Current full guide; no extra requirement |
| Explicit memory | `memory-embed` / `memory` | Relevant anchored memory inquiry, inspect a relevant returned body, verify claims in source; record empty results |
| Efficiency | `memory-embed` / `efficiency` | Exact/scoped discovery, avoid repeated adequate unchanged source, preserve context/caller checks and correctness testing |
| No-`rg` | `memory-embed` / `forced` | Existing forced-search contract: jscout for repository-wide discovery, symbol lookup and navigation; no `rg`/grep/find/custom scan substitutes; targeted reads of identified files, edits and test/build output inspection allowed |

Memory and efficiency instructions are independent; the no-`rg` arm does not
also inherit either intervention. The restriction is prompt-based and audited
from commands, not claimed to be enforced by an OS sandbox. Report violations.
Tool counts or claimed memory use in the final answer are not enough: check
actual requests and returned artifacts.

Serial order: prefetch control/current/memory/efficiency/no-`rg`, then root params
no-`rg`/efficiency/memory/current/control. Reversed order limits a simple ordering
confound across cases but is not replication or statistical counterbalancing.
All conditions retain native build/test access and the original 3,600-second
budget; no case-specific solution hint or reduced testing requirement is added.

## Database upgrade and reuse

Original September 5 `memory-embed` databases remain read-only. Verify their
recorded hashes, clone them, rebuild the exact parent source environment, then
run current index, checker enrichment and missing-vector synchronization.
Retain cached vector bytes and scouted artifacts wherever their content/evidence
remains valid. Do not manually mark stale artifacts fresh or rewrite provenance.
Record before/after counts, persisted-data retention and current availability.

No generative scouting is rerun. Existing scouting used Astra/low; its original
partial/failed preparation statuses remain recorded. Local inference reuses the
same BGE-M3 and BGE reranker v2 M3 revisions and configuration on a separate owned
loopback service. Missing text may be embedded locally; report that count rather
than calling every vector reused. Upgrade/setup work is outside solver time.

## Measurement and grading

Retain prompts and guide hashes, product/runner/task/data hashes, events, request
streams, telemetry, patches, original grades and process-cleanup evidence. Save
event arrival timestamps separately without modifying raw events. Arrival time
is not backend execution time, and does not repair absent upstream output.

Report each attempt's original correctness, stronger root-params supplemental
outcomes, duration, fresh/cached/output tokens and actual treatment use. Attempt
duration includes investigation, edits and tests but excludes preparation and
external grading. Since only final patches are externally graded, do not label
it first-time-to-solve. Reconcile cumulative usage once.

After all solvers finish, apply the same authored supplemental root-params suite
to all five saved patches. Preserve original grades alongside it. The suite
tests actual typed calls, rejects `any`, and covers single/multiple/parameterless
roots and removal; dev uses a fresh server per state, not a live hot-reload claim.
Never give supplemental feedback to a solver or revise the oracle to favor an arm.

Audit repository discovery versus tests/logs/edits, broad queries, repeated
locators/source, errors, degradation, memory artifacts read and no-`rg`
compliance. Keep source-duplication estimates bounded by actual capture; do not
reconstruct intended output as observed content. Preserve infrastructure failures
separately from failed behavior; stop and inspect before any recovery, retain the
failed attempt, and do not silently expand the ten-solver matrix.

One attempt per cell is descriptive. Compare instruction arms with their
same-build current-guide arm, not just the historical binary. Any historical
comparison also changes software and possibly environment; it cannot isolate
the effect of either a fix or an instruction. More testing effort can improve a
patch and must not automatically be labeled retrieval overhead.

The OpenAI Docs skill informed the prompt-isolation and instruction-conflict
checks; the experiment retains the explicitly requested model and reasoning.
See [Astra prompting guidance](https://developers.openai.com/api/docs/guides/latest-model#prompting-best-practices).
It is guidance for the setup, not evidence that a treatment will succeed.

External artifact root:
`jscout-replay-runs/next-astra-instructions-2026-09-06.zIALfv/`.
