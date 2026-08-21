# G20b Next.js full-posture path transport

- Date: 2026-08-21
- Status: prospective real-corpus full-posture check complete
- Corpus: Next.js parent `1d8e326d1b360da4a439cf440316fe76a359bfd3`
- Source snapshot: `/private/tmp/jscout-g17-g18-review.5ZhBrH`
- Isolated database: `/private/tmp/jscout-g20b-next-full-posture.db`
- Raw responses and timing logs: `/private/tmp/jscout-g20b-next-full-posture-results`

## Corpus and retrieval posture

The database was an APFS clone of the retained `memory-embed` preparation for
the root-layout-parameter task. The preserved preparation was not mutated. The
clone was migrated to schema v24 and refreshed with the G20b binary:

- 19,758 indexed files;
- 53,722 chunks;
- 98,196 references;
- 11 expected rejected parser-error or unsupported-Flow fixtures;
- 449 cards, 86 workflows, and 64 summaries; and
- 599 cached semantic embeddings.

`embed --product --semantic` reused 11,522 current code documents, embedded
1,853 missing product documents, synchronized 15,013 selected product
occurrences, reused all 599 semantic vectors, and synchronized all 599 semantic
occurrences. The resulting active profile contains 13,506 code documents and
16,423 indexed code occurrences; cached non-product occurrences are retained
rather than destructively pruned.

Every measured search explicitly enabled:

- local BGE-M3 vector retrieval;
- the local `BAAI/bge-reranker-v2-m3` cross-encoder;
- evidence-connected attached memory with four artifact slots; and
- structural expansion with six hits, depth two, three seeds, eight paths,
  40 nodes, 80 edges, and 24 KB graph and whole-response bounds.

Timing logs confirm `embed-query+sqlite-vec` and `rerank(50)` ran successfully
for all eight calls. No vector or reranker degradation occurred.

## Fixed calls

Projection order was counterbalanced by query. Each query ran once with
`--expand-mode paths` and once with `--expand-mode neighborhood`; every other
argument and the immutable database were identical.

| Query | Neighborhood bytes | Path bytes | Reduction | Neighborhood nodes/edges | Path nodes/edges | Memory artifacts |
|---|---:|---:|---:|---:|---:|---:|
| `root-layout parameters and dynamic segments` | 21,498 | 10,068 | 53.2% | 40 / 49 | 12 / 10 | 4 / 4 |
| `explicit type generation build development declarations` | 18,965 | 6,798 | 64.2% | 40 / 72 | 11 / 8 | 0 / 0 |
| `RouteTypesManifest layout routes writeRouteTypesManifest` | 20,181 | 9,926 | 50.8% | 40 / 63 | 12 / 9 | 4 / 4 |
| `createRouteTypesManifest getRootParamsFromLayouts collectedRootParams NextTypesPlugin` | 21,866 | 10,280 | 53.0% | 40 / 73 | 13 / 10 | 4 / 4 |
| **Aggregate** | **82,510** | **37,072** | **55.1%** | **160 / 257** | **48 / 37** | **12 / 12** |

Path projection reduced aggregate graph nodes by 70.0% and edges by 85.6%.
All four pairs preserved the same ordered hit anchors and the same delivered
memory artifact IDs. Three pairs delivered four connected artifacts in both
modes. The second query had no evidence-connected artifact in either mode;
that is a matched no-attachment result, not a response-budget omission.

## Defect exposed and fixed

The first pre-fix replay falsified one G20b invariant. Search builds its
candidate graph as the union of a bounded neighborhood for each returned seed.
Under one global 40-node projection budget, path mode retained the cross-file
`getPageFromPath` continuation while neighborhood mode spent that slot on a
higher-ranked continuation from another seed. The path node and edge were real,
but the returned diagnostic neighborhood was not a strict superset under the
same bounds.

Neighborhood projection now reserves the compact path forest first and spends
only the remaining node, edge, and byte budget on ranked fan-out. A deterministic
multi-seed fixture reproduces the former starvation shape. After the fix and a
complete eight-call rerun, every path node and normalized path edge is present
in the paired diagnostic neighborhood for all four queries.

## Claim boundary

This satisfies the requested prospective full-posture corpus data point: code
vectors, semantic vectors, reranking, attached-memory selection, and expansion
all operated together on a large real repository. It supports the path default
at materially lower bytes while preserving hits and delivered memory.

It does not satisfy G20's registered 60% fixed-call aggregate gate. The 55.1%
reduction is below that threshold, and these four Next.js calls are not the
unavailable 42-call architecture inquiry or 19-call TargetsQueue workload.
Those historical gates remain explicitly open rather than being replaced by
this check.
