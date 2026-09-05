use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::{
    embed, file_role, origin,
    publication::{Identities, Plane},
    query, semantic, store, structural,
};

mod snippet;

pub(crate) type EdgeIdentity = (String, String, String, Option<String>, Option<i64>);

pub const DEFAULT_RESPONSE_BYTE_LIMIT: usize = 30_000;
pub const DEFAULT_RESULT_LIMIT: usize = 10;
pub const MAX_EXHAUSTIVE_PAGE_SIZE: usize = 200;
pub const DEFAULT_MEMORY_GRAPH_DEPTH: usize = 2;
pub const DEFAULT_MEMORY_GRAPH_NODE_LIMIT: usize = 2_000;
pub const MAX_MEMORY_GRAPH_DEPTH: usize = 8;
pub const MAX_MEMORY_GRAPH_NODE_LIMIT: usize = 20_000;
const DEFAULT_TOTAL_RENDERED_SUPPORT_LIMIT: usize = 8;
pub const DEFAULT_EXPANSION_PATH_LIMIT: usize = 8;
pub const MAX_EXPANSION_PATH_LIMIT: usize = 50;

pub(crate) fn resolve_search_limit(
    exhaustive: bool,
    requested: Option<usize>,
    configured: usize,
) -> usize {
    match requested {
        Some(limit) => limit,
        None if exhaustive => configured.min(MAX_EXHAUSTIVE_PAGE_SIZE),
        None => configured,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpansionProjection {
    Paths,
    Neighborhood,
}

impl ExpansionProjection {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "paths" => Ok(Self::Paths),
            "neighborhood" => Ok(Self::Neighborhood),
            _ => anyhow::bail!("expansion projection must be one of: paths, neighborhood"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Neighborhood => "neighborhood",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExpansionOptions {
    pub projection: ExpansionProjection,
    pub depth: usize,
    pub seed_limit: usize,
    pub path_limit: usize,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub byte_limit: usize,
    pub min_confidence: String,
    /// Eligible file-backed expansion roles. Empty means all roles.
    pub file_roles: Vec<String>,
    /// Eligible backing-file origins. Dependency files require explicit opt-in.
    pub file_origins: Vec<String>,
}

impl Default for ExpansionOptions {
    fn default() -> Self {
        Self {
            projection: ExpansionProjection::Paths,
            depth: 1,
            seed_limit: 3,
            path_limit: DEFAULT_EXPANSION_PATH_LIMIT,
            node_limit: 40,
            edge_limit: 120,
            byte_limit: 24_000,
            min_confidence: "likely".into(),
            file_roles: file_role::DEFAULT_EXPANSION
                .iter()
                .map(|role| (*role).to_string())
                .collect(),
            file_origins: origin::defaults(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub mode: SearchMode,
    pub limit: usize,
    pub expand: bool,
    /// Maximum bytes in the serialized JSON search envelope. This covers hits,
    /// expansion, metadata, and serialization overhead.
    pub response_byte_limit: usize,
    /// Optional role allowlist for primary hits. Empty preserves normal recall.
    pub file_roles: Vec<String>,
    /// Backing-file origin allowlist. Defaults to first-party origins.
    pub file_origins: Vec<String>,
    /// Code-format allowlist. Empty means every registered code format.
    pub formats: Vec<String>,
    pub include_memory: bool,
    pub memory_limit: usize,
    /// Maximum likely/certain structural hops used to connect a semantic
    /// support to a returned code hit.
    pub memory_graph_depth: usize,
    /// Maximum graph nodes visited while connecting memory to code hits.
    pub memory_graph_node_limit: usize,
    /// Apply the separately configured cross-encoder to the fused candidate
    /// pool. This is independent of whether vector retrieval is enabled.
    pub rerank: bool,
    /// Resolved reranker service and pool policy. `rerank` remains the
    /// per-request enable/disable switch.
    pub reranker: Option<Reranker>,
    /// Emit stage timing diagnostics to stderr.
    pub timing: bool,
    /// Budget and render the agent-facing compact transport rather than the
    /// diagnostic representation.
    pub compact: bool,
    pub expansion: ExpansionOptions,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            mode: SearchMode::Ranked,
            limit: DEFAULT_RESULT_LIMIT,
            expand: false,
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            formats: Vec::new(),
            include_memory: false,
            memory_limit: 4,
            memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
            memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            rerank: true,
            reranker: None,
            timing: false,
            compact: false,
            expansion: ExpansionOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SearchMode {
    #[default]
    Ranked,
    Exhaustive {
        cursor: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScopeFileRoles {
    All,
    Selected(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScopeFormats {
    All,
    Selected(Vec<String>),
}

impl serde::Serialize for SearchScopeFormats {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::All => serializer.serialize_str("all"),
            Self::Selected(formats) => serde::Serialize::serialize(formats, serializer),
        }
    }
}

impl serde::Serialize for SearchScopeFileRoles {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::All => serializer.serialize_str("all"),
            Self::Selected(roles) => serde::Serialize::serialize(roles, serializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SearchScope {
    pub corpus: &'static str,
    pub file_roles: SearchScopeFileRoles,
    pub origins: Vec<String>,
    pub formats: SearchScopeFormats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct EffectiveSearchPosture {
    pub vector: bool,
    pub rerank: bool,
    pub expand: bool,
    pub include_memory: bool,
    pub page_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExhaustiveSearchMetadata {
    pub total_chunks: usize,
    pub returned: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ExhaustiveSearchWarning>,
    pub effective: EffectiveSearchPosture,
    pub scope: SearchScope,
    #[serde(skip)]
    request_fingerprint: String,
    #[serde(skip)]
    selected_positions: Vec<ExhaustiveCursorPosition>,
    #[serde(skip)]
    page_had_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExhaustiveSearchWarning {
    pub code: &'static str,
    pub terms: Vec<String>,
    pub total_chunks: usize,
    pub message: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponseBudgetTooSmall {
    pub byte_limit: usize,
    pub minimum_bytes: usize,
}

impl std::fmt::Display for ResponseBudgetTooSmall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "response_budget_too_small: response byte limit {} cannot fit the minimum exhaustive response; minimum_bytes={}",
            self.byte_limit, self.minimum_bytes
        )
    }
}

impl std::error::Error for ResponseBudgetTooSmall {}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub snapshot: String,
    pub publication_snapshot: String,
    #[serde(flatten)]
    pub exhaustive: Option<ExhaustiveSearchMetadata>,
    pub retrieval: RetrievalStatus,
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub semantic_artifacts: Vec<semantic::SemanticArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_retrieval: Option<semantic::ArtifactRetrievalStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_attachment: Option<MemoryAttachmentStatus>,
    /// Ranked retrieval pool before the memory result limit. This is a
    /// candidate count, not a calibrated relevant-match count.
    pub semantic_candidates: usize,
    /// Memory previews selected before the whole-response byte budget.
    pub semantic_selected: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion: Option<SearchExpansion>,
    pub response_budget: ResponseBudget,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RetrievalStatus {
    pub lexical: &'static str,
    pub vector: &'static str,
    pub reranker: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_action: Option<&'static str>,
    #[serde(skip)]
    pub vector_timings: Option<embed::VectorSearchTimings>,
    #[serde(skip)]
    pub reranker_timing: Option<std::time::Duration>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryAttachmentStatus {
    pub status: &'static str,
    pub connected_candidates: usize,
    pub graph_depth: usize,
    pub graph_nodes: usize,
    pub graph_truncated: bool,
}

impl RetrievalStatus {
    pub(crate) fn vector_disabled() -> Self {
        Self {
            lexical: "active",
            vector: "disabled",
            reranker: "disabled",
            vector_action: None,
            vector_timings: None,
            reranker_timing: None,
        }
    }

    fn vector_active() -> Self {
        Self {
            lexical: "active",
            vector: "active",
            reranker: "disabled",
            vector_action: None,
            vector_timings: None,
            reranker_timing: None,
        }
    }

    fn vector_degraded(action: &'static str) -> Self {
        Self {
            lexical: "active",
            vector: "degraded",
            reranker: "disabled",
            vector_action: Some(action),
            vector_timings: None,
            reranker_timing: None,
        }
    }

    fn reranker_active(&mut self) {
        self.reranker = "active";
    }

    fn reranker_degraded(&mut self) {
        self.reranker = "degraded";
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ResponseBudget {
    #[serde(skip)]
    pub byte_limit: usize,
    pub rendered_bytes: usize,
    pub unbudgeted_bytes: usize,
    pub truncated: bool,
    pub omitted_hits: usize,
    pub omitted_semantic_artifacts: usize,
    pub omitted_semantic_supports: usize,
    pub omitted_nodes: usize,
    pub omitted_edges: usize,
    pub truncated_snippets: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_sections: Option<SearchSectionBytes>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SearchSectionBytes {
    pub hits_bytes: usize,
    pub graph_bytes: usize,
    pub memory_bytes: usize,
    pub envelope_bytes: usize,
    pub total_bytes: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchExpansion {
    pub projection: ExpansionProjection,
    pub seeds: Vec<String>,
    pub nodes: Vec<structural::GraphNode>,
    pub edges: Vec<structural::GraphEdge>,
    #[serde(skip_serializing_if = "is_zero")]
    pub candidate_paths: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub selected_paths: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted_paths: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted_nodes: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted_edges: usize,
    /// Accepted path edge sets used to keep path omission accounting accurate
    /// if the outer whole-response budget later sheds graph edges.
    #[serde(skip)]
    pub(crate) selected_path_edges: Vec<Vec<EdgeIdentity>>,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
    pub payload_bytes: usize,
    pub truncated: bool,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a predicate taking &usize"
)]
fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub file: String,
    /// Deterministic path/content classification from the indexer.
    pub file_role: String,
    /// Fresh, likely repository-scout override, when one is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_role: Option<String>,
    pub file_origin: String,
    pub kind: String,
    pub name: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub score: f64,
    #[serde(rename = "match")]
    pub match_reason: MatchReason,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matched_identifiers: Vec<String>,
    /// Absolute file lines containing at least one lexical query-term match.
    /// Exhaustive search reports unique lines rather than match multiplicity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_lines: Option<Vec<i64>>,
    pub snippet: String,
    /// First excerpt line when it differs from the retrieval chunk's start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_line: Option<i64>,
    pub snippet_truncated: bool,
    /// Snapshot-scoped structural handles projected from this retrieval chunk.
    pub anchors: Vec<String>,
    /// Present only when the file is projected into the structural graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_anchor: Option<String>,
    /// Graph context: symbols this chunk calls / renders (resolved names).
    pub uses: Vec<String>,
    /// Symbols declared here that other files use, with usage counts.
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    ExactDefinition,
    ExactOccurrence,
    Lexical,
    Hybrid,
}

#[derive(Debug)]
struct ExactIntentCandidates {
    identifiers: Vec<String>,
    definitions: Vec<Vec<i64>>,
    occurrences: Vec<Vec<i64>>,
}

#[derive(Debug)]
struct RankedHitCandidate {
    chunk_id: i64,
    score: f64,
    match_reason: MatchReason,
    matched_identifiers: Vec<String>,
}

/// Build an FTS5 query: each identifier-ish token is quoted and OR-joined, so
/// any match ranks (BM25 handles weighting) and no user input is FTS syntax.
fn fts_terms(q: &str) -> Vec<&str> {
    q.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|term| !term.is_empty())
        .collect()
}

fn fts_query_for_column(q: &str, column: Option<&str>) -> String {
    fts_terms(q)
        .into_iter()
        .map(|term| match column {
            Some(column) => format!("{column}:\"{term}\""),
            None => format!("\"{term}\""),
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn fts_query(q: &str) -> String {
    fts_query_for_column(q, None)
}

fn exhaustive_fts_query(q: &str) -> String {
    fts_query_for_column(q, Some("content"))
}

const BROAD_OR_QUERY_CHUNK_THRESHOLD: usize = 200;
const BROAD_OR_QUERY_MESSAGE: &str = "Exhaustive search OR-joins FTS terms. Refine or abandon this traversal if that is not the intended evidence set.";

fn exhaustive_effective_terms(q: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    fts_terms(q)
        .into_iter()
        .filter_map(|term| {
            let normalized = term.to_lowercase();
            seen.insert(normalized).then(|| term.to_string())
        })
        .collect()
}

fn exhaustive_warnings(
    q: &str,
    total_chunks: usize,
    first_page: bool,
) -> Vec<ExhaustiveSearchWarning> {
    if !first_page || total_chunks < BROAD_OR_QUERY_CHUNK_THRESHOLD {
        return Vec::new();
    }
    let terms = exhaustive_effective_terms(q);
    if terms.len() < 2 {
        return Vec::new();
    }
    vec![ExhaustiveSearchWarning {
        code: "broad_or_query",
        terms,
        total_chunks,
        message: BROAD_OR_QUERY_MESSAGE,
    }]
}

fn exact_intent_tokens(query: &str) -> Vec<String> {
    let raw_tokens = identifier_tokens(query);
    let single_identifier = raw_tokens.len() == 1 && is_identifier_token(raw_tokens[0]);
    let mut tokens = Vec::new();
    let mut seen = HashSet::new();
    for token in raw_tokens {
        if !is_identifier_token(token) || (!single_identifier && !is_code_shaped_identifier(token))
        {
            continue;
        }
        if seen.insert(token.to_string()) {
            tokens.push(token.to_string());
        }
    }
    tokens
}

fn identifier_tokens(query: &str) -> Vec<&str> {
    query
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '$')
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_single_identifier_intent(query: &str) -> bool {
    let tokens = identifier_tokens(query);
    tokens.len() == 1 && is_identifier_token(tokens[0])
}

fn is_identifier_token(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

fn is_code_shaped_identifier(value: &str) -> bool {
    value.starts_with('_')
        || value.starts_with('$')
        || value.contains('_')
        || value.contains('$')
        || value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
        || value
            .chars()
            .skip(1)
            .any(|character| character.is_ascii_uppercase())
}

fn exact_intent_candidates(
    conn: &Connection,
    query: &str,
    per_identifier_limit: usize,
    file_roles: &[String],
    file_origins: &[String],
    file_formats: &[String],
) -> Result<ExactIntentCandidates> {
    let identifiers = exact_intent_tokens(query);
    // A pure identifier lookup is an explicit request for every exact usage.
    // In a mixed natural-language query, one occurrence per identifier is
    // enough to establish exact coverage; letting a common incidental type
    // consume the whole result budget would hide the hybrid task matches.
    let occurrence_limit = if is_single_identifier_intent(query) {
        per_identifier_limit
    } else {
        1
    };
    let mut definitions = Vec::with_capacity(identifiers.len());
    let mut occurrences = Vec::with_capacity(identifiers.len());
    for identifier in &identifiers {
        let definition_ids = exact_definition_chunks(
            conn,
            identifier,
            per_identifier_limit,
            file_roles,
            file_origins,
            file_formats,
        )?;
        let definition_set = definition_ids.iter().copied().collect::<HashSet<_>>();
        // Fetch through the normal bounded per-identifier window before
        // applying the mixed-query admission cap. A definition chunk can also
        // contain an occurrence; limiting first could filter that one row and
        // incorrectly hide the next non-definition occurrence.
        let mut occurrence_ids = exact_occurrence_chunks(
            conn,
            identifier,
            per_identifier_limit,
            file_roles,
            file_origins,
            file_formats,
        )?
        .into_iter()
        .filter(|chunk_id| !definition_set.contains(chunk_id))
        .collect::<Vec<_>>();
        occurrence_ids.truncate(occurrence_limit);
        definitions.push(definition_ids);
        occurrences.push(occurrence_ids);
    }
    Ok(ExactIntentCandidates {
        identifiers,
        definitions,
        occurrences,
    })
}

fn exact_definition_chunks(
    conn: &Connection,
    identifier: &str,
    limit: usize,
    file_roles: &[String],
    file_origins: &[String],
    file_formats: &[String],
) -> Result<Vec<i64>> {
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
    let eligible_formats =
        format_allowlist_json(file_formats, crate::formats::Capability::ExactDefinition)?;
    let row_limit = limit.max(1) as i64;
    let mut rows = Vec::<(i64, i64, i64, i64, String, i64)>::new();

    let mut named_chunks = conn.prepare_cached(
        "SELECT chunk.id, 0 AS name_priority, 1 AS export_priority,
                chunk.end-chunk.start AS span, file.path, chunk.start
         FROM code_chunks chunk
         JOIN code_files file ON file.id=chunk.file_id
         WHERE chunk.name=?1 COLLATE BINARY
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
           AND file.format IN (SELECT value FROM json_each(?7))
         ORDER BY file.path, chunk.start, chunk.id
         LIMIT ?8",
    )?;
    let named = named_chunks.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
            eligible_formats,
            row_limit,
        ],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    for row in named {
        rows.push(row?);
    }

    let roles_json = serde_json::to_string(file_roles)?;
    let eligible_formats =
        format_allowlist_json(file_formats, crate::formats::Capability::ExactDefinition)?;
    let mut containing_chunks = conn.prepare_cached(
        "SELECT chunk.id,
                CASE WHEN chunk.name=?1 COLLATE BINARY THEN 0 ELSE 1 END AS name_priority,
                CASE WHEN symbol.exported=1 THEN 0 ELSE 1 END AS export_priority,
                chunk.end-chunk.start AS span, file.path, chunk.start
         FROM symbols symbol
         JOIN code_files file ON file.id=symbol.file_id
         JOIN code_chunks chunk ON chunk.file_id=symbol.file_id
           AND chunk.start<=symbol.decl_start AND symbol.decl_start<chunk.end
         WHERE symbol.name=?1 COLLATE BINARY
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
           AND file.format IN (SELECT value FROM json_each(?7))
         ORDER BY name_priority, export_priority, span, file.path, chunk.start, chunk.id
         LIMIT ?8",
    )?;
    let containing = containing_chunks.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
            eligible_formats,
            row_limit,
        ],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    for row in containing {
        rows.push(row?);
    }

    rows.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.4.cmp(&right.4))
            .then_with(|| left.5.cmp(&right.5))
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for (chunk_id, ..) in rows {
        if seen.insert(chunk_id) {
            result.push(chunk_id);
            if result.len() == limit {
                break;
            }
        }
    }
    Ok(result)
}

fn exact_occurrence_chunks(
    conn: &Connection,
    identifier: &str,
    limit: usize,
    file_roles: &[String],
    file_origins: &[String],
    file_formats: &[String],
) -> Result<Vec<i64>> {
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
    let eligible_formats =
        format_allowlist_json(file_formats, crate::formats::Capability::ExactOccurrence)?;
    let mut statement = conn.prepare_cached(
        "SELECT candidate.chunk_id
         FROM (
           SELECT ref.chunk_id AS chunk_id, file.path AS path, ref.start AS position
           FROM refs ref
           JOIN chunks chunk ON chunk.id=ref.chunk_id
           JOIN files file ON file.id=chunk.file_id
           WHERE ref.chunk_id IS NOT NULL AND ref.target_name=?1 COLLATE BINARY
             AND ((?2 AND file.origin='repository')
               OR (?3 AND file.origin='workspace')
               OR (?4 AND file.origin='dependency'))
             AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
             AND file.format IN (SELECT value FROM json_each(?7))
           UNION ALL
           SELECT call.chunk_id, file.path, call.start
           FROM member_calls call
           JOIN chunks chunk ON chunk.id=call.chunk_id
           JOIN files file ON file.id=chunk.file_id
           WHERE call.chunk_id IS NOT NULL AND call.prop=?1 COLLATE BINARY
             AND ((?2 AND file.origin='repository')
               OR (?3 AND file.origin='workspace')
               OR (?4 AND file.origin='dependency'))
             AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
             AND file.format IN (SELECT value FROM json_each(?7))
           UNION ALL
           SELECT site.chunk_id, file.path, site.start
           FROM entity_sites site
           JOIN chunks chunk ON chunk.id=site.chunk_id
           JOIN files file ON file.id=chunk.file_id
           WHERE site.chunk_id IS NOT NULL AND site.target_name=?1 COLLATE BINARY
             AND ((?2 AND file.origin='repository')
               OR (?3 AND file.origin='workspace')
               OR (?4 AND file.origin='dependency'))
             AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
             AND file.format IN (SELECT value FROM json_each(?7))
         ) candidate
         GROUP BY candidate.chunk_id
         ORDER BY MIN(candidate.path), MIN(candidate.position), candidate.chunk_id
         LIMIT ?8",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
            eligible_formats,
            limit.max(1) as i64,
        ],
        |row| row.get::<_, i64>(0),
    )?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }

    // Structured tables deliberately omit many property-key occurrences:
    // object-literal fields, non-call member reads/writes, and some computed
    // state containers are not refs or member calls. Use FTS only as a bounded
    // candidate generator, then enforce case-sensitive identifier boundaries
    // against the stored source chunk before admitting an exact occurrence.
    if result.len() < limit {
        let eligible_formats =
            format_allowlist_json(file_formats, crate::formats::Capability::ExactOccurrence)?;
        let mut textual = conn.prepare_cached(
            "SELECT chunk.id, chunk.content
             FROM chunks_fts
             JOIN chunks chunk ON chunk.id=chunks_fts.rowid
             JOIN files file ON file.id=chunk.file_id
             WHERE chunks_fts MATCH ?1
               AND ((?2 AND file.origin='repository')
                 OR (?3 AND file.origin='workspace')
                 OR (?4 AND file.origin='dependency'))
               AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
               AND file.format IN (SELECT value FROM json_each(?7))
             ORDER BY file.path, chunk.start, chunk.id
             LIMIT ?8",
        )?;
        let candidate_limit = limit.saturating_mul(32).clamp(32, 4_096) as i64;
        let rows = textual.query_map(
            rusqlite::params![
                format!("\"{identifier}\""),
                flags.0,
                flags.1,
                flags.2,
                file_roles.is_empty(),
                roles_json,
                eligible_formats,
                candidate_limit,
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut seen = result.iter().copied().collect::<HashSet<_>>();
        for row in rows {
            let (chunk_id, content) = row?;
            if contains_code_identifier(&content, identifier) && seen.insert(chunk_id) {
                result.push(chunk_id);
                if result.len() == limit {
                    break;
                }
            }
        }
    }
    Ok(result)
}

fn contains_code_identifier(content: &str, identifier: &str) -> bool {
    #[derive(Clone, Copy)]
    enum LexicalState {
        Code,
        SingleQuoted,
        DoubleQuoted,
        Template,
        LineComment,
        BlockComment,
    }

    let source = content.as_bytes();
    let needle = identifier.as_bytes();
    let mut state = LexicalState::Code;
    let mut cursor = 0;
    while cursor < source.len() {
        match state {
            LexicalState::Code => {
                if source[cursor..].starts_with(b"//") {
                    state = LexicalState::LineComment;
                    cursor += 2;
                    continue;
                }
                if source[cursor..].starts_with(b"/*") {
                    state = LexicalState::BlockComment;
                    cursor += 2;
                    continue;
                }
                state = match source[cursor] {
                    b'\'' => LexicalState::SingleQuoted,
                    b'"' => LexicalState::DoubleQuoted,
                    b'`' => LexicalState::Template,
                    _ => {
                        if source[cursor..].starts_with(needle)
                            && (cursor == 0 || !is_identifier_continue_byte(source[cursor - 1]))
                            && (cursor + needle.len() == source.len()
                                || !is_identifier_continue_byte(source[cursor + needle.len()]))
                        {
                            return true;
                        }
                        cursor += 1;
                        continue;
                    }
                };
                cursor += 1;
            }
            LexicalState::SingleQuoted | LexicalState::DoubleQuoted | LexicalState::Template => {
                if source[cursor] == b'\\' {
                    cursor = (cursor + 2).min(source.len());
                    continue;
                }
                let closes = matches!(
                    (state, source[cursor]),
                    (LexicalState::SingleQuoted, b'\'')
                        | (LexicalState::DoubleQuoted, b'"')
                        | (LexicalState::Template, b'`')
                );
                if closes {
                    state = LexicalState::Code;
                }
                cursor += 1;
            }
            LexicalState::LineComment => {
                if source[cursor] == b'\n' {
                    state = LexicalState::Code;
                }
                cursor += 1;
            }
            LexicalState::BlockComment => {
                if source[cursor..].starts_with(b"*/") {
                    state = LexicalState::Code;
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
        }
    }
    false
}

fn is_identifier_continue_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn tiered_candidates(
    exact: ExactIntentCandidates,
    hybrid: &[(i64, f64)],
) -> Vec<RankedHitCandidate> {
    let hybrid_scores = hybrid.iter().copied().collect::<HashMap<_, _>>();
    let hybrid_positions = hybrid
        .iter()
        .enumerate()
        .map(|(position, (chunk_id, _))| (*chunk_id, position))
        .collect::<HashMap<_, _>>();
    let mut occurrences = exact.occurrences;
    for per_identifier in &mut occurrences {
        // Exact occurrences retain their absolute tier, but peers which also
        // survived the hybrid pipeline use its reranker/repository-policy
        // order. Stable sorting preserves the structural/path order for
        // exact-only candidates outside the bounded hybrid pool.
        per_identifier.sort_by_key(|chunk_id| {
            hybrid_positions
                .get(chunk_id)
                .copied()
                .map_or((1, usize::MAX), |position| (0, position))
        });
    }
    let mut ranked = Vec::<RankedHitCandidate>::new();
    let mut positions = HashMap::<i64, usize>::new();

    append_exact_tier(
        &mut ranked,
        &mut positions,
        &exact.identifiers,
        &exact.definitions,
        MatchReason::ExactDefinition,
        &hybrid_scores,
    );
    append_exact_tier(
        &mut ranked,
        &mut positions,
        &exact.identifiers,
        &occurrences,
        MatchReason::ExactOccurrence,
        &hybrid_scores,
    );
    for &(chunk_id, score) in hybrid {
        if positions.contains_key(&chunk_id) {
            continue;
        }
        positions.insert(chunk_id, ranked.len());
        ranked.push(RankedHitCandidate {
            chunk_id,
            score,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
        });
    }
    ranked
}

fn append_exact_tier(
    ranked: &mut Vec<RankedHitCandidate>,
    positions: &mut HashMap<i64, usize>,
    identifiers: &[String],
    candidates: &[Vec<i64>],
    match_reason: MatchReason,
    hybrid_scores: &HashMap<i64, f64>,
) {
    let maximum_depth = candidates.iter().map(Vec::len).max().unwrap_or(0);
    for depth in 0..maximum_depth {
        for (identifier, per_identifier) in identifiers.iter().zip(candidates) {
            let Some(&chunk_id) = per_identifier.get(depth) else {
                continue;
            };
            if let Some(&position) = positions.get(&chunk_id) {
                if !ranked[position]
                    .matched_identifiers
                    .iter()
                    .any(|value| value == identifier)
                {
                    ranked[position]
                        .matched_identifiers
                        .push(identifier.clone());
                }
                continue;
            }
            positions.insert(chunk_id, ranked.len());
            ranked.push(RankedHitCandidate {
                chunk_id,
                score: hybrid_scores.get(&chunk_id).copied().unwrap_or(0.0),
                match_reason,
                matched_identifiers: vec![identifier.clone()],
            });
        }
    }
}

fn bm25_ranking(
    conn: &Connection,
    q: &str,
    limit: usize,
    file_roles: &[String],
    file_origins: &[String],
    file_formats: &[String],
) -> Result<Vec<(i64, f64)>> {
    let fq = fts_query(q);
    if fq.is_empty() {
        return Ok(vec![]);
    }
    let eligible_formats =
        format_allowlist_json(file_formats, crate::formats::Capability::CodeLexical)?;
    let mut stmt = conn.prepare(
        "SELECT chunks_fts.rowid, bm25(chunks_fts, 2.0, 4.0, 3.0, 1.0) AS r
         FROM chunks_fts
         JOIN chunks chunk ON chunk.id=chunks_fts.rowid
         JOIN files file ON file.id=chunk.file_id
         WHERE chunks_fts MATCH ?1
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
           AND file.format IN (SELECT value FROM json_each(?7))
         ORDER BY r LIMIT ?8",
    )?;
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
    let rows = stmt.query_map(
        rusqlite::params![
            fq,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            &roles_json,
            &eligible_formats,
            limit as i64
        ],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn vector_ranking(
    conn: &Connection,
    provider: &embed::Provider,
    q: &str,
    limit: usize,
    file_origins: &[String],
    file_formats: &[String],
) -> Result<embed::VectorSearchResult> {
    embed::vector_search(conn, provider, q, limit, file_origins, file_formats)
}

fn origin_flags(origins: &[String]) -> (bool, bool, bool) {
    (
        origins.iter().any(|origin| origin == "repository"),
        origins.iter().any(|origin| origin == "workspace"),
        origins.iter().any(|origin| origin == "dependency"),
    )
}

const EXHAUSTIVE_CURSOR_PREFIX: &str = "jscout-exhaustive-v3";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExhaustiveCursorPosition {
    path: String,
    start: i64,
    hash: String,
}

#[derive(Debug)]
struct ExhaustivePageState {
    total_chunks: usize,
    selected_positions: Vec<ExhaustiveCursorPosition>,
    has_more: bool,
    request_fingerprint: String,
    scope: SearchScope,
}

#[derive(Debug)]
struct ExhaustiveHitRow {
    chunk_id: i64,
    position: ExhaustiveCursorPosition,
    file_role: String,
    file_origin: String,
    kind: String,
    name: Option<String>,
    start_line: i64,
    end_line: i64,
    repository_role: Option<String>,
    content: String,
    format: String,
}

const EXHAUSTIVE_MATCH_START: &str = "\u{1e}jscout-match-start\u{1f}";
const EXHAUSTIVE_MATCH_END: &str = "\u{1e}jscout-match-end\u{1f}";

fn exhaustive_highlight_markers(rows: &[ExhaustiveHitRow]) -> (String, String) {
    let mut suffix = String::new();
    loop {
        let (start, end) = if suffix.is_empty() {
            (EXHAUSTIVE_MATCH_START.into(), EXHAUSTIVE_MATCH_END.into())
        } else {
            (
                format!("\u{1e}jscout-match-start{suffix}\u{1f}"),
                format!("\u{1e}jscout-match-end{suffix}\u{1f}"),
            )
        };
        if rows
            .iter()
            .all(|row| !row.content.contains(&start) && !row.content.contains(&end))
        {
            return (start, end);
        }
        // Every page is finite, so eventually each candidate is longer than
        // every source string and therefore cannot collide.
        suffix.push('-');
    }
}

fn exhaustive_highlights(
    conn: &Connection,
    query: &str,
    rows: &[ExhaustiveHitRow],
) -> Result<(HashMap<i64, String>, String)> {
    if rows.is_empty() {
        return Ok((HashMap::new(), String::new()));
    }
    let (start_marker, end_marker) = exhaustive_highlight_markers(rows);
    let chunk_ids =
        serde_json::to_string(&rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>())?;
    let mut statement = conn.prepare(
        "SELECT chunks_fts.rowid, highlight(chunks_fts, 0, ?3, ?4)
         FROM chunks_fts
         WHERE chunks_fts MATCH ?1
           AND chunks_fts.rowid IN (SELECT value FROM json_each(?2))",
    )?;
    let highlighted = statement.query_map(
        rusqlite::params![query, chunk_ids, start_marker, end_marker],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )?;
    let by_chunk = highlighted.collect::<std::result::Result<HashMap<_, _>, _>>()?;
    if by_chunk.len() != rows.len() {
        anyhow::bail!("exhaustive search could not highlight every selected chunk");
    }
    Ok((by_chunk, start_marker))
}

fn exhaustive_match_lines(
    highlighted_content: &str,
    start_line: i64,
    start_marker: &str,
) -> Vec<i64> {
    let mut lines = Vec::new();
    let mut line = start_line;
    let mut remaining = highlighted_content;
    while let Some(marker) = remaining.find(start_marker) {
        line += remaining[..marker]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as i64;
        if lines.last().copied() != Some(line) {
            lines.push(line);
        }
        remaining = &remaining[marker + start_marker.len()..];
    }
    lines
}

fn normalized_allowlist(values: &[String], allowed: &[&str]) -> Vec<String> {
    allowed
        .iter()
        .filter(|candidate| values.iter().any(|value| value == **candidate))
        .map(|value| (*value).to_string())
        .collect()
}

fn code_format_ids() -> Vec<&'static str> {
    crate::formats::ALL
        .iter()
        .filter(|format| format.corpus == crate::formats::Corpus::Code)
        .map(|format| format.id)
        .collect()
}

pub(crate) fn validate_code_formats(values: &[String]) -> Result<()> {
    let allowed = code_format_ids();
    for format in values {
        if !allowed.contains(&format.as_str()) {
            anyhow::bail!(
                "code format must be one of: {}; got `{format}`",
                allowed.join(", ")
            );
        }
    }
    Ok(())
}

pub(crate) fn format_scope_supports_code_vectors(values: &[String]) -> bool {
    !format_allowlist(values, crate::formats::Capability::CodeVector).is_empty()
}

fn normalized_code_formats(values: &[String]) -> Vec<String> {
    normalized_allowlist(values, &code_format_ids())
}

fn format_allowlist(requested: &[String], capability: crate::formats::Capability) -> Vec<String> {
    crate::formats::eligible_ids(capability)
        .into_iter()
        .filter(|format| requested.is_empty() || requested.iter().any(|value| value == format))
        .map(str::to_string)
        .collect()
}

fn format_allowlist_json(
    requested: &[String],
    capability: crate::formats::Capability,
) -> Result<String> {
    Ok(serde_json::to_string(&format_allowlist(
        requested, capability,
    ))?)
}

fn exhaustive_scope(options: &SearchOptions) -> (Vec<String>, Vec<String>, SearchScope) {
    let selected_roles = normalized_allowlist(&options.file_roles, file_role::ALL);
    let all_roles = selected_roles.len() == file_role::ALL.len();
    let query_roles = if options.file_roles.is_empty() || all_roles {
        Vec::new()
    } else {
        selected_roles.clone()
    };
    let file_roles = if query_roles.is_empty() {
        SearchScopeFileRoles::All
    } else {
        SearchScopeFileRoles::Selected(selected_roles)
    };
    let origins = normalized_allowlist(&options.file_origins, origin::ALL);
    let selected_formats = normalized_code_formats(&options.formats);
    let all_formats = selected_formats.len() == code_format_ids().len();
    let query_formats = if options.formats.is_empty() || all_formats {
        Vec::new()
    } else {
        selected_formats.clone()
    };
    let formats = if query_formats.is_empty() {
        SearchScopeFormats::All
    } else {
        SearchScopeFormats::Selected(selected_formats)
    };
    (
        query_roles,
        query_formats,
        SearchScope {
            corpus: "indexed_chunks",
            file_roles,
            origins,
            formats,
        },
    )
}

fn exhaustive_request_fingerprint(
    q: &str,
    file_roles: &[String],
    origins: &[String],
    formats: &[String],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-exhaustive-request-v2\0");
    hasher.update(q.as_bytes());
    hasher.update(b"\0roles\0");
    if file_roles.is_empty() {
        hasher.update(b"all\0");
    } else {
        for role in file_roles {
            hasher.update(role.as_bytes());
            hasher.update(b"\0");
        }
    }
    hasher.update(b"origins\0");
    for origin in origins {
        hasher.update(origin.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"formats\0");
    if formats.is_empty() {
        hasher.update(b"all\0");
    } else {
        for format in formats {
            hasher.update(format.as_bytes());
            hasher.update(b"\0");
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn encode_cursor_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(path.len() * 2);
    for byte in path.bytes() {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_cursor_path(encoded: &str) -> Result<String> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        anyhow::bail!("invalid exhaustive search cursor");
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow::anyhow!("invalid exhaustive search cursor"))?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow::anyhow!("invalid exhaustive search cursor"))?;
        decoded.push(((high << 4) | low) as u8);
    }
    String::from_utf8(decoded).map_err(|_| anyhow::anyhow!("invalid exhaustive search cursor"))
}

fn encode_exhaustive_cursor(
    snapshot: &str,
    fingerprint: &str,
    position: &ExhaustiveCursorPosition,
) -> String {
    format!(
        "{EXHAUSTIVE_CURSOR_PREFIX}.{snapshot}.{fingerprint}.{}.{:016x}.{}",
        encode_cursor_path(&position.path),
        position.start,
        position.hash,
    )
}

fn decode_exhaustive_cursor(
    cursor: &str,
    snapshot: &str,
    fingerprint: &str,
) -> Result<ExhaustiveCursorPosition> {
    let mut parts = cursor.split('.');
    let (
        Some(prefix),
        Some(cursor_snapshot),
        Some(cursor_fingerprint),
        Some(path),
        Some(start),
        Some(hash),
    ) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    )
    else {
        anyhow::bail!("invalid exhaustive search cursor");
    };
    if parts.next().is_some()
        || prefix != EXHAUSTIVE_CURSOR_PREFIX
        || cursor_snapshot.len() != 64
        || cursor_fingerprint.len() != 64
        || start.len() != 16
        || hash.len() != 64
        || !cursor_snapshot.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !cursor_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !start.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        anyhow::bail!("invalid exhaustive search cursor");
    }
    if cursor_snapshot != snapshot {
        anyhow::bail!(
            "exhaustive search cursor snapshot changed: expected {cursor_snapshot}, current {snapshot}"
        );
    }
    if cursor_fingerprint != fingerprint {
        anyhow::bail!("exhaustive search cursor does not match the query and scope");
    }
    let start = i64::from_str_radix(start, 16)
        .map_err(|_| anyhow::anyhow!("invalid exhaustive search cursor"))?;
    if start < 0 {
        anyhow::bail!("invalid exhaustive search cursor");
    }
    Ok(ExhaustiveCursorPosition {
        path: decode_cursor_path(path)?,
        start,
        hash: hash.to_string(),
    })
}

fn exhaustive_cursor_position(
    conn: &Connection,
    fts_query: &str,
    position: &ExhaustiveCursorPosition,
    file_roles: &[String],
    file_origins: &[String],
    file_formats: &[String],
) -> Result<i64> {
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
    let eligible_formats =
        format_allowlist_json(file_formats, crate::formats::Capability::CodeLexical)?;
    let (chunk_id, matches): (Option<i64>, i64) = conn.query_row(
        "SELECT MIN(chunk.id), COUNT(*)
             FROM chunks_fts
             JOIN chunks chunk ON chunk.id=chunks_fts.rowid
             JOIN files file ON file.id=chunk.file_id
             WHERE chunks_fts MATCH ?1
               AND file.path=?2 AND chunk.start=?3 AND chunk.hash=?4
               AND ((?5 AND file.origin='repository')
                 OR (?6 AND file.origin='workspace')
                 OR (?7 AND file.origin='dependency'))
               AND (?8 OR file.role IN (SELECT value FROM json_each(?9)))
               AND file.format IN (SELECT value FROM json_each(?10))",
        rusqlite::params![
            fts_query,
            position.path,
            position.start,
            position.hash,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
            eligible_formats,
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if matches != 1 {
        anyhow::bail!("exhaustive search cursor does not identify one matching chunk in scope");
    }
    chunk_id.ok_or_else(|| anyhow::anyhow!("invalid exhaustive search cursor"))
}

fn exhaustive_hits(
    conn: &Connection,
    q: &str,
    options: &SearchOptions,
    snapshot: &str,
    cursor: Option<&str>,
) -> Result<(Vec<Hit>, ExhaustivePageState)> {
    let (file_roles, file_formats, scope) = exhaustive_scope(options);
    let request_fingerprint =
        exhaustive_request_fingerprint(q, &file_roles, &scope.origins, &file_formats);
    let query = exhaustive_fts_query(q);
    let cursor_position = if let Some(cursor) = cursor {
        let position = decode_exhaustive_cursor(cursor, snapshot, &request_fingerprint)?;
        if query.is_empty() {
            anyhow::bail!("exhaustive search cursor does not identify a matching chunk in scope");
        }
        let chunk_id = exhaustive_cursor_position(
            conn,
            &query,
            &position,
            &file_roles,
            &scope.origins,
            &file_formats,
        )?;
        Some((position.path, position.start, chunk_id))
    } else {
        None
    };
    if query.is_empty() {
        return Ok((
            Vec::new(),
            ExhaustivePageState {
                total_chunks: 0,
                selected_positions: Vec::new(),
                has_more: false,
                request_fingerprint,
                scope,
            },
        ));
    }

    let flags = origin_flags(&scope.origins);
    let roles_json = serde_json::to_string(&file_roles)?;
    let eligible_formats =
        format_allowlist_json(&file_formats, crate::formats::Capability::CodeLexical)?;
    let total: i64 = conn.query_row(
        "SELECT count(*)
         FROM chunks_fts
         JOIN chunks chunk ON chunk.id=chunks_fts.rowid
         JOIN files file ON file.id=chunk.file_id
         WHERE chunks_fts MATCH ?1
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
           AND file.format IN (SELECT value FROM json_each(?7))",
        rusqlite::params![
            query,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            &roles_json,
            &eligible_formats,
        ],
        |row| row.get(0),
    )?;
    let total_chunks = usize::try_from(total)
        .map_err(|_| anyhow::anyhow!("exhaustive search match count exceeded this platform"))?;

    let cursor_path = cursor_position.as_ref().map(|position| position.0.as_str());
    let cursor_start = cursor_position.as_ref().map_or(0, |position| position.1);
    let cursor_chunk_id = cursor_position.as_ref().map_or(0, |position| position.2);
    let mut statement = conn.prepare(
        "SELECT chunk.id, file.path, chunk.start, chunk.hash,
                file.role, file.origin, chunk.kind, chunk.name,
                chunk.start_line, chunk.end_line, policy.effective_role,
                chunk.content, file.format
         FROM chunks_fts
         JOIN chunks chunk ON chunk.id=chunks_fts.rowid
         JOIN files file ON file.id=chunk.file_id
         LEFT JOIN repository_file_policy policy ON policy.file_id=file.id
         WHERE chunks_fts MATCH ?1
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
           AND file.format IN (SELECT value FROM json_each(?7))
           AND (?8 IS NULL OR (file.path,chunk.start,chunk.id)>(?8,?9,?10))
         ORDER BY file.path,chunk.start,chunk.id
         LIMIT ?11",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            query,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            &roles_json,
            &eligible_formats,
            cursor_path,
            cursor_start,
            cursor_chunk_id,
            (options.limit + 1) as i64,
        ],
        |row| {
            Ok(ExhaustiveHitRow {
                chunk_id: row.get(0)?,
                position: ExhaustiveCursorPosition {
                    path: row.get(1)?,
                    start: row.get(2)?,
                    hash: row.get(3)?,
                },
                file_role: row.get(4)?,
                file_origin: row.get(5)?,
                kind: row.get(6)?,
                name: row.get(7)?,
                start_line: row.get(8)?,
                end_line: row.get(9)?,
                repository_role: row.get(10)?,
                content: row.get(11)?,
                format: row.get(12)?,
            })
        },
    )?;
    let mut selected = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = selected.len() > options.limit;
    selected.truncate(options.limit);

    let (mut highlighted, start_marker) = exhaustive_highlights(conn, &query, &selected)?;
    let mut anchors = project_exhaustive_anchors(conn, &selected)?;
    let mut hits = Vec::with_capacity(selected.len());
    let mut selected_positions = Vec::with_capacity(selected.len());
    for row in selected {
        let structurally_eligible =
            crate::formats::by_id(&row.format).is_some_and(|format| format.structural_eligible());
        let file_anchor = structurally_eligible.then(|| format!("file:{}", row.position.path));
        let highlighted_content = highlighted
            .remove(&row.chunk_id)
            .ok_or_else(|| anyhow::anyhow!("missing exhaustive highlight for selected chunk"))?;
        let match_lines =
            exhaustive_match_lines(&highlighted_content, row.start_line, &start_marker);
        let projected_anchors = anchors.remove(&row.chunk_id).unwrap_or_default();
        hits.push(Hit {
            chunk_id: row.chunk_id,
            file: row.position.path.clone(),
            file_role: row.file_role,
            repository_role: row.repository_role,
            file_origin: row.file_origin,
            kind: row.kind,
            name: row.name,
            start_line: row.start_line,
            end_line: row.end_line,
            score: 0.0,
            match_reason: MatchReason::Lexical,
            matched_identifiers: Vec::new(),
            match_lines: Some(match_lines),
            snippet: String::new(),
            snippet_line: None,
            snippet_truncated: false,
            anchors: projected_anchors,
            file_anchor,
            uses: Vec::new(),
            used_by: Vec::new(),
        });
        selected_positions.push(row.position);
    }
    Ok((
        hits,
        ExhaustivePageState {
            total_chunks,
            selected_positions,
            has_more,
            request_fingerprint,
            scope,
        },
    ))
}

/// Optional cross-encoder rerank stage (dms-style service:
/// POST {model, query, candidates:[{id,text}]} -> {scores:[{id,score}]}).
/// Local embeddings use the bundled service automatically; an explicit
/// `reranker.url` overrides that endpoint.
#[derive(Debug, Clone)]
pub struct Reranker {
    url: String,
    model: String,
    pool: usize,
    max_chars: usize,
}

impl Reranker {
    pub fn from_settings(
        reranker: &crate::config::RerankerSettings,
        embedding: &crate::config::EmbeddingSettings,
        inference: &crate::config::InferenceSettings,
    ) -> Option<Self> {
        let url = reranker.url.clone().or_else(|| {
            (embedding.provider.as_deref() == Some("local"))
                .then(|| format!("{}/rerank", inference.url.trim_end_matches('/')))
        })?;
        Some(Self {
            url,
            model: reranker.model.clone(),
            pool: reranker.top.min(100),
            max_chars: reranker.max_chars,
        })
    }

    pub(crate) fn rerank(
        &self,
        query: &str,
        candidates: &[(i64, String)],
    ) -> Result<Vec<(i64, f64)>> {
        let body = serde_json::json!({
            "model": self.model,
            "query": query,
            "deadline_ms": 120_000,
            "candidates": candidates
                .iter()
                .map(|(id, text)| serde_json::json!({ "id": id.to_string(), "text": text }))
                .collect::<Vec<_>>(),
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(125)))
            .build()
            .new_agent();
        let mut resp = agent
            .post(&self.url)
            .header("content-type", "application/json")
            .send(body.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
        let scores = parsed["scores"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("unexpected rerank response: {parsed}"))?;
        let mut out: Vec<(i64, f64)> = scores
            .iter()
            .filter_map(|s| {
                let id: i64 = s["id"].as_str()?.parse().ok()?;
                Some((id, s["score"].as_f64()?))
            })
            .collect();
        let expected = candidates.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
        let returned = out.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
        if out.len() != candidates.len() || returned != expected {
            anyhow::bail!("reranker did not return every candidate exactly once");
        }
        let incoming = candidates
            .iter()
            .enumerate()
            .map(|(position, (id, _))| (*id, position))
            .collect::<HashMap<_, _>>();
        out.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| incoming[&a.0].cmp(&incoming[&b.0]))
        });
        Ok(out)
    }

    pub(crate) const fn candidate_limit(&self) -> usize {
        self.pool
    }

    pub(crate) fn truncate_document(&self, document: &mut String) {
        truncate_utf8(document, self.max_chars);
    }
}

/// Reciprocal Rank Fusion over the available rankings.
fn rrf(rankings: &[Vec<(i64, f64)>], k: f64) -> Vec<(i64, f64)> {
    let mut positions = HashMap::new();
    let mut out = Vec::<(i64, f64)>::new();
    for ranking in rankings {
        for (rank, (id, _)) in ranking.iter().enumerate() {
            let position = *positions.entry(*id).or_insert_with(|| {
                out.push((*id, 0.0));
                out.len() - 1
            });
            out[position].1 += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    // Stable sorting breaks equal scores by first appearance in the supplied
    // rankings, independent of HashMap iteration and database row IDs.
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

pub fn search(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    q: &str,
    options: &SearchOptions,
) -> Result<SearchResult> {
    file_role::validate_all(&options.file_roles)?;
    file_role::validate_all(&options.expansion.file_roles)?;
    origin::validate_all(&options.file_origins)?;
    origin::validate_all(&options.expansion.file_origins)?;
    validate_code_formats(&options.formats)?;
    if options.memory_graph_depth > MAX_MEMORY_GRAPH_DEPTH {
        anyhow::bail!("memory graph depth must be at most {MAX_MEMORY_GRAPH_DEPTH}");
    }
    if options.memory_graph_node_limit == 0
        || options.memory_graph_node_limit > MAX_MEMORY_GRAPH_NODE_LIMIT
    {
        anyhow::bail!(
            "memory graph node limit must be between 1 and {MAX_MEMORY_GRAPH_NODE_LIMIT}"
        );
    }
    if options.include_memory && (options.memory_limit == 0 || options.memory_limit > 100) {
        anyhow::bail!("memory limit must be between 1 and 100");
    }
    let exhaustive_cursor = match &options.mode {
        SearchMode::Ranked => None,
        SearchMode::Exhaustive { cursor } => {
            if options.limit == 0 || options.limit > MAX_EXHAUSTIVE_PAGE_SIZE {
                anyhow::bail!(
                    "exhaustive search page size must be between 1 and {MAX_EXHAUSTIVE_PAGE_SIZE}"
                );
            }
            if provider.is_some() || options.rerank || options.expand || options.include_memory {
                anyhow::bail!(
                    "exhaustive search requires vector, rerank, expand, and include_memory to be disabled"
                );
            }
            cursor.as_deref()
        }
    };
    store::with_read_snapshot(conn, "jscout_search", || {
        let identity = Identities::read(conn)?.response(Plane::Code);
        let snapshot = &identity.snapshot;
        let (hits, retrieval, exhaustive_state) =
            if matches!(&options.mode, SearchMode::Exhaustive { .. }) {
                let (hits, state) = exhaustive_hits(conn, q, options, snapshot, exhaustive_cursor)?;
                (hits, RetrievalStatus::vector_disabled(), Some(state))
            } else {
                let (hits, retrieval) = ranked_hits(conn, provider, q, options)?;
                (hits, retrieval, None)
            };
        let (semantic_artifacts, semantic_retrieval, semantic_attachment, semantic_candidates) =
            if options.include_memory {
                let candidate_limit = options.memory_limit.saturating_mul(8).clamp(1, 100);
                let (artifacts, retrieval, candidates) =
                    semantic::search_with_provider(conn, provider, q, candidate_limit)?;
                let (artifacts, attachment) = select_attached_memory(
                    conn,
                    artifacts,
                    &hits,
                    options.memory_limit,
                    options.memory_graph_depth,
                    options.memory_graph_node_limit,
                    &options.file_origins,
                )?;
                let attachment = (retrieval.corpus_artifacts > 0).then_some(attachment);
                (artifacts, Some(retrieval), attachment, candidates)
            } else {
                (Vec::new(), None, None, 0)
            };
        let semantic_selected = semantic_artifacts.len();
        let expansion = options
            .expand
            .then(|| expand_hits(conn, snapshot, &hits, &options.expansion, options.compact))
            .transpose()?;
        let exhaustive = exhaustive_state
            .as_ref()
            .map(|state| ExhaustiveSearchMetadata {
                total_chunks: state.total_chunks,
                returned: hits.len(),
                truncated: state.has_more,
                next_cursor: None,
                warnings: exhaustive_warnings(q, state.total_chunks, exhaustive_cursor.is_none()),
                effective: EffectiveSearchPosture {
                    vector: false,
                    rerank: false,
                    expand: false,
                    include_memory: false,
                    page_size: options.limit,
                },
                scope: state.scope.clone(),
                request_fingerprint: state.request_fingerprint.clone(),
                selected_positions: state.selected_positions.clone(),
                page_had_more: state.has_more,
            });
        let mut result = SearchResult {
            snapshot: identity.snapshot,
            publication_snapshot: identity.publication_snapshot,
            exhaustive,
            retrieval,
            hits,
            semantic_artifacts,
            semantic_retrieval,
            semantic_attachment,
            semantic_candidates,
            semantic_selected,
            expansion,
            response_budget: ResponseBudget {
                byte_limit: options.response_byte_limit,
                ..Default::default()
            },
        };
        apply_response_budget(&mut result, options.compact)?;
        Ok(result)
    })
}

/// Count the repository-wide same-name reference occurrences that the compact
/// search surface deliberately no longer labels as exact callers.
///
/// This reproduces the former top-three-symbol diagnostic for telemetry only.
/// It is intentionally approximate: references are matched by name, not by a
/// resolved declaration anchor, and the same reference may contribute to more
/// than one returned hit. Callers should never render this as `used_by`.
pub(crate) fn approximate_name_usage_occurrences(conn: &Connection, hits: &[Hit]) -> Result<usize> {
    let mut symbols_stmt = conn.prepare_cached("SELECT symbols FROM chunks WHERE id = ?1")?;
    let mut count_stmt =
        conn.prepare_cached("SELECT COUNT(*) FROM refs WHERE target_name = ?1 AND chunk_id != ?2")?;
    let mut total = 0_u64;
    for hit in hits {
        let symbols: String = symbols_stmt.query_row([hit.chunk_id], |row| row.get(0))?;
        for symbol in symbols.split_whitespace().take(3) {
            let count: i64 =
                count_stmt.query_row(rusqlite::params![symbol, hit.chunk_id], |row| row.get(0))?;
            total = total.saturating_add(count.max(0) as u64);
        }
    }
    Ok(usize::try_from(total).unwrap_or(usize::MAX))
}

#[derive(Debug)]
struct ConnectedMemoryCandidate {
    artifact: semantic::SemanticArtifact,
    tier: u8,
    rank: usize,
    support_key: Option<String>,
}

fn select_attached_memory(
    conn: &Connection,
    artifacts: Vec<semantic::SemanticArtifact>,
    hits: &[Hit],
    limit: usize,
    graph_depth: usize,
    graph_node_limit: usize,
    file_origins: &[String],
) -> Result<(Vec<semantic::SemanticArtifact>, MemoryAttachmentStatus)> {
    let mut seed_keys = Vec::new();
    let mut seen_seeds = HashSet::new();
    let mut hit_files = HashSet::new();
    for hit in hits {
        hit_files.insert(hit.file.as_str());
        for anchor in hit.anchors.iter().chain(hit.file_anchor.iter()) {
            if seen_seeds.insert(anchor.as_str()) {
                seed_keys.push(anchor.clone());
            }
        }
    }
    let (distances, graph_truncated) = memory_graph_distances(
        conn,
        &seed_keys,
        graph_depth,
        graph_node_limit,
        file_origins,
    )?;

    let mut connected = Vec::new();
    let mut unconnected = Vec::new();
    for (rank, artifact) in artifacts.into_iter().enumerate() {
        let direct = artifact.supports.iter().find(|support| {
            seen_seeds.contains(support.anchor.as_str())
                || hit_files.contains(support.evidence_file.as_str())
        });
        if let Some(support) = direct {
            connected.push(ConnectedMemoryCandidate {
                support_key: Some(support.anchor.clone()),
                artifact,
                tier: 0,
                rank,
            });
            continue;
        }
        let nearby = artifact
            .supports
            .iter()
            .filter_map(|support| {
                distances
                    .get(&support.anchor)
                    .copied()
                    .filter(|distance| *distance > 0 && *distance <= graph_depth)
                    .map(|distance| (distance, support.anchor.clone()))
            })
            .min_by_key(|(distance, _)| *distance);
        if let Some((_, support_key)) = nearby {
            connected.push(ConnectedMemoryCandidate {
                artifact,
                tier: 1,
                rank,
                support_key: Some(support_key),
            });
        } else {
            unconnected.push((rank, artifact));
        }
    }

    if !connected.is_empty() && !unconnected.is_empty() {
        let base_ids = connected
            .iter()
            .map(|candidate| candidate.artifact.id)
            .collect::<HashSet<_>>();
        let candidate_ids = unconnected
            .iter()
            .map(|(_, artifact)| artifact.id)
            .chain(base_ids.iter().copied())
            .collect::<Vec<_>>();
        let candidate_json = serde_json::to_string(&candidate_ids)?;
        let mut related_to_base = HashMap::<i64, i64>::new();
        let mut statement = conn.prepare(
            "SELECT src_artifact_id,dst_artifact_id FROM semantic_relations
             WHERE src_artifact_id IN (SELECT value FROM json_each(?1))
                OR dst_artifact_id IN (SELECT value FROM json_each(?1))
             ORDER BY src_artifact_id,dst_artifact_id",
        )?;
        let rows = statement.query_map([candidate_json], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (src, dst) = row?;
            if base_ids.contains(&src) {
                related_to_base.entry(dst).or_insert(src);
            }
            if base_ids.contains(&dst) {
                related_to_base.entry(src).or_insert(dst);
            }
        }
        for (rank, artifact) in unconnected {
            if let Some(base_id) = related_to_base.get(&artifact.id) {
                connected.push(ConnectedMemoryCandidate {
                    artifact,
                    tier: 2,
                    rank,
                    support_key: Some(format!("artifact:{base_id}")),
                });
            }
        }
    }

    let connected_candidates = connected.len();
    connected.sort_by(|left, right| {
        left.tier.cmp(&right.tier).then_with(|| {
            let left_score = left
                .artifact
                .retrieval_score
                .as_ref()
                .map_or(0.0, |score| score.rank_score);
            let right_score = right
                .artifact
                .retrieval_score
                .as_ref()
                .map_or(0.0, |score| score.rank_score);
            right_score
                .total_cmp(&left_score)
                .then(left.rank.cmp(&right.rank))
        })
    });
    let selected = diversify_memory_ties(connected, limit)
        .into_iter()
        .map(|candidate| candidate.artifact)
        .collect::<Vec<_>>();
    Ok((
        selected,
        MemoryAttachmentStatus {
            status: if connected_candidates == 0 {
                "no_connected_memory"
            } else {
                "connected"
            },
            connected_candidates,
            graph_depth,
            graph_nodes: distances.len(),
            graph_truncated,
        },
    ))
}

fn diversify_memory_ties(
    candidates: Vec<ConnectedMemoryCandidate>,
    limit: usize,
) -> Vec<ConnectedMemoryCandidate> {
    let mut selected = Vec::new();
    let mut seen_types = HashSet::new();
    let mut seen_supports = HashSet::new();
    let mut cursor = 0;
    while cursor < candidates.len() && selected.len() < limit {
        let tier = candidates[cursor].tier;
        let score = candidates[cursor]
            .artifact
            .retrieval_score
            .as_ref()
            .map_or(0.0, |score| score.rank_score);
        let mut end = cursor + 1;
        while end < candidates.len()
            && candidates[end].tier == tier
            && candidates[end]
                .artifact
                .retrieval_score
                .as_ref()
                .map_or(0.0, |score| score.rank_score)
                .to_bits()
                == score.to_bits()
        {
            end += 1;
        }
        let mut group = candidates[cursor..end].iter().collect::<Vec<_>>();
        while !group.is_empty() && selected.len() < limit {
            let mut choice = 0;
            let mut best_novelty = 0;
            for (index, candidate) in group.iter().enumerate() {
                let novelty = usize::from(!seen_types.contains(&candidate.artifact.artifact_type))
                    + usize::from(
                        candidate
                            .support_key
                            .as_ref()
                            .is_some_and(|key| !seen_supports.contains(key)),
                    );
                if novelty > best_novelty {
                    choice = index;
                    best_novelty = novelty;
                }
            }
            let candidate = group.remove(choice);
            seen_types.insert(candidate.artifact.artifact_type.clone());
            if let Some(key) = &candidate.support_key {
                seen_supports.insert(key.clone());
            }
            selected.push(candidate.rank);
        }
        cursor = end;
    }
    let mut by_rank = candidates
        .into_iter()
        .map(|candidate| (candidate.rank, candidate))
        .collect::<HashMap<_, _>>();
    selected
        .into_iter()
        .filter_map(|rank| by_rank.remove(&rank))
        .collect()
}

fn memory_graph_distances(
    conn: &Connection,
    seeds: &[String],
    max_depth: usize,
    node_limit: usize,
    file_origins: &[String],
) -> Result<(HashMap<String, usize>, bool)> {
    let mut distances = HashMap::new();
    let mut frontier = Vec::new();
    let mut truncated = false;
    for seed in seeds {
        if distances.len() == node_limit {
            truncated = true;
            break;
        }
        if distances.insert(seed.clone(), 0).is_none() {
            frontier.push(seed.clone());
        }
    }
    if max_depth == 0 || frontier.is_empty() {
        return Ok((distances, truncated));
    }
    let origins_json = serde_json::to_string(file_origins)?;
    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for frontier_chunk in frontier.chunks(400) {
            let frontier_json = serde_json::to_string(frontier_chunk)?;
            let row_limit = node_limit.saturating_mul(4).saturating_add(1) as i64;
            let mut statement = conn.prepare_cached(
                "SELECT edge.src_key,edge.dst_key
                 FROM resolved_edges edge
                 JOIN graph_nodes src ON src.node_key=edge.src_key
                 JOIN graph_nodes dst ON dst.node_key=edge.dst_key
                 LEFT JOIN files src_file ON src_file.id=src.file_id
                 LEFT JOIN files dst_file ON dst_file.id=dst.file_id
                 WHERE edge.confidence IN ('certain','likely')
                   AND (edge.src_key IN (SELECT value FROM json_each(?1))
                     OR edge.dst_key IN (SELECT value FROM json_each(?1)))
                   AND (src.file_id IS NULL
                     OR src_file.origin IN (SELECT value FROM json_each(?2)))
                   AND (dst.file_id IS NULL
                     OR dst_file.origin IN (SELECT value FROM json_each(?2)))
                 ORDER BY edge.id
                 LIMIT ?3",
            )?;
            let rows = statement.query_map(
                rusqlite::params![frontier_json, origins_json, row_limit],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            let frontier_set = frontier_chunk
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut row_count = 0usize;
            for row in rows {
                row_count += 1;
                let (src, dst) = row?;
                let neighbor = if frontier_set.contains(src.as_str()) {
                    dst
                } else if frontier_set.contains(dst.as_str()) {
                    src
                } else {
                    continue;
                };
                if distances.contains_key(&neighbor) {
                    continue;
                }
                if distances.len() == node_limit {
                    truncated = true;
                    continue;
                }
                distances.insert(neighbor.clone(), depth);
                next.push(neighbor);
            }
            if row_count >= row_limit as usize {
                truncated = true;
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok((distances, truncated))
}

fn ranked_hits(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    q: &str,
    options: &SearchOptions,
) -> Result<(Vec<Hit>, RetrievalStatus)> {
    let timing = options.timing;
    let (pool, vector_pool) = candidate_pool_limits(options.limit, !options.file_roles.is_empty());
    let exact = exact_intent_candidates(
        conn,
        q,
        options.limit,
        &options.file_roles,
        &options.file_origins,
        &options.formats,
    )?;
    let t0 = std::time::Instant::now();
    let mut rankings = vec![bm25_ranking(
        conn,
        q,
        pool,
        &options.file_roles,
        &options.file_origins,
        &options.formats,
    )?];
    let mut retrieval = RetrievalStatus::vector_disabled();
    if timing {
        eprintln!("timing: bm25 {:?}", t0.elapsed());
    }
    if let Some(p) = provider.filter(|_| format_scope_supports_code_vectors(&options.formats)) {
        let t = std::time::Instant::now();
        retrieval = record_vector_ranking(
            &mut rankings,
            vector_ranking(
                conn,
                p,
                q,
                vector_pool,
                &options.file_origins,
                &options.formats,
            ),
        );
        if timing {
            eprintln!("timing: embed-query+sqlite-vec {:?}", t.elapsed());
        }
    }
    for ranking in &mut rankings {
        prefilter_ranking_by_role(conn, ranking, &options.file_roles)?;
        ranking.truncate(pool);
    }
    let mut fused = rrf(&rankings, 60.0);

    // Cross-encoder rerank of the configured candidate prefix.
    if options.rerank
        && let Some(reranker) = options.reranker.as_ref()
    {
        let started = std::time::Instant::now();
        let top: Vec<(i64, String)> = fused
            .iter()
            .take(reranker.pool)
            .map(|(id, _)| reranker_document(conn, *id, reranker.max_chars))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        if !top.is_empty() {
            match reranker.rerank(q, &top) {
                Ok(reranked) if !reranked.is_empty() => {
                    fused = merge_reranked_prefix(&fused, reranked);
                    retrieval.reranker_active();
                }
                Ok(_) => {}
                Err(e) => {
                    retrieval.reranker_degraded();
                    eprintln!("rerank unavailable, using RRF order: {e}");
                }
            }
            let elapsed = started.elapsed();
            if timing {
                eprintln!("timing: rerank({}) {elapsed:?}", top.len());
            }
            retrieval.reranker_timing = Some(elapsed);
        }
    }
    if options.file_roles.is_empty() {
        apply_repository_policy_penalty(conn, &mut fused)?;
    }
    let ranked = tiered_candidates(exact, &fused);

    let mut hits = Vec::new();
    let allowed_roles: HashSet<&str> = options.file_roles.iter().map(String::as_str).collect();
    for candidate in ranked {
        if let Some(hit) = load_hit(conn, candidate, q, &options.file_origins, &options.formats)? {
            if !allowed_roles.is_empty() && !allowed_roles.contains(hit.file_role.as_str()) {
                continue;
            }
            hits.push(hit);
            if hits.len() >= options.limit {
                break;
            }
        }
    }
    Ok((hits, retrieval))
}

fn apply_repository_policy_penalty(conn: &Connection, ranking: &mut Vec<(i64, f64)>) -> Result<()> {
    let mut weighted = ranking
        .iter()
        .enumerate()
        .map(|(rank, &(chunk_id, score))| {
            Ok((
                chunk_id,
                score,
                crate::recon::chunk_policy_penalty(conn, chunk_id)? / (rank as f64 + 1.0),
                rank,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    weighted.sort_by(|left, right| {
        right
            .2
            .total_cmp(&left.2)
            .then_with(|| left.3.cmp(&right.3))
    });
    *ranking = weighted
        .into_iter()
        .map(|(chunk_id, score, _, _)| (chunk_id, score))
        .collect();
    Ok(())
}

fn candidate_pool_limits(limit: usize, role_filtered: bool) -> (usize, usize) {
    let pool = limit.max(10).saturating_mul(5);
    // sqlite-vec applies KNN's k before jscout can inspect the joined file role.
    // Fetch a bounded surplus so selective role filters do not starve the vector
    // ranking and accidentally tilt RRF toward BM25.
    let vector_pool = if role_filtered {
        pool.saturating_mul(4)
    } else {
        pool
    };
    (pool, vector_pool)
}

pub(crate) fn merge_reranked_prefix(
    fused: &[(i64, f64)],
    mut reranked: Vec<(i64, f64)>,
) -> Vec<(i64, f64)> {
    let reranked_ids = reranked
        .iter()
        .map(|(chunk_id, _)| *chunk_id)
        .collect::<HashSet<_>>();
    reranked.extend(
        fused
            .iter()
            .filter(|(chunk_id, _)| !reranked_ids.contains(chunk_id))
            .copied(),
    );
    reranked
}

fn prefilter_ranking_by_role(
    conn: &Connection,
    ranking: &mut Vec<(i64, f64)>,
    file_roles: &[String],
) -> Result<()> {
    if file_roles.is_empty() {
        return Ok(());
    }
    let allowed = file_roles
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut filtered = Vec::with_capacity(ranking.len());
    let mut role_statement = conn.prepare_cached(
        "SELECT file.role
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE chunk.id=?1",
    )?;
    for &(chunk_id, score) in ranking.iter() {
        let role = role_statement.query_row([chunk_id], |row| row.get::<_, String>(0));
        if role.is_ok_and(|role| allowed.contains(role.as_str())) {
            filtered.push((chunk_id, score));
        }
    }
    *ranking = filtered;
    Ok(())
}

fn reranker_document(
    conn: &Connection,
    chunk_id: i64,
    max_chars: usize,
) -> Result<Option<(i64, String)>> {
    let row = conn.query_row(
        "SELECT file.path, file.role, file.origin, chunk.kind, chunk.name,
                chunk.start_line, chunk.end_line, chunk.content, package.name,
                policy.scope_role, policy.effective_role
         FROM chunks chunk
         JOIN files file ON file.id=chunk.file_id
         LEFT JOIN package_instances package ON package.id=file.package_instance_id
         LEFT JOIN repository_file_policy policy ON policy.file_id=file.id
         WHERE chunk.id=?1",
        [chunk_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    );
    let (
        file,
        deterministic_role,
        origin,
        kind,
        name,
        start_line,
        end_line,
        content,
        package,
        scope_role,
        effective_role,
    ) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let scope = package.unwrap_or_else(|| {
        file.split_once('/')
            .map_or_else(|| "(root)".to_string(), |(scope, _)| scope.to_string())
    });
    let symbol = name.as_deref().unwrap_or("(anonymous)");
    let role = effective_role.as_deref().unwrap_or(&deterministic_role);
    let role_context = scope_role.as_ref().map_or_else(String::new, |scope_role| {
        format!("\ndeterministic_role: {deterministic_role}\nscouted_scope_role: {scope_role}")
    });
    let mut document = format!(
        "path: {file}\nscope: {scope}\nsymbol: {symbol}\nkind: {kind}\nrole: {role}{role_context}\norigin: {origin}\nlines: {start_line}-{end_line}\n\n{content}"
    );
    truncate_utf8(&mut document, max_chars);
    Ok(Some((chunk_id, document)))
}

fn record_vector_ranking(
    rankings: &mut Vec<Vec<(i64, f64)>>,
    result: Result<embed::VectorSearchResult>,
) -> RetrievalStatus {
    match result {
        Ok(output) => {
            rankings.push(output.ranking);
            let mut retrieval = RetrievalStatus::vector_active();
            retrieval.vector_timings = Some(output.timings);
            retrieval
        }
        Err(error) => {
            eprintln!("vector search unavailable: {error}");
            RetrievalStatus::vector_degraded(embed::code_vector_failure_action(&error))
        }
    }
}

fn load_hit(
    conn: &Connection,
    candidate: RankedHitCandidate,
    query: &str,
    file_origins: &[String],
    file_formats: &[String],
) -> Result<Option<Hit>> {
    let RankedHitCandidate {
        chunk_id,
        score,
        match_reason,
        matched_identifiers,
    } = candidate;
    let row = conn
        .query_row(
            "SELECT f.path, f.role, f.origin, c.kind, c.name, c.start_line, c.end_line,
                    c.content, policy.effective_role, f.format
             FROM chunks c
             JOIN files f ON c.file_id = f.id
             LEFT JOIN repository_file_policy policy ON policy.file_id=f.id
             WHERE c.id = ?1",
            [chunk_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        )
        .ok();
    let Some((
        file,
        role,
        file_origin,
        kind,
        name,
        start_line,
        end_line,
        content,
        repository_role,
        format,
    )) = row
    else {
        return Ok(None);
    };
    if !file_formats.is_empty() && !file_formats.iter().any(|candidate| candidate == &format) {
        return Ok(None);
    }
    let structurally_eligible =
        crate::formats::by_id(&format).is_some_and(|format| format.structural_eligible());
    let anchors = if structurally_eligible {
        project_chunk_anchors(conn, chunk_id, &file, name.as_deref())?
    } else {
        Vec::new()
    };
    let file_anchor = structurally_eligible.then(|| format!("file:{file}"));

    // Outgoing: what this chunk calls/renders (top few, certain only).
    let uses: Vec<String> = if structurally_eligible {
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT target_name || ' (' || kind || ')' FROM refs
             WHERE chunk_id = ?1 AND kind IN ('call','render','extend') AND confidence='certain'
             LIMIT 6",
        )?;
        let rows = stmt.query_map([chunk_id], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        Vec::new()
    };

    // Only label anchor-resolved incoming edges as `used_by`. Repository-wide
    // same-name reference counts are not callers of this exact declaration.
    let used_by = match anchors.as_slice() {
        [anchor] if anchor.starts_with("sym:") => {
            let count = query::who_uses_anchor_in_scope(conn, anchor, file_origins, file_formats)?
                .into_iter()
                .filter(|usage| usage.file != file)
                .count();
            if count == 0 {
                Vec::new()
            } else {
                vec![format!(
                    "{}: {count} sites",
                    name.as_deref().unwrap_or(anchor)
                )]
            }
        }
        _ => Vec::new(),
    };

    let excerpt = snippet::select(conn, chunk_id, &content, query, &matched_identifiers)?;
    Ok(Some(Hit {
        chunk_id,
        file,
        file_role: role,
        repository_role,
        file_origin,
        kind,
        name,
        start_line,
        end_line,
        score,
        match_reason,
        matched_identifiers,
        match_lines: None,
        snippet: excerpt.text,
        snippet_line: (excerpt.line_offset > 0).then_some(start_line + excerpt.line_offset as i64),
        snippet_truncated: false,
        anchors,
        file_anchor,
        uses,
        used_by,
    }))
}

fn refresh_exhaustive_metadata(result: &mut SearchResult) {
    let Some(metadata) = result.exhaustive.as_ref() else {
        return;
    };
    let returned = result.hits.len();
    let truncated = metadata.page_had_more || returned < metadata.selected_positions.len();
    let next_cursor = if truncated {
        returned
            .checked_sub(1)
            .and_then(|index| metadata.selected_positions.get(index))
            .map(|position| {
                encode_exhaustive_cursor(&result.snapshot, &metadata.request_fingerprint, position)
            })
    } else {
        None
    };
    let metadata = result
        .exhaustive
        .as_mut()
        .expect("checked exhaustive metadata");
    metadata.returned = returned;
    metadata.truncated = truncated;
    metadata.next_cursor = next_cursor;
}

fn apply_response_budget(result: &mut SearchResult, compact: bool) -> Result<()> {
    let byte_limit = result.response_budget.byte_limit;
    if byte_limit == 0 {
        anyhow::bail!("response byte limit must be greater than zero");
    }
    let exhaustive_baseline = result.exhaustive.is_some().then(|| result.clone());
    if apply_response_budget_once(result, compact)? {
        return Ok(());
    }
    let rendered = result.response_budget.rendered_bytes;
    if let Some(baseline) = exhaustive_baseline {
        let minimum_bytes = minimum_exhaustive_response_bytes(&baseline, compact)?;
        return Err(ResponseBudgetTooSmall {
            byte_limit,
            minimum_bytes,
        }
        .into());
    }
    anyhow::bail!(
        "response byte limit {byte_limit} is below the minimum search envelope ({rendered} bytes)"
    );
}

/// Apply one deterministic shedding pass. `false` means no retained response
/// fits the requested limit; the caller decides which public error to return.
fn apply_response_budget_once(result: &mut SearchResult, compact: bool) -> Result<bool> {
    let byte_limit = result.response_budget.byte_limit;

    refresh_exhaustive_metadata(result);
    capture_unbudgeted_bytes(result, compact)?;

    // Search attaches semantic memory as secondary context. A stored artifact
    // can carry up to 160 evidence rows, but rendering all of them here can
    // consume the whole response before primary code or explicitly requested
    // structural context appears. The dedicated semantic-memory surface owns
    // deeper evidence retrieval; search keeps one global, fairly distributed
    // preview rather than multiplying the cap by the number of artifacts.
    if !compact {
        cap_semantic_supports(result, DEFAULT_TOTAL_RENDERED_SUPPORT_LIMIT);
    }

    loop {
        refresh_exhaustive_metadata(result);
        let rendered = settle_search_response(result, compact)?;
        if rendered <= byte_limit {
            break;
        }
        result.response_budget.truncated = true;

        // Semantic memory is an untrusted, optional attachment to search. Shed
        // lower-ranked artifacts and redundant evidence before source-backed
        // code hits or an explicitly requested structural expansion.
        if result.semantic_artifacts.len() > 1 {
            result.semantic_artifacts.pop();
            result.response_budget.omitted_semantic_artifacts += 1;
            continue;
        }
        if let Some(artifact) = result
            .semantic_artifacts
            .iter_mut()
            .rev()
            .find(|artifact| artifact.supports.len() > 1)
        {
            artifact.supports.pop();
            result.response_budget.omitted_semantic_supports += 1;
            continue;
        }
        if result.semantic_artifacts.pop().is_some() {
            result.response_budget.omitted_semantic_artifacts += 1;
            continue;
        }

        if let Some(expansion) = result.expansion.as_mut() {
            // Edges are relevance-sorted, so the tail is the least useful
            // relationship. Remove it before its now-unreferenced endpoint;
            // node-first shedding produced relationship-free context packs.
            if expansion.edges.pop().is_some() {
                expansion.truncated = true;
                expansion.omitted_edges += 1;
                result.response_budget.omitted_edges += 1;
                let omitted_nodes = prune_expansion_nodes(expansion);
                expansion.omitted_nodes += omitted_nodes;
                result.response_budget.omitted_nodes += omitted_nodes;
                refresh_expansion_path_counts(expansion);
                expansion.payload_bytes = expansion_payload_bytes(expansion, compact)?;
                continue;
            }
            if let Some(index) = expansion
                .nodes
                .iter()
                .rposition(|node| !expansion.seeds.contains(&node.key))
            {
                expansion.nodes.remove(index);
                expansion.truncated = true;
                expansion.omitted_nodes += 1;
                result.response_budget.omitted_nodes += 1;
                expansion.payload_bytes = expansion_payload_bytes(expansion, compact)?;
                continue;
            }
        }

        // Shed only from the tail: ranked search keeps its top hit, while an
        // exhaustive page keeps a prefix whose cursor identifies the last
        // rendered hit.
        if result.hits.len() > 1 {
            result.hits.pop();
            result.response_budget.omitted_hits += 1;
            continue;
        }

        if let Some(hit) = result
            .hits
            .iter_mut()
            .rev()
            .find(|hit| !hit.used_by.is_empty())
        {
            hit.used_by.pop();
            continue;
        }
        if let Some(hit) = result
            .hits
            .iter_mut()
            .rev()
            .find(|hit| !hit.uses.is_empty())
        {
            hit.uses.pop();
            continue;
        }
        if result.exhaustive.is_none()
            && let Some(hit) = result
                .hits
                .iter_mut()
                .rev()
                .find(|hit| hit.anchors.len() > 1)
        {
            hit.anchors.pop();
            continue;
        }

        if let Some((index, _)) = result
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| !hit.snippet.is_empty())
            .max_by_key(|(_, hit)| hit.snippet.len())
        {
            let overshoot = rendered.saturating_sub(byte_limit);
            let hit = &mut result.hits[index];
            let target = hit.snippet.len().saturating_sub(overshoot.max(128));
            truncate_utf8(&mut hit.snippet, target);
            if !hit.snippet_truncated {
                hit.snippet_truncated = true;
                result.response_budget.truncated_snippets += 1;
            }
            continue;
        }

        settle_search_response(result, compact)?;
        return Ok(false);
    }

    refresh_exhaustive_metadata(result);
    settle_search_response(result, compact)?;
    Ok(true)
}

fn cap_semantic_supports(result: &mut SearchResult, limit: usize) {
    let original = result
        .semantic_artifacts
        .iter()
        .map(|artifact| artifact.supports.len())
        .sum::<usize>();
    if original <= limit {
        return;
    }

    let mut keep = vec![0_usize; result.semantic_artifacts.len()];
    let mut remaining = limit;
    // Preserve one evidence row per returned artifact before allocating a
    // second row. Artifact and support order are already deterministic rank
    // order, so this is stable while avoiding first-artifact monopolization.
    while remaining > 0 {
        let mut allocated = false;
        for (index, artifact) in result.semantic_artifacts.iter().enumerate() {
            if keep[index] < artifact.supports.len() {
                keep[index] += 1;
                remaining -= 1;
                allocated = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !allocated {
            break;
        }
    }
    for (artifact, count) in result.semantic_artifacts.iter_mut().zip(keep) {
        artifact.supports.truncate(count);
    }
    result.response_budget.omitted_semantic_supports += original - limit;
    result.response_budget.truncated = true;
}

fn prune_expansion_nodes(expansion: &mut SearchExpansion) -> usize {
    let referenced = expansion
        .edges
        .iter()
        .flat_map(|edge| [edge.source.as_str(), edge.target.as_str()])
        .collect::<HashSet<_>>();
    let original = expansion.nodes.len();
    expansion.nodes.retain(|node| {
        expansion.seeds.contains(&node.key) || referenced.contains(node.key.as_str())
    });
    original - expansion.nodes.len()
}

fn refresh_expansion_path_counts(expansion: &mut SearchExpansion) {
    if expansion.projection != ExpansionProjection::Paths {
        return;
    }
    let retained = expansion
        .edges
        .iter()
        .map(edge_identity)
        .collect::<HashSet<_>>();
    expansion.selected_paths = expansion
        .selected_path_edges
        .iter()
        .filter(|path| path.iter().all(|edge| retained.contains(edge)))
        .count();
    expansion.omitted_paths = expansion
        .candidate_paths
        .saturating_sub(expansion.selected_paths);
}

fn settle_rendered_bytes(result: &mut SearchResult, compact: bool) -> Result<usize> {
    for _ in 0..8 {
        let rendered = rendered_bytes(result, compact)?;
        if result.response_budget.rendered_bytes == rendered {
            return Ok(rendered);
        }
        result.response_budget.rendered_bytes = rendered;
    }
    rendered_bytes(result, compact)
}

/// Settle the exact transport that the caller will receive. Diagnostic output
/// includes compact-section accounting, so placeholder widths cannot be used
/// to decide whether a response fits or to advertise a retry floor.
fn settle_search_response(result: &mut SearchResult, compact: bool) -> Result<usize> {
    if compact {
        return settle_rendered_bytes(result, true);
    }
    for _ in 0..8 {
        let sections = crate::compact::search_section_bytes(result)?;
        result.response_budget.transport_sections = Some(sections);
        let rendered = settle_rendered_bytes(result, false)?;
        if crate::compact::search_section_bytes(result)? == sections {
            return Ok(rendered);
        }
    }
    let sections = crate::compact::search_section_bytes(result)?;
    result.response_budget.transport_sections = Some(sections);
    settle_rendered_bytes(result, false)
}

fn capture_unbudgeted_bytes(result: &mut SearchResult, compact: bool) -> Result<usize> {
    for _ in 0..8 {
        let rendered = settle_search_response(result, compact)?;
        if result.response_budget.unbudgeted_bytes == rendered {
            return Ok(rendered);
        }
        result.response_budget.unbudgeted_bytes = rendered;
    }
    settle_search_response(result, compact)
}

/// Find the first byte limit for which the real budgeting pass succeeds. The
/// baseline is cloned before any shedding, so initial and empty responses are
/// candidates and every probe recomputes retry-dependent diagnostic fields.
fn minimum_exhaustive_response_bytes(baseline: &SearchResult, compact: bool) -> Result<usize> {
    let mut upper_candidate = baseline.clone();
    upper_candidate.response_budget.byte_limit = usize::MAX;
    if !apply_response_budget_once(&mut upper_candidate, compact)? {
        anyhow::bail!("exhaustive response did not fit the maximum byte limit");
    }
    let mut lower = 1_usize;
    let mut upper = upper_candidate.response_budget.rendered_bytes;
    while lower < upper {
        let candidate_limit = lower + (upper - lower) / 2;
        let mut candidate = baseline.clone();
        candidate.response_budget.byte_limit = candidate_limit;
        if apply_response_budget_once(&mut candidate, compact)? {
            upper = candidate_limit;
        } else {
            lower = candidate_limit + 1;
        }
    }
    Ok(lower)
}

fn rendered_bytes(result: &SearchResult, compact: bool) -> Result<usize> {
    if compact {
        crate::compact::search_rendered_bytes(result)
    } else {
        Ok(serde_json::to_string(result)?.len())
    }
}

fn expansion_payload_bytes(expansion: &SearchExpansion, compact: bool) -> Result<usize> {
    expansion_parts_bytes(
        &expansion.nodes,
        &expansion.edges,
        &expansion.seeds,
        compact,
    )
}

fn expansion_parts_bytes(
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    seeds: &[String],
    compact: bool,
) -> Result<usize> {
    if compact {
        return crate::compact::expansion_payload_bytes(nodes, edges, seeds);
    }
    let node_bytes = nodes
        .iter()
        .map(|node| serde_json::to_vec(node).map(|bytes| bytes.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let edge_bytes = edges
        .iter()
        .map(|edge| serde_json::to_vec(edge).map(|bytes| bytes.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(node_bytes.iter().sum::<usize>()
        + edge_bytes.iter().sum::<usize>()
        + node_bytes.len().saturating_sub(1)
        + edge_bytes.len().saturating_sub(1))
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
}

type ChunkAnchorCandidate = (String, String, String, String);

fn select_chunk_anchors(
    file: &str,
    chunk_name: Option<&str>,
    candidates: Vec<ChunkAnchorCandidate>,
) -> Vec<String> {
    if let Some(name) = chunk_name {
        let exact: Vec<String> = candidates
            .iter()
            .filter(|(_, symbol_name, symbol_scope, chunk_scope)| {
                symbol_name == name && symbol_scope == chunk_scope
            })
            .map(|(key, _, _, _)| key.clone())
            .collect();
        if !exact.is_empty() {
            return dedup(exact);
        }
    }
    let overlaps = dedup(candidates.into_iter().map(|(key, _, _, _)| key).collect());
    if overlaps.is_empty() {
        vec![format!("file:{file}")]
    } else {
        overlaps
    }
}

fn project_exhaustive_anchors(
    conn: &Connection,
    rows: &[ExhaustiveHitRow],
) -> Result<HashMap<i64, Vec<String>>> {
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    let chunk_ids =
        serde_json::to_string(&rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>())?;
    let structural_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::Structural);
    let mut stmt = conn.prepare(
        "SELECT c.id, g.node_key, s.name, s.scope_chain, c.scope_chain
         FROM chunks c
         JOIN files f ON f.id=c.file_id
         JOIN symbols s ON s.file_id=c.file_id
           AND s.decl_start < c.end AND s.decl_end > c.start
         JOIN graph_nodes g ON g.native_table='symbols' AND g.native_id=s.id
         WHERE c.id IN (SELECT value FROM json_each(?1))
           AND f.format IN (SELECT value FROM json_each(?2))
         ORDER BY c.id, s.decl_start, s.decl_end, g.node_key",
    )?;
    let candidates = stmt.query_map(rusqlite::params![chunk_ids, structural_formats], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut by_chunk: HashMap<i64, Vec<ChunkAnchorCandidate>> = HashMap::new();
    for candidate in candidates {
        let (chunk_id, key, name, symbol_scope, chunk_scope) = candidate?;
        by_chunk
            .entry(chunk_id)
            .or_default()
            .push((key, name, symbol_scope, chunk_scope));
    }
    let mut projected = HashMap::with_capacity(rows.len());
    for row in rows {
        let structurally_eligible =
            crate::formats::by_id(&row.format).is_some_and(|format| format.structural_eligible());
        projected.insert(
            row.chunk_id,
            if structurally_eligible {
                select_chunk_anchors(
                    &row.position.path,
                    row.name.as_deref(),
                    by_chunk.remove(&row.chunk_id).unwrap_or_default(),
                )
            } else {
                Vec::new()
            },
        );
    }
    Ok(projected)
}

fn project_chunk_anchors(
    conn: &Connection,
    chunk_id: i64,
    file: &str,
    chunk_name: Option<&str>,
) -> Result<Vec<String>> {
    let structural_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::Structural);
    let mut stmt = conn.prepare(
        "SELECT g.node_key, s.name, s.scope_chain, c.scope_chain
         FROM chunks c
         JOIN files f ON f.id=c.file_id
         JOIN symbols s ON s.file_id=c.file_id
           AND s.decl_start < c.end AND s.decl_end > c.start
         JOIN graph_nodes g ON g.native_table='symbols' AND g.native_id=s.id
         WHERE c.id=?1
           AND f.format IN (SELECT value FROM json_each(?2))
         ORDER BY s.decl_start, s.decl_end, g.node_key",
    )?;
    let rows = stmt.query_map(rusqlite::params![chunk_id, structural_formats], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let candidates = rows.collect::<std::result::Result<_, _>>()?;
    Ok(select_chunk_anchors(file, chunk_name, candidates))
}

fn dedup(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn expand_hits(
    conn: &Connection,
    snapshot: &str,
    hits: &[Hit],
    options: &ExpansionOptions,
    compact: bool,
) -> Result<SearchExpansion> {
    if options.seed_limit == 0
        || options.path_limit == 0
        || options.node_limit == 0
        || options.edge_limit == 0
        || options.byte_limit == 0
    {
        anyhow::bail!(
            "expansion seed, path, node, edge, and byte limits must be greater than zero"
        );
    }
    if options.path_limit > MAX_EXPANSION_PATH_LIMIT {
        anyhow::bail!("expansion path limit must be at most {MAX_EXPANSION_PATH_LIMIT}");
    }

    let mut seeds = Vec::new();
    let mut seen_seeds = HashSet::new();
    let allowed_roles: HashSet<&str> = options.file_roles.iter().map(String::as_str).collect();
    // Prefer declaration anchors across the ranked set. Import/module chunks
    // often outrank the implementation they mention; letting those file
    // fallbacks consume every seed would turn expansion into an import listing.
    for file_fallbacks in [false, true] {
        for hit in hits {
            if !allowed_roles.is_empty() && !allowed_roles.contains(hit.file_role.as_str()) {
                continue;
            }
            for anchor in &hit.anchors {
                if anchor.starts_with("file:") != file_fallbacks {
                    continue;
                }
                if seen_seeds.insert(anchor.clone()) {
                    seeds.push(anchor.clone());
                    if seeds.len() >= options.seed_limit {
                        break;
                    }
                }
            }
            if seeds.len() >= options.seed_limit {
                break;
            }
        }
        if seeds.len() >= options.seed_limit {
            break;
        }
    }

    let mut candidate_nodes: HashMap<String, structural::GraphNode> = HashMap::new();
    let mut candidate_edges: HashMap<EdgeIdentity, structural::GraphEdge> = HashMap::new();
    let mut truncated = false;
    for seed in &seeds {
        let neighborhood = structural::neighborhood(
            conn,
            seed,
            &structural::NeighborhoodOptions {
                expected_snapshot: Some(snapshot.to_string()),
                depth: options.depth,
                direction: "both".into(),
                node_limit: options.node_limit,
                edge_limit: options.edge_limit,
                min_confidence: options.min_confidence.clone(),
                kinds: Vec::new(),
                file_roles: options.file_roles.clone(),
                file_origins: options.file_origins.clone(),
                penalize_file_roles: true,
            },
        )?;
        truncated |= neighborhood.truncated;
        for node in neighborhood.nodes {
            candidate_nodes
                .entry(node.key.clone())
                .and_modify(|current| {
                    if node.relevance > current.relevance {
                        *current = node.clone();
                    }
                })
                .or_insert(node);
        }
        for edge in neighborhood.edges {
            let key = (
                edge.source.clone(),
                edge.target.clone(),
                edge.kind.clone(),
                edge.file.clone(),
                edge.line,
            );
            candidate_edges
                .entry(key)
                .and_modify(|current| {
                    if edge.relevance > current.relevance {
                        *current = edge.clone();
                    }
                })
                .or_insert(edge);
        }
    }

    let mut ranked_nodes: Vec<_> = candidate_nodes.into_values().collect();
    ranked_nodes.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut ranked_edges: Vec<_> = candidate_edges.into_values().collect();
    ranked_edges.sort_by(|a, b| {
        b.relevance.total_cmp(&a.relevance).then_with(|| {
            (&a.source, &a.kind, &a.target, &a.file, a.line)
                .cmp(&(&b.source, &b.kind, &b.target, &b.file, b.line))
        })
    });

    let candidate_node_count = ranked_nodes.len();
    let candidate_edge_count = ranked_edges.len();
    let selection = match options.projection {
        ExpansionProjection::Paths => {
            select_path_projection(&seeds, &ranked_nodes, &ranked_edges, options, compact)?
        }
        ExpansionProjection::Neighborhood => {
            // Diagnostic mode is a strict superset of the compact projection
            // under the same limits. Reserve the path forest first, then use
            // the remaining node/edge/byte budget for ranked fan-out.
            let paths =
                select_path_projection(&seeds, &ranked_nodes, &ranked_edges, options, compact)?;
            select_neighborhood_projection(
                &seeds,
                &ranked_nodes,
                &ranked_edges,
                options,
                compact,
                &paths,
            )?
        }
    };
    let nodes = selection.nodes;
    let edges = selection.edges;
    let omitted_nodes = candidate_node_count.saturating_sub(nodes.len());
    let omitted_edges = candidate_edge_count.saturating_sub(edges.len());
    truncated |= selection.truncated || omitted_nodes > 0 || omitted_edges > 0;
    let payload_bytes = expansion_parts_bytes(&nodes, &edges, &seeds, compact)?;
    Ok(SearchExpansion {
        projection: options.projection,
        seeds,
        nodes,
        edges,
        candidate_paths: selection.candidate_paths,
        selected_paths: selection.selected_paths,
        omitted_paths: selection
            .candidate_paths
            .saturating_sub(selection.selected_paths),
        omitted_nodes,
        omitted_edges,
        selected_path_edges: selection.selected_path_edges,
        node_limit: options.node_limit,
        edge_limit: options.edge_limit,
        file_roles: options.file_roles.clone(),
        file_origins: options.file_origins.clone(),
        payload_bytes,
        truncated,
    })
}

struct ExpansionSelection {
    nodes: Vec<structural::GraphNode>,
    edges: Vec<structural::GraphEdge>,
    candidate_paths: usize,
    selected_paths: usize,
    selected_path_edges: Vec<Vec<EdgeIdentity>>,
    truncated: bool,
}

fn select_neighborhood_projection(
    seeds: &[String],
    ranked_nodes: &[structural::GraphNode],
    ranked_edges: &[structural::GraphEdge],
    options: &ExpansionOptions,
    compact: bool,
    required: &ExpansionSelection,
) -> Result<ExpansionSelection> {
    let nodes_by_key = ranked_nodes
        .iter()
        .map(|node| (node.key.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let mut nodes = required.nodes.clone();
    let mut selected_node_keys = required
        .nodes
        .iter()
        .map(|node| node.key.clone())
        .collect::<HashSet<_>>();
    let mut edges = required.edges.clone();
    let mut selected_edge_keys = required
        .edges
        .iter()
        .map(edge_identity)
        .collect::<HashSet<_>>();
    let mut truncated = false;

    for edge in ranked_edges {
        if selected_edge_keys.contains(&edge_identity(edge)) {
            continue;
        }
        if edges.len() >= options.edge_limit {
            truncated = true;
            continue;
        }
        let mut candidate_nodes = nodes.clone();
        let mut candidate_keys = selected_node_keys.clone();
        let mut endpoints_available = true;
        for key in [&edge.source, &edge.target] {
            if candidate_keys.contains(key) {
                continue;
            }
            let Some(node) = nodes_by_key.get(key) else {
                endpoints_available = false;
                break;
            };
            candidate_keys.insert(key.clone());
            candidate_nodes.push(node.clone());
        }
        if !endpoints_available || candidate_nodes.len() > options.node_limit {
            truncated = true;
            continue;
        }
        let mut candidate_edges = edges.clone();
        candidate_edges.push(edge.clone());
        if expansion_parts_bytes(&candidate_nodes, &candidate_edges, seeds, compact)?
            > options.byte_limit
        {
            truncated = true;
            continue;
        }
        nodes = candidate_nodes;
        selected_node_keys = candidate_keys;
        selected_edge_keys.insert(edge_identity(edge));
        edges = candidate_edges;
    }

    // Diagnostic neighborhood mode retains high-relevance standalone nodes.
    for node in ranked_nodes {
        if selected_node_keys.contains(&node.key) {
            continue;
        }
        let mut candidate_nodes = nodes.clone();
        candidate_nodes.push(node.clone());
        if candidate_nodes.len() <= options.node_limit
            && expansion_parts_bytes(&candidate_nodes, &edges, seeds, compact)?
                <= options.byte_limit
        {
            selected_node_keys.insert(node.key.clone());
            nodes = candidate_nodes;
        } else {
            truncated = true;
        }
    }
    Ok(ExpansionSelection {
        nodes,
        edges,
        candidate_paths: 0,
        selected_paths: 0,
        selected_path_edges: Vec::new(),
        truncated,
    })
}

#[derive(Clone)]
struct PathReach {
    root: String,
    score: f64,
    depth: usize,
    parent: Option<(String, EdgeIdentity)>,
}

struct RankedExpansionPath {
    key: String,
    roots: Vec<String>,
    edges: Vec<EdgeIdentity>,
    priority: u8,
    score: f64,
    depth: usize,
}

fn select_path_projection(
    seeds: &[String],
    ranked_nodes: &[structural::GraphNode],
    ranked_edges: &[structural::GraphEdge],
    options: &ExpansionOptions,
    compact: bool,
) -> Result<ExpansionSelection> {
    let nodes_by_key = ranked_nodes
        .iter()
        .map(|node| (node.key.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let edges_by_key = ranked_edges
        .iter()
        .map(|edge| (edge_identity(edge), edge.clone()))
        .collect::<HashMap<_, _>>();
    let mut adjacency: HashMap<String, Vec<(String, EdgeIdentity)>> = HashMap::new();
    for edge in ranked_edges {
        let identity = edge_identity(edge);
        adjacency
            .entry(edge.source.clone())
            .or_default()
            .push((edge.target.clone(), identity.clone()));
        adjacency
            .entry(edge.target.clone())
            .or_default()
            .push((edge.source.clone(), identity));
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort_by(|(left_node, left_edge), (right_node, right_edge)| {
            let left = edges_by_key.get(left_edge).expect("path edge");
            let right = edges_by_key.get(right_edge).expect("path edge");
            right
                .relevance
                .total_cmp(&left.relevance)
                .then_with(|| left_node.cmp(right_node))
                .then_with(|| left_edge.cmp(right_edge))
        });
    }

    // Multi-source maximum-bottleneck traversal. Every reached node keeps one
    // deterministic predecessor, producing a bounded forest instead of the
    // complete induced neighborhood.
    let seed_keys = seeds.iter().cloned().collect::<HashSet<_>>();
    let mut reach = seeds
        .iter()
        .filter(|seed| nodes_by_key.contains_key(*seed))
        .map(|seed| {
            (
                seed.clone(),
                PathReach {
                    root: seed.clone(),
                    score: 1.0,
                    depth: 0,
                    parent: None,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut settled = HashSet::new();
    loop {
        let next = reach
            .iter()
            .filter(|(key, _)| !settled.contains(*key))
            .max_by(|(left_key, left), (right_key, right)| {
                compare_path_reach(left_key, left, right_key, right)
            })
            .map(|(key, state)| (key.clone(), state.clone()));
        let Some((key, state)) = next else { break };
        settled.insert(key.clone());
        if state.depth >= options.depth {
            continue;
        }
        for (other, identity) in adjacency.get(&key).into_iter().flatten() {
            if settled.contains(other) || seed_keys.contains(other) {
                continue;
            }
            let edge = edges_by_key.get(identity).expect("path edge");
            let candidate = PathReach {
                root: state.root.clone(),
                score: state.score.min(edge.relevance),
                depth: state.depth + 1,
                parent: Some((key.clone(), identity.clone())),
            };
            let replace = reach
                .get(other)
                .is_none_or(|current| path_reach_is_stronger(other, &candidate, current));
            if replace {
                reach.insert(other.clone(), candidate);
            }
        }
    }

    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for state in reach.values() {
        if let Some((parent, _)) = &state.parent {
            *child_counts.entry(parent.clone()).or_default() += 1;
        }
    }
    let mut paths = reach
        .iter()
        .filter_map(|(key, state)| {
            let node = nodes_by_key.get(key)?;
            if state.depth == 0 {
                return None;
            }
            let root = nodes_by_key.get(&state.root)?;
            let boundary = node.kind != "symbol";
            let cross_file = node.file != root.file;
            let direct = state.depth == 1;
            let leaf = child_counts.get(key).copied().unwrap_or(0) == 0;
            (boundary || cross_file || direct || leaf).then_some(RankedExpansionPath {
                key: key.clone(),
                roots: vec![state.root.clone()],
                edges: predecessor_path(key, &reach),
                priority: if boundary || cross_file {
                    0
                } else if direct {
                    1
                } else {
                    2
                },
                score: state.score,
                depth: state.depth,
            })
        })
        .collect::<Vec<_>>();
    // A direct relation between two returned seeds is itself a useful path.
    // Multi-source traversal keeps both seeds as roots, so admit these edges
    // explicitly instead of silently dropping root-to-root relationships.
    paths.extend(
        ranked_edges
            .iter()
            .filter(|edge| seed_keys.contains(&edge.source) && seed_keys.contains(&edge.target))
            .map(|edge| RankedExpansionPath {
                key: format!("{}>{}:{}", edge.source, edge.target, edge.kind),
                roots: vec![edge.source.clone(), edge.target.clone()],
                edges: vec![edge_identity(edge)],
                priority: 0,
                score: edge.relevance,
                depth: 1,
            }),
    );
    paths.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| right.depth.cmp(&left.depth))
            .then_with(|| left.key.cmp(&right.key))
    });
    // Preserve the best continuation from every returned search seed before
    // spending the remaining path budget on a stronger seed's fan-out. This
    // keeps a multi-symbol search from silently dropping one localized entry
    // point merely because another has many high-scoring adjacent boundaries.
    let mut covered_roots = HashSet::new();
    let (mut coverage, remaining): (Vec<_>, Vec<_>) = paths.into_iter().partition(|path| {
        let contributes = path.roots.iter().any(|root| !covered_roots.contains(root));
        if contributes {
            covered_roots.extend(path.roots.iter().cloned());
        }
        contributes
    });
    coverage.extend(remaining);
    let paths = coverage;

    let candidate_paths = paths.len();
    let (mut nodes, mut selected_node_keys, mut truncated) =
        select_expansion_seeds(seeds, &nodes_by_key, options, compact)?;
    let mut edges = Vec::new();
    let mut selected_edge_keys = HashSet::new();
    let mut selected_paths = 0;
    let mut selected_path_edges = Vec::new();
    for path in paths {
        if selected_paths >= options.path_limit {
            truncated = true;
            break;
        }
        let mut candidate_nodes = nodes.clone();
        let mut candidate_node_keys = selected_node_keys.clone();
        let mut candidate_edges = edges.clone();
        let mut candidate_edge_keys = selected_edge_keys.clone();
        let mut complete = true;
        let path_edges = path.edges;
        for identity in &path_edges {
            let Some(edge) = edges_by_key.get(identity) else {
                complete = false;
                break;
            };
            for key in [&edge.source, &edge.target] {
                if candidate_node_keys.insert(key.clone()) {
                    let Some(node) = nodes_by_key.get(key) else {
                        complete = false;
                        break;
                    };
                    candidate_nodes.push(node.clone());
                }
            }
            if !complete {
                break;
            }
            if candidate_edge_keys.insert(identity.clone()) {
                candidate_edges.push(edge.clone());
            }
        }
        if !complete
            || candidate_nodes.len() > options.node_limit
            || candidate_edges.len() > options.edge_limit
            || expansion_parts_bytes(&candidate_nodes, &candidate_edges, seeds, compact)?
                > options.byte_limit
        {
            truncated = true;
            continue;
        }
        nodes = candidate_nodes;
        selected_node_keys = candidate_node_keys;
        edges = candidate_edges;
        selected_edge_keys = candidate_edge_keys;
        selected_paths += 1;
        selected_path_edges.push(path_edges);
    }

    Ok(ExpansionSelection {
        nodes,
        edges,
        candidate_paths,
        selected_paths,
        selected_path_edges,
        truncated: truncated || selected_paths < candidate_paths,
    })
}

fn select_expansion_seeds(
    seeds: &[String],
    nodes_by_key: &HashMap<String, structural::GraphNode>,
    options: &ExpansionOptions,
    compact: bool,
) -> Result<(Vec<structural::GraphNode>, HashSet<String>, bool)> {
    let mut nodes = Vec::new();
    let mut selected = HashSet::new();
    let mut truncated = false;
    for seed in seeds {
        let Some(node) = nodes_by_key.get(seed) else {
            continue;
        };
        let mut candidate = nodes.clone();
        candidate.push(node.clone());
        if candidate.len() <= options.node_limit
            && expansion_parts_bytes(&candidate, &[], seeds, compact)? <= options.byte_limit
        {
            selected.insert(seed.clone());
            nodes = candidate;
        } else {
            truncated = true;
        }
    }
    Ok((nodes, selected, truncated))
}

fn edge_identity(edge: &structural::GraphEdge) -> EdgeIdentity {
    (
        edge.source.clone(),
        edge.target.clone(),
        edge.kind.clone(),
        edge.file.clone(),
        edge.line,
    )
}

fn compare_path_reach(
    left_key: &str,
    left: &PathReach,
    right_key: &str,
    right: &PathReach,
) -> Ordering {
    left.score
        .total_cmp(&right.score)
        .then_with(|| right.depth.cmp(&left.depth))
        .then_with(|| right.root.cmp(&left.root))
        .then_with(|| right_key.cmp(left_key))
}

fn path_reach_is_stronger(key: &str, candidate: &PathReach, current: &PathReach) -> bool {
    compare_path_reach(key, candidate, key, current).is_gt()
}

fn predecessor_path(target: &str, reach: &HashMap<String, PathReach>) -> Vec<EdgeIdentity> {
    let mut path = Vec::new();
    let mut cursor = target;
    while let Some((parent, edge)) = reach.get(cursor).and_then(|state| state.parent.as_ref()) {
        path.push(edge.clone());
        cursor = parent;
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests;
