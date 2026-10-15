# Per-project TSR parallelism — 2026-10-15

`checker.checkers_per_project` / `--checkers-per-project` sets a limit of 1–16
embedded TSR query checkers per active project, independently of `checker.jobs`.
Both default to one. The TypeScript sidecar ignores the new setting.

## Implementation and boundaries

The pinned compiler's public `CompilerCheckerPool` supplies independent semantic
caches over one immutable shared program. File affinity preserves per-file query
order; owned answers are assembled in request order after workers join. Parallel
preparation spreads its 128-query waves across files. The effective pool is capped
by the distinct selected files in its first wave, so one selected file remains serial.

An effective pool above one enables parallel program loading as well as queries.
TSR's loader has a separate `max(4, available CPUs)` worker bound per project;
`jobs * checkers_per_project` is not a process-wide thread or memory cap. These
measurements therefore compare the combined loading/query mode, not query-only
parallelism. There was no documented compiler requirement for the adapter's old
unconditional `single_threaded = true`; the one-checker default retains serial
loading and queries.

Returned compiler failures quarantine the entire affected file. Workers join
before the pool is rebuilt over the same program and healthy unfinished files
resume. Panics retire the generation and clear the request state. Deadlines remain
cooperative: a running compiler call must return before cancellation can finish.
Independent worker diagnostic context prevents concurrent errors from acquiring
another file's location. Existing input-manifest and publication checks remain.

Concurrency is invocation policy, not a semantic input. Changing either worker
count alone does not invalidate reusable results; changing the adapter does change
its compiler identity. Watch must be restarted after configuration changes.

## Automated verification

Passed: 890 unit tests, 34 integration tests (including actual TS7 execution),
36 tooling/documentation checks, all-target Clippy with warnings denied, and
formatting checks. Regressions prove actual query overlap and exact answers,
input observations and fingerprints at counts 1/2/4. They cover late-file
quarantine, independent diagnostics, pool rebuilding after returned compiler
errors, panic cleanup, timeouts while workers are active, joined callbacks before
return, and subsequent requests. The end-to-end partial-publication/resume test
runs with both one and two checkers, including invalidation of previously completed facts.

## Measurement method

The candidate is ordinary release commit
`e76a70529fd33d1f8379512c1135ac3efc7f36d5`, built with
`cargo build --locked --release --bin jscout`, without PGO or native CPU flags.
The old-main reference binary was built from
`57346978bc96832ef7e98ffa394110c10da86295`; its product sources are identical to
merged main `b4996fbaf070ab07542281c47109da220420abd8`. Binary hashes and exact
commands are in the evidence manifest.

Host: Apple M5 Pro, 18 logical CPUs, 64 GiB RAM, Darwin 25.6.0; Rust 1.97.1 /
LLVM 22.1.6. Corpora: ai-pipe `ea13166c59cfc52574e96959413f5c54be20e8c8`
and Next.js `0f36a7df88245ed6f1d035f169b69c5a2f4b1b13`, with installed dependencies.

The [existing measurement recipe](performance-pgo.md) is reused. Every workflow
starts from a fresh external database and performs a full index with `--no-deps`,
then provider-free embedded `enrich --full --timeout 600`. ai-pipe checks all
default-selected projects. Next.js scaling uses `--file packages/next/src/server`,
which selects exactly one project, `packages/next/tsconfig.build.json`, and 5,029
occurrences. No occurrence cap or `--all` is used.

All timing-table arms use the same candidate binary; old main is a correctness
reference. Counts 1/2/4 each get three serial, counterbalanced repetitions with
`jobs=1`; one earlier ai-pipe warmup is excluded. No task builds or profilers overlap timed
commands. Filesystem caches are warm and uncontrolled. Three samples establish
local evidence, not a general speed guarantee. Separate full-Next correctness
runs cover the omitted/default setting, four checkers, and two projects with two
checkers each.

## Results

Wall seconds: median (minimum–maximum), three samples per cell. Workflow totals
sum index/checker within each repetition before computing the median and range.

| Workload | 1 checker | 2 checkers | 4 checkers |
| --- | ---: | ---: | ---: |
| ai-pipe index | 1.258 (1.217–1.362) | 1.266 (1.241–1.451) | 1.251 (1.224–1.392) |
| ai-pipe checker | 3.091 (3.077–3.302) | 2.658 (2.650–2.881) | 2.478 (2.431–2.641) |
| ai-pipe workflow | 4.349 (4.294–4.664) | 3.924 (3.891–4.332) | 3.702 (3.682–4.033) |
| Next.js index | 9.179 (8.267–10.469) | 8.964 (8.537–9.568) | 9.144 (8.613–9.720) |
| Next.js single-project checker | 20.571 (17.322–21.530) | 16.844 (16.188–16.986) | 16.518 (16.224–18.072) |
| Next.js workflow | 29.750 (25.589–31.999) | 25.808 (24.726–26.554) | 25.368 (25.132–27.792) |

Median checker CPU seconds (user + system) and process peak RSS:

| Workload | 1 checker | 2 checkers | 4 checkers |
| --- | ---: | ---: | ---: |
| ai-pipe CPU seconds | 3.057 | 3.517 | 3.552 |
| ai-pipe peak RSS MiB | 220.5 | 227.2 | 243.4 |
| Next.js CPU seconds | 18.977 | 17.662 | 17.837 |
| Next.js peak RSS MiB | 613.2 | 651.9 | 679.2 |

Two/four checkers reduce Next.js checker median wall time by **18.1% / 19.7%**
and ai-pipe by **14.0% / 19.8%**. Four-checker index-plus-selected-checker workflow
reductions are 14.7% and 14.9%, respectively. Next.js includes full indexing but
only the selected single-project checker workload, not all 235 projects.
Indexing is unchanged by this feature; these index timing differences do not
establish an indexing improvement.

The Next.js two/four-checker ranges overlap substantially, and its one-checker
range is broad. The extra two checkers do not establish a meaningful improvement
over two on this workload. Two is a reasonable starting setting here, while one
remains the default. Peak memory rises, and ai-pipe spends more total CPU despite
finishing sooner. Discovery, validation and publication remain outside the query
pool. Wave barriers and thread startup can also limit scaling; no claim of linear
scaling or universal optimal worker count is made.

## Correctness and evidence

All 18 scaling databases pass integrity, complete publication and exact canonical
output/input-observation parity. ai-pipe has nine projects / 3,110 occurrences /
19 facts and 12,764 input rows per run; the Next.js single-project selection has
5,029 / 964 and 13,050 input rows. These comparisons cover index/graph/docs rows,
four digests, checker facts and coverage, and exact per-project filesystem
operation/hash observations. Compiler-version-dependent cache identities are
recorded separately, not silently normalized into equality.

All three full-Next modes also match the old-main reference exactly: 235 completed
projects / 18,952 occurrences / 1,516 facts, including all 589,515 per-project input
rows. These are single, unrepeated correctness runs; their timings are observations,
not a scaling benchmark:

| Full Next.js checker mode | Wall seconds | CPU seconds | Peak RSS MiB |
| --- | ---: | ---: | ---: |
| Old-main reference, jobs=1 | 77.102 | 71.590 | 896.6 |
| New default, jobs=1 / checkers=1 | 74.151 | 69.627 | 848.1 |
| jobs=1 / checkers=4 | 61.256 | 68.988 | 1,012.7 |
| jobs=2 / checkers=2 | 37.719 | 71.876 | 1,237.1 |

The raw logs retain an initial read-only SQLite verification error after the first
completed baseline. Completed, checkpointed databases are now inspected with
immutable read-only connections, explicitly rejecting remaining WAL data. No
workload rerun or relaxed parity comparison was needed.

Durable record:
`/Users/cristian/Documents/Codex/jscout-project-parallelism-2026-10-15`.
It contains exact commands/configs, binary identities, timing/CPU/RSS records,
strict parity reports, and build/test logs, including the initial fixed test and
lint failures. `retention.json` records per-file SHA-256 hashes. Databases, binaries
and build caches remain outside this compact record.
