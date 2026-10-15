# Full index / embedded-checker profiling and native PGO — 2026-10-15

Full indexing and embedded-checker profiling, the corrected native PGO build,
and output-parity checks are complete. On Next.js, PGO reduced median checker
time by 9.9% versus ordinary release and 5.0% versus the same native build without
PGO. Indexing did not improve consistently. The largest remaining opportunity
is checker discovery/configuration work, about 60% of sampled CPU. See the
[build recipe](performance-pgo.md).

## Scope and method

- Product baseline: `2280c2ffc4886c170d27a8e75ce3553269e11e81`. Final comparison
  arms all use `1906e5ca3a3f1117817e88693df33cc73e937bef`, including the
  cross-project observation-mask fix (`4ed1d57`) below.
- Host: macOS arm64, Darwin 25.6.0, Apple M5 Pro, 18 logical CPUs, 64 GiB RAM.
  Rust 1.97.1 uses LLVM 22.1.6; its native CPU selection reports `apple-m4`.
- Small corpus: ai-pipe `ea13166c59cfc52574e96959413f5c54be20e8c8`, materialized
  with `git archive` and a link to installed dependencies. Initial index:
  856 files, 13,736 chunks, 28,527 refs, zero rejected files.
- Large corpus: clean Next.js `0f36a7df88245ed6f1d035f169b69c5a2f4b1b13`, with
  installed dependencies. Initial index: 24,309 files, 70,231 chunks, 95,900
  refs, 11 rejected files. Rejections remain part of the recorded workload.
- Both use explicit configuration, external fresh databases, no dependency
  corpus indexing, no embeddings/providers, no vector/reranker, embedded TSR,
  and one checker job. `enrich --full --timeout 600` recomputes the complete
  default selection without occurrence/file caps; `--all` was not silently added.
  The compiler may still read installed dependencies for resolution.
- Profiling uses samply 0.13.1 at 500 Hz with local presymbolication and retained
  debug symbols. It samples uninstrumented builds, not the PGO training binary.
  Initial captures ran during development/build activity: their wall times are
  diagnostic observations, **not** benchmark comparisons.

The [summary utility](../bench/perf/samply-summary.mjs) attributes recorded
`threadCPUDelta` to sampled native stacks. It excludes the first delta per
thread (which can include pre-capture execution) and does not count weighted idle
samples as CPU. It ignores samply sidecar inline frames because that format
caches them by native function rather than sampled instruction. Self metrics
are exclusive leaf attribution; inclusive metrics overlap. Groups are name
heuristics: "other" includes unclassified library work. These are estimated CPU
attribution, not exact function timers or wall-clock shares.

## Where indexing spends time

The native-baseline Next.js index capture contains 6.873 seconds of attributed
CPU across three threads. Ordinary-release attribution agrees on the main costs;
its 7.467-second sample is not a controlled speed comparison.

| Native-baseline index observation | Attributed CPU | Share | Metric |
| --- | ---: | ---: | --- |
| Filesystem calls | 2.456 s | 35.73% | Self, grouped |
| `sqlite3_step` and callees | 3.361 s | 48.91% | Inclusive |
| `insert_file` and callees | 1.598 s | 23.25% | Inclusive |
| Structural projection rebuild | 1.674 s | 24.36% | Inclusive |
| `extract_file` and callees | 0.681 s | 9.91% | Inclusive |
| Hashing | 0.110 s | 1.60% | Self, grouped |

Do not add the rows: SQLite includes filesystem work, and insertion/projection
include SQLite. `pwrite` alone accounts for 10.79% self attribution. The evidence
points first to database writes, index/FTS maintenance and structural projection,
not to replacing the parser or hash function. A follow-up should identify the
expensive SQL statements and evaluate reduced repeated work or more efficient
bulk publication while preserving transaction/FTS/projection behavior.

Within the roughly 741 ms of `pwrite` attribution, about 474 ms appears under
SQLite `pagerStress` and 191 ms under WAL checkpointing. A concrete first
experiment is a bounded larger writer page cache, measuring spill/write counts,
RSS and canonical output parity. `configure_writer_connection` currently sets
WAL/NORMAL/foreign keys but no cache policy. This is an experiment to measure,
not a recommendation to weaken durability or an already demonstrated speedup.

## Where checker enrichment spends time

The pre-fix full Next.js capture contains 162.922 seconds of attributed CPU
across 5,454 observed threads; 1.614 seconds of first-thread deltas are excluded.
Every selected project completed, but final publication failed. This diagnoses
preparation/checking, not a successfully published end-to-end run or the cost of
successful post-publication projection.

| Full checker observation | Attributed CPU | Share | Metric |
| --- | ---: | ---: | --- |
| Filesystem calls | 83.600 s | 51.31% | Self, grouped |
| TSR native functions | 36.624 s | 22.48% | Self, grouped |
| Hashing | 8.305 s | 5.10% | Self, grouped |
| SQLite | 1.123 s | 0.69% | Self, grouped |
| Main-thread project discovery | 62.687 s | 38.48% | Inclusive |
| Config parsing/glob work on separate parser workers | 34.984 s | 21.47% | Inclusive |
| `type_to_string_ex` and callees | 17.022 s | 10.45% | Inclusive |
| `refresh_manifest` and callees | 5.299 s | 3.25% | Inclusive |

The two discovery rows execute on disjoint threads: together they attribute
97.671 seconds, about 59.95% of captured CPU, to discovery/configuration work.
This is a ceiling for eliminating that work, not an attainable speedup claim.
The largest syscall leaves are `__open_nocancel` (22.71%), directory enumeration
(12.61%), and `lstat` (7.83%).

Priorities for follow-up, not changes already implemented:

1. Reuse validated project/configuration discovery within an enrichment run,
   instead of repeatedly enumerating/parsing the repository. Preserve input
   observation/invalidation guarantees; do not introduce a stale global cache.
2. Evaluate avoiding repeated type rendering or caching it within one checker
   session. Do not omit output fields without agreeing on a functionality change.
3. Measure repeated manifest observation/hashing after discovery improvements.
   Hashing is visible, but it is not the dominant cost in this capture.
4. Investigate parser-worker reuse separately. The thread count indicates churn,
   but does not establish that scheduling dominates elapsed time.

## Publication bug found during the full workload

All nine ai-pipe projects completed 3,110 selected occurrences; all 235 Next.js
projects (149 configured, 86 inferred) completed 18,952 occurrences, staging
1,516 facts. Both then failed with "checker projects disagreed on one input hash".

The embedded host signature is `tsr-observed:v2:<mask>:<digest>`. Its mask records
operations actually performed: read=1, stat=2, directory entries=4, realpath=8.
Different projects can legitimately observe different operation sets on one path.
Activation incorrectly compared their complete signatures as if each were a
plain file-content hash. ai-pipe had 46 such conflicting paths and **zero**
disagreements between signatures with the same mask.

For example, absent `server/package.json` was observed with stat only (mask 2)
and read+stat (mask 3). Both reported absence, but the encodings necessarily hash
differently. The corpus root had masks 2/14 and React's JSX declarations 9/11.
This was not filesystem churn from the benchmark or concurrent builds.

The approved fix scopes deduplication by `(kind, path, observation mask)` and
freshly verifies every retained signature. Same-mask disagreements and ordinary
TypeScript content-hash disagreements still fail. Regression cases cover valid
file/directory/absent-path mask combinations and reuse, same-mask conflicts, and
real changes observed through overlapping masks while retaining staged work and
the prior active publication. Performance training/comparison restarted from the
corrected source; failed or reduced-scope runs are not final evidence.
The corrected binary successfully resumed and published the retained complete
staging batches for both repositories, including all 235 Next.js projects.
Those resume runs validate publication/reuse, not full-recompute performance.

## PGO coverage and build comparison

The three final arms are ordinary release, native `release-perf` without PGO,
and the same native profile with PGO. The latter two share ThinLTO, one codegen
unit, frame pointers and debug settings. This separates PGO from other tuning.
Training covered the full ai-pipe and Next.js indexes, then all 235 default-selected
Next.js checker projects: 18,952 completed occurrences and 1,516 published facts.
ai-pipe's checker workload was held out of training; its indexing was not.
Three raw profiles produced a 17.1 MiB merged profile containing 43,920 functions.
The filtered TSR report contains 3,004 entries with nonzero counters, confirming
that training reached the linked compiler; these counts are not timing evidence.

Measurements were serial, with three fresh-database index+full-checker
repetitions per arm and corpus: 36 timed commands and 18 completed databases.
Each binary first ran one recorded, unmeasured ai-pipe index+checker warmup.
Arm order was counterbalanced as native/PGO/ordinary, PGO/ordinary/native,
ordinary/native/PGO. There were no concurrent task builds or profilers. The host
and warm filesystem caches remained uncontrolled; fresh databases do not imply
cold filesystem caches. With three samples, these are local observations, not
confidence intervals or a general speedup guarantee.

### Wall time

Seconds, median (minimum–maximum), three samples per cell:

| Workload | Ordinary release | Native, no PGO | Native PGO |
| --- | ---: | ---: | ---: |
| ai-pipe index | 1.316 (1.172–1.355) | 1.235 (1.173–1.370) | 1.325 (1.194–1.333) |
| ai-pipe checker | 3.146 (2.873–3.212) | 2.784 (2.717–3.000) | 2.652 (2.475–2.673) |
| Next.js index | 7.435 (6.788–7.618) | 7.869 (6.841–8.335) | 7.543 (6.518–7.544) |
| Next.js checker | 177.168 (170.907–183.125) | 167.913 (166.340–170.189) | 159.596 (158.821–164.723) |

Checker median reductions are 15.7% versus ordinary release / 4.8% versus native
without PGO for ai-pipe, and 9.9% / 5.0% for Next.js. The small corpus provides
held-out checker evidence, not independent held-out indexing evidence.

There is no consistent indexing improvement: PGO's median is 0.6% slower than
ordinary release for ai-pipe and 1.5% slower for Next.js. Against native without
PGO, it is 7.3% slower for ai-pipe and 4.1% faster for Next.js. Ranges overlap;
do not present the checker result as an across-the-board indexing speedup.

Full-workflow totals below sum index and checker within each repetition before
taking the median/range; they are **not** sums of the separate workload medians.

| Index + checker | Ordinary release | Native, no PGO | Native PGO |
| --- | ---: | ---: | ---: |
| ai-pipe | 4.501 (4.045–4.529) | 4.019 (3.890–4.370) | 3.985 (3.670–3.998) |
| Next.js | 183.956 (178.525–190.559) | 174.754 (174.209–178.524) | 166.365 (166.114–172.266) |

That is an 11.5% / 0.9% median full-workflow reduction for ai-pipe and 9.6% / 4.8%
for Next.js, relative to ordinary release / native without PGO respectively.

### CPU and memory

Each cell gives median process CPU seconds (user + system) / median peak RSS in
MiB. These are per-command resource measurements, unlike sampled attribution
above; they include successful publication.

| Workload | Ordinary release | Native, no PGO | Native PGO |
| --- | ---: | ---: | ---: |
| ai-pipe index | 1.151 / 103.9 | 1.118 / 101.8 | 1.152 / 101.1 |
| ai-pipe checker | 2.887 / 220.3 | 2.722 / 217.2 | 2.446 / 215.5 |
| Next.js index | 6.663 / 310.5 | 6.886 / 303.1 | 6.583 / 304.0 |
| Next.js checker | 172.324 / 897.5 | 167.443 / 906.1 | 158.181 / 903.4 |

The checker CPU reductions support the wall-time direction. Next.js checker
memory does not improve: PGO and native medians are around 903–906 MiB versus
897 MiB ordinary release; observed ranges overlap (PGO 895–909 MiB, ordinary
827–906 MiB). No memory improvement is claimed.

### Correctness checks

All 18 timed databases pass `PRAGMA integrity_check`. All projects completed,
with one active published batch per database: ai-pipe has nine projects / 3,110
completed project-occurrences / 19 facts; Next.js has 235 / 18,952 / 1,516.

Across all nine databases per corpus, canonical outputs match for files, chunks,
symbols, graph nodes, resolved edges, module edges, documentation chunks and
documentation inventory, plus the four plane/snapshot digests, active checker
facts, occurrence coverage and project completion records. This compares
canonical selected fields, not database bytes or incidental row IDs/timestamps.

Six unseen lexical queries (three per corpus) also returned nonempty,
byte-identical JSON across the three binaries using their first-repetition
databases. They ran with `--lexical-only --no-memory --no-expand -k 20`; embeddings,
providers, watch mode and non-default checker concurrency are outside this test.

### Build identity and limitations

The final build-input identity is
`91b278b316631ae314d79fb425e64759faf2604d8a9da3d620e2d03804b6b443`.
The optimized binary is retained at
`build-optimize/aarch64-apple-darwin/release-perf/jscout` under the final campaign
directory, with SHA-256
`0981b59de7b229e8facaeca19ef1a50f0bd426eb6f846fbfbeae8008698e9f09`.
This native-CPU build is for the training host, not a portable release artifact.

Rust jscout and embedded TSR receive the profile-use flags. In this
campaign, `cc` 1.4.0 also instrumented SQLite/C code, but Apple LLVM 21 could not
read the LLVM 22 merged profile. `cc` emitted warnings and omitted C profile use.
SQLite counters in the profile are not evidence that final C code was optimized
with PGO. No Clang install or C-toolchain change is part of this campaign.
The final Rust optimized build reported 103 missing-function-profile warnings,
with no reported profile hash-mismatch/corrupt-profile warnings. Training does
not cover every command or function, and it does not rebuild the precompiled
Rust standard library. The next source-level performance work should target
validated discovery/configuration reuse, not assume another build flag will
remove the dominant filesystem work.

## Evidence record

Initial diagnostic campaign: `jscout-pgo-20261015.1tdlg1`; corrected resume checks:
`jscout-pgo-fixed-20261015.w5M4db`; final comparison campaign:
`jscout-pgo-final-20261015.Zb0jgD`. These are local temporary artifact
directories, not committed binaries or databases. The initial record contains
`manifest.json`, `profile.toml`, `*.metrics.json`, command stdout/stderr,
`next-index.json.gz`, `next-checker.json.gz`, `standard-next-index.json.gz`, their
symbol sidecars and `*-summary.json` files. Final build stages retain
`build-<stage>.log`, binary hashes, raw training profiles and `merged.profdata`.
Dependency build-script `build/*/output` files retain the rejected C profile-use
diagnostics, which Cargo's console log does not necessarily repeat.
The final record also contains the 36 `bench-*.metrics.json` files and command
stdout/stderr, `benchmark.py`, `verify.py`, `comparison.json`, `search-smoke.py`
and `search-smoke.json`. Run-specific command paths remain in those local records.
