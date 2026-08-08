# n8n + Twenty post-cutoff evaluation — 2026-08-09

## Decision

This run does not support a claim that jscout improves repository localization
over grep for a capable agent. Grep had the best observed correctness, lowest
mean token usage, lowest mean wall time, and fewest irrelevant file reads.
Baseline and structural retrieval each missed one adjudicated boundary across
24 runs. The paired token and wall-time confidence intervals cross zero, so the
observed cost differences are not stable wins or losses. Structural retrieval
did cause a stable increase in irrelevant inspection.

The result changes the next implementation priority: add production/test file
roles and path filtering before expanding graph retrieval or starting broad
semantic cards. The bounded workflow-synthesis experiment remains useful
because this suite tests localization, not the product question “which
workflows does this code participate in?”

## Corpus and admission

| Repository | Frozen commit | Tracked files | Indexed files | Chunks | References | Resolved edges | Member calls |
|---|---:|---:|---:|---:|---:|---:|---:|
| n8n | `9d9e9bf97e8ae5382a930cd662637a9cf7046ef9` | 26,341 | 19,199 | 92,210 | 404,990 | 772,596 | 545,748 |
| Twenty | `02a187d065354872c0f318b0723a1e7d8762ae00` | 27,790 | 22,735 | 75,095 | 298,700 | 450,576 | 134,902 |

Tasks were mined from symbols introduced or materially reshaped after
2026-01-01 and certified with `git log -S`/`--follow`. Before admission, the
same execution model was asked each question in an empty workspace with no
repository, MCP, web, plugins, or user configuration. Any tool activity made a
probe invalid; complete remembered file localization rejected the task; partial
overlap required review. All eight Terra/high probes returned unknown with no
files, symbols, or tool activity. The certificates are in
[`post-cutoff-contamination-2026-08-09.jsonl`](post-cutoff-contamination-2026-08-09.jsonl).

Sol/high then independently solved all eight tasks against the frozen source
and matched every hand-built gold file and symbol set exactly.

## Protocol

- Codex CLI `0.147.0`.
- Execution model: `gpt-5.6-terra`, high reasoning.
- Gold review and blind disagreement adjudication: `gpt-5.6-sol`, high reasoning.
- Three trials, eight tasks, three profiles: 72 agent runs.
- Profile order counterbalanced per task.
- `grep`: repository-local shell/filesystem only, no jscout server.
- `baseline`: required to start with jscout baseline and verify source.
- `structural`: required to start with jscout structural and verify source;
  expanded search available but not required.
- Frozen source archives and prebuilt index databases were reused across arms.
- Primary outcome: exact file+symbol set, followed by blind semantic
  adjudication of disagreements.
- Efficiency deltas are paired by task and trial. The 95% intervals use 20,000
  bootstrap samples clustered by task, so three trials of one task are not
  treated as three independent task designs.

## Results

| Profile | Adjudicated correct | Exact set | Mean file P/R | Mean symbol P/R | Mean total tokens | Mean wall time | Mean inspected | Mean irrelevant |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| grep | 24/24 | 24/24 | 1.000 / 1.000 | 1.000 / 1.000 | 172,867 | 48.95 s | 6.79 | 2.67 |
| baseline | 23/24 | 22/24 | 0.990 / 0.990 | 0.979 / 0.979 | 214,805 | 56.59 s | 7.42 | 3.29 |
| structural | 23/24 | 23/24 | 0.990 / 0.990 | 0.990 / 0.990 | 185,850 | 53.75 s | 13.08 | 9.04 |

Repository exact-set counts:

| Repository | grep | baseline | structural |
|---|---:|---:|---:|
| n8n | 12/12 | 11/12 | 11/12 |
| Twenty | 12/12 | 11/12 | 12/12 |

Blind Sol adjudication reviewed only the three exact-set disagreements, with
profile labels hidden:

- Baseline n8n redaction: `processExecution` was accepted as a concrete
  implementation-entrypoint alternative to `ExecutionRedactionService` because
  the file set and architectural boundary were otherwise exact.
- Baseline Twenty Slack: incorrect. It returned the parser leaf and omitted
  `enqueueSlackAssistantRequest`, which owns empty-request handling,
  subscription gating, and dispatch to idempotent record creation.
- Structural n8n redaction: incorrect. It returned the generic module loader
  and omitted the concrete execution-redaction implementation.

The adjudication records are in
[`post-cutoff-adjudications-2026-08-09.jsonl`](post-cutoff-adjudications-2026-08-09.jsonl).

## Paired deltas versus grep

Positive token/time/file values are worse than grep. Accuracy is percentage
points on adjudicated task correctness.

| Profile delta | Mean | Median | Task-clustered 95% interval |
|---|---:|---:|---:|
| baseline correctness | −4.17 pp | 0 | [−12.5, 0] pp |
| structural correctness | −4.17 pp | 0 | [−12.5, 0] pp |
| baseline tokens | +41,938 | +30,977 | [−16,696, +124,831] |
| structural tokens | +12,983 | +21,901 | [−20,108, +43,752] |
| baseline wall time | +7.64 s | +1.77 s | [−2.62, +21.01] s |
| structural wall time | +4.80 s | +3.96 s | [−3.01, +14.02] s |
| baseline irrelevant files | +0.63 | 0 | [−1.83, +2.67] |
| structural irrelevant files | +6.38 | +5 | [+1.00, +12.38] |

Only structural irrelevant-file inspection has an interval excluding zero.
The evidence supports an expansion-noise problem; it does not support a stable
token or latency effect in either direction.

## Tool behavior and payloads

| Profile | Calls | Failed | Result bytes | Max response | Definition | Search | Outline | Who-uses |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline | 242 | 0 | 1,146,032 | 26,200 | 134 | 61 | 26 | 21 |
| structural | 197 | 0 | 1,240,959 | 28,479 | 107 | 68 | 16 | 6 |

Structural agents explicitly enabled search expansion 39 times; 29 structural
searches omitted the expansion flag. They made zero direct `neighborhood` or
`events` calls. Expanded search remains the actual delivery mechanism for the
graph. The higher structural inspection count came largely from tests,
fixtures, generated files, and adjacent framework code returned or followed
from expanded results.

Whole-response budgeting held the maximum MCP response below 29 KB even when
agents requested 30 KB, and no response exceeded the earlier 40 KB failure
case. There was one source-budget truncation across 439 indexed-profile calls.
Budgeting is working; relevance is now the larger payload problem.

## Scale result and bug found

The first full-corpus index attempt exposed a UTF-8 boundary panic in the
line-fallback chunker: a byte budget could land inside an em dash or Arabic
character before the code searched backward for a newline. The fix backs the
provisional end offset to a valid character boundary and has a regression test
covering two multibyte splits.

After the fix, release indexing completed with zero failed files:

- n8n: 19,199 files, 92,210 chunks, 404,990 references in 13.33 s.
- Twenty: 22,735 files, 75,095 chunks, 298,700 references in 10.03 s.

The indexer is fast enough at this scale. Retrieval precision, not index build
time, is the current bottleneck.

## Consequences

1. Keep expansion opt-in. The large-corpus result strengthens that decision.
2. Add indexed file roles (`production`, `test`, `fixture`, `generated`,
   `documentation`, `unknown`) and search/expansion filters. Agent prompts that
   explicitly exclude tests currently cannot express that constraint to the
   retrieval layer.
3. Penalize test/fixture/generated nodes during graph expansion unless the
   query requests them. Apply the penalty before the global node/byte budget so
   production evidence is not displaced.
4. Keep standalone `neighborhood` as plumbing. Agents reached graph context
   through expanded search in all three trials.
5. Do not claim a general localization win. Run this suite again only after a
   retrieval change with a pre-registered expected effect.
6. Continue the bounded SC-2a workflow experiment separately. Its gate must be
   questions the L1 localization suite cannot answer directly, including
   “which workflows does this code participate in?”, with freshness and source
   evidence required from the first prototype.

Agent token usage in this run frequently exceeded 200,000 because the CLI
reports cumulative input plus output across turns, including repeated/cached
context. That number is not a context-window ceiling and was not used as a
failure threshold.
