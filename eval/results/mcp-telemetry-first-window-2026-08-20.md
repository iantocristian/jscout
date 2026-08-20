# Production MCP telemetry — first measured window

- Date: 2026-08-20
- Source: `output/cli-bmg/telemetry.jsonl` — 145 per-call rows, 20 pid-based
  sessions, 2026-08-11 → 2026-08-20, production monorepo
- Status: measured rows (not agent-reported), with interpretation corrected
  after checking build history and the intentionally mixed retrieval
  deployments; no relevance labels or call arguments in ordinary telemetry

## Headline findings

**1. A historical pre-cap `who_uses` incident dominates the window; the
current surface is already capped.** Three byte-identical calls of 1,864,097
bytes each (one session, 8 seconds apart — likely a repeated call on a
very-high-fanout symbol) are 78.7% of every byte in the file. Those calls were
recorded on 08-11; the compact definition/usage transport with a complete 24 KB
response ceiling landed on 08-15. `source_budget_truncations` measures source
rendering, not the complete `who_uses` response budget, so its zero value cannot
prove that a whole-response cap did or did not fire. The remaining action is a
current-binary high-fanout regression/replay and delivered-value measurement,
not implementation of a cap that already exists.

**2. The combined vector-plus-reranker posture is slower in this window, but
the responsible stage is not identified.** Holding expansion fixed,
`semantic_search` median elapsed is ~6.2s when vector and reranker are both
active versus ~0.3s when both are disabled; expansion adds ~2.1× bytes (median
17.5KB vs 8.3KB; ~81% of search bytes) without a comparable elapsed-time
change. All 32 ordinary vector-active rows also report the reranker active, and
the rows come from different build/date windows. The data therefore cannot
assign the latency to query embedding, sqlite-vec, reranking, model cold start,
or another deployment difference. Stage timings and a one-variable replay are
required before changing either default.

**3. Retrieval posture is not reconstructable for the whole window.** Twenty-two
search rows explicitly report vector and reranker disabled, while 19 older rows
do not contain retrieval-status fields. Several versions were intentionally
deployed with and without embeddings. These rows do not establish unnoticed
degradation. They do establish the need to record binary/build identity,
effective configuration, and requested versus actual retrieval stages; the
current response-level active/degraded/disabled status remains useful to the
agent.

**4. A freshness change occurred on 08-20**: 100% of returned artifacts were
fresh on 08-19 (163/163) versus 10% on 08-20 (6/59, the rest degraded/stale),
dominated by one long session whose snapshot changed twice. This is consistent
with source drift invalidating supports with no re-scout running, but two days
and a few sessions do not establish a trend or the value of automatic
stale-delta scouting. The event is an operational case to investigate under
G19, not acceptance evidence for it.

**5. One failure burst**: six `semantic_memory` rejections in 8 seconds
(47 bytes, 0ms each — synchronous rejection, client retrying), one minute
after a 121.7s search stall in the same session. 6/145 = 4.1% overall
failure rate, but 23% of `semantic_memory` calls.

## Tool mix (145 calls)

| tool | calls | median B | max B | median ms | fails |
|---|---:|---:|---:|---:|---:|
| semantic_search | 75 | 13,037 | 49,774 | 484 | 0 |
| semantic_memory | 26 | 9,766 | 23,838 | 23 | 6 |
| definition | 13 | 1,882 | 5,980 | 1 | 0 |
| who_uses | 13 | 1,104 | 1,864,097 | 808 | 0 |
| repository_overview | 10 | 5,814 | 20,351 | 275 | 0 |
| calls / file_outline / neighborhood | 3 / 3 / 2 | — | — | — | 0 |

Correction to the cross-trace synthesis in this PR: the two retained traces
showed zero `who_uses` selection, but the wider telemetry window shows 13 calls
across ~6 sessions. The right reading is “used occasionally, with one severe
historical pre-cap incident.” The current cap should be replayed against the
same high-fanout shape; fold-vs-keep still requires delivered-vs-selected data.

Other structure: median 5 calls/session; 7 of 20 sessions were ≤2 calls;
two sessions hold 40% of all calls; `repository_overview`, when used, is
the session's first call in 7 of 9 cases. `annotate`: zero, ever. Parallel
batching (timestamp proxy): 23% of calls in multi-call batches, and big
responses (>10KB) almost never batch (5 of 72) — the batching-waste class in
the traces is real but the telemetry window's users mostly self-serialized.

## Limits and corrected claim boundary

80% of all bytes are one historical incident (the `who_uses` triple); 40% of
calls are two sessions; per-tool conclusions outside search rest on small
samples; no arguments means repeats and query quality are unknowable; pid
sessions span idle gaps up to 103 minutes and are process lifetimes, not units
of work; rows mix builds and intentionally different retrieval configs, so
cross-date latency comparisons are confounded. There are no answer-quality or
relevance labels. Use the window to locate incidents and missing
instrumentation, not to attribute causal value or cost to a retrieval stage.

## Telemetry fixes this window itself demonstrates

Add binary/build identity, a non-secret effective-configuration fingerprint,
requested vector/reranker/memory/expansion posture, and stage-specific timing,
plus session labels (`JSCOUT_SESSION_ID` was unset throughout) and a task label
(`null` on all 145 rows). Exact call arguments already belong in the separate,
privacy-sensitive `--request-log`; they should not silently enter ordinary
telemetry. Same-client-batch membership and outer/client truncation are not
observable by the MCP server unless the client supplies identifiers, so eval
harnesses must record them rather than treating server telemetry as complete.
