# Pre-registration — file-role classification and retrieval filtering

Registered: 2026-08-09, **before implementation begins**. The n8n+Twenty
post-cutoff report requires re-runs to carry "a pre-registered expected
effect"; this document is that registration for the file-roles change. It is
immutable once implementation starts — amendments go in a dated addendum
section, never edits.

## Change under test

Indexed file roles (`production | test | fixture | generated | documentation |
unknown`) with search/expansion filters and expansion penalties for
non-production nodes, applied before the global node/byte budget
(consequences 2–3 of
[n8n-twenty-post-cutoff-2026-08-09.md](../results/n8n-twenty-post-cutoff-2026-08-09.md)).

## Motivating measurement

Structural retrieval's only statistically stable effect in the 72-run suite
was harm: **+6.38 irrelevant inspected files vs grep, 95% CI [+1.00, +12.38]**,
attributed by telemetry to tests, fixtures, generated files, and adjacent
framework code surfaced through expanded results.

## Protocol (identical to the recorded suite)

Same eight tasks, same frozen commits (`9d9e9bf9` n8n / `02a187d0` Twenty),
same three profiles, three trials, `gpt-5.6-terra` high reasoning, Codex CLI
pinned, same grading and blind adjudication, task-clustered bootstrap. The
**only** difference between compared builds is the file-roles change (record
both jscout snapshots). The [SKILL.md](https://github.com/iantocristian/jscout/blob/dc26ffd/integrations/jscout/SKILL.md)
agent guide (the single pre-G28 guide, linked at its last commit) stays byte-identical. No other retrieval change rides along.

## Pre-registered expectations

**Primary (the change succeeds if):** the structural-vs-grep paired
irrelevant-inspected-files delta shrinks to a point estimate **≤ +2.0** with a
task-clustered 95% CI that **includes zero**.

**Secondary (the change is invalidated if any of these regress):**
1. Adjudicated correctness stays ≥ 23/24 in each indexed arm.
2. Structural-vs-grep token delta does not worsen beyond its recorded point
   (+12,983); CI expected to still cross zero.
3. The share of expanded-search result nodes with role ∈ {test, fixture,
   generated} falls to less than half its pre-change value. *Implementation
   note: this requires role tags on expansion payloads and telemetry rows —
   build the measurement into the feature.*

**Explicitly not expected:** a correctness gain (the suite is at ceiling), or
a stable token/latency win vs grep. Claiming either from this run would be
post-hoc.

## Failure consequences (registered now to prevent re-litigating later)

- If the primary effect is not achieved: file roles are insufficient; the
  standing hypothesis becomes seed relevance ranking (per the discriminating
  run: "the graph expands the wrong seeds honestly; ranking cannot repair a
  poor seed set").
- **One** post-hoc-informed revision of the roles/penalty design may be
  re-registered and re-run. A second failure closes L1 retrieval investment;
  effort moves entirely to the SC-2a workflow/memory gate
  ([two-session protocol](../protocols/two-session-memory.md)).
