# ai-pipe semantic-memory scale and index experiment — 2026-08-22

This result closes the deferred semantic-history index question from the local
performance baseline. The [machine-readable report](./semantic-memory-ai-pipe-2026-08-22.json)
is canonical; the reusable runner is
[`bench/perf/semantic-memory.mjs`](../perf/semantic-memory.mjs).

## Decision

- **P1: optimize semantic discovery by bounding artifact hydration.** At 25,000
  current artifacts, recent and common lexical discovery take 2.76–2.77 s
  median even though the response returns 20 handles. The implementation ranks
  candidate IDs, loads and freshness-checks the entire surviving candidate
  set, and only then truncates it. Another ordinary SQLite index cannot remove
  that work.
- **P2: add `semantic_artifacts(scout_run_id)` as a persistence/reuse index.**
  At 29,999 total artifact rows it changes the relevant plans from table scans
  to covering lookups, costs 307,200 bytes, and reduces actual zero-model card
  reuse by about 2–3 ms. It does not speed up `semantic_memory` queries and
  should not be presented as a semantic-search optimization.
- **Do not add another semantic-query index from this result.** Existing exact
  relation/support paths remain tens of milliseconds at the largest fixture.
  The measured broad-query bottleneck is application-side hydration and
  freshness work.

## Provenance and method

| Item | Value |
|---|---|
| Harness source | `6a2ede3a649590c456a258427b1dce3f6f044085` (clean) |
| Release binary SHA-256 | `e63cbdb6dfefc974891fd68c5048e7c4b600ebf3fe425066d06d64fd5b3e1b12` |
| JScout version | `0.4.0` |
| ai-pipe revision | `ea13166c59cfc52574e96959413f5c54be20e8c8` |
| Host | Apple M5 Pro, 18 logical CPUs, 64 GB, arm64 |
| Runtime | Node.js 24.15.0, Rust 1.97.1, SQLite 3.51.0 |
| Samples | 20 measured and 3 warmups per semantic case |
| Retrieval | Persistent MCP, lexical semantic memory, vectors disabled |
| Model traffic | Zero remote requests; one deterministic local fake-gateway publication |

The ai-pipe working checkout was dirty, but the harness staged the exact pinned
revision with `git archive`, recorded the status before and after, and did not
write the checkout. Filesystem cache and host load were uncontrolled.

## Deterministic fixture

The runner publishes one real card through the fake gateway, then validates 31
additional source supports through the production annotation path. Generated
rows rotate four supports across those 32 distinct files and context hashes.
Each scale includes 20% superseded history and one current artifact with 40
relations for exact-detail/source testing.

| Current artifacts | Historical artifacts | Total artifacts/runs | Supports | Relations | Database bytes |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 199 | 1,199 | 4,793 | 40 | 41,689,088 |
| 5,000 | 999 | 5,999 | 23,993 | 40 | 53,059,584 |
| 25,000 | 4,999 | 29,999 | 119,993 | 40 | 109,797,376 |

Every fixture passed `PRAGMA integrity_check`. The exact full/source case also
verified current source and context freshness through the public query surface.

## Semantic-memory scale results

All values are persistent-MCP round-trip milliseconds. Repeated responses were
byte-stable for every case.

| Case | 1k median | 5k median | 25k median | 25k p95 |
|---|---:|---:|---:|---:|
| Recent discovery, 20 handles | 128.36 | 593.99 | 2,760.17 | 2,892.41 |
| Common lexical query, 20 handles | 123.26 | 590.09 | 2,774.02 | 2,852.19 |
| Selective lexical query | 14.97 | 70.52 | 334.51 | 350.53 |
| Lexical miss | 0.88 | 4.95 | 23.73 | 25.95 |
| Selective anchor scope | 29.58 | 142.31 | 676.16 | 707.69 |
| Directly related artifacts | 6.16 | 8.65 | 20.04 | 23.55 |
| Exact artifact body | 6.17 | 8.46 | 18.51 | 20.81 |
| Exact full artifact plus source | 17.01 | 18.32 | 27.92 | 32.56 |

The candidate counts explain the split at 25k:

- recent and common discovery hydrate 25,000 current artifacts before returning
  20;
- the selective lexical case hydrates 3,124 matched artifacts;
- the anchor case hydrates 3,126 directly supported artifacts;
- the lexical miss still scans/ranks the corpus but hydrates none, completing
  in 23.73 ms median;
- related and exact cases operate on 40 and one candidate respectively.

The 25k/1k median ratio is 21.5× for recent discovery, 22.5× for a common
lexical query, 22.3× for the selective lexical query, and 22.9× for the anchor
scope. This is close to fixture cardinality growth and is consistent with the
current load-before-truncate control flow. Output serialization is not driving
the increase: result sizes remain essentially constant at each response limit.

The safe optimization experiment is to hydrate ranked candidates in bounded
batches until the requested number survives freshness filters. It must preserve
stable ranking, freshness semantics, exact diagnostic candidate counts (or
explicitly revise that contract), response-budget behavior, and byte parity for
untruncated results. Exact-ID calls should also use a query shape that lets
SQLite seek the primary key instead of retaining the generic nullable-filter
scan.

## `scout_run_id` candidate index

The candidate was created only in a copied benchmark database:

```sql
CREATE INDEX candidate_semantic_artifacts_scout_run
ON semantic_artifacts(scout_run_id);
```

At 29,999 artifact rows it added 75 4-KiB pages (307,200 bytes, about 0.28% of
the full fixture database). Logical table signatures and integrity checks
matched before and after index creation.

| Path | Baseline median | Indexed median | Qualification |
|---|---:|---:|---|
| 2,000 run-ID lookups in one SQLite process | 7,206.57 ms | 13.28 ms | 20 paired samples; process startup amortized |
| Actual card reuse CLI | 79.85 ms | 77.51 ms | 40 paired samples; includes process/gateway startup |
| Semantic recent discovery | 3,058.76 ms | 3,130.41 ms | 20 paired samples; byte-identical |
| Semantic selective lexical | 390.66 ms | 385.36 ms | 20 paired samples; byte-identical |
| Semantic anchor scope | 766.33 ms | 784.56 ms | 20 paired samples; byte-identical |

The lookup batch is about 543× faster because the baseline performs 2,000 full
artifact scans while the indexed arm performs covering lookups. That ratio is
not an end-to-end product claim. In the real card-reuse path the separate-arm
median difference is 2.34 ms (2.9%); the median paired delta is 2.98 ms in favor
of the index, and the indexed arm is faster in 29 of 40 pairs. Every reuse made
zero model calls and left semantic content unchanged.

SQLite plans confirm the intended mechanism:

- direct run lookup: `SCAN semantic_artifacts` becomes `SEARCH ... USING
  COVERING INDEX ... (scout_run_id=?)`;
- the `reusable_run` left join changes from `SCAN artifact LEFT-JOIN` to the
  same covering index lookup;
- the existing one-successor index continues to serve supersession checks.

The paired semantic-query arms show no consistent gain and the responses are
byte-identical. That is expected: lexical discovery does not filter or join on
`scout_run_id`.

## Limits

- This is one machine under uncontrolled host load; absolute timings are not a
  CI threshold or cross-machine estimate.
- The generated history is deterministic and structurally valid, but its body
  lengths, support distribution, and 20% history ratio are an explicit fixture,
  not a measured production distribution.
- Semantic-vector retrieval is disabled. The result makes no latency or
  quality claim about a populated semantic vector index.
- The index arm changes schema only in an isolated copied database. No
  production schema/index change is included in this branch.

## Reproduction

```sh
cargo build --release --locked
node bench/perf/semantic-memory.mjs \
  --repo /path/to/ai-pipe \
  --binary target/release/jscout \
  --scales 1000,5000,25000 \
  --samples 20 \
  --warmups 3 \
  --output /tmp/jscout-semantic-memory.json
```

The output path must remain outside the JScout and ai-pipe source trees. The
checked JSON contains no absolute developer paths.
