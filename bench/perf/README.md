# Provider-free ai-pipe performance baseline

This harness measures local JScout work against one pinned ai-pipe revision. It
does not measure retrieval quality or remote model latency. The older
`bench/bench-aipipe.py` script is a provider-dependent retrieval evaluation and
remains a separate workload.

## Prerequisites

- Rust and Cargo versions accepted by the repository.
- Node.js 22 or newer.
- macOS or Linux, with Git and `tar`.
- The `sqlite3` CLI with JSON output and `.backup` support.
- A local ai-pipe checkout containing the pinned revision.
- For `--suite full`, that ai-pipe checkout's dependencies installed in
  `node_modules`, plus JScout checker dependencies under `checker/node_modules`.
- Enough temporary disk space for the staged corpus and transient SQLite
  backups (allow at least 1 GiB for a full run).

Build the binary immediately before a recorded run:

```sh
cargo build --release --locked
```

The report fingerprints the binary and the harness checkout separately. It
does not claim that an arbitrary supplied binary was built from that checkout;
the operator is responsible for that relationship.

## Running the harness

Run the full suite and write the machine-readable report outside the checkout:

```sh
node bench/perf/ai-pipe.mjs \
  --repo /path/to/ai-pipe \
  --revision ea13166c59cfc52574e96959413f5c54be20e8c8 \
  --binary target/release/jscout \
  --suite full \
  --checker-sidecar checker/src/main.mjs \
  --node-modules /path/to/ai-pipe/node_modules \
  --output /tmp/ai-pipe-performance.json
```

Use `--suite quick` while changing the harness. It omits enrichment and all
embedding measurements, as well as watch. `baseline` adds embedding; `full`
adds watch and enrichment. Every selected mode includes indexing and the local
fake-gateway card publication/reuse persistence smoke. It does not run reduced
versions of omitted workloads and present them as a full run.

`--samples N` overrides repeatable measured sample or pass counts; suites may
expand one pass across multiple fixed queries or cases. The full enrichment
population remains one observation. `--warmups N` applies to search,
neighborhood, and scouting preparation. Use `--keep-workdir` only for
diagnosis. The harness refuses to overwrite an output file unless
`--force` is supplied, and the output must be outside every source or
dependency tree supplied to the run.

Progress is written to stderr. Standard output and `--output` contain one JSON
report using the `jscout.performance.v1` schema.

## Measured scenarios

The full suite records:

- first and repeated fresh-database indexing;
- unchanged watch reconciliation triggered by a same-content mtime event;
- enrichment planning, one full local enrichment, and unchanged reuse;
- lexical-only search through both short-lived CLI processes and one
  persistent MCP process;
- fixed low-, medium-, high-, and extreme-degree neighborhood requests plus
  response-budget cases;
- provider-free scouting preparation;
- deterministic local embedding population and zero-missing synchronization;
- a local fake-gateway scouting publication and persistence-reuse smoke.

Queries, anchors, expected corpus counts, and the ai-pipe revision live in
`ai-pipe-fixture.mjs`. Corpus counts are fixture-drift checks, not performance
thresholds.

## Method

Every command receives an explicit configuration and database path. Search
disables vectors, reranking, memory attachment, and expansion unless a scenario
explicitly measures one of them. The embedding scenario uses
`mock-inference.mjs`, which binds an operating-system-selected loopback port and
returns deterministic, unit-normalized dense vectors. Its request
counts, input size, and handler time are included in the report.

The harness records raw samples and nearest-rank summaries. A measurement says
whether it is `wall_ms`, persistent-MCP `roundtrip_ms`, watch
`internal_refresh_ms`, JScout `bm25_ms`, mock-provider `provider_handler_ms`, or
serialized `result_bytes`; these values are not interchangeable. JScout
telemetry is integer-millisecond data and is not the primary metric for
sub-millisecond requests.

"Fresh database" means that no index database existed before the command. It
does not mean the operating-system filesystem cache was flushed. The first
post-snapshot index is therefore reported separately from later fresh-database
runs.

## Isolation and safety

The supplied ai-pipe checkout is used only as a Git object and, for enrichment,
as an external dependency source. The harness materializes the requested
revision in a uniquely named temporary directory and places all generated
configurations, databases, telemetry, and mock-service state there. It never
relies on or writes the source checkout's `.jscout.db`. Enrichment records the
checker sidecar, lockfile, Node binary, and `node_modules/.package-lock.json`
fingerprints; it does not claim to hash every file in the external dependency
tree.

Provider, reranker, inference, LLM, and gateway environment overrides are
removed from benchmark child processes. The embedding fixture binds only to a
dynamically selected loopback port. The fake scouting gateway uses JScout's
stdio protocol and opens no port. Asynchronous watch, MCP, and mock-service
children are tracked, terminated, and awaited during cleanup. Synchronous
benchmark commands have hard timeouts. A terminal interrupt normally reaches
the foreground process group, but a signal sent only to the harness PID is
observed after the active synchronous command returns.

Per-sample database files and their WAL/SHM families are deleted after their
counters and integrity checks are captured. The shared seed is retained only
until all selected suites finish. `--keep-workdir` disables that deletion for
diagnosis. Output-path checks resolve symlinks and existing parent directories
before rejecting paths inside any protected source or dependency tree.

Temporary databases, WAL/SHM files, telemetry, corpus snapshots, and profiler
samples must not be committed. Checked-in result JSON must not contain absolute
developer paths.

## Interpreting results

Results describe one machine under uncontrolled host load. They are suitable
for paired before/after runs on the same host, not absolute cross-machine
thresholds. The full enrichment population has one initial observation because
it takes several minutes. Mock embedding excludes model latency but still
includes local JSON serialization and HTTP transport; the mock handler counters
make that cost visible.

Do not run the full suite as a CI performance gate. Candidate schema/index
experiments and platform-specific profilers are diagnostic follow-ups, not
baseline scenarios.

## Semantic-memory scale and persistence-index experiment

`semantic-memory.mjs` is a separate, provider-free benchmark for the semantic
tables. It exists because ai-pipe has no natural semantic history and a tiny
hand-seeded database cannot support an index decision.

```sh
cargo build --release --locked
node bench/perf/semantic-memory.mjs \
  --repo /path/to/ai-pipe \
  --binary target/release/jscout \
  --scales 1000,5000,25000 \
  --samples 20 \
  --warmups 3 \
  --output /tmp/ai-pipe-semantic-memory.json
```

The runner first publishes one real card through the deterministic fake
gateway, then validates 31 more support anchors through the production
annotation path. It rotates four supports per generated row across those 32
distinct files and context hashes. It creates deterministic annotation
lineages at each requested scale, including 20% superseded history and a
40-edge exact-detail case. The anchor-scoped case selects one of the 32 support
templates, so its cardinality grows predictably without matching the entire
fixture.

The scale suite measures recent discovery, common/selective/missing lexical
queries, anchor and relation scopes, exact body reads, and full reads with
source evidence through one persistent MCP process. Semantic vectors are
disabled, so the result isolates local lexical ranking, freshness, SQLite, and
serialization work.

At the largest scale, the runner separately A/B tests the deferred
`semantic_artifacts(scout_run_id)` candidate. It captures SQLite query plans,
index size, batched run-ID lookup time, actual zero-model card reuse, and
byte-identical semantic-memory responses. This distinction matters: the index
serves scouting persistence/reuse joins; it is not an index over lexical
artifact contents and should not be credited with semantic-query speedups.

Use `--scales` to change corpus sizes. The fixture generator and query cases
live in `semantic-memory-fixture.mjs`; no benchmark database, WAL, staged
corpus, or absolute developer path belongs in a checked result.

The recorded 1k/5k/25k run and its index decision are in
[`bench/results/semantic-memory-ai-pipe-2026-08-22.md`](../results/semantic-memory-ai-pipe-2026-08-22.md).
