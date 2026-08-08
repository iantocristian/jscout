# File-role retrieval re-run — 2026-08-09

## Decision

The pre-registered file-role gate passes. Structural retrieval's paired
irrelevant-file delta versus grep fell from **+6.38** to **+1.08**, with a
task-clustered 95% interval of **[-9.21, +10.71]**. The registered success
boundary was a point estimate no greater than +2.0 with an interval including
zero.

All three arms answered 24/24 tasks exactly, so no blind adjudication was
needed. Structural's token delta versus grep was +12,758 with an interval that
crosses zero, just inside the pre-registered non-regression ceiling of +12,983.
Expanded test/fixture/generated nodes fell from 20/156 (12.82%) in the retained
pre-change artifacts to 0/116 (0%).

This is evidence that role filtering removed the measured expansion-noise
effect. It is not evidence that jscout beats grep on correctness, tokens, or
latency: correctness remains ceilinged and all cost intervals cross zero. The
registered L1 intervention is complete; the next experiment is the separate
SC-2a two-session workflow-memory gate.

## Frozen treatment and controls

- Evaluation-control commit: `83e141a`.
- Sole source treatment: `fce27b79ebe1246e34d298c68ffdf0985a099ecc`.
- Codex CLI `0.147.0`; execution model `gpt-5.6-terra`, high reasoning.
- Same eight tasks, frozen commits, three profiles, three trials, prompt,
  profile counterbalancing, grading, and 20,000-sample task-clustered bootstrap
  as the original run.
- n8n commit `9d9e9bf97e8ae5382a930cd662637a9cf7046ef9`, role-aware snapshot
  `0a9290afc77541d3a3f26572c3909bbed5a208117b56042b47ba7d9739e98d60`.
- Twenty commit `02a187d065354872c0f318b0723a1e7d8762ae00`, role-aware snapshot
  `5fb20574f2b8c69d57aa0fd87d980abff911a164723ffc762bab75be24e149b2`.
- `integrations/jscout/SKILL.md` remained byte-identical, SHA-256
  `4c1e2b81f79c523e4e314df275b50ce3eb8108cec907408c9c1fe2e2cae89238`.
- 72 unique response sessions, all stamped `gpt-5.6-terra/high`; zero runner
  errors, zero MCP failures, and no missing task/profile/trial rows.

An initial sandboxed launch was excluded before analysis: every Codex process
failed during initialization because its state database was read-only. It made
zero model/tool calls and produced only runner-error rows. The valid batch used
fresh paths and the normal Codex state environment.

## Indexed role distribution

Schema v4 databases migrated to schema v5 and refreshed roles without
reparsing unchanged files. Both projections rebuilt at version 3.

| Repository | Production | Test | Fixture | Generated | Documentation | Unknown | Failed files | Re-index time |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| n8n | 10,717 | 8,278 | 28 | 69 | 107 | 0 | 0 | 9.08 s |
| Twenty | 18,025 | 3,923 | 140 | 189 | 458 | 0 | 0 | 7.17 s |

## Results

| Profile | Exact / correct | Mean tokens | Mean wall time | Mean inspected | Mean irrelevant |
|---|---:|---:|---:|---:|---:|
| grep | 24/24 | 215,631 | 75.39 s | 11.25 | 7.13 |
| baseline | 24/24 | 219,900 | 78.53 s | 10.96 | 6.83 |
| structural | 24/24 | 228,389 | 80.01 s | 12.33 | 8.21 |

Every repository/profile cell was 12/12 exact. The higher absolute inspection
counts than the first run show substantial trial variance; the registered
decision uses within-task, within-trial paired deltas.

## Paired deltas versus grep

Positive cost/file values are worse than grep.

| Profile delta | Mean | Median | Task-clustered 95% interval |
|---|---:|---:|---:|
| baseline tokens | +4,269 | +38,116 | [-95,234, +93,276] |
| structural tokens | +12,758 | +10,607 | [-94,072, +132,597] |
| baseline wall time | +3.14 s | +8.99 s | [-17.07, +22.70] s |
| structural wall time | +4.62 s | +0.71 s | [-22.67, +38.18] s |
| baseline irrelevant files | -0.29 | +3 | [-7.58, +5.54] |
| structural irrelevant files | **+1.08** | +4 | **[-9.21, +10.71]** |

Correctness, file precision/recall, and symbol precision/recall deltas were all
zero.

## Expansion-role outcome

The pre-change denominator was backfilled from the retained structural Codex
artifacts by joining returned expansion node paths to schema-v5 roles on the
same frozen commits. The recorded baseline is
[`file-roles-prechange-expansion-backfill-2026-08-09.json`](file-roles-prechange-expansion-backfill-2026-08-09.json).

| Expansion payload | File-backed nodes | Production | Test | Fixture | Generated | Registered share |
|---|---:|---:|---:|---:|---:|---:|
| Pre-change | 156 | 136 | 20 | 0 | 0 | 12.82% |
| File roles | 116 | 116 | 0 | 0 | 0 | **0%** |

The pre-registered threshold was below half the baseline, or <6.41%. Telemetry
now records these counts on each expanded-search call; no paths, queries, or
source are written to telemetry.

## Tool behavior

| Profile | Calls | Failed | Result bytes | Max response | Search | Definition | Outline | Who-uses |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline | 215 | 0 | 1,137,449 | 19,895 | 52 | 122 | 27 | 14 |
| structural | 215 | 0 | 1,318,166 | 29,711 | 53 | 126 | 27 | 9 |

No direct `neighborhood` or `events` calls occurred. Expanded search remains
the delivery path for structural context.

## Registered criteria

| Criterion | Boundary | Result | Status |
|---|---|---|---|
| Primary irrelevant-file effect | structural minus grep <= +2.0; CI includes zero | +1.08; [-9.21, +10.71] | Pass |
| Indexed-arm correctness | each >=23/24 | baseline 24/24; structural 24/24 | Pass |
| Structural token non-regression | point <= +12,983; CI crosses zero | +12,758; [-94,072, +132,597] | Pass |
| Expansion role share | <6.41% | 0% | Pass |

The result closes this L1 retrieval intervention. It does not reopen expansion
breadth, standalone neighborhood UX, or source compression. Those surfaces
remain opt-in/plumbing until a different pre-registered task class earns them.
