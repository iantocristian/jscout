# Pre-registration: semantic-memory response-budget replay — 2026-08-09

## Status and reason

This is one explicitly labelled **post-v1 informed revision**, not a rewrite of
the original SC-2a result. V1 is permanently recorded as inconclusive in
`eval/results/twenty-memory-gate-2026-08-09.md`.

The revision is admitted because the treatment was not delivered in 4/18 warm
session-2 runs. In every miss, semantic search found the matching artifact but
`apply_response_budget` removed it before shrinking or dropping code hits. The
recorded response metadata says `omitted_semantic_artifacts: 1`; this is a
deterministic delivery defect, not a post-hoc hypothesis about model behavior.

## Frozen treatment change

Change only whole-response budget priority:

1. Remove/truncate expansion first, as today.
2. If multiple semantic artifacts match, lower-ranked artifacts may be removed.
3. Preserve the top matching semantic artifact while truncating hit snippets,
   hit context, and lower-ranked hits.
4. Remove the final artifact only when it cannot fit in the response envelope by
   itself.

A regression test must prove that one matching artifact survives truncation
when the original envelope is too large but that artifact fits by itself.

The following are controlled and may not change before or during replay:

- semantic-artifact schema, validation, ranking, or rendering;
- `annotate` tool schema or server instructions;
- `SKILL.md` or runner task guidance;
- task prompts, gold sets, source commit, structural seed, model, or reasoning;
- stored session-1 artifact contents.

## Replay design

- Reuse the exact 18 `after-session1.db` snapshots archived by v1, verified
  against each recorded semantic-table fingerprint before use.
- Run only a fresh warm session 2 for every `(pair, trial)` using
  `gpt-5.6-terra` at high reasoning.
- Reuse the corresponding frozen v1 cold session-2 rows. Do not regenerate cold
  runs or session-1 artifacts; doing so would mix delivery priority with model
  and workflow-generation variance.
- Use new session identifiers and separate telemetry/artifacts.
- Blindly adjudicate every new exact-set disagreement with Sol/high under the
  same no-arm-label procedure.
- Staleness is not rerun: v1 delivered degraded memory in 6/6 cases and the
  revision changes only which payload class survives a byte budget. Unit and
  non-claim smoke tests cover the revised truncation path.

## Registered outcomes

Primary comparison remains warm-replay minus the frozen paired cold row:
session-2 total tokens, with tool calls and wall time secondary.

The revision passes only if all are true:

1. The top semantic artifact is actually returned in 18/18 replay runs.
2. Median session-2 token reduction is at least 20%.
3. Adjudicated replay correctness is at least frozen-cold correctness (14/18).
4. Every correct warm token win retrieved an artifact.

Session-1 correctness remains reported but is not re-gated in this replay: the
producer snapshots are frozen inputs, and the original protocol names
session-2 correctness parity as the outcome. This does not retroactively change
the v1 result.

Below 10% median reduction with no new capability result closes semantic-memory
efficiency work. A result from 10% through less than 20%, failure to deliver all
18 artifacts, or any correctness regression remains inconclusive and blocks
SC-2b/default rollout. No second delivery-priority revision is allowed.

## Frozen v1 inputs

- Source commit: `02a187d065354872c0f318b0723a1e7d8762ae00`
- Structural seed SHA-256:
  `d11b78352cdd1841d1332732a83ad27474a57f55c5ef6adaa44b6f7e5fa83994`
- Response SHA-256:
  - 001: `1c2f91cbf4b2c64c9e2a88d218351fbb72904ae6a333ad533c49a6bce25bce72`
  - 002: `f008358b9936ad1df6ed5c920e5146edcd52e7fca39cc23e0b6166de9a7d56f9`
  - 003: `8523ec83199455bba53c69ec1acbcc493ec8c55eaa81bd90d0cc7fc75e6a56cc`
- Telemetry SHA-256:
  - 001: `81537ec05959101743152297ca4a765b43aa43daa629469ac41d31af381a1b3e`
  - 002: `d0a6f43df828639b7f671158458ee2426ade243186312ed7d5c69b4abb18b838`
  - 003: `5251c857116c625e946bec043851a0721dbbb2c89c81b668fafb86eec2df1d19`
