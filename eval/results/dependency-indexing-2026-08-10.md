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

Result: 64 tests passed; strict Clippy passed.

## n8n-scale first-party boundary check

The available n8n checkout has no `node_modules`, so it cannot validate actual
dependency-source discovery without first installing the repository. It was
still used to check that package ownership and the schema/projection changes
hold at monorepo scale using an isolated database under `/tmp`.

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

Twenty was not repeated because its checkout also lacks `node_modules`; it
would test the same first-party ownership path, not dependency indexing.

## Remaining external validation gap

No installed n8n/Twenty dependency tree was available in this workspace.
Therefore the real-package claims are limited to deterministic fixtures that
model npm and pnpm layouts. Before treating the feature as release-proven, run
one scoped package on an already-installed npm/pnpm monorepo and inspect:

1. every selected physical package instance and version;
2. the chosen `manifest-source` versus `runtime` basis;
3. truncated/skipped file and byte totals;
4. first-party-to-package boundary edges; and
5. default-hidden versus explicit-origin retrieval results.

Source-map reconstruction and Yarn PnP archive loading remain out of scope.
