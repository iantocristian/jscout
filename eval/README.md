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
its source response before cloning it and assigns new session identifiers.
The three-seed, post-cutoff n8n/Twenty follow-up is recorded in
[`results/n8n-twenty-post-cutoff-2026-08-09.md`](results/n8n-twenty-post-cutoff-2026-08-09.md).
The pre-registered file-role re-run is recorded in
[`results/file-roles-rerun-2026-08-09.md`](results/file-roles-rerun-2026-08-09.md).
