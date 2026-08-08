# jscout

A fast, runtime-level JavaScript/TypeScript codebase indexer for RAG and agent retrieval, written in Rust on [oxc](https://oxc.rs).

Philosophy: **TypeScript is for humans.** TS syntax is parsed, but type-level constructs (interfaces, type aliases, `declare`, type-only imports/exports) are erased — the index covers the *runtime value graph*: functions, classes, components, calls, renders, module edges. This makes the indexer equally precise on plain JavaScript.

## Commands

```
jscout index <root>            # build/update .jscout.db (incremental, content-hash based)
jscout search <root> "query"   # hybrid BM25 + embedding search (BM25-only without a provider)
                               #   add --expand for a bounded structural context pack
jscout who-uses <root> SPEC    # all usage sites of a symbol, grouped by confidence
jscout neighborhood <root> A   # bounded structural traversal around an anchor
jscout events <root> [name]    # string-keyed event wiring (emit/listen sites)
jscout watch <root> [--embed]  # re-index on file change (ms-scale for single edits)
jscout embed <root>            # embed chunks missing embeddings (cached by content hash)
jscout mcp <root>              # MCP stdio server: semantic_search, neighborhood,
                               #   who_uses, definition, file_outline, events
jscout stats <root>            # parse stats
jscout chunks <root>           # dump AST-aware chunks as JSONL
jscout agent-guide             # print agent integration guidance
jscout agent-guide --install R # install a project-local jscout skill
```

`SPEC` is `NAME` or `path-substring:NAME`, e.g. `getUser` or `services/user:getUser`.

`A` accepts a returned node key, a repo-relative file path, a symbol name, or
`path-substring:NAME`. Every neighborhood includes the current repository
snapshot. When reusing an anchor after edits, pass that value with `--snapshot`;
jscout re-resolves stale symbol anchors by path, scope, and name, and returns an
error with candidates instead of guessing when the identity is ambiguous.
Traversal defaults to `certain`/`likely` edges. Use
`--min-confidence possible` to include unresolved string-event hubs and other
explicit candidates. Unknown-receiver member calls are projected through
property hubs; use depth two to traverse from a candidate symbol to possible
callers without materializing every call-site × symbol pair.

## Search anchors and expansion

Search returns a repository snapshot plus ranked hits. Every hit includes a
`file_role`, a `file_anchor`, and one or more snapshot-scoped `anchors`
projected from the chunk's overlapping declarations. Roles are deterministic:
`production`, `test`, `fixture`, `generated`, `documentation`, or `unknown`.
Use repeatable `--file-role` flags to restrict primary hits. Chunks remain
retrieval units; they do not become graph identity.

Structural expansion is off by default and does not alter search scores. Add
`--expand` to attach a separately labelled context pack:

```bash
jscout search /path/to/repo "checkout inventory" --json --expand \
  --response-bytes 24000 \
  --expand-depth 1 --expand-seeds 3 \
  --expand-nodes 40 --expand-edges 120 --expand-bytes 24000
```

Expansion defaults to `production` and `unknown` file-backed nodes while
retaining structural hubs. Use repeatable `--expand-file-role` flags to opt
tests, fixtures, generated files, or documentation back in. Explicitly
included non-production nodes receive deterministic ranking penalties before
the traversal and global node/byte budgets are consumed.

`--response-bytes` caps the complete pretty-printed JSON envelope: hits,
expansion, budget metadata, and serialization overhead. The result reports its
actual `rendered_bytes`, original `unbudgeted_bytes`, and any omitted content.
The expansion node, edge, and payload limits are subordinate budgets shared
across all search-hit seeds. `--expand-min-confidence` defaults to `likely`;
use `possible` only when explicit unresolved candidates are useful.

## Confidence tiers

- **certain** — resolved through binding analysis + Node module resolution (incl. package.json `exports`, tsconfig `paths`, barrel/star re-exports, CommonJS `require` with literals, dynamic `import('...')` literals).
- **possible** — name-matched member calls (`x.getUser()`): candidates listed, never silently dropped. This is the honest checker-less answer for calls through type annotations.

When an otherwise-certain reference resolves to multiple same-named root
symbols, the traversal projection emits every candidate at `possible`
confidence and includes ambiguity details instead of dropping the edge.

Event wiring (`emit('x')` ↔ `on('x')`) is surfaced by the `events` tool/command.

## Embeddings (optional)

Search works BM25-only out of the box. For hybrid semantic search set one of:

- `VOYAGE_API_KEY` — uses `voyage-code-3`
- `OPENAI_API_KEY` — uses `text-embedding-3-small`
- `JSCOUT_EMBED_URL` (+ `JSCOUT_EMBED_KEY`) — any OpenAI-compatible endpoint (Ollama, LM Studio, vLLM)

Overrides: `JSCOUT_EMBED_MODEL`, `JSCOUT_EMBED_PROVIDER=voyage|openai|none`.

Asymmetric models: when the model name contains `nomic-embed-code` or `coderankembed`,
queries are automatically prefixed with `"Represent this query for searching relevant
code: "` (documents embed raw). Override with `JSCOUT_QUERY_PREFIX`.

LM Studio example (loads `nomic-embed-code` GGUF, serves OpenAI-compatible API):

```bash
JSCOUT_EMBED_URL=http://localhost:1234/v1/embeddings \
JSCOUT_EMBED_MODEL=text-embedding-nomic-embed-code \
jscout embed /path/to/repo
```

Embeddings are keyed by (chunk content hash, model): unchanged code is never
re-embedded, and multiple models can coexist in one index.

## Reranking (optional)

Set `JSCOUT_RERANK_URL` to a cross-encoder service speaking
`POST {query, candidates:[{id,text}]}` → `{scores:[{id,score}]}` (e.g. a local
bge-reranker-v2-m3). The top RRF candidates are reranked before final ordering.
Tuning: `JSCOUT_RERANK_TOP` (candidate pool, default 50), `JSCOUT_RERANK_CHARS`
(per-candidate truncation, default 4000), `JSCOUT_RERANK_MODEL`.
Diagnostics: `JSCOUT_TIMING=1` prints per-stage latency (bm25 / embed-query /
vector-scan / rerank) to stderr on search and structural-projection stage
timings during indexing.

## MCP integration

```json
{
  "mcpServers": {
    "jscout": { "command": "/path/to/jscout", "args": ["mcp", "/path/to/repo"] }
  }
}
```

Run `jscout index` (or `jscout watch`) beside it to keep the DB fresh.

MCP metadata alone does not reliably cause every agent client to select a
repository tool. Install the shipped project-local guide so supported agents
receive an explicit integration contract:

```bash
jscout agent-guide --install /path/to/repo
```

The command creates `.agents/skills/jscout/SKILL.md` and refuses to overwrite
an existing guide. Use `jscout agent-guide` to print the same text for clients
that consume `AGENTS.md` or another instruction format.

For controlled evaluation, `jscout mcp` accepts `--profile baseline` (no
`neighborhood` or search expansion) and `--profile structural` (the default).
See [eval/README.md](eval/README.md) for the paired-run protocol and grader.

`definition` returns full source by default. `jscout mcp --source-view elided`
enables the experimental deterministic renderer, and each call can override it
with `view: "full"` or `view: "elided"`. Both representations obey the same
per-definition `source_bytes` ceiling and report original/rendered byte counts.
The first SC-1 agent run found no compression on the artifacts selected by the
elided arm, so elision remains experimental rather than becoming the default.
The first discriminating three-arm run found no outcome gain over grep: both
grep and structural answered 4/4 exactly, while structural inspected fewer
files at substantially higher agent-token cost. See
[eval/results/ai-pipe-discriminating-2026-08-07.md](eval/results/ai-pipe-discriminating-2026-08-07.md).

For opt-in agent-behavior measurement, start MCP with
`--telemetry .jscout-telemetry.jsonl` or set `JSCOUT_TELEMETRY_FILE`. The JSONL
records tool name, latency, success, response size, session, and snapshot. It
does not record queries, arguments, source, or results. Set
`JSCOUT_SESSION_ID` to correlate calls from one evaluation run and
`JSCOUT_TASK_ID` to join it to an evaluation task. Profile and task labels are
included in each record. Expanded searches also record aggregate node totals
and `expansion_role_counts`; no paths or source are added to telemetry.

## Storage

Everything lives in one SQLite file, `.jscout.db`, in the repo root (add it to
`.gitignore`): chunks + FTS5 (BM25), symbols, import/export tables, classified
references, event/member-call sites, embeddings, and a disposable
`graph_nodes`/`resolved_edges` traversal projection. The projection is rebuilt
after indexing so barrel changes can reroute references in otherwise unchanged
files without leaving stale graph edges behind. File roles live on canonical
file rows and are refreshed even when source hashes are unchanged.

## Build

```
cargo build --release   # binary at target/release/jscout
```
