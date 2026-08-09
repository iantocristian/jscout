# Pre-registration: candidate-closed workflow scouting — 2026-08-09

## Status and reason

The free-form workflow-scope treatment was blocked in non-claim preflight. A
Terra producer twice omitted later-needed cross-file operations from the stored
participant set, including after explicit coverage guidance. Final-answer
correctness hid the omission because the consumer reopened source.

The next design separates two responsibilities:

- deterministic structure enumerates a bounded candidate subgraph;
- the LLM classifies every candidate semantically.

This pre-registration is recorded before the candidate-set tools or any Twenty
candidate report are implemented.

## Frozen candidate contract

`workflow_candidates` accepts explicit current seed anchors plus bounded graph
options and returns:

- the current structural snapshot;
- exact resolved seed anchors;
- at most 31 ranked production symbol candidates;
- each candidate's exact anchor, file, declaration span, display name, and
  graph relevance;
- whether traversal/candidate truncation occurred;
- a deterministic fingerprint over snapshot, options, seeds, and ordered
  candidate anchors.

Initial fixed options for the gate:

- direction `both`;
- depth 2;
- minimum confidence `likely`;
- production files only;
- 31 symbol candidates and the existing ranked neighborhood budgets;
- no new learned weights or edge kinds.

`record_workflow` consumes that fingerprint plus one classification for every
issued candidate:

- `defining`: minimal stable workflow skeleton/handoff;
- `supporting`: relevant internal or leaf stage retained for localization;
- `excluded`: structurally adjacent but not part of the workflow, with a
  concise reason.

The server recomputes the candidate set against the requested snapshot and
rejects missing, duplicated, unknown, or stale classifications. It constructs
all evidence spans from indexed candidates; the model cannot invent or omit
support rows. Stored memory retains included participants, excluded decisions,
and the candidate fingerprint. No classification is `certain`.

The direct `annotate/v2` workflow path remains available for manual agent
write-back but is not the broad-scout producer under test.

## Stage A: structural candidate gate

Use the frozen Twenty repository and the six existing memory pairs. Derive
seeds only from each session-1 gold production file/symbol set; no session-2
anchor may be used as a seed. Resolve those seeds in the frozen index and run
the fixed candidate options without an LLM.

Report all-participant candidate recall against hidden session-2 gold, every
missing boundary, candidate count, and truncation per pair.

Stage A passes only if:

1. Micro candidate recall is at least 90%.
2. Every pair has at least 60% recall.
3. No pair's candidate set is truncated before all reachable ranked candidates
   under the fixed budget are reported.

If Stage A fails, do not run semantic classification. Diagnose seeds/edge
coverage as L1 or deterministic entity work; do not tune the LLM prompt.

## Stage B: semantic classification gate

Only after Stage A passes:

- run `gpt-5.6-terra` at high reasoning over three fresh trials;
- require exhaustive classification of the issued candidate set and one stored
  workflow per session-1 run;
- run fresh warm/cold/stale session-2 arms under the existing protocol;
- blindly adjudicate exact answer disagreements with `gpt-5.6-sol`/high.

Stage B passes only if all are true:

1. 18/18 writes consume an unchanged candidate fingerprint and classify every
   candidate exactly once.
2. Stored included-participant recall against hidden session-2 gold is at least
   80% micro and at least 50% for every pair.
3. Adjudicated warm correctness is at least cold correctness, with zero Recall
   regressions.
4. Median warm session-2 token reduction is at least 20%; memory is returned in
   18/18 warm runs; every correct warm token win reads memory.
5. Failed workflow-record calls are at most 9 (0.5 per producer run).
6. The existing hard stale-safety gate has no failure.

Candidate recall, classification recall, and consumer outcomes are reported
separately. A candidate miss cannot be credited to the model, and a model
exclusion cannot be blamed on graph traversal.

## Non-claims

This gate does not test default memory retrieval, repository-wide autonomous
seed discovery, file/module summaries, concepts, or repositories beyond the
admitted Twenty workflows. Passing Stage A earns semantic classification work;
passing Stage B earns bounded SC-2b expansion only.
