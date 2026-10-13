# Embedded TSR validation on Next.js

Initial implementation validated on 2026-10-11 using the main `/Users/cristian/git/next.js` checkout,
revision `0f36a7df88245ed6f1d035f169b69c5a2f4b1b13`. Its worktree remained clean.
The implementation plan is [embedded-tsr-plan.md](embedded-tsr-plan.md).
The final PR branch was also validated against current jscout main; see
[pull request validation](#pull-request-validation-against-current-main).

## Build and scope

- Release binary: `target/release/jscout`, 50,531,536 bytes, Darwin arm64.
- Embedded compiler: `ferrotype/tsr` revision
  `2009f6ec101358966902698f9ac2a7ff71ed57c3`; runtime identity
  `tsr-0.4.0+2009f6ec.978f528806de33bc`.
  The local compiler checkout advanced to `506820e71cbab92af5a05429df9b1ecdec1cbbe0`
  during this work; the tested dependency remains pinned to `2009f6ec`.
- Default backend: Next.js's installed TypeScript **6.0.2**, Node **24.20.0**.
- A fresh index covered **23,293 files**, 67,305 chunks and 95,900 references.
  Oxc rejected 11 files containing unsupported Flow or parser-error fixtures.
  Both checkers used copies of this same index, with snapshot
  `7c9873f93d91bd9a3b1ac6179f267cc0eccdda8b6529b3ea4ef7eba8f06eb4fe`.
- Both inventories found **324 projects** and **nine configuration problems**.
  The production comparison selected the actual
  `packages/next/tsconfig.build.json`, including its inherited configuration and
  installed dependencies. No simplified compiler configuration was substituted.
- The uncapped pass selected **12,077 calls** under `packages/next/src`.
  Another 127 calls in 37 files had no configured owner and were omitted by the
  existing default inferred-project policy. This is a production-package check,
  not a check of every example and test fixture in the repository.

## Results

| Uncapped production pass | TypeScript sidecar | Embedded TSR |
| --- | ---: | ---: |
| Selected and queried calls | 12,077 | 12,077 |
| Published graph facts | 1,462 | 1,462 |
| Unknown answers | 261 | 258 |
| Unmapped declarations | 10,437 | 10,416 |
| Elapsed time | 57.72 s | 40.00 s |
| OS-reported peak RSS | 1,543.5 MiB | 685.8 MiB |

All **1,462 occurrence/project/target/confidence tuples match exactly**.
The selected occurrence sets also match exactly. This establishes graph-target
parity for this scope, not complete TypeScript semantic parity.

Three calls are `unknown` in the sidecar and `resolved` in TSR, without producing
additional indexed graph targets:

- `server/dev/dev-validation-worker-pool.ts:199`, `current.end()`.
- `server/dev/next-dev-server.ts:873`, `staticPathsWorker.end()`.
- `server/dev/use-cache-probe-pool.ts:135`, `current.end()`.

Receiver text differs on 42 fact rows covering 40 calls. Inspection found quote
style, union order, template-literal union order, and object-property order
differences. The graph targets and confidence of those rows are unchanged.
Unmapped context counts are also different: sidecar/TSR are 7,029/7,017 library,
2,600/2,588 `@types`, 389/392 repository-unanchored, and 419/419 vendored.
These coverage differences remain visible; the adapter does not rewrite them
to force agreement with TypeScript 6.

An initial 512-call sample also matched all 29 published targets and all coverage
statuses. Repeating both the sample and full embedded pass reused the exact
completed batch. Full-pass reuse took 6.87 seconds with `PATH` pointing to a
nonexistent directory and an invalid checker-sidecar path.

Times are single local runs with warm filesystem caches, separate processes,
and fresh copied databases. The two uncapped runs were serial. Peak RSS comes
from macOS `wait4` resource usage, not the protocol's Node heap counters.
Embedded protocol memory counters remain zero/unmeasured. These measurements
are not a general performance guarantee.

## Reproduction and captures

The databases, stdout/stderr, per-run commands and metrics, and detailed SQL
comparisons are outside Next.js at:

`/Users/cristian/git/js-rag/target/embedded-tsr-next-2026-10-11/`

Relevant captures are `comparison-full.json`, `comparison-512.json`,
`tsr-full.{stdout,stderr,metrics.json}`, `node-full.{stdout,stderr,metrics.json}`,
and `tsr-full-reuse.{stdout,metrics.json}`. The local `run.py` records commands,
exit status, elapsed time and RSS; `compare.py` compares active published rows.
That directory is build output and is not committed.

From the jscout checkout, build and index once:

```sh
cargo build --locked --release
target/release/jscout index /Users/cristian/git/next.js --database /tmp/next-base.db
```

Use SQLite's backup API to copy the closed base database to two independent
databases. Run the same selection with each backend:

```sh
target/release/jscout enrich /Users/cristian/git/next.js \
  --database /tmp/next-node.db --file packages/next/src
target/release/jscout --experimental-tsr enrich /Users/cristian/git/next.js \
  --database /tmp/next-tsr.db --file packages/next/src
```

Add `--max-occurrences 512` for the sample or `--dry-run` for configuration-only
planning. Repeating an unchanged command verifies exact reuse; use `--full` to
force recomputation.

## Integration verification

- `cargo test --locked --all-targets --all-features`: **605 unit tests and two
  CLI integration tests passed**. Coverage includes shared TypeScript/TSR
  fixtures, source and declaration spans, JSDoc, libraries, overloads, unions,
  aliases, ownership, inferred scopes, changed inputs, session retirement,
  panic/deadline handling, Ctrl-C, native-only CLI execution, publication, reuse,
  and actual backend switching.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` passed.
- `cargo fmt --all -- --check` and `git diff --check` passed.
- Existing Node checker suite: **36 passed**.
- npm launcher suite: **two passed**, one Linux-specific test skipped on macOS.
- npm platform package assembly and dry-run packing include the compiler
  notices and licenses under `third-party/tsr/`.

Next.js testing caught and fixed deferred input discovery across query batches,
inherited `include` fields incorrectly identifying base configurations as
owners, and duplicate directory spellings in the input manifest. Failed runs
published no incomplete project facts. Their logs are retained alongside the
successful runs. This implementation did not modify compiler source or Next.js
source.

The flag remains experimental: TSR targets TypeScript 7, timeouts are
cooperative, and interrupted preparation restarts the selected project's
preparation before batch staging resumes.

## Pull request validation against current main

The PR implementation is based on jscout main
`9b5192537541e63b4c5b2f53ad0894c8fe38c672` (version 0.5.2). Integration preserves
its progress reporter and `--no-progress` behavior, documentation guide layout,
code-digest validation, and watcher configuration handling. The existing progress
HTTP fixture now explicitly uses blocking accepted sockets: repeated macOS runs
exposed intermittent `WouldBlock` reads before this test-only fix.

The final release binary is 51,919,600 bytes on Darwin arm64, with checker identity
`tsr-0.4.0+2009f6ec.ecb9adeacfae35c8`. The same clean Next.js revision was
indexed again into a fresh database. This index contains 24,309 files, including
1,016 Rust files supported by current main, 70,231 chunks and 95,900 references;
the same 11 parser-error fixtures were rejected. Its checker code digest is
`79596593f822a05e79580fa5ce4ebc8b908c347ee8feca8d67b0da9f6e1f2240`.

| Final PR production pass | TypeScript sidecar | Embedded TSR |
| --- | ---: | ---: |
| Selected and queried calls | 12,077 | 12,077 |
| Published graph facts | 1,462 | 1,462 |
| Elapsed time | 64.77 s | 43.94 s |
| OS-reported peak RSS | 1,447.3 MiB | 668.0 MiB |

All occurrence/project/target/confidence tuples and all 324 project inventory
records match. The same three coverage-status differences and 42 receiver-text
row differences described above remain. Both full TSR execution and exact reuse
ran with `PATH=/jscout-test-no-executables` and `--sidecar-path /missing-checker`.
Reuse completed in 7.06 seconds. Next.js remained clean. Timing caveats from the
initial comparison also apply here; the full checker passes ran serially.

Final checks passed: **826 Rust unit tests, 18 CLI integration tests**, Clippy
with warnings denied, formatting, staged diff whitespace validation, **36 Node
checker tests**, and **22 documentation/npm tests** (one Linux-only test skipped
on macOS). The CLI tests exercise `--no-progress` with embedded enrichment.
Native package assembly and npm dry-run packing include all five compiler
attribution/license files. The release packaging script passes shell syntax
validation and includes the plan, report, and compiler notices.

Final-run databases, stdout/stderr, commands, metrics and SQL comparisons are at
`/Users/cristian/git/js-rag/target/embedded-tsr-pr-next-2026-10-11/`.
The `run.py` and `compare.py` procedures from the initial experiment were reused.
All captures and databases remain build output outside the Next.js checkout.

## PR #130 watch and cache review

The review follow-up starts from main `569b486d51f8dd605108366c1ae359e25e1300b4`.
Regression tests first reproduced repository paths becoming external watch
targets, unselected outside events scheduling work, README creation invalidating
exact checker reuse, and an unreadable discovery directory aborting the command.

The fix separates cache observations from watcher coverage. Visible repository
sources/directories retain normal incremental classification; ignored or
unindexed compiler inputs and explicitly selected external inputs remain
covered. Ancestor directory probes are not broad event targets. Missing lookup
chains subscribe through an existing parent to their first absent child, avoiding
failed registrations while still detecting creation. Directory-listing events
request incremental refreshes. Directory fingerprints now hash subdirectories
and TS/JS/JSON names, including relevant symlinks; exact reads, missing paths and
symlink targets remain validated. The observation format changes to v2 so old
checker batches are recomputed once. Discovery skips unreadable or disappearing
subdirectories without swallowing cancellation or discovered config errors.

Live macOS watch validation with enrichment enabled on a three-source fixture:

- Editing `src/main.ts` produced `refresh_scope=incremental`, `indexed=1`,
  `unchanged=2`, and successful enrichment.
- Creating `src/notes.txt` and `src/.DS_Store` caused zero refreshes.
- Five parent/grandparent/sibling creation or deletion events caused zero
  refreshes.

On the same Next.js revision and 24,309-file index, the final release compiler
pass selected **12,077 calls** and published **1,462 facts**. Compared with the
previous TSR run, selection, target/confidence tuples, receiver text and coverage
statuses all match. The pass ran without Node or a usable sidecar. Adding four
unrelated MD, Finder-style metadata, PNG and BIN files to an observed source
directory retained **exact batch reuse**.

The Next.js notification probe used the active **13,504-row checker input
manifest** with enrichment disabled to isolate structural refresh behavior.
Creating/deleting an unrelated file beside the repository caused zero refreshes.
Appending a temporary comment to `packages/next/src/shared/lib/is-plain-object.ts`
produced `refresh_scope=incremental`, **1 indexed / 24,308 unchanged**, and the
same 11 parser-fixture rejections. There were **zero degraded watch coverage
warnings**. All temporary Next.js changes were restored and Git status is clean.
The three-source live probe separately verifies the enrichment phase.

Final validation passed: **829 Rust unit tests, 19 CLI integration tests**, Clippy
with warnings denied, formatting and whitespace checks, release compilation,
and **18 documentation tests**. Regression coverage retains new TS/JS/JSON and
directory invalidation, exact file reads, missing inputs, symlink retargeting,
ignored/unindexed inputs, explicit external inputs and Git controls. The Unix
permission test runs `checker doctor` and enrichment with an unreadable subtree
under a wildcard include (and skips when the user can bypass mode bits).

Commands, before-fix failures, test/build logs, comparisons and live-watch
captures are retained at
`/Users/cristian/git/js-rag/target/embedded-tsr-watch-review-2026-10-13/`.
The existing `run.py` and `compare.py` procedures were reused; `watch_probe.py`
and `next_probe.py` record the event tests. `next.db` is a test artifact whose
last structural snapshot includes the temporary comment; rebuild it before reuse.
