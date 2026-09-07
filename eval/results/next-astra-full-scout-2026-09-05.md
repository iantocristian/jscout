# Next.js capability replay — Astra, September 5, 2026

Recorded in the repository on 2026-09-06. Six completed implementation attempts:
two tasks, three treatments, one attempt per cell. **No demonstrated correctness
gain from jscout on the registered oracle.** All prefetch attempts fail the same
test. All root-params attempts pass that oracle, but only one passes the stronger
supplemental suite. That successful solver did not consult semantic memory.

This is a descriptive experiment, not a statistical treatment-effect estimate.
The [machine results](next-astra-full-scout-2026-09-05.json) preserve exact usage,
per-state supplemental outcomes, preparation accounting, and evidence hashes.

## Outcomes and solver effort

| Task / treatment | Original oracle | Supplemental | Attempt minutes | Fresh input | Cached input | Output |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Prefetch / no jscout | Fail, 10/11 | — | 19.55 | 137,619 | 4,871,552 | 25,134 |
| Prefetch / scouted code | Fail, 10/11 | — | 16.57 | 157,359 | 6,315,904 | 22,568 |
| Prefetch / memory available | Fail, 10/11 | — | 12.94 | 128,818 | 4,671,232 | 16,422 |
| Root params / no jscout | Pass | 3/12 | 8.46 | 83,145 | 1,518,208 | 12,922 |
| Root params / scouted code | Pass | 3/12 | 12.58 | 124,959 | 3,953,536 | 17,898 |
| Root params / memory available | Pass | 12/12 | 19.16 | 143,432 | 5,140,992 | 27,316 |

**Attempt duration is not time-to-solve.** It measures solver execution, including
its own investigation, edits and tests, but excludes external preparation and
grading. Only final patches were graded; first-correct-patch time is unknown.
Prefetch remained unsolved. Supplemental success establishes the authored suite,
not every feature edge case.

Six-attempt totals: **89.2643 solver minutes, 775,332 fresh input tokens,
26,471,424 cached input tokens, and 122,260 output tokens**. Input usage is
reconciled against cumulative event usage once; fresh input is input minus cached
input. Reasoning tokens are already included in output. Cached input includes
repeated conversation context, not that many distinct source tokens. These
are recorded usage counts, not dollar charges.

Relative to each task's control, scouted prefetch used 14.3% more fresh input
and 15.2% less time; memory-available prefetch used 6.4% less fresh input and
33.8% less time, with the same failing grade. Root scouted used 50.3% more fresh
input and 48.7% more time for the same supplemental result. Root memory-available
used 72.5% more fresh input and 126.4% more time and produced the stronger patch.
Native verification workloads differed, so these observations neither isolate
retrieval overhead nor establish a speedup to a correct solution.

## Frozen design

- Solver: `gpt-6-astra`, high reasoning, one implementation phase, 3,600-second
  allowance per attempt. No completed solver was rerun.
- jscout: `7b738ba1ce40e59fc5dc2c7c7271832b45de8706` (after #117), release binary
  SHA-256 `c269ba22534f834950b406c22ea36912a6b03482c91efaa6faf92f91e6a10609`.
- Prefetch parent: `7cb68c12828a758492ea54251393b4f988aecd6e`.
  Root-params parent: `1d8e326d1b360da4a439cf440316fe76a359bfd3`.
- Each solver received a fresh history-free workspace, the unchanged task story,
  and no prior solution/findings. Hidden grading material remained outside its
  workspace during implementation.
- Control: no jscout MCP or skill. Scouted: checker enrichment, repository
  classifications, code vectors, reranker, full skill. Memory: the same code
  retrieval state plus workflow/card/summary artifacts and semantic vectors.
- Independent database clones came from shared per-task preparation. The sealed
  substrate audit matched the indexed pair across all 19 inspected
  code/vector/FTS/policy groups; semantic memory was the intended added plane.
- Serial order: prefetch control → scouted → memory; root memory → scouted →
  control. Reversed order across tasks is not replication within a task.
- Preparation: task-blind `openai-codex:gpt-6-astra`, low reasoning. Per-task caps:
  96 repository calls, 96 workflows, 448 cards, 64 summaries. Code embeddings
  used BGE-M3 and reranking BGE reranker v2 M3, with pinned revisions retained in
  the external protocol. Preparation was not targeted using either solution.

## What the patches accomplished

### Prefetch: useful localization, same missing transition

All three patches repair related optimistic-routing behavior without disabling
the feature. All fail shape-preserving rewrite detection when parameter values
change: the prediction still exposes the wrong `promo` rather than `sale` value.
Discovery-time rejection does not cover an already-learned prediction meeting
that rewrite. Their own rewrite regressions do not exercise that transition.

The indexed agents reached the current implementation and inspected
recovery-related code despite deprecated helpers appearing high in ranked
results. The failure is not explained simply by those files being absent from
retrieval. This does not establish which intervention would have solved it.

### Root params: generated declarations were not sufficient

The original oracle passes all three patches. Separate story-derived checks
exercise actual zero-argument namespace calls under strict TypeScript 6.0.3,
explicitly reject `any`, and test required/optional parameters plus stale cleanup.
Each mode executes S (single root), M (multiple roots), M0 (add a parameterless
root), then N (remove all parameters).

| Saved patch | Build S/M/M0/N | Dev S/M/M0/N | Typegen S/M/M0/N |
| --- | --- | --- | --- |
| No jscout | Fail / Fail / Fail / Pass | Fail / Fail / Fail / Pass | Fail / Fail / Fail / Pass |
| Scouted code | Fail / Fail / Fail / Pass | Fail / Fail / Fail / Pass | Fail / Fail / Fail / Pass |
| Memory available | Pass / Pass / Pass / Pass | Pass / Pass / Pass / Pass | Pass / Pass / Pass / Pass |

Both failing patches still expose `any` to actual callers. Control additionally
makes required `lang` optional in S and M. Scouted's declaration signatures look
correct, illustrating why inspecting emitted text alone misses the defect.
The successful patch replaces the package's untyped shorthand ambient-module
fallback with `export {}`. This is a source-supported explanation, not an isolated
one-line ablation: the patches also differ elsewhere.

The completed supplemental attempt has 39 states: three expected parent
negatives and 12 per patch. All nine cleanup cases first create a real generated
file. Dev uses a fresh server per state and **does not establish live hot-reload
correctness**. These grades supplement, rather than overwrite, the original ones.

## Actual retrieval and preparation

| Task / treatment | Shell commands | jscout calls | Canonical jscout bytes |
| --- | ---: | ---: | ---: |
| Prefetch / control | 62 | 0 | 0 |
| Prefetch / scouted code | 74 | 8 | 27,621 |
| Prefetch / memory available | 53 | 4 | 15,617 |
| Root params / control | 40 | 0 | 0 |
| Root params / scouted code | 49 | 6 | 40,736 |
| Root params / memory available | 71 | 9 | 35,011 |

All 27 calls succeeded without provider degradation: 19 searches (six ranked,
13 exhaustive), four definitions and four usage lookups. Total: **118,985
canonical bytes and 22.772 seconds of server time**. Tool-result wire bytes were
247,484; transport duplication must not be added as distinct content. One
definition was explicitly budget-truncated and correctly flagged.

Exact identifiers located implementations and callers, including the shared
type writer's three entry points. Ranked results were mixed. Two warned broad
OR queries consumed 42,991 bytes (36.1% of all canonical output); both traversals
were abandoned. Shell command totals include tests, editing and log inspection,
not just discovery. The subsequent [discovery audit and proposed follow-ups](next-astra-discovery-audit-2026-09-06.md)
quantify overlap and retained source delivery, distinguish verification work,
and document incomplete capture. Neither report treats every shell call as waste.

**Neither memory-arm solver consulted semantic memory.** There were no memory
tool calls, returned artifacts, or semantic-vector execution. Both read the
complete supplied guide; it directs ordinary discovery to `include_memory:false`
and makes separate memory inquiry conditional. This tests availability under
that guide, not the benefit of reading the generated knowledge. It neither
credits the successful root patch to memory nor proves that memory was useless.

Preparation produced 587 prefetch and 584 root-params semantic artifacts, all
embedded. Across both tasks: **1,408 logical scouting calls, 1,410 reported
provider attempts, 6,413,187 recorded preparation tokens**. Each preparation
runner took about 192–195 minutes; they ran concurrently, so those durations are
not summed as campaign wall time. Stage work and setup are included. Two
submissions failed validation and 43 workflows reported incomplete supplied
evidence. Accepted artifacts were not independently scored for usefulness.
Reported preparation tokens retain gateway semantics and are separate from
solver fresh/cached accounting; missing retry usage is not assumed zero.

## Sol and Terra context, not a controlled model ranking

| Cohort | Prefetch | Root params original oracle |
| --- | --- | --- |
| Astra/high, this experiment | 0/3 full passes; all 10/11 | Control 1/1, scouted 1/1, memory available 1/1 |
| Sol/high, later historical cohorts | Three memory-only attempts: 9/11, 10/11, 10/11 | Control 0/2, scouted 2/2, memory 1/2 |
| Terra/high, same-day separate matrix | 0/6 full passes; five 10/11, one 9/11 | No corresponding root-params matrix |

Sol root-params used two counterbalanced trials; its prefetch figures are three
memory-only attempts, not three treatments. Terra's same-day experiment used
checker/code-vector skill and forced-prompt treatments, **without scouting**,
plus controls; it is not another replica of this campaign. Historical Sol
preparation used Terra and older product state. The supplemental suite has not
been applied to those historical patches. These cohorts cannot isolate model,
product-version, scouting or instruction effects.

The separate Terra report observes improved source density versus its selected
historical responses (prefetch skill full-response source share 14.15% → 32.77%;
cache forced 31.09% → 56.55%), but no demonstrated correctness gain or skill-arm
fresh-input saving. Queries and limits differed, so this is not a paired
fixed-query measurement of #117. Its original 12-run grades include one native
build crash; a separate regrade of that saved patch failed a behavioral test.
That crash and regrade must remain separate, not relabeled as two solver trials.

## Integrity, limitations, and later fixes

- Root setup initially lacked its pinned browser. A campaign-local install
  repaired setup before any root solver call; the failed setup was retained.
- The first supplemental attempt stopped at runtime helper resolution. A
  separately authorized attempt added the already-declared `@swc/helpers:0.5.15`
  directly to the fixture and repeated all three patches and parent controls.
  It completed without infrastructure failure. Neither attempt reran a solver.
- A preparation metadata-export error and a substrate-audit read timeout were
  repaired without repeating provider work. Original failures remain retained.
- Solver traces contain shared fixed `/tmp` log names outside their literal
  workspace boundary. Inspected logs identify their producing workspaces and
  show no observed prior-cell content; this is not proof of perfect isolation.
- Native testing scope differed between solvers, some detailed test-count tails
  were missing, and exploratory generator/browser/dev checks failed. Final
  registered grades and the separately executed supplemental checks are the
  outcome evidence, not self-reported test totals.
- #120 subsequently fixed definition completeness, RRF tie ordering and optional
  inference revision compatibility. #121 fixed locally declared identifier
  default exports and workflow-boundary deduplication; see the
  [native regression report](default-export-workflow-dedup-2026-09-05.md).
  **These solver results predate those fixes.** No fix was shown to cause an
  observed solver outcome, and no later solver rerun is implied.

## Retained evidence

External campaign directory: `jscout-replay-runs/next-astra-full-scout-2026-09-05/`.
Its `RESULTS.md`, `protocol.md`, `analysis/analysis.final.json`, six per-cell audits,
`analysis/historical-comparison.md`, `analysis/memory-treatment-use.md`,
`supplemental-repaired/results/summary.json`, original supplemental failures,
transcripts, requests, telemetry, saved patches and sealed databases are retained.
The sibling `next-main-snippets-2026-09-05/` contains the separate Terra comparison.
Historical Sol reports already in this repository are
[root params](next-root-params-types-2026-08-17.md) and
[prefetch](next-optimistic-prefetch-2026-08-15.md).

The companion JSON uses campaign-relative evidence paths and SHA-256 hashes;
raw source, credentials, databases and transcripts are not published here.
This write-up adds no model, scouting, embedding, indexing or native grading runs.
