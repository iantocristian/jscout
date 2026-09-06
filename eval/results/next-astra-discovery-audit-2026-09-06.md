# Astra Next.js discovery audit and proposed follow-ups

Recorded 2026-09-06. This is a follow-up analysis of the six completed attempts
in the [September 5 capability report](next-astra-full-scout-2026-09-05.md), not
another experiment. The original grades, model/product pins and usage accounting
remain unchanged. No solver, scout or index was rerun for this audit.

**Bottom line:** jscout mostly supplemented discovery rather than replacing
repository-targeting `rg`. Broad queries are a clear efficiency target, but the
records do not justify attributing all extra tokens to duplicate source reading.
Verification effort and incomplete output capture prevent that conclusion.

The actions below are **proposed experiments and measurement work**, not changes
to product defaults or authorization to start a new matrix. Execution budget,
repetition count and the precise no-`rg` restriction still need agreement.

## Findings

### 1. Repository-targeting `rg` activity remained similar

| Task / treatment | Shell batches | Batches containing `rg` | Repository-targeting `rg` | jscout calls |
| --- | ---: | ---: | ---: | ---: |
| Prefetch / control | 62 | 25 | 21 | 0 |
| Prefetch / scouted | 74 | 28 | 21 | 8 |
| Prefetch / memory available | 53 | 26 | 22 | 4 |
| Root params / control | 40 | 18 | 17 | 0 |
| Root params / scouted | 49 | 22 | 16 | 6 |
| Root params / memory available | 71 | 25 | 22 | 9 |

Totals: 349 shell batches, 144 containing `rg`, 119 repository-targeting `rg`
batches and 27 jscout calls. The other 25 `rg` batches target logs (17), generated
output (4), installed dependencies (2), or instruction-file enumeration only (2).

A batch can contain several commands; piped `rg` filters are not counted as
additional batches. Repository-targeting means at least one `rg` searches
repository source, tests, docs, configuration or paths. Such a batch can also
contain log inspection or setup. These are not counts of redundant searches:
test-harness investigation is repository-targeting too.

In indexed arms, 22,459 of 393,103 mapped JS/TS source bytes (5.7%) arrived through
jscout; the rest arrived through shell. jscout primarily provided locators and
small excerpts, with shell supplying bulk source. That percentage describes the
mapped subset, not all model input; see the coverage limitation below.

### 2. Two broad queries consumed 36.1% of MCP response bytes

| Arm / event | Query | Returned / total matches | Canonical response bytes |
| --- | --- | ---: | ---: |
| Root/scouted `item_11` | `root-params` | 200 / 3,821 | 30,400 |
| Root/memory `item_12` | `root-params.d.ts` | 100 / 5,743 | 12,591 |

Both exhaustive queries warned that tokenized terms were OR-joined. Neither
traversal was paged further, and no later mapped shell source came from its
returned files. Together they account for 42,991 of 118,985 canonical MCP bytes.
This is the clearest identified low-yield retrieval payload; it does not prove
that removing those calls alone would reduce attempt time or improve a patch.

All 27 calls succeeded. Aggregate server time was 22.772 seconds, excluding agent
interpretation and follow-through. Canonical content is counted once, not twice
for equivalent structured/text transport copies. It is not a token count.

### 3. Discovery overlaps exist, but are not all redundant

The calls comprise 19 searches, four definitions and four usage lookups. No
complete tool/argument set repeats within an arm. Search results contain 24
locators already returned by an earlier search out of 419 total hits. Excluding
the two broad queries leaves 24 repeated locators among 119 hits.

| Indexed arm | Non-broad hits | Previously returned locators | Identifier queries also scanned with source `rg` |
| --- | ---: | ---: | ---: |
| Prefetch / scouted | 47 | 18 | 3 |
| Prefetch / memory available | 23 | 1 | 0 |
| Root params / scouted | 16 | 1 | 0 |
| Root params / memory available | 33 | 4 | 1 |

Four of the 11 exhaustive single-identifier queries overlap a source `rg`
pattern in the same arm:

- Prefetch/scouted: `hasDynamicRewrite` (`item_17` jscout → `item_19` shell),
  `markRouteEntryAsDynamicRewrite` (`item_19` shell → `item_20` jscout), and
  `discoverKnownRoute` (`item_19` shell → `item_65` jscout).
- Root/memory: `runTypeCheck` (`item_47` shell → `item_48` jscout).

The scopes differ. An exhaustive repository query after a local scan can add
callers, so these are not four calls that should automatically be deleted. A
repeated locator under a different query can also answer a different question.
Root/memory's later `rg` for `writeRouteTypesManifest` in compiled
`dist/build/index.js` is output verification, not indexed-source rediscovery.

### 4. Duplicate source delivery is confirmed, including in controls

| Task / treatment | Mapped JS/TS bytes | Re-delivered JS/TS bytes | Re-delivered across shell/jscout |
| --- | ---: | ---: | ---: |
| Prefetch / control | 185,268 | 34,845 | — |
| Prefetch / scouted | 146,233 | 1,937 | 246 |
| Prefetch / memory available | 107,587 | 2,436 | 2,295 |
| Root params / control | 53,440 | 4,151 | — |
| Root params / scouted | 71,550 | 3,689 | 940 |
| Root params / memory available | 67,733 | 1,382 | 0 detected |

Examples from retained source:

- Root/scouted reads the full `route-types-utils.ts` in shell `item_6`, then gets
  `definition(writeRouteTypesManifest)` in `item_13`: 698 normalized source bytes
  already delivered. Its subsequent `who_uses` asks a different question.
- Prefetch/memory reads the relevant range in shell `item_8`, then gets 1,393
  normalized bytes again in definition `item_11`. Its explicit 1,500-byte request
  cap caused flagged truncation; that is not an unflagged missing-body failure.
- Prefetch/control reads the full `optimistic-routes.ts` in `item_2`, then reads
  overlapping ranges later. That file alone contributes 27,719 repeated bytes.

The remaining detected cross-channel repetition is mainly small snippets inside
later, larger shell reads. Fetching missing surrounding context can be useful.
The total 3,481 detected cross-channel duplicate bytes therefore cannot all be
labeled waste, nor compared with token increases as though capture were complete.

### 5. Verification effort and instructions confound the efficiency comparison

Every shell batch has one manually reviewed purpose:

| Task / treatment | Investigation | Execution | Edits | Logs | Setup | Orientation | Patch review | Mixed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Prefetch / control | 16 | 5 | 1 | 4 | 2 | 1 | 0 | 33 |
| Prefetch / scouted | 22 | 7 | 2 | 7 | 5 | 3 | 4 | 24 |
| Prefetch / memory available | 17 | 5 | 0 | 9 | 0 | 1 | 2 | 19 |
| Root params / control | 12 | 10 | 2 | 0 | 2 | 1 | 1 | 12 |
| Root params / scouted | 9 | 6 | 1 | 7 | 1 | 1 | 0 | 24 |
| Root params / memory available | 13 | 21 | 8 | 1 | 4 | 1 | 2 | 21 |

Investigation includes test-harness reads. Execution includes compiler probes,
builds, tests, lint and format checks. Mixed batches combine purposes; their
entire output is not attributed to each component. Edits here count pure shell
editing batches, not all edits: 24 explicit file-change events and edits inside
mixed batches are separate. These labels are not necessary/avoidable verdicts.

Root/memory performs 21 execution-only batches versus control's 10. Its
`item_58` inspects compiler inputs, and `item_59` tests fallback variants through
the TypeScript compiler host. The final patch fixes the ambient module's `any`
behavior and passes 12/12 supplemental cases versus 3/12 for the other patches.
That ties some extra work to a relevant investigation without proving every
extra command was necessary.

Prefetch/scouted also retains 65,644 bytes in orientation-only batches. Just
5,836 are the jscout guide; the rest are Next.js skills and repository
instructions. Controls read some instructions inside other categories, so the
category difference is not a jscout-specific overhead estimate.

### 6. Memory availability was not an exercised treatment

Neither memory-equipped solver queried semantic memory or received an artifact.
Both read the guide, which routes ordinary discovery away from attached memory
and makes separate memory inquiry conditional. The stronger root patch cannot
be credited to memory, and this run cannot establish whether consulting memory
would have helped. This confirms the earlier report's treatment-use finding.

## Method and limits

The audit maps actual retained output to immutable task-parent contents using
unique four-consecutive-line anchors, extended through exactly matching adjacent
lines. It counts nonempty line-content UTF-8 bytes, excluding framing, newlines
and trailing whitespace. Source identity is path, baseline line number and
normalized content. Cross-channel repetition compares with the first delivery
channel, not the immediately preceding read. Changed/generated source is not
reconstructed; the displayed source table covers JS/TS-family files only.

**The source counts are lower bounds.** Some recorded commands request source
absent from their retained `aggregated_output`: root/memory `item_5`, for example,
names a plugin read but retains a file listing. Four investigation batches have
empty output. Short/ambiguous snippets, truncation, unsupported file types and
unexpanded brace/glob-only paths can also be missed. Intended output is never
reconstructed and counted as observed delivery. Coverage differs across arms;
lower detected repetition does not establish lower total repetition.

One attempt per cell does not establish a treatment effect. Attempt duration is
not first-time-to-solve; only final patches were graded. Cached input includes
conversation replay, not that many distinct source tokens. Bytes and tokens
must not be conflated. The [original report](next-astra-full-scout-2026-09-05.md)
retains full timing, token, correctness and preparation accounting.

## Proposed actions, in execution order

### A1. Establish a current, measurable comparison baseline

**Why:** the tested binary predates later fixes, and incomplete output capture
limits attribution. This is a prerequisite, not another instruction treatment.

- Rebuild latest main once for the campaign; pin its commit/binary hash and use
  that same build for all comparison cells. Do not compare a new guide only
  against an older-binary control and attribute the difference to instructions.
- Keep the existing task parents and prepared state fixed where compatible.
  Check compatibility on independent database copies and record the minimal
  index/enrichment refresh required by the newer build. Reuse compatible vectors;
  do not promise that every old database can be used unchanged.
- Check tool-output capture on a small harness fixture before expensive runs:
  retain complete payloads, explicit truncation, per-call timing and source spans
  where available. Separate canonical payload size from transport duplication.
- Freeze comparison cells, order, repetitions and budget before running. Record
  setup/refresh costs separately from solver effort. Use the same original and
  supplemental grading for every corresponding arm, with no hidden-test feedback
  supplied to solvers. Do not drop failed or incomplete attempts.

**Deliverable:** a pinned run manifest and a verified capture path. Preserve
original and supplemental outcomes separately; current supplemental dev checks
use fresh servers per state, not proof of live hot-reload correctness.

### A2. Test efficiency instructions against the current guide

**Why:** repository search was not displaced, some source was fetched twice, and
two broad queries generated disproportionate low-yield output.

- Start with exact identifiers when known; scope queries appropriately and act
  on broad-query warnings before requesting more results. Test these instructions
  first; this audit does not propose silently changing FTS matching semantics.
- Reuse adequate unchanged source already in context. Fetch a needed definition
  with sufficient budget once; use targeted reads to fill actual gaps. Allow
  rereading after edits, stale/truncated results, and for missing context.
- Keep caller/completeness questions distinct from fetching a definition. Do not
  suppress useful verification merely because it mentions an already-seen symbol.
- Compare with the current guide using the same binary, indexed state, model,
  task and limits. Change the efficiency instructions alone.

**Measure:** correctness first; then attempt time, fresh/cached/output tokens,
repository searches, repeated locators/source, broad-query bytes, and separate
investigation versus verification effort. Fewer calls alone is not success.
Do not promote new default guidance from one unreplicated cell.

### A3. Exercise memory explicitly as a separate instruction comparison

**Why:** availability did not cause either solver to consult the prepared memory.

- Compare the existing conditional-memory guide with an explicit requirement to
  make a relevant memory inquiry at an identified symbol/workflow, inspect a
  relevant returned artifact if one exists, and verify its claims against source.
- Keep the prepared memory state and other instructions identical across this
  pair. Do not change efficiency instructions at the same time.
- Record the inquiry, artifacts actually delivered/read, source verification and
  whether memory supplied information beyond code discovery. An empty or
  irrelevant memory result is an outcome, not a reason to fabricate a use case.

**Measure:** actual treatment use and correctness/cost, not just memory being
enabled. This remains an experiment, not mandatory product behavior.

### A4. Run a clearly defined no-`rg` condition

**Why:** similar `rg` counts leave open whether indexed discovery can replace the
filesystem-search path when the solver is required to rely on it.

**Recommended scope, to confirm before execution:** prohibit repository content
and path discovery through `rg` and equivalent `grep`/`find`/script substitutes.
Permit targeted reads of already-known paths, edits, builds/tests, log inspection
and generated-output verification. Compare this with A2's guide on the same
state, changing only the search restriction.

A literal blanket `rg` ban is a different condition: it also changes log and
verification workflows. Do not silently substitute one definition for the
other. State the rule in the prompt and audit compliance from actual commands;
report violations rather than claiming a restriction the runner did not enforce.

**Measure:** correctness, fallback needs, missing discovery capabilities and cost.
A failure can expose a tool/instruction gap; do not force an apparent success by
removing tests or allowing undeclared search substitutes.

### A5. Compare the existing cases before selecting new ones

Report all corresponding cells against the same grades and separate necessary
verification from discovery overhead. Use repeated paired trials with
counterbalanced order before drawing a general efficiency conclusion; decide
the repetition count and acceptable trade-offs with the user, not in this report.

Only then recover the prior Next.js candidate list and inspect history for a new
cross-package or indirect-dependency case. Predeclare its task/oracle and check
that the parent fails and the reference succeeds before admitting it. **New
cases and their fresh reindex remain last**, as requested. These six attempts
alone do not establish that Next.js is too small or a poor evaluation target.

## Evidence and validation

Raw traces, databases and the detailed audit remain outside the product repo.
The external campaign is `jscout-replay-runs/next-astra-full-scout-2026-09-05/`;
the audit is under `analysis/discovery-audit-2026-09-06.RguLTz/`.

Its `final-summary.json` contains the per-arm aggregates and SHA-256 manifest for
the six traces, audit implementation, reviewed labels and per-delivery data.
The six event hashes also match the evidence list in the
[original machine report](next-astra-full-scout-2026-09-05.json).

| Audit artifact | SHA-256 |
| --- | --- |
| `final-summary.json` | `b46b5abbb79b1af56581812bccb406eaf68adf2979062a6cd497a7445f5cb262` |
| `reviewed.json` | `fbb60acd349045a88fcc244ce4af7b43920fae6a4f18a79e4cb2ddfd14ef59e5` |
| `audit.mjs` | `b1892b2b14596896255f5ee8261343f76f28a686e4c3fe23a9fe7d91eb9fd10e` |

From that external directory, `node --test audit.test.mjs` passes 13 tests covering
source normalization, ambiguity, canonical MCP handling, 349 purpose labels,
144 `rg` classifications, arithmetic, real-trace overlaps and unchanged inputs.
`node audit.mjs reviewed-labels.json` recomputes detailed data;
`node summarize.mjs` recomputes the summary. They only read traces and Git blobs
at the frozen task parents, never execute recorded commands. Reproduction needs
the external traces and the Next.js Git objects; this documentation-only PR does
not vendor those artifacts or make the audit runnable from a bare product clone.
