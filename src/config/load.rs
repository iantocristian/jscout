use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{
    DatabaseSettings, DiagnosticsSettings, DocsSearchSettings, DocsSettings, EffectiveConfig,
    EmbeddingSettings, ExpansionSettings, FILE_NAME, IndexSettings, InferenceSettings, LlmSettings,
    McpSettings, OpenAiCompatibleModel, OpenAiCompatibleProvider, RerankerSettings, RuntimeConfig,
    SCHEMA_VERSION, SearchSettings, SidecarSettings, TEMPLATE, TelemetrySettings, ValueSource,
    WatchSettings,
};
use crate::{docs, file_role, origin, search, store};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    version: u32,
    #[serde(default)]
    database: DatabaseFileConfig,
    #[serde(default)]
    docs: DocsFileConfig,
    #[serde(default)]
    search: SearchFileConfig,
    #[serde(default)]
    embedding: EmbeddingFileConfig,
    #[serde(default)]
    inference: InferenceFileConfig,
    #[serde(default)]
    reranker: RerankerFileConfig,
    #[serde(default)]
    llm: LlmFileConfig,
    #[serde(default)]
    sidecars: SidecarFileConfig,
    #[serde(default)]
    mcp: McpFileConfig,
    #[serde(default)]
    telemetry: TelemetryFileConfig,
    #[serde(default)]
    diagnostics: DiagnosticsFileConfig,
    #[serde(default)]
    index: IndexFileConfig,
    #[serde(default)]
    watch: WatchFileConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseFileConfig {
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocsFileConfig {
    enabled: Option<bool>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    #[serde(default)]
    search: DocsSearchFileConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocsSearchFileConfig {
    vector: Option<bool>,
    rerank: Option<bool>,
    freshness: Option<bool>,
    max_rank_movement: Option<usize>,
    limit: Option<usize>,
    response_bytes: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchFileConfig {
    vector: Option<bool>,
    rerank: Option<bool>,
    attach_memory: Option<bool>,
    limit: Option<usize>,
    response_bytes: Option<usize>,
    file_roles: Option<Vec<String>>,
    origins: Option<Vec<String>>,
    memory_limit: Option<usize>,
    memory_depth: Option<usize>,
    memory_nodes: Option<usize>,
    #[serde(default)]
    expansion: ExpansionFileConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpansionFileConfig {
    enabled: Option<bool>,
    mode: Option<String>,
    depth: Option<usize>,
    seeds: Option<usize>,
    paths: Option<usize>,
    nodes: Option<usize>,
    edges: Option<usize>,
    bytes: Option<usize>,
    min_confidence: Option<String>,
    file_roles: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingFileConfig {
    provider: Option<String>,
    model: Option<String>,
    revision: Option<String>,
    url: Option<String>,
    api_key_env: Option<String>,
    query_prefix: Option<String>,
    batch: Option<usize>,
    origins: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InferenceFileConfig {
    url: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    project: Option<String>,
    uv: Option<String>,
    allow_remote: Option<bool>,
    batch_size: Option<usize>,
    max_length: Option<usize>,
    model_cache_root: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RerankerFileConfig {
    url: Option<String>,
    model: Option<String>,
    revision: Option<String>,
    top: Option<usize>,
    max_chars: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmFileConfig {
    model: Option<String>,
    reasoning: Option<String>,
    max_concurrency: Option<usize>,
    openai_base_url: Option<String>,
    api_key_env: Option<String>,
    auth_file: Option<String>,
    openai_compatible_providers: Option<Vec<OpenAiCompatibleProviderFileConfig>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleProviderFileConfig {
    id: String,
    name: Option<String>,
    #[serde(alias = "baseUrl")]
    base_url: String,
    #[serde(alias = "apiKeyEnv")]
    api_key_env: Option<String>,
    models: Vec<OpenAiCompatibleModelFileConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleModelFileConfig {
    id: String,
    name: Option<String>,
    reasoning: Option<bool>,
    #[serde(alias = "contextWindow")]
    context_window: Option<usize>,
    #[serde(alias = "maxTokens")]
    max_tokens: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarFileConfig {
    node: Option<String>,
    gateway: Option<String>,
    checker: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpFileConfig {
    profile: Option<String>,
    source_view: Option<String>,
    result_transport: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TelemetryFileConfig {
    file: Option<String>,
    request_log: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticsFileConfig {
    timing: Option<bool>,
    debug: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexFileConfig {
    dependencies: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchFileConfig {
    embed: Option<bool>,
    product: Option<bool>,
    dependencies: Option<Vec<String>>,
    enrich: Option<bool>,
    enrich_timeout_seconds: Option<u64>,
    debounce_ms: Option<u64>,
    reconcile_seconds: Option<u64>,
}

struct Resolver {
    sources: BTreeMap<String, ValueSource>,
}

impl Resolver {
    fn new() -> Self {
        Self {
            sources: BTreeMap::new(),
        }
    }

    fn configured_or<T: Clone>(&mut self, key: &str, configured: Option<T>, default: T) -> T {
        match configured {
            Some(value) => {
                self.sources.insert(key.to_string(), ValueSource::Config);
                value
            }
            None => {
                self.sources.insert(key.to_string(), ValueSource::Builtin);
                default
            }
        }
    }

    fn string(
        &mut self,
        key: &str,
        configured: Option<String>,
        env_name: Option<&str>,
        default: &str,
    ) -> String {
        if let Some(value) = configured {
            self.sources.insert(key.to_string(), ValueSource::Config);
            return value;
        }
        if let Some(name) = env_name
            && let Some(value) = nonempty_env(name)
        {
            self.sources.insert(key.to_string(), ValueSource::LegacyEnv);
            return value;
        }
        self.sources.insert(key.to_string(), ValueSource::Builtin);
        default.to_string()
    }

    fn optional_string(
        &mut self,
        key: &str,
        configured: Option<String>,
        env_name: Option<&str>,
    ) -> Option<String> {
        if let Some(value) = configured {
            self.sources.insert(key.to_string(), ValueSource::Config);
            return Some(value);
        }
        if let Some(name) = env_name
            && let Some(value) = nonempty_env(name)
        {
            self.sources.insert(key.to_string(), ValueSource::LegacyEnv);
            return Some(value);
        }
        self.sources.insert(key.to_string(), ValueSource::Builtin);
        None
    }

    fn optional_string_with_internal(
        &mut self,
        key: &str,
        configured: Option<String>,
        legacy_env_name: &str,
        internal_env_name: &str,
    ) -> Option<String> {
        if let Some(value) = configured {
            self.sources.insert(key.to_string(), ValueSource::Config);
            return Some(value);
        }
        if let Some(value) = nonempty_env(legacy_env_name) {
            self.sources.insert(key.to_string(), ValueSource::LegacyEnv);
            return Some(value);
        }
        if let Some(value) = nonempty_env(internal_env_name) {
            // Installed-package launcher transport is an implementation of
            // built-in discovery, not a user-facing legacy setting.
            self.sources.insert(key.to_string(), ValueSource::Builtin);
            return Some(value);
        }
        self.sources.insert(key.to_string(), ValueSource::Builtin);
        None
    }

    fn bool(
        &mut self,
        key: &str,
        configured: Option<bool>,
        env_name: Option<&str>,
        default: bool,
    ) -> Result<bool> {
        if let Some(value) = configured {
            self.sources.insert(key.to_string(), ValueSource::Config);
            return Ok(value);
        }
        if let Some(name) = env_name
            && let Some(value) = nonempty_env(name)
        {
            self.sources.insert(key.to_string(), ValueSource::LegacyEnv);
            return parse_bool(name, &value);
        }
        self.sources.insert(key.to_string(), ValueSource::Builtin);
        Ok(default)
    }

    fn usize(
        &mut self,
        key: &str,
        configured: Option<usize>,
        env_name: Option<&str>,
        default: usize,
    ) -> Result<usize> {
        if let Some(value) = configured {
            self.sources.insert(key.to_string(), ValueSource::Config);
            return positive(key, value);
        }
        if let Some(name) = env_name
            && let Some(value) = nonempty_env(name)
        {
            self.sources.insert(key.to_string(), ValueSource::LegacyEnv);
            let value = value
                .parse::<usize>()
                .with_context(|| format!("{name} must be a positive integer"))?;
            return positive(name, value);
        }
        self.sources.insert(key.to_string(), ValueSource::Builtin);
        Ok(default)
    }
}

impl RuntimeConfig {
    pub fn load(root: Option<&Path>, explicit_path: Option<&Path>) -> Result<Self> {
        let root = root
            .map(|path| {
                path.canonicalize()
                    .with_context(|| format!("repository root does not exist: {}", path.display()))
            })
            .transpose()?;
        let config_path = match explicit_path {
            Some(path) => Some(absolute_from_cwd(path)?),
            None => root.as_ref().map(|root| root.join(FILE_NAME)),
        };
        let raw = match config_path.as_ref() {
            Some(path) if path.is_file() => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("read configuration {}", path.display()))?;
                let parsed: FileConfig = toml::from_str(&text)
                    .with_context(|| format!("parse configuration {}", path.display()))?;
                if parsed.version != SCHEMA_VERSION {
                    bail!(
                        "unsupported jscout configuration version {} in {}; expected {}",
                        parsed.version,
                        path.display(),
                        SCHEMA_VERSION
                    );
                }
                Some(parsed)
            }
            Some(path) if explicit_path.is_some() => {
                bail!("explicit configuration does not exist: {}", path.display())
            }
            _ => None,
        };
        let loaded = raw.is_some();
        let raw = raw.unwrap_or_default();
        let mut resolver = Resolver::new();

        let database_configured = raw.database.path.is_some();
        let database_value =
            resolver.string("database.path", raw.database.path, None, store::DB_FILE);
        let database_path = if root.is_none() && !database_configured {
            PathBuf::from(store::DB_FILE)
        } else {
            resolve_path(root.as_deref(), &database_value, false)?
        };

        let docs_enabled = resolver.bool("docs.enabled", raw.docs.enabled, None, true)?;
        let docs_include = resolver.configured_or(
            "docs.include",
            raw.docs.include,
            docs::default_include_globs(),
        );
        let docs_exclude =
            resolver.configured_or("docs.exclude", raw.docs.exclude, Vec::<String>::new());
        docs::corpus::validate_patterns(&docs_include, &docs_exclude)
            .context("validate documentation include/exclude patterns")?;
        let docs = DocsSettings {
            enabled: docs_enabled,
            include: docs_include,
            exclude: docs_exclude,
            search: DocsSearchSettings {
                vector: resolver.bool("docs.search.vector", raw.docs.search.vector, None, true)?,
                rerank: resolver.bool("docs.search.rerank", raw.docs.search.rerank, None, true)?,
                // The preregistered Phase 3 evaluation chooses whether this
                // ships enabled. Candidate runs set the treatment explicitly.
                freshness: resolver.bool(
                    "docs.search.freshness",
                    raw.docs.search.freshness,
                    None,
                    false,
                )?,
                max_rank_movement: {
                    let value = resolver.usize(
                        "docs.search.max_rank_movement",
                        raw.docs.search.max_rank_movement,
                        None,
                        2,
                    )?;
                    anyhow::ensure!(
                        (1..=3).contains(&value),
                        "docs.search.max_rank_movement must be between 1 and 3"
                    );
                    value
                },
                limit: resolver.usize("docs.search.limit", raw.docs.search.limit, None, 10)?,
                response_bytes: resolver.usize(
                    "docs.search.response_bytes",
                    raw.docs.search.response_bytes,
                    None,
                    24_000,
                )?,
            },
        };

        let search_file_roles = resolver.configured_or(
            "search.file_roles",
            raw.search.file_roles,
            Vec::<String>::new(),
        );
        file_role::validate_all(&search_file_roles)?;
        let search_origins =
            resolver.configured_or("search.origins", raw.search.origins, origin::defaults());
        origin::validate_all(&search_origins)?;
        let expansion_file_roles = resolver.configured_or(
            "search.expansion.file_roles",
            raw.search.expansion.file_roles,
            file_role::DEFAULT_EXPANSION
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        );
        file_role::validate_all(&expansion_file_roles)?;
        let min_confidence = resolver.string(
            "search.expansion.min_confidence",
            raw.search.expansion.min_confidence,
            None,
            "likely",
        );
        if !matches!(min_confidence.as_str(), "certain" | "likely" | "possible") {
            bail!("search.expansion.min_confidence must be certain, likely, or possible");
        }
        let expansion_mode = resolver.string(
            "search.expansion.mode",
            raw.search.expansion.mode,
            None,
            "paths",
        );
        search::ExpansionProjection::parse(&expansion_mode)
            .context("validate search.expansion.mode")?;
        let search = SearchSettings {
            vector: resolver.bool("search.vector", raw.search.vector, None, true)?,
            rerank: resolver.bool("search.rerank", raw.search.rerank, None, true)?,
            attach_memory: resolver.bool(
                "search.attach_memory",
                raw.search.attach_memory,
                None,
                false,
            )?,
            limit: resolver.usize(
                "search.limit",
                raw.search.limit,
                None,
                search::DEFAULT_RESULT_LIMIT,
            )?,
            response_bytes: resolver.usize(
                "search.response_bytes",
                raw.search.response_bytes,
                None,
                search::DEFAULT_RESPONSE_BYTE_LIMIT,
            )?,
            file_roles: search_file_roles,
            origins: search_origins,
            memory_limit: resolver.usize(
                "search.memory_limit",
                raw.search.memory_limit,
                None,
                4,
            )?,
            memory_depth: resolver.usize(
                "search.memory_depth",
                raw.search.memory_depth,
                None,
                search::DEFAULT_MEMORY_GRAPH_DEPTH,
            )?,
            memory_nodes: resolver.usize(
                "search.memory_nodes",
                raw.search.memory_nodes,
                None,
                search::DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            )?,
            expansion: ExpansionSettings {
                enabled: resolver.bool(
                    "search.expansion.enabled",
                    raw.search.expansion.enabled,
                    None,
                    false,
                )?,
                mode: expansion_mode,
                depth: resolver.usize(
                    "search.expansion.depth",
                    raw.search.expansion.depth,
                    None,
                    1,
                )?,
                seeds: resolver.usize(
                    "search.expansion.seeds",
                    raw.search.expansion.seeds,
                    None,
                    3,
                )?,
                paths: resolver.usize(
                    "search.expansion.paths",
                    raw.search.expansion.paths,
                    None,
                    search::DEFAULT_EXPANSION_PATH_LIMIT,
                )?,
                nodes: resolver.usize(
                    "search.expansion.nodes",
                    raw.search.expansion.nodes,
                    None,
                    40,
                )?,
                edges: resolver.usize(
                    "search.expansion.edges",
                    raw.search.expansion.edges,
                    None,
                    120,
                )?,
                bytes: resolver.usize(
                    "search.expansion.bytes",
                    raw.search.expansion.bytes,
                    None,
                    24_000,
                )?,
                min_confidence,
                file_roles: expansion_file_roles,
            },
        };
        if search.memory_limit > 100 {
            bail!("search.memory_limit must be at most 100");
        }
        if search.expansion.paths > search::MAX_EXPANSION_PATH_LIMIT {
            bail!(
                "search.expansion.paths must be at most {}",
                search::MAX_EXPANSION_PATH_LIMIT
            );
        }
        if search.memory_depth > search::MAX_MEMORY_GRAPH_DEPTH {
            bail!(
                "search.memory_depth must be at most {}",
                search::MAX_MEMORY_GRAPH_DEPTH
            );
        }
        if search.memory_nodes > search::MAX_MEMORY_GRAPH_NODE_LIMIT {
            bail!(
                "search.memory_nodes must be at most {}",
                search::MAX_MEMORY_GRAPH_NODE_LIMIT
            );
        }

        let provider = normalize_provider(resolver.optional_string(
            "embedding.provider",
            raw.embedding.provider,
            Some("JSCOUT_EMBED_PROVIDER"),
        ))?;
        let model_default = match provider.as_deref() {
            Some("local") => Some("BAAI/bge-m3"),
            Some("voyage") => Some("voyage-code-3"),
            Some("openai") => Some("text-embedding-3-small"),
            _ => None,
        };
        let model = match model_default {
            Some(default) => Some(resolver.string(
                "embedding.model",
                raw.embedding.model,
                Some("JSCOUT_EMBED_MODEL"),
                default,
            )),
            None => resolver.optional_string(
                "embedding.model",
                raw.embedding.model,
                Some("JSCOUT_EMBED_MODEL"),
            ),
        };
        let embed_url =
            resolver.optional_string("embedding.url", raw.embedding.url, Some("JSCOUT_EMBED_URL"));
        validate_optional_endpoint("embedding.url", embed_url.as_deref())?;
        if embed_url.is_some() && provider.as_deref() != Some("openai") {
            bail!("embedding.url is supported only when embedding.provider = \"openai\"");
        }
        let key_default = match provider.as_deref() {
            Some("voyage") => Some("VOYAGE_API_KEY"),
            Some("openai") if embed_url.is_some() => Some("JSCOUT_EMBED_KEY"),
            Some("openai") => Some("OPENAI_API_KEY"),
            _ => None,
        };
        let api_key_env_configured = raw.embedding.api_key_env.is_some();
        let api_key_env = raw
            .embedding
            .api_key_env
            .or_else(|| key_default.map(str::to_string));
        resolver.sources.insert(
            "embedding.api_key_env".to_string(),
            if api_key_env_configured {
                ValueSource::Config
            } else {
                ValueSource::Builtin
            },
        );
        let embed_origins = resolver.configured_or(
            "embedding.origins",
            raw.embedding.origins,
            origin::defaults(),
        );
        origin::validate_all(&embed_origins)?;
        let embedding = EmbeddingSettings {
            provider,
            model,
            revision: resolver.optional_string(
                "embedding.revision",
                raw.embedding.revision,
                Some("JSCOUT_EMBED_REVISION"),
            ),
            url: embed_url,
            api_key_env,
            query_prefix: resolver.optional_string(
                "embedding.query_prefix",
                raw.embedding.query_prefix,
                Some("JSCOUT_QUERY_PREFIX"),
            ),
            batch: resolver.usize("embedding.batch", raw.embedding.batch, None, 64)?,
            origins: embed_origins,
        };
        validate_optional_nonempty("embedding.model", embedding.model.as_deref())?;
        validate_optional_nonempty("embedding.revision", embedding.revision.as_deref())?;
        validate_optional_nonempty("embedding.api_key_env", embedding.api_key_env.as_deref())?;

        let inference_host = resolver.string(
            "inference.host",
            raw.inference.host,
            Some("JSCOUT_INFERENCE_HOST"),
            "127.0.0.1",
        );
        validate_nonempty("inference.host", &inference_host)?;
        let inference_port = resolve_port(&mut resolver, raw.inference.port)?;
        let derived_url = client_url(&inference_host, inference_port);
        let inference_url = resolver.string(
            "inference.url",
            raw.inference.url,
            Some("JSCOUT_INFERENCE_URL"),
            &derived_url,
        );
        validate_endpoint("inference.url", &inference_url)?;
        let inference_project = resolver
            .optional_string(
                "inference.project",
                raw.inference.project,
                Some("JSCOUT_INFERENCE_PROJECT"),
            )
            .map(|value| resolve_path(root.as_deref(), &value, false))
            .transpose()?;
        let model_cache_root = resolver
            .optional_string(
                "inference.model_cache_root",
                raw.inference.model_cache_root,
                Some("JSCOUT_MODEL_CACHE_ROOT"),
            )
            .map(|value| resolve_path(root.as_deref(), &value, true))
            .transpose()?;
        let inference_allow_remote = resolver.bool(
            "inference.allow_remote",
            raw.inference.allow_remote,
            Some("JSCOUT_INFERENCE_ALLOW_REMOTE"),
            false,
        )?;
        if resolver.sources["inference.host"] == ValueSource::Config
            && !inference_allow_remote
            && !matches!(inference_host.as_str(), "127.0.0.1" | "localhost" | "::1")
        {
            bail!(
                "inference.host is non-loopback; set inference.allow_remote = true only on a trusted network"
            );
        }
        let inference_uv =
            resolver.string("inference.uv", raw.inference.uv, Some("JSCOUT_UV"), "uv");
        validate_nonempty("inference.uv", &inference_uv)?;
        let inference = InferenceSettings {
            url: inference_url,
            host: inference_host,
            port: inference_port,
            project: inference_project,
            uv: inference_uv,
            allow_remote: inference_allow_remote,
            batch_size: resolver.usize(
                "inference.batch_size",
                raw.inference.batch_size,
                Some("JSCOUT_INFERENCE_BATCH_SIZE"),
                16,
            )?,
            max_length: resolver.usize(
                "inference.max_length",
                raw.inference.max_length,
                Some("JSCOUT_INFERENCE_MAX_LENGTH"),
                4096,
            )?,
            model_cache_root,
        };

        let reranker_url =
            resolver.optional_string("reranker.url", raw.reranker.url, Some("JSCOUT_RERANK_URL"));
        validate_optional_endpoint("reranker.url", reranker_url.as_deref())?;
        let mut reranker_top = resolver.usize(
            "reranker.top",
            raw.reranker.top,
            Some("JSCOUT_RERANK_TOP"),
            50,
        )?;
        if reranker_top > 100 {
            if resolver.sources["reranker.top"] == ValueSource::Config {
                bail!("reranker.top must be at most 100");
            }
            // Preserve the legacy environment surface's historical clamp.
            reranker_top = 100;
        }
        let reranker = RerankerSettings {
            url: reranker_url,
            model: resolver.string(
                "reranker.model",
                raw.reranker.model,
                Some("JSCOUT_RERANK_MODEL"),
                "BAAI/bge-reranker-v2-m3",
            ),
            revision: resolver.optional_string(
                "reranker.revision",
                raw.reranker.revision,
                Some("JSCOUT_RERANK_REVISION"),
            ),
            top: reranker_top,
            max_chars: resolver.usize(
                "reranker.max_chars",
                raw.reranker.max_chars,
                Some("JSCOUT_RERANK_CHARS"),
                4000,
            )?,
        };
        validate_nonempty("reranker.model", &reranker.model)?;
        validate_optional_nonempty("reranker.revision", reranker.revision.as_deref())?;

        let openai_base_url = resolver.optional_string(
            "llm.openai_base_url",
            raw.llm.openai_base_url,
            Some("JSCOUT_PI_AI_OPENAI_BASE_URL"),
        );
        validate_optional_endpoint("llm.openai_base_url", openai_base_url.as_deref())?;
        let auth_file_text = resolver.string(
            "llm.auth_file",
            raw.llm.auth_file,
            Some("JSCOUT_PI_AI_AUTH_FILE"),
            "~/.pi-ai/auth.json",
        );
        let compatible_providers =
            resolve_compatible_providers(&mut resolver, raw.llm.openai_compatible_providers)?;
        let llm = LlmSettings {
            model: resolver.string(
                "llm.model",
                raw.llm.model,
                Some("JSCOUT_LLM_MODEL"),
                crate::llm::config::DEFAULT_MODEL,
            ),
            reasoning: resolver.optional_string(
                "llm.reasoning",
                raw.llm.reasoning,
                Some("JSCOUT_LLM_REASONING"),
            ),
            max_concurrency: resolver.usize(
                "llm.max_concurrency",
                raw.llm.max_concurrency,
                None,
                1,
            )?,
            openai_base_url,
            api_key_env: resolver.string(
                "llm.api_key_env",
                raw.llm.api_key_env,
                None,
                "OPENAI_API_KEY",
            ),
            auth_file: resolve_path(root.as_deref(), &auth_file_text, true)?,
            openai_compatible_providers: compatible_providers,
        };
        validate_nonempty("llm.model", &llm.model)?;
        validate_optional_nonempty("llm.reasoning", llm.reasoning.as_deref())?;
        validate_nonempty("llm.api_key_env", &llm.api_key_env)?;

        let gateway = resolver
            .optional_string_with_internal(
                "sidecars.gateway",
                raw.sidecars.gateway,
                "JSCOUT_PI_AI_GATEWAY",
                "JSCOUT_BUNDLED_GATEWAY",
            )
            .map(|value| resolve_path(root.as_deref(), &value, true))
            .transpose()?;
        let checker = resolver
            .optional_string_with_internal(
                "sidecars.checker",
                raw.sidecars.checker,
                "JSCOUT_CHECKER_SIDECAR",
                "JSCOUT_BUNDLED_CHECKER",
            )
            .map(|value| resolve_path(root.as_deref(), &value, true))
            .transpose()?;
        validate_optional_file("sidecars.gateway", gateway.as_deref())?;
        validate_optional_file("sidecars.checker", checker.as_deref())?;
        let sidecars = SidecarSettings {
            node: resolver.string(
                "sidecars.node",
                raw.sidecars.node,
                Some("JSCOUT_NODE"),
                "node",
            ),
            gateway,
            checker,
        };
        validate_nonempty("sidecars.node", &sidecars.node)?;

        let profile = resolver.string("mcp.profile", raw.mcp.profile, None, "structural");
        if !matches!(profile.as_str(), "baseline" | "structural") {
            bail!("mcp.profile must be baseline or structural");
        }
        let source_view = resolver.string("mcp.source_view", raw.mcp.source_view, None, "full");
        if !matches!(source_view.as_str(), "full" | "elided") {
            bail!("mcp.source_view must be full or elided");
        }
        let result_transport = resolver.string(
            "mcp.result_transport",
            raw.mcp.result_transport,
            None,
            "auto",
        );
        if !matches!(result_transport.as_str(), "auto" | "text" | "structured") {
            bail!("mcp.result_transport must be auto, text, or structured");
        }
        let mcp = McpSettings {
            profile,
            source_view,
            result_transport,
        };

        let telemetry = TelemetrySettings {
            file: resolver
                .optional_string(
                    "telemetry.file",
                    raw.telemetry.file,
                    Some("JSCOUT_TELEMETRY_FILE"),
                )
                .map(|value| resolve_path(root.as_deref(), &value, false))
                .transpose()?,
            request_log: resolver
                .optional_string("telemetry.request_log", raw.telemetry.request_log, None)
                .map(|value| resolve_path(root.as_deref(), &value, false))
                .transpose()?,
        };
        let diagnostics = DiagnosticsSettings {
            timing: resolver.bool(
                "diagnostics.timing",
                raw.diagnostics.timing,
                Some("JSCOUT_TIMING"),
                false,
            )?,
            debug: resolver.bool(
                "diagnostics.debug",
                raw.diagnostics.debug,
                Some("JSCOUT_DEBUG"),
                false,
            )?,
        };
        let index = IndexSettings {
            dependencies: resolver.configured_or(
                "index.dependencies",
                raw.index.dependencies,
                Vec::new(),
            ),
        };
        validate_string_list("index.dependencies", &index.dependencies)?;
        let watch_dependencies =
            resolver.configured_or("watch.dependencies", raw.watch.dependencies, Vec::new());
        let watch = WatchSettings {
            embed: resolver.bool("watch.embed", raw.watch.embed, None, false)?,
            product: resolver.bool("watch.product", raw.watch.product, None, false)?,
            dependencies: watch_dependencies,
            enrich: resolver.bool("watch.enrich", raw.watch.enrich, None, false)?,
            enrich_timeout_seconds: resolver.configured_or(
                "watch.enrich_timeout_seconds",
                raw.watch.enrich_timeout_seconds,
                300,
            ),
            debounce_ms: resolver.configured_or("watch.debounce_ms", raw.watch.debounce_ms, 2_000),
            reconcile_seconds: resolver.configured_or(
                "watch.reconcile_seconds",
                raw.watch.reconcile_seconds,
                600,
            ),
        };
        validate_string_list("watch.dependencies", &watch.dependencies)?;
        if watch.product && !watch.embed {
            bail!("watch.product requires watch.embed = true");
        }
        if watch.enrich_timeout_seconds == 0 {
            bail!("watch.enrich_timeout_seconds must be greater than zero");
        }
        if watch.debounce_ms == 0 {
            bail!("watch.debounce_ms must be greater than zero");
        }
        if watch.reconcile_seconds != 0
            && watch.reconcile_seconds.saturating_mul(1000) <= watch.debounce_ms
        {
            bail!("watch.reconcile_seconds must exceed watch.debounce_ms or be zero");
        }

        let effective = EffectiveConfig {
            database: DatabaseSettings {
                path: database_path,
            },
            docs,
            search,
            embedding,
            inference,
            reranker,
            llm,
            sidecars,
            mcp,
            telemetry,
            diagnostics,
            index,
            watch,
        };
        let encoded = serde_json::to_vec(&effective)?;
        let fingerprint = blake3::hash(&encoded).to_hex().to_string();
        Ok(Self {
            root,
            config_path,
            config_loaded: loaded,
            config_explicit: explicit_path.is_some(),
            fingerprint,
            effective,
            sources: resolver.sources,
        })
    }
}

pub fn init(root: &Path, explicit_path: Option<&Path>) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    let path = match explicit_path {
        Some(path) => absolute_from_cwd(path)?,
        None => root.join(FILE_NAME),
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| {
            format!(
                "create configuration {} (refusing to overwrite)",
                path.display()
            )
        })?;
    file.write_all(TEMPLATE.as_bytes())?;
    file.flush()?;
    Ok(path)
}

pub(super) fn source_name(source: ValueSource) -> &'static str {
    match source {
        ValueSource::Config => "config",
        ValueSource::LegacyEnv => "legacy-env",
        ValueSource::Builtin => "builtin",
    }
}

pub(super) fn display_optional_path(path: Option<&Path>) -> String {
    path.map_or_else(
        || "<disabled>".to_string(),
        |path| path.display().to_string(),
    )
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be true/false, yes/no, on/off, or 1/0"),
    }
}

fn positive(name: &str, value: usize) -> Result<usize> {
    if value == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(value)
}

fn validate_nonempty(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(())
}

fn validate_optional_nonempty(name: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_nonempty(name, value)?;
    }
    Ok(())
}

fn validate_string_list(name: &str, values: &[String]) -> Result<()> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_nonempty(name, value)?;
        if !unique.insert(value) {
            bail!("{name} contains duplicate value `{value}`");
        }
    }
    Ok(())
}

fn resolve_port(resolver: &mut Resolver, configured: Option<u16>) -> Result<u16> {
    if let Some(value) = configured {
        if value == 0 {
            bail!("inference.port must be greater than zero");
        }
        resolver
            .sources
            .insert("inference.port".to_string(), ValueSource::Config);
        return Ok(value);
    }
    if let Some(value) = nonempty_env("JSCOUT_INFERENCE_PORT") {
        let parsed = value
            .parse::<u16>()
            .with_context(|| "JSCOUT_INFERENCE_PORT must be between 1 and 65535")?;
        if parsed == 0 {
            bail!("JSCOUT_INFERENCE_PORT must be between 1 and 65535");
        }
        resolver
            .sources
            .insert("inference.port".to_string(), ValueSource::LegacyEnv);
        return Ok(parsed);
    }
    resolver
        .sources
        .insert("inference.port".to_string(), ValueSource::Builtin);
    Ok(8792)
}

fn normalize_provider(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "" | "none" => Ok(None),
        "local" | "voyage" | "openai" => Ok(Some(value)),
        _ => bail!("embedding.provider must be local, voyage, openai, or none"),
    }
}

fn validate_endpoint(name: &str, value: &str) -> Result<()> {
    if value.contains('?') || value.contains('#') {
        bail!("{name} cannot contain a query string or fragment");
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        bail!("{name} must be an absolute http(s) URL");
    };
    if !matches!(scheme, "http" | "https") {
        bail!("{name} must use http or https");
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        bail!("{name} must have a host and cannot contain credentials");
    }
    Ok(())
}

fn validate_optional_endpoint(name: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_endpoint(name, value)?;
    }
    Ok(())
}

fn validate_optional_file(name: &str, path: Option<&Path>) -> Result<()> {
    if let Some(path) = path
        && !path.is_file()
    {
        bail!("{name} does not name an existing file: {}", path.display());
    }
    Ok(())
}

fn client_url(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "::" => "[::1]".to_string(),
        value if value.contains(':') && !value.starts_with('[') => format!("[{value}]"),
        value => value.to_string(),
    };
    format!("http://{host}:{port}")
}

fn resolve_path(root: Option<&Path>, value: &str, allow_tilde: bool) -> Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        bail!("configured path must not be empty");
    }
    if allow_tilde && (value == "~" || value.starts_with("~/")) {
        let home =
            env::var_os("HOME").context("HOME is not set; cannot expand configured ~/ path")?;
        let mut path = PathBuf::from(home);
        if value.len() > 2 {
            path.push(&value[2..]);
        }
        return Ok(path);
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    let root = root.context("relative configuration paths require a repository root")?;
    Ok(root.join(path))
}

fn absolute_from_cwd(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(env::current_dir()?.join(path))
}

fn resolve_compatible_providers(
    resolver: &mut Resolver,
    configured: Option<Vec<OpenAiCompatibleProviderFileConfig>>,
) -> Result<Vec<OpenAiCompatibleProvider>> {
    let (providers, source) = if let Some(configured) = configured {
        (
            normalize_compatible_provider_files(configured),
            ValueSource::Config,
        )
    } else if let Some(value) = nonempty_env("JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS") {
        (
            parse_legacy_compatible_providers(&value)?,
            ValueSource::LegacyEnv,
        )
    } else {
        (Vec::new(), ValueSource::Builtin)
    };
    resolver
        .sources
        .insert("llm.openai_compatible_providers".to_string(), source);
    validate_compatible_providers(&providers)?;
    Ok(providers)
}

pub(super) fn parse_legacy_compatible_providers(
    value: &str,
) -> Result<Vec<OpenAiCompatibleProvider>> {
    let legacy: Vec<OpenAiCompatibleProviderFileConfig> = serde_json::from_str(value)
        .context("JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS must contain the provider JSON array")?;
    Ok(normalize_compatible_provider_files(legacy))
}

fn normalize_compatible_provider_files(
    providers: Vec<OpenAiCompatibleProviderFileConfig>,
) -> Vec<OpenAiCompatibleProvider> {
    providers
        .into_iter()
        .map(|provider| OpenAiCompatibleProvider {
            name: provider.name.unwrap_or_else(|| provider.id.clone()),
            id: provider.id,
            base_url: provider.base_url,
            api_key_env: provider.api_key_env,
            models: provider
                .models
                .into_iter()
                .map(|model| OpenAiCompatibleModel {
                    name: model.name.unwrap_or_else(|| model.id.clone()),
                    id: model.id,
                    reasoning: model.reasoning.unwrap_or(false),
                    context_window: model.context_window.unwrap_or(131_072),
                    max_tokens: model.max_tokens.unwrap_or(32_768),
                })
                .collect(),
        })
        .collect()
}

fn validate_compatible_providers(providers: &[OpenAiCompatibleProvider]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for provider in providers {
        if provider.id.trim().is_empty() || provider.name.trim().is_empty() {
            bail!("llm.openai_compatible_providers id and name must not be empty");
        }
        if !ids.insert(provider.id.as_str()) {
            bail!(
                "duplicate llm.openai_compatible_providers id `{}`",
                provider.id
            );
        }
        validate_endpoint(
            "llm.openai_compatible_providers.base_url",
            &provider.base_url,
        )?;
        if provider
            .api_key_env
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("compatible-provider api_key_env must not be empty");
        }
        if provider.models.is_empty() {
            bail!(
                "llm.openai_compatible_providers `{}` must declare at least one model",
                provider.id
            );
        }
        let mut model_ids = BTreeSet::new();
        for model in &provider.models {
            if model.id.trim().is_empty() || model.name.trim().is_empty() {
                bail!("compatible-provider model id and name must not be empty");
            }
            if model.context_window == 0 || model.max_tokens == 0 {
                bail!("compatible-provider model limits must be greater than zero");
            }
            if !model_ids.insert(model.id.as_str()) {
                bail!(
                    "duplicate model id `{}` in compatible provider `{}`",
                    model.id,
                    provider.id
                );
            }
        }
    }
    Ok(())
}
