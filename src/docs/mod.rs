pub mod corpus;
pub mod freshness;
pub(crate) mod provenance;
pub(crate) mod provenance_store;
pub mod retrieval;
pub mod store;

/// Default documentation admission covers the two inert Markdown-family
/// formats supported by the shared documentation parser.
pub const DEFAULT_INCLUDE_GLOBS: &[&str] = &["**/*.md", "**/*.mdx"];

/// Initial parser, rendering, and admission contract shared by lexical and
/// vector documentation retrieval.
pub const CHUNK_FORMAT_VERSION: &str = "documentation-v1";

/// Git-attribution and per-chunk provenance projection contract.
pub const PROVENANCE_FORMAT_VERSION: &str = "documentation-provenance-v2";

pub fn default_include_globs() -> Vec<String> {
    DEFAULT_INCLUDE_GLOBS
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect()
}
