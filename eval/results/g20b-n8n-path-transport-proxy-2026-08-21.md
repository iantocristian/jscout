# G20b n8n path-transport proxy

- Date: 2026-08-21
- Status: real-corpus projection validation; not the registered historical replay
- Corpus: n8n `9d9e9bf97e8ae5382a930cd662637a9cf7046ef9`
- Index: fresh external database, 19,235 files, 92,234 chunks, 404,999 references
- Retrieval: lexical only, six hits, no vectors, reranker, or semantic memory
- Expansion: depth 2, three seeds, 40 nodes, 80 edges, 24 KB graph and response bounds

## Claim boundary

The private repository used by the retained 42-call architecture inquiry and
19-call TargetsQueue investigation is no longer present locally. Those reports
also state that raw responses were not retained, and several TargetsQueue query
strings survive only in abbreviated form. Replaying different calls on a
different corpus cannot satisfy the registered equal-call/equal-fact gate.

This proxy tests the isolated G20b projection change on a large real monorepo.
It compares G20a's induced neighborhood, G20b's explicit compatibility
neighborhood, and G20b's default path projection over one immutable database.
It does not count toward the plan's historical 60% acceptance threshold.

## Queries and results

| Query | G20a neighborhood bytes | G20b neighborhood bytes | G20b paths bytes | Path reduction | Old nodes/edges | Path nodes/edges |
|---|---:|---:|---:|---:|---:|---:|
| `WebhookRequestHandler executeWebhook runWorkflow WorkflowExecute` | 16,597 | 16,659 | 6,926 | 58.3% | 40 / 55 | 12 / 9 |
| `WorkflowExecute run executeNode pushExecutionData` | 18,067 | 18,130 | 6,920 | 61.7% | 40 / 52 | 11 / 8 |
| `executeWorkflow additionalData workflowRunner run` | 18,245 | 18,273 | 6,873 | 62.3% | 40 / 53 | 10 / 9 |
| `queue recovery stalled executions retry execution` | 19,731 | 19,793 | 6,606 | 66.5% | 40 / 80 | 11 / 8 |
| **Aggregate** | **72,640** | **72,855** | **27,325** | **62.4%** | **160 / 240** | **44 / 34** |

G20b neighborhood retained the same node and edge counts as G20a. Its 215-byte
aggregate increase is the explicit projection and omission envelope, not a
retrieval change. Path mode reduced nodes by 72.5% and edges by 85.8%.

## Fact checks

For all four queries:

- the ordered six search-hit anchors were identical in all three arms;
- every emitted path edge was present in G20b's full diagnostic neighborhood;
- seed anchors remained present even when they had no admitted continuation;
- every seed with at least one neighborhood edge received a selected path
  before another seed consumed a second path slot; and
- omitted path/node/edge counts were explicit.

The first run exposed a seed-starvation defect: global ranking spent the path
budget on the stronger webhook seeds and returned no continuation for the
`WorkflowExecute` seed. G20b now reserves the best continuation from each seed
before filling the remaining slots by global rank. A deterministic fixture
locks that behavior. In the queue-recovery query, the third seed still has no
edge because it is isolated in both path and neighborhood projections; this is
an index fact rather than budget starvation.

## Decision

The proxy supports keeping path projection as the compact default and retaining
neighborhood as an explicit diagnostic mode. It validates projection fidelity
and large byte reduction on this corpus. Aggregate transport acceptance remains
open until the two historical source snapshots and complete call inventories
are available or a new fixed-call workload is registered prospectively.

