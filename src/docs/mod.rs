pub mod corpus;
pub mod retrieval;
pub mod store;

/// Initial parser, rendering, and admission contract shared by lexical and
/// vector documentation retrieval.
pub const CHUNK_FORMAT_VERSION: &str = "documentation-v1";

pub fn default_include_globs() -> Vec<String> {
    crate::formats::ALL
        .iter()
        .filter(|format| format.documentation())
        .flat_map(|format| format.extensions)
        .map(|extension| format!("**/*.{extension}"))
        .collect()
}
