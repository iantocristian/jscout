# AFFiNE experiment analysis

Date: 2026-08-14
Corpus: AFFiNE at `0f349af8ee` (`canary`)
Tested jscout commit: `1d0d9b0` (merged via PR #25)

Related documents:

- [Architecture diagrams](affine-architecture-diagrams-2026-08-14.md)
- [Proposed fixes and next steps](affine-proposed-fixes-2026-08-14.md)
- [Comprehensive experiment output](affine-experiment-full-output-2026-08-14.md)

## Executive conclusion

The experiment supports keeping structural indexing, embeddings and checker enrichment as independent phases.

- Structural indexing is fast enough to rebuild freely and provides useful exact surfaces.
- Embeddings improve cold-start localization for conceptual questions.
- Checker enrichment materially improves TypeScript member dispatch and multi-hop traversal.
- The current local reranker is not a reliable default on AFFiNE.
- Internal structural breadth is not the primary problem. Agent-facing serialization is.
- Repository coverage remains incomplete because the corpus is JavaScript/TypeScript-only.

The highest-priority defects exposed by the run are the `doc` file-role misclassification, verbose graph responses, lint-only aggregate `tsconfig` ownership during enrichment, and lack of indexed-language diagnostics.

## Experiment state

The release binary was built from jscout commit `1d0d9b0` and run against the
AFFiNE checkout at commit `0f349af8ee`.

The following local artifacts were created in AFFiNE:

- installed jscout skill at `.agents/skills/jscout/SKILL.md` in the AFFiNE checkout;
- AFFiNE index at `.jscout.db` in that checkout.

At the end of the experiment:

- `.agents/` was untracked;
- `.jscout.db`, `.jscout.db-shm` and `.jscout.db-wal` were untracked;
- the main database was 685 MB;
- the local inference service had been stopped;
- AFFiNE source files were not modified.

Installing the skill does not itself register the MCP server. The dispatched agents were therefore given the release CLI path explicitly.

## Phase results

| Phase | Measured result | Database size |
|---|---:|---:|
| Structural index | 6,960 files, 34,177 chunks, 280,707 graph edges, 0 failures, 5.35 seconds | 321 MB |
| BGE-M3 embeddings | 34,172 distinct chunk hashes embedded | 607 MB |
| TypeScript checker enrichment | 29,931 facts and 20,539 additional graph edges | 685 MB |

Structural inventory:

- 41,924 symbols;
- 34,233 entity occurrences;
- 6,109 production files;
- 722 test files;
- 103 documentation files;
- 19 fixtures;
- 7 generated files;
- 123 detected workspace package instances, 116 with indexed files.

Chunk inventory:

- 29,114 production chunks;
- 4,069 test chunks;
- 599 documentation chunks;
- 350 generated chunks;
- 45 fixture chunks;
- 697 bytes average chunk size;
- 7,977 bytes maximum chunk size;
- only five duplicate content hashes.

The first embedding pass therefore had little cross-chunk deduplication available. The durable cache becomes valuable on later reindexes, branch switches and partially unchanged checkouts.

## What is optional

### During `jscout index`

The only meaningful corpus option is `--deps <named-package,...>`.

- First-party JavaScript/TypeScript files are always indexed.
- Production, test, fixture, generated and documentation roles all enter the structural corpus.
- Named dependencies are opt-in.
- Blanket `node_modules` indexing is not supported.

There is currently no index-time include/exclude control for first-party path, file role or chunk kind.

### Separate optional phases

- `jscout embed`: optional vector generation, currently filterable by origin but not role or chunk kind.
- `jscout enrich`: optional TypeScript checker facts, filterable by file, package, member, role and maximum occurrence count.
- scouting: optional LLM-generated semantic artifacts and workflow memory.

### Retrieval options

- lexical-only or hybrid retrieval;
- vector search on/off;
- reranking on/off;
- semantic memory on/off;
- structural expansion on/off;
- primary-hit and expansion file-role filters;
- origin filters;
- confidence, graph-depth, node, edge and byte budgets.

## Structural-only behavior

Structural indexing made exact event strings, symbol names, imports, storage types and known calls searchable. The dispatched agents reconstructed three real workflows:

### Document synchronization

The verified production flow is:

```text
Y.Doc
  → DocFrontend
  → local IndexedDB or SQLite
  → DocSyncPeer
  → CloudDocStorage
  → SpaceSyncGateway
  → PostgreSQL pending updates
  → deferred snapshot merge
  → broadcasts
  → receiving CloudDocStorage
  → local store
  → DocFrontend.applyUpdate
```

Structural search helped find protocol names and gateway/storage wiring. It missed important runtime joins:

- `@OnEvent('doc.updates.pushed')` was not connected to the emitter;
- `emitWithAck` and `@SubscribeMessage` did not enter the event surface;
- `DocSyncPeer` required exact source inspection;
- playground and obsolete paths sometimes ranked above production implementations.

The source review also found a likely behavioral defect: if `PgWorkspaceDocStorageAdapter.pushDocUpdates` filters out every update and returns `0`, `SpaceSyncGateway.onReceiveDocUpdate` still broadcasts the original update and reports `{ accepted: true, timestamp: 0 }`.

### Copilot document permission

The verified regular tool path is:

```text
ToolRuntime.getTools
  → buildDocContentGetter
  → PermissionAccess.user(...).workspace(...).doc(...).can('Doc.Read')
  → DocAccessControllerBuilder.can
  → PermissionService.canDoc
  → PermissionContextLoader
  → native permission evaluator
  → DocReader.getDocMarkdown
```

The structural graph found the tool and reader but could not follow the fluent builder chain or enter the Rust evaluator.

### Blob upload and URL prefixes

The verified TypeScript upload path includes:

- `usePresignedURL.urlPrefix` and `signKey` configuration;
- `WorkspaceBlobStorage.uploadURLConfig`;
- provider-presigned URLs plus `withURLPrefix`, or signed proxy URL generation;
- GraphQL resolver output;
- `CloudBlobStorage` single and multipart client upload paths;
- proxy termination in `R2UploadController`.

The tested AFFiNE checkout also contains a newer Rust custom GET URL path in
[`config.rs`](https://github.com/toeverything/AFFiNE/blob/0f349af8ee/packages/backend/native/src/runtime/object_storage/config.rs#L189).
Structural jscout search could not see it.

## Embedding behavior

The local provider used:

- embedding model: `BAAI/bge-m3`;
- dimensions: 1,024;
- device: Apple MPS;
- reranker: `BAAI/bge-reranker-v2-m3`.

Each embedding is keyed by chunk hash plus the complete profile fingerprint. The embedding text contains repository path, optional scope, optional symbol and chunk source. Graph edges, checker facts and semantic artifacts are not included.

The full cold run embedded 34,172 distinct hashes. Progress was emitted once per 16 chunks, producing more than 2,000 repetitive progress lines. Embeddings were committed incrementally, while the vector occurrence index was synchronized only after the full corpus completed.

### Retrieval comparison

Hybrid search without reranking produced the most reliable overall results.

Examples:

- `transcript retry` placed `CopilotTranscriptionService.retryTask` first, while lexical-only search returned generic retry code.
- permission queries placed `WorkspaceAccessControllerBuilder` and related permission machinery at the top.
- blob queries put `withURLPrefix`, `uploadBlob`, `createProxyUploadUrl` and the client `set` path near the top.
- conceptual sync queries surfaced the gateway listener and storage adapter, although some documentation/test chunks remained ahead of them.

The reranker was inconsistent:

- it improved some tightly worded permission queries;
- it promoted tests and unrelated Copilot code above the defining sync flow;
- it promoted blob tests above `withURLPrefix`;
- it sometimes demoted the TypeScript native wrapper to a negative-scored low rank;
- it added approximately 5–10 seconds in the dispatched agents' observations.

The current reranker receives only chunk content. It does not receive the path, symbol, scope or file role used by embedding. File-role filtering is applied after reranking. These mechanics explain part of the observed degradation and wasted candidate budget.

## Enrichment behavior

Checker doctor found:

- repository TypeScript 6.0.3;
- 118 configured projects;
- 103 projects owning eligible occurrences;
- zero configuration problems.

The dry run reported:

- 71,129 discovered member-call occurrences;
- 49,142 eligible and selected occurrences.

The completed run reported:

- 97,793 queried occurrences;
- 851 request batches;
- 18,086 unknown answers;
- 76,960 unmapped declarations;
- 29,931 published facts;
- 4,323,966,976 peak RSS bytes;
- 3,659,522,712 peak heap bytes.

The run succeeded because it processed isolated projects and staged facts in bounded batches. Memory dropped after each project. Package projects generally stayed below 1.5 GiB RSS.

The root `tsconfig.eslint.json` then claimed all 49,142 eligible occurrences, nearly doubling total occurrence queries and reaching approximately 4.1 GiB RSS. The execution mechanism worked; the ownership/project-selection decision was still wrong for a tooling-only aggregate project.

### Material graph improvement

Graph edges increased from 280,707 to 301,246. All 20,539 added edges were projected as `member_call` relationships after cross-project reconciliation and target validation.

The permission agent could newly traverse:

```text
buildDocContentGetter
  → PermissionAccess.user
  → UserAccessControllerBuilder.workspace
  → WorkspaceAccessControllerBuilder.doc
  → DocAccessControllerBuilder.can
  → PermissionService.canDoc
  → PermissionService.docPermissions
  → PermissionService.evaluateLoaded
```

The blob agent could newly traverse:

```text
WorkspaceBlobResolver.createBlobUpload
  → WorkspaceBlobStorage.presignPut
  → uploadURLConfig
  → createProxyUploadUrl or StorageRuntimeProvider.presignPut
  → withURLPrefix
```

The sync agent could newly traverse:

```text
SpaceSyncGateway.onReceiveDocUpdate
  → permission checks
  → SyncSocketAdapter.push
  → DocStorageAdapter.pushDocUpdates
```

The facts carried `checker` provenance, exact occurrence spans, receiver types, owning-project lists and `likely` or `possible` confidence.

### Remaining graph gaps

- Calls to abstract interfaces do not compose with class-level `extend` edges to reach concrete implementations.
- The PostgreSQL adapter's calls to models, validation and queueing remained absent in focused traversal.
- `DocSyncPeer` and `CloudDocStorage.pushDocUpdate` had empty focused member-call neighborhoods.
- Nest/Socket.IO decorators exist as documentary nodes, but their literal arguments are not joined to producers.
- GraphQL client operation → handler and response-field dataflow remain disconnected.
- Native-module dispatch and all Rust internals remain outside the corpus.
- Search hit `uses` and `used_by` summaries do not incorporate checker-enriched edges; agents must call `neighborhood` to see them.

Rerunning `jscout embed` after enrichment reported `embedded 0/0 chunks`. An existing hybrid query returned identical chunk IDs, order and RRF scores. This directly verified that enrichment does not invalidate or alter embeddings.

## Too much information versus not enough

### Internal information

The structural database is broad, but the run did not show a compelling reason to delete tests, contracts, fixtures or deterministic entities from the structural corpus. Structural indexing took 5.35 seconds, and these roles can matter for behavioral evidence and blast-radius analysis.

Embedding every role and chunk kind is less defensible:

- 15% of chunks were non-production;
- 5,656 chunks were import-only modules;
- embedding currently cannot filter by role or kind;
- import-only and test chunks frequently occupied high ranks.

Role-based embedding must not be implemented until role classification is trustworthy.

### Missing information

The main coverage gaps are:

- non-JavaScript/TypeScript languages;
- dynamic decorators and event transports;
- interface-to-concrete implementation dispatch;
- GraphQL operation/handler/dataflow joins;
- argument values and control-flow ordering;
- explicit indexed-language and ignored-language diagnostics.

The latest AFFiNE commit was primarily Rust. jscout returned unrelated TypeScript results for its defining behavior. This is the strongest evidence that overview/search must disclose corpus coverage rather than imply repository completeness.

## Confirmed file-role defect

[`file_role.rs`](../src/file_role.rs#L73) treated any path component named
`doc` as documentation at the tested commit.

AFFiNE contains at least 62 JavaScript/TypeScript files under a singular `doc` path component. Many are production code, including:

- backend document storage and reader code;
- Blocksuite synchronization code;
- frontend document modules.

Because default expansion permits only `production` and `unknown`, these defining files can disappear from graph traversal. During the sync experiment, enabling `--file-role production` removed the gateway-to-storage edge.

## Output size

Observed sizes:

- overview: 6,396 bytes;
- ordinary eight-hit search: roughly 7–10 KB;
- expanded searches: 24–30 KB with truncation;
- permission fluent path, 10 nodes and 9 edges: 14.9 KB;
- sync gateway depth one, 7 edges: 11.2 KB;
- blob `presignPut`, 6 nodes and 5 edges: approximately 9 KB;
- two-edge focused neighborhoods: approximately 4.1 KB;
- empty neighborhoods: approximately 1.0–1.2 KB.

A compact projection of a real 7,039-character search response was 2,606 characters, a 63% reduction, while retaining file/lines, symbol, kind, snippet and non-empty use information.

Most avoidable bytes come from:

- chunk IDs;
- repeated full anchors;
- default file role and origin;
- `false` booleans and empty arrays;
- byte offsets in every node and edge;
- repeated checker project lists;
- empty unknown/failed-project arrays;
- occurrence IDs and flags;
- zero-valued response-budget counters;
- duplicated evidence metadata.

The appropriate response is to preserve internal structure and add a compact agent transport, with the current representation retained for diagnostics.

## Operational findings

- Embedding progress once per 16 chunks is too verbose.
- Enrichment progress once per 128 occurrences is too verbose on large projects.
- Five parallel neighborhood commands transiently failed with `no readable schema / unable to open database file`; serial retries succeeded. The current neighborhood path uses SQLite read-only/query-only connections, so the experiment does not establish migration or write locking as the cause. This needs an isolated reproduction before changing database-open behavior.
- `.jscout.db*` is not ignored in AFFiNE and is now visible to `git status`.
- The installed agent skill assumes MCP tools, while skill installation does not configure MCP.

## Final assessment

jscout already gives an agent a real advantage on a large TypeScript corpus when the workflow is:

1. hybrid search without reranking;
2. choose a concrete function or method;
3. use an exact surface or a focused enriched neighborhood;
4. verify behavior in source;
5. fall back to language-native search outside the indexed boundary.

It does not yet provide a repository-complete workflow model. The next implementation work should improve role correctness, compact delivery, project selection and coverage transparency before adding more indexed detail.
