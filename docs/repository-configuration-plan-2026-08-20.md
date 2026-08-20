# Repository runtime configuration implementation plan

- Date: 2026-08-20
- Status: implemented as G21
- Scope: repository-local, non-secret operator configuration for CLI and MCP

## Why this exists

Jscout currently has three overlapping configuration surfaces:

1. command-line flags select one invocation's behavior;
2. `JSCOUT_*` environment variables configure providers, sidecars, telemetry,
   and diagnostics; and
3. MCP tool arguments select retrieval behavior one call at a time.

That is workable for experiments but not for a persistent repository tool. An
operator can disable reranking for one CLI search with `--no-rerank`, and an
MCP caller can pass `rerank: false`, but there is no durable repository default.
The same problem applies to vector retrieval, attached memory, provider/model
selection, the gateway endpoint, telemetry, and an external database path.

The production telemetry retained in PR #51 cannot settle those defaults. It
mixes binaries, repository states, and intentionally different retrieval
postures. Every ordinary row with an active vector stage also had an active
reranker, so the observed latency cannot be assigned to embeddings or
reranking separately. It contains no relevance judgments. Configuration must
therefore make the posture explicit and measurable before any global default
is changed.

## Current MCP and database contract

MCP is already repository-scoped rather than a global database-discovery
daemon:

```text
jscout mcp /path/to/repository
        |        |
        |        +-- canonical repository root
        +----------- one stdio MCP process

default database: /path/to/repository/.jscout.db
override:         jscout mcp /path/to/repository --database /other/index.db
```

The process canonicalizes its root once and opens one database read-only.
Serving another repository requires another process or a restart with another
root. G21 preserves that boundary. It does not add multi-repository routing,
cwd-dependent database discovery, or hot configuration reload.

## Configuration contract

The repository file is named `.jscout.toml` and starts with an explicit schema
version:

```toml
version = 1

[database]
path = ".jscout.db"

[search]
vector = true
rerank = false
attach_memory = false
limit = 10
response_bytes = 24000

[search.expansion]
enabled = false
depth = 1
seeds = 3
nodes = 40
edges = 120
bytes = 24000
min_confidence = "likely"
file_roles = ["production", "unknown"]

[embedding]
provider = "local"
model = "BAAI/bge-m3"
revision = "5617a9f61b028005a4858fdac845db406aefb181"

[inference]
url = "http://127.0.0.1:8792"
uv = "uv"
allow_remote = false
model_cache_root = "~/.cache/jscout/models"

[reranker]
url = "http://127.0.0.1:8792/rerank"
model = "BAAI/bge-reranker-v2-m3"
revision = "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e"
top = 50
max_chars = 4000

[llm]
model = "openai:gpt-5.6-terra"
reasoning = "high"
openai_base_url = "https://gateway.example.com/v1"
api_key_env = "OPENAI_API_KEY"
auth_file = "~/.pi-ai/auth.json"

[sidecars]
node = "node"
# gateway = "/absolute/path/to/gateway/src/main.mjs"
# checker = "/absolute/path/to/checker/src/main.mjs"

[mcp]
profile = "structural"
source_view = "full"

[telemetry]
file = ".jscout/telemetry.jsonl"
# request_log = ".jscout/requests.jsonl"
```

This example deliberately disables reranking for a repository without changing
the product default. It is a deployment choice to measure, not a conclusion
that reranking has no value.

### What belongs in the file

The file owns stable operator policy and command defaults:

- database location;
- code-vector, semantic-vector, reranker, attached-memory, and expansion
  defaults;
- search limits, byte ceilings, origins, and file-role policy;
- embedding, reranker, inference, LLM, gateway, and checker configuration;
- stable dependency-corpus selections and watch phase defaults when those
  command sections are added; and
- MCP profile/source view plus telemetry and request-log paths.

One-shot intent remains on the command line or in the MCP request: the search
query, an exact artifact/anchor, a dry run, a particular scout subject, and a
temporary widened budget. The configuration file must not turn an invocation's
`--max-calls`, target, or mutation request into hidden standing authority.

### Existing environment migration inventory

G21 is not complete after moving only search toggles. Every product-owned
environment setting must have an explicit disposition:

| Current input | Config destination or disposition |
|---|---|
| `JSCOUT_LLM_MODEL`, `JSCOUT_LLM_REASONING` | `llm.model`, `llm.reasoning` |
| `JSCOUT_PI_AI_AUTH_FILE`, `JSCOUT_PI_AI_OPENAI_BASE_URL` | `llm.auth_file`, `llm.openai_base_url` |
| `JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS` | typed `[[llm.openai_compatible_providers]]` tables; no JSON string |
| `JSCOUT_PI_AI_GATEWAY`, `JSCOUT_NODE`, `JSCOUT_CHECKER_SIDECAR` | `sidecars.gateway`, `sidecars.node`, `sidecars.checker` |
| `JSCOUT_EMBED_PROVIDER`, `JSCOUT_EMBED_MODEL`, `JSCOUT_EMBED_REVISION` | `embedding.provider`, `embedding.model`, `embedding.revision` |
| `JSCOUT_EMBED_URL`, `JSCOUT_QUERY_PREFIX` | `embedding.url`, `embedding.query_prefix` |
| `JSCOUT_EMBED_KEY` | secret input named by `embedding.api_key_env`; never copied into TOML |
| `JSCOUT_RERANK_URL`, `JSCOUT_RERANK_MODEL`, `JSCOUT_RERANK_REVISION` | `reranker.url`, `reranker.model`, `reranker.revision` |
| `JSCOUT_RERANK_TOP`, `JSCOUT_RERANK_CHARS` | `reranker.top`, `reranker.max_chars` |
| `JSCOUT_INFERENCE_URL`, `JSCOUT_INFERENCE_HOST`, `JSCOUT_INFERENCE_PORT` | `inference.url`, `inference.host`, `inference.port`; validation distinguishes client URL from server bind |
| `JSCOUT_INFERENCE_PROJECT`, `JSCOUT_UV`, `JSCOUT_INFERENCE_ALLOW_REMOTE` | `inference.project`, `inference.uv`, `inference.allow_remote` |
| `JSCOUT_INFERENCE_BATCH_SIZE`, `JSCOUT_INFERENCE_MAX_LENGTH`, `JSCOUT_MODEL_CACHE_ROOT` | `inference.batch_size`, `inference.max_length`, `inference.model_cache_root` |
| `JSCOUT_TIMING`, `JSCOUT_DEBUG` | `diagnostics.timing`, `diagnostics.debug`; CLI can override |
| `JSCOUT_TELEMETRY_FILE` | `telemetry.file` |
| `JSCOUT_SESSION_ID`, `JSCOUT_TASK_ID`, `JSCOUT_PROFILE_LABEL` | invocation/evaluation labels, not durable repository policy; keep CLI/env injection |
| `OPENAI_API_KEY`, `VOYAGE_API_KEY`, custom provider keys | referenced secrets only |
| `NODE_EXTRA_CA_CERTS` and standard proxy/TLS variables | process/runtime contract, not reinterpreted by jscout config |

Evaluation-only variables such as `JSCOUT_BIN`, `JSCOUT_BENCH_REPO`,
`JSCOUT_EVAL_*`, `JSCOUT_MEMORY_ARM`, `JSCOUT_PHASE`,
`JSCOUT_MCP_FORWARDED_ENV`, and `JSCOUT_REPLAY_TEST_ENV` remain owned by the
evaluation harness and do not enter the product schema.
`JSCOUT_BUNDLED_GATEWAY` and `JSCOUT_BUNDLED_CHECKER` are private npm-launcher
transport for installed sidecar discovery; they are not operator policy or
legacy inputs and never trigger migration warnings.

### Secrets

`.jscout.toml` does not contain API keys, bearer tokens, or inline credential
objects. It may name a secret environment variable (`api_key_env`) or a
credential file (`auth_file`). Existing `OPENAI_API_KEY`, `VOYAGE_API_KEY`,
and custom-provider keys remain secret inputs rather than ordinary
configuration.

The pi-ai compatible-provider JSON currently supplied through
`JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS` becomes typed TOML provider tables.
Those tables contain model and endpoint metadata plus a key-variable name, not
the key. Rust may pass resolved non-secret settings to a spawned JS or Python
sidecar through its child environment as an internal transport detail; users
configure the file, not a shell export list.

### Discovery and paths

- Every repository command resolves its root first and reads exactly
  `<canonical-root>/.jscout.toml`.
- There is no parent-directory search. A nested checkout cannot accidentally
  inherit another repository's model, database, or telemetry policy.
- `--config PATH` selects an explicit file for automation and testing. It does
  not merge an arbitrary chain of files.
- Relative paths are resolved against the canonical repository root, including
  an explicitly selected config outside the root. This keeps `.jscout.db` and
  telemetry behavior independent of the process cwd.
- A leading `~/` is expanded only for machine-level paths such as auth and
  model-cache locations; it is never treated as a repository-relative path.
- A missing implicit file is valid and preserves current built-in behavior. A
  missing explicit file, invalid TOML, unsupported version, unknown field, bad
  enum, unsafe endpoint, or contradictory setting fails before opening a
  database or starting a sidecar.
- MCP and watch load and validate the file once at startup. Operators restart
  either long-running process after changing it; hot reload is deferred until
  there is an observed need.

### Precedence

For non-secret settings, resolution is:

1. an explicit CLI flag or MCP tool argument;
2. the selected/root `.jscout.toml` field;
3. the corresponding legacy `JSCOUT_*` environment value; and
4. the current built-in default.

Environment variables are a compatibility fallback, not a silent override of
an explicit repository field. Secret variables referenced by the chosen
configuration are resolved after this precedence decision.

Each optional MCP boolean is tri-state. Omission means “use repository
configuration”; explicit `true` or `false` wins. The JSON Schema must stop
advertising hard-coded defaults that may disagree with the repository. CLI
search keeps `--no-vector`, `--no-rerank`, and `--lexical-only` and gains
positive `--vector` and `--rerank` overrides so an operator or agent can widen
a repository default deliberately.

## Fingerprints and cached data

G21 adds an effective runtime-configuration fingerprint for observability. It
must not become a global cache invalidation key.

- Changing `search.rerank`, a byte budget, telemetry, or MCP source view does
  not invalidate the structural snapshot and does not mint a new embedding
  profile.
- Embedding cache identity remains scoped to provider, model, immutable
  revision, device/numeric contract where applicable, dimensions, and document
  representation.
- Scout run identity remains scoped to its evidence, prompt/schema, model, and
  generation policy.
- Index-affecting fields such as selected dependency corpora participate only
  in the structural indexing plan that consumes them.

This preserves the expensive embedding cache when operators switch branches,
reindex from scratch, or turn reranking on and off.

## Observability and commands

Add these operator commands:

```text
jscout config show ROOT
jscout config show ROOT --json
jscout config validate ROOT
jscout config init ROOT
```

`init` creates a documented template and refuses to overwrite an existing
file. `show` reports the repository baseline and each durable value's source
(`config`, `legacy-env`, or `builtin`) while redacting credentials. One-shot
CLI and MCP overrides are visible in that command's behavior and telemetry;
they do not mutate the baseline returned by `config show`. At minimum it
reports the canonical root, config path/status, database path, active search
posture, provider/model endpoints, MCP profile, and telemetry destinations.

MCP telemetry gains:

- jscout binary version and runtime configuration fingerprint;
- effective requested vector/reranker/memory/expansion posture per call;
- retrieval stage outcomes; and
- separate embedding-query, vector-index, and reranker timings where those
  stages run.

The privacy-minimal telemetry file continues to omit queries, anchors, source,
and complete arguments. Exact arguments already belong in the explicit
privacy-sensitive `--request-log` surface. Whether several tool calls shared a
client batch and whether the client later truncated an outer message are not
facts an MCP server can discover by itself; evaluation harnesses must record
them, or a client must explicitly send a group identifier.

Search responses continue to expose active/degraded/disabled retrieval status
because it changes the agent's next action. `config_fingerprint` should be
available in MCP initialization/server metadata and telemetry, not repeated in
every search hit.

## Implementation sequence

### Phase 1 — typed loader without behavior changes

1. Add a `config` module with `serde`-derived, `deny_unknown_fields` TOML
   structures, schema-version validation, URL/path validation, redacted display,
   and canonical non-secret fingerprinting.
2. Add a top-level `--config` selector and resolve each command's canonical
   repository root before loading configuration.
3. Implement `config show`, `validate`, and `init`.
4. Keep every absent-file/default path byte-for-byte behavior-compatible with
   the current CLI and MCP.

### Phase 2 — database, MCP, and retrieval policy

1. Route every database-opening command through one resolved database path.
   `--database` remains the highest-priority override.
2. Pass one immutable resolved configuration into `mcp::serve`; do not reread
   environment variables or the file inside tool calls.
3. Make MCP `vector`, `rerank`, `include_memory`, expansion, limits, and byte
   ceilings optional overrides of repository defaults.
4. Add positive CLI vector/reranker overrides and retain all existing negative
   flags.
5. Emit effective posture and configuration identity in diagnostics and
   telemetry.

### Phase 3 — providers and sidecars

1. Replace `embed::Provider::from_env()` with construction from the resolved
   embedding/inference sections plus separately resolved secrets.
2. Resolve reranker endpoint/model/pool limits from configuration and pass a
   stage-specific settings object into search rather than reading process env
   in the ranking loop.
3. Resolve LLM model/reasoning/base URL/auth reference, Node, gateway, and
   checker sidecar from the same configuration object. Preserve CLI overrides
   and legacy-env fallback.
4. Convert compatible pi-ai providers from JSON-in-an-env-var to typed TOML and
   a validated child-sidecar representation.

### Phase 4 — remaining stable command defaults

1. Inventory every remaining `JSCOUT_*` read and classify it as stable config,
   secret, diagnostic switch, or deprecated compatibility input.
2. Add stable index/dependency, embedding batch/origin, checker, watch, scout,
   and summary defaults without converting one-shot targets or monetary/model
   authority into persistent defaults.
3. Update `.env.example` to contain secrets and legacy migration examples only;
   make `.jscout.toml` the operating reference.
4. Warn, but do not fail, when a legacy environment setting supplies a value
   absent from the config. Do not warn for referenced secret variables.

No phase changes the built-in vector, reranker, memory, or expansion defaults.
Default changes belong in separate, measured decisions so configuration work
cannot be credited with retrieval-quality effects.

## Verification

Unit and integration coverage must establish:

- absent config preserves current behavior;
- invalid/unknown configuration fails with the exact file and field;
- CLI/MCP overrides, config, legacy env, and built-in defaults obey precedence;
- secret values never appear in `config show`, errors, fingerprints, telemetry,
  or the database;
- relative database, telemetry, gateway, checker, auth, and cache paths resolve
  from the repository root rather than cwd;
- two MCP processes with different roots/configs open different databases and
  do not share provider policy;
- an MCP process loads once and reports that restart is required after a file
  change;
- `search.rerank = false` suppresses reranking when the argument is omitted,
  while explicit MCP/CLI enablement runs it;
- toggling reranking, memory attachment, expansion, or response budgets reuses
  the same materialized embeddings and does not alter the structural snapshot;
- changing an embedding model/revision creates or selects the correct scoped
  embedding profile;
- external databases continue to work for index, embed, search, annotate, and
  MCP; and
- telemetry rows from different binaries or runtime configurations are
  mechanically distinguishable.

Before changing the built-in reranker default, replay the same fixed query set
against one binary, database, snapshot, and embedding profile with only
reranking toggled. Record per-stage latency, top-k overlap, exact-identifier
rank, selected follow-ups, and a human or task-specific relevance judgment.
The existing mixed production window is not that comparison.

## Acceptance

G21 is accepted when a repository can be indexed, embedded, scouted, searched,
and served over MCP with all non-secret stable settings supplied by one
`.jscout.toml`; users need environment variables only for referenced secrets or
legacy compatibility; MCP reports the exact database and effective retrieval
posture it loaded; and disabling/re-enabling reranking does not discard or
recompute cached embeddings.
