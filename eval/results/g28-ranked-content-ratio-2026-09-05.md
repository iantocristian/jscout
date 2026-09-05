# G28 ranked-hit content bytes — 2026-09-05

The retained majority-content condition is not met by these current outputs.
Next.js returns 33.43% snippet content by aggregate hit bytes, with only 13/90
individual hits above 50%. ai-pipe's aggregate is 65.97%, but only 36/75 hits
are majority content. A few large snippets do not establish a per-hit claim.
This measurement corrects the prior unsupported acceptance claim; it does not
change the threshold, retrieval behavior, or the retired replay decision.

## Definition and reproduction

For each compact ranked `semantic_search` hit:

- Numerator: UTF-8 bytes of `JSON.stringify(hit.snippet)`, minus the two
  surrounding quote bytes. JSON escapes belong to the source payload; the
  `snippet` key and string delimiters do not.
- Denominator: UTF-8 bytes of `JSON.stringify(hit)`, including every field,
  anchor, location, relation, and delimiter in the hit object.
- Majority means strictly greater than 50%. Report each hit and the aggregate
  `sum(snippet_bytes) / sum(hit_bytes)` separately. Empty results have no ratio.

The existing `hits_bytes` / `envelope_bytes` telemetry counts the whole hit
array versus the outer response, so it cannot answer this question. Neither
that envelope nor the array delimiters enter this per-hit denominator.

The scorer is [scripts/eval-ranked-content-ratio.mjs](../../scripts/eval-ranked-content-ratio.mjs).
It takes compact CLI JSON (the same hit projection as MCP), not debug JSON:

```sh
jscout search ROOT QUERY --database DB --lexical-only --no-memory --no-expand \
  -k LIMIT --response-bytes BUDGET --json |
  node scripts/eval-ranked-content-ratio.mjs

node scripts/eval-ranked-content-ratio.mjs response.json
node --test scripts/eval-ranked-content-ratio.test.mjs
```

The tests verify escaped Unicode bytes, metadata exclusion, byte-weighted
aggregation, per-hit failures, strict majority, empty results, and rejection
of exhaustive/debug-shaped output. Successful scorer execution is not a gate
pass; it reports the measurement without deciding acceptance.

## Inputs

- Binary: PR #116 at `24658dd` plus the accompanying review fixes, debug build;
  no retrieval or compaction changes. SHA-256:
  `411746c8894c047dd2ffd5fe059731795391dfba8376742b0806a905de61750f`.
- ai-pipe: fresh scratch index of the checkout at `ea13166c` (690 code files,
  169 documentation files), retaining its pre-existing non-code changes rather
  than claiming a pristine Git export. Every prompt from
  [ai-pipe-p0.json](../tasks/ai-pipe-p0.json) is used
  verbatim as a ranked query, plus `evaluateBrokerRiskPolicy` and
  `startScheduler`. All ten use limit 8 and budget 20,000 bytes.
- Next.js: scratch copy of the prepared `checker-embed` database from trial
  `g28e-1` of the [G28 replay](next-stale-development-cache-g28-2026-09-02.md),
  indexed from task parent `70f8b678` (22,282 code files, 1,206 documentation
  files). Queries 1–5 are every recorded ranked code query from the kept
  `g28b` forced and `g28e-1`/`g28e-2` skill arms, in that order; query 6 is the
  short-identifier probe `getHmrRefreshHash`. Query text and limits are fixed
  before scoring; none are omitted for failing the ratio.
- All current calls use `--lexical-only --no-memory --no-expand`, default
  repository-origin scope, and compact JSON. These are current fixed-query
  output probes, not replacements for the historical agent replay results.
  The original calls requested vectors; this measurement does not reproduce
  their provider-dependent ranking. Original explicit budgets are retained;
  query 5 uses the default 24,000-byte budget. No snippet was budget-truncated.
- Searches read indexed content from the scratch databases. Existing source
  repositories and their databases were not modified. Next.js's current root
  supplied the CLI root argument; the prepared database, not that checkout's
  current source, supplied the measured code state.

## Results

`Content %` is byte-weighted across the returned hits, not the mean of their
individual ratios. `Majority` is the count of hits with a ratio above 50%.

| ai-pipe query/task | Hits | Snippet bytes | Hit bytes | Content % | Majority |
|---|---:|---:|---:|---:|---:|
| lookup-broker-risk-policy | 8 | 4,192 | 5,718 | 73.31 | 6 |
| lookup-flow-graph-state | 8 | 6,459 | 8,203 | 78.74 | 7 |
| startup-orphan-reconciliation | 8 | 529 | 2,059 | 25.69 | 0 |
| pattern-journal-callers | 8 | 3,306 | 5,135 | 64.38 | 5 |
| macro-index-ibkr-path | 8 | 4,224 | 5,878 | 71.86 | 7 |
| runtime-secret-consumers | 8 | 529 | 2,059 | 25.69 | 0 |
| x-post-delivery-workflow | 8 | 4,193 | 6,173 | 67.92 | 5 |
| dynamic-node-dispatch-boundary | 8 | 7,635 | 9,492 | 80.44 | 6 |
| evaluateBrokerRiskPolicy | 8 | 1,558 | 4,146 | 37.58 | 0 |
| startScheduler | 3 | 385 | 1,174 | 32.79 | 0 |
| Total | 75 | 33,010 | 50,037 | 65.97 | 36 |

| Next.js query | Limit | Budget | Hits | Snippet bytes | Hit bytes | Content % | Majority |
|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | 12 | 16,000 | 12 | 1,889 | 4,810 | 39.27 | 2 |
| 2 | 10 | 16,000 | 10 | 1,326 | 4,865 | 27.26 | 0 |
| 3 | 20 | 20,000 | 20 | 3,053 | 7,754 | 39.37 | 5 |
| 4 | 30 | 30,000 | 30 | 4,474 | 12,936 | 34.59 | 1 |
| 5 | 12 | 24,000 | 12 | 1,743 | 3,817 | 45.66 | 5 |
| 6 | 8 | 20,000 | 6 | 931 | 5,952 | 15.64 | 0 |
| Total | — | — | 90 | 13,416 | 40,134 | 33.43 | 13 |

Exact Next.js queries:

1. `development cached server computations source edit stale fresh clients route handlers cache invalidation webpack turbopack`
2. `app route entry webpack reactServerComponents layer server component changed`
3. `development cached server computations stale after edit fresh clients curl route handlers invalidation`
4. `development server compilation content hash cache invalidation source change use cache HMR`
5. `development server cached server computations remain stale after source edit for curl fresh clients route handlers cache invalidation`
6. `getHmrRefreshHash`

## Evidence and limits

All 16 compact response files and the exact CLI argument manifests are retained
outside the repository at `/private/tmp/jscout-pr116-ratio.qfZWqX/`. Re-running
all 16 calls with the rebuilt review-fix binary returned identical JSON.

Code snapshots:

- ai-pipe: `caf8a36e14f448c5de2f91ccedf115f8064662c836ac839e1112cd98f2d0ffaf`
- Next.js: `88cb36c1bd042484fe219a25ad0322b101ce904bc0adf79245b561f2bda9baaf`

Response-set SHA-256 hashes (concatenate `id + "\n" + raw response file` in
each `*-cases.json` manifest's order):

- ai-pipe: `152301759f9009ec20660fa55e51799babcb819c4f7d7639bc2d88ac0edfec27`
- Next.js: `dc72d5802d3122070491478c1eec72b14f3be7aeb2dcd21d810fb6f181102199`

This is a small descriptive JS/TS code-output sample, not a retrieval-quality,
latency, token-cost, adoption, documentation, or Rust evaluation. Query results
can overlap; hit counts are observations, not distinct chunks. The current
renderer retains up to four source lines per hit, while locations, anchors,
symbols, and relations remain metadata. Both source-line length and metadata
size affect the ratio; a large aggregate alone can hide many short-hit
failures. The measurement does not imply that adding source solely to improve
the ratio or removing useful metadata is the right product decision.
