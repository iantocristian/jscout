# Next.js root-layout parameter types replay

Date: 2026-08-18

Task: Next.js feature #91019, project-specific root-layout parameter types

Evaluation base: `1d8e326d1b360da4a439cf440316fe76a359bfd3`

Historical reference: `46e2114ea28a39aab4c04c2451aa462af14e62df`

Evaluation implementation: PR #46, `codex/eval-two-phase`

## Executive result

The valid matrix completed 12 arms across two models, two workflows, and three
retrieval profiles. Eight arms passed the registered hidden oracle.

| Model | Workflow | Grep | Checker/scout/vector | Memory/vector |
| --- | --- | --- | --- | --- |
| `gpt-5.6-sol` | Single phase | Fail | Pass | Pass |
| `gpt-5.6-sol` | Design then implement | Pass | Fail | Pass |
| `gpt-5.6-terra` | Single phase | Pass | Pass | Pass |
| `gpt-5.6-terra` | Design then implement | Fail | Fail | Pass |

The first Terra single-phase checker arm was invalidated by host sleep. It
recorded `timed out after 7200s`, zero model tokens, no final response, and no
patch. The exact arm was rerun under trial `002-terra-single-retry`; the retry
passed and is the only checker result included for that matrix cell. The invalid
artifact remains preserved for audit.

The observed pass rates are:

- overall: 8/12;
- single phase: 5/6;
- design then implement: 3/6;
- grep: 2/4;
- checker/scout/vector: 2/4;
- memory/vector: 4/4;
- Sol: 4/6;
- Terra: 4/6.

This is one task with one valid run per cell. These rates describe this replay;
they are not estimates of general product performance.

## Experimental contract

All arms used the same prepared Next.js snapshot, task prompt, hidden oracle,
model reasoning level (`high`), and skill-only treatment. There was no forced
jscout treatment.

The three profiles were:

1. `grep`: no jscout MCP surface;
2. `checker-scout-embed`: deterministic index, TypeScript checker enrichment,
   repository reconnaissance, vectors, and reranking;
3. `memory-embed`: the checker/scout/vector corpus plus workflows, cards,
   summaries, and semantic-artifact embeddings.

The two workflows were:

1. `single`: one implementation turn;
2. `design-implement`: a read-only design turn producing a structured handoff,
   followed by a separate implementation turn receiving that handoff.

The repository was prepared and built before the arms. Prepared databases were
reused rather than rebuilt per arm.

## Per-arm measurements

`Input` below is non-cached input. Cached tokens are reported separately because
the raw totals are dominated by cache reads.

| Model | Workflow | Profile | Result | Minutes | Input | Cached input | Output | Commands | jscout calls |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sol | Single | Grep | Fail | 9.7 | 176.3K | 4.35M | 21.2K | 43 | 0 |
| Sol | Single | Checker | Pass | 10.7 | 176.4K | 6.47M | 24.7K | 38 | 6 |
| Sol | Single | Memory | Pass | 15.8 | 263.3K | 11.08M | 31.8K | 72 | 5 |
| Sol | Design/implement | Grep | Pass | 17.8 | 307.7K | 7.71M | 43.0K | 60 | 0 |
| Sol | Design/implement | Checker | Fail | 21.0 | 521.7K | 11.75M | 51.0K | 95 | 19 |
| Sol | Design/implement | Memory | Pass | 27.1 | 500.2K | 11.89M | 54.1K | 103 | 17 |
| Terra | Single | Grep | Pass | 11.2 | 195.0K | 4.53M | 27.6K | 35 | 0 |
| Terra | Single | Checker | Pass | 12.8 | 209.0K | 7.59M | 28.2K | 37 | 5 |
| Terra | Single | Memory | Pass | 15.8 | 271.9K | 8.76M | 34.8K | 41 | 7 |
| Terra | Design/implement | Grep | Fail | 13.1 | 272.1K | 4.43M | 35.8K | 47 | 0 |
| Terra | Design/implement | Checker | Fail | 16.9 | 553.4K | 7.06M | 41.2K | 55 | 18 |
| Terra | Design/implement | Memory | Pass | 14.5 | 319.1K | 5.49M | 36.2K | 48 | 13 |

Across the valid matrix:

- non-cached input: 3,765,913 tokens;
- cached input: 91,119,104 tokens;
- output: 429,546 tokens;
- recorded model-run time: 11,189,657 ms, about 3.1 hours.

The design/implementation workflow averaged 412K non-cached input versus 215K
for single phase, a 91% increase. It averaged 18.4 minutes versus 12.7 minutes,
a 45% increase, and 43.5K output tokens versus 28.0K, a 55% increase.

## jscout call behavior

The valid indexed arms made 90 product calls:

| Tool | Calls | Result bytes | Average | Maximum |
| --- | ---: | ---: | ---: | ---: |
| `semantic_search` | 60 | 668,634 | 11,143 | 21,919 |
| `repository_overview` | 12 | 133,530 | 11,127 | 13,518 |
| `semantic_memory` | 3 | 71,278 | 23,759 | 23,987 |
| `definition` | 9 | 30,172 | 3,352 | 6,839 |
| `file_outline` | 4 | 700 | 175 | 175 |
| `who_uses` | 2 | 1,813 | 906 | 919 |
| **Total** | **90** | **906,127** | **10,068** | — |

Searches were incremental and bounded:

- 1 search used limit 6;
- 3 used limit 7;
- 32 used limit 8;
- 24 used limit 10.

No search exceeded limit 10. Agents normally started with an overview and one
or two bounded searches, then widened with more bounded searches or targeted
definitions/outlines. The skill guidance is producing the intended call shape;
the experiment does not support replacing it with large one-shot retrieval.

## Retrieval findings

### Exact identifiers are ranked incorrectly

Two exact-symbol searches produced clear ranking failures:

- `createRouteTypesManifest` returned an unrelated example helper named
  `createRoute` ahead of the exact repository symbol;
- `getRootParamsFromLayouts collectedRootParams NextTypesPlugin`, restricted to
  production files and limit 6, returned unrelated Sitecore example code.

The agents recovered with `rg` and source reads, but exact lexical/symbol matches
must dominate vector and reranker scores. This is an MCP/retrieval defect, not a
prompt-budget issue.

### Explicit semantic memory was mostly irrelevant

The three explicit memory calls had candidate pools of 93, 270, and 220. They
returned 8, 8, and 10 artifacts respectively, with zero matched concept tags in
all three calls.

Most returned artifacts were unrelated CMS examples, route normalizers, Pages
Router initialization, or client bootstrap summaries. One returned artifact,
`generateCacheLifeTypes`, was a useful adjacent implementation precedent. The
memory calls were close to the 24 KB response limit, but relevance—not budget—was
the binding problem.

Telemetry also records six semantic artifacts attached to memory-profile search
responses. The run artifacts do not show those artifacts supplying the decisive
dedicated-file mechanism.

The memory profile passed 4/4, but that is not evidence that retrieved semantic
memory caused the passes. In the successful arms, the agents reached the correct
architecture through search and source inspection. With one run per cell, the
4/4 result may reflect model variance or another profile-correlated effect.

Increasing the memory response budget is not supported by this replay. It would
have returned more low-relevance artifacts.

## Correctness failure analysis

### Sol single-phase grep

The implementation did not create `.next/types/root-params.d.ts`. Both hidden
fixtures failed with `ENOENT` when the oracle read that file.

### Sol design/implementation checker

The design selected the old webpack-adjacent `types/server.d.ts` surface. The
implementation followed the handoff, and the hidden oracle again failed because
`.next/types/root-params.d.ts` did not exist.

### Terra design/implementation checker

The design correctly rejected `server.d.ts`, but embedded the declaration in
`routes.d.ts` instead of generating the dedicated `root-params.d.ts`. Its own
new tests passed that alternative contract; the registered hidden oracle failed.

This is the clearest example of a design handoff preserving a coherent but wrong
contract through implementation.

### Terra design/implementation grep

The production architecture used the correct dedicated file. The arm failed
because its validation plan added a catch-all root named `id` to the existing
multiple-roots fixture. That changed the generated `id()` return type to include
`string[]`, while the hidden fixture expected `string | undefined`. The agent's
additional test surface changed the semantics under the oracle.

This was not a localization failure. It was an over-broad validation change that
polluted a shared fixture.

## Design-phase finding

The explicit design phase is not a general improvement on this task.

It helped the Sol grep arm move from a missing-file failure to a pass. It also
produced detailed architecture in several arms. However, it locked both checker
arms into the wrong output contract, and the Terra grep design prescribed the
fixture change that caused its failure.

For this task, design/implementation cost more and passed less often. The result
supports keeping two-phase execution as an optional evaluation treatment. It
does not support adding it to the jscout product surface or making it the default
agent workflow.

## Harness findings

The controlled hidden oracle completed for every valid arm. The Playwright
sidecar artifacts were present, and the registered start-mode oracle remained
usable.

Agent-initiated verification still exposed two infrastructure limitations:

1. isolated Next.js e2e setup can fail when its internal `pnpm install` receives
   sandbox `EPERM`;
2. non-isolated development tests can still emit repeated `EMFILE` watcher
   failures despite inheriting `ulimit -n 65536`.

These failures did not replace hidden-oracle results, but they weaken the
agent's edit/verify loop and should be fixed before selecting dev/watch-heavy
tasks.

## Decisions and next work

1. Preserve the current incremental skill guidance. Agents used bounded,
   repeated queries as intended.
2. Fix exact-identifier ranking before adding more retrieval surface. Exact
   symbol and lexical hits should be pinned above vector/reranker candidates.
3. Proceed with the planned memory-selection redesign. Candidate volume and
   larger response budgets are not substitutes for scope/evidence relevance.
4. Do not flip a memory default based on the 4/4 result; rerun discriminating
   tasks after selection changes and with replicas.
5. Keep design/implementation in the evaluation harness only. Test it on tasks
   whose bottleneck is independently established as hypothesis generation.
6. Fix the `EMFILE` and sandboxed-install verification paths before another
   watcher-sensitive Next.js campaign.

## Artifact location

The uncommitted large artifacts are stored outside the jscout repository at:

`/Users/cristian/git/jscout-replay-runs/next-root-layout-param-types-2026-08-17/experiment-001`

The directory contains per-arm prompts, event streams, jscout requests,
telemetry, patches, grades, responses, browser-sidecar manifests, and the
preserved invalid sleep-timeout arm. Prepared databases are under its
`prepared-databases` directory. They are intentionally not copied into git.
