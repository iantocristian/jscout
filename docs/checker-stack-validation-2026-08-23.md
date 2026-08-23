> Generated validation record for the #76–#80 checker stack. Binary under test: the stack at `6a93b0d`, before the projection-scan fix `3fb30ef` merged with #80; the 374.62 s n8n restricted-enrichment time below is pre-fix (post-fix: 62.36 s cold / 11.03 s reuse with identical facts, PLAN.md item 5). Scratch paths (`S=/private/tmp/jscout-stack-rw-2026-08-22`, `S/logs`, `S/sql`, `S/db`) were ephemeral and are not committed; the numbers quoted in PLAN.md come from this record.

# jscout stack validation (PRs #76-#80) on real repositories

Date: 2026-08-23 (local). Machine: Apple M5 Pro, 18 cores, 64 GB RAM, macOS Darwin 25.6.0.
Node v24.15.0 (/Users/cristian/.nvm/versions/node/v24.15.0/bin/node), sqlite3 /usr/bin/sqlite3, cargo 1.97.1.
Scratch root S=/private/tmp/jscout-stack-rw-2026-08-22. Logs in S/logs, SQL outputs in S/sql, databases in S/db.
No LLM/billed calls were made (no `scout`, no `--embed`).

## Setup

| arm | commit | worktree | binary sha256 | version |
|---|---|---|---|---|
| stack (PRs #76-#80) | 6a93b0d610273039604b806b51502cc83001e2c9 | S/stack | e6031d52b51f66ea342cdac9ad253c376d0a94df802b0799ba837702d295ebfe | jscout 0.4.0 (schema 28, projection 12, checker protocol 4) |
| prestack (PR #74 merge) | 1e9acac80281649606bcac499854ae339a609052 | S/prestack | 5146ba511f91080f9b4f4e19f0308673bb15346a2fa5c55993b393313ca11d3d | jscout 0.4.0 (schema 26, projection 11, checker protocol 2) |

Builds: `cargo build --release` in each worktree (stack 1m01s, prestack 58s; logs S/logs/build-*.log).
Checker node_modules symlinked from /Users/cristian/git/js-rag/checker/node_modules in both worktrees.

Sidecar resolution: no `--sidecar-path` needed. Each binary auto-resolves `<worktree>/checker/src/main.mjs`
(target/release -> repository root walk in src/checker/mod.rs::resolve_sidecar). Confirmed with `jscout checker doctor`:

- stack: `checker sidecar: S/stack/checker/src/main.mjs`, protocol 4 (S/logs/doctor-stack-aipipe.log)
- prestack: `checker sidecar: S/prestack/checker/src/main.mjs`, protocol 2 (S/logs/doctor-prestack-aipipe.log)
- Both report `TypeScript: 6.0.3 (repository)` for ai-pipe, i.e. the sidecar uses the repository's own TypeScript
  (ai-pipe/node_modules/typescript 6.0.3), not the 5.9.3 pinned under checker/node_modules.
- ai-pipe: 9 configured projects (including 3 under `.claude/worktrees/market-patterns/`), 0 configuration problems.

Repositories (read-only inputs):

- ai-pipe: /Users/cristian/git/ai-pipe @ ea13166c59cfc52574e96959413f5c54be20e8c8 (package `ai-pipe`, `"type": "module"`, root tsconfig.json/tsconfig.app.json/tsconfig.node.json cover `src/` only; `server/` is plain .mjs)
- n8n: /Users/cristian/git/n8n @ 9d9e9bf97e8ae5382a930cd662637a9cf7046ef9
- Old database for arm 4: /Users/cristian/git/n8n/.jscout.db (838,758,400 bytes, Aug 10; -wal is 0 bytes) copied to S/db/n8n-old-copy.db. Its meta: schema_version=6, projection_version=3.

Every jscout command in this report passes `--database <S/db/...>` except `checker doctor` (no database) and `who-uses` (has no `--database` flag; see arm 1h for how it was run).

## Arm 1 — ai-pipe (JS-first package; server/ is plain .mjs with no tsconfig owner)

Repo HEAD ea13166c59cfc52574e96959413f5c54be20e8c8. Stack database S/db/aipipe-stack.db; prestack database S/db/aipipe-prestack.db.
Read-only SQL was run on byte copies (S/db/aipipe-stack-idxcopy.db = after index only; S/db/aipipe-stack-run1copy.db = after enrich run 1) so no query overlapped a writer. sqlite3 `-readonly` cannot open these WAL-mode files without the -shm sidecar, so the copies were opened with `file:...?immutable=1`.

### 1a. index (stack)

`jscout index /Users/cristian/git/ai-pipe --database S/db/aipipe-stack.db` (S/logs/arm1a-index-stack.log)

- `indexed 690 files (removed=0, rejected=0) — 5483 chunks, 28527 refs in 684ms`; `real 0.69s`.
- The index command prints only files/chunks/refs. SQL counters (S/sql/arm1a-counters.txt): files 690 (423 production, 267 test), symbols 4538, member_calls 25340, resolved_edges 38227, receiver_value_flows (extraction rows) 10096, value_binding_flows 87, function_return_flows 204, class_value_flows 12, class_member_value_flow_blockers 21.
- member_call edges (S/sql/arm1a-edges-by-kind.txt): `member-name-match/possible` 5158, `receiver-value-flow/likely` 1025. Other kinds unchanged vs prestack (call 25015, use 3023, import 2219, ...).

### 1b. enrich --dry-run (stack)

`jscout enrich /Users/cristian/git/ai-pipe --database S/db/aipipe-stack.db --dry-run` (S/logs/arm1b-dryrun-stack.log, 0.25s)

- projects 9 (configured_projects 9 discovered; 4 configured + 5 inferred actually selected), configuration_problems 0.
- occurrences: discovered 5158, eligible 3110, selected 3110, omitted 0, deprioritized_builtin_receiver 341, skipped_foreign_namesake 60.
- files_without_configured_project 227, occurrences_without_configured_project 2357, occurrences_skipped_inferred_project 0.
- Inferred scope IDs admitted (all `purpose_reasons: no-configured-owner, compiler-family:node-esm`):

| scope | selected occurrences |
|---|---|
| inferred:.#node-esm/scripts~1 | 171 |
| inferred:.#node-esm/server~1 | 858 |
| inferred:.#node-esm/server~2 | 1305 |
| inferred:tradebook#node-esm | 7 |
| inferred:tradebook/api#node-esm | 16 |

- Configured projects selected: tradebook/api/tsconfig.json 253, tradebook/contracts/tsconfig.json 237, tradebook/web/tsconfig.json 65, tsconfig.app.json 198. The three `.claude/worktrees/market-patterns/*` tsconfigs and root tsconfig.json/tsconfig.node.json selected nothing.
- Test files are not in any scope (no `tests/` scope appears; 267 test-role files). Note `server` is split into two groups (`server~1`, `server~2`), and there is no separate `tradebook/scripts` scope: it is rolled into `inferred:tradebook#node-esm` (7 occurrences).

### 1c. enrich full (stack), run 1 and run 2

`jscout enrich /Users/cristian/git/ai-pipe --database S/db/aipipe-stack.db` (S/logs/arm1c-enrich-stack-run1.log, -run2.log)

| run | real | request_batches | occurrences_queried | unknown_answers | facts_published | peak RSS |
|---|---|---|---|---|---|---|
| 1 (cold) | 17.16s (user 21.47s) | 29 | 3110 | 833 | 19 | 706 MB |
| 2 (unchanged) | 0.30s | 0 | 0 (3110 resumed) | 0 | 19 | n/a |

- All 9 project runs `completed`, execution_kind `checked`, no errors (S/sql/arm1f-project-runs.txt). Per-project sidecar peak RSS 319-673 MB.
- unknown_projects lists 7 of the 9 projects (all 5 inferred scopes plus tradebook/api and tradebook/web). unmapped_declarations 2302: lib 1994, vendored 246, repo_unanchored 37, types 25 — i.e. most checker answers land on lib/node_modules declarations, not repository symbols.
- Only 19 checker facts were published, all `likely`, all from `tradebook/api/tsconfig.json` (BrokerService/CampaignService/OverviewService/TradeService/ResponseCache methods). The five inferred scopes covering 2357 orphan occurrences (server/, scripts/) produced 0 checker facts; the 858+1305 server occurrences answered unknown or mapped to lib/vendored declarations. See Findings.
- Run 2 is an exact reuse: 0 request batches, 3110 resumed, 0.30s.

### 1d. SQL (after index; S/sql/arm1d-*.txt)

```
SELECT provenance, confidence, count(*) FROM resolved_edges WHERE kind='member_call' GROUP BY 1,2;
member-name-match | possible | 5158
receiver-value-flow | likely | 1025      (after enrichment: + checker | likely | 19)
```

Value-flow edges: 1025 edges over 557 distinct occurrences in 120 files.

| flow | edges | occurrences |
|---|---|---|
| factory | 989 | 521 |
| construct | 16 | 16 |
| this | 16 | 16 |
| binding | 4 | 4 |

candidateCount: 1 -> 89 occurrences (89 edges); 2 -> 468 occurrences (936 edges). No occurrence has more than 2 targets.
By file role: 58 occurrences in production files, 499 in test files (tests are not excluded from the index-level pass; top files are tests/dbAdapter.test.mjs 36, tests/newsSource.test.mjs 19, ...). Only 9 value-flow occurrences are in `server/` (1 in api.mjs, 3 in capabilities/builtins.mjs, 5 `this` calls in db/sqliteAdapter.mjs).

Union case (`openDatabase()` returns `createPgAdapter()` or `createSqliteAdapter()`): edges to `PgAdapter::query` / `SqliteAdapter::query` exist for 12 occurrences, none of them in `server/` (the server passes `db` around as a parameter); they are in scripts/migrate-sqlite-to-pg.mjs (3), scripts/patternStats.mjs (1), tests/ (8). Every occurrence whose receiver comes from `openDatabase(...)` has exactly two targets, confidence likely, candidateCount 2 (6 occurrences). The other 6 have exactly one target with candidateCount 1 because the receiver is `createSqliteAdapter(':memory:')` via a helper (tests/dbAdapter.test.mjs) or `new PgAdapter(pool)` (tests/pgAdapterRetry.test.mjs). S/sql/arm1d-union-case.txt.

`this` case: 16 edges; sample of 5 = server/db/sqliteAdapter.mjs lines 71, 75, 82, 89, 95, all `this._enqueue(...)` -> `SqliteAdapter::_enqueue`, candidateCount 1, likely (S/sql/arm1d-this-sample.txt). Every `this.x()` member call in production files (16 of 16) has an extraction row and an edge.

### 1e. Hand truth check

13 receiver-value-flow occurrences (7 production, 6 test), picked by spreading over files/flows rather than a seeded random (sqlite has no seedable random); the full occurrence list is S/sql/arm1e-all-vf-occurrences.txt.

| # | file:line | receiver text | flow / cc | target(s) | verdict | reason |
|---|---|---|---|---|---|---|
| 1 | scripts/_patch-xfollow-v2.mjs:55 | `db.close()` | factory / 2 | PgAdapter::close, SqliteAdapter::close | correct | `const db = openDatabase(...)` (l.36); openDatabase returns `createPgAdapter()` or `createSqliteAdapter()`, which return `new PgAdapter`/`new SqliteAdapter` |
| 2 | scripts/migrate-sqlite-to-pg.mjs:82 | `await sqlite.query(...)` | factory / 2 | PgAdapter::query, SqliteAdapter::query | correct (over-approx.) | `const sqlite = openDatabase(SQLITE_PATH, { driver: 'sqlite' })` — at runtime always SqliteAdapter; the flow cannot see the argument and keeps both |
| 3 | scripts/patternStats.mjs:18 | `await db.query(...)` | factory / 2 | PgAdapter::query, SqliteAdapter::query | correct | `const db = openDatabase(process.env.AIPIPE_DB_PATH ?? ...)` (l.15) |
| 4 | server/api.mjs:130 | `await db.close()` | factory / 2 | PgAdapter::close, SqliteAdapter::close | correct | `const db = openDatabase(DB_PATH)` (l.80) in the same top-level `if (import.meta.url ...)` block; the shutdown closure captures that const, not the `db` parameter of createApiServer (l.35) |
| 5 | server/capabilities/builtins.mjs:8 | `builtinCapabilityRegistry.register(...)` | factory / 1 | CapabilityRegistry::register | correct | `export const builtinCapabilityRegistry = createCapabilityRegistry()` -> `return new CapabilityRegistry()` |
| 6 | server/db/sqliteAdapter.mjs:71 | `this._enqueue(...)` | this / 1 | SqliteAdapter::_enqueue | correct | inside `SqliteAdapter.query`; no subclass overrides `_enqueue` |
| 7 | tradebook/api/src/services/campaign-service.ts:43 | `this.requirePool()` | this / 1 | CampaignService::requirePool | correct | inside `CampaignService.campaigns` |
| 8 | tests/dbAdapter.test.mjs:18 | `await db.query(...)` | factory / 1 | SqliteAdapter::query | correct | `const db = freshDb()`; freshDb returns `createSqliteAdapter(':memory:')` which returns `new SqliteAdapter(db)` (two-hop factory chain) |
| 9 | tests/pgAdapterRetry.test.mjs:19 | `new PgAdapter(pool).query('select 1')` | construct / 1 | PgAdapter::query | correct | direct construction (member_calls.receiver is NULL for this shape) |
| 10 | tests/architectureBoundaries.test.mjs:129 | `builtinCapabilityRegistry.resolve(...)` | binding / 1 | CapabilityRegistry::resolve | correct | imported const from builtins.mjs (= `createCapabilityRegistry()`), cross-module binding flow |
| 11 | tests/helpers/freshDb.mjs:46 | `await adapter.query(...)` | factory / 2 | PgAdapter::query, SqliteAdapter::query | correct (over-approx.) | `const adapter = openDatabase('unused', { driver: 'postgres', ... })` (l.43) — runtime always PgAdapter |
| 12 | tradebook/api/src/services/response-cache.test.ts:7 | `cache.set(...)` | construct / 1 | ResponseCache::set | correct | `const cache = new ResponseCache(10, 2, 100)` (l.6) |
| 13 | tests/advancedMarketWorkflow.test.mjs:62 | `db.close()` | factory / 2 | PgAdapter::close, SqliteAdapter::close | correct (over-approx.) | `const db = openDatabase(':memory:')` (l.10); openDatabase forces SQLite for ':memory:' at runtime |

Result: 13/13 correct; 0 wrong; 4 of the 13 carry a runtime-dead second target because the factory's branch depends on an argument/env value the flow does not model. No edge pointed at a class the receiver cannot be.

Member calls that look like candidates but got NO value-flow edge (S/sql/arm1e-missing-candidates*.txt; `server/` production only):

| # | file:line | call | why the pass gave up |
|---|---|---|---|
| 1 | server/campaigns.mjs:40 | `await db.execute(...)` | `db` is a parameter of `insertCampaign(db, campaign, ...)` (l.22); parameters are open. This pattern (`db.execute/query/queryOne` on a parameter) accounts for 170 server call sites |
| 2 | server/db.mjs:1025 | `await tx.execute(...)` | `tx` is the callback parameter of `db.transaction(async (tx) => ...)` (l.1006); parameter |
| 3 | server/db/pgAdapter.mjs:129 | `await client.query('begin')` | `const client = await this.pool.connect()` (l.127): `await` is excluded by design (thenable assimilation) |
| 4 | server/db/sqliteAdapter.mjs:131 | `db.exec('PRAGMA ...')` | `const db = new DatabaseSync(path)`; extraction recorded `construct>DatabaseSync`, but DatabaseSync is imported from `node:sqlite`, not a repository class, so projection has no target |
| 5 | server/api/routes/brokers.mjs:41 | `await provider.cancelOrder(...)` | `const provider = createIbkrBrokerProvider({...})`; the factory returns an object literal (`return { broker: 'ibkr', health: () => ..., ... }`), not a class instance, so the factory return is unsupported |
| 6 | server/capabilities/runtime.mjs:10 | `registry.resolve(...)` | `registry` is a destructured parameter of `executeCapabilitySync({ registry, ... })` |
| 7 | server/executors/brokerExecutor.mjs:29 | `comms.postNotice(...)` | `const comms = getCommsService()` returns a module-level `let service` that `setCommsService()` reassigns; mutable binding, not closed |

Other un-projected extraction rows in server/ are factories returning arrays/strings/maps (asArray, normalizeList, tagList, ...) or dependency constructors (XMLParser, PDFParse, pg Client) — no repository class to point at.

### 1f. Checker facts vs value-flow (S/sql/arm1f-checker-facts.txt)

```
SELECT confidence, count(*) FROM checker_enrichments GROUP BY 1;   -> likely | 19
checker facts whose member_call_id has a receiver-value-flow edge    -> 0
```

### 1g. enrich --all: prestack vs stack

Prestack: `jscout index` (S/logs/arm1g-index-prestack.log: 690 files, 5483 chunks, 28527 refs, real 0.56s) then
`jscout enrich /Users/cristian/git/ai-pipe --database S/db/aipipe-prestack.db --all` (S/logs/arm1g-enrich-prestack-all.log).
Stack: `jscout enrich ... --database S/db/aipipe-stack-all.db --all` on a fresh byte copy of the stack index (S/logs/arm1g-enrich-stack-all.log).

| | prestack `--all` | stack `--all` |
|---|---|---|
| wall time | **355.06s** (user 487s, sys 51s) | **29.30s** (user 36s, sys 3s) |
| projects | 458 (4 configured + 454 one-Program-per-file: 176 server, 209 tests, 55 scripts, 14 tradebook) | 12 (4 configured + 8 grouped: scripts~1, server~1, server~2, tests~1, tests~2, tradebook, tradebook/api, tradebook/web) |
| occurrences discovered / eligible / selected | 5158 / 5158 / 5158 | 5158 / 4601 / 4601 (557 value-flow occurrences excluded as already resolved) |
| request batches | 466 | 42 |
| unknown answers | 1689 | 1104 |
| facts published | 1412 (108 likely on 108 occ; 1304 possible on 652 occ) | 387 (255 likely on 137 occ; 132 possible on 66 occ) |
| unmapped declarations | 2866 (lib 2393, vendored 244, repo_unanchored 144, types 85) | 3471 (lib 2392, vendored 246, repo_unanchored 144, types 689) |
| peak RSS | 598 MB | 876 MB |
| checker facts by scope | tests/* 58 likely + 1226 possible; scripts/* 76 possible; server/* 8 likely + 2 possible; tradebook/* 12 likely; tradebook/api/tsconfig.json 30 likely | tests~1 200 likely + 40 possible; tests~2 36 likely + 92 possible; tradebook/api/tsconfig.json 19 likely; server/scripts/tradebook inferred scopes 0 |

Checker `likely` facts in the stack carry closed candidate sets: 19 edges with candidateCount 1 and 236 edges (118 occurrences) with candidateCount 2, e.g. receiverTypes `PgAdapter | SqliteAdapter` (PR #76). Prestack published such pairs as `possible` (652 occurrences x 2).

### 1h. Comparison

member_call edges at likely/certain, ai-pipe, `--all` runs (S/sql/arm1g-*-summary.txt):

| provenance | prestack | stack |
|---|---|---|
| receiver-value-flow / likely | – | 1025 edges, 557 occurrences |
| checker / likely | 108 edges, 108 occ | 255 edges, 137 occ |
| checker / possible | 1304 edges, 652 occ | 132 edges, 66 occ |
| member-name-match / possible | 5158 | 5158 |
| **occurrences with >= 1 likely edge** | **108** | **694** |

Set difference on `file:line receiver.prop -> target` strings (S/sql/arm1h-likely-lost.txt, -gained.txt): 0 likely edges lost, 1172 gained (936 receiver-value-flow, 236 checker). Gained occurrences by directory: tests 1094, scripts 76, server 2. The 8 prestack `inferred:server/*` likely facts are all still likely in the stack (the set difference is empty); what changed in `server/` is therefore only +2, consistent with `server/` passing `db` as parameters (1e).

`who-uses` for `SqliteAdapter.query`: the CLI command has no `--database` flag, so each DB was copied to S/aipipe-copy/.jscout.db in turn and the matching binary run against S/aipipe-copy (then removed):
`jscout who-uses S/aipipe-copy "server/db/sqliteAdapter:query"` (S/logs/arm1h-whouses-stack.log, -prestack.log, -stack.json).
Both outputs are byte-identical (280 lines): two targets (`method:SqliteTx query` l.31 and `method:SqliteAdapter query` l.70), each with a `[possible]` list of 137 name-matched `*.query()` call sites and no `[likely]` section. `--json` confirms 274 usages, all `kind: call`, `confidence: possible`. Reason: `cmd_who_uses` (src/commands/core.rs:375) calls `query::who_uses_in_origins` (module-graph/name lookup), not `query::who_uses_anchor_in_origins`, which is the function that reads `resolved_edges`; so the CLI `who-uses` does not surface checker or value-flow edges in either binary.
`jscout neighborhood S/aipipe-copy "sym:server/db/sqliteAdapter.mjs#SqliteAdapter::query@1" --direction in --depth 1` on the stack DB does show them: 11 likely in-edges (10 receiver-value-flow: scripts/patternStats.mjs:18, scripts/migrate-sqlite-to-pg.mjs:82/132/163, tests/helpers/freshDb.mjs:46, tests/patternJournalWriters.test.mjs:52, tests/dbAdapter.test.mjs:18/53/71/118; 1 checker with receiver_types `PgAdapter | SqliteAdapter`: tests/stockPatternsNode.test.mjs:118) (S/logs/arm1h-neighborhood-stack.log). Prestack had 4 likely edges to this target (all checker, tests/dbAdapter.test.mjs).

What this shows: on a JS-first package the stack replaces 454 per-file Programs with 8 grouped scopes (355s -> 29s for `--all`, 17s for the default gate), the value-flow pass resolves 557 occurrences that the checker could not, and 13/13 hand-checked value-flow edges are correct. The default gate produced only 19 checker facts because the server code receives its adapters as parameters and the inferred node-esm scopes answer unknown for them.

## Arm 2 — n8n (TS-first pnpm monorepo; orphans are scripts)

Repo HEAD 9d9e9bf97e8ae5382a930cd662637a9cf7046ef9 (19,310 tracked .js/.ts/.mjs/.cjs files, 1,231 .vue, 166 tsconfig*.json). Stack DB S/db/n8n-stack.db, prestack DB S/db/n8n-prestack.db.

### 2a. index: stack vs prestack (run back to back, nothing else running)

| binary | command | files | chunks | refs | real | user | sys | DB size |
|---|---|---|---|---|---|---|---|---|
| stack | `jscout index /Users/cristian/git/n8n --database S/db/n8n-stack.db` | 19235 | 92234 | 404999 | **22.09s** | 11.54s | 6.66s | 1,106 MB |
| prestack | `jscout index /Users/cristian/git/n8n --database S/db/n8n-prestack.db` | 19235 | 92234 | 404999 | **21.39s** | 10.60s | 6.40s | 1,081 MB |

Value-flow pass overhead on index: +0.70s wall (+3%), +0.94s user, +26 MB on disk (S/logs/arm2a-index-n8n-*.log). Single runs, so the difference is within run-to-run noise.
Stack index counters (S/sql/arm2-n8n-stats.txt): symbols 109,967; member_calls 545,758; resolved_edges 869,952; receiver_value_flows 121,375 (this 16,430; binding 45,866; construct 10,087; factory 48,992); function_return_flows 1,585; value_binding_flows 5,050; class_value_flows 3,033; class_member_value_flow_blockers 7,640. Files by role: production 10,752, test 8,281, documentation 107, generated 67, fixture 28.

### 2b. enrich --dry-run (stack)

`jscout enrich /Users/cristian/git/n8n --database S/db/n8n-stack.db --dry-run` — real 13.83s (S/logs/arm2b-dryrun-n8n-stack.log).

- configured_projects 152, configuration_problems 4 (four tsconfigs with "No inputs were found": packages/@n8n/create-node/tsconfig.json and three under packages/@n8n/typescript-config/; S/logs/arm2-doctor-n8n-stack.log), projects 149 in the plan (131 configured with selections + 19 inferred; `project_decisions` has 150 entries, one configured project with 0 selected / 208 excluded: packages/@n8n/eslint-plugin-community-nodes/tsconfig.eslint.json).
- occurrences: discovered 284,184; eligible 99,662; **selected 99,525**; omitted 137; deprioritized_builtin_receiver 11,944; skipped_foreign_namesake 16,088; occurrences_avoided_by_tooling_filter 208; occurrences_skipped_inferred_project 137.
- files_without_configured_project 98; occurrences_without_configured_project 1,570; inferred scopes cover 1,433 of them.
- checker: TypeScript 6.0.2 (repository).
- Admitted inferred scope IDs (19; all `no-configured-owner` + compiler family):

| scope | selected |
|---|---|
| inferred:.#node-esm | 855 |
| inferred:.#node-cjs | 7 |
| inferred:packages/@n8n/benchmark#node-esm | 105 |
| inferred:packages/@n8n/instance-ai#node-cjs | 77 |
| inferred:packages/cli#node-esm | 54 |
| inferred:packages/@n8n/db#node-esm | 53 |
| inferred:packages/@n8n/scan-community-package#node-esm | 48 |
| inferred:packages/@n8n/workflow-sdk#node-cjs | 47 |
| inferred:packages/@n8n/benchmark#node-cjs | 46 |
| inferred:packages/@n8n/codemirror-lang-html#node-esm | 45 |
| inferred:packages/nodes-base#node-cjs | 43 |
| inferred:packages/cli#node-cjs | 38 |
| inferred:packages/@n8n/ai-utilities#node-cjs | 3 |
| inferred:packages/@n8n/expression-runtime#node-cjs | 3 |
| inferred:packages/@n8n/node-cli#node-esm | 3 |
| inferred:packages/@n8n/telemetry#node-esm | 2 |
| inferred:packages/frontend/@n8n/chat#node-esm | 2 |
| inferred:packages/@n8n/ai-workflow-builder.ee#node-cjs | 1 |
| inferred:packages/@n8n/create-node#node-cjs | 1 |

- There is no `inferred:.github/scripts#node-esm` scope because `.github/` is not indexed at all (0 files with path `.github/%` in the index; the walker runs with `.hidden(true)`, src/walk.rs:86). The root `scripts/` orphans are 47 indexed files (11 test-role) and sit in `inferred:.#node-esm` (855 occurrences; this also absorbs other root-level orphans).
- `scripts/licenses/*.test.mjs` (check-sbom-licenses.test.mjs, enrich-sbom.test.mjs, pipeline.test.mjs, properties.test.mjs, render-licenses-md.test.mjs) are indexed with role=test; `scripts/licenses/*.mjs` are production. Per-file dry-run probes below confirm the test files select 0 occurrences by default.
- `packages/cli/bin/n8n` (no extension) is not indexed (0 rows with path `packages/cli/bin/%`).

### 2c. Value-flow on n8n (S/sql/arm2-n8n-stats.txt)

member_call edges: member-name-match/possible 284,184; receiver-value-flow/likely **14,456 edges on 14,414 occurrences** (1,743 files). No checker edges yet (index only).

| flow | edges | occurrences |
|---|---|---|
| this | 4926 | 4926 |
| construct | 4523 | 4523 |
| binding | 4374 | 4374 |
| factory | 633 | 591 |

candidateCount: 1 -> 14,393 occurrences; 3 -> 21 occurrences (63 edges; all `splitInBatches(...)` in @n8n/workflow-sdk, whose factory returns one of three builder classes). By role: production 5,311 occurrences (727 files), test 9,071 (1,010 files), fixture 27, generated 5.

NestJS parameter-property negative check (S/sql/arm2c-nestjs-check.txt), file packages/cli/src/services/credentials-tester.service.ts (`@Service()` class, constructor(private readonly logger, errorReporter, credentialTypes, nodeTypes, credentialsHelper)):

| line | call | receiver-value-flow edge |
|---|---|---|
| 111 | `this.credentialTypes.getByName(...)` | none |
| 118 | `this.credentialTypes.getSupportedNodes(...)` | none |
| 120 | `this.nodeTypes.getByName(...)` | none |
| 225 | `this.credentialsHelper.applyDefaultsAndOverwrites(...)` | none |
| 420 | `this.nodeTypes.getByNameAndVersion(...)` | none |

The file has 0 value-flow edges, no `class_value_flows` row (decorated classes are skipped entirely by `class_has_runtime_decorators`), and only 2 extraction rows (one binding, one construct). Repo-wide: 0 value-flow edges whose receiver text starts with `this.` (member-chain receivers are never projected), and 0 `this`-flow edges whose target member name is listed in `class_member_value_flow_blockers` for that file.

Hand check of 10 value-flow edges (stride sample over S/sql/arm2c-all-vf-occurrences.txt: production every 1060th, test every 3000th) plus one candidateCount=3 case:

| # | file:line | receiver text | flow / cc | target | verdict | reason |
|---|---|---|---|---|---|---|
| 1 | packages/@n8n/agents/src/evals/contains-keywords.ts:9 | `new Eval('contains-keywords').description(...)` | construct / 1 | Eval::description | correct | chained call on a `new` expression |
| 2 | packages/@n8n/db/src/migrations/postgresdb/1784000000028-AddProjectIdToInstanceAiThread.ts:37 | `this.backfillRemainingToInstanceOwner(ctx)` | this / 1 | AddProjectIdToInstanceAiThreadBase::backfillRemainingToInstanceOwner | correct | class extends the imported abstract base; method is defined only in the base (l.89), resolved through the super chain |
| 3 | packages/@n8n/typeorm/src/driver/postgres/PostgresQueryRunner.ts:2640 | `await this.getCurrentDatabase()` | this / 1 | PostgresQueryRunner::getCurrentDatabase | correct | method defined in the same class (l.347); `await` is on the result, not the receiver |
| 4 | packages/@n8n/typeorm/src/util/DepGraph.ts:95 | `this.hasNode(node)` | this / 1 | DepGraph::hasNode | correct | l.87 |
| 5 | packages/cli/src/modules/instance-ai/tracing/instance-ai-tracing.service.ts:486 | `manager.getAuthHeaders()` | construct / 1 | ProxyTokenManager::getAuthHeaders | correct | `const manager = new ProxyTokenManager(...)` (l.480), call inside an arrow closure; ProxyTokenManager is undecorated |
| 6 | packages/workflow/src/workflow.ts:734 | `this.getParentMainInputNode(returnNode)` | this / 1 | Workflow::getParentMainInputNode | correct | recursive call inside the same method (l.691) |
| 7 | packages/@n8n/agents/src/__tests__/agent-configuration.test.ts:62 | `agent.configuration(...)` | construct / 1 | Agent::configuration | correct | `const agent = new Agent('test')` in the same test |
| 8 | packages/@n8n/task-runner/src/data-request/__tests__/data-request-response-reconstruct.test.ts:42 | `reconstruct.reconstructConnectionInputItems(...)` | construct / 1 | DataRequestResponseReconstruct::reconstructConnectionInputItems | correct | `const reconstruct = new DataRequestResponseReconstruct()` (l.14) |
| 9 | packages/cli/test/integration/project.api.test.ts:1231 | `Container.get(SharedWorkflowRepository)` | binding / 1 | ContainerClass::get (packages/@n8n/di/src/di.ts) | correct | `import { Container } from '@n8n/di'`; di.ts: `export const Container = new ContainerClass()` — cross-workspace-package binding |
| 10 | packages/workflow/test/workflow.test.ts:2345 | `workflow.getHighestNode(...)` | construct / 1 | Workflow::getHighestNode | correct | `const workflow = new Workflow({...})` (l.2337) |
| 11 | packages/@n8n/workflow-sdk/src/merge.test.ts:514 | `splitInBatches(sibNode).onEachBatch(...)` | factory / 3 | SplitInBatchesBuilderImpl, SplitInBatchesBuilderWithExistingNode, SplitInBatchesNamedSyntaxBuilder ::onEachBatch | correct | `splitInBatches()` has three `return new ...` branches (split-in-batches.ts l.363/371/376) |

Result: 11/11 correct, 0 wrong, 0 uncertain.

Per-file / per-package dry-run probes (S/logs/arm2-dryrun-probes.log, S/logs/arm2-dryrun-probe*.json):

| filter | selected | scopes |
|---|---|---|
| `--file scripts/licenses/check-sbom-licenses.test.mjs` | 0 (eligible 0, projects 0) | none — test role excluded by default |
| `--file scripts/licenses/check-sbom-licenses.mjs` | 31 | inferred:.#node-esm 31 |
| `--file scripts` | 855 | inferred:.#node-esm 848, inferred:.#node-cjs 7 |
| `--package n8n-workflow` | 1235 | packages/workflow/tsconfig.build.cjs.json 1235, tsconfig.build.esm.json 1235, tsconfig.json 1235 (same occurrences in all three) |
| `--package @n8n/db` | 5319 | packages/@n8n/db/tsconfig.build.json 5241, tsconfig.json 5241, inferred:packages/@n8n/db#node-esm 53, tsconfig.configs.json 25 |
| `--package @n8n/di` | 10 | packages/@n8n/di/tsconfig.build.json 10, tsconfig.json 10 |

Note that a package's tsconfig.json and tsconfig.build.*.json each select the same occurrences, so the plan builds one Program per tsconfig for the same files (3x for n8n-workflow, 2x for @n8n/db); see arm 2d for what this costs.

### 2d. Restricted enrichment (stack)

The default plan selects 99,525 occurrences (> 40,000), so the full `enrich` was not run. Instead (S/logs/arm2d-enrich-n8n-*.log, all against S/db/n8n-stack.db, sequential):

1. `jscout enrich /Users/cristian/git/n8n --database S/db/n8n-stack.db --package n8n-workflow --package @n8n/db` (run 1, cold)
2. the same command again (run 2, reuse)
3. `jscout enrich ... --file scripts` (root orphan scopes only: inferred:.#node-esm + inferred:.#node-cjs) (run 1)
4. the same command again (run 2, reuse)

Run 1 (packages): **real 374.62s** (user 202.6s, sys 168.2s), projects 7 (packages/@n8n/db/tsconfig.build.json + tsconfig.json, packages/workflow/tsconfig.build.cjs.json + tsconfig.build.esm.json + tsconfig.json, tsconfig.configs.json, inferred:packages/@n8n/db#node-esm), occurrences selected 6,554 but **queried 14,265** (each occurrence is sent to every tsconfig that owns its file: 2x for @n8n/db, 3x for n8n-workflow), request batches 114, unknown answers 4,805, unmapped declarations 7,416 (lib 4,748; repo_unanchored 1,633; vendored 778; types 257), **facts published 3,836**, peak RSS 679 MB. All 7 project runs completed.

Phase split observed by polling `ps`: the sidecar finished staging the last project (inferred:packages/@n8n/db#node-esm, 53/53) by ~01:19:55, i.e. about 60s after start; the jscout process then ran single-threaded at 97-100% CPU until 01:25:11 (about 5 minutes) with a 633 MB WAL, i.e. the **publish phase dominated** (~5 of 6.2 minutes). `checker::enrich` ends with `structural::rebuild_projection` (src/checker/enrich.rs:665/846/863), which deletes and rebuilds all 869,952 resolved_edges/graph_nodes for the snapshot; inside it `project_checker_enrichments` (src/structural.rs:2182) runs a correlated `NOT EXISTS (SELECT 1 FROM resolved_edges value_flow WHERE provenance='receiver-value-flow' AND confidence='likely' AND source_ref_id=call.rowid)` per checker fact row, and `EXPLAIN QUERY PLAN` shows `SCAN value_flow` — resolved_edges has indexes only on (src_key, confidence, kind) and (dst_key, confidence, kind), none on source_ref_id (S/sql/finding-checker-projection-scan.txt). On ai-pipe (38k edges) this is invisible; on n8n (870k edges x thousands of facts) it is consistent with the minutes-long publish. Not proven by profiling — inferred from the query plan and the phase timing.

Run 2 (packages, unchanged): **real 11.60s**, request_batches 0, occurrences_queried 0, occurrences_resumed 6,554, facts_published 3,836 — exact reuse, and the reuse path does not pay the 5-minute publish again.

Runs 3 and 4 (`--file scripts`, the two root orphan scopes): both **exit 1** after 25.8s / 18.8s with
`Error: checker staging batch has no targeted facts; the previously active batch was retained` (src/checker/enrich.rs:3339-3341: a staged batch with 0 facts while another batch is active for the snapshot is treated as an error). The sidecar did run both scopes (inferred:.#node-esm 848/848 staged, inferred:.#node-cjs 7/7 staged; checker_project_runs rows for batch 2 are `completed`) but none of the 855 root-script occurrences yielded a repository-anchored fact, so batch 2 is stored with active=0 and the package batch (id 1) stays active. No JSON summary is printed on this path, so unknown/unmapped counts for the scripts scope are not available. The second run re-ran the sidecar (no reuse) because the failed batch was never activated.

After enrichment (S/sql/arm2d-n8n-after-enrich*.txt):

| | |
|---|---|
| checker_enrichments | likely 3,812 rows on 1,866 occurrences; possible 24 rows on 3 occurrences (rows are per project: @n8n/db facts are stored twice, n8n-workflow facts three times, one per owning tsconfig) |
| facts by project | packages/@n8n/db/tsconfig.build.json and tsconfig.json: 1,786 likely + 12 possible each; packages/workflow/tsconfig.{build.cjs,build.esm,}.json: 80 likely each; tsconfig.configs.json and inferred:packages/@n8n/db#node-esm: 0 |
| resolved member_call edges | checker/likely 1,866 (candidateCount 1), checker/possible 12 on 3 occurrences (candidateCount 4: `this.findMultipleExecutions(...)` / `this.findSingleExecution(...)` in execution.repository.ts, whose targets are the 4 overload signatures `@1..@4` of the same method), receiver-value-flow/likely 14,456, member-name-match/possible 284,184 |
| checker facts on an occurrence that has a value-flow edge | 0 |
| batches | id 1 (packages, 6,554 selected, 7 projects) active=1; id 2 (scripts, 855 selected, 2 projects) active=0 |

What this shows: on n8n the index-time value-flow pass costs ~3% wall on `index` and yields 14.4k likely edges (11/11 hand-checked correct, and the NestJS parameter-property/decorator limit holds); restricted checker runs work and reuse exactly, but (a) the post-checker publish on the 1.1 GB database takes ~5 minutes single-threaded, (b) occurrences are checked once per owning tsconfig, and (c) a restricted run that produces no facts exits non-zero.

## Arm 3 — watch smoke on a copy of ai-pipe (stack binary)

Copy: `rsync -a --exclude .git /Users/cristian/git/ai-pipe/ S/aipipe-copy/` (5.2 GB incl. node_modules; S/logs/arm3-rsync.log).
Command (S/arm3-watch-start.sh): `jscout watch S/aipipe-copy --enrich --database S/db/aipipe-watch.db --debounce-ms 500`, log S/logs/arm3-watch.log, started 01:26:22, stopped 01:33:25 (7 minutes of the 15-minute cap).

| generation | trigger | refresh | enrich |
|---|---|---|---|
| 1 | startup | full: indexed=690, projection=rebuilt, 694 ms | 9 projects, occurrences=3110, facts=19, 17,402 ms (same as the CLI run in 1c) |
| 2 | edit server/db/sqliteAdapter.mjs (added method `_watchProbe()` and a call `this._watchProbe()` inside `close()`, 01:32:08) | incremental: indexed=1 unchanged=689, 279 ms | reasons=source:server/db/sqliteAdapter.mjs; the sidecar processed only `project 1/9 inferred:.#node-esm/server~1 (17 pending, 841 resumed)`, the other 8 projects were carried (projects_carried=8, occurrences_carried=3093, occurrences=875), facts=19, 4,553 ms |
| 3 | edit tests/dbAdapter.test.mjs (added `db._watchProbe()` inside `freshDb()`, 01:32:46) | incremental: indexed=1, 276 ms | reasons=source:tests/dbAdapter.test.mjs; no project was re-checked (`carried 3110/3110 occurrences across 9/9 projects`, occurrences=0, projects_carried=9), facts=19, 831 ms |

DB verification on byte copies of S/db/aipipe-watch.db (S/sql/arm3-db-after-edits.txt): after generation 2 the new call `server/db/sqliteAdapter.mjs:123 this._watchProbe()` has a `receiver-value-flow/likely` edge to `SqliteAdapter::_watchProbe@1` (flow=this); after generation 3 the test call `tests/dbAdapter.test.mjs:7 db._watchProbe()` also has a `receiver-value-flow/likely` edge (flow=factory, via `createSqliteAdapter(':memory:')`). Both came from the incremental projection, not from the checker. The active checker batch advanced with each snapshot (batch ids 2, 3; 3110 selected; active=1). member_call edge totals after the edits: value-flow 1026, checker 19, name-match 5159.

`kill -INT <pid>`: the log ends with `watch generation=3 status=clean` and the process exited with status 130 (SIGINT) immediately; no `watch status=stopped reason=interrupt` line was printed (that line exists in src/watch.rs:945 but was not emitted on this path). No `.jscout.db` was created inside S/aipipe-copy (the `--database` flag was honoured).

What this shows: the production watch path re-indexes incrementally in <300 ms, re-checks only the dirty inferred scope (server~1) first and carries the rest, and does not schedule any inferred-scope enrichment for a test-file change.

## Arm 4 — old database: read-only behaviour and migration (COPY only)

Copy: `cp /Users/cristian/git/n8n/.jscout.db S/db/n8n-old-copy.db` (838,758,400 bytes; the original's -wal is 0 bytes so the main file is complete). Original never opened by any jscout binary (its mtime is still Aug 10). Copy sha256 before: b1944d50c22eda1eaac5f1b3eb9dacd0c3366893a60c2b58297bc08876bf5bc0; meta: schema_version=6, projection_version=3, 21 tables, `embeddings` 0 rows, `semantic_artifacts` 0 rows.

1. Read-only path (stack): `jscout search /Users/cristian/git/n8n "workflow execute" --database S/db/n8n-old-copy.db --lexical-only`
   -> exit 1: `Error: index database `S/db/n8n-old-copy.db` uses schema v6, but this jscout requires v28; run `jscout index`` (S/logs/arm4-readonly-search.log). File sha256 unchanged afterwards. (`neighborhood` has no `--database` flag, so it could not be pointed at the copy.)
2. Write path (stack): `jscout index /Users/cristian/git/n8n --database S/db/n8n-old-copy.db`
   -> exit 1 in 0.00s: `Error: index database `S/db/n8n-old-copy.db` uses unsupported durable schema v6; preserve the old file if its embedding cache or semantic memory matters, then create a fresh index` (S/logs/arm4-index-oldcopy.log). It does **not** migrate or rebuild: schema stays 6, 21 tables, size and sha256 identical (b1944d50...). This is the `DURABLE_SCHEMA_FLOOR = 16` rule in src/store.rs:9/140-147: anything below 16 is refused outright.
3. To exercise the actual migration path, a copy of the prestack ai-pipe DB (schema 26 / projection 11, 43 tables, 1412 checker_enrichments) was opened with the stack binary (S/logs/arm4-v26-migration.log):
   - `search --database` -> exit 1: `uses schema v26, but this jscout requires v28; run `jscout index`` (read-only path refuses).
   - `index --database` -> succeeds in 1.04s; schema 28, projection 12, 49 tables; disposable tables were dropped and recreated (checker_enrichments 1412 -> 0, resolved_edges rebuilt to 38,227); the durable tables (embedding_profiles, embeddings, semantic_embeddings, embedding_index_entries, semantic_artifacts, semantic_relations, semantic_supports, semantic_embedding_index_entries) are still present afterwards. They had 0 rows in this DB, so survival of durable *rows* was not exercised (no embeddings were ever made, per the no-billed-calls rule).
   - `search` works after the migration.

What this shows: read-only commands refuse both a v6 and a v26 database instead of migrating; `index` refuses v6 (below the durable floor) without touching the file and migrates v26 in place by rebuilding the disposable tables.

## Findings

1. **CLI `who-uses` does not surface enriched edges (either binary).** Pre and post outputs for `SqliteAdapter.query` are byte-identical (274 name-matched `possible` call sites, no `[likely]` block) although the stack DB holds 11 likely in-edges to that method (10 value-flow, 1 checker). `cmd_who_uses` (src/commands/core.rs:375) calls `query::who_uses_in_origins` rather than `query::who_uses_anchor_in_origins` (the one that reads `resolved_edges`). `neighborhood --direction in` shows all 11. Evidence: S/logs/arm1h-whouses-stack.log, -prestack.log, -stack.json, S/logs/arm1h-neighborhood-stack.log.
2. **n8n enrichment spends ~5 of 6.2 minutes in the post-checker publish**, single-threaded, on the 1.1 GB DB (real 374.6s; sidecar staging done in ~60s). `rebuild_projection` rebuilds all 870k edges and `project_checker_enrichments` runs a correlated `NOT EXISTS ... resolved_edges.source_ref_id=call.rowid` per fact that `EXPLAIN QUERY PLAN` reports as a full `SCAN` (no index on source_ref_id/provenance). A hand-written query of the same shape on the same DB did not finish in 5 minutes. Inferred, not profiled. Evidence: S/logs/arm2d-enrich-n8n-pkg-run1.log, S/sql/finding-checker-projection-scan.txt, S/sql/arm2d-n8n-after-enrich.txt (truncated by the 5-minute timeout). The ai-pipe publish is instant (38k edges). sys time for that run was 168s (user 203s).
3. **A restricted run that yields zero facts exits 1** (`checker staging batch has no targeted facts; the previously active batch was retained`), prints no JSON summary, and the next identical run re-runs the sidecar (no reuse, 25.8s then 18.8s). Observed for n8n `--file scripts` (855 root-script occurrences, 0 facts). Evidence: S/logs/arm2d-enrich-n8n-scripts-run1.log, -run2.log; src/checker/enrich.rs:3339.
4. **Occurrences are checked once per owning tsconfig.** n8n `--package n8n-workflow --package @n8n/db`: selected 6,554, queried 14,265 (3 Programs for packages/workflow, 2 for @n8n/db over the same files), and facts are stored 2-3x in checker_enrichments (3,812 rows for 1,866 occurrences). Evidence: S/logs/arm2d-enrich-n8n-pkg-run1.log, S/sql/arm2d-n8n-after-enrich.txt, S/logs/arm2-dryrun-probes.log.
5. **Inferred node-esm/node-cjs scopes produced 0 checker facts on both repositories.** ai-pipe default gate: 19 facts, all from tradebook/api/tsconfig.json; the five inferred scopes (2,357 orphan occurrences in server/ and scripts/) published nothing (833 unknown answers; unmapped declarations lib 1,994 / vendored 246). n8n root scripts: 0 facts (finding 3). The stack `--all` run also published nothing from scripts/server inferred scopes, while the prestack per-file run had 8 likely + 78 possible there — those occurrences are now covered by value-flow edges instead. Evidence: S/logs/arm1c-enrich-stack-run1.log, S/sql/arm1f-project-runs.txt, S/sql/arm1g-*-summary.txt.
6. **`.github/` is never indexed** (walker `.hidden(true)`, src/walk.rs:86), so the expected `inferred:.github/scripts#node-esm` scope cannot appear; `.github/scripts/*.mjs` and their tests are invisible to jscout. Evidence: S/sql/arm2-n8n-stats.txt (0 files under `.github/`).
7. **Value-flow edges are mostly in test files**: ai-pipe 499 of 557 occurrences, n8n 9,071 of 14,414 (the index-level pass has no role gate). Only 9 value-flow occurrences are in ai-pipe `server/`, because the server passes adapters as parameters (170 `db.execute/query/queryOne` call sites on parameters). Evidence: S/sql/arm1d-valueflow-stats.txt, S/sql/arm1e-missing-candidates2.txt, S/sql/arm2-n8n-stats.txt.
8. **Hand-check accuracy: 13/13 (ai-pipe) and 11/11 (n8n) value-flow edges correct; 0 wrong.** 4 of the ai-pipe edges carry a second, runtime-dead target (`openDatabase(...)` with an explicit driver or `':memory:'` always yields one adapter; the flow keeps both with confidence likely). Evidence: REPORT 1e and 2c tables.
9. **Old v6 database is refused, not migrated**, by both the read-only path and `index` (exit 1, file untouched, message tells the user to keep the file and create a fresh index). The v26 -> v28 path does migrate in place (1.0s on ai-pipe), dropping disposable tables and keeping the durable ones (row survival untested: 0 embedding rows). Evidence: S/logs/arm4-readonly-search.log, S/logs/arm4-index-oldcopy.log, S/logs/arm4-v26-migration.log.
10. **TypeScript overloads yield `possible` facts with candidateCount 4** (3 n8n occurrences, `this.findMultipleExecutions` / `this.findSingleExecution` -> `@1..@4` overload signatures of the same method) rather than one likely edge. Evidence: S/sql/arm2d-n8n-after-enrich-2.txt.
11. **Watch SIGINT exit** prints no `watch status=stopped reason=interrupt` line (src/watch.rs:945); the process simply exits 130 after the clean generation. Harmless but the log does not record the shutdown. Evidence: S/logs/arm3-watch.log.
12. Operational notes: `who-uses` and `neighborhood` have no `--database` flag (a scratch DB had to be copied into the repo copy as `.jscout.db` to use them); the sidecar uses the repository's TypeScript (6.0.3 in ai-pipe, 6.0.2 in n8n), not the 5.9.3 pinned under checker/node_modules; `sqlite3 -readonly` cannot open jscout's WAL-mode databases without the -shm file (CANTOPEN), `file:...?immutable=1` works once the writer has exited.

## Headline numbers

| measure | prestack | stack |
|---|---|---|
| ai-pipe `enrich` (default gate) | n/a (gate skipped orphan files) | 17.16s cold / 0.30s reuse, 19 checker facts |
| ai-pipe `enrich --all` | 355.06s, 458 projects, 1412 facts (108 likely) | 29.30s, 12 projects, 387 facts (255 likely) |
| ai-pipe occurrences with a likely member_call edge | 108 | 694 (557 value-flow + 137 checker) |
| ai-pipe value-flow edges | – | 1025 edges / 557 occurrences; hand check 13/13 correct |
| n8n `index` | 21.39s | 22.09s (+0.7s, +3%) |
| n8n value-flow edges | – | 14,456 edges / 14,414 occurrences; hand check 11/11 correct |
| n8n `enrich --package n8n-workflow --package @n8n/db` | not run | 374.62s cold (3,836 facts; ~5 min of it publish) / 11.60s reuse |
| n8n `enrich --file scripts` | not run | exit 1, 0 facts (finding 3) |
| watch (ai-pipe copy) | not run | server edit: 279 ms refresh + 4.6s enrich (dirty scope only); test edit: 276 ms + 0.8s, no scope re-checked |
| old v6 DB | not run | read-only and index both refuse; file untouched |

Scratch layout: S/REPORT.md (this file), S/logs/*.log (all command output), S/sql/*.txt (SQL outputs), S/db/*.db (all databases), S/stack and S/prestack (worktrees), S/aipipe-copy (modified copy used by arm 3).
