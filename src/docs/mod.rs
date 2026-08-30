pub mod corpus;
pub mod freshness;
pub(crate) mod provenance;
pub(crate) mod provenance_store;
pub mod retrieval;
pub mod store;

/// Initial parser, rendering, and admission contract shared by lexical and
/// vector documentation retrieval.
pub const CHUNK_FORMAT_VERSION: &str = "documentation-v1";

/// Git-attribution and per-chunk provenance projection contract.
pub const PROVENANCE_FORMAT_VERSION: &str = "documentation-provenance-v2";

/// Published readiness marker for the optional documentation provenance
/// projection. Missing is treated like `false` so databases produced before
/// this marker fail closed when freshness ranking is requested.
pub(crate) const PROVENANCE_ENABLED_META_KEY: &str = "documentation_provenance_enabled";

/// Content identity of the published documentation-provenance plane. It is
/// folded into `meta.snapshot` but remains a separate invalidation identity,
/// so Git-history-only changes do not invalidate code or documentation gates.
pub(crate) const PROVENANCE_DIGEST_META_KEY: &str = "documentation_provenance_digest";

pub fn default_include_globs() -> Vec<String> {
    crate::formats::ALL
        .iter()
        .filter(|format| format.documentation())
        .flat_map(|format| format.extensions)
        .map(|extension| format!("**/*.{extension}"))
        .collect()
}
