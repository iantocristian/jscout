# Eight-line ranked snippets and a 30,000-byte default — 2026-09-05

Follow-up to the [query-aware snippet measurement](query-aware-snippets-2026-09-05.md).
Control is PR #117 at `548ef49`: four lines / 512 source bytes and a
24,000-byte default response ceiling. Treatment raises the excerpt bounds to
eight lines / 1,024 UTF-8 bytes, including clipping ellipses, and the default
code-search response ceiling to 30,000 bytes (+25%). Selector logic and
relation previews are unchanged. Short chunks are not padded.

The response budget covers the complete serialized search result, not just
source text or a token allowance. Repository configuration and explicit
CLI/MCP limits still win. The example configuration uses the new default;
existing configuration files are not rewritten. Documentation search and
other tools retain their budgets, including four non-search call sites that
previously reused the search constant. CLI unbounded-debug behavior remains
unchanged. No database migration or reindex is needed.

## Paired fixed-query check

Same 16 queries and scratch databases as the preceding measurement: ai-pipe
(690 code files) and the prepared Next.js replay database (22,282 code files).
Exact corpus versions, queries, limits, and original response budgets are in
the [initial ratio report](g28-ranked-content-ratio-2026-09-05.md). All calls
remain lexical-only, no memory/expansion, compact JSON. Original explicit
budgets are retained; Next.js query 5 exercises the changed default. None of
these 16 responses reaches its byte ceiling in either variant.

For every pair, removing only `snippet` and `snippet_line` makes the complete
responses equal. All 165 hit observations preserve order, identities,
snapshots, and relation previews. Every new excerpt was matched to indexed
`chunks.content`, its first source line verified, and both eight-line and
1,024-byte bounds checked. Three additional runs per binary/query returned
identical JSON. The repositories and their original databases were not edited.

| Measure | ai-pipe 4-line | ai-pipe 8-line | Next.js 4-line | Next.js 8-line |
|---|---:|---:|---:|---:|
| Hit observations | 75 | 75 | 90 | 90 |
| Snippet payload bytes | 22,290 | 41,955 | 20,013 | 31,314 |
| Complete hit bytes | 40,202 | 59,830 | 48,362 | 59,661 |
| Complete response bytes | 42,307 | 61,935 | 49,670 | 60,969 |
| Byte-weighted source share | 55.45% | 70.12% | 41.38% | 52.49% |
| Individual majority-content hits | 39/75 | 51/75 | 33/90 | 62/90 |
| Any literal query term visible | 75 | 75 | 90 | 90 |
| Distinct query-term observations | 305 | 388 | 324 | 356 |

Complete response bytes grow 46.39% on ai-pipe and 22.75% on Next.js. The
three identifier-only queries retain visibility in all 17 returned hits.
Distinct-term visibility never decreases in this cohort; natural-language
queries contain generic terms, so that is a lexical proxy, not a quality grade.
The source-share scorer is the existing tested
[script](../../scripts/eval-ranked-content-ratio.mjs), using escaped source
payload bytes divided by complete hit bytes.

Manual checks: `startScheduler` now shows more default parameter values, and
Next.js `createUseCacheStore` shows adjacent revalidation/draft-mode fields
after the matching HMR call. Both remain partial source excerpts; an eight-line
window does not promise a complete declaration or eliminate `definition` calls.
Aggregate source share now exceeds 50% in both repositories, but individual
hits still fall below it. This does not close or waive the G28 per-hit condition.

## Real byte-pressure probe

The broad query `export` with limit 100 deliberately exceeds the budgets.
It is a truncation test, not representative retrieval-quality evidence.
Other flags and databases are the same as above.

| Repository | 4 lines, old 24,000 default | 8 lines, explicit 24,000 | 8 lines, new 30,000 default |
|---|---:|---:|---:|
| ai-pipe returned hits / response bytes | 54 / 23,650 | 47 / 23,733 | 57 / 29,586 |
| Next.js returned hits / response bytes | 58 / 23,598 | 47 / 23,792 | 63 / 29,915 |

The new default equals an explicit 30,000-byte request, stays within that
ceiling, and retains the entire hit prefix from the explicit 24,000-byte
request. Increasing excerpt size without raising the ceiling would retain
fewer hits in these pressure probes; the approved budget increase compensates
here. It is not a guarantee of equal hit counts for every possible query.

## Verification and artifacts

- `cargo test --all-targets --all-features`: 764 passed.
- Eleven selector tests, including the exact 1,024-byte boundary, eight-line
  clipping/fallback, UTF-8, source offsets, and unchanged short excerpts.
- MCP eight-line/anchor round trip and unchanged exhaustive hit shape;
  complete-response default/explicit budget tests; config/CLI override tests;
  existing JSON escaping and cross-plane differential tests pass.
- Clippy with warnings denied, formatting, and diff checks pass.
- `node --test scripts/eval-ranked-content-ratio.test.mjs`: five passed.

Raw responses, CLI manifests in `measurements.json`, and the scratch paired
runner are retained at `/private/tmp/jscout-snippet-budget.eYMgOG/`. Original
input manifests are in `/private/tmp/jscout-pr116-ratio.qfZWqX/`. The runner is
not a checked-in portable harness. SHA-256:

- Control binary: `5206496f285be3b1549c530fa06dd20d4546426a8a47fd337f225efc0a4e221e`
- Treatment binary: `d91de10b748559ea9de12c4c5001e592c777ddcfc16740dd06530f07df99983d`
- Runner: `d8102b0c1333812f3219df02ed4eb0886f65d2d91afd5520f2b19734e4465ed6`
- Measurements: `2c4831ac10e314a8a73bdeeceb3daf107aa553951b33dd5c9d899e4d79f2cd53`

No live agent, provider-backed retrieval, or timing evaluation was run in
this follow-up. These results establish output size, literal visibility,
source fidelity, and budget behavior on the sampled code, not better answers
or fewer follow-up calls.
