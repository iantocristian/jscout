# jscout

A fast, runtime-level JavaScript/TypeScript codebase indexer for RAG and agent retrieval, written in Rust on [oxc](https://oxc.rs).

Philosophy: **TypeScript is for humans.** TS syntax is parsed, but type-level constructs (interfaces, type aliases, `declare`, type-only imports/exports) are erased — the index covers the *runtime value graph*: functions, classes, components, calls, renders, module edges. This makes the indexer equally precise on plain JavaScript.

## Commands

```
jscout index <root>            # build/update .jscout.db (incremental, content-hash based)
jscout search <root> "query"   # hybrid BM25 + embedding search (BM25-only without a provider)
jscout who-uses <root> SPEC    # all usage sites of a symbol, grouped by confidence
jscout events <root> [name]    # string-keyed event wiring (emit/listen sites)
jscout watch <root> [--embed]  # re-index on file change (ms-scale for single edits)
jscout embed <root>            # embed chunks missing embeddings (cached by content hash)
jscout mcp <root>              # MCP stdio server: semantic_search, who_uses, definition,
                               #   file_outline, events
jscout stats <root>            # parse stats
jscout chunks <root>           # dump AST-aware chunks as JSONL
```

`SPEC` is `NAME` or `path-substring:NAME`, e.g. `getUser` or `services/user:getUser`.

## Confidence tiers

- **certain** — resolved through binding analysis + Node module resolution (incl. package.json `exports`, tsconfig `paths`, barrel/star re-exports, CommonJS `require` with literals, dynamic `import('...')` literals).
- **possible** — name-matched member calls (`x.getUser()`): candidates listed, never silently dropped. This is the honest checker-less answer for calls through type annotations.

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
vector-scan / rerank) to stderr on any search.

## MCP integration

```json
{
  "mcpServers": {
    "jscout": { "command": "/path/to/jscout", "args": ["mcp", "/path/to/repo"] }
  }
}
```

Run `jscout index` (or `jscout watch`) beside it to keep the DB fresh.

## Storage

Everything lives in one SQLite file, `.jscout.db`, in the repo root (add it to `.gitignore`): chunks + FTS5 (BM25), symbols, import/export tables, resolved module edges, classified references, event sites, member-call sites, embeddings.

## Build

```
cargo build --release   # binary at target/release/jscout
```
