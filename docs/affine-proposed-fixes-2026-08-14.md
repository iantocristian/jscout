# AFFiNE-derived fixes and next steps

Date: 2026-08-14
Evidence: [AFFiNE experiment analysis](affine-experiment-analysis-2026-08-14.md)
Architecture: [jscout architecture diagrams](affine-architecture-diagrams-2026-08-14.md)
Raw record: [comprehensive experiment output](affine-experiment-full-output-2026-08-14.md)

## Decision

Continue implementation. Do not start another broad evaluation campaign yet.

The AFFiNE run established that structural indexing, embeddings and enrichment each provide distinct value. The immediate work is to make those surfaces correct, compact and operationally predictable.

## P0 — correctness and safe defaults

### 1. Fix singular `doc` file-role classification

Problem:

- [`file_role.rs`](../src/file_role.rs#L73) classified every `doc` path
  component as documentation at the tested commit.
- AFFiNE has at least 62 JavaScript/TypeScript files under `doc`; many are production storage, synchronization and frontend modules.
- Default expansion excludes documentation, so defining production paths disappear.

Implementation:

- remove generic singular `doc` from documentation components;
- retain `docs`, `documentation`, `.storybook`, `stories` and story filename markers;
- if singular `doc` support is still wanted, constrain it to repository/package roots or explicit documentation layouts rather than any component;
- reclassify on the next full index rebuild.

Acceptance checks:

- `packages/backend/server/src/core/doc/writer.ts` is `production`;
- `packages/common/nbstore/src/sync/doc/peer.ts` is `production`;
- actual `docs/**` and `*.stories.*` remain `documentation`;
- production-only sync expansion retains gateway-to-storage edges.

### 2. Make local reranking opt-in by default

Problem:

- hybrid without reranking was the most reliable AFFiNE mode;
- the reranker promoted tests, import chunks and unrelated code on several real queries;
- it added approximately 5–10 seconds in agent observations;
- reranker input contains only chunk content, losing path, scope, symbol and role context;
- file-role filtering happens after reranking.

Implementation:

- do not automatically enable the local reranker merely because the local provider is configured;
- expose an explicit `--rerank` flag or separate configured default;
- move file-role filtering before candidate-pool construction;
- build reranker documents from the same path/scope/symbol header used for embeddings;
- preserve hybrid/RRF score and lexical/vector ranks in debug output;
- consider a small role prior or diversified candidate pool rather than excluding tests entirely.

Acceptance checks:

- the four AFFiNE queries in the experiment retain their defining production paths in the top results;
- reranking never returns fewer requested results because disallowed roles consumed the rerank pool;
- response diagnostics say whether BM25, vector and reranking contributed.

### 3. Exclude tooling-only aggregate TypeScript projects from enrichment ownership

Problem:

- package projects resolved their occurrences first;
- `tsconfig.eslint.json` then rechecked all 49,142 eligible occurrences;
- total queried occurrences became 97,793;
- the lint project reached 4.32 GB peak RSS, versus approximately 1.46 GB for the largest package project.

Implementation:

- classify configured projects by purpose using deterministic signals:
  - filenames such as `tsconfig.eslint.json`, `tsconfig.lint.json` and analogous tooling configs;
  - `noEmit` plus broad include patterns alone must not be the only signal, because legitimate build configs may use `noEmit`;
  - package-manager scripts and `extends` lineage can provide supporting evidence;
- exclude tooling-only aggregate projects from ownership when a more specific build/runtime project owns the file;
- retain them as fallback owners only for otherwise-unowned occurrences;
- record the exclusion/fallback decision in `checker doctor` and dry-run output.

Acceptance checks:

- AFFiNE dry run still selects the same unique eligible occurrences;
- `tsconfig.eslint.json` does not requery occurrences already owned by package projects;
- unresolved files owned only by the lint config are reported rather than silently dropped;
- full enrichment peak memory stays near the largest real package project instead of the aggregate project.

## P1 — compact agent transport

### 4. Add a compact response format and make it the MCP/agent default

Problem:

- a real search response shrank 63% without losing useful localization fields;
- focused checker neighborhoods spend roughly 1.5–2 KB per edge/node pair;
- critical edges can be truncated after diagnostic metadata consumes the byte budget.

Proposed compact search shape:

```json
{
  "hits": [
    {
      "at": "packages/backend/server/src/core/sync/gateway.ts:706-766",
      "symbol": "onReceiveDocUpdate",
      "kind": "method",
      "snippet": "@SubscribeMessage('space:push-doc-update') ..."
    }
  ]
}
```

Proposed compact graph shape:

```json
{
  "nodes": {
    "n1": "doc-read.ts:25 buildDocContentGetter",
    "n2": "builder.ts:180 DocAccessControllerBuilder.can"
  },
  "edges": [
    ["n1", "member_call", "n2", "likely"]
  ]
}
```

Default compact output should retain:

- file and line range;
- symbol and kind;
- snippet;
- relation and direction;
- confidence/provenance when non-default;
- receiver type when it disambiguates dispatch;
- concise truncation notice when anything was omitted.

Move to debug-only output:

- chunk and occurrence IDs;
- raw byte spans;
- full anchors repeated on every edge;
- default role/origin;
- false flags and empty arrays;
- repeated project lists;
- empty unknown/failed-project lists;
- `occurrenceSpecific: true`;
- zero response-budget counters;
- nested duplicate evidence objects.

Retain the current representation as `--debug-json` or an MCP diagnostic mode.

Acceptance checks:

- ordinary eight-hit AFFiNE responses are below 4 KB unless snippets themselves require more;
- ten checker edges fit below 8 KB in compact mode;
- byte-budget truncation removes low-ranked edges before required node definitions;
- a critical terminal edge is not omitted merely because preceding edges carry verbose diagnostics.

### 5. Throttle progress output

Problem:

- embedding printed more than 2,000 progress lines at one line per 16 chunks;
- enrichment printed one line per 128 occurrences, producing hundreds of lines for large projects;
- inference server access logs printed one line per request.

Implementation:

- interactive terminal: update a single progress line or emit every few seconds;
- non-interactive logs: emit phase/project summaries and periodic percentage checkpoints;
- detailed per-batch logging only under `--verbose` or a diagnostics environment flag;
- suppress routine inference access logs by default.

## P1 — coverage and product honesty

### 6. Report indexed and ignored language coverage

Problem:

- AFFiNE contains substantial Rust, Swift and other code;
- the latest checked-out change was primarily Rust;
- jscout returned unrelated TypeScript results without explaining that the defining files were absent;
- "complete embedding corpus" can be misunderstood as complete repository coverage.

Implementation:

- during overview/index, count tracked source files by extension/language;
- report indexed files, ignored files and unsupported-language files;
- attach a compact coverage diagnostic to search when the query contains an exact identifier found only in ignored tracked files, if this can be detected cheaply;
- document that embeddings cover indexed chunks, not every repository file.

Acceptance checks:

- AFFiNE overview visibly reports JavaScript/TypeScript indexed and Rust/Swift unsupported;
- searching `custom_presign_get` does not imply a repository-complete zero result;
- agent guide explicitly tells agents when to fall back to language-native tools.

### 7. Keep the structural corpus broad; add embed-time selection later

Do not remove tests, fixtures, generated code or contracts from the structural index based on this experiment. The rebuild is cheap, and those files are useful for behavior and blast radius.

After role correctness is fixed, add optional embedding controls:

- repeatable `--role`;
- repeatable `--kind` or a small policy such as `behavioral`, `all`;
- default policy should be decided from real use, not assumed;
- always retain exact lexical access to skipped chunks.

Potential initial policy to measure:

- production code and contract-bearing chunks embedded;
- tests included but given a lower retrieval prior;
- import-only chunks skipped or embedded only when they contain meaningful reexports;
- generated and fixture chunks opt-in.

## P2 — make enrichment visible and composable

### 8. Include checker edges in search hit summaries

Problem:

- enriched neighborhoods contain useful incoming/outgoing calls;
- search-hit `uses` and `used_by` remain based on older structural summaries;
- agents cannot tell from search that enrichment exists.

Implementation:

- merge bounded checker-backed likely edges into search hit `uses`/`used_by`;
- distinguish `certain` deterministic calls from `likely checker` calls compactly;
- keep possible candidates separately labelled;
- expose active checker batch/version/freshness once per response in debug mode.

### 9. Compose interface calls with implementations

Problem:

- sync traversal stops at `DocStorageAdapter.pushDocUpdates`;
- a class-level `extend` relation exists, but traversal cannot infer the concrete method implementation;
- permission reads similarly stop at abstract `DocReader.getDocMarkdown`.

Implementation direction:

- represent method override/implements relationships explicitly;
- from a call to an abstract/interface method, expose bounded implementation candidates;
- preserve confidence: static receiver type plus registered/constructed provider evidence can increase confidence, while unconstrained subclasses remain possible;
- do not collapse interface and implementation into one certain edge.

Acceptance checks:

- gateway storage traversal can propose `PgWorkspaceDocStorageAdapter.pushDocUpdates` with evidence;
- `DocReader.getDocMarkdown` exposes `DatabaseDocReader` and `RpcDocReader` as implementation candidates;
- candidate fanout is ranked and budgeted.

### 10. Join decorator and transport identities

Problem:

- decorator entities exist, but string arguments are not joined to producer identities;
- `doc.updates.pushed`, `space:push-doc-update`, GraphQL operations and jobs remain fragmented.

Implementation direction:

- retain literal arguments for known decorators/call shapes;
- join:
  - event emitters to `@OnEvent` listeners;
  - Socket.IO `emit`/`emitWithAck` to `@SubscribeMessage` handlers;
  - job producers to `@OnJob` handlers;
  - GraphQL operation definitions/client documents to handlers where identity is deterministic;
- project these as runtime-boundary entities rather than pretending they are ordinary call edges.

Acceptance checks:

- `doc.updates.pushed` returns both writer and gateway listener;
- `space:push-doc-update` returns client producer and gateway handler;
- transcript job producer and `transcriptTask` handler join through the job identity.

### 11. Add limited dataflow evidence where it answers workflow questions

Do not attempt general whole-program dataflow yet. Add bounded, deterministic cases:

- resolver result field → generated GraphQL operation field → immediate client consumption;
- literal option values at exact calls;
- registry identity passed from producer to dispatcher;
- return-object fields consumed in the same generated operation contract.

Keep these relationships separate from runtime call certainty.

## P2 — concurrency and local repository hygiene

### 12. Reproduce the parallel read-open failure before changing storage

Problem:

- parallel neighborhood calls transiently failed with `no readable schema / unable to open database file`;
- serial retries succeeded;
- the current neighborhood path already uses `open_read_only` with SQLite read-only and query-only flags;
- the AFFiNE observation therefore does not identify a write-lock or migration cause.

Implementation:

- reproduce multiple concurrent neighborhood/search processes against an idle, completed database and retain the complete nested SQLite error chain;
- separately repeat while an index, embed or enrichment publication is active;
- test whether sqlite-vec auto-extension registration, WAL sidecar availability, file-descriptor pressure or schema publication is the actual boundary;
- keep ordinary query commands read-only; do not reintroduce schema/profile/vector synchronization into search as a workaround;
- if the issue occurs only during generation publication, add a bounded retry for the publication window and report that condition precisely.

Acceptance checks:

- multiple parallel search/neighborhood processes succeed against one idle completed database;
- concurrent publication either remains readable or returns a precise, retryable generation error;
- no query path acquires write authority.

### 13. Keep `.jscout.db*` out of accidental commits

Implementation options:

- `agent-guide --install` can add `.jscout.db*` to `.git/info/exclude` with an explicit message;
- `jscout index` can warn when its default database is unignored;
- documentation should recommend a user-level or repository ignore entry;
- do not silently edit a repository's tracked `.gitignore`.

## P3 — onboarding consistency

### 14. Align installed skill, CLI and MCP surfaces

Problem:

- the installed skill assumes MCP tools;
- installing the skill does not configure the MCP server;
- some documented/skill surfaces do not exist as equivalent CLI commands.

Implementation:

- installation output must say whether MCP is configured, merely documented or unavailable;
- include CLI fallbacks in the skill;
- add a machine-readable `jscout doctor` section for agent integration state;
- keep README, `agent-guide`, CLI help and MCP tool inventory generated from one source where possible.

## Suggested implementation sequence

1. File-role correction.
2. Compact search/neighborhood transport and progress throttling.
3. Reranker safe-default and candidate-context changes.
4. Tooling-only `tsconfig` project selection.
5. Coverage reporting.
6. Checker edges in search summaries.
7. Interface-to-implementation candidates.
8. Decorator/event/job/GraphQL identity joins.
9. Read-only/concurrent database opening.
10. Embed role/kind policies after the classifier is reliable.

No further broad evaluation is required between steps 1–5. Validate each change on the preserved AFFiNE database or a fresh AFFiNE rebuild, then continue implementation. A later real-agent pass can assess the combined finished surface.
