# Workflow architecture-inquiry call trace

- Date: 2026-08-19
- Status: observational real-use evidence; follow-up implementation evidence pending
- Workload: explicit repository architecture and workflow questions
- Repository scale: more than 7,000 files

## Scope and claim boundary

This session asked an agent to discover and explain several end-to-end product
workflows. It is a direct jscout use case and a stress test for repository
orientation, workflow memory, semantic search, graph expansion, and transport.
It is not an evaluation of an independent coding agent gathering information
while implementing a story or fixing a bug.

The observations support claims about localization value, tool selection, and
payload shape for architecture inquiry. They do not establish correctness,
token, or adoption effects for implementation work. Later real coding sessions
must be recorded as a separate workload rather than pooled with this one.

The call inventory and qualitative judgments below were reported by the agent
after the session. Twenty-seven calls retained exact `rendered_bytes`; raw
responses for every call were not retained. Totals involving the other fifteen
calls are estimates and are labelled accordingly.

## Volume

- 42 jscout calls; calls within each described batch were launched in parallel.
- 27 retained responses totalled 358,334 inner rendered bytes (about 350 KiB).
- Extrapolation over 15 truncated, omitted, or error responses estimated
  460–510 KiB total jscout output.
- The estimate corresponds to roughly 115k–145k raw tokens before client-side
  truncation; the agent estimated that it processed roughly 100k–115k tokens.
- Approximately 11.8k initial tool-description tokens were attributed to the
  client's discovery/orchestration behavior and are excluded from jscout output.

Call taxonomy:

| Class | Calls | Notes |
|---|---:|---|
| Repository overview | 1 | Structural orientation plus requested semantic inventory |
| Broad/narrow semantic workflow discovery | 6 | Calls 2, 9–12, and 29 |
| Failed exact-artifact reads | 6 | Calls 13–18 all sent `source_limit: 0` in one parallel batch |
| Successful exact-artifact reads | 13 | Nine unique artifacts; four additional repeated reads |
| Semantic code search | 16 | Thirteen expanded, three unexpanded |
| Standalone exact/graph/entity tools | 0 | No `definition`, `who_uses`, `paths`, `neighborhood`, `events`, `calls`, `entities`, or `file_outline` |
| Memory write-back | 0 | No `annotate`; the task was read-only inquiry |

## Exact call inventory

### Repository orientation

1. `repository_overview`
   - semantic types: workflows, summaries, concepts
   - `semantic_limit: 20`
   - `area_limit: 25`
   - `reconnaissance_limit: 15`

### Initial workflow discovery

2. `semantic_memory`
   - query: `core end-to-end product workflows data import export configuration publish authentication workbook calculation`
   - fresh workflows only
   - `limit: 30`
   - vector enabled

Calls 3–8 used `semantic_search` with production files, `limit: 6`, expansion
depth 2, 18 nodes, 30 edges, 3 seeds, and attached memory:

3. `configuration package publish migrations tenant provisioning end to end`
4. `bulk spreadsheet upload import queue worker database notification end to end`
5. `calculation engine scheduled dynamic attributes dependency execution end to end`
6. `export request queue worker object storage notification secure download end to end`
7. `workbook card user interaction query data gateway database update end to end`
8. `authentication authorization tenant user management login request end to end`

Calls 9–12 used `semantic_memory`, fresh workflows, `limit: 12`, vector
enabled, three supports per artifact, and a 12 KB response budget:

9. `configuration package publish upload migration protection notification tenant`
10. `export workspace dashboard calculated data queue download notification`
11. `workspace card load edit writeback query measure interaction`
12. `login authentication authorization tenant user roles session`

### Initial artifact drill-down

Calls 13–18 used exact `semantic_memory` with `source_limit: 0`. All failed
because the schema minimum was one. Since the batch was parallel, the first
error could not prevent the five identical failures:

13. artifact 51
14. artifact 174
15. artifact 10
16. artifact 54
17. artifact 56
18. artifact 46

Calls 19–24 retried those artifacts with `relation_limit: 6`:

19. artifact 51 — Config designer change publish
20. artifact 174 — Bulk spreadsheet import lifecycle
21. artifact 10 — Dynamic queue processing
22. artifact 54 — Workspace/dashboard export
23. artifact 56 — Workspace view rendering
24. artifact 46 — Authenticated HTTP dispatch

Calls 25–27 fetched three artifacts again, individually, with
`relation_limit: 1` and a 9 KB response budget:

25. artifact 10
26. artifact 54
27. artifact 56

28. `semantic_search`
    - query: `export route handler queue.push ExportJobPayload export worker output file`
    - production files
    - `limit: 8`
    - expansion depth 2, 20 nodes, 35 edges, 4 seeds

### Configuration-publish explanation

29. `semantic_memory`
    - query: `configuration publish end to end designer upload analysis package migrations configure database S3 notifications`
    - fresh workflows
    - `limit: 15`
    - vector enabled
    - three supports per artifact

Calls 30–33 used exact `semantic_memory`, `relation_limit: 2`, and a 10 KB
response budget:

30. artifact 75 — Configuration publish progress notifications
31. artifact 51 — Config designer publish workflow
32. artifact 91 — Apply changes and generate artifacts
33. artifact 72 — Asynchronous publish notifications

Calls 34–37 used `semantic_search`, production files, `limit: 8`, expansion
depth 2, 24 nodes, 40 edges, and 4 seeds:

34. `uploadChanges analyseChanges storeAnalysedChangesResult publishId`
35. `publishPackage startPublish runMigrations runPublishTasks saveConfigToS3 notifyPublish`
36. `publishChanges applyChanges generateFiles`
37. `handlePublishNotification connectSubscriptions published configured migrated error`

38. `semantic_search`
    - query: `CM_SERVICE_QUEUE_NAME queue worker action publishChanges publishPackage`
    - production files
    - `limit: 8`
    - expansion depth 2, 20 nodes, 30 edges, 4 seeds

### Configuration-preparation explanation

39. `semantic_search`
    - query: `prepare configuration package tar.gz required files build upload analyse before publish`
    - production and documentation files
    - `limit: 10`
    - expansion depth 2, 24 nodes, 40 edges, 4 seeds
    - attached memory, `memory_limit: 5`

Calls 40–42 used `semantic_search`, production and documentation files,
`limit: 8`, no expansion, and attached memory with `memory_limit: 3`:

40. `configuration tarball parseAndValidateAsync required info dataset tree poFiles jsFiles`
41. `config package build tar.gz data-demo package script`
42. `dataAction userSavedWorkbooksAction skipDiffAction finalProtectionLevel uploadTarball`

## Value observed

The session judged jscout useful primarily for localization in a large
monorepo. The highest-value calls were:

- semantic workflow discovery for configuration publishing;
- the expanded search connecting `uploadChanges`, `analyseChanges`,
  `storeAnalysedChangesResult`, and `publishId`;
- the mutation-path search connecting `publishPackage`, `startPublish`,
  `runMigrations`, `runPublishTasks`, and notification behavior;
- queue-name localization proving how worker execution reaches the publishers;
- exact artifact drill-down for workflow intent before source verification; and
- repository overview for scale, package areas, and structural orientation.

The stable source-verified skeleton was:

```text
uploadChanges
  -> analyseChanges route
  -> workerExecution
  -> storeAnalysedChangesResult
  -> /publish/{publishId}
  -> publishPackage / publishChanges
  -> startPublish
  -> runMigrations
  -> runPublishTasks
  -> endPublish
  -> notifyPublish
  -> persisted notification
  -> UI state update
```

Useful response material was estimated at 25–35% and consisted mainly of:

- exact symbols, files, and lines;
- small source snippets and copy-safe anchors;
- direct `uses`/`used_by` and cross-file call edges;
- one-sentence workflow descriptions;
- defining workflow participants; and
- freshness information.

Direct source inspection with local `rg` and `sed` remained necessary to prove
runtime behavior. This is consistent with jscout's complementary localization
role. It also means this session provides no evidence that implementation
agents will select jscout's exact drill-down tools under edit pressure.

## Waste and failure modes observed

- Complete snapshot/origin/tool argument objects were repeated for many hits;
  none of the emitted follow-ups were selected in this workload.
- Broad semantic discovery returned duplicate or overlapping import workflows
  and unrelated matches; retrieval score did not establish importance.
- Thirteen searches requested graph expansion. Multiple expansions launched in
  one batch took several minutes and the combined client output omitted later
  results.
- Six parallel artifact reads failed on the same `source_limit: 0` constraint.
- Thirteen successful detail reads covered nine unique artifacts. Artifacts 10,
  51, 54, and 56 were each read twice successfully.
- Several full artifacts fetched in parallel were truncated or interleaved,
  leading to smaller repeat requests.
- Exact multi-identifier matches sometimes selected import occurrences rather
  than executable behavior.
- Exact artifact details carried full provenance, support hashes, related
  artifacts, and supporting leaf helpers after the defining workflow was known.
- Expanded graphs returned unrelated nodes and common calls rather than only
  the shortest useful cross-file continuations.
- Successful retrieval and graph-attachment diagnostics occupied agent context
  despite being primarily evaluation/telemetry material.
- JSON text was observed escaped inside an outer tool-result representation.
- Repeated semantic attachments continued after the relevant artifacts had
  already been discovered.

The reported 2,000-node bound belongs to the attached-memory evidence-connection
traversal unless separate telemetry shows otherwise; explicit returned expansion
bounds in this session were 18–24 nodes. These must not be conflated.

## Architecture consequences

### G14 interaction

The skill should favor narrow sequential searches, limits of 4–6, exact source
drill-down when useful, one expansion at a time after localization, and
`include_memory: false` after useful memory is known. Local source tools remain
valid; this inquiry does not justify forcing all verification through jscout.

The absence of follow-up selection does not justify deleting every complete
copy-safe object before observing implementation work. G19 retains one complete
top-hit handoff and represents lower hits with anchors and compatible tool names.

### G17

Exact occurrences require syntax-aware ordering so imports do not consume the
reserved occurrence for an identifier when an executable use is available.

### G18

Workflow overlap requires support/participant-aware diagnosis before
deduplication. A source-verified consolidated workflow is appropriate for
`annotate` only when persistent write-back is explicitly authorized; a
read-only question does not authorize it automatically.

### G19

G19 owns compact artifact views, routine-diagnostic gating, concise
cross-origin-safe follow-ups, path-shaped expansion, and structured-content
compatibility. Its evaluation separates:

1. fixed-call transport replay, holding valid calls and arguments constant; and
2. staged-session replay, measuring fewer calls and better orchestration.

Only the first may support the serializer byte-reduction claim. The six invalid
calls are excluded from fact-parity bytes and tested separately with the new
zero-means-omit behavior.

### G16

This session does not trigger G16. Useful artifacts were found directly through
`semantic_memory`, and attached memory was repeatedly delivered. No useful
artifact was shown to be rejected solely by G14's evidence-connection boundary.

## Follow-up evidence requested

Future observations should preserve the same telemetry for real implementation
and bug-fix work:

- task type and requested outcome;
- exact ordered calls and whether batches were parallel;
- inner, wire, and client-visible bytes;
- follow-up objects delivered and actually selected;
- source verification method;
- artifacts delivered, opened, repeated, or ignored;
- first edit time and symbols/files edited;
- whether retrieved workflow evidence affected the implementation mechanism;
- test/oracle outcome; and
- whether memory write-back was requested or authorized.

Architecture-inquiry and implementation workloads remain separate in reports.
They may be compared descriptively but must not be pooled into one product-value
estimate.
