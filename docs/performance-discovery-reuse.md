# Invocation-local TSR discovery reuse — 2026-10-15

Next.js checker median wall time fell **64.0%**, from 206.979 s to 74.615 s;
the full index/checker workflow fell **61.3%**. Strict output/input parity passed.
No convincing small-repository/indexing gain is established; Next.js median RSS rose.
This follows the [profiling findings](performance-pgo-results.md) and
[measurement recipe](performance-pgo.md), without PGO in either comparison arm.

## What the initial evidence establishes

- PR #142's false rejection required different operation masks for a shared path,
  not just multiple projects. Cross-project/differing-mask publication lacked a regression.
- The Next.js capture contained 5,454 observed threads, including 5,451 named
  `ts-parser`, not that many checker instances or scans. Discovery/configuration
  accounted for 97.671 s / 162.922 s sampled CPU (59.95%), not a promised speedup.
- Of 235 projects, 149 configured workers rediscovered; 86 inferred workers used requested roots.
- Native CPU targeting, not PGO itself, limited the earlier artifact's portability.
  Normal releases still use `--release`; `matrix.native` only gates smoke tests.

## Implementation and safety boundaries

One invocation-local `ProjectPlan` supplies configured seeds instead of rediscovery.
Inferred construction and the TypeScript sidecar are unchanged. For this selection,
149 redundant passes disappear: 151 become two (initial planning and fresh final
validation), absent retries. Per-project input checks remain.

Before loading, workers fork and freshly verify observed config/inherited-config/
membership inputs. Forks have independent observations/deadlines and live reads for
new inputs, not discovery's metadata cache. Compiler sessions are not shared.

Seeds are released before fresh full-inventory discovery, which still catches
unselected ownership changes. Post-load/activation checks, selection, fields and
publication policy remain. There is no persistent/global cache. Changed adapter
identity intentionally changes compiler-dependent fingerprints.

## Automated verification

Passed: **879 unit tests, 33 integration tests, 36 tooling/documentation checks,
and all-target Clippy**. Regressions cover parallel workers/retries, seed isolation,
fresh-worker answer/manifest parity, later-project rejection, config/extends/glob
changes, live dependency reads and post-load invalidation, and fresh final ownership
discovery after edited/new configs affect unselected files.

Gate tests isolate Ctrl-C handlers in a subprocess; the original failed-suite log is retained.

## Controlled real-repository comparison

Baseline `de36ef40e08041008317ddb0fd0fce8ee1f044c7` and candidate
`57346978bc96832ef7e98ffa394110c10da86295` both use ordinary
`cargo build --locked --release --bin jscout`, without PGO/native CPU flags.
The older PGO binary predates PR #141 and is not the baseline; the manifest records identities.

Host: Apple M5 Pro, 18 logical CPUs, 64 GiB RAM, Darwin 25.6.0; Rust 1.97.1 /
LLVM 22.1.6. Corpora are ai-pipe `ea13166c59cfc52574e96959413f5c54be20e8c8`
and Next.js `0f36a7df88245ed6f1d035f169b69c5a2f4b1b13`, with installed dependencies.

After one excluded ai-pipe warmup per binary, three serial fresh-database repeats
per arm/corpus produced 12 databases and 24 timed commands. Alternating order puts
each arm first in three of six blocks. No task builds/profilers ran concurrently;
filesystem caches were warm and uncontrolled. Three samples are local evidence,
not a general speed guarantee or confidence interval.

Both use provider-free config, no dependency corpus indexing, and `checker.jobs=1`.
`enrich --full --timeout 600` recomputes default selection without file/occurrence
caps; `--all` is not used. Separate jobs=2 correctness runs are excluded from timing.

## Results

Wall seconds: median (minimum–maximum), three samples per cell. Workflow totals
sum index/checker within each repetition before taking the median/range.

| Workload | Baseline | Candidate |
| --- | ---: | ---: |
| ai-pipe index | 1.381 (1.187–1.407) | 1.281 (1.146–1.407) |
| ai-pipe checker | 3.116 (2.995–3.496) | 3.036 (2.868–3.297) |
| ai-pipe workflow | 4.523 (4.182–4.877) | 4.317 (4.013–4.704) |
| Next.js index | 9.149 (7.361–11.718) | 8.832 (7.941–9.600) |
| Next.js checker | 206.979 (202.185–210.445) | 74.615 (72.495–76.709) |
| Next.js workflow | 217.805 (211.334–218.697) | 84.215 (81.327–84.650) |

Median process CPU (user + system) and peak RSS, baseline → candidate:

| Workload | CPU seconds | Peak RSS MiB |
| --- | ---: | ---: |
| ai-pipe index | 1.190 → 1.202 | 104.3 → 105.3 |
| ai-pipe checker | 2.977 → 3.003 | 218.8 → 218.7 |
| Next.js index | 7.624 → 7.643 | 305.2 → 314.7 |
| Next.js checker | 199.612 → 70.949 | 714.6 → 856.7 |

ai-pipe checker wall time improves only 2.6%, while CPU increases slightly and
ranges overlap. Index CPU is effectively unchanged; noisy wall medians do not
establish an indexing gain. Next.js checker median RSS rises 142 MiB; ranges
overlap (baseline 658–855, candidate 736–901 MiB). These runs do not establish
the cause of that increase; no memory improvement is claimed.

## Correctness and post-change profile

All 12 timed databases, four separate jobs=2 databases, and the profiled database
pass integrity, complete publication and strict semantic/input-observation parity.
ai-pipe completes nine projects / 3,110 project-occurrences / 19 facts; Next.js
completes 235 / 18,952 / 1,516. Per-project input rows match exactly: 12,764 and
589,515 respectively. Index/graph/docs rows, four digests, facts and coverage match;
version-dependent compiler/cache fingerprints are recorded separately, not normalized
into equality. Jobs=2 outputs match jobs=1; this is correctness evidence, not scaling data.

The fresh candidate Next.js capture attributes 62.854 s CPU across 303 threads.
Discovery is 0.841 s plus 0.482 s on disjoint config-parser workers: **2.1%**.
`type_to_string_ex` is 10.744 s / 17.1% inclusive; `refresh_manifest` is 5.070 s /
8.1% inclusive. Self groups are TSR 44.8%, filesystem 19.3%, hashing 12.0%.
Inclusive shares overlap; 7.7% of self CPU is unsymbolized. Native-only symbolization
and first-delta exclusions follow the existing recipe. The old capture used a different
source/build and failed publication: it is **not** a matched before/after benchmark.
Use unprofiled timings for speedups; investigate type rendering/manifest work without dropping checks.

## Evidence record

Durable record: `/Users/cristian/Documents/Codex/jscout-discovery-reuse-2026-10-15`
(150 files, SHA-256 inventory in `retention.json`). Includes comparison/jobs2/profile
JSONs, raw metrics, capture/symbols, `candidate-next-checker-attribution-final.json`,
reproduction scripts, configs and build/test logs, including both initial failures.
