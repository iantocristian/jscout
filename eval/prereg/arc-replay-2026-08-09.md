# Pre-registration — arc-replay suite (n8n + Twenty)

Registered: 2026-08-09, before any registered arm runs. Smoke runs (labeled
`--trial smoke*`) validate mechanics only and are excluded from all claims.
This document is immutable once registered runs begin; amendments go in a
dated addendum.

## Suite

Task sets: [`tasks/n8n-arc-pilot.json`](../tasks/n8n-arc-pilot.json) (3 arcs)
and [`tasks/twenty-arc-pilot.json`](../tasks/twenty-arc-pilot.json) (2 arcs);
snapshots fingerprinted under `~/git/jscout-replay-runs/{n8n,twenty}/`.
Stories certified not-`anchored`; arcs verifier-confirmed, closed,
cutoff-bounded (design and rationale:
[`value-hypotheses-2026-08-09.md`](../value-hypotheses-2026-08-09.md)).

Runner: `scripts/eval-run-replay.mjs`. Arms: `grep` (no jscout MCP) vs
`structural` (indexed workspace + full tool surface). ≥2 trials per
(task, arm), profile order counterbalanced, one execution model for all
registered rows (the pooled report enforces this). Grading:
`eval-pr-grade.mjs` — layer 2/3 only (layer-1 tests deferred: no dependency
install in workspaces).

## Primary pre-registered outcomes

1. **Confirmed-omission rate** (gold files unpatched, blind-adjudicated
   `omission`; `alternative_covered`/`not_required` excluded from the
   denominator): structural < grep. **MIE: 15 points absolute** on the
   pooled rate.
2. **Missed-edge-case rate** on multi-commit arcs (did the agent's single
   attempt cover what the human's follow-up commits added — adjudicated per
   follow-up hunk against `gold/members/*.patch`): structural < grep.
   Reported even though only one 3-commit arc exists in the pilot
   (directional, not powered — stated now so it cannot be upgraded to a
   claim post-hoc).

## Secondary outcomes (reported, not gated)

Extraneous-edit rate; plan_mentioned-vs-patched divergence (gaming
indicator); tokens/wall time (expected: no stable difference — claiming a
cost win from this pilot would be post-hoc); navigation metrics (exploratory
until the inspected-files audit exists).

## Explicitly not expected

A correctness/tests-pass claim (layer 1 disabled); a token or latency win;
any claim from the ai-pipe harness-validation corpus.

## Failure consequences

If structural shows no omission-rate advantage at MIE on this pilot: one
re-registered revision of retrieval/tool guidance may be tried; a second
null closes the completeness hypothesis (H2) for L1 retrieval, leaving the
memory gate (SC-2a) as the only open value hypothesis.

## Amendments

(none)
