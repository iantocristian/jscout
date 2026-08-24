use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{file_role, origin, search};

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
            attach_memory: false,
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
    pub mode: String,
    pub depth: usize,
    pub seeds: usize,
    pub paths: usize,
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
            mode: "paths".to_string(),
            depth: 1,
            seeds: 3,
            paths: search::DEFAULT_EXPANSION_PATH_LIMIT,
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
    /// Maximum number of scouting model requests allowed in flight. Model
    /// execution may overlap; validation and database publication do not.
    pub max_concurrency: usize,
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
    pub result_transport: String,
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
