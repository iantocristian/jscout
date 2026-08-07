# Agent A/B evaluation

This harness compares agent behavior with the same prompts, model, limits, and
repository snapshot under two MCP profiles:

- `baseline`: existing retrieval/definition/usage tools; no `neighborhood` and
  no search expansion;
- `structural`: the same tools plus `neighborhood` and opt-in search expansion.

The included fixture validates the protocol. It is not evidence that jscout
improves behavior on a large real repository.

## Codex runner

`scripts/eval-run-codex.mjs` runs a frozen repository task set through Codex in
both profiles, counterbalances profile order, captures structured responses,
and tags jscout telemetry by task/profile/session. It isolates user config and
disables unrelated tools by default.

First run a naturalistic adoption pass by omitting `--require-jscout`. Then run
the capability comparison with `--require-jscout true`, which gives both
profiles the same integration instruction to start with jscout but does not
tell the structural agent to use expansion or `neighborhood`.

```bash
node scripts/eval-run-codex.mjs \
  --tasks eval/tasks/ai-pipe-p0.json \
  --repository /path/to/frozen/repository \
  --jscout "$PWD/target/debug/jscout" \
  --responses /tmp/jscout-responses.jsonl \
  --telemetry /tmp/jscout-telemetry.jsonl \
  --artifacts /tmp/jscout-artifacts \
  --model gpt-5.6-terra \
  --reasoning low \
  --trial 001 \
  --require-jscout true
```

Use new output paths for every run; the runner refuses non-empty response,
telemetry, or artifact targets. Change `--trial` when repeating a task/profile
pair so its telemetry session remains unique. The target repository should be
a clean Git checkout because its own
agent instructions may require a status check even though the runner permits a
non-Git directory. Raw artifact logs can contain repository source; keep them
outside the product repository.

The representative P0 result is recorded in
[`results/ai-pipe-p0-2026-08-07.md`](results/ai-pipe-p0-2026-08-07.md).

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
