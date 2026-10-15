# Profiling and native PGO builds

This recipe targets Apple Silicon macOS. It builds jscout **and its linked TSR
compiler crates** with instrumentation, then uses the collected execution counts
to optimize them together. It does not optimize the external Node/TypeScript
checker or precompiled Rust standard library. Bundled C dependencies also inherit
PGO flags through `cc` when the host Clang supports the profile format; inspect
build warnings before claiming C optimization. Native CPU tuning is for the
training machine, not a portable release artifact.

`release-perf` inherits release optimization, adds ThinLTO, one codegen unit,
limited debug information with a packed dSYM, and keeps panic unwinding (the
embedded checker catches compiler panics). The helper uses frame pointers and
the same target/CPU/profile flags in all three stages. Compare its baseline
against its PGO build to isolate PGO; compare ordinary release separately if
measuring the combined effect of the other build settings.

## Build and train

Use the repository-pinned Rust toolchain and its `llvm-tools-preview` component.
No separate TSR executable or training build is needed. From the checkout:

```sh
run_dir="$(mktemp -d /private/tmp/jscout-pgo.XXXXXX)"
bash scripts/build-pgo.sh baseline "$run_dir"
bash scripts/build-pgo.sh instrument "$run_dir"
instrumented="$run_dir/build-instrument/aarch64-apple-darwin/release-perf/jscout"

# Use a fixed corpus checkout and explicit, recorded configuration.
corpus=/absolute/path/to/training-repository
config=/absolute/path/to/benchmark.toml
export LLVM_PROFILE_FILE="$run_dir/profiles/raw/training-%m-%p.profraw"
"$instrumented" --config "$config" --no-progress index "$corpus" \
  --database "$run_dir/training.db"
"$instrumented" --config "$config" --no-progress --experimental-tsr \
  enrich "$corpus" --database "$run_dir/training.db" --full --timeout 600
unset LLVM_PROFILE_FILE

bash scripts/build-pgo.sh optimize "$run_dir"
"$run_dir/build-optimize/aarch64-apple-darwin/release-perf/jscout" --version
```

Repeat training on a small and large representative repository, with a distinct
fresh database for each. Indexing and enrichment are separate commands;
indexing alone does not train the checker. `--full` prevents cached enrichment
from replacing the expensive work; it does not expand selection. Optionally add
`--all` for a separately recorded expanded-role/orphan workload, not as a synonym
for full recomputation.
The embedded deadline is cooperative, not a hard process limit. Record selected,
queried, completed, failed and unknown counts from the report and
`checker_project_runs`; a successful process exit alone is not proof that every
project completed. Do not accept timeout/error-only training. Abruptly killed
processes may not flush their profiles.

The helper refuses nonempty raw directories when instrumenting and changed
source/toolchain/CPU settings when optimizing. It retains `merged.profdata` and
refuses to overwrite it; start a new campaign for retraining. Keep source and
Cargo settings unchanged throughout. Raw profiles must come only from that
campaign's instrumented binary. The explicit Cargo target applies Rust flags to
target dependencies without instrumenting host build scripts. `llvm-profdata`
is resolved from the active Rust sysroot, not Apple's potentially different LLVM.
Each stage retains compiler output in `build-<stage>.log`, including failures.
The build identity includes the C/C++ compiler paths/versions and Apple SDK
overrides; ambient C/C++ compiler/flag overrides are rejected.
This is not a hermetic build: untracked, ancestor or user-global Cargo
configuration and installed toolchain/dependency files must remain unchanged.

## Measure and verify

- Sample an uninstrumented baseline with `samply` or macOS `sample`; the PGO
  training binary has overhead and is not a timing baseline. Retain the dSYM.
- Alternate baseline and PGO runs on identical inputs, with fresh external
  databases and no concurrent builds. Record wall/CPU time, peak RSS, cold/warm
  filesystem-cache context, binary hashes, toolchain, corpus revision and config.
- Compare canonical index/checker outputs and database integrity, not SQLite file
  bytes or timestamps. Check a held-out corpus or package, plus a lexical-search
  smoke: training-specific speedups must not hide correctness changes/regressions.
- Confirm training reached TSR with the matching tool's
  `llvm-profdata show --counts --function=tsr_checker <merged.profdata>`;
  check nonzero counters and inspect jscout functions as well. Do not combine
  `--covered` with the function filter: it selects covered functions outside that
  filter too. Counts describe executions, not elapsed time.
- Separate source-code improvements suggested by sampling from build-flag/PGO
  effects. Keep databases, raw profiles, dSYMs and absolute run paths outside Git;
  retain commands and summary evidence in the run record.

The existing provider-free [performance harness](../bench/perf/README.md) covers
other workloads, but its enrichment arm currently uses the TypeScript sidecar;
it is not an embedded-TSR benchmark without adaptation. With custom build paths,
pass `--sidecar-path` explicitly for any TypeScript-sidecar comparison.

Recipe checked against Rust 1.97.1 / LLVM 22.1.6 on macOS arm64. References:
[Rust PGO workflow](https://doc.rust-lang.org/rustc/profile-guided-optimization.html),
[Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html),
[LLVM profile merging](https://llvm.org/docs/CommandGuide/llvm-profdata.html#profdata-merge),
and [LLVM profile filenames](https://clang.llvm.org/docs/UsersManual.html#profiling-with-instrumentation).
Dependency-defined Cargo release settings are ignored; the root profile controls
TSR too. Do not use sparse profile merging: preserving zero-count functions helps
the optimizer distinguish cold code from missing coverage.

Native C caveat from the initial macOS run: `cc` 1.4.0 passed generation flags to
Apple Clang 21, so SQLite objects contained profiling counters. But Apple LLVM 21
could not read Rust LLVM 22's merged profile format. `cc` warned that inherited
`-fprofile-use` was unsupported and omitted it for bundled C compilation. Rust
and TSR still used the profile; SQLite execution counts in `merged.profdata`
alone do **not** establish C PGO. Optimizing C as well requires a compatible Clang
selected consistently before a fresh baseline/instrument/train/optimize campaign.
