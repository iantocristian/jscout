use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{file_role, origin, search, store};

pub const FILE_NAME: &str = ".jscout.toml";
pub const SCHEMA_VERSION: u32 = 1;

pub const TEMPLATE: &str = include_str!("../.jscout.toml.example");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueSource {
    Config,
    LegacyEnv,
    Builtin,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeConfig {
    pub root: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub config_loaded: bool,
    pub config_explicit: bool,
    pub fingerprint: String,
    pub effective: EffectiveConfig,
    pub sources: BTreeMap<String, ValueSource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveConfig {
    pub database: DatabaseSettings,
    pub search: SearchSettings,
    pub embedding: EmbeddingSettings,
    pub inference: InferenceSettings,
    pub reranker: RerankerSettings,
    pub llm: LlmSettings,
    pub sidecars: SidecarSettings,
    pub mcp: McpSettings,
    pub telemetry: TelemetrySettings,
    pub diagnostics: DiagnosticsSettings,
    pub index: IndexSettings,
    pub watch: WatchSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseSettings {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchSettings {
    pub vector: bool,
    pub rerank: bool,
    pub attach_memory: bool,
    pub limit: usize,
    pub response_bytes: usize,
    pub file_roles: Vec<String>,
    pub origins: Vec<String>,
    pub memory_limit: usize,
    pub memory_depth: usize,
    pub memory_nodes: usize,
    pub expansion: ExpansionSettings,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            vector: true,
            rerank: true,
            attach_memory: true,
            limit: search::DEFAULT_RESULT_LIMIT,
            response_bytes: search::DEFAULT_RESPONSE_BYTE_LIMIT,
            file_roles: Vec::new(),
            origins: origin::defaults(),
            memory_limit: 4,
            memory_depth: search::DEFAULT_MEMORY_GRAPH_DEPTH,
            memory_nodes: search::DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            expansion: ExpansionSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpansionSettings {
    pub enabled: bool,
    pub depth: usize,
    pub seeds: usize,
    pub nodes: usize,
    pub edges: usize,
    pub bytes: usize,
    pub min_confidence: String,
    pub file_roles: Vec<String>,
}

impl Default for ExpansionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            depth: 1,
            seeds: 3,
            nodes: 40,
            edges: 120,
            bytes: 24_000,
            min_confidence: "likely".to_string(),
            file_roles: file_role::DEFAULT_EXPANSION
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub revision: Option<String>,
    pub url: Option<String>,
    pub api_key_env: Option<String>,
    pub query_prefix: Option<String>,
    pub batch: usize,
    pub origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InferenceSettings {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub project: Option<PathBuf>,
    pub uv: String,
    pub allow_remote: bool,
    pub batch_size: usize,
    pub max_length: usize,
    pub model_cache_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RerankerSettings {
    pub url: Option<String>,
    pub model: String,
    pub revision: Option<String>,
    pub top: usize,
    pub max_chars: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmSettings {
    pub model: String,
    pub reasoning: Option<String>,
    pub openai_base_url: Option<String>,
    pub api_key_env: String,
    pub auth_file: PathBuf,
    pub openai_compatible_providers: Vec<OpenAiCompatibleProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleProvider {
    pub id: String,
    pub name: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(rename = "apiKeyEnv", skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    pub models: Vec<OpenAiCompatibleModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    #[serde(rename = "contextWindow")]
    pub context_window: usize,
    #[serde(rename = "maxTokens")]
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SidecarSettings {
    pub node: String,
    pub gateway: Option<PathBuf>,
    pub checker: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpSettings {
    pub profile: String,
    pub source_view: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySettings {
    pub file: Option<PathBuf>,
    pub request_log: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsSettings {
    pub timing: bool,
    pub debug: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexSettings {
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchSettings {
    pub embed: bool,
    pub product: bool,
    pub dependencies: Vec<String>,
    pub enrich: bool,
    pub enrich_timeout_seconds: u64,
    pub debounce_ms: u64,
    pub reconcile_seconds: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    version: u32,
    #[serde(default)]
    database: DatabaseFileConfig,
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
    depth: Option<usize>,
    seeds: Option<usize>,
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
    base_url: String,
    api_key_env: Option<String>,
    models: Vec<OpenAiCompatibleModelFileConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleModelFileConfig {
    id: String,
    name: Option<String>,
    reasoning: Option<bool>,
    context_window: Option<usize>,
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
        let search = SearchSettings {
            vector: resolver.bool("search.vector", raw.search.vector, None, true)?,
            rerank: resolver.bool("search.rerank", raw.search.rerank, None, true)?,
            attach_memory: resolver.bool(
                "search.attach_memory",
                raw.search.attach_memory,
                None,
                true,
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

        let inference_host = resolver.string(
            "inference.host",
            raw.inference.host,
            Some("JSCOUT_INFERENCE_HOST"),
            "127.0.0.1",
        );
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
        if !inference_allow_remote
            && !matches!(inference_host.as_str(), "127.0.0.1" | "localhost" | "::1")
        {
            bail!(
                "inference.host is non-loopback; set inference.allow_remote = true only on a trusted network"
            );
        }
        let inference = InferenceSettings {
            url: inference_url,
            host: inference_host,
            port: inference_port,
            project: inference_project,
            uv: resolver.string("inference.uv", raw.inference.uv, Some("JSCOUT_UV"), "uv"),
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
            top: resolver
                .usize(
                    "reranker.top",
                    raw.reranker.top,
                    Some("JSCOUT_RERANK_TOP"),
                    50,
                )?
                .min(100),
            max_chars: resolver.usize(
                "reranker.max_chars",
                raw.reranker.max_chars,
                Some("JSCOUT_RERANK_CHARS"),
                4000,
            )?,
        };

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

        let gateway = resolver
            .optional_string(
                "sidecars.gateway",
                raw.sidecars.gateway,
                Some("JSCOUT_PI_AI_GATEWAY"),
            )
            .map(|value| resolve_path(root.as_deref(), &value, true))
            .transpose()?;
        let checker = resolver
            .optional_string(
                "sidecars.checker",
                raw.sidecars.checker,
                Some("JSCOUT_CHECKER_SIDECAR"),
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

        let profile = resolver.string("mcp.profile", raw.mcp.profile, None, "structural");
        if !matches!(profile.as_str(), "baseline" | "structural") {
            bail!("mcp.profile must be baseline or structural");
        }
        let source_view = resolver.string("mcp.source_view", raw.mcp.source_view, None, "full");
        if !matches!(source_view.as_str(), "full" | "elided") {
            bail!("mcp.source_view must be full or elided");
        }
        let mcp = McpSettings {
            profile,
            source_view,
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
        if watch.product && !watch.embed {
            bail!("watch.product requires watch.embed = true");
        }
        if watch.enrich && watch.enrich_timeout_seconds == 0 {
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

    pub fn show_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn show_text(&self) -> String {
        let root = self
            .root
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let path = self
            .config_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let mut lines = vec![
            format!("root: {root}"),
            format!(
                "config: {path} ({})",
                if self.config_loaded {
                    "loaded"
                } else {
                    "not found; using fallbacks"
                }
            ),
            format!("fingerprint: {}", self.fingerprint),
            format!("database: {}", self.effective.database.path.display()),
            format!(
                "search: vector={} rerank={} memory={} expand={} limit={} response_bytes={}",
                self.effective.search.vector,
                self.effective.search.rerank,
                self.effective.search.attach_memory,
                self.effective.search.expansion.enabled,
                self.effective.search.limit,
                self.effective.search.response_bytes
            ),
            format!(
                "embedding: provider={} model={}",
                self.effective
                    .embedding
                    .provider
                    .as_deref()
                    .unwrap_or("none"),
                self.effective
                    .embedding
                    .model
                    .as_deref()
                    .unwrap_or("<none>")
            ),
            format!(
                "reranker: endpoint={} model={}",
                self.effective.reranker.url.as_deref().unwrap_or_else(|| {
                    if self.effective.embedding.provider.as_deref() == Some("local") {
                        "<local inference default>"
                    } else {
                        "<none>"
                    }
                }),
                self.effective.reranker.model
            ),
            format!(
                "llm: model={} reasoning={} base_url={}",
                self.effective.llm.model,
                self.effective
                    .llm
                    .reasoning
                    .as_deref()
                    .unwrap_or("<provider default>"),
                self.effective
                    .llm
                    .openai_base_url
                    .as_deref()
                    .unwrap_or("<provider default>")
            ),
            format!(
                "mcp: profile={} source_view={} telemetry={} request_log={}",
                self.effective.mcp.profile,
                self.effective.mcp.source_view,
                display_optional_path(self.effective.telemetry.file.as_deref()),
                display_optional_path(self.effective.telemetry.request_log.as_deref())
            ),
            "sources:".to_string(),
        ];
        for (key, source) in &self.sources {
            lines.push(format!("  {key}: {}", source_name(*source)));
        }
        lines.join("\n")
    }

    /// Stable non-secret settings still supplied by the compatibility
    /// environment surface. Secret variables are resolved separately and are
    /// deliberately absent from `sources`.
    pub fn legacy_environment_keys(&self) -> Vec<&str> {
        self.sources
            .iter()
            .filter_map(|(key, source)| (*source == ValueSource::LegacyEnv).then_some(key.as_str()))
            .collect()
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

fn source_name(source: ValueSource) -> &'static str {
    match source {
        ValueSource::Config => "config",
        ValueSource::LegacyEnv => "legacy-env",
        ValueSource::Builtin => "builtin",
    }
}

fn display_optional_path(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| "<disabled>".to_string())
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
        let providers = configured
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
            .collect();
        (providers, ValueSource::Config)
    } else if let Some(value) = nonempty_env("JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS") {
        let legacy: Vec<OpenAiCompatibleProvider> = serde_json::from_str(&value).context(
            "JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS must contain the provider JSON array",
        )?;
        (legacy, ValueSource::LegacyEnv)
    } else {
        (Vec::new(), ValueSource::Builtin)
    };
    resolver
        .sources
        .insert("llm.openai_compatible_providers".to_string(), source);
    validate_compatible_providers(&providers)?;
    Ok(providers)
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

#[cfg(test)]
mod tests {
    use super::{FILE_NAME, RuntimeConfig, SCHEMA_VERSION, TEMPLATE, ValueSource, init};
    use std::path::Path;

    fn write_config(root: &Path, text: &str) -> anyhow::Result<()> {
        std::fs::write(root.join(FILE_NAME), text)?;
        Ok(())
    }

    #[test]
    fn absent_file_preserves_current_search_and_database_defaults() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let config = RuntimeConfig::load(Some(root.path()), None)?;
        assert!(!config.config_loaded);
        assert_eq!(
            config.effective.database.path,
            root.path().canonicalize()?.join(".jscout.db")
        );
        assert!(config.effective.search.vector);
        assert!(config.effective.search.rerank);
        assert!(config.effective.search.attach_memory);
        assert_eq!(config.sources["search.rerank"], ValueSource::Builtin);
        Ok(())
    }

    #[test]
    fn repository_config_resolves_paths_and_records_sources() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        write_config(
            root.path(),
            r#"version = 1
[database]
path = "state/index.db"
[search]
rerank = false
attach_memory = false
[telemetry]
file = "logs/mcp.jsonl"
"#,
        )?;
        let config = RuntimeConfig::load(Some(root.path()), None)?;
        assert!(config.config_loaded);
        assert_eq!(
            config.effective.database.path,
            root.path().canonicalize()?.join("state/index.db")
        );
        assert_eq!(
            config.effective.telemetry.file,
            Some(root.path().canonicalize()?.join("logs/mcp.jsonl"))
        );
        assert!(!config.effective.search.rerank);
        assert!(!config.effective.search.attach_memory);
        assert_eq!(config.sources["search.rerank"], ValueSource::Config);
        Ok(())
    }

    #[test]
    fn repository_roots_resolve_independent_database_and_provider_policy() -> anyhow::Result<()> {
        let first_root = tempfile::tempdir()?;
        let second_root = tempfile::tempdir()?;
        write_config(
            first_root.path(),
            "version = 1\n[database]\npath = \"state/first.db\"\n[search]\nrerank = false\n",
        )?;
        write_config(
            second_root.path(),
            "version = 1\n[database]\npath = \"state/second.db\"\n[search]\nrerank = true\n",
        )?;

        let first = RuntimeConfig::load(Some(first_root.path()), None)?;
        let second = RuntimeConfig::load(Some(second_root.path()), None)?;
        assert_ne!(
            first.effective.database.path,
            second.effective.database.path
        );
        assert!(!first.effective.search.rerank);
        assert!(second.effective.search.rerank);
        assert_ne!(first.fingerprint, second.fingerprint);
        Ok(())
    }

    #[test]
    fn unknown_fields_and_versions_fail_with_the_configuration_path() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        write_config(root.path(), "version = 1\n[search]\nrerak = false\n")?;
        let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
        assert!(error.to_string().contains(FILE_NAME));

        write_config(root.path(), "version = 99\n")?;
        let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported jscout configuration version")
        );
        Ok(())
    }

    #[test]
    fn unsafe_endpoints_and_remote_binds_fail_during_configuration_load() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        write_config(
            root.path(),
            "version = 1\n[inference]\nurl = \"https://example.test/v1?token=x\"\n",
        )?;
        assert!(
            RuntimeConfig::load(Some(root.path()), None)
                .unwrap_err()
                .to_string()
                .contains("query string or fragment")
        );

        write_config(
            root.path(),
            "version = 1\n[inference]\nhost = \"0.0.0.0\"\n",
        )?;
        assert!(
            RuntimeConfig::load(Some(root.path()), None)
                .unwrap_err()
                .to_string()
                .contains("inference.allow_remote")
        );
        Ok(())
    }

    #[test]
    fn fingerprint_changes_with_runtime_policy_not_source_labels() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        write_config(root.path(), "version = 1\n[search]\nrerank = false\n")?;
        let first = RuntimeConfig::load(Some(root.path()), None)?;
        write_config(root.path(), "version = 1\n[search]\nrerank = true\n")?;
        let second = RuntimeConfig::load(Some(root.path()), None)?;
        assert_ne!(first.fingerprint, second.fingerprint);
        Ok(())
    }

    #[test]
    fn init_refuses_to_overwrite_and_emits_the_current_schema() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let path = init(root.path(), None)?;
        assert_eq!(path, root.path().canonicalize()?.join(FILE_NAME));
        assert!(std::fs::read_to_string(&path)?.contains(&format!("version = {SCHEMA_VERSION}")));
        let loaded = RuntimeConfig::load(Some(root.path()), None)?;
        assert!(loaded.config_loaded);
        assert_eq!(loaded.effective.mcp.profile, "structural");
        assert!(init(root.path(), None).is_err());
        assert!(TEMPLATE.contains("rerank = true"));
        Ok(())
    }

    #[test]
    fn typed_compatible_providers_keep_only_secret_variable_names() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        write_config(
            root.path(),
            r#"version = 1
[llm]
openai_compatible_providers = []
"#,
        )?;
        let empty = RuntimeConfig::load(Some(root.path()), None)?;
        assert!(empty.effective.llm.openai_compatible_providers.is_empty());
        assert_eq!(
            empty.sources["llm.openai_compatible_providers"],
            ValueSource::Config
        );

        write_config(
            root.path(),
            r#"version = 1
[[llm.openai_compatible_providers]]
id = "private"
base_url = "https://models.example.test/v1"
api_key_env = "PRIVATE_MODEL_KEY"

[[llm.openai_compatible_providers.models]]
id = "model-1"
reasoning = true
context_window = 100000
max_tokens = 10000
"#,
        )?;
        let configured = RuntimeConfig::load(Some(root.path()), None)?;
        let provider = &configured.effective.llm.openai_compatible_providers[0];
        assert_eq!(provider.name, "private");
        assert_eq!(provider.api_key_env.as_deref(), Some("PRIVATE_MODEL_KEY"));
        Ok(())
    }
}
