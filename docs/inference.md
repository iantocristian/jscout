[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# Embeddings and local inference

## Start with lexical retrieval

Indexing and BM25 search need no Python, model, provider, or API key:

```bash
jscout index /path/to/repo
jscout search /path/to/repo "checkout inventory" --lexical-only
jscout docs search /path/to/repo "deployment procedure" --lexical-only
```

Vectors are optional. Code and documentation have separate embedding commands
and readiness state, while sharing the configured provider/model.

## Local service quickstart

Install [uv](https://docs.astral.sh/uv/getting-started/installation/). The
service uses Python 3.11 or 3.12; uv can obtain a compatible Python if one is
not installed. npm packages and release archives include the service source and
lockfile—no jscout checkout or manual `uv sync --project inference` is needed.

Add this to the indexed repository's `.jscout.toml`:

```toml
[embedding]
provider = "local"
```

Start the service explicitly **from that repository**, because inference
commands load the current directory's configuration:

```bash
cd /path/to/repo
jscout inference serve
```

Bundled installations use `uv run --locked` to prepare the pinned Python environment and run
one service at `http://127.0.0.1:8792`. Startup may download Python and Python
dependencies. Models load lazily on first embedding/reranking use. npm install
and `jscout setup` do not perform any of these downloads.

Keep that terminal running. From another terminal:

```bash
cd /path/to/repo
jscout inference doctor
jscout embed .
jscout docs embed .
jscout search . "checkout inventory"
jscout docs search . "deployment procedure"
```

`inference doctor` reports reachability, device, model/revision and dimensions;
it is not an embedding/retrieval test. The embedding commands exercise actual
model inference and fill their respective caches.

For bundled service sources, the Python environment lives under
`$XDG_CACHE_HOME/jscout/inference/<hash>` when XDG_CACHE_HOME is an absolute
path, otherwise `~/.cache/jscout/inference/<hash>`. The hash includes
`pyproject.toml` and `uv.lock`, so identical installs can share the environment.
An explicit `UV_PROJECT_ENVIRONMENT` overrides that location. Model weights
live separately at `~/.cache/jscout/models`, overridable by
`inference.model_cache_root`.

For a development service, `jscout inference serve --project /path/to/inference`
or `inference.project` takes priority over the bundled copy and uses normal uv
project-environment behavior. Bundled copies take priority over automatic
source-checkout discovery. Keep `service.py`, `pyproject.toml`, and
`uv.lock` together.

## Endpoint and configuration changes

By default the client URL derives from `inference.host` and
`inference.port`. To use another local port, change only:

```toml
[inference]
port = 8793
```

Remove an existing explicit `inference.url` override if you want it to derive
from that port. An explicit URL is independent and is useful when connecting
to a separately managed endpoint. Restart both the service and clients after
changing inference settings. Non-loopback binding requires
`inference.allow_remote = true`; it is not needed for local use.

Code `embed` does not embed documentation. `watch --embed` also maintains
code vectors only: after new document representations appear, run
`jscout docs embed` explicitly. See [documentation indexing](documentation.md)
for corpus enablement, vector fallback, and freshness controls.

## Troubleshooting

- **Service project not found:** use an npm/release package containing
  `inference/`, retain the adjacent directory with archive installs, or pass
  `--project` for a source install.
- **uv not found:** install uv and ensure it is on the service launcher's
  `PATH`, or set `inference.uv` to its executable.
- **Connection refused / wrong port:** run doctor from the same repository as
  the service and check `inference.url` for an explicit stale override.
- **Configuration changed / revision unresolved after an upgrade:** stop the
  old service and restart it with the upgraded package. A successful HTTP
  health check alone does not prove the old process implements the current
  embedding-profile contract. Retry the embedding command afterward.
- **Vectors degraded after a profile change:** rerun the appropriate
  `jscout embed` or `jscout docs embed`; incompatible vectors are never
  mixed. Lexical retrieval remains available.
- **Slow first request / model download:** model loading is lazy; initial
  downloads and CPU inference can be substantial. Verify available disk and
  memory before choosing local models.

## Embeddings (optional)

Search works BM25-only out of the box. Provider selection is explicit:

- `embedding.provider = "local"` — bundled BGE-M3 service at `inference.url`
  (default `http://127.0.0.1:8792`)
- `embedding.provider = "voyage"` plus its configured secret variable —
  `voyage-code-3`
- `embedding.provider = "openai"` plus its configured secret variable —
  `text-embedding-3-small`
- `embedding.provider = "openai"` plus `embedding.url` — an
  OpenAI-compatible endpoint (LM Studio, Ollama, vLLM); optionally authenticate
  with the variable named by `embedding.api_key_env`

An API key or URL without `embedding.provider` does nothing. When a custom
embedding URL is configured, jscout never falls back to `OPENAI_API_KEY` for
that request.

The local service has one process and one port for both models. Its embedding
model is intentionally fixed to BGE-M3; use the OpenAI-compatible adapter for
other embedding models. It selects MPS, then CUDA, then CPU; loads each model
lazily; serializes inference to bound
memory; and exposes `/health`, `/configuration`, `/embed`, and `/rerank`.
Configure its cache and models in `.jscout.toml`. Pin `embedding.revision` and
`reranker.revision` to select different immutable commits; the bundled defaults
are already pinned and their revisions are part of the embedding-profile
fingerprint. Runtime device is diagnostic,
not cache identity: MPS and CUDA reuse the same float16 profile, while CPU uses
a separate float32 profile because dtype changes the generated vectors.

Asymmetric models: when the model name contains `nomic-embed-code` or `coderankembed`,
queries are automatically prefixed with `"Represent this query for searching relevant
code: "` (documents embed raw). Override with `embedding.query_prefix`.

LM Studio example (loads `nomic-embed-code` GGUF, serves OpenAI-compatible API):

```toml
[embedding]
provider = "openai"
url = "http://localhost:1234/v1/embeddings"
model = "text-embedding-nomic-embed-code"
```

Embeddings are keyed by chunk content hash and a fingerprint of provider,
model, endpoint/protocol, revision, pooling, normalization, document-text
format, and other output-affecting configuration. Documents embed bounded chunk
content only. Path, scope, symbol, and imports are occurrence metadata and are
deliberately excluded from text stored under a content-only key; including them
would make duplicate content reuse a vector from an arbitrary path. Each
distinct missing hash is sent to the provider once. The profile records and
enforces vector dimensions, and changing the document representation creates a
new profile instead of silently reusing incompatible vectors. Unchanged code is
not re-embedded, and compatible duplicate chunks share one cached vector.

Upgrade note for `content-v2`: profiles created before the content-only
document format remain intact but are intentionally incompatible. Existing
embedded repositories therefore report `retrieval.vector=degraded` and use
BM25 until `jscout embed <root>` creates the new profile. This is a one-time
full re-embed per provider/model configuration; old vectors are not mixed into
the new space or deleted automatically.

Vector retrieval uses the statically linked `sqlite-vec` extension. jscout
creates one cosine `vec0` virtual table per embedding dimension, partitioned by
profile, source origin, and code format, and keeps occurrence rows in the same
SQLite file.
This removes the Rust full-table cosine loop. The stable `vec0` implementation
is native exact KNN, not an HNSW/approximate index.

`jscout embed` owns profile creation and incrementally materializes new chunk
occurrences when their content hashes already have cached vectors. A profile
without a completed synchronization marker automatically receives a full
repair; `jscout embed <root> --repair` explicitly audits an already-synchronized
profile for orphaned or missing sqlite-vec rows. Search performs readiness
checks only; it never creates a profile, table, or vector row inside its read
snapshot. If vector state is missing or incomplete, the vector stage reports
that `jscout embed` is needed and search continues with BM25. On the August 2026 n8n validation corpus
(92,215 vector occurrences), a warm release search measured 107 ms for exact
KNN and 332 ms for the complete vector stage. ANN/HNSW remains a separate
follow-up rather than a correctness dependency of this storage change.

## Reranking (optional)

With `embedding.provider = "local"`, search sends the top RRF candidates to
the same service's BGE reranker when `search.rerank = true`. To use a separate
service, set `reranker.url` to an endpoint speaking
`POST {model,query,candidates:[{id,text}]}` → `{scores:[{id,score}]}`. A malformed
or incomplete score set is rejected and search falls back to RRF ordering.
File-role allowlists are applied before fusion and reranker pool construction,
so excluded tests or documentation cannot consume the cross-encoder budget.
Each reranker candidate includes an occurrence-specific header with path,
scope, symbol, chunk kind, role, origin, and line range before the source body.
Successful reranking preserves the unreranked RRF tail rather than discarding
it. Search reports `retrieval.reranker` as `active`, `disabled`, or `degraded`.
Tuning: `reranker.top` (candidate pool, default 50), `reranker.max_chars`
(per-candidate truncation, default 4000), and `reranker.model`. `--no-vector`
keeps BM25 plus reranking, `--no-rerank` disables only the cross-encoder, and
`--lexical-only` disables both optional stages.
These changes do not alter the current reranking default. Re-measure real
queries with the contextual input before changing that default in either
direction.
Diagnostics: `diagnostics.timing = true` prints per-stage latency (BM25 /
embed-query + sqlite-vec / rerank) to stderr on search and
structural-projection stage timings during indexing. MCP telemetry records
embedding-query, vector-index, and reranker timings separately without adding
them to agent-facing responses.
