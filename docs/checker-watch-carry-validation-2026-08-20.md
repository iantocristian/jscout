# Watch checker carry-forward validation — 2026-08-20

## Result

Watch-only checker carry-forward produced the same canonical checker facts and
occurrence coverage as a carry-free `jscout enrich --full` pass on Next.js,
n8n, and AFFiNE. It reduced checker-phase wall time by 72.6–98.4% after a
single-file content-only edit.

The production monorepo path used in the earlier watcher incident was not
present on this machine. AFFiNE was used as the available large monorepo in its
place; this substitution is a limitation, not a claim about the unavailable
checkout.

| Corpus | Commit | Edited project | Carried projects | Carried occurrences | Carry wall time | Full wall time | Reduction | Canonical diff |
|---|---|---|---:|---:|---:|---:|---:|---|
| Next.js | `0f36a7df88` | `examples/api-routes-cors/tsconfig.json` | 148/149 | 17,570/17,571 | 8.245 s | 427.59 s | 98.1% | zero |
| n8n | `9d9e9bf97e` | `packages/@n8n/node-cli/src/template/templates/programmatic/ai/model-openai-compatible/template/tsconfig.json` | 18/19 | 621/624 | 12.385 s | 45.17 s | 72.6% | zero |
| AFFiNE | `0f349af8ee` | `packages/frontend/routes/tsconfig.json` | 102/103 | 49,661/49,663 | 10.173 s | 626.38 s | 98.4% | zero |

The canonical row counts were:

| Corpus | Facts in each arm | Coverage rows in each arm | Carry-only rows | Full-only rows |
|---|---:|---:|---:|---:|
| Next.js | 2,312 | 18,132 | 0 | 0 |
| n8n | 102 | 1,113 | 0 | 0 |
| AFFiNE | 11,024 | 49,663 | 0 | 0 |

## Method

Each corpus was cloned locally at the commit above. Its existing
`node_modules` installation was linked into the isolated clone, and every test
database lived outside the checkout. No existing repository database was read
or modified.

For each corpus:

1. Run a release build of `jscout index`, then `jscout enrich --full`.
2. Checkpoint SQLite and copy the database into independent carry and full
   arms.
3. Append a trailing comment after all calls in one selected source file. This
   changes the file hash without changing call spans, project membership, or
   checker semantics.
4. Start `watch --enrich` against the edited tree and the carry database. The
   startup full refresh exercises changed-snapshot carry-forward.
5. Run manual `jscout index`, then `jscout enrich --full`, against the edited
   tree and the independent full database.
6. Compare both active batches using bidirectional SQLite `EXCEPT` queries.

The fact comparison included source file/hash, exact call/receiver/property
spans, project, receiver type, target anchor/fingerprint, confidence, and
provenance. The coverage comparison included the same occurrence identity,
project, and resolved/unknown/failed status. Batch IDs, SQLite row IDs,
`member_call_id`, `source_file_id`, execution kind, and checker-input
fingerprints were excluded because they are operational identities or
provenance that necessarily differ between carried and rechecked execution.

The comparison was stricter than the planned “modulo re-enriched projects”
check: semantic rows from every project, including the project deliberately
rechecked in each arm, were equal.

## Performance defect found during measurement

The first Next.js carry attempt took 307.879 seconds despite carrying 148/149
projects. External checker inputs were being read and hashed once per
occurrence owner. Next.js had 61,714 project-input rows referring to 1,975
distinct absolute paths, so the same declarations were hashed repeatedly.

Carry validation now hashes each distinct external path once and evaluates all
project expectations against that cache. The same Next.js scenario then took
8.245 seconds. A regression test also verifies that changing an external input
still prevents the owning project from carrying.

## Scope and limitations

- The source edit was deliberately content-only. Config-chain, membership,
  target-content, shared-owner, external-input, rowid-rebinding, dirty-order,
  and daily-flush invalidation paths are covered by focused tests rather than
  by this three-corpus timing run.
- The temporary filesystem did not deliver the first live Next.js file event
  to the running watcher. The measurements therefore use watcher startup over
  an edited checkout. This exercises the same changed-snapshot carry planner,
  but not notification latency.
- Wall time is one development run per arm, not a statistical benchmark. The
  large effect and exact equality are the relevant merge checks.
- The feature remains an optimization. Manual `jscout index` clears checker
  state, `jscout enrich --full` recomputes it, and the watcher performs an
  independent daily carry-free drift flush.
