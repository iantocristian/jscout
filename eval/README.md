# Agent A/B evaluation

This harness compares agent behavior with the same prompts, model, limits, and
repository snapshot under two MCP profiles:

- `baseline`: existing retrieval/definition/usage tools; no `neighborhood` and
  no search expansion;
- `structural`: the same tools plus `neighborhood` and opt-in search expansion.

The included fixture validates the protocol. It is not evidence that jscout
improves behavior on a large real repository.

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

Repeat with `--profile structural`. Do not tell the structural agent that it
must use graph tools; tool selection is one of the measured outcomes.

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

After the fixture protocol is stable, add a fingerprinted task set for a
representative large repository. That run—not this synthetic fixture—is the P0
product gate.
