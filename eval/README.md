# Agent evaluation

This harness compares agent behavior with the same prompts, model, limits, and
repository snapshot under controlled retrieval profiles:

- `grep`: no repository-index MCP server; shell/filesystem search only;
- `baseline`: existing retrieval/definition/usage tools; no `neighborhood` and
  no search expansion;
- `structural`: the same tools plus `neighborhood` and opt-in search expansion.

The included fixture validates the protocol. It is not evidence that jscout
improves behavior on a large real repository.

## Codex runner

`scripts/eval-run-codex.mjs` runs a frozen repository task set through Codex in
the configured profiles, counterbalances profile order, captures structured responses,
and tags jscout telemetry by task/profile/session. It isolates user config and
disables unrelated tools by default.

First run a naturalistic adoption pass by omitting `--require-jscout`. Then run
the capability comparison with `--require-jscout true`, which gives both
profiles the same integration instruction to start with jscout but does not
tell the structural agent to use expansion or `neighborhood`.

For the next value comparison, use three arms: `grep` has no jscout MCP server,
`baseline` has indexed search/definition/usage tools, and `structural` adds
expansion and `neighborhood`. Declare the same list in the task set's optional
top-level `profiles` field so the grader can report entirely missing arms:

```bash
node scripts/eval-run-codex.mjs \
  --tasks eval/tasks/ai-pipe-p0.json \
  --repository /path/to/frozen/repository \
  --jscout "$PWD/target/debug/jscout" \
  --responses /tmp/jscout-responses.jsonl \
  --telemetry /tmp/jscout-telemetry.jsonl \
  --artifacts /tmp/jscout-artifacts \
  --model gpt-5.6-terra \
  --reasoning high \
  --profiles grep,baseline,structural \
  --trial 001 \
  --require-jscout true
```

The SC-1 source-compression gate uses `structural-full` and
`structural-elided`. Both map to the same structural tool surface and differ
only in the default `definition` source representation. Each definition uses
the same 12,000-byte source ceiling. Telemetry records rendered and original
source bytes without recording source text:

```bash
node scripts/eval-run-codex.mjs \
  --tasks eval/tasks/ai-pipe-sc1.json \
  --repository /path/to/frozen/repository \
  --jscout "$PWD/target/release/jscout" \
  --responses /tmp/jscout-sc1-responses.jsonl \
  --telemetry /tmp/jscout-sc1-telemetry.jsonl \
  --artifacts /tmp/jscout-sc1-artifacts \
  --profiles structural-full,structural-elided \
  --trial 001 \
  --require-jscout true \
  --require-definition true
```

The primary gate is answer correctness at lower
`mean_source_rendered_bytes`; compression ratio alone is not a success metric.

## Post-cutoff task admission

For fast-moving repositories, mine candidate tasks from code introduced or
materially reshaped after the model cutoff and retain the `git log -S` evidence
in the task file. Before admitting a candidate, run the same execution model in
an empty workspace with no repository or tools:

```bash
node scripts/eval-contamination-probe.mjs \
  --tasks eval/tasks/n8n-post-cutoff-pilot.json \
  --output /tmp/n8n-contamination.jsonl \
  --artifacts /tmp/n8n-contamination-artifacts \
  --model gpt-5.6-terra \
  --reasoning high
```

The probe rejects tool-using runs and fully remembered file sets. Any partial
file or symbol overlap is labelled `review` and is not automatically admitted.
The no-tool rule is verified from Codex JSONL events rather than trusted from
the prompt alone.

Use Sol/high to review gold answers independently before execution. Blindly
adjudicate exact-set disagreements after the run; do not silently rewrite gold
to favor an arm.

For repeated trials across multiple task sets, pool paired results with
task-clustered bootstrap intervals:

```bash
node scripts/eval-pooled-report.mjs \
  --tasks eval/tasks/n8n-post-cutoff-pilot.json,eval/tasks/twenty-post-cutoff-pilot.json \
  --responses /tmp/n8n-seed1.jsonl,/tmp/twenty-seed1.jsonl \
  --telemetry /tmp/n8n-seed1-telemetry.jsonl,/tmp/twenty-seed1-telemetry.jsonl \
  --adjudications eval/results/post-cutoff-adjudications-2026-08-09.jsonl
```

Use new output paths for every run; the runner refuses non-empty response,
telemetry, or artifact targets. Change `--trial` when repeating a task/profile
pair so its telemetry session remains unique. The target repository should be
a clean Git checkout because its own agent instructions may require a status
check even though the runner permits a non-Git directory. Raw artifact logs can
contain repository source; keep them outside the product repository.
If a batch is interrupted and the prior process has exited, rerun the same
command with `--resume true`; the runner preserves existing artifacts and skips
completed task/profile pairs. An exclusive response-file lock rejects
overlapping writers.

The representative P0 result is recorded in
[`results/ai-pipe-p0-2026-08-07.md`](results/ai-pipe-p0-2026-08-07.md).
The first SC-1 gate is recorded in
[`results/ai-pipe-sc1-2026-08-07.md`](results/ai-pipe-sc1-2026-08-07.md); it
keeps full source as the default. The harder three-arm task set is
[`tasks/ai-pipe-discriminating.json`](tasks/ai-pipe-discriminating.json), with
its result recorded in
[`results/ai-pipe-discriminating-2026-08-07.md`](results/ai-pipe-discriminating-2026-08-07.md).

## 1. Build and index the fixture

```bash
cargo build
target/debug/jscout index eval/fixtures/structural
```

## 2. Run every task in both profiles

Use prompts from `tasks/structural.json`. Give both conditions the same model,
system prompt, turn/token budget, and repository state. Start the MCP server
with a unique session and the task id:

```bash
JSCOUT_TASK_ID=http-to-inventory-path \
JSCOUT_SESSION_ID=baseline-http-to-inventory-path-001 \
target/debug/jscout mcp eval/fixtures/structural \
  --profile baseline --telemetry .jscout-telemetry.jsonl
```

Repeat with `--profile structural`. Do not tell only the structural agent that
it must use graph tools; tool selection is one of the measured outcomes. An
identical jscout integration instruction for both profiles is a separate,
assisted capability comparison and must be labelled as such.

Record one JSONL outcome per run using the shape in
`responses.example.jsonl`. `files` and `symbols` are the agent's final claimed
answer, not every location it inspected. `correct` is an optional independent
task-level judgment for answers whose ordering or explanation matters beyond
set overlap. When the agent runner exposes them, also record
`inspected_files`, `total_tokens`, and `duration_ms`; the grader reports
irrelevant file reads and resource deltas without fabricating missing values.

## 3. Grade

```bash
node scripts/eval-report.mjs \
  --tasks eval/tasks/structural.json \
  --responses eval/responses.jsonl \
  --telemetry .jscout-telemetry.jsonl
```

The report joins calls by `(task, profile, session)` and reports file/symbol
precision and recall, optional correctness rate, tool calls, failures, latency,
and returned bytes. Missing task/profile runs are listed explicitly.

The fingerprinted `ai-pipe` task set is the first representative-repository P0
run. It is a bounded direction gate, not a general product benchmark.

## Task admission and pooling discipline

- `scripts/eval-anchor-certify.mjs --tasks <sets> --repository <frozen checkout>`
  classifies each task as `anchored` / `weak` / `anchor-free` by whether prompt
  tokens appear in gold files. Graph-discriminating suites should require
  `anchor-free` (`--require anchor-free`); the 2026-08-09 Twenty pilot tasks
  all certify `weak`.
- Responses now record `model` and `reasoning`;
  `scripts/eval-pooled-report.mjs` refuses to pool mixed execution models
  unless `--allow-cross-model true` is passed and the result is labelled.
- Retrieval changes re-run a recorded suite only with a pre-registration in
  `eval/prereg/` (first: [file-roles](prereg/file-roles-2026-08-09.md)).
- File-role runs read `tool_profiles.<profile>.expansion_role_counts` and
  `expansion_test_fixture_generated_share` from the pooled report. These are
  aggregated from privacy-minimal MCP telemetry rather than agent-authored
  inspected-file lists.
- Backfill the same role share for pre-role telemetry from retained raw Codex
  artifacts with `scripts/eval-expansion-role-backfill.mjs`; it reads only
  expansion node paths and the frozen repository's indexed role column. The
  registered baseline is recorded in
  `results/file-roles-prechange-expansion-backfill-2026-08-09.json`.
- The SC-2a memory gate is specified in
  [protocols/two-session-memory.md](protocols/two-session-memory.md).
- The post-SC-2a workflow-scope treatment is pre-registered in
  [prereg/workflow-participant-scope-2026-08-09.md](prereg/workflow-participant-scope-2026-08-09.md).
- Candidate-closed workflow scouting and its structural-before-semantic gate are
  pre-registered in
  [prereg/candidate-closed-scouting-2026-08-09.md](prereg/candidate-closed-scouting-2026-08-09.md).

## Two-session semantic-memory runner

`scripts/eval-run-memory.mjs` executes each admitted pair as one session-1
write-back run plus counterbalanced cold/warm session-2 runs. Pairs with a
controlled `staleness.edit` also get a stale session-2 arm after a copy-on-write
repository clone, exact edit-hash validation, and isolated reindex. It requires a
schema-v6 seed database with empty semantic tables, gives each arm an isolated
copy-on-write clone through `jscout mcp --database`, and archives the warm
database immediately after session 1. Responses record the structural seed
fingerprint and before/after semantic fingerprints.

```bash
node scripts/eval-run-memory.mjs \
  --tasks eval/tasks/memory-pairs.json \
  --repository /path/to/frozen/repository \
  --seed-database /path/to/clean-v6.db \
  --jscout "$PWD/target/release/jscout" \
  --responses /tmp/jscout-memory-responses.jsonl \
  --telemetry /tmp/jscout-memory-telemetry.jsonl \
  --artifacts /tmp/jscout-memory-artifacts \
  --model gpt-5.6-terra --reasoning high --trial 001
```

The runner requires copy-on-write database cloning because a large-repository
index is hundreds of megabytes. Keep the seed and artifact directory on one
reflink-capable volume. `--allow-full-copy true` is an explicit, expensive
fallback. `eval/tasks/memory-fixture-smoke.json` checks the runner only and is
excluded from product claims.

Task admission is a separate, ordered phase. First run session 1 only with
`--mode prepare`, then prove that its final answer cannot directly yield the
session-2 key. The probe runs the same execution model without tools or a
repository and can emit a certified task-set copy:

```bash
node scripts/eval-memory-transfer-probe.mjs \
  --tasks eval/tasks/memory-pairs-draft.json \
  --session1-responses /tmp/memory-admission-responses.jsonl \
  --output /tmp/memory-transfer-certificates.jsonl \
  --artifacts /tmp/memory-transfer-artifacts \
  --certified-tasks eval/tasks/memory-pairs.json \
  --model gpt-5.6-terra --reasoning high
```

After the registered runs, blindly adjudicate every stale response with Sol.
The judge has no repository or tools and sees only the controlled edit and
recorded answer. A pass additionally requires the runner-recorded
`inspected_files` to contain the edited file:

```bash
node scripts/eval-memory-staleness-adjudicate.mjs \
  --tasks eval/tasks/memory-pairs.json \
  --responses /tmp/memory-seed1.jsonl,/tmp/memory-seed2.jsonl \
  --output /tmp/memory-stale-adjudications.jsonl \
  --artifacts /tmp/memory-stale-adjudication-artifacts \
  --model gpt-5.6-sol --reasoning high
```

Blindly source-review exact-set disagreements without exposing arm labels:

```bash
node scripts/eval-memory-correctness-adjudicate.mjs \
  --tasks eval/tasks/memory-pairs.json \
  --repository /path/to/frozen/repository \
  --responses /tmp/memory-seed1.jsonl,/tmp/memory-seed2.jsonl \
  --output /tmp/memory-correctness-adjudications.jsonl \
  --artifacts /tmp/memory-correctness-adjudication-artifacts \
  --model gpt-5.6-sol --reasoning high --batch-size 4
```

Report one or more trials with:

```bash
node scripts/eval-memory-report.mjs \
  --tasks eval/tasks/memory-pairs.json \
  --responses /tmp/memory-seed1.jsonl,/tmp/memory-seed2.jsonl \
  --telemetry /tmp/memory-seed1-telemetry.jsonl,/tmp/memory-seed2-telemetry.jsonl \
  --adjudications /tmp/memory-stale-adjudications.jsonl
```

The report enforces single-model pooling, pairs by task/trial, checks actual
artifact reads and writes, bootstraps by pair, and implements the registered
20%/10% gate plus the hard stale-handling failures: not retrieved, not visibly
stale/degraded, or not re-verified.

For a pre-registered fixed-snapshot replay, reuse archived session-1 databases
without regenerating memory or cold rows:

```bash
node scripts/eval-run-memory-replay.mjs \
  --tasks eval/tasks/memory-pairs.json \
  --repository /path/to/frozen/repository \
  --jscout "$PWD/target/release/jscout" \
  --source-responses /tmp/memory-001.jsonl,/tmp/memory-002.jsonl \
  --source-artifacts /tmp/memory-001-artifacts,/tmp/memory-002-artifacts \
  --responses /tmp/memory-replay-responses.jsonl \
  --telemetry /tmp/memory-replay-telemetry.jsonl \
  --artifacts /tmp/memory-replay-artifacts \
  --model gpt-5.6-terra --reasoning high
```

The replay runner verifies every archived semantic-table fingerprint against
its source response before cloning it and assigns new session identifiers. A
replay whose pre-registration treats session-1 databases as frozen inputs must
pass `--require-session1-correctness false` to the report; the output records
that gate setting rather than silently changing the default protocol.
The first fixed-snapshot replay and its passing registered result are recorded
in [`results/twenty-memory-budget-replay-2026-08-09.md`](results/twenty-memory-budget-replay-2026-08-09.md).

For the participant-scope treatment, inspect the archived session-1 databases
directly and compare all participant anchors with the hidden follow-up gold:

```bash
node scripts/eval-workflow-scope-report.mjs \
  --tasks eval/tasks/twenty-memory-pairs.json \
  --responses /tmp/memory-001.jsonl,/tmp/memory-002.jsonl \
  --artifacts /tmp/memory-001-artifacts,/tmp/memory-002-artifacts
```

The report exposes every missing follow-up boundary and reports whether matched
boundaries were defining or supporting. Additional participants are diagnostic,
not false positives, because the follow-up asks for only one workflow slice.
Prose in the stored artifact cannot affect this diagnostic.
The free-form producer preflight and its decision to block the full treatment
run are recorded in
[`results/workflow-scope-preflight-2026-08-09.md`](results/workflow-scope-preflight-2026-08-09.md).

Run candidate-closed Stage A before any semantic-classification calls:

```bash
node scripts/eval-workflow-candidate-gate.mjs \
  --tasks eval/tasks/twenty-memory-pairs.json \
  --repository /path/to/frozen/twenty \
  --database /path/to/frozen/twenty-v6.db \
  --jscout "$PWD/target/release/jscout" \
  --output /tmp/twenty-workflow-candidates.json
```

Seeds are mechanically resolved from session-1 gold only. The report keeps
candidate recall, per-pair misses, and graph truncation separate; a failure
stops the experiment before any Terra classification run.
The failed Stage A run, resolver diagnosis, deterministic repair rerun, and
decision to require explicit runtime-boundary entities are recorded in
[`results/twenty-workflow-candidate-gate-2026-08-09.md`](results/twenty-workflow-candidate-gate-2026-08-09.md).
The first runtime-boundary implementation and its real-repository regression
check are recorded in
[`results/runtime-boundary-entities-2026-08-10.md`](results/runtime-boundary-entities-2026-08-10.md).
The separate contract-plane implementation, scale cost, and type-only barrel
fixture are recorded in
[`results/contract-plane-2026-08-10.md`](results/contract-plane-2026-08-10.md).
The post-merge fixes for documentary module-edge labeling, scoped generic
parameters, and contract graph load cost are recorded in
[`results/contract-plane-followups-2026-08-10.md`](results/contract-plane-followups-2026-08-10.md).
The route, GraphQL, environment, database, feature-flag, and external-host
entity implementation is recorded in
[`results/general-entities-2026-08-10.md`](results/general-entities-2026-08-10.md).
The post-merge recognizer corrections for handler attribution, named routers,
database holders, GraphQL option objects, and configuration keys are recorded
in [`results/general-entities-followups-2026-08-10.md`](results/general-entities-followups-2026-08-10.md).
The deterministic repository overview, entity lookup, bounded paths, and the
known-workflow regression rerun are recorded in
[`results/agent-surfaces-2026-08-10.md`](results/agent-surfaces-2026-08-10.md).
The post-merge path availability bound, SQL-ranked entity lookup, explicit
reverse steps, and dependency-area coverage are recorded in
[`results/agent-surfaces-followups-2026-08-10.md`](results/agent-surfaces-followups-2026-08-10.md).
The workflow-specific logical traversal and its 24/24 known-regression result
are recorded in
[`results/workflow-logical-routing-2026-08-10.md`](results/workflow-logical-routing-2026-08-10.md).
The post-merge hub-threshold fixture, directional DI fan-out rule, degree
memoization, and symbol-only seed contract are recorded in
[`results/workflow-logical-routing-followups-2026-08-10.md`](results/workflow-logical-routing-followups-2026-08-10.md).
The three-seed, post-cutoff n8n/Twenty follow-up is recorded in
[`results/n8n-twenty-post-cutoff-2026-08-09.md`](results/n8n-twenty-post-cutoff-2026-08-09.md).
The pre-registered file-role re-run is recorded in
[`results/file-roles-rerun-2026-08-09.md`](results/file-roles-rerun-2026-08-09.md).

## PR-replay suite (primary value suite)

Design and pre-registration sketch:
[`value-hypotheses-2026-08-09.md`](value-hypotheses-2026-08-09.md). Real
merged changes are replayed from a story; the agent implements them in a
history-free snapshot; grading compares against the real implementation.

1. **Mine** candidates (scaffolds only — no diff content, so the story
   author is never staring at the answer):

   ```bash
   node scripts/eval-pr-mine.mjs --repository /path/to/repo \
     --since 2026-01-01 --output eval/tasks/repo-replay-candidates.json
   ```

2. **Author stories** from the symptom/issue text — never the diff — then
   certify each story against the gold patch files with
   `eval-anchor-certify.mjs` (must not be `anchored`).

3. **Prepare** each admitted task. The workspace is a `git archive` export
   with **no `.git`** (a parent checkout still contains the real commit in
   its object store); the gold bundle lives outside the agent sandbox and
   must never be mounted into it. Repository-specific `setup_command` and
   `test_command` values live in the task set. Setup installs dependencies and
   builds the historical parent once; then the test-only patch must apply
   independently and FAIL, else the task is layer-1 ineligible:

   ```bash
   node scripts/eval-pr-prepare.mjs \
     --tasks eval/tasks/next-calibration.json \
     --repository /path/to/next.js \
     --runs-root /runs/next-calibration \
     --work-root /tmp/jr
   ```

   The prepared `pristine/` tree includes `node_modules` and build outputs.
   Preparation and grading use copy-on-write clones when supported so those
   gigabytes are not recopied for every admission probe and execution arm.
   Admission overlays exact post-change test files onto two throwaway clones:
   they must fail on the parent and pass after the reference production patch.
   Exact overlays are used instead of `git apply` during grading because an
   agent may legitimately edit the same public test file.

4. **Run** the agent in the workspace (± jscout arms, counterbalanced,
   ≥2 trials), then **grade**:

   ```bash
   node scripts/eval-pr-grade.mjs --pristine /runs/<task>/pristine \
     --workspace /runs/<task>/workspace --gold /runs/<task>/gold \
     --response /runs/<task>/response.json \
     --adjudications /runs/<task>/adjudications.jsonl \
     --test-command "npm test" --output /runs/<task>/grade.json
   ```

   Grading discipline: layer 1 applies the admitted fail-to-pass tests to
   the agent's workspace; layer 2 **adjudicates before scoring** — every
   unpatched gold file is a pending row (`omission | alternative_covered |
   not_required`, blind Sol-style) and `confirmed_omission_rate` stays null
   until the queue is empty. `patched` and `plan_mentioned` metrics are
   reported separately and never combined. Navigation metrics from these
   runs inherit the H1 instrumentation caveat (self-reported
   `inspected_files` must be audited against Codex event artifacts before
   registered claims).

5. **Batch runs** use the dedicated runner (write-sandbox, per-run workspace
   workspaces, per-arm indexing, automatic grading; `--codex` is overridable
   for stubbed tests). Each jscout arm installs the shipped project-local skill
   into the synthetic baseline and verifies that the agent did not alter it.
   It also retains a per-session
   `jscout-requests.jsonl` containing every MCP request and exact tool
   arguments; the shared privacy-minimal telemetry remains the metrics input:

   ```bash
   node scripts/eval-run-replay.mjs \
     --tasks eval/tasks/ai-pipe-replay-pilot.json \
     --repository /path/to/source-clone \
     --runs-root /Users/cristian/git/jscout-replay-runs/ai-pipe \
     --jscout "$PWD/target/release/jscout" \
     --responses /tmp/replay-responses.jsonl \
     --telemetry /tmp/replay-telemetry.jsonl \
     --artifacts /tmp/replay-artifacts \
     --profiles grep,structural --trial 001
   ```

   Every arm independently exports the parent into a short-path workspace,
   installs dependencies, and builds. The short path avoids package-manager
   filename-limit failures in large monorepos. The runner then creates a
   synthetic one-commit repository, with no
   remote or upstream history, for exact patch capture. Agents may run local
   package-manager/build/test commands. Their prompt forbids the source clone,
   other filesystem paths, GitHub/remotes, and web search; package-registry
   access for declared dependencies is the only allowed network use.

   Response rows preserve input, cached input, derived non-cached input,
   output, reasoning-output, and total token counts alongside wall time. Raw
   Codex JSONL remains authoritative if accounting fields change in a later
   CLI version. They also record command counts, failures, total command-output
   bytes, and the largest single command output. `eval-report.mjs` keeps
   `profile/treatment` summaries separate instead of silently pooling
   skill-only and forced-search runs.

   PR-replay calibration supports these cumulative jscout profiles:

   - `structural`: parser-derived index and graph;
   - `checker`: structural plus `jscout enrich`;
   - `checker-embed`: checker plus local embeddings/reranking;
   - `checker-scout`: checker plus repository LLM reconnaissance;
   - `checker-scout-embed`: checker, reconnaissance, and product embeddings.

   Each jscout profile runs twice by default. `skill` installs the shipped
   skill without a prompt instruction; `forced` requires jscout exclusively
   for repository-wide code discovery while still permitting direct reads of
   localized files, edits, builds, and tests. Pass `--treatments skill` or
   `--treatments forced` to run only one. The two treatments reuse a
   copy-on-write clone of the same prepared database so embeddings and scout
   generations are not repeated or allowed to vary between them. Grep remains
   a single `control` treatment.

   ```bash
   node scripts/eval-run-replay.mjs \
     --tasks eval/tasks/next-calibration.json \
     --repository /path/to/next.js \
     --runs-root /runs/next-calibration \
     --jscout "$PWD/target/release/jscout" \
     --responses /runs/next-matrix/responses.jsonl \
     --telemetry /runs/next-matrix/telemetry.jsonl \
     --artifacts /runs/next-matrix/artifacts \
     --profiles structural,checker,checker-embed,checker-scout,checker-scout-embed \
     --treatments skill,forced --work-root /tmp/jr --trial calibration-001
   ```

   Embedding profiles require the local inference service. Repository-scout
   profiles use the configured pi-ai gateway and share the command-level
   `--scout-max-calls` budget (default 64) per prepared profile. The separate
   `--scout-max-subjects` inventory bound defaults to 512 so large repositories
   retain room for bounded subdivision without increasing model calls. Short-path
   workspaces are deleted after patch capture and grading by default; use
   `--keep-workspaces true` only for a debugging run.

The task unit is a **change arc**, not a single PR/commit: seed + every
semantically related follow-up until the feature stabilized (see
[`value-hypotheses-2026-08-09.md`](value-hypotheses-2026-08-09.md)). Arc
rules: membership is semantic (file overlap alone is hot-file
contamination); open arcs are excluded; **members before the model cutoff
are prehistory** — they already exist in the baseline workspace, so replay
scope is post-cutoff members only. Arc snapshots pass
`--members <sha,...>` so gold is restricted to the union of member files
(interleaved unrelated commits never leak in) and per-member patches land in
`gold/members/` as adjudication provenance.

**Primary corpus**: [`tasks/n8n-arc-pilot.json`](tasks/n8n-arc-pilot.json)
(3 arcs, incl. one 3-commit arc) and
[`tasks/twenty-arc-pilot.json`](tasks/twenty-arc-pilot.json) (2 arcs) —
human-authored code, post-cutoff, discovered by an Opus workflow
(mechanical mining → seed filtering → arc tracing → adversarial member
verification; the verifier caught one missed member and one unclosed arc).
All stories certify `weak` or better; snapshots under
`~/git/jscout-replay-runs/{n8n,twenty}/`.

**Harness-validation corpus only**:
[`tasks/ai-pipe-replay-pilot.json`](tasks/ai-pipe-replay-pilot.json) — ai-pipe
is mostly AI-generated code, so it validates the pipeline, not product value.
Layer 1 is deferred for the older pilots whose task files do not declare setup
commands. `tasks/next-calibration.json` exercises the complete prepared-base
and fail-to-pass path on a real monorepo.
