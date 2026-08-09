# Workflow participant-scope preflight — 2026-08-09

## Decision

**Blocked before the registered Twenty run.** The direct `annotate/v2` workflow
shape fixed write ergonomics, but free-form workflow synthesis did not reliably
retain all distinct cross-file stages as participants. The final non-claim
fixture omitted both later-needed operation anchors, so the pre-registered
coverage prerequisite failed and no Twenty treatment run was started.

The passing semantic-memory budget replay remains valid. This result blocks
broad workflow generation from an unconstrained agent-authored participant
list; it does not reverse the value of retrieving a good existing artifact.

## Preflight sequence

All runs used the synthetic checkout fixture, `gpt-5.6-terra`, high reasoning,
and are excluded from product claims.

| Smoke | Workflow request | Failed / total writes | Stored artifacts | Session-1 tokens | Follow-up participant recall |
|---|---|---:|---:|---:|---:|
| 1 | Generic body + JSON-pointer supports | 25 / 27 | 2 | 846,324 | 2/2 |
| 2 | Direct participants with inline evidence | 1 / 2 | 1 | 237,374 | 0/2 |
| 3 | Direct shape + explicit complete-stage guidance | 1 / 2 | 1 | 161,393 | 0/2 |

Smoke 1 eventually produced a comprehensive second artifact, but only after a
field-by-field error-discovery loop and a duplicate partial write. That is not a
usable agent interface.

The amended direct shape met the ergonomic prerequisite: one failed probe and
one successful write, with five stored supports instead of 25. Both direct
smokes nevertheless compressed inventory reservation and payment authorization
into the checkout orchestrator's prose role. Neither stored
`reserveInventory` nor `authorizePayment` as a participant, even after the MCP
contract explicitly said not to omit anchored stages. In smoke 2 the producer
had inspected both target files and still omitted them. In smoke 3 it did not
inspect them directly at all.

Warm session 2 answered correctly by reopening source. That demonstrates the
importance of separating artifact coverage from final-answer correctness: the
agent recovered despite incomplete memory.

## What survives

- The direct workflow request is retained. Agents send top-level participants
  with inline evidence; jscout constructs the canonical body and support rows.
- Every new participant is `defining` or `supporting`, duplicate anchors are
  rejected, and at least one defining participant is required.
- Returned supports explicitly identify artifact-name, defining-participant,
  supporting-participant, generic-claim, or legacy-participant evidence.
- Existing `annotate/v1` artifacts remain readable. New direct writes are
  `annotate/v2`.
- The archive auditor measures all-participant follow-up coverage and reports
  defining/supporting overlap without treating a partial follow-up gold set as
  precision ground truth.

These are storage and interface improvements. They do not establish that an
LLM will discover a complete workflow participant set from a prose request.

## Architectural consequence

The next scout must be **candidate-closed**:

1. Start from explicit entry/seed anchors and deterministically expand certain
   and likely production call/event edges under a bounded graph budget.
2. Issue a snapshot-bound candidate-set fingerprint containing exact anchors
   and evidence locations.
3. Require the LLM to classify every issued candidate as `defining`,
   `supporting`, or `excluded` with a reason. Silent omission is invalid.
4. Let the server construct evidence supports from indexed anchors/spans and
   reject a write whose candidate set or snapshot changed.
5. Evaluate candidate recall separately from LLM classification. If graph
   expansion misses a stage, that is an L1/seed problem; if the model excludes
   it incorrectly, that is a semantic-classification problem.

This is the concrete role for scouting after L1: deterministic structure owns
candidate enumeration; the LLM owns semantic compression and scope labels.
Prompting an LLM to both discover and classify the set leaves omissions
unobservable until a later task fails.

## Result handling

The pre-registration and its pre-run amendments are preserved in
`eval/prereg/workflow-participant-scope-2026-08-09.md`. Raw fixture responses,
telemetry, databases, and model events remain outside the repository. No
registered Twenty rows exist for this treatment.
