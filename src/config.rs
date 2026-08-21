mod display;
mod load;
mod model;

pub use load::init;
pub use model::{
    DatabaseSettings, DiagnosticsSettings, EffectiveConfig, EmbeddingSettings, ExpansionSettings,
    IndexSettings, InferenceSettings, LlmSettings, McpSettings, OpenAiCompatibleModel,
    OpenAiCompatibleProvider, RerankerSettings, RuntimeConfig, SearchSettings, SidecarSettings,
    TelemetrySettings, ValueSource, WatchSettings,
};

#[cfg(test)]
use load::parse_legacy_compatible_providers;

pub const FILE_NAME: &str = ".jscout.toml";
pub const SCHEMA_VERSION: u32 = 1;

pub const TEMPLATE: &str = include_str!("../.jscout.toml.example");

#[cfg(test)]
mod tests;
