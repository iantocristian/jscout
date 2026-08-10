# Scoped dependency indexing implementation check — 2026-08-10

This is an engineering validation record, not an agent-value result.

## What was exercised

The automated fixture covers:

- exact-package selection without blanket `node_modules` traversal;
- package name/version/canonical-root identity;
- deterministic runtime-tree planning and hard file/byte limits;
- explicit manifest-source preference;
- nested `node_modules` exclusion and minified/bundle filtering;
- pnpm-style realpath deduplication, multiple installed versions, and
  workspace-link classification;
- incremental reuse on an unchanged second pass;
- removal when a later index run omits `--deps`;
- canonical module edges into indexed dependency files;
- versioned package boundary hubs;
- dependency exclusion before BM25 candidate ranking and graph budgets;
- explicit dependency search/definition/traversal opt-in;
- origin-aware shorthand-anchor resolution; and
- explicit Yarn PnP rejection.

Commands:

```text
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Result after review fixes: 70 tests passed; strict Clippy passed.

Regression coverage includes dependency-corpus FTS cleanup, fail-closed
snapshot invalidation when selected-package discovery fails, literal workspace
path ownership, scoped-package discovery/resolution, forced entry precedence
under package limits, and declaration-ordered export conditions.

## n8n-scale first-party boundary check

```text
jscout index ../n8n --database /tmp/jscout-dep-validation-n8n-20260810.db
```

Observed initial pass:

```text
19,198 files indexed; 0 failed
92,215 chunks
404,999 references
59.34 s wall time
```

Ownership after the pass:

```text
files: 49 repository, 19,149 workspace
package instances: 77 workspace
module edges with package-instance identity: 41,538
```

An unchanged second pass reported 19,198 unchanged files, zero indexed/failed
files, and completed in 23.07 s. Most of that time remains the deliberately
full structural projection rebuild (14.77 s), not file extraction.

## Real installed-package checks

### n8n / pnpm 10.32.1

After installing n8n's modules with pnpm, the existing isolated index was
updated with one selected package:

```text
jscout index ../n8n \
  --database /tmp/jscout-dep-validation-n8n-20260810.db \
  --deps zod
```

Observed:

```text
zod@3.25.76
canonical root: /Users/cristian/git/n8n/node_modules/.pnpm/zod@3.25.76/node_modules/zod
locator: node_modules/.pnpm/zod@3.25.76/node_modules/zod
source basis/status: runtime / complete
417 dependency files, 3,022,930 bytes
0 skipped files, 0 extraction failures
4,723 chunks, 20,704 references added
1,317 module edges resolved into dependency files
1 versioned package-instance graph hub
```

This confirms that the logical workspace/package symlinks resolve to the pnpm
store's physical package instance without duplicate file identities.

For the dependency-only term `datetimeRegexWithLeapYearValidation`, an MCP
`semantic_search` with omitted origins returned zero hits. The same call with
`origins: ["dependency"]` returned two dependency hits.

### Twenty / Yarn 4.13.0 with node_modules

Twenty's Yarn installation uses a regular `node_modules` tree rather than PnP.
A fresh isolated index was built with the same selected package:

```text
jscout index ../twenty \
  --database /tmp/jscout-dep-validation-twenty-20260810.db \
  --deps zod
```

Observed:

```text
zod@4.1.11
canonical root: /Users/cristian/git/twenty/node_modules/zod
locator: node_modules/zod
source basis/status: runtime / complete
450 dependency files, 3,346,331 bytes
0 skipped files, 0 extraction failures
970 module edges resolved into dependency files
1 versioned package-instance graph hub
```

The complete Twenty pass indexed 23,185 files and produced 80,305 chunks and
321,480 references in 33.96 seconds. File ownership was 1,711 repository,
21,024 workspace, and 450 dependency.

For the dependency-only term `ZodMiniDiscriminatedUnion`, default MCP search
returned zero hits; `origins: ["dependency"]` returned three dependency hits.

## Remaining scope boundaries

The real checks cover one pnpm physical-store instance and one Yarn
`node_modules` installation. Multiple-version discovery is still fixture-tested
rather than observed in these two selected-package runs. Source-map
reconstruction and Yarn PnP archive loading remain out of scope.
