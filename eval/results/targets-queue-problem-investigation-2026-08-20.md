# TargetsQueue problem-solving investigation

- Date: 2026-08-20
- Status: observational real-use evidence
- Workload: investigate a runaway target-selection/update feedback loop and
  review the limits and edge cases of a proposed fix
- Outcome boundary: source-level mechanism and review findings were produced;
  this was not a controlled implementation-outcome comparison

## Scope and claim boundary

This session is the first retained call trace in this review round from an
actual problem-solving investigation rather than a general repository-
architecture inquiry. The agent had to localize an unfamiliar subsystem,
identify a feedback mechanism, distinguish existing protections, and inspect
the edge cases of a proposed change.

It therefore provides evidence about tool selection after localization and
about which response fields help a concrete investigation. It does not by
itself establish that jscout improves patch correctness, tokens, or time: the
session was neither counterbalanced nor compared with a non-jscout arm, and it
did not report a hidden-oracle implementation result.

The measurements and qualitative judgments below were reported by the agent
after the session. Every one of the 19 jscout responses retained its
`rendered_bytes` measurement. One outer combined tool response was truncated,
so the measured inner response total is slightly larger than the text that
actually reached the model.

## Volume

| Response class | Calls | Reported bytes |
|---|---:|---:|
| Expanded `semantic_search` | 9 | 162.9 KB |
| Non-expanded `semantic_search` | 4 | 38.1 KB |
| Exact `definition` | 4 | 11.6 KB |
| `repository_overview` | 1 | 11.3 KB |
| `semantic_memory` | 1 | 4.6 KB |
| **Total** | **19** | **228.5 KB / 223.1 KiB** |

The agent estimated the total at roughly 60k–65k raw tokens for JSON, paths,
and TypeScript. Approximately 11.8k additional tokens from client-side tool
schema discovery were excluded, as were repeated reads of the jscout skill.

Expanded searches generated about 71% of all measured jscout bytes. Exact
definitions generated about 5%, despite carrying the most decisive source
evidence. The agent estimated that only 35–45% of the complete payload was
decision-relevant and that roughly 125–145 KB was avoidable.

## What jscout contributed

The first expanded search localized the working set:

- `TargetsQueue` and `_add`;
- `queueUpdateTargets`;
- `recalculateAttributes` and `calculateAttributes`; and
- trigger collection and iterator consumption.

Exact definitions then established the mechanism:

```text
consume target
  -> compute target
  -> calculated card / impact / link update
  -> queueUpdateTargets()
  -> append more targets
```

That evidence distinguished target-value loop prevention from a cumulative
target-selection limit and showed why compacting processed queue entries alone
cannot terminate the loop. Later targeted searches verified the runtime
collection path, all update-generated requeue paths, startup/runtime
differences, and the different lifetimes of `_sourceContext` and
`_updateContext`.

The highest-value responses were:

1. `definition(TargetsQueue._add)` at 5.5 KiB. It exposed queue insertion,
   position tracking, deduplication, processed-entry handling, priority target
   behavior, and the precise limit site.
2. `definition(recalculateAttributes)` at 2.9 KiB. It established the feedback
   mechanism directly.
3. The initial expanded `TargetsQueue` cascade search at 18.6 KiB. Its first
   hits localized the complete working set, although most attached graph data
   was unnecessary.
4. The iterator-consumer search at 16.7 KiB. Its first hits connected iterator
   consumption to `computeTarget()` and `recalculateAttributes()`.
5. Runtime-context and fan-out searches at 35.7 KiB combined. They were useful
   for edge-case review after the main mechanism was known.

The repository snapshot also changed after the reviewed pull request was
rebased; retaining the response-level snapshot/freshness signal helped the
agent notice that its evidence boundary had changed.

## Waste and failure modes

### Expansion dominated the session

Nine searches used `expand=true`. After the first pass had identified
`targets.ts`, `recalculate.ts`, `loop.ts`, and `compute.ts`, most later
expansions repeated known graph context. The useful facts were usually in the
first few hits, while full node dictionaries and unrelated neighborhood edges
consumed most of the response.

The first parallel group exceeded the outer 10,000-token result budget. A
later response reported 23,931 rendered bytes and 29,419 unbudgeted bytes while
omitting memory, supports, nodes, and edges. Its top hit was nevertheless
sufficient. Increasing the global response budget would have delivered more
discarded context rather than fixing the interaction pattern.

### Automatically attached memory was weak

The broad memory query and attached previews returned degraded, stale, or weak
artifacts such as an export workflow, scheduled dependency ordering, and
generic file summaries. They suggested occasional files but did not answer the
incident question. One warning-code search attached unrelated workflow memory.
All decisive claims were verified in source.

This is evidence for making search-attached memory explicit rather than for
widening its budget. Direct `semantic_memory` remains available when a task
actually requires persistent workflow or concept retrieval.

### Exact retrieval remained useful under vector degradation

One `_targets`/`_positions`/`_next` query reported degraded vector retrieval and
recommended embedding the repository. Exact lexical/symbol retrieval still
localized the intended code. Pure exact-identifier intent must not depend on a
healthy vector provider.

### Two compact fields overclaimed or over-rendered

Code inspection after the session confirmed both issues:

- compact graph rendering copies the checker's raw `receiverTypes` strings.
  One edge therefore included essentially the complete generic
  `Errors<{...}>` calculator error catalogue rather than the useful receiver
  head `Errors`;
- search computes `used_by` by counting every `refs.target_name` match for up
  to three whitespace-delimited symbols in the chunk. It does not resolve the
  references to the hit's exact anchor. Values such as
  `recalculateAttributes: 333 sites` and `calculateAttributes: 380 sites` are
  repository-wide same-name occurrence counts, not literal callers of the
  returned definition.

These are product contract defects. Compact receiver displays need a bounded
type head with explicit truncation, while complete type strings remain a debug
detail. Name-level counts must either be replaced with anchor-resolved incoming
edges or be labelled as approximate name occurrences; `who_uses` remains the
authoritative exact drill-down.

### Orientation and discovery overhead were avoidable

The skill caused an 11.3 KB `repository_overview` call even though the
repository, package, and task area were already supplied. Overview is useful
only when the agent lacks repository orientation or stable task anchors.
Client-side printing of complete hidden tool schemas added another estimated
11.8k tokens; that is a client presentation problem rather than a second
jscout discovery protocol.

## Exact call inventory

| # | Call | Reported size |
|---:|---|---:|
| 1 | `repository_overview(area_limit=12, relation_limit=10, reconnaissance_limit=10)` | 11.0 KiB |
| 2 | `semantic_memory("Engine Dynamic runaway target cycle …")` | 4.4 KiB |
| 3 | `semantic_search("TargetsQueue _add queueUpdateTargets calculated target cascade", expand=true)` | 18.6 KiB |
| 4 | `semantic_search("calculated attribute cycle detection dependency graph …", expand=true)` | 18.4 KiB |
| 5 | `definition(TargetsQueue)` | 0.8 KiB |
| 6 | `definition(TargetsQueue._add)` | 5.5 KiB |
| 7 | `definition(recalculateAttributes)` | 2.9 KiB |
| 8 | `definition(registerInstructions)` | 2.1 KiB |
| 9 | `semantic_search("loopableVariables populated …", expand=true)` | 16.7 KiB |
| 10 | `semantic_search("W1378_CALCULATION_LOOP preventLoop procedures …")` | 11.2 KiB |
| 11 | `semantic_search("TargetsQueue compact processed null backing array …")` | 9.7 KiB |
| 12 | `semantic_search("preventLoop TargetSelection queueUpdateTargets …", expand=true)` | 23.4 KiB |
| 13 | `semantic_search("TargetsQueue backing array processed null positions …", expand=true)` | 15.0 KiB |
| 14 | `semantic_search("TargetsQueue _targets _positions _next …", expand=true)` | 14.6 KiB |
| 15 | `semantic_search("for target of targets computeTarget …", expand=true)` | 16.7 KiB |
| 16 | `semantic_search("maxUpdateTargets cumulative queue limit …")` | 7.7 KiB |
| 17 | `semantic_search("queueCardRuntimeTargets call sites …", expand=true)` | 18.6 KiB |
| 18 | `semantic_search("TargetsQueue size after iteration performance …")` | 8.6 KiB |
| 19 | `semantic_search("queueUpdateTargets results.cards results.impacts …", expand=true)` | 17.1 KiB |

No `who_uses`, `neighborhood`, `paths`, `events`, `calls`, `entities`,
`file_outline`, or `annotate` call was made. Unlike the earlier architecture
inquiry, this agent selected four exact `definition` calls naturally, and those
calls were the most byte-efficient part of the investigation.

## Design consequences

1. Preserve exact definition as a separate, copy-safe drill-down. The evidence
   argues against automatically inlining full definitions into search hits.
2. Make search-attached semantic memory opt-in. Keep direct semantic-memory
   discovery explicit for causal/workflow questions.
3. After one localization expansion, guide the agent to exact definitions,
   exact usages, and unexpanded searches. A later expansion should name the
   unresolved boundary it is trying to cross.
4. Replace induced-neighborhood expansion in compact search with the G19 path
   projection. A depth-one request should naturally become a compact
   caller/callee view without another tool mode.
5. Bound receiver-type display and correct or relabel name-level incoming
   counts.
6. Record per-section canonical bytes in telemetry/debug (`hits`, `graph`,
   `memory`, and envelope) so transport work can be measured without adding
   routine response overhead.
7. For a pure identifier query with admitted exact results, stop at the exact
   tier by default and make hybrid widening explicit. Mixed behavioral queries
   retain hybrid retrieval.
8. Deduplicate repeated import/export occurrences without collapsing distinct
   executable occurrences in the same file.
9. Skip overview when repository/package context or stable task anchors already
   provide orientation.

Two suggestions are not adopted at this stage:

- a generic `fields` selector would multiply response contracts and make skill
  use harder to stabilize; named compact/debug/artifact views and strict byte
  budgets cover the measured need;
- `include_definition: true` would put the most useful progressive drill-down
  back into the largest response class. Four separate definition calls cost
  only 11.6 KB total, so there is no measured byte case for inlining them.

## Measurement use

G19 should replay this exact 19-call inventory separately from the 42-call
architecture inquiry. The fixed-call replay measures serializer and projection
changes at equal calls and facts. A staged replay measures the different agent
strategy: skip unnecessary overview, localize once, then use exact drill-down
and unexpanded searches. Savings from those two treatments must not be pooled.

This session does not trigger G16. It exposed repeated or weak delivery and
over-broad graph projection, not a useful artifact hidden solely by G14's
evidence-connection boundary.
