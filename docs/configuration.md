# Configuration reference

jscout works without a config file: lexical code and documentation indexing/search
need neither credentials nor an inference service. Keep repository policy in
`<root>/.jscout.toml`. The smallest valid file is:

```toml
version = 1
```

`jscout config init <root>` creates the fully commented
[reference template](../.jscout.toml.example), refusing to overwrite an existing
file. `jscout config validate <root>` checks it without indexing or contacting a
model provider. `jscout config show <root>` prints **every effective setting and
its source**; add `--json` for the structured form. Neither form prints secret
values. Validation checks policy, not provider credentials or service readiness;
see [installation and authentication](installation.md) and [local inference](inference.md).

## Resolution and paths

For each setting, an explicit CLI/MCP argument wins over TOML, which wins over a
supported legacy environment variable, which wins over the built-in default.
Omitting a field uses the next layer; it does not mean false. Empty legacy
variables count as absent. `config show` describes TOML/environment/default
resolution, not the overrides of another command invocation. Its sources are
`config`, `legacy-env` and `builtin`.

Only the target root's `.jscout.toml` is loaded: no parent-directory search or
global merge. `jscout --config /path/policy.toml ...` selects an explicit file;
relative `--config` paths resolve from the shell's working directory. Relative
paths **inside** it still resolve from the indexed root, not the config's
directory. An explicit missing file, unknown field, missing `version`, or
unsupported version is an error. Omitting the default file is fine.

`~/` expansion is supported for `llm.auth_file`, `inference.model_cache_root` and
the sidecar script overrides, but not other paths. `sidecars.gateway` and
`sidecars.checker` must name existing files. Endpoint URLs must be absolute
HTTP(S), without embedded credentials, query strings or fragments.

jscout does **not** load `.env` or `.env.example`. Export the variable named by
`api_key_env` into the process that launches jscout; put only its name in TOML.
See [the env example](../.env.example) and [MCP setup](mcp.md) for client processes.

Settings are resolved at process startup. A running watcher hot-reloads only
`docs.enabled`, `docs.include`, `docs.exclude`, and `docs.search.freshness` at a
successful generation boundary. Restart long-running MCP/watch processes for
other config or credential-environment changes.

## Options

Defaults below are for an otherwise unconfigured process. `none` means an
omitted optional field, not a TOML `null` value. Boolean fields accept TOML
`true`/`false`. Positive integer means greater than zero; any additional ceiling
is stated. Lists replace the default list; they do not append to it.

### Schema, database and documentation

| Key | Default | Meaning and constraints |
| --- | --- | --- |
| `version` | required in a file | Integer `1`; independent of the database schema version. |
| `database.path` | `".jscout.db"` | Shared database for code, docs, vectors and provenance. CLI `--database` overrides it where supported. |
| `docs.enabled` | `true` | Admit documentation during indexing. When false, docs search/embed are unavailable; code indexing still works. Reindex after changing it, or let watch publish the change. |
| `docs.include` | `["**/*.md", "**/*.mdx"]` | Root-relative file globs; `[]` admits no docs. See membership rules below. |
| `docs.exclude` | `[]` | Root-relative file globs; exclusion wins over inclusion. |
| `docs.search.vector` | `true` | Permit already-built documentation vectors in retrieval. Does not enable the corpus or make embedding calls. |
| `docs.search.rerank` | `true` | Permit reranking when a reranker is available. |
| `docs.search.freshness` | `false` | Enable index-time provenance collection and bounded freshness reranking. After enabling, reindex before freshness-enabled queries; watch schedules that generation automatically. |
| `docs.search.max_rank_movement` | `2` | Integer `1`–`3`; maximum positions freshness may move a result. Not an age cutoff. |
| `docs.search.limit` | `10` | Positive result-count default for documentation search. |
| `docs.search.response_bytes` | `24000` | Positive serialized response budget for documentation search; explicit request budgets override it. |

#### Documentation membership

The shared deterministic repository walk decides admission in this order:

1. Hard-skipped directories (`.git`, `node_modules`, `dist`, `.next`, `coverage`,
   `out`) and ignore rules prune traversal. The normal walker respects `.ignore`,
   `.gitignore`, Git excludes and global Git ignore rules with their usual
   repository semantics. Symlinks are not followed.
2. Hidden paths are excluded except the **root-level directories** `.github`,
   `.claude`, and `.agents`. Hidden files and further hidden components inside
   those directories remain excluded. An ignore-file whitelist cannot reopen
   other hidden docs paths.
3. Only registered documentation formats with exact lowercase `.md` or `.mdx`
   extensions are eligible. `docs.exclude` globs reject matching files.
4. At least one `docs.include` glob must match the file.

Globs use slash-normalized paths relative to the indexed root and are
case-sensitive. `*` does not cross `/`; `**` can. They match files, not
directories: trailing `/`, leading `!` negation and brace alternation are
rejected. Use separate patterns instead of `**/*.{md,mdx}`. Include cannot revive
an ignored, hidden, hard-skipped or unsupported-format file. Exclusions are
independent of code-file admission.

`jscout docs status <root>` reports membership decisions, including the deciding
rule for a rejected file or pruned directory (without enumerating its children).
The scanner also rejects oversized files (over 4 MiB), invalid UTF-8 and durable
read failures. See [documentation indexing](documentation.md) for chunking,
front matter, MDX and provenance behavior.

### Code search and expansion

File-role values are `production`, `test`, `fixture`, `generated`,
`documentation`, `unknown`. Origin values are `repository`, `workspace`,
`dependency`. Origins must be a nonempty list; file-role `[]` means no role
filter. A `documentation` **file role** describes code such as story files; it
does not put Markdown into code search.

| Key | Default | Meaning and constraints |
| --- | --- | --- |
| `search.vector` | `true` | Permit code-vector retrieval when configured and available; lexical search still works without a provider. |
| `search.rerank` | `true` | Permit reranking when available. |
| `search.attach_memory` | `false` | Attach relevant semantic memory to code search. |
| `search.limit` | `10` | Positive result-count default. |
| `search.response_bytes` | `30000` | Positive compact search response budget. Non-search tool defaults remain 24000 bytes; this setting does not change all tools. |
| `search.file_roles` | `[]` | Allowed file roles; empty includes all roles. |
| `search.origins` | `["repository", "workspace"]` | Allowed origins; add `dependency` to retrieve indexed dependency internals. |
| `search.memory_limit` | `4` | Attached semantic-memory result cap, `1`–`100`. |
| `search.memory_depth` | `2` | Semantic-memory graph depth, `1`–`8`. |
| `search.memory_nodes` | `2000` | Semantic-memory traversal node budget, `1`–`20000`. |
| `search.expansion.enabled` | `false` | Attach bounded structural context to search. Core MCP does not expose structural expansion. |
| `search.expansion.mode` | `"paths"` | `paths` for compact path evidence, or `neighborhood` for diagnostic graph context. |
| `search.expansion.depth` | `1` | Positive traversal depth. |
| `search.expansion.seeds` | `3` | Positive maximum distinct search-hit anchors to expand. |
| `search.expansion.paths` | `8` | Path result cap, `1`–`50`. |
| `search.expansion.nodes` | `40` | Positive expansion node budget. |
| `search.expansion.edges` | `120` | Positive expansion edge budget. |
| `search.expansion.bytes` | `24000` | Positive expansion byte budget, also subject to the overall response budget. |
| `search.expansion.min_confidence` | `"likely"` | `certain`, `likely`, or `possible`. |
| `search.expansion.file_roles` | `["production", "unknown"]` | Roles allowed in expansion; `[]` removes this filter. |

### Embeddings, inference and reranking

Selecting a provider is explicit; possessing an API key does not enable it.
Code and documentation share the embedding provider/model configuration, but
embedding is requested separately with `jscout embed <root>` and
`jscout docs embed <root>`. `index` does not download models or request new
vectors. Disabling vector search does not disable indexing or delete its corpus.

The [template](../.jscout.toml.example) contains separately validated recipes for
local, Voyage, OpenAI and OpenAI-compatible embeddings. Uncomment **one** recipe.
Only the OpenAI provider accepts `embedding.url`; local clients use
`inference.url` instead. See [inference setup](inference.md) before selecting local.

| Key | Default | Meaning and constraints |
| --- | --- | --- |
| `embedding.provider` | none | `local`, `voyage`, `openai`, or `none` (off). |
| `embedding.model` | provider-derived | Local: `BAAI/bge-m3`; Voyage: `voyage-code-3`; OpenAI: `text-embedding-3-small`. Nonempty model ID if set. |
| `embedding.revision` | none | Nonempty model revision pin, used by local Hugging Face loading and profile identity. |
| `embedding.url` | none | Custom OpenAI-compatible embeddings endpoint, including its route (for example `/v1/embeddings`). Valid only with provider `openai`. |
| `embedding.api_key_env` | provider-derived | Secret variable name: `VOYAGE_API_KEY`, or `OPENAI_API_KEY` for normal OpenAI, or `JSCOUT_EMBED_KEY` for a custom OpenAI endpoint. None for local/off. |
| `embedding.query_prefix` | none | Optional model-specific query prefix; empty string is allowed. |
| `embedding.batch` | `64` | Positive embedding request batch size. |
| `embedding.origins` | `["repository", "workspace"]` | Nonempty code-origin allowlist for embedding; documentation is embedded through its separate command. |
| `inference.url` | derived from host/port | Client base URL, normally `http://127.0.0.1:8792`. Omit it to follow host/port changes. A wildcard bind derives a loopback client URL. |
| `inference.host` | `"127.0.0.1"` | Service bind host. A non-loopback TOML value requires `allow_remote = true`. |
| `inference.port` | `8792` | Bind port, `1`–`65535`. Does not override an explicit client URL. |
| `inference.project` | auto-discovered | Override path to the Python inference project. |
| `inference.uv` | `"uv"` | uv executable name/path. |
| `inference.allow_remote` | `false` | Allow a non-loopback bind; only use on a trusted network. |
| `inference.batch_size` | `16` | Positive local-service model inference batch size, separate from embedding request batching. |
| `inference.max_length` | `4096` | Positive local-service tokenizer length limit. |
| `inference.model_cache_root` | service default | Optional Hugging Face cache root; the managed service defaults to `~/.cache/jscout/models`. |
| `reranker.url` | derived for local; otherwise none | Explicit reranking endpoint; local-provider discovery otherwise uses the inference service. |
| `reranker.model` | `"BAAI/bge-reranker-v2-m3"` | Nonempty model ID. |
| `reranker.revision` | none | Nonempty optional revision pin for local model loading. |
| `reranker.top` | `50` | Candidate cap, `1`–`100`; legacy env values above 100 are clamped, TOML values rejected. |
| `reranker.max_chars` | `4000` | Positive per-document character cap sent for reranking. |

Provider and service checks happen when the corresponding feature is used;
`config validate` does not perform requests. Model/revision changes create a new
embedding identity; run the appropriate embedding command for the new profile.

### LLM and sidecars

Generative scouting is optional and is separate from deterministic indexing.
See [authentication](installation.md) for the first login/API-key setup and
[advanced workflows](advanced.md) for scouting concurrency and budgets.

| Key | Default | Meaning and constraints |
| --- | --- | --- |
| `llm.model` | `"openai-codex:gpt-5.6-terra"` | `provider:model` selector; default uses pi-ai OAuth credentials. Model existence is checked by the gateway, not config validation. |
| `llm.reasoning` | provider default | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `provider-default`; provider/model support varies and the gateway rejects unsupported requests. |
| `llm.max_concurrency` | `1` | Positive maximum overlapping scouting model requests; no fixed ceiling. Validation/publication remains serialized. |
| `llm.openai_base_url` | provider default | Base URL for the built-in OpenAI Responses-compatible provider. Not an embedding endpoint. |
| `llm.api_key_env` | `"OPENAI_API_KEY"` | API-key environment-variable name for the built-in OpenAI provider; not used for Codex OAuth. |
| `llm.auth_file` | `"~/.pi-ai/auth.json"` | pi-ai OAuth credential file. jscout does not read Codex CLI credentials from `~/.codex/auth.json`. |
| `llm.openai_compatible_providers` | `[]` | Array of custom OpenAI-compatible provider definitions; nested fields below. Select one through `llm.model = "<id>:<model-id>"`. |
| `llm.openai_compatible_providers.id` | required | Nonempty provider ID, unique within the list. |
| `llm.openai_compatible_providers.name` | provider ID | Nonempty display name. |
| `llm.openai_compatible_providers.base_url` | required | HTTP(S) API base URL, such as `http://127.0.0.1:1234/v1`. |
| `llm.openai_compatible_providers.api_key_env` | none | Optional nonempty secret environment-variable name. |
| `llm.openai_compatible_providers.models.id` | required | Nonempty model ID, unique within its provider. At least one model is required. |
| `llm.openai_compatible_providers.models.name` | model ID | Nonempty display name. |
| `llm.openai_compatible_providers.models.reasoning` | `false` | Whether the model advertises reasoning capability. |
| `llm.openai_compatible_providers.models.context_window` | `131072` | Positive context-token capacity. |
| `llm.openai_compatible_providers.models.max_tokens` | `32768` | Positive maximum output-token capacity. |
| `sidecars.node` | `"node"` | Node executable used for gateway/checker; Node ≥22.19.0 required. |
| `sidecars.gateway` | auto-discovered | Existing gateway entry file; release/npm installations bundle it. |
| `sidecars.checker` | auto-discovered | Existing checker entry file; release/npm installations bundle it. |

Use `[[llm.openai_compatible_providers]]` and
`[[llm.openai_compatible_providers.models]]` TOML array-table syntax as shown in
the template. Legacy camelCase aliases `baseUrl`, `apiKeyEnv`, `contextWindow`,
and `maxTokens` are accepted; prefer the snake_case TOML names above.

### MCP, diagnostics, indexing and watch

| Key | Default | Meaning and constraints |
| --- | --- | --- |
| `mcp.profile` | `"core"` | `core` or `full`; `baseline` aliases core and `structural` aliases full. Core exposes production code/docs retrieval; full adds graph and semantic tools including `annotate`. |
| `mcp.tools` | omitted: all profile tools | Optional nonempty tool-name allowlist. Only narrows the selected profile; unknown names and explicit `[]` are errors. Use actual names such as `semantic_search`, not CLI command labels. |
| `mcp.source_view` | `"full"` | `full` or `elided` source view for applicable semantic operations. |
| `mcp.result_transport` | `"auto"` | `auto`, `text`, or `structured`. Auto includes structured content only for verified clients; JSON-text fallback is always retained. Structured forces the dual form. |
| `telemetry.file` | none | Optional JSONL output path for MCP telemetry. |
| `telemetry.request_log` | none | Optional JSONL raw MCP request log; includes request content. |
| `diagnostics.timing` | `false` | Print operation timing diagnostics. |
| `diagnostics.debug` | `false` | Enable embedding/reranking debug diagnostics. |
| `index.dependencies` | `[]` | Installed package names whose internals are indexed. Nonempty names, no duplicates. Omitted `--deps` uses this list; `--no-deps` disables it for a pass. |
| `watch.embed` | `false` | Refresh code embeddings after a watch generation; does not request documentation embeddings. |
| `watch.product` | `false` | Also embed the default product semantic view; requires `watch.embed = true`. |
| `watch.dependencies` | `[]` | Installed dependency packages to keep indexed by watch; independent of `index.dependencies`. Nonempty names, no duplicates. |
| `watch.enrich` | `false` | Run checker enrichment after generations. |
| `watch.enrich_timeout_seconds` | `300` | Positive hard checker-enrichment timeout. |
| `watch.debounce_ms` | `2000` | Positive event coalescing delay in milliseconds. |
| `watch.reconcile_seconds` | `600` | Periodic full reconciliation interval; `0` disables it, otherwise must exceed the debounce delay. |

MCP registration is client configuration, not a `.jscout.toml` field; see
[MCP setup](mcp.md). Non-search tool budgets are independent of
`search.response_bytes`. Watch publishes complete generations transactionally;
concurrent readers retain the last committed snapshot while rebuilding.

## Legacy environment migration

These fallback variables still work when the corresponding TOML field is absent.
jscout warns when they supply non-secret policy. There is no generic
`JSCOUT_<TOML_KEY>` conversion: keys not listed here have no environment fallback.
Legacy boolean values accept `true/false`, `yes/no`, `on/off`, and `1/0`.

| Legacy variable | TOML replacement |
| --- | --- |
| `JSCOUT_EMBED_PROVIDER` | `embedding.provider` |
| `JSCOUT_EMBED_MODEL` | `embedding.model` |
| `JSCOUT_EMBED_REVISION` | `embedding.revision` |
| `JSCOUT_EMBED_URL` | `embedding.url` |
| `JSCOUT_QUERY_PREFIX` | `embedding.query_prefix` |
| `JSCOUT_INFERENCE_URL` | `inference.url` |
| `JSCOUT_INFERENCE_HOST` | `inference.host` |
| `JSCOUT_INFERENCE_PORT` | `inference.port` |
| `JSCOUT_INFERENCE_PROJECT` | `inference.project` |
| `JSCOUT_UV` | `inference.uv` |
| `JSCOUT_INFERENCE_ALLOW_REMOTE` | `inference.allow_remote` |
| `JSCOUT_INFERENCE_BATCH_SIZE` | `inference.batch_size` |
| `JSCOUT_INFERENCE_MAX_LENGTH` | `inference.max_length` |
| `JSCOUT_MODEL_CACHE_ROOT` | `inference.model_cache_root` |
| `JSCOUT_RERANK_URL` | `reranker.url` |
| `JSCOUT_RERANK_MODEL` | `reranker.model` |
| `JSCOUT_RERANK_REVISION` | `reranker.revision` |
| `JSCOUT_RERANK_TOP` | `reranker.top` |
| `JSCOUT_RERANK_CHARS` | `reranker.max_chars` |
| `JSCOUT_LLM_MODEL` | `llm.model` |
| `JSCOUT_LLM_REASONING` | `llm.reasoning` |
| `JSCOUT_PI_AI_OPENAI_BASE_URL` | `llm.openai_base_url` |
| `JSCOUT_PI_AI_AUTH_FILE` | `llm.auth_file` |
| `JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS` | `llm.openai_compatible_providers` (legacy value is a JSON array) |
| `JSCOUT_NODE` | `sidecars.node` |
| `JSCOUT_PI_AI_GATEWAY` | `sidecars.gateway` |
| `JSCOUT_CHECKER_SIDECAR` | `sidecars.checker` |
| `JSCOUT_TELEMETRY_FILE` | `telemetry.file` |
| `JSCOUT_TIMING` | `diagnostics.timing` |
| `JSCOUT_DEBUG` | `diagnostics.debug` |

Secrets (`OPENAI_API_KEY`, `VOYAGE_API_KEY`, or the configured variable name),
standard Node TLS settings such as `NODE_EXTRA_CA_CERTS`, and invocation labels
`JSCOUT_SESSION_ID`, `JSCOUT_TASK_ID`, `JSCOUT_PROFILE_LABEL` remain environment
values; they are not missing TOML options. `JSCOUT_BUNDLED_*` paths are internal
installed-package discovery transport, reported as `builtin`, not user policy.
