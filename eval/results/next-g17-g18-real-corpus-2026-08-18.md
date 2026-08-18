# Next.js G17/G18 real-corpus validation

Date: 2026-08-18  
Status: completed focused validation; not a comparative treatment estimate  
Branch: `codex/g17-g18` / PR #50  
Task: reconstruct Next.js root-layout parameter declaration feature #91019  
Parent: `1d8e326d1b360da4a439cf440316fe76a359bfd3`

## Question

This run checked whether G17 exact-identifier dominance and G18 task-directed
semantic selection work on a real Next.js snapshot, whether the expensive
embedding corpus can be reused, and whether agents use the resulting surface
incrementally.

It was not registered as a model/profile comparison. The two initial model
runs and one focused interface rerun are single observations. Outcome
differences must not be attributed to jscout or a model from this sample.

## Reused substrate

The starting database was the existing `memory-embed` profile from the prior
root-layout evaluation:

`/Users/cristian/git/jscout-replay-runs/next-root-layout-param-types-2026-08-17/experiment-001/prepared-databases/next-root-layout-param-types/memory-embed.db`

| Corpus item | Before | After augmentation |
| --- | ---: | ---: |
| Files | 19,359 | 19,758 |
| Chunks | 51,967 | 53,722 |
| Code embeddings | 11,653 | 11,653 reused |
| Semantic artifacts | 595 | 599 |
| Semantic embeddings | 595 | 599 |

The deterministic reindex retained every existing code embedding. Only four
new targeted cards were generated and only those four semantic artifacts were
embedded. No full re-embedding or full scouting pass ran.

The augmented reusable database and provenance are stored outside the source
workspace:

- `/Users/cristian/git/jscout-replay-runs/next-g17-g18-validation-2026-08-18/prepared/next-root-layout-param-types/memory-embed.db`
- `/Users/cristian/git/jscout-replay-runs/next-g17-g18-validation-2026-08-18/augmentation.json`

## Review findings before the replay

### Authored build code was absent from the corpus

The reusable database had no `getRootParamsFromLayouts`, `collectedRootParams`,
or `NextTypesPlugin` chunks. The cause was not retrieval: the walker globally
excluded every directory named `build`, including authored source under
`packages/next/src/build/**`. It also excluded authored declaration files such
as `packages/next/root-params.d.ts` despite the contract plane indexing type
surfaces.

The fix removes `build` from the global skip list and indexes authored `.d.ts`,
`.d.mts`, and `.d.cts` files. The reindex added 399 files and 1,755 chunks while
reusing all old vectors.

### Exact property occurrences needed a bounded fallback

`collectedRootParams` is an object property/member occurrence, not a symbol,
reference target, member call, or entity site in all relevant chunks. G17
therefore needed a bounded textual candidate fallback. FTS supplies candidate
chunks; a case-sensitive lightweight lexer verifies identifier boundaries and
rejects comments and quoted/template strings before assigning
`exact_occurrence`.

### Mixed queries could be monopolized by one exact occurrence family

The query `generated type declarations LayoutProps route params root layout`
initially returned ten `LayoutProps` occurrences, mostly examples, ahead of all
hybrid task matches. That behavior followed the original tier definition but
was counterproductive for incremental natural-language search.

The reviewed rule is now:

- exact definitions remain an absolute tier;
- a pure single-identifier query may return every bounded exact occurrence;
- a mixed query admits one exact occurrence per identifier, then resumes the
  hybrid ranking;
- exact-occurrence peers present in the hybrid pool use its reranker and
  repository-policy order without crossing a tier boundary.

After the fix, the same query returned one `LayoutProps` occurrence followed by
the relevant `typegen.ts` and `next-types-plugin` chunks. The compact response
was 9,734 bytes with vector retrieval and reranking active.

### `repository` is not the complete repository

The stored origin vocabulary has a product-facing trap:

- `workspace` means first-party files owned by a monorepo/workspace package;
- `repository` means root-level or otherwise unowned first-party files;
- the normal first-party default is both.

The first Sol run explicitly sent `origins: ["repository"]` on every call and
therefore excluded `packages/next/**`. Server instructions, every relevant MCP
origin schema, and the shipped skill now say to omit `origins` normally and
state that `repository` alone is not the whole repo.

## G18 generation and retrieval checks

Before targeted scouting:

- a broad `root layout parameter type generation` semantic query considered
  230 candidates, returned six compact handles in about 6.8 KB, and returned no
  supported artifact for the decisive helper;
- an exact `getRootParamsFromLayouts` support query returned
  `no_supported_memory` in about 1 KB rather than filling the response with
  analogies;
- a file-target dry run found 17 subjects, selected 17, and accurately
  reported that an eight-call cap would omit the helper ranked ninth. Exact
  anchor targeting is the intended path when that distinction matters.

Four exact-anchor Terra/high card calls completed successfully:

| Artifact | Anchor |
| ---: | --- |
| 596 | `NextTypesPlugin` |
| 597 | `getRootParamsFromLayouts` |
| 598 | `pluginState` |
| 599 | `createRouteTypesManifest` |

An exact-anchor semantic-memory query for `getRootParamsFromLayouts` then
returned only artifact 597 with `exact_anchor_support`. Exact artifact
drill-down returned its complete supported body and hash-verified evidence.
`embed --semantic-only` embedded 4/4 new artifacts without touching the code
vectors.

## Agent replay protocol

Each arm used:

- a fresh history-free exact-parent Next.js snapshot;
- dependency install and build performed by the replay harness;
- the same byte-shared augmented `memory-embed` database;
- the shipped jscout skill, with no forced-search treatment;
- high reasoning;
- the registered hidden typecheck oracle from the root-layout task.

The durable run root is:

`/Users/cristian/git/jscout-replay-runs/next-g17-g18-validation-2026-08-18`

## Results

| Run | Oracle | Seconds | Noncached input | Cached input | Output | Shell calls | jscout calls | MCP bytes | Attached artifacts |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Terra/high initial | fail | 602.9 | 206,846 | 4,612,096 | 25,191 | 32 | 4 | 53,970 | 5 |
| Sol/high initial | pass | 779.1 | 212,758 | 8,294,144 | 28,193 | 53 | 5 | 37,017 | 0 |
| Sol/high interface rerun | pass | 686.1 | 183,529 | 6,279,168 | 23,278 | 58 | 7 | 84,952 | 18 |

### Terra initial: relevant memory delivered, wrong contract chosen

Terra used both first-party origins. Its first search ranked
`getRootParamsFromLayouts` fourth and attached artifacts 597, 598, and 596.
It read the relevant source and implemented coherent union/optionality
semantics, but wrote `.next/types/server.d.ts`. The hidden oracle required
`.next/types/root-params.d.ts`; both fixtures failed with `ENOENT`.

The card accurately described existing behavior. It could not encode a future
output filename that did not exist in the parent snapshot. This is a boundary
of evidence-backed repository memory, not a reason to generate a more
confident unsupported card.

### Sol initial: pass without effective jscout retrieval

Sol passed, but every call used `origins: ["repository"]`. That excluded the
owned `packages/next` source and returned test/example surfaces with no attached
memory. Sol recovered through `rg` and direct source reads. This result is not
evidence that G17 or G18 helped.

### Sol interface rerun: correct incremental use and oracle pass

After clarifying origin semantics, Sol omitted `origins` on every call. It made
one overview and six incremental searches:

1. root-layout parameters and dynamic segments;
2. explicit type generation;
3. generated declarations across build/development;
4. `RootParams`;
5. `RouteTypesManifest` and layout routes;
6. `writeRouteTypesManifest` call paths.

The first broad query still attached three weakly related cards. Later queries
localized `typegen.ts`, `route-types-utils.ts`, `setup-dev-bundler.ts`,
`next-typegen.ts`, and the webpack plugin. They attached all four targeted
cards, including artifact 599 on the shared manifest path and artifacts
596–598 on the existing webpack-only behavior.

The agent moved generation to the shared manifest/writer path, retained
filesystem-distinct route-group roots, preserved scalar/catch-all types,
included `undefined` for optional/absent parameters, removed stale output, and
removed the superseded webpack-only output. The hidden oracle passed.

This remains one replay. It shows that the corrected interface can deliver and
be used; it does not establish that memory caused the pass. Sol had already
passed one run while jscout was effectively misconfigured.

## Transport observations

The successful rerun used 84,952 MCP bytes across seven calls, about 12.1 KB per
call. Six searches attached 18 artifact previews, with the same targeted cards
reappearing as queries narrowed. The agent did not call `semantic_memory` for a
full body; the compact summaries plus source reads were enough for its path.

There is no evidence for raising the 24 KB response default. The remaining
transport issue is repeated previews across adjacent searches, not insufficient
per-call budget. Session-aware deduplication would expand product state and is
not justified by this run; agents can disable attached memory after a useful
preview or drill into one artifact when needed.

## Harness findings

The external Playwright browser server started and published a valid endpoint,
but agent-invoked browser tests still attempted to launch Chromium inside the
sandbox. The runner injected `NEXT_TEST_BROWSER_WS_ENDPOINT` only into the
outer Codex process. Codex applies a separate shell environment policy to agent
subprocesses, so the variable did not reach the test command. The same gap
affected the teardown `NODE_OPTIONS` and process-registry variable.

The runner now passes task-declared variables and the four browser/teardown
variables through explicit `shell_environment_policy.set.*` CLI configuration,
in addition to the outer process environment. This follows the
[official Codex configuration contract](https://developers.openai.com/codex/config-reference/)
for explicit subprocess environment values. The runner test asserts each
generated setting. A minimal actual `gpt-5.6-sol` CLI probe then ran an inner
shell assertion against the configured `NEXT_TEST_BROWSER_WS_ENDPOINT` and
printed `BROWSER_ENV_OK`. The registered hidden oracle did not need Chromium
and passed before this harness correction.

The agent's optional dev-mode test still hit `EMFILE` despite the inherited
65,536 descriptor limit. That is not explained by the former low-ulimit bug;
it can reflect repository watcher fan-out or machine-wide concurrent watcher
pressure. This run does not justify another product change. A future dev-mode
evaluation should log the effective inner-shell limit and process/watch counts
at failure before claiming the descriptor wrapper is ineffective.

## Validation

Focused checks performed during review:

- real-corpus reindex and count comparison;
- vector/reranker-enabled searches against the augmented Next.js database;
- exact definition, property occurrence, mixed-query, and localized-memory
  queries;
- exact-anchor targeted scouting and exact artifact drill-down;
- semantic-only embedding of the four new artifacts;
- Terra and Sol real-agent replays plus a focused Sol interface rerun;
- hidden oracle grading for all three replays;
- replay-runner browser environment regression test.
- actual Sol CLI inner-shell environment probe (`BROWSER_ENV_OK`).

The final repository gate also runs formatting, warnings-as-errors Clippy, the
complete Rust suite, and the npm script suite before delivery.

## Conclusion

G17 now fixes exact-definition loss without letting one incidental occurrence
family consume mixed-query results. G18's hard support scopes, targeted card
generation, compact handles, and semantic-only embedding work on the real
corpus while reusing the expensive vector substrate.

The experiment does not show that more semantic memory is the next priority.
It shows three narrower facts:

1. missing indexed source cannot be repaired by retrieval or LLM memory;
2. supported targeted cards can reach an agent through incremental searches;
3. delivery is not sufficient to force the correct new contract, and one
   passing replay is not causal evidence.
