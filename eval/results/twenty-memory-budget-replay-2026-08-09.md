# Twenty semantic-memory budget replay — 2026-08-09

## Decision

**Pass the registered post-v1 revision.** Preserving the top matching semantic
artifact before lower-ranked code hits raised artifact delivery from 14/18 to
18/18 and the median paired session-2 token reduction from 19.29% to **36.40%**.
Adjudicated correctness was **17/18 warm versus 14/18 frozen cold**, and all ten
correct warm token wins retrieved an artifact.

This accepts the response-budget priority change and clears the bounded SC-2a
memory gate. It does not establish default rollout, broad workflow coverage, or
the value of file/module summaries. One Recall run still regressed because the
stored workflow promoted downstream import helpers over the signed-callback
boundaries; artifact semantic scope remains a separate treatment.

## Frozen replay

- Pre-registration: `eval/prereg/memory-budget-replay-2026-08-09.md`.
- Repository: Twenty at `02a187d065354872c0f318b0723a1e7d8762ae00`.
- Structural seed SHA-256:
  `d11b78352cdd1841d1332732a83ad27474a57f55c5ef6adaa44b6f7e5fa83994`.
- Inputs: the exact 18 archived v1 `after-session1.db` snapshots, each verified
  against its recorded semantic-table fingerprint before cloning.
- Comparison: 18 new warm session-2 runs against the corresponding 18 frozen
  v1 cold rows; six pairs over three trials.
- Execution: `gpt-5.6-terra`, high reasoning, one execution-model stamp, zero
  missing rows, zero runner errors.
- Treatment change: response-budget priority only. Artifact schema, ranking,
  rendering, task prompts, gold, `SKILL.md`, guidance, and stored artifact
  contents remained frozen.
- Grading: exact set first, then blind `gpt-5.6-sol`/high source review of the
  two disagreements. One was incorrect; one was an acceptable extra symbol in
  a required file.
- Staleness was not rerun, as registered. The unchanged v1 stale arm remains
  6/6 degraded, re-opened, and successfully reverified.

Session-1 correctness remains reported at 14/18 but is not re-gated: those
producer runs and their database outputs are frozen inputs to this replay. The
report command records `require_session1_correctness: false` explicitly.

## Registered outcomes

| Gate | Required | Observed | Result |
|---|---:|---:|---|
| Top artifact delivered | 18/18 | **18/18** | pass |
| Median session-2 token reduction | at least 20% | **36.40%** | pass |
| Adjudicated warm correctness | at least cold 14/18 | **17/18** | pass |
| Correct warm token wins with an artifact read | all | **10/10** | pass |

Supporting paired metrics:

| Outcome | Warm minus frozen cold |
|---|---:|
| Mean token-reduction fraction | +23.12% |
| Pair-clustered 95% interval, mean reduction fraction | [−5.49%, +48.29%] |
| Mean token delta | −56,568 |
| Median token delta | −56,746 |
| Pair-clustered 95% interval, mean token delta | [−114,293, −370] |
| Mean wall-time delta | −18.95 s |
| Median wall-time delta | −21.88 s |
| Pair-clustered 95% interval, mean wall time | [−33.89 s, +0.73 s] |
| Mean tool-call delta | −3.06 |
| Median tool-call delta | −3 |
| Pair-clustered 95% interval, mean tool calls | [−5.33, −0.67] |

Positive reduction means warm used fewer tokens. The registered decision uses
the median threshold, not the bootstrap interval. The interval on the mean
reduction fraction still includes zero, so six workflow pairs are insufficient
for a general effect-size claim.

## Workflow-level result

| Workflow | Median token reduction | Warm correctness | Cold correctness |
|---|---:|---:|---:|
| Fireflies synchronization | +55.28% | 3/3 | 3/3 |
| Self-hosting identity linkage | +51.31% | 3/3 | 0/3 |
| PDL post-install seeding | +49.46% | 3/3 | 3/3 |
| Slack assistant queue | +29.23% | 3/3 | 3/3 |
| Document generation | −1.45% | 3/3 | 2/3 |
| Recall callback lifecycle | −10.93% | 2/3 | 3/3 |

Delivery was the direct v1 defect: all 18 sessions now received memory, with 19
total artifact returns and no stale/degraded return. The outcome is still
heterogeneous. Recall remained slower and supplied the only correctness
regression. Blind adjudication found that the failed answer omitted signature
verification and workspace extraction while substituting two downstream
artifact-import boundaries. This matches the already-recorded defect in the
frozen workflow artifact; the budget fix exposed it consistently rather than
repairing it.

The replay does not estimate artifact-authoring return on investment. Adding
the same observed session-1 run to both sides produces mean two-session totals
of 520,723 warm and 577,291 comparison tokens, but that construction cancels
the authoring cost rather than comparing write-back against never writing it.

## Architectural consequence

Keep the budget priority now implemented: matching semantic memory must not be
silently discarded while lower-ranked code hits consume the response envelope.

SC-2a is accepted as an opt-in, evidence-backed memory capability. The next
semantic treatment is workflow quality, not summaries:

1. Separate defining participants from evidence-only helpers in the workflow
   body and renderer.
2. Make the `annotate` schema expose participant/support requirements directly
   enough to remove the repeated repair loop (35 failed writes in v1).
3. Evaluate broader workflow generation on fresh pairs before enabling default
   retrieval or SC-2c symbol/file/module summaries.

No second delivery-priority revision is permitted by the pre-registration.

## Result records

- Replay correctness adjudications:
  `eval/results/twenty-memory-budget-replay-correctness-adjudications-2026-08-09.jsonl`
- Reused v1 cold/correctness/staleness records:
  `eval/results/twenty-memory-gate-2026-08-09.md`
- Replay response SHA-256:
  `efee779a08f7efad18a6b2f4b33e201f2a9f04e413f6b65a1cd5ba8ee234e452`
- Replay telemetry SHA-256:
  `ec5a48c13e4afaa58494f8247c55306e83a8583aaae9f9fed210640e4a2154e1`
- Replay adjudication SHA-256:
  `839a535e31fd3a16150e0d60422625b9d37f0ce786ba63142d4bf230a29e56b1`

Raw responses, telemetry, database snapshots, and model artifacts remain
outside the repository because they can contain source.
