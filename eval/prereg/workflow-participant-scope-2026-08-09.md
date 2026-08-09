# Pre-registration: workflow participant scope — 2026-08-09

## Status and reason

This is a new semantic-quality treatment after the passing response-budget
replay. It does not revise that result.

The frozen v1 artifacts represent every named production function as an
undifferentiated `participant`. In the failed Recall replay, the durable record
gave private `handleRecallStatusEvent` and downstream
`importCallRecordingArtifacts` the same status as the signed-callback route,
workspace extraction, scoped processor, and lifecycle boundary. The consumer
then substituted those internals for two required boundaries. Document records
show the inverse failure in some trials: important PDF stages were omitted.

The hypothesis is that semantic memory needs an explicit abstraction level,
not more prose.

## Frozen treatment

Change workflow write-back and rendering only:

1. Every workflow participant declares `scope` as `defining` or `supporting`.
   - `defining`: part of the minimal stable cross-file skeleton needed to
     explain the workflow's handoffs and effects.
   - `supporting`: a relevant internal helper, leaf operation, or evidence
     anchor that should not replace a defining participant in an overview.
2. A workflow must have at least one defining participant; every participant
   still requires an exact current anchor, role, evidence span, and confidence.
3. Serialized evidence identifies whether it supports the artifact name, a
   defining participant, a supporting participant, or another claim.
4. The MCP input schema exposes the nested workflow participant requirements
   and scope enum directly. Server instructions define the distinction.
5. New writes use `prompt_version = annotate/v2`. Existing v1 artifacts remain
   readable and are visibly labelled as legacy participant evidence when their
   body has no scope.

The following may not change during the registered run:

- search ranking, memory matching, response-budget priority, or artifact byte
  allocation;
- task prompts, gold sets, source commit, seed database, model, or reasoning;
- freshness/fingerprint rules or structural extraction;
- workflow participant scope vocabulary or definitions;
- `SKILL.md` and runner guidance outside the MCP schema/instructions above.

File/module summaries, symbol cards, autonomous broad scouting, and default
memory retrieval remain out of scope.

## Evaluation

- Repository and six admitted pairs: the same frozen Twenty task set at
  `02a187d065354872c0f318b0723a1e7d8762ae00`.
- Execution: `gpt-5.6-terra`, high reasoning, three fresh trials from the empty
  schema-v6 semantic seed. Generate new session-1 workflows and new warm/cold
  session-2 runs; do not reuse replay warm responses.
- Keep the stale arm because participant supports still participate in
  freshness; its original hard failure rules remain unchanged.
- Blindly adjudicate every exact-set disagreement with `gpt-5.6-sol`/high.
- Independently extract every successful session-1 workflow and compare its
  `defining` anchors with that pair's hidden session-2 gold. This is an artifact
  representation diagnostic, not an answer grade. Report micro and per-pair
  precision/recall plus every false-defining and missing-defining anchor.

The defining-scope diagnostic is intentionally keyed to the later question:
the artifact is useful only if it preserves the abstraction level a later
agent needs without receiving that question's answer directly.

## Registered outcomes

The treatment passes only if all are true:

1. 18/18 session-1 runs write an `annotate/v2` workflow in which every
   participant has a valid scope and at least one participant is `defining`.
   Runs with no `supporting` participant are reported but do not fail
   mechanically.
2. Micro defining-participant precision and recall are each at least 80%, and
   no pair has either metric below 50% across its three trials.
3. Adjudicated warm session-2 correctness is at least cold correctness, with
   zero warm regressions on Recall.
4. Median warm session-2 token reduction is at least 20%, memory is returned in
   18/18 warm runs, and every correct warm token win retrieves memory.
5. Failed `annotate` calls are at most 17, half the v1 count rounded down.
6. The existing stale-safety gate has no failure.

Failure of outcome 2 blocks broad workflow generation: the new field would be
structured guesswork. Failure only of outcome 5 keeps the semantic model but
requires an ergonomic write API before agent write-back expands. Correctness
regression or stale-safety failure blocks SC-2b/default rollout. Efficiency
below 20% keeps memory opt-in even if the artifact representation improves.

This run may establish the value of scoped workflow artifacts. It may not be
claimed as evidence for file/module summaries, concepts, autonomous repository-
wide scouting, or repositories beyond the admitted Twenty workflows.

## Pre-run ergonomic amendment

Before any registered Twenty run, a non-claim fixture smoke exposed 25 failed
`annotate` calls and two successful writes in one session. The agent discovered
the generic request one field at a time, then duplicated JSON-pointer supports
for participant anchor/scope/role leaves. A richer nested schema description
did not fix the v1 repair loop.

The workflow request shape is therefore refined before execution, without
changing the semantic treatment:

- `type`, `name`, `participants`, `confidence`, `snapshot`, and optional
  `supersedes` are top-level;
- each participant carries `anchor`, `role`, `scope`, exact evidence file/span,
  and confidence directly;
- the server constructs the canonical body plus `/name` and participant-role
  support rows; agents do not author JSON pointers for workflows;
- generic `body` plus `supports` remains the request shape for free-form
  `annotation` records only.

The full run may start only after a fresh non-claim fixture session writes one
valid scoped workflow with at most one failed `annotate` call. The smoke is a
mechanical API check and does not contribute to any registered outcome.
