# Production MCP windows — 2026-08-22 links iteration and 2026-08-24 three-arm comparison

- Date: 2026-09-02 (write-up); sessions recorded 2026-08-22 and 2026-08-24
- Source: measured per-call telemetry rows and the MCP request log from the
  production monorepo (`telemetry-24-08.jsonl`, 24 rows; `telemetry-24-08-2.jsonl`,
  98 rows; `requests.jsonl`, 129 rows), plus four agent transcripts used only for
  the agents' own call inventories and the ranking of answers
- Status: production data. The raw rows and transcripts are deliberately not
  committed; this report carries the aggregates and the SHA-256 of each source
  file so the figures can be re-derived by whoever holds the files. Byte and
  latency figures are measured; call rationales, answer rankings, and the
  "why" behind each call are agent-reported.
- Why it exists: G28 decision 5 required these windows to be written up before
  they were lost. They are the only production data with a full argument log,
  and the 2026-08-24 run is the only same-question comparison of jscout
  postures on record. The G28 replay that would have re-run these arms against
  the new surface is retired; these numbers are the recorded baseline for any
  later production window.

## Headline findings

**1. On the same question, the skill-guided session beat both the tool-breadth
session and the no-jscout control, at 3% of the breadth session's bytes.**
Three arms answered one data-flow question ("where is this table accessed
directly, and how does it propagate to downstream derivatives") on the same
checkout, through the Codex CLI client (`codex-mcp-client/0.149.0-alpha.4.1`;
execution model not recorded):

| Arm | Instruction | Calls | Bytes | Tool time | Ranking by the control arm's grader |
|---|---|---|---|---|---|
| A — jscout only, every endpoint | "use all the available endpoints ... including semantic memory" | 74 | 704,511 | 54.6 s (5 m 39 s telemetry span; 6 m 22 s wall) | 3rd: most exhaustive-looking, but a concrete control-flow error, citations that point at function starts rather than the claimed operation, and one terminology error |
| B — jscout with the installed skill | none beyond the question | 2 | 21,798 | 7.5 s | 1st: best balance of correctness and completeness; found the indirect wrapper callers the control missed |
| C — no jscout (repository search only) | "without using the jscout skill or MCP" | — | — | — | 2nd: correct architecture, missed several wrapper callers; uniquely noticed one unused load |

Arm B's two calls were one `semantic_memory` discovery (7.5 s, 4,673 bytes,
five fresh artifacts, "nothing sufficiently specific") and one exhaustive
`semantic_search` (4 ms, 17,125 bytes, 74 of 74 matching chunks on a single
page at `limit: 200`), after which the agent verified the flow with local text
search and source reads. Arm A's breadth was instructed, not natural: it is a
capability probe of every endpoint, and its ranking says that breadth did not
buy correctness.

**2. One mis-specified exhaustive query cost more than half of the breadth
arm.** A dotted identifier tokenized into an OR query; the agent recognized it
("tokenization made this overly broad") and nevertheless paged all 1,496 chunks
"to preserve exhaustive-cursor semantics". The seven pages at or above 40 KB
total **362,531 bytes — 51.5% of the arm's 704,511** (page sizes 48,061 to
56,779). This is the sole origin of the G23 abandonment contract and the
`broad_or_query` warning, and it is the first tip in the G28 core skill.

**3. Latency lives in the ranked posture, not in lexical or exact reads.** In
arm A, the five vector-plus-reranker searches took a median 7.7 s (6.5–12.5 s)
for 72,791 bytes, while the thirteen lexical and exhaustive searches took a
median 18 ms (maximum 24 ms) for 424,113 bytes; `definition` answered in 1–19 ms
at a median 2,918 bytes. The cheapest tools carried the decisive evidence.

**4. Most MCP sessions in the log never called a tool.** Of the 17 sessions in
the request log, **15 consist only of `initialize`, `notifications/initialized`,
and `tools/list`** — the client fetched the full tool schema and made no call.
Under the pre-G28 surface that was ~27 KB of schema and instructions paid per
session for nothing, which is the cost the G28 core surface (7,781 bytes) and
the per-project `[mcp].tools` allowlist address.

## Tool mix, arm A (74 calls, measured)

| Tool | Calls | Bytes | Share | Median bytes | Max bytes | Median ms | Max ms |
|---|---|---|---|---|---|---|---|
| `semantic_search` | 18 | 496,904 | 70.5% | 19,441 | 56,779 | 19 | 12,470 |
| `definition` | 24 | 78,234 | 11.1% | 2,918 | 8,813 | 1 | 19 |
| `calls` | 10 | 34,675 | 4.9% | 2,291 | 10,308 | 276 | 2,165 |
| `entities` | 1 | 23,945 | 3.4% | — | — | 36 | 36 |
| `neighborhood` | 1 | 23,949 | 3.4% | — | — | 26 | 26 |
| `semantic_memory` | 4 | 13,453 | 1.9% | 2,977 | 5,621 | 14 | 6,899 |
| `annotate` | 2 | 9,994 | 1.4% | 4,997 | 9,647 | 322 | 644 |
| `events` | 2 | 8,711 | 1.2% | 4,355 | 6,182 | 2 | 2 |
| `file_outline` | 6 | 7,587 | 1.1% | 760 | 3,431 | 1 | 3 |
| `who_uses` | 5 | 6,212 | 0.9% | 928 | 2,863 | 24 | 26 |
| `paths` | 1 | 847 | 0.1% | — | — | 10 | 10 |

Notes from the agent's own inventory, checked against the rows: `entities` and
`neighborhood` both rendered at the 24 KB ceiling; `paths` returned no complete
path because a plain assignment is not a graph edge, and the agent fell back to
source; the first `annotate` failed on a missing top-level `confidence` (the one
failed call in the arm) and the second succeeded; one search used `expand: true`
(22,955 bytes, five expansion nodes); the agent used `who_uses`, `calls`, and
`file_outline` after localization, exactly where the skill now places them.

## The 2026-08-22 links-iteration session (22 calls, measured)

A repository-convention check ("is this dictionary routinely iterated without an
own-property guard?") on the pre-G22 binary, structural profile, one snapshot,
57 minutes of session time, 43.2 s of tool time, 169,652 bytes, no failures.

| Tool | Calls | Bytes | Median bytes | Max bytes | Median ms | Max ms |
|---|---|---|---|---|---|---|
| `semantic_search` | 14 | 133,349 | 9,704 | 12,411 | 2,110 | 17,041 |
| `definition` | 7 | 25,467 | 3,927 | 6,940 | 1 | 1 |
| `repository_overview` | 1 | 10,836 | — | — | 313 | 313 |

Twelve of the fourteen searches ran lexical-only at `limit: 10` with
`origins: ["workspace"]` and `file_roles: ["production"]`; two ran with vector
and reranker active. Attached memory returned 27 artifacts across the session
(11 fresh, 7 degraded, 9 stale). The agent's conclusion about the convention
was correct, but its own comparison recorded that the ranked `limit: 10` posture
"omitted some lower-ranked loops" a literal search listed, and it never widened
the limit — reading `limit` as a session ceiling. This session is the recorded
motivation for G22 exhaustive search and the "limit is a page size, not a
ceiling" tip. The one `repository_overview` call cost 10,836 bytes on a task
that already named its package and identifiers.

## Waste and failure modes, ranked by bytes

1. Paging a known-bad exhaustive query to completion: 362,531 bytes (arm A).
2. Ranked vector-plus-reranker searches after localization: 72,791 bytes and
   40 s of the arm's 54.6 s tool time, in a session whose decisive evidence came
   from lexical pages and definitions.
3. Two full-ceiling graph/entity responses (`entities`, `neighborhood`): 47,894
   bytes, neither cited as decisive in the answer.
4. Orientation on an anchored task: `repository_overview` at 10,836 bytes
   (08-22).
5. A schema-invalid `annotate` attempt: one failed call, retried.
6. Reading a ranked limit as a completeness boundary (08-22): not a byte cost,
   but a missed-occurrence correctness cost.

## Limits and claim boundary

- One production monorepo; one question per window; a single execution model
  per run, not recorded in the transcripts.
- The 2026-08-24 ranking was produced by the control arm's own model comparing
  the three answers; there is no independent rubric or relevance labels.
- Arm A's breadth was instructed. Its tool distribution is a capability probe,
  not a natural usage distribution; only the byte and latency figures per tool
  transfer.
- Arm C has no telemetry by construction; its cost is wall time only (2 m 30 s
  per transcript).
- The 08-22 session ran a binary without the client-identity telemetry fields;
  the 08-24 sessions ran with them. Both predate G24 documentation retrieval,
  G26 Rust, G27 plane identities, and G28; `documentation_search` therefore
  does not appear, and that absence is not evidence about it.
- Agent-reported call counts differ slightly from measured rows (the 08-22
  agent reported 20 calls; telemetry holds 22). Measured rows win.

## Use for G28

These windows are the recorded baseline the G28 replay would have been measured
against; with the replay retired, they remain the comparison point for any
future production window on the G28 surface:

- skill-guided arm: 2 calls, 21,798 bytes, 7.5 s tool time, ranked best;
- use-every-endpoint arm: 74 calls, 704,511 bytes, 54.6 s tool time, ranked
  worst;
- per-session schema cost before G28: ~27 KB, paid by 15 of 17 sessions that
  made no call.

Each anti-pattern above maps to one line of the G28 core skill: abandon a
`broad_or_query` page immediately, do not re-search or expand after
localization, `limit` is a page size, copy anchors verbatim, and do not call
`repository_overview` when the task already names the code.

## Source file hashes (SHA-256)

- `telemetry-24-08.jsonl` — `696bbdfa395973c751466fa136884c62e9cab7da2ce9f8b71da083756c92f845`
- `telemetry-24-08-2.jsonl` — `fd9743eb81b0328779e4fd52680a11b3fa0c60bc8ee2fd49132ee1708387f02a`
- `requests.jsonl` — `9a5960f8c835f53795524ed0f8af64cd4560a3b5dd608d0d1a76960ba577ae7d`
- `search-links.txt` — `d374b1873465f5a95250922a0e9c29b1d2a0dee2433dbea23baad019bb367fad`
- `carddHistory1.txt` — `df14351e52217b4325d7e74dcaf1e14745570725b3959a612b5a40f8fa3b9ecb`
- `cardHistory2.txt` — `c69629d75a24f4a0000d883369823d7a6647fa23ddb4468b90afb0710453834c`
- `cardHistory3.txt` — `0126c98b0d961787af1f4c47bde1fc2406a2ba23279f3a58d9452b195ad2f03a`
