# Guided interactive sessions — currency + Insights arcs, 2026-08-09

## Protocol

Human-driven interactive Codex sessions (executor: gpt-5.6-sol, high), one
workspace copy per arm from the frozen arc snapshots. Structural arms: jscout
index + MCP mount + **installed SKILL.md (Option B) — both structural
sessions** (correction on the record: currency was initially reported as
Option A). Grading: `eval-pr-grade.mjs` (tree diff vs pristine) +
adjudication per [adjudication-rubric.md](../protocols/adjudication-rubric.md),
judge `claude-fable-5`. **Limitations: n=1 per cell; judge non-blind (observed
sessions); human guidance uncontrolled; exploratory — no registered claims.**
In-session nudge status: **confirmed — the human instructed both structural
arms to use jscout over rg.** Tool usage is therefore instructed usage, not
guidance-channel adoption.

**Protocol note — live-retrieval contamination channel:** interactive
sessions expose a channel the harness does not — the agent could browse the
upstream public repository or web-search the project, where the gold PR
itself is published. Harness runs disable web tools; interactive arms need
an explicit restriction. **The human included this restriction from the
start of every interactive session** (no contamination window existed; all
recorded grades are clean). Wording used (verbatim, for reproducibility):
"You are not allowed to browse the remote repository
https://github.com/n8n-io or search for n8n info on the web. You are
supposed to fix this bug independently. You can search for related
technical info if you need to, but not n8n directly. use jscout rather
than rg." (Repo name adjusted per corpus; the jscout-over-rg clause is
meaningful only in structural arms.)

## Results

| Task / arm | Gold coverage | Confirmed omission rate | Edge-case (follow-up) rulings | Wall time | jscout calls |
|---|---|---|---|---|---|
| currency / grep | 2 matched + 2 alt + 1 n/r (of 5) | **0** | n/a (single-member arc) | 7m46s | 0 |
| currency / structural | 2 matched + 2 alt + 1 n/r | **0** | n/a | 8m58s | 43 |
| insights / grep | 6/7 matched + 1 alt | **0** | 1 missed, 1 covered (test-only) | unrecorded | 0 |
| insights / structural | **7/7 matched** | **0** | 1 missed, 1 covered (test-only) | unrecorded | 34 |

Adjudication and grade records: [`guided-sessions/`](guided-sessions/) (copied from the run directories); every verdict cites gold and agent hunks.

## Findings

1. **Outcome ceiling persists on implementation tasks.** All four arms: zero
   confirmed omissions. Sol-guided solves these arcs regardless of tooling.
2. **The edge-case layer measured something real on its first activation.**
   Both Insights arms independently missed the same thing: the injectable
   `now` parameter on the date-range CTE helper — the determinism hardening
   the maintainers added after midnight-flaky tests (arc member `4ef9944`,
   retained in final gold). A single session — either arm — reproduces the
   human's *final behavior* but only the human's *first attempt's* hardening.
   This is the arc concept's designed signal, observed.
3. **Micro-quality differences, no winner.** Structural found gold's exact
   serializer-level `timeZone` placement (auto-covers future API consumers);
   grep wired call-sites instead (works today, weaker maintenance posture)
   but additionally updated public-API types + OpenAPI spec where structural
   left minor spec drift. Both extended the public API beyond gold's scope.
4. **Adoption claim withdrawn (attribution corrected).** The human
   explicitly instructed both structural arms to prefer jscout over rg, so
   the 43/34 calls are instructed usage — consistent with the assisted
   harness runs, and NOT evidence that the SKILL.md channel drives adoption
   on its own. The skill-only adoption test remains unrun: it requires a
   session with the skill installed and no verbal instruction.
5. **Same-model convergence.** The two currency arms invented the identical
   new util filename; both Insights arms chose the same public-API extension.
   Same-model arms are not independent solution samples.
6. Incidental: telemetry shows 2 `workflow_candidates` calls — the SC-2a
   tool surface is discoverable and was touched unprompted.

## Pipeline defects found and logged

- `__stories__` paths classify as code; should be test-like.
- Grader matches adjudications by full commit SHA; label ids silently
  filtered (cost one retry).
- Judge near-miss: a zsh glob error masqueraded as an empty grep result and
  nearly produced a wrong omission verdict — evidence-citation requirement
  caught it.

## Deep-task session — n8n console redaction (`deep-242da024ac04`)

Doc-stripped protocol: workspaces built from `pristine-nodocs` (all `*.md`,
`.agents/`, `.claude/`, `.cursor/` removed — 584 files on n8n) after the
repo's own `n8n:conventions` skill was observed steering an earlier, aborted
attempt. Grading baseline: `pristine-nodocs`. 5 packages, 9 gold files.

| | grep | structural |
|---|---|---|
| Matched | 6/9 | 7/9 (incl. gold's base-context chokepoint) |
| Alt-covered / not-required | 1 / 1 | 0 / 1 |
| **Confirmed omission rate** | **0.125** | **0.125** |
| jscout calls | 0 | 10 (2 search, 3 definition, 2 who_uses, 1 neighborhood, 2 workflow_candidates) |

Both arms found every console sink (all current `sendMessageToUI` producers
gated). Both missed the **same** thing: gold's hardening pair — fail-closed
policy resolution plus the license hook writing an explicit no-redaction
snapshot, so snapshot absence can never silently disable redaction. Both
arms reused the platform's pre-existing fail-open resolver and left the
hook untouched. Structural additionally flagged the Python task-runner
stdout path as a policy bypass (beyond gold's own scope) and chose gold's
exact base-class chokepoint where grep gated the two leaf call sites.

## Cross-session synthesis (three task pairs, six graded arms)

1. **First nonzero omission rates appeared only on the deep task** (0.125
   both arms) — depth cuts, but it cut both arms identically.
2. **The consistent human-vs-agent gap is the hardening layer**: fail-closed
   semantics + license-gate snapshot (redaction), injectable `now`
   (Insights). These live in operational history — flaky tests, production
   incidents — not in code structure. Neither grep nor structural retrieval
   surfaces them. This is the strongest empirical argument in the program
   for the memory layer over the retrieval layer: what agents miss is
   precisely what follow-up commits record.
3. **Structural's recurring edge is architectural placement**, not outcome:
   serializer-level injection (Insights), base-class chokepoint (redaction),
   7/7 and 7/9 direct matches. Grep is never behind on outcome.
4. Doc-stripping did not visibly degrade localization on the one task run
   without docs (all sinks found); n=1.

## Implications for the registered runs

- The pre-registered primary (confirmed-omission rate, MIE 15 points) is
  unlikely to separate arms at Sol-grade capability on these arcs; the
  edge-case rate and architectural-fidelity texture are where differences
  live. Consider adding a weaker-model arm before spending registered trials.
- The SKILL.md channel is now the default integration mechanism to test in
  any adoption-sensitive arm.
