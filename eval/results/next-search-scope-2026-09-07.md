# Next.js search scope and broad-query guard replay — 2026-09-07

## Outcome

The confirmed OR escape hatch preserves every first-page hit from the three saved
broad queries. The default AND sets are strict subsets of the original OR sets;
there is no silent OR fallback. The guard returns real nonzero counts with
`confirmation_required: true`, `truncated: true`, no hits and no cursor.

For the **two actual September 5 calls**, if requested as **explicit OR under
the guard**, counts-first responses would reduce canonical payload from
**42,991 to 1,545 bytes** (96.4% less). Their original requests,
replayed unchanged, instead use the new AND default: **35,540 combined bytes
(17.3% less)**. The guard saving is not the automatic effect of replaying old
requests. Those calls accounted for 36.1% of canonical MCP bytes across the
September 5 six-run campaign. These are query-response comparisons, not evidence
of solver-time, token or correctness improvement: no solver or scouting run was
repeated.

**AND is a meaning change, not a guaranteed byte reduction.** `root-params` narrows
from 3,821 to 249 chunks, but its first AND page is **34,771 bytes versus 30,400**
for the original OR page. It has 830 reported match-line entries across 141 hits,
versus 437 across 200 OR hits. The 35,000-byte envelope sheds the AND page to a
shorter prefix; it still has a real continuation cursor. Other saved AND queries
are much smaller. Do not generalize the guard's byte saving to every AND query.

## Method and inputs

- Baseline: frozen `9947f4d` product binary from the September 6 instruction run.
- Candidate: `409af154e31b3b4c8b6bd1869dec5b20ef4c4767`, based on
  `f4e95c9cabda282ba03571753255f267a1987b8d`. The executable was built and tested
  before committing those changes; its exact binary hash is recorded below.
- Read-only copies of the existing September 6 prepared databases: the root
  case has 20,718 code files / 56,329 code chunks and 1,148 documentation files /
  7,080 documentation chunks. The second database is the existing optimistic
  prefetch case. No filesystem inventory or reindex was performed.
- 26 request variants, one discarded warmup each, five measured CLI processes
  each. Request order alternates forward/reverse by round. Results below report
  median wall time **including process startup, SQLite opening and serialization**.
  Five warm measurements are a sanity check, not a performance benchmark.
- Compact serialized JSON bytes exclude the terminal newline. Full hit arrays
  were compared, not just counts. Direct read-only FTS queries checked complete
  AND/OR chunk-set inclusion. Every repeated response was byte-stable.
- Explicit database paths and a config-free root; searches were exhaustive,
  lexical-only or path-only. **No provider, model, embedding, index or scouting
  calls. Both original and copied database SHA-256 hashes remained unchanged.**

[Machine-readable requests, timing samples, output hashes and checks](next-search-scope-2026-09-07.json)
contain all exact commands, budgets, scopes, binary paths and input hashes.

## Saved broad queries

| Query | Behavior | Returned / matching chunks | Canonical bytes | Median process ms |
| --- | --- | ---: | ---: | ---: |
| `root-params` | Baseline OR | 200 / 3,821 | 30,400 | 22.8 |
| `root-params` | Default AND | 141 / 249 | 34,771 | 32.3 |
| `root-params` | Explicit OR, guarded | 0 / 3,821 | 768 | 12.9 |
| `root-params` | Explicit OR, confirmed | 200 / 3,821 | 30,479 | 23.2 |
| `root-params.d.ts` | Baseline OR | 100 / 5,743 | 12,591 | 25.3 |
| `root-params.d.ts` | Default AND | 1 / 1 | 769 | 10.0 |
| `root-params.d.ts` | Explicit OR, guarded | 0 / 5,743 | 777 | 15.0 |
| `root-params.d.ts` | Explicit OR, confirmed | 100 / 5,743 | 12,670 | 26.3 |
| `optimistic route` | Baseline OR | 100 / 2,711 | 14,565 | 19.6 |
| `optimistic route` | Default AND | 12 / 12 | 5,323 | 10.9 |
| `optimistic route` | Explicit OR, guarded | 0 / 2,711 | 773 | 12.8 |
| `optimistic route` | Explicit OR, confirmed | 100 / 2,711 | 14,644 | 20.2 |

The baseline replay reproduces the archived response byte counts exactly:
30,400 and 12,591 for the September 5 pair, and 14,565 for the optimistic query.
This is not a whole-response comparison against the original run's snapshot
identities. Comparing baseline and candidate on the same frozen database, all
three confirmed-OR hit arrays are byte-equivalent after canonicalizing JSON;
the 79 extra envelope bytes describe the new operator/confirmation fields and
warning. Single-identifier exhaustive responses gain 49 envelope bytes.

Guards remove delivery and hit-projection work; they still count the scoped
matching set. A confirmation followed by full delivery would add the small guard
response to the normal response—it is an explicit choice, not a claim that every
OR traversal becomes cheaper.

## Unchanged queries

| Query | Equal hits, both binaries | Baseline → candidate bytes | Median process ms |
| --- | ---: | ---: | ---: |
| `getRootParamsFromLayouts` | 2 | 937 → 986 | 9.2 → 9.2 |
| `writeRouteTypesManifest` | 8 | 3,277 → 3,326 | 9.5 → 9.6 |
| `rootParamKeys` | 10 | 7,309 → 7,309 | 43.4 → 44.0 |

The first two are exhaustive single identifiers. `rootParamKeys` is ranked
lexical search with memory and expansion disabled; its entire hit array and
payload size are unchanged. This does not evaluate vector or model-reranker
quality; scoped native-vector behavior is covered by product regression tests.

## Scoped navigation

| Probe | Hits | Canonical bytes | Median process ms |
| --- | ---: | ---: | ---: |
| Code exact path + identifier | 1 | 1,001 | 16.4 |
| Code prefix + text | 10 | 7,034 | 36.3 |
| Code exact path only | 4 | 2,991 | 19.8 |
| Code prefix only | 10 | 5,749 | 33.2 |
| Docs exact README path only | 4 | 2,190 | 11.4 |
| Docs prefix only | 6 | 8,032 | 13.9 |
| Docs prefix + text | 6 | 7,561 | 11.9 |
| Missing root README path | 0 | 327 | 11.6 |

The code probes use exact
`packages/next/src/server/lib/router-utils/route-types-utils.ts`, exact
`packages/next/src/server/request/root-params.ts`, and directory prefix
`packages/next/src/server/request/`. Every primary hit is inside its echoed scope.
There is no measured exact-path latency problem here that justifies additional
SQL complexity.

`apps/docs/README.md` is indexed and returns its four chunks directly. The
documentation prefix is `docs/01-app/`; all returned paths stay inside it. Its
six-hit path-only response correctly reports truncation. Repository-root
`README.md` is **not indexed in this database** and returns an honest scoped
zero-hit response. Path filters do not resurrect absent or excluded files.

Selected documentation files are absent beneath the isolated replay root, so
the existing source verifier reports `source_mismatch` and serves cached indexed
content. These results establish indexed membership/navigation, not current
filesystem-source validation. Tests separately cover current source files.

## Follow-through and limits

1. Land the scope, explicit-OR guard and AND-default behavior with these checks.
   Preserve explicit OR and the visible refinement/confirmation response.
2. Keep the AND byte-growth example in the evidence: tighter matching is useful,
   but payload efficiency depends on which chunks and locators survive.
3. Do not claim solved-case, time-to-solve or token savings from this replay.
   A later solver experiment must measure actual treatment use and source
   duplication independently; it was deliberately not run here.

## Exact fingerprints and reproduction

```text
baseline binary  332a614077b1068f5d66ec5147ca6a13e22d2255ea17ab4071cfbbf2c2875967
candidate binary 56659f53b7e0ff49e22075d3a33a562c96f87aa09482db9b282baa8033fa2a15
root-params DB   3d7be7a0f547eb7cd03481bff8cf662f30ea7f443918f73aab24caab78c921f3
prefetch DB      0d785147de4cdc83fed6acbdec71277f01caea7b03780d06d61a1bd7370f9b14
```

Each JSON request includes a copyable CLI command. The local query-only driver
used for all checks is `/private/tmp/jscout-query-replay.0Wf5kC/replay.py`:

```sh
python3 /private/tmp/jscout-query-replay.0Wf5kC/replay.py
```

It writes temporary raw responses and evidence, rejects changed input database
or binary hashes during a run, and never invokes index/embed/scout/solver tools.
The retained raw-response paths are recorded in the JSON. Historical attribution
comes from [the September 5 discovery audit](next-astra-discovery-audit-2026-09-06.md).
