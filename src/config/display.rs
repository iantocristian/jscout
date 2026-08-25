use anyhow::Result;

use super::load::{display_optional_path, source_name};
use super::{RuntimeConfig, ValueSource};

impl RuntimeConfig {
    pub fn show_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn show_text(&self) -> String {
        let root = self
            .root
            .as_deref()
            .map_or_else(|| "<none>".to_string(), |path| path.display().to_string());
        let path = self
            .config_path
            .as_deref()
            .map_or_else(|| "<none>".to_string(), |path| path.display().to_string());
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
                "docs: include={:?} exclude={:?}",
                self.effective.docs.include, self.effective.docs.exclude,
            ),
            format!(
                "docs-search: vector={} rerank={} limit={} response_bytes={}",
                self.effective.docs.search.vector,
                self.effective.docs.search.rerank,
                self.effective.docs.search.limit,
                self.effective.docs.search.response_bytes,
            ),
            format!(
                "search: vector={} rerank={} memory={} expand={} expansion_mode={} limit={} response_bytes={}",
                self.effective.search.vector,
                self.effective.search.rerank,
                self.effective.search.attach_memory,
                self.effective.search.expansion.enabled,
                self.effective.search.expansion.mode,
                self.effective.search.limit,
                self.effective.search.response_bytes
            ),
            format!(
                "embedding: provider={} model={} endpoint={}",
                self.effective
                    .embedding
                    .provider
                    .as_deref()
                    .unwrap_or("none"),
                self.effective
                    .embedding
                    .model
                    .as_deref()
                    .unwrap_or("<none>"),
                self.effective.embedding.url.as_deref().unwrap_or(
                    match self.effective.embedding.provider.as_deref() {
                        Some("local") => self.effective.inference.url.as_str(),
                        Some(_) => "<provider default>",
                        None => "<none>",
                    }
                )
            ),
            format!(
                "inference: client={} bind={}:{} allow_remote={}",
                self.effective.inference.url,
                self.effective.inference.host,
                self.effective.inference.port,
                self.effective.inference.allow_remote,
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
                "llm: model={} reasoning={} max_concurrency={} base_url={}",
                self.effective.llm.model,
                self.effective
                    .llm
                    .reasoning
                    .as_deref()
                    .unwrap_or("<provider default>"),
                self.effective.llm.max_concurrency,
                self.effective
                    .llm
                    .openai_base_url
                    .as_deref()
                    .unwrap_or("<provider default>")
            ),
            format!(
                "mcp: profile={} source_view={} result_transport={} telemetry={} request_log={}",
                self.effective.mcp.profile,
                self.effective.mcp.source_view,
                self.effective.mcp.result_transport,
                display_optional_path(self.effective.telemetry.file.as_deref()),
                display_optional_path(self.effective.telemetry.request_log.as_deref())
            ),
            format!(
                "sidecars: node={} gateway={} checker={}",
                self.effective.sidecars.node,
                display_optional_path(self.effective.sidecars.gateway.as_deref()),
                display_optional_path(self.effective.sidecars.checker.as_deref()),
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
