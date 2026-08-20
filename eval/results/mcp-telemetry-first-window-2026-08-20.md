# Production MCP telemetry — first measured window

- Date: 2026-08-20
- Source: `output/cli-bmg/telemetry.jsonl` — 145 per-call rows, 20 pid-based
  sessions, 2026-08-11 → 2026-08-20, production monorepo
- Status: measured (not agent-reported); no call arguments in rows; mixed
  builds and workloads across the window — see limits

## Headline findings

**1. `who_uses` has no byte guard, and its worst case dominated the window.**
Three byte-identical calls of 1,864,097 bytes each (one session, 8 seconds
apart — almost certainly a repeated call on a very-high-fanout symbol) are
78.7% of every byte in the file. `source_budget_truncations` is zero on all
145 rows: no cap ever fired. The tool's byte spread is otherwise tiny
(median 1.1KB). Whatever the eventual fold-vs-keep decision, `who_uses`
needs a response cap immediately.

**2. The vector path costs ~20× in latency; expansion costs bytes, not
time.** Holding expansion fixed, `semantic_search` median elapsed is ~6.2s
with vector active vs ~0.3s disabled; expansion adds ~2.1× bytes
(median 17.5KB vs 8.3KB; ~81% of all search bytes) with essentially zero
added latency. Confounded with build/date windows, but consistent across
expansion states. If it replicates, per-query embedding latency — not
serializer bytes — is the dominant interactive cost of vector retrieval,
and it plausibly explains agents' parallel-batching pressure.

**3. Production ran lexical-only for roughly four days without anyone
noticing.** 35 semantic calls across 8 sessions (08-15 → 08-19) report
`vector: disabled`. Silent degradation of exactly the class the evaluation
round already flagged; retrieval health needs to be surfaced, not logged.

**4. A freshness collapse on 08-20**: 100% of returned artifacts fresh on
08-19 (163/163) vs 10% on 08-20 (6/59, the rest degraded/stale), dominated
by one long session whose snapshot changed twice. Consistent with real
source drift invalidating supports with no re-scout running — the
operational case for stale-delta scouting (G19) arriving on its own.

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
showed zero `who_uses` selection, but the wider telemetry window shows 13
calls across ~6 sessions. The right reading is "used occasionally,
unguarded worst case, near-zero value density at the tail" — the
recommendation shifts from retire-first to cap-first, with fold-vs-keep
decided on delivered-vs-selected data once arguments are recorded.

Other structure: median 5 calls/session; 7 of 20 sessions were ≤2 calls;
two sessions hold 40% of all calls; `repository_overview`, when used, is
the session's first call in 7 of 9 cases. `annotate`: zero, ever. Parallel
batching (timestamp proxy): 23% of calls in multi-call batches, and big
responses (>10KB) almost never batch (5 of 72) — the batching-waste class in
the traces is real but the telemetry window's users mostly self-serialized.

## Limits (verbatim from the analysis)

80% of all bytes are one incident (the `who_uses` triple); 40% of calls are
two sessions; per-tool conclusions outside search rest on ≤3 observations;
no arguments means repeats and query quality are unknowable; pid sessions
span idle gaps up to 103 minutes and are process lifetimes, not units of
work; rows mix at least two builds (retrieval-status fields appear
mid-window) and different retrieval configs, so cross-date latency
comparisons are confounded. The vector-latency finding is the most robust;
the freshness collapse is two days and ~three sessions — investigate, don't
trend.

## Telemetry fixes this window itself demonstrates

The same three fields proposed in the synthesis doc — call arguments,
same-batch marker, outer-truncation indicator — plus session labels
(`JSCOUT_SESSION_ID` unset throughout; pid identities recycle) and a `task`
label (null on all 145 rows). With arguments recorded, the `who_uses`
triple, repeat reads, and delivered-vs-selected all become measurable
instead of inferable.
