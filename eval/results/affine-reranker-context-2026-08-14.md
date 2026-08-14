# AFFiNE contextual reranker smoke

Date: 2026-08-14  
Corpus: AFFiNE using the preserved 685 MB index  
Change under test: role pre-filtering plus occurrence-context reranker documents

## Constraint

The preserved database contains the embedding profile that predates the
content-only `content-v2` document format. Every search therefore reported
`retrieval.vector=degraded` and fell back to lexical retrieval. This run is a
BM25/RRF versus BM25/RRF-plus-reranker smoke, not a reproduction of the earlier
hybrid-vector comparison. It cannot justify a reranker default change.

Both arms used `--file-role production --no-memory --limit 10`. The treatment
used the local `BAAI/bge-reranker-v2-m3` service with the default 50-candidate
pool and the new path/scope/symbol/kind/role/origin/line header.

## Results

| Query target | No reranker | Contextual reranker | Observed rerank time |
|---|---|---|---:|
| Collaborative document sync | `onReceiveDocUpdate` #4 | `onReceiveDocUpdate` #1 | 5.32 s |
| Copilot document-read authorization | `buildDocContentGetter` #1; MCP provider #7 | MCP provider #1; `buildDocContentGetter` #2 | 3.26 s |
| Storage URL-prefix blob upload | `withURLPrefix` #6; `createBlobUpload` absent | `createBlobUpload` #2; `withURLPrefix` #10 | 3.04 s |
| Transcript retry | `retryTask` #9 | `retryTask` #1 | 2.30 s |

Every arm returned 10/10 allowed-role hits. Excluded roles did not consume the
reranker pool. The results are mixed: two substantial improvements, one
boundary reorder without a clear outcome change, and one tradeoff between two
necessary blob-flow locations.

## Decision

Keep the existing default unchanged. A valid default decision requires a full
`content-v2` re-embed followed by the same paired hybrid queries, ideally with
more than one wording per behavior. The machine-readable retrieval status now
prevents this degraded-vector run from being mistaken for hybrid evidence.
