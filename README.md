# js-rag

A fast, runtime-level JavaScript/TypeScript codebase indexer for RAG and agent retrieval, written in Rust on [oxc](https://oxc.rs).

Philosophy: **TypeScript is for humans.** TS syntax is parsed, but type-level constructs (interfaces, type aliases, `declare`, type-only imports/exports) are erased — the index covers the *runtime value graph*: functions, classes, components, calls, renders, module edges. This makes the indexer equally precise on plain JavaScript.

## Commands

```
js-rag index <root>            # build/update .jsrag.db (incremental, content-hash based)
js-rag search <root> "query"   # hybrid BM25 + embedding search (BM25-only without a provider)
js-rag who-uses <root> SPEC    # all usage sites of a symbol, grouped by confidence
js-rag events <root> [name]    # string-keyed event wiring (emit/listen sites)
js-rag watch <root> [--embed]  # re-index on file change (ms-scale for single edits)
js-rag embed <root>            # embed chunks missing embeddings (cached by content hash)
js-rag mcp <root>              # MCP stdio server: semantic_search, who_uses, definition,
                               #   file_outline, events
js-rag stats <root>            # parse stats
js-rag chunks <root>           # dump AST-aware chunks as JSONL
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
- `JSRAG_EMBED_URL` (+ `JSRAG_EMBED_KEY`) — any OpenAI-compatible endpoint (Ollama, LM Studio, vLLM)

Overrides: `JSRAG_EMBED_MODEL`, `JSRAG_EMBED_PROVIDER=voyage|openai|none`.

Asymmetric models: when the model name contains `nomic-embed-code` or `coderankembed`,
queries are automatically prefixed with `"Represent this query for searching relevant
code: "` (documents embed raw). Override with `JSRAG_QUERY_PREFIX`.

LM Studio example (loads `nomic-embed-code` GGUF, serves OpenAI-compatible API):

```bash
JSRAG_EMBED_URL=http://localhost:1234/v1/embeddings \
JSRAG_EMBED_MODEL=text-embedding-nomic-embed-code \
js-rag embed /path/to/repo
```

Embeddings are keyed by (chunk content hash, model): unchanged code is never
re-embedded, and multiple models can coexist in one index.

## Reranking (optional)

Set `JSRAG_RERANK_URL` to a cross-encoder service speaking
`POST {query, candidates:[{id,text}]}` → `{scores:[{id,score}]}` (e.g. a local
bge-reranker-v2-m3). The top RRF candidates are reranked before final ordering.
Tuning: `JSRAG_RERANK_TOP` (candidate pool, default 50), `JSRAG_RERANK_CHARS`
(per-candidate truncation, default 4000), `JSRAG_RERANK_MODEL`.
Diagnostics: `JSRAG_TIMING=1` prints per-stage latency (bm25 / embed-query /
vector-scan / rerank) to stderr on any search.

## MCP integration

```json
{
  "mcpServers": {
    "js-rag": { "command": "/path/to/js-rag", "args": ["mcp", "/path/to/repo"] }
  }
}
```

Run `js-rag index` (or `js-rag watch`) beside it to keep the DB fresh.

## Storage

Everything lives in one SQLite file, `.jsrag.db`, in the repo root (add it to `.gitignore`): chunks + FTS5 (BM25), symbols, import/export tables, resolved module edges, classified references, event sites, member-call sites, embeddings.

## Build

```
cargo build --release   # binary at target/release/js-rag
```
