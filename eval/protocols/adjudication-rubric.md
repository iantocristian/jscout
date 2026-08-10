# Arc-replay adjudication rubric

Judge: Claude (claude-fable-5), assigned 2026-08-09. Registered **before any
guided-session output existed**, because this judge is not neutral: it
authored the tool under evaluation and the eval program. The rubric and the
asymmetric default below are the controls for that stake. Every verdict row
records `judge: "claude-fable-5"` and cites evidence; verdicts without cited
evidence are invalid.

## Known limitation

When the judge observes sessions live, arm identity is not blind. Recorded
as a protocol limitation on every result that uses live observation. Where
feasible, patches are judged from artifacts with arm labels stripped.

## Verdicts for an unpatched gold file (layer 2)

- **`omission`** — the agent's implementation leaves the user-visible
  problem partially unsolved or regressable because this file's change (or
  an equivalent elsewhere) is absent. Evidence: what the gold change does
  (from `gold/members/*.patch`) and where the agent's patch fails to cover
  it.
- **`alternative_covered`** — the agent achieved the same behavior another
  way. Evidence: the agent-patch hunks that cover it. "Could plausibly be
  fine" is NOT evidence; the covering code must be identified.
- **`not_required`** — the gold change was incidental to the story
  (drive-by cleanup, one-off migration scripts, style), or addressed a
  concern outside the story's scope. Evidence: why the story does not imply
  it.

**Asymmetric default (anti-author-bias control): when uncertain between
`omission` and either other verdict, rule `omission`.** All uncertainty
resolves AGAINST the tool author's interest.

## Missed-edge-case verdicts (multi-commit arcs)

For each follow-up member hunk (from `gold/members/<sha>.patch`), one
verdict: `covered` (agent's single attempt already handles what the human
added later — cite the agent hunk) or `missed` (same uncertainty default:
`missed`). The human's own first attempt scored `missed` on all of its
follow-up content by construction — that is the comparison line.

## Divergence review (agent did something the gold didn't)

`valid_alternative` | `unnecessary` (extraneous but harmless) |
`defect` (introduces a bug or regression — cite the failure scenario).
Extraneous edits are never laundered into coverage credit.

## Output

One JSONL row per verdict:
`{task_id, arm, kind: omission|edge_case|divergence, subject, verdict,
evidence, judge: "claude-fable-5"}` — appended to the run's
`adjudications.jsonl`, consumed by `eval-pr-grade.mjs --adjudications`.
