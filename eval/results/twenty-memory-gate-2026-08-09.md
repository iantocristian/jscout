# Twenty two-session workflow-memory gate — 2026-08-09

## Decision

**Inconclusive. Do not advance semantic memory to default product surface or
start SC-2b summaries yet.** The registered pass threshold was a median
session-2 token reduction of at least 20% at correctness parity with memory
actually retrieved in winning runs. The observed median was **19.29%**, but the
pair-clustered interval spans material harm and benefit, session-1 correctness
was only 14/18, and 2 of 7 correct token wins did not retrieve an artifact.

The registered stop rule also does not fire: the median is above 10%, and the
warm arm produced four capability wins. The result is therefore neither a pass
nor evidence to delete the experiment. It identifies two implementation faults
that should be isolated before one informed revision: response budgeting can
silently remove the treatment, and workflow records do not distinguish defining
participants from evidence-only helpers reliably enough.

## Frozen design

- Repository: Twenty at `02a187d065354872c0f318b0723a1e7d8762ae00`.
- Indexed source: 22,735 files; schema-v6 structural seed SHA-256
  `d11b78352cdd1841d1332732a83ad27474a57f55c5ef6adaa44b6f7e5fa83994`.
- Task set: 6 related session pairs, all mechanically certified `weak` and all
  passing same-model no-tools transfer-triviality probes.
- Execution: `gpt-5.6-terra`, high reasoning, 3 trials, committed harness
  `cbfc89e`; skill/tool guidance stayed unchanged throughout the run.
- Scale: 18 session-1 write-back runs, 36 normal session-2 runs, and 6 stale
  session-2 runs: 60 Terra runs total.
- Grading: exact set first, followed by blind `gpt-5.6-sol`/high source review
  of all 12 disagreements. The judge saw no session or arm labels. All 12 were
  substantively incorrect; no verdict changed the exact grade.
- Completeness: 18/18 paired comparisons, zero missing rows, zero runner errors,
  and a single execution-model stamp.

## Registered outcomes

| Outcome | Warm | Cold | Warm minus cold / result |
|---|---:|---:|---:|
| Adjudicated session-2 correctness | 14/18 | 14/18 | 0 pp |
| Capability wins / regressions | — | — | 4 / 4 |
| Median token reduction | — | — | **19.29%** |
| Mean token-reduction fraction | — | — | **−0.73%** |
| Pair-clustered 95% interval, reduction fraction | — | — | **[−35.38%, +27.84%]** |
| Mean token delta | — | — | **−24,919.5** |
| Median token delta | — | — | **−29,176.5** |
| Pair-clustered 95% interval, token delta | — | — | **[−75,191.6, +25,273.8]** |
| Mean wall-time delta | — | — | **−8.64 s** |
| Median wall-time delta | — | — | **−11.20 s** |
| Pair-clustered 95% interval, wall time | — | — | **[−25.97 s, +9.51 s]** |
| Mean tool-call delta | — | — | **−1.0** |
| Median tool-call delta | — | — | **−1.5** |
| Pair-clustered 95% interval, tool calls | — | — | **[−2.44, +0.44]** |

Positive reduction means warm used fewer tokens. The median is near the gate,
but the mean fraction is slightly negative because the result is heterogeneous
and includes large warm regressions. None of the cost intervals excludes zero.

The combined two-session means were 552,372 warm tokens and 577,291 cold-side
comparison tokens; medians were 498,840 and 546,745. Both totals add the same
observed session-1 investigation, so this comparison does not independently
estimate write-back overhead.

## Memory delivery and write-back

- Every session-1 run eventually wrote one workflow: 18/18.
- Warm session 2 actually received an artifact in 14/18 runs: 77.78%.
- Seven paired runs were correct in both arms and cheaper warm; two of those
  seven retrieved no artifact, leaving five artifact-backed token wins.
- `annotate` failed 35 times before the 18 successful writes. Fifteen failures
  omitted the required `/name` support, nine used a body without the required
  root `participants` array, and eleven were claim-support, participant-role,
  or evidence-span errors.

The four memory-delivery misses are not search-relevance misses. The matching
artifact existed and scored, but `apply_response_budget` removed expansion and
then semantic artifacts *before* shrinking or dropping code hits. Recorded
responses explicitly report `omitted_semantic_artifacts: 1`. A warm arm that
silently drops its only memory record is not receiving the intended treatment.

## Workflow-level heterogeneity

Median token reduction by pair:

| Workflow | Median reduction | Warm correctness | Cold correctness |
|---|---:|---:|---:|
| Self-hosting identity linkage | +49.70% | 3/3 | 0/3 |
| PDL post-install seeding | +28.06% | 3/3 | 3/3 |
| Fireflies synchronization | +19.94% | 3/3 | 3/3 |
| Slack assistant queue | +18.65% | 3/3 | 3/3 |
| Recall callback lifecycle | −25.30% | 1/3 | 3/3 |
| Document generation | −52.99% | 1/3 | 2/3 |

Aggregate correctness parity hides equal and opposite changes. The warm arm
fixed self-hosting localization in all three trials, but regressed Recall twice
and document generation twice; document generation also supplied one warm win.

The stored records explain the split:

- The self-hosting workflow body named exactly the three defining boundaries
  later requested. Warm answered 3/3; cold repeatedly substituted an object
  constant for the email-filter boundary.
- A Recall workflow body omitted `processRecallWebhookHandler` as a participant
  and promoted downstream `importCallRecordingArtifacts`. Its evidence supports
  also exposed private/helper anchors such as `handleRecallStatusEvent` and
  `getRecallWebhookBotMetadata`; those same helpers appeared as unjustified
  answer boundaries. This correlation is a likely mechanism, not proof of
  model causation.
- One document workflow omitted both `generateDocumentPdf` and
  `attachGeneratedPdf` from participants. Warm answers twice substituted
  `flattenRecord` for the missing attachment boundary.

The current artifact format therefore mixes three roles that agents need kept
separate: defining workflow participants, supporting implementation helpers,
and evidence anchors. Freshness/provenance is sound, but semantic scope is not.

## Staleness result

The stale-safety gate passed all six cases:

- 6/6 returned the session-1 artifact.
- 6/6 labelled it `degraded`; none was silently served as fresh.
- 6/6 directly reopened the edited evidence file.
- 6/6 passed no-tools Sol adjudication for not repeating stale behavior; two
  answers explicitly stated the new behavior.

This supports the fingerprint/freshness design. It does not rescue the
efficiency gate.

## Architectural consequence

Keep SC-2a behind the experimental/opt-in boundary. Do not add file/module
summaries or default semantic-memory expansion from this result.

The next revision should change one treatment at a time:

1. Reserve the top matching semantic artifact before lower-ranked code hits in
   the whole-response budget, with a regression test proving memory survives
   truncation when it fits by itself.
2. Replay the 18 warm session-2 runs against the **same archived session-1
   database snapshots** and existing cold rows. This isolates delivery priority
   from workflow-generation variance; report it as a post-v1 revision, never as
   the original preregistered result.
3. Only after that replay, address workflow quality separately: make defining
   participants distinct from evidence-only anchors and make the workflow write
   schema explicit enough to eliminate the repeated `/name`/`participants`
   repair loop. That is a second treatment and must not be mixed into the
   budget replay.

## Result records

- Task definitions: `eval/tasks/twenty-memory-pairs.json`
- Transfer certificates:
  `eval/results/twenty-memory-transfer-certificates-2026-08-09.jsonl`
- Stale adjudications:
  `eval/results/twenty-memory-stale-adjudications-2026-08-09.jsonl`
- Correctness adjudications:
  `eval/results/twenty-memory-correctness-adjudications-2026-08-09.jsonl`

Raw response/telemetry SHA-256 values, in trial order:

| Trial | Responses | Telemetry |
|---|---|---|
| 001 | `1c2f91cbf4b2c64c9e2a88d218351fbb72904ae6a333ad533c49a6bce25bce72` | `81537ec05959101743152297ca4a765b43aa43daa629469ac41d31af381a1b3e` |
| 002 | `f008358b9936ad1df6ed5c920e5146edcd52e7fca39cc23e0b6166de9a7d56f9` | `d0a6f43df828639b7f671158458ee2426ade243186312ed7d5c69b4abb18b838` |
| 003 | `8523ec83199455bba53c69ec1acbcc493ec8c55eaa81bd90d0cc7fc75e6a56cc` | `5251c857116c625e946bec043851a0721dbbb2c89c81b668fafb86eec2df1d19` |
