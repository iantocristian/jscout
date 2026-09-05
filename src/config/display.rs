use anyhow::Result;

use super::load::source_name;
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
        ];
        // Loading already serializes this non-secret value to fingerprint it.
        // Reuse that representation instead of maintaining a partial second
        // list of settings here. Collections remain one policy value, matching
        // their source entry (including custom model-provider definitions).
        let effective = serde_json::to_value(&self.effective)
            .expect("loaded runtime configuration is serializable");
        for (key, source) in &self.sources {
            let value = key.split('.').fold(&effective, |value, part| &value[part]);
            lines.push(format!("{key} = {value} [{}]", source_name(*source)));
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
