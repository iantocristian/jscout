# Next.js Astra instruction comparison — September 6, 2026

Completed: ten solver attempts and 63 supplemental states, with no solver or
scouting reruns. [Machine results](next-astra-instructions-2026-09-06.json) retain
exact measurements and evidence hashes. No product behavior or defaults change.

This implements the thread's requested follow-ups: build current main, upgrade
copies of the existing databases, reuse valid vectors/scouting, rerun the two
existing cases under five instruction conditions, and consolidate the outcomes.
It does **not** execute PR #123's numbered proposals or introduce new cases.
See the [preregistration](../prereg/next-astra-instructions-2026-09-06.md) and the
[September 5 comparison](next-astra-full-scout-2026-09-05.md).

## Outcomes and effort

**This is not an overall jscout win on these cases.** Every indexed attempt uses
more fresh input than its same-case control. Prefetch remains 10/11 in all five
arms. On root params, control, efficiency and no-`rg` pass all 12 supplemental
states; current and explicit memory pass only 3. Efficiency is the most promising
instruction candidate here, but it does not dominate control on time and tokens.

| Case / condition | Original oracle | Supplemental | Attempt minutes | Fresh input | Cached input | Output |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Prefetch / control | Fail, 10/11 | — | 9.50 | 106,812 | 2,352,512 | 13,408 |
| Prefetch / current | Fail, 10/11 | — | 14.20 | 142,984 | 5,130,880 | 21,670 |
| Prefetch / memory | Fail, 10/11 | — | 14.82 | 151,161 | 6,505,088 | 22,698 |
| Prefetch / efficiency | Fail, 10/11 | — | 13.43 | 136,384 | 4,205,568 | 20,524 |
| Prefetch / no-rg | Fail, 10/11 | — | 22.50 | 344,211 | 7,765,120 | 33,040 |
| Root params / control | Pass | 12/12 | 15.13 | 118,582 | 3,128,192 | 21,335 |
| Root params / current | Pass | 3/12 | 16.76 | 158,697 | 4,174,080 | 24,558 |
| Root params / memory | Pass | 3/12 | 13.71 | 133,306 | 3,800,448 | 19,505 |
| Root params / efficiency | Pass | 12/12 | 13.94 | 126,460 | 3,621,504 | 19,834 |
| Root params / no-rg | Pass | 12/12 | 21.84 | 190,678 | 6,494,336 | 32,352 |

On prefetch, efficiency used 4.6% less fresh input and 5.4% less time than the
same-build current guide. No-`rg` used 141% more fresh input and 58% more time.
Neither improved correctness. On root params, efficiency used 20.3% less fresh
input and 16.8% less time than current; versus control, it was 7.8% faster but
used 6.6% more fresh input, with the same 12/12 supplemental correctness. Testing
workloads differ, and one attempt per cell cannot attribute every difference to
the instruction.

No-`rg` is the most expensive condition on both cases. It matches control's
root-params correctness but takes 44% longer and uses 61% more fresh input;
it does not improve prefetch correctness. This sample does not justify making
the restriction a general default.

The ten attempts consumed **155.84 solver minutes**, **1,609,275 fresh input**,
47,177,728 cached input and 228,924 output tokens. Preparation and grading are
additional; these totals are not the entire campaign's wall time or a dollar bill.

Attempt time includes investigation, editing and the solver's own tests, but
excludes preparation and external grading. Only final patches are graded, so
**first-time-to-solve remains unknown**. Fresh input is cumulative input minus
cached input; output already includes reasoning. Cached input is repeated context,
not that many distinct source tokens. Counts are not dollar charges.

## Fixed comparison

- Product: main `9947f4da26964ec6ea7dd700557a21c694aa6f06`, release SHA-256
  `332a614077b1068f5d66ec5147ca6a13e22d2255ea17ab4071cfbbf2c2875967`.
- Solver: `gpt-6-astra`, high reasoning, CLI 0.153.4. One single-phase attempt
  per cell, with the same 3,600-second allowance. No solver retries.
- Prefetch parent: `7cb68c12828a758492ea54251393b4f988aecd6e`.
  Root-params parent: `1d8e326d1b360da4a439cf440316fe76a359bfd3`.
- Same stories, source parents, task-native dependency pins and original hidden
  checks. Solvers get history-free workspaces, no prior patches or results, and
  no hidden-test feedback. The root grader honors its separate hidden unit-plus-
  e2e command without revealing the hidden unit filename in the solver prompt.
- All four indexed conditions receive independent copies of the **same
  memory-enabled database** per case. Data availability does not vary with the
  instructions. Control has no jscout server or guide.
- The runner serves that prepared parent index without starting a watcher or
  reindexing solver edits. Changed/new source must be read directly; this is not
  an evaluation of a live-watch editing workflow. A search miss for newly authored
  code is not evidence that initial repository discovery failed.
- Prefetch order: control/current/memory/efficiency/no-`rg`; root order reversed.
  One attempt per cell is descriptive, not statistical counterbalancing.

| Condition | Difference from current guide |
| --- | --- |
| Control | No jscout |
| Current | Full current guide, no extra requirement |
| Explicit memory | Relevant anchored memory inquiry; read a relevant returned body and verify decisive claims; disclose empty results |
| Efficiency | Exact/scoped discovery, reuse adequate unchanged source, preserve caller checks and correctness testing |
| No-`rg` | jscout-only repository discovery/navigation; no rg/grep/find/custom scan substitutes; targeted reads, edits and native test/log inspection remain allowed |

These instruction interventions are independent. No-`rg` does not also inherit
memory/efficiency instructions. Its restriction is prompt-based, not OS-enforced;
actual recorded commands must be audited. An `rg` mention in a log-filtering
command is not automatically a violation.

## Reused data and preparation accounting

Original September 5 databases are unchanged. Extraction upgrades from 7 to 8;
current indexing and checker enrichment run on copies. No generative scouting
is repeated. Final synchronization needs **zero new code or memory vectors**.
Original vector bytes, memory artifacts, supports, relations, scouting runs and
repository classifications are retained and verified.

| Case | Cached code texts | Code vector occurrences | Memory vectors | Fresh / degraded / stale artifacts | Current scope policies / governed files |
| --- | ---: | ---: | ---: | --- | --- |
| Prefetch | 13,775 | 16,584 | 587 | 552 / 28 / 7 | 48 / 18,890 |
| Root params | 12,869 | 15,409 | 584 | 541 / 36 / 7 | 51 / 18,044 |

Counts match the original vector/policy substrate. The old installed guide is
excluded from prepared exports; each indexed solver gets the current full guide
installed separately. Otherwise indexed file hashes match the original inventory.
Local BGE-M3 and BGE reranker v2 M3 retain their original pinned revisions and
MPS/float16 configuration, on a separate owned service.

Preparation had corrections, not a clean first pass:

1. The outer sandbox denied Chromium startup and selected CPU/float32 instead
   of MPS/float16. The wrong-profile embedding jobs were stopped; native setup
   and the owned inference service restored the matching profile. Solvers still
   run in their own workspace sandbox.
2. The initial prefetch export lacked synthetic Git metadata, disabling
   Git-dependent ignores. A source-inventory audit caught generated codemod
   files **before any indexed solver**. That export was reconstructed with the
   correct ignore semantics; root params already had synthetic Git metadata.
3. Mistaken preparation temporarily added 1,489 float16 prefetch cache entries
   and 96/160 float32 entries. Evaluation copies were backed up, extra entries
   removed, and current `embed --product --semantic --repair` restored the exact
   original vector cache/materialization. Final input hashes and counts are in
   `readiness-final.json`, superseding initial preparation records.
4. The completed prefetch control overlapped that repair. A deliberately withheld
   manifest stopped the first indexed arm during setup, before model invocation;
   continuation skipped the completed control and ran the remaining nine.
   **The control is not a host-contention-controlled timing baseline.** No extra
   solver attempt or feedback was introduced.
5. A task-independent reranker smoke ran during the first indexed solver, not
   before it. The first actual query includes 5.452 seconds of cold reranker time.

Logs, failed setup records, originals, final copies and pre-correction backups
are retained. Disposable preparation/build trees are removed to bound disk use.
The previous scouting expense is inherited, not incurred again or treated as free
product preparation: its accounting remains in the September 5 report.

The successful parent-source preparation stages give a useful cost breakdown:

| Case | Dependency/build setup | Index | Checker enrichment | Final vector repair |
| --- | ---: | ---: | ---: | ---: |
| Prefetch, corrected pass | 55.0 s | 26.2 s | 619.9 s | 8.7 s |
| Root params | 81.4 s | 21.7 s | 583.8 s | 5.6 s |

Checker work dominates this upgrade, not embedding. These are recorded command
durations, not isolated benchmarks or the entire preparation bill. Initial failed
passes, audits and correction work remain additional; preparation from the first
copy to final readiness occupied about 39 minutes of wall time with overlapping
work. The release build itself took 44.0 seconds. Future solver arms clone the
prepared database; they do not repeat this enrichment or generative scouting.

## Actual memory coverage

The prefetch explicit-memory solver queried `cache.ts` with the causal question
`optimistic route prefetch rewrite catch-all retry`. It received
`no_supported_memory`, disclosed that result and continued with source. No memory
artifact body was returned or read, so this attempt establishes instruction use,
not a benefit from reading generated knowledge.

Read-only inspection finds **zero support rows for that file in both original
and upgraded databases**. The segment-cache directory has direct artifact supports
only for `cache-map.ts` and `types.ts`, one each—not `optimistic-routes.ts`,
`scheduler.ts` or `navigation.ts`. This inquiry has a pre-existing coverage hole;
it is not merely a keyword mismatch or an upgrade loss. It does not establish
that every possible inquiry would return nothing useful.

The retained card log records 448 completed calls and no failed card subjects,
but 575 selected subjects skipped by call budget. The Next package received 184
calls from 3,339 discovered subjects; examples received 183 from 2,039. These are
card-stage counts, not all memory. They motivate examining allocation, but do not
identify the exact omission cause of each unsupported file.

The root-params explicit-memory arm did receive and open the workflow artifact
“Webpack App Router type-guard generation.” Its source reads cover the supported
webpack configuration and plugin ranges; it explicitly compares the workflow
description with current source before choosing the shared writer. That is actual
body use plus source verification, not merely a memory-call count. Its original
oracle passes, but supplemental grading is only 3/12. Actual memory use did not
produce a correct patch under the stronger suite in this attempt.

## Observed discovery and delivery

| Case / condition | Shell batches | Repository / log-only / runtime rg | jscout calls | Canonical jscout bytes | Captured source / repeated / cross-channel repeat bytes |
| --- | ---: | --- | ---: | ---: | --- |
| Prefetch / control | 36 | 18 / 1 / 0 | 0 | 0 | 164,499 / 24,043 / 0 |
| Prefetch / current | 64 | 13 / 6 / 0 | 7 | 27,504 | 188,807 / 17,368 / 239 |
| Prefetch / memory | 73 | 13 / 14 / 0 | 11 | 26,823 | 146,396 / 25,334 / 633 |
| Prefetch / efficiency | 56 | 8 / 5 / 0 | 8 | 19,929 | 138,512 / 693 / 302 |
| Prefetch / no-rg | 85 | 0 / 0 / 0 | 44 | 155,249 | 198,699 / 11,515 / 3,716 |
| Root / control | 65 | 23 / 0 / 1 | 0 | 0 | 105,097 / 4,669 / 0 |
| Root / current | 63 | 15 / 4 / 0 | 7 | 25,221 | 100,138 / 4,817 / 1,298 |
| Root / memory | 49 | 15 / 1 / 0 | 13 | 26,508 | 128,803 / 13,942 / 1,298 |
| Root / efficiency | 47 | 9 / 0 / 2 | 14 | 45,831 | 123,453 / 5,577 / 1,633 |
| Root / no-rg | 79 | 0 / 0 / 0 | 46 | 124,608 | 122,033 / 9,523 / 3,383 |

Process checks, `test/tmp` and explicit built declarations are separated from
authored source discovery. A mixed batch that also searches authored source stays
in the repository category. Log-only `rg` counts do not count every log read:
many use `cat` or `tail`. Each shell batch has one reviewed purpose in the machine
results: investigation, setup, execution, editing, review, logs, orientation or
mixed. Mixed purposes are not apportioned.

The 150 jscout calls return 451,673 canonical bytes and total 90.827 seconds of
server time, with no failed calls or provider degradation. That does not measure
client/model round-trip cost. Root efficiency uses more jscout calls and bytes
than current while consuming fewer fresh tokens; tool-output bytes alone do not
explain total effort.

Source figures are conservative captured-source lower bounds, not complete
delivery or wasted-token estimates. The completed indexed arms do not show large
cross-channel duplication of identical source. No-`rg` reduced that channel of
filesystem discovery but did not reduce overall shell work, retrieval or tokens.
The prefetch no-`rg` arm's 44 calls included eight definitions, six outlines and two docs searches;
backend service time totaled 35.122 seconds. Many other command batches debugged
regression fixtures that initially masked the loop, then rebuilt and tested.

No forbidden repository scan is observed in the no-`rg` trace: no rg/grep/find,
git grep/ls-files or custom recursive discovery. Python filtered test logs; Node
checked patch markers in two explicitly named built files. However, the solver
also directly probed fixed ancestor READMEs and adjacent config/fixture files:
**not every read path was individually returned by jscout**. The no-scanning
intervention held; do not claim literal perfection on the stricter path wording.

README-oriented documentation queries returned unrelated guides and fixture
READMEs before those direct probes. The docs tool exposes text queries without
an exact path/prefix filter. This is evidence of navigation friction, not proof
that desired files were absent or that all docs queries are useless.

The prefetch test-generator wrapper failed across these attempts. Efficiency and
no-`rg` eventually invoked the installed node-plop generator directly; others
authored fixtures. This setup work, and the differing baseline/encoded-path/
Turbopack/Webpack checks, should not all be charged to retrieval overhead.

## Supplemental checks and interpretation

The repaired September 5 root-params suite was frozen before any root solver and
ran only after all ten attempts finished. It applied identical checks to all five
saved root patches, preserving original grades: 63 states—three parent
negatives plus build/dev/typegen × single/multiple/parameterless/removed roots
for each patch. Actual typed calls reject `any`, `unknown` and `never`; generated
state is preserved across transitions. Dev restarts per state, not live hot reload.
All states completed without infrastructure-inconclusive results in 15.21 minutes.
All three parent negatives failed as expected. Strict checking, loaded assertions,
TypeScript 6.0.3 and 45 retained declaration records were independently verified.

Current and explicit memory fail the nine states that retain parameters, and
pass the three removal states. The loaded strict TypeScript assertions reject
the actual call results as `any`; emitted declaration text alone was not enough.
Both patches leave the permissive `next/root-params` placeholder unchanged.
The explicit-memory trace actually contains that placeholder's source before
editing; its failure is not explained by never receiving the file.
Control's own application-level test caught that problem after its initial unit
tests passed, and its final patch passes the stronger suite. The failure is not
missing dependencies, a native crash or a provider failure.

Source repetition uses the same conservative four-line baseline fingerprinting
as the previous audit, also applied to authored docs-tool content in these runs.
It covers baseline code, configuration and docs; generated memory is separate.
Missing capture, short/ambiguous matches and changed source
are excluded. Repeat-channel attribution is relative to each normalized line's
first mapped delivery, not every pair of deliveries. Repeated source and repeated
locators are **not automatic waste**;
a caller check answers a different question than a definition read. Shell totals
include setup, editing, tests and logs, not just discovery.

Passive event-arrival timestamps are retained separately without changing raw
events. They are not backend timings and cannot repair empty captured output.
Per-purpose command intervals may overlap and mixed batches are not apportioned.
Canonical bytes count one result body: use the search-specific metric where
present and serialized `result_bytes` for other tools, reconciled against captured
results. Transport bytes are separate; duplicate wire representations do not
establish how the client assembled model context. These ephemeral solver runs do
not retain a separate model-input trace to settle that question.

Same-build instruction comparisons are primary. Historical comparisons also
change software and environment and cannot isolate a particular merged fix.
Different verification workloads must not all be labeled retrieval overhead.

The no-`rg` prefetch trace exercises full definitions, including the 12,864-byte
`fetchSegmentPrefetchesUsingDynamicRequest` body. Independent TypeScript AST
checks confirm that six actual returned definitions match the frozen source and
reach their function-body ends. This verifies useful retrieval behavior in a
solver session, not a causal improvement in its final patch.

The root current and memory arms also issued an identical ranked query. Their
4,953-byte canonical responses are identical. This is observed repeatability,
not an isolated test of the fusion-score tie condition.

All original and prepared database hashes remain unchanged after the runs. The
temporary inference service was intentionally stopped after solver grading;
unrelated services were untouched. Native test trees were removed after retaining
patches and declaration evidence, with no tracked test processes left running.
An exact cross-cell workspace-path screen found no matches in captured commands,
output or answers. Shared caches/log locations and missing capture mean this is
not a claim of hermetic host isolation.

## Historical comparison, not a binary ablation

The closest September 5 indexed comparison is `memory-embed/skill`, called
“current” here—not the old `checker-scout-embed` arm, which lacked memory data.

| Case / arm | Sep 5 → Sep 6 minutes | Sep 5 → Sep 6 fresh input | Correctness under the same case checks |
| --- | --- | --- | --- |
| Prefetch / control | 19.55 → 9.50 | 137,619 → 106,812 | 10/11 → 10/11 |
| Prefetch / current | 12.94 → 14.20 | 128,818 → 142,984 | 10/11 → 10/11 |
| Root / control | 8.46 → 15.13 | 83,145 → 118,582 | Supplemental 3/12 → 12/12 |
| Root / current | 19.16 → 16.76 | 143,432 → 158,697 | Supplemental 12/12 → 3/12 |

Even control changes substantially. Software, guide/setup details and solver
realizations are not isolated across dates. The latest binary demonstrably
returns complete definitions in actual use, but these solver outcomes do not
establish that the merged fixes improved task correctness or overall efficiency.

## Follow-ups

Proposals for user decision, not changed defaults or queued runs:

1. Preflight the fixture generator, browser-endpoint wiring and native dev-test
   mode before another solver comparison. These repeatedly consumed effort.
   Keep the parent-negative controls and real consumer typing checks; do not
   hide those failures behind declaration-string assertions.
2. Treat efficiency instructions as a candidate for a small replicated
   comparison with current instructions. Do not infer a general win from one
   attempt per cell or from lower tool counts alone.
3. Audit task-blind memory coverage/allocation before spending on another
   memory-benefit run. Prefetch lacks supported knowledge at the queried file;
   root params uses memory but still produces a flawed patch. Coverage, use and
   correctness are three separate measurements. Do not seed scouting from the
   hidden solution or regenerate data for just one arm.
4. Keep no-`rg` as a diagnostic treatment for now. Consider path-aware lookup or
   an explicit known-file navigation allowance for future restrictive trials:
   path-shaped OR searches and README discovery created concrete friction.
5. Only then select a new discovery-heavy case and perform its fresh indexing
   and scouting at the end of the sequence, as requested. This batch adds no
   new case and repeats no generative scouting.

External artifacts:
`jscout-replay-runs/next-astra-instructions-2026-09-06.zIALfv/`.
The repository contains the report, portable machine results and evidence hashes,
not raw source, databases, credentials or full transcripts. Validation: 13 replay
harness tests, seven analysis tests, 28 frozen supplemental self-tests (local
TypeScript 6.0.2), and the completed native matrix (task-pinned TypeScript 6.0.3).
The OpenAI Docs skill informed isolation of the
prompt variants; it is not evidence that any treatment succeeds.
