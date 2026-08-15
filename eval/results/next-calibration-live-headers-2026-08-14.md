# Next.js PR-replay calibration: live request headers

Execution: 2026-08-14 through 2026-08-15
Task: `next-live-request-headers`
Fix commit: `3c97df56ead9d1df81b36f891ba5ac0724c4eec0`
Parent: `ac65c6b27c53df92c814b95326e2cfba7bc57a82`
Agent: `gpt-5.6-terra`, high reasoning

## Outcome

- All 11 arms passed the hidden production-mode behavioral e2e and changed
  both gold production files. This task is therefore at a correctness ceiling;
  it supports no claim that one retrieval profile is more correct.
- The installed skill alone produced jscout adoption in all 5 jscout profiles.
  The earlier zero-adoption result does not generalize to Terra/high with the
  shipped skill.
- Requiring jscout for all repository-wide localization was viable: all 5
  forced arms passed without `rg`, `grep`, `find`, or `git grep` over the repo.
- Forced use was inefficient in this seed. It averaged 23.8 jscout calls and
  143,658 result bytes, versus 4.8 calls and 22,192 bytes for skill-only.
- Scout policy reduced unique embedding inputs from 35,180 to 11,886 (-66.2%).
  Scout plus product embedding took 17m12s, versus 68m03s for full embedding.

## Correction to the earlier calibration result

The previous version of this report said the saved grep and structural patches
failed the hidden oracle. That oracle was overfit to the reference patch's
internal API shape, including a particular hidden-name container, rather than
the behavioral contract.

The admitted oracle is now the production-mode e2e at
`test/e2e/app-dir/proxy-headers-live-view/proxy-headers-live-view.test.ts`.
The grader rebuilds the submitted source before running it. This oracle fails on
the historical parent, passes the reference patch, and also passes both saved
calibration implementations. The old failure conclusion is withdrawn.

## Admission and isolation

Each arm independently:

1. archives the historical parent without upstream Git history or remotes;
2. installs dependencies and builds;
3. creates a synthetic one-commit baseline;
4. installs and hash-verifies the jscout skill for jscout arms;
5. prepares an isolated external database for the declared profile;
6. runs the agent with network restricted to declared package dependencies;
7. captures the patch, exact MCP request log, Codex event stream, and tokens;
8. rebuilds submitted source and runs the hidden production e2e.

The story contains no PR number, file path, symbol name, or regression-test
name. A Terra/high no-tools contamination probe did not identify the files or
symbols.

The structural index contained 17,535 files, 47,893 chunks, and 78,741 refs.
It reported 4,112 extraction failures. Of the preflight failures, 4,106 were
`.js`, 3,183 were below `test/`, and none were below `packages/next/`. Most are
JSX-in-`.js` examples and fixtures, but the failure rate is still a real parser
coverage gap for future Next.js tasks.

## Matrix

`Fresh in` excludes cached input. `Cached in` is reported separately because
Codex usage events are cumulative and repeated context dominates raw totals.
`Tool bytes` is the sum of jscout MCP response bytes.

| Profile | Treatment | E2E | jscout calls | Tool bytes | Fresh in | Cached in | Output | Agent time |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| grep | control | pass | 0 | 0 | 104,252 | 1,761,280 | 13,119 | 323.1s |
| structural | skill | pass | 2 | 19,515 | 93,954 | 1,292,288 | 12,604 | 299.6s |
| structural | forced | pass | 40 | 158,330 | 135,020 | 2,947,328 | 15,595 | 396.7s |
| checker | skill | pass | 9 | 55,836 | 111,453 | 1,648,640 | 12,182 | 290.6s |
| checker | forced | pass | 12 | 73,707 | 110,527 | 2,228,224 | 15,749 | 410.6s |
| checker + embed | skill | pass | 5 | 10,573 | 110,184 | 1,903,104 | 14,049 | 376.1s |
| checker + embed | forced | pass | 18 | 104,985 | 92,815 | 1,360,384 | 11,338 | 296.2s |
| checker + scout | skill | pass | 6 | 11,797 | 133,940 | 2,737,664 | 17,900 | 429.6s |
| checker + scout | forced | pass | 19 | 117,148 | 134,005 | 2,759,936 | 13,766 | 358.2s |
| checker + scout + embed | skill | pass | 2 | 13,241 | 105,719 | 1,902,592 | 13,474 | 352.0s |
| checker + scout + embed | forced | pass | 30 | 264,119 | 140,326 | 2,599,680 | 13,131 | 360.5s |

Across the five jscout profiles, forced versus skill-only produced:

| Mean metric | Skill-only | Forced | Change |
|---|---:|---:|---:|
| jscout calls | 4.8 | 23.8 | 4.96x |
| semantic searches | 1.4 | 9.8 | 7.00x |
| jscout result bytes | 22,192 | 143,658 | 6.47x |
| fresh input tokens | 111,050 | 122,539 | +10.3% |
| cumulative total tokens | 2,021,949 | 2,515,565 | +24.4% |
| agent time | 349.6s | 364.4s | +4.3% |

These are one-seed descriptive results, not stable treatment estimates.

## Preparation cost

The profile database was prepared once and copied byte-for-byte for the two
agent treatments.

| Stage | Added time | Result |
|---|---:|---|
| structural index | 6s | 256 MiB database |
| checker enrichment | 18m10s | 328 MiB database |
| full embedding | 68m03s | 35,180 unique vectors; 680 MiB database |
| repository scout | 5m03s | 64 classifications; 331 MiB database |
| product embedding after scout | 12m09s | 11,886 unique vectors; 445 MiB database |

The scout used 64 plan-backed Terra calls: 191,055 input tokens, 9,460 output
tokens, and 200,515 total. It produced 28 runtime, 27 tooling, 7 test, 1 mixed,
and 1 unknown classification. `packages/next` was `runtime/likely`. The call
budget skipped 307 remaining subjects, so this is partial reconnaissance with
neutral fallback, not full-repository classification.

Including scout cost, scout + product embedding was 75% faster than full
embedding for this snapshot and reduced the resulting database by 34.6%.

Checker enrichment discovered 76,018 member-call occurrences, selected 18,809,
issued 635 request batches, and published 1,873 facts. It also reported 2,213
unknown answers, 24,804 unmapped declarations, and 304 projects with unknown
answers. Peak checker RSS was 1.34 GiB. This task shows no agent-level value
from that cost because every profile passed; it does not prove the checker is
useless on tasks that require dynamic receiver resolution.

The combined experimental profiles intentionally enriched before scouting so
they could reuse the identical checker fact set and isolate the scout overlay.
That means this run did not measure the production ordering `index -> scout ->
checker`, where scout policy can avoid tooling projects. The checker reported
zero occurrences avoided by the tooling filter for this reason.

## Output and context behavior

Whole-response budgets worked: no jscout response exceeded 21,682 bytes and no
source-budget truncation occurred. Cumulative payload is still a problem. The
combined forced arm accumulated 264,119 jscout bytes across 30 calls.

Shell output was often larger than MCP output: grep emitted 1.37 MB,
structural/forced 1.25 MB, and scout/skill 887 KB. Several single failed or
watch-build commands emitted 0.5-1.0 MB. Therefore agent token usage cannot be
attributed to jscout output alone. Future reports must include both MCP result
bytes and shell-output bytes.

Hybrid searches activated both vector retrieval and the local reranker. A
single hybrid search took about 3-10 seconds here; repeated forced searches
accumulated up to 50 seconds of MCP latency. Scout filtering kept combined
expansions to production files, but did not prevent repeated searches or large
`definition`/`who_uses` responses.

## What this result supports

1. Use the installed skill as the realistic default integration. Terra/high
   adopted it in 5/5 profiles without explicit coercion.
2. Do not default to a jscout-only prompt. It is useful as a capability probe,
   but it multiplied calls and payload without improving this task's outcome.
3. Prefer product-only embedding after fresh scout policy on large monorepos.
   The preparation-time and storage reduction here is large enough to retain.
4. Add a compact locator form for `definition` and `who_uses`, and make full
   source/context opt-in. Per-call budgets alone do not control cumulative
   context.
5. Track shell-output bytes alongside MCP bytes and prevent broad test/watch
   commands from dumping unbounded logs into agent context.
6. Fix JSX-in-`.js` extraction before selecting Next.js tasks that depend on
   examples or test fixtures.
7. Add a production-order profile (`index -> scout -> checker -> product
   embed`) separately from the current additive/isolation profile.
8. Do not infer retrieval quality from this task. Admit harder tasks and run
   multiple seeds; 11/11 correctness means this case calibrated the harness and
   operational costs, not product value.

## Artifacts

Durable raw artifacts are under:

`~/git/jscout-replay-runs/next-calibration-v3/matrix-001/`

This includes 11 event streams, patches, grades, exact MCP request logs,
telemetry, prepared profile databases, setup logs, checker output, scout usage,
and embedding progress. The directory is approximately 6.1 GiB.
