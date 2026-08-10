//! Deterministic non-symbol facts that join runtime workflows across dynamic
//! boundaries. Extraction records source-local sites first; resolution later
//! groups those sites under snapshot-canonical entities.

use oxc_ast::ast::Program;

#[derive(Debug, Clone)]
pub struct EntitySite {
    pub plane: &'static str,
    pub entity_type: &'static str,
    pub role: &'static str,
    pub identity_kind: &'static str,
    pub identity_name: String,
    pub identity_start: u32,
    pub target_name: Option<String>,
    pub target_start: Option<u32>,
    pub span_start: u32,
    pub span_end: u32,
    pub extractor: &'static str,
    pub provenance: &'static str,
    pub confidence: &'static str,
    pub detail: serde_json::Value,
}

/// Extract source-local entity evidence. Framework/runtime recognizers are
/// added as independent visitors; returning an empty set is the valid base
/// behavior for files without recognized boundaries.
pub fn extract(_program: &Program<'_>) -> Vec<EntitySite> {
    Vec::new()
}
