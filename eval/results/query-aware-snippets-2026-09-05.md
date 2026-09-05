# Query-aware ranked snippets — 2026-09-05

Paired output check on the same small ai-pipe and large Next.js databases and
16 fixed queries used in the [G28 content-ratio measurement](g28-ranked-content-ratio-2026-09-05.md).
This validates source presentation, not ranking quality or agent task success.
The G28 majority-content requirement and relation previews remain unchanged.

## Treatment and invariants

Control is the first four source lines. Treatment uses the existing FTS
tokenizer to locate literal matches in the selected chunk. Exact-tier hits
prefer their matched identifiers; other hits consider the query's FTS terms.
It selects a four-line window with the most distinct case-folded matching
token spellings, preferring one context line before the first match, then
earlier source order. No literal source match keeps the header fallback.

Excerpts are capped at 512 UTF-8 bytes, including clipping ellipses. Long
windows are clipped around the first match, so not every matching term in the
selected window is guaranteed to survive clipping. This is a rendering bound,
not a threshold chosen to achieve majority-content output. `snippet_line`
locates a moved excerpt; chunk `at`, anchors, ranking, and relations are
unchanged. Source-byte limits and the existing complete JSON response budget
are tested independently because escaping can expand source bytes.

All 16 calls use the original query, limit, budget, and database with
`--lexical-only --no-memory --no-expand --json`. On every pair:

- Removing only `snippet` and `snippet_line` makes the entire responses equal.
  All 165 hit observations retain order, identity, relations, and snapshots.
- Every new excerpt was matched back to the indexed `chunks.content`, and its
  reported starting line was verified against that source, including clipped
  excerpts. All 165 passed; none exceeds 512 UTF-8 bytes.
- Ten further calls per binary and query returned identical JSON, alternating
  control/treatment order between repetitions. Timings below exclude the
  initial untimed pair. No compiler or test suite was run by this task during
  the final timing pass; the host was otherwise uncontrolled.

## Identifier visibility

These three queries name code identifiers directly. Visibility means that the
actual snippet, not its metadata or relation previews, contains the requested
identifier under the same FTS tokenizer.

| Query | Returned hits | Visible before | Visible after |
|---|---:|---:|---:|
| ai-pipe: `evaluateBrokerRiskPolicy` | 8 | 5 | 8 |
| ai-pipe: `startScheduler` | 3 | 1 | 3 |
| Next.js: `getHmrRefreshHash` | 6 | 1 | 6 |
| Total | 17 | 7 | 17 |

Manual source spot-check: the Next.js `createUseCacheStore` hit previously
showed only a multi-line signature. It now includes
`hmrRefreshHash: getHmrRefreshHash(outerWorkUnitStore)` at the unchanged chunk
location, with `snippet_line: 676`. `runValidationInDevImpl` previously showed
four lines of its JSDoc; it now shows the `getHmrRefreshHash` call with
`snippet_line: 6139`. The definition hit still shows its matching signature.

## Complete cohort

"Any term visible" counts hits whose snippet matches at least one query term.
"Distinct term observations" sums distinct visible query terms per hit. Both
use an in-memory FTS table with the production tokenizer, not substring tests.
Natural-language prompts include common words, so these are literal-coverage
proxies, not relevance grades. Mixed-query exact tiers can also contain
incidental capitalized words; the identifier-only table above avoids treating
those as evidence of task-specific identifier localization.

| Measure | ai-pipe before | ai-pipe after | Next.js before | Next.js after |
|---|---:|---:|---:|---:|
| Hit observations | 75 | 75 | 90 | 90 |
| Any term visible | 69 | 75 | 57 | 90 |
| Distinct term observations | 252 | 305 | 110 | 324 |
| Snippet payload bytes | 33,010 | 22,290 | 13,416 | 20,013 |
| Complete hit bytes | 50,037 | 40,202 | 40,134 | 48,362 |
| Complete response bytes | 52,142 | 42,307 | 41,442 | 49,670 |
| Byte-weighted content ratio | 65.97% | 55.45% | 33.43% | 41.38% |
| Individual majority-content hits | 36/75 | 39/75 | 13/90 | 33/90 |

ai-pipe response bytes fall 18.86%; Next.js response bytes grow 19.85%, including
the excerpt locations. This is not a universal byte reduction. The ai-pipe
`macro-index-ibkr-path` query loses one distinct-term observation (45 to 44)
while improving exact-tier identifier visibility (1/2 to 2/2); not every
coverage measure improves in every query. No hit count or complete-response
budget changed in this cohort.

The majority-content acceptance is still unmet. These measurements neither
waive it nor infer that increasing the ratio is itself an improvement.

## Timing and verification

Debug CLI processes, ten repetitions per binary per query, one macOS host.
The per-query median uses sorted sample index `floor(n/2)` (the upper middle
for ten samples). Reported times include process launch and database reads;
this is a descriptive smoke, not a latency gate or a provider benchmark.

| Query | Before median ms | After median ms |
|---|---:|---:|
| ai-pipe: lookup-broker-risk-policy | 41.8 | 46.5 |
| ai-pipe: lookup-flow-graph-state | 41.1 | 46.7 |
| ai-pipe: startup-orphan-reconciliation | 48.4 | 49.7 |
| ai-pipe: pattern-journal-callers | 43.3 | 48.7 |
| ai-pipe: macro-index-ibkr-path | 53.0 | 58.2 |
| ai-pipe: runtime-secret-consumers | 43.6 | 44.2 |
| ai-pipe: x-post-delivery-workflow | 46.5 | 54.3 |
| ai-pipe: dynamic-node-dispatch-boundary | 54.9 | 61.6 |
| ai-pipe: evaluateBrokerRiskPolicy | 20.2 | 21.5 |
| ai-pipe: startScheduler | 14.6 | 15.8 |
| Next.js: 1 | 161.0 | 174.0 |
| Next.js: 2 | 147.1 | 151.2 |
| Next.js: 3 | 132.6 | 143.0 |
| Next.js: 4 | 231.5 | 233.3 |
| Next.js: 5 | 113.8 | 124.4 |
| Next.js: getHmrRefreshHash | 68.8 | 68.1 |

Tests: 762 Rust tests pass, including ten selector tests, the MCP snippet-line
and anchor round trip, compact field omission, and JSON-budget truncation.
Selectors cover late matches, exact-identifier preference, repeated terms,
deterministic ties, path/vector-only fallback, case and diacritic matching,
identifier boundaries, delimiter collisions, NUL, CRLF, UTF-8 clipping, and
36 combinations of match position and line width. The existing cross-plane
differential and exhaustive-search tests also pass. Clippy with warnings
denied and formatting checks pass. No database migration or reindex is needed.

## Artifacts and limits

Based on merged main `b2a110c`; control retrieval/compaction is the byte-identical
pre-snippet implementation whose binary was recorded in the earlier report.
Corpus versions, exact queries, limits, budgets, and code snapshots are in
that report. Existing repositories and their databases were not modified.

The paired runner, before/after JSON, and raw timings are kept outside the
repository in `/private/tmp/jscout-snippet-eval.cMYjRH/`; the exact input CLI
manifests remain in `/private/tmp/jscout-pr116-ratio.qfZWqX/`. The runner asserts
metadata/order equality, source location, and byte bounds before recording a
result. The scratch runner and raw files are not a checked-in portable replay
harness. SHA-256:

- Control binary: `411746c8894c047dd2ffd5fe059731795391dfba8376742b0806a905de61750f`
- Treatment binary: `5206496f285be3b1549c530fa06dd20d4546426a8a47fd337f225efc0a4e221e`
- `measure.mjs`: `8abe6fea61b4a3eefeab884826b9ed07ca4926770b99afe22db13275b8063439`
- `measurements.json`: `f041773c7fb1c658881d1111576679f4b10afb04f19bc4e7ad84f34915adb669`

No live agent or provider-backed retrieval was rerun. Semantic-only fallback
is covered by selector tests, not a new embedding evaluation. The evidence
supports better literal visibility and correct source presentation on the
sampled hits, not better end-to-end answers or fewer follow-up calls.
