use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::{embed, file_role, origin, semantic, store, structural};

type EdgeIdentity = (String, String, String, Option<String>, Option<i64>);

pub const DEFAULT_RESPONSE_BYTE_LIMIT: usize = 24_000;
pub const DEFAULT_RESULT_LIMIT: usize = 10;
pub const DEFAULT_MEMORY_GRAPH_DEPTH: usize = 2;
pub const DEFAULT_MEMORY_GRAPH_NODE_LIMIT: usize = 2_000;
pub const MAX_MEMORY_GRAPH_DEPTH: usize = 8;
pub const MAX_MEMORY_GRAPH_NODE_LIMIT: usize = 20_000;
const DEFAULT_TOTAL_RENDERED_SUPPORT_LIMIT: usize = 8;

#[derive(Debug, Clone)]
pub struct ExpansionOptions {
    pub depth: usize,
    pub seed_limit: usize,
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
            depth: 1,
            seed_limit: 3,
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
    pub limit: usize,
    pub expand: bool,
    /// Maximum bytes in the pretty-printed JSON search envelope. This covers
    /// hits, expansion, metadata, and serialization overhead.
    pub response_byte_limit: usize,
    /// Optional role allowlist for primary hits. Empty preserves normal recall.
    pub file_roles: Vec<String>,
    /// Backing-file origin allowlist. Defaults to first-party origins.
    pub file_origins: Vec<String>,
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
    /// Budget and render the agent-facing compact transport rather than the
    /// diagnostic representation.
    pub compact: bool,
    /// Emit neighborhood follow-ups when that structural tool is available.
    /// Baseline MCP search disables these while retaining exact definition and
    /// who_uses hand-offs.
    pub include_neighborhood_followups: bool,
    pub expansion: ExpansionOptions,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RESULT_LIMIT,
            expand: false,
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            include_memory: true,
            memory_limit: 4,
            memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
            memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            rerank: true,
            compact: false,
            include_neighborhood_followups: true,
            expansion: ExpansionOptions::default(),
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SearchResult {
    pub snapshot: String,
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
    #[serde(skip)]
    pub semantic_candidates: usize,
    /// Memory previews selected before the whole-response byte budget.
    #[serde(skip)]
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
        }
    }

    fn vector_active() -> Self {
        Self {
            lexical: "active",
            vector: "active",
            reranker: "disabled",
            vector_action: None,
        }
    }

    fn vector_degraded(action: &'static str) -> Self {
        Self {
            lexical: "active",
            vector: "degraded",
            reranker: "disabled",
            vector_action: Some(action),
        }
    }

    fn reranker_active(&mut self) {
        self.reranker = "active";
    }

    fn reranker_degraded(&mut self) {
        self.reranker = "degraded";
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ResponseBudget {
    pub byte_limit: usize,
    pub rendered_bytes: usize,
    pub unbudgeted_bytes: usize,
    pub truncated: bool,
    pub omitted_hits: usize,
    pub omitted_semantic_artifacts: usize,
    pub omitted_semantic_supports: usize,
    pub omitted_nodes: usize,
    pub omitted_edges: usize,
    pub omitted_followups: usize,
    pub truncated_snippets: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct SearchExpansion {
    pub seeds: Vec<String>,
    pub nodes: Vec<structural::GraphNode>,
    pub edges: Vec<structural::GraphEdge>,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub byte_limit: usize,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
    pub payload_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, serde::Serialize)]
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
    pub snippet: String,
    pub snippet_truncated: bool,
    /// Snapshot-scoped structural handles projected from this retrieval chunk.
    pub anchors: Vec<String>,
    pub file_anchor: String,
    /// Graph context: symbols this chunk calls / renders (resolved names).
    pub uses: Vec<String>,
    /// Symbols declared here that other files use, with usage counts.
    pub used_by: Vec<String>,
    /// Compact transport may shed copy-safe follow-up arguments from lower
    /// ranked hits before dropping the hit itself.
    #[serde(skip)]
    pub include_followups: bool,
    #[serde(skip)]
    pub include_neighborhood_followup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    ExactDefinition,
    ExactOccurrence,
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

/// Build an FTS5 query: each identifier-ish token quoted, OR-joined, so any
/// match ranks (BM25 handles weighting) and no user input is FTS syntax.
fn fts_query(q: &str) -> String {
    let tokens: Vec<String> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    tokens.join(" OR ")
}

fn exact_intent_tokens(query: &str) -> Vec<String> {
    let raw_tokens = query
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '$')
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
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
) -> Result<ExactIntentCandidates> {
    let identifiers = exact_intent_tokens(query);
    let mut definitions = Vec::with_capacity(identifiers.len());
    let mut occurrences = Vec::with_capacity(identifiers.len());
    for identifier in &identifiers {
        let definition_ids = exact_definition_chunks(
            conn,
            identifier,
            per_identifier_limit,
            file_roles,
            file_origins,
        )?;
        let definition_set = definition_ids.iter().copied().collect::<HashSet<_>>();
        let occurrence_ids = exact_occurrence_chunks(
            conn,
            identifier,
            per_identifier_limit,
            file_roles,
            file_origins,
        )?
        .into_iter()
        .filter(|chunk_id| !definition_set.contains(chunk_id))
        .collect();
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
) -> Result<Vec<i64>> {
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
    let row_limit = limit.max(1) as i64;
    let mut rows = Vec::<(i64, i64, i64, i64, String, i64)>::new();

    let mut named_chunks = conn.prepare_cached(
        "SELECT chunk.id, 0 AS name_priority, 1 AS export_priority,
                chunk.end-chunk.start AS span, file.path, chunk.start
         FROM chunks chunk
         JOIN files file ON file.id=chunk.file_id
         WHERE chunk.name=?1 COLLATE BINARY
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
         ORDER BY file.path, chunk.start, chunk.id
         LIMIT ?7",
    )?;
    let named = named_chunks.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
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
    let mut containing_chunks = conn.prepare_cached(
        "SELECT chunk.id,
                CASE WHEN chunk.name=?1 COLLATE BINARY THEN 0 ELSE 1 END AS name_priority,
                CASE WHEN symbol.exported=1 THEN 0 ELSE 1 END AS export_priority,
                chunk.end-chunk.start AS span, file.path, chunk.start
         FROM symbols symbol
         JOIN files file ON file.id=symbol.file_id
         JOIN chunks chunk ON chunk.file_id=symbol.file_id
           AND chunk.start<=symbol.decl_start AND symbol.decl_start<chunk.end
         WHERE symbol.name=?1 COLLATE BINARY
           AND ((?2 AND file.origin='repository')
             OR (?3 AND file.origin='workspace')
             OR (?4 AND file.origin='dependency'))
           AND (?5 OR file.role IN (SELECT value FROM json_each(?6)))
         ORDER BY name_priority, export_priority, span, file.path, chunk.start, chunk.id
         LIMIT ?7",
    )?;
    let containing = containing_chunks.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
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
) -> Result<Vec<i64>> {
    let flags = origin_flags(file_origins);
    let roles_json = serde_json::to_string(file_roles)?;
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
         ) candidate
         GROUP BY candidate.chunk_id
         ORDER BY MIN(candidate.path), MIN(candidate.position), candidate.chunk_id
         LIMIT ?7",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            identifier,
            flags.0,
            flags.1,
            flags.2,
            file_roles.is_empty(),
            roles_json,
            limit.max(1) as i64,
        ],
        |row| row.get::<_, i64>(0),
    )?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn tiered_candidates(
    exact: ExactIntentCandidates,
    hybrid: &[(i64, f64)],
) -> Vec<RankedHitCandidate> {
    let hybrid_scores = hybrid.iter().copied().collect::<HashMap<_, _>>();
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
        &exact.occurrences,
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
) -> Result<Vec<(i64, f64)>> {
    let fq = fts_query(q);
    if fq.is_empty() {
        return Ok(vec![]);
    }
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
         ORDER BY r LIMIT ?7",
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
            roles_json,
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
) -> Result<Vec<(i64, f64)>> {
    let t = std::time::Instant::now();
    let scored = embed::vector_search(conn, provider, q, limit, file_origins)?;
    if std::env::var_os("JSCOUT_TIMING").is_some() {
        eprintln!("timing:   embed-query+sqlite-vec {:?}", t.elapsed());
    }
    Ok(scored)
}

fn origin_flags(origins: &[String]) -> (bool, bool, bool) {
    (
        origins.iter().any(|origin| origin == "repository"),
        origins.iter().any(|origin| origin == "workspace"),
        origins.iter().any(|origin| origin == "dependency"),
    )
}

/// Optional cross-encoder rerank stage (dms-style service:
/// POST {model, query, candidates:[{id,text}]} -> {scores:[{id,score}]}).
/// Local embeddings use the bundled service automatically; an explicit
/// JSCOUT_RERANK_URL overrides that endpoint.
pub struct Reranker {
    url: String,
    model: String,
}

impl Reranker {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("JSCOUT_RERANK_URL").ok().or_else(|| {
            std::env::var("JSCOUT_EMBED_PROVIDER")
                .ok()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("local"))
                .then(|| format!("{}/rerank", crate::inference::base_url()))
        })?;
        Some(Self {
            url,
            model: std::env::var("JSCOUT_RERANK_MODEL")
                .unwrap_or_else(|_| "BAAI/bge-reranker-v2-m3".to_string()),
        })
    }

    fn rerank(&self, query: &str, candidates: &[(i64, String)]) -> Result<Vec<(i64, f64)>> {
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
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(out)
    }
}

/// Reciprocal Rank Fusion over the available rankings.
fn rrf(rankings: &[Vec<(i64, f64)>], k: f64) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for ranking in rankings {
        for (rank, (id, _)) in ranking.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64)> = scores.into_iter().collect();
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
    store::with_read_snapshot(conn, "jscout_search", || {
        let snapshot = structural::current_snapshot(conn)?;
        let (mut hits, retrieval) = ranked_hits(
            conn,
            provider,
            q,
            options.limit,
            &options.file_roles,
            &options.file_origins,
            options.rerank,
        )?;
        if !options.include_neighborhood_followups {
            for hit in &mut hits {
                hit.include_neighborhood_followup = false;
            }
        }
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
            .then(|| expand_hits(conn, &snapshot, &hits, &options.expansion, options.compact))
            .transpose()?;
        let mut result = SearchResult {
            snapshot,
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
        for anchor in hit.anchors.iter().chain(std::iter::once(&hit.file_anchor)) {
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
    limit: usize,
    file_roles: &[String],
    file_origins: &[String],
    rerank: bool,
) -> Result<(Vec<Hit>, RetrievalStatus)> {
    let timing = std::env::var_os("JSCOUT_TIMING").is_some();
    let (pool, vector_pool) = candidate_pool_limits(limit, !file_roles.is_empty());
    let exact = exact_intent_candidates(conn, q, limit, file_roles, file_origins)?;
    let t0 = std::time::Instant::now();
    let mut rankings = vec![bm25_ranking(conn, q, pool, file_roles, file_origins)?];
    let mut retrieval = RetrievalStatus::vector_disabled();
    if timing {
        eprintln!("timing: bm25 {:?}", t0.elapsed());
    }
    if let Some(p) = provider {
        let t = std::time::Instant::now();
        retrieval = record_vector_ranking(
            &mut rankings,
            vector_ranking(conn, p, q, vector_pool, file_origins),
        );
        if timing {
            eprintln!("timing: embed-query+sqlite-vec {:?}", t.elapsed());
        }
    }
    for ranking in &mut rankings {
        prefilter_ranking_by_role(conn, ranking, file_roles)?;
        ranking.truncate(pool);
    }
    let mut fused = rrf(&rankings, 60.0);

    // Cross-encoder rerank of the candidate pool, when a service is configured.
    // Pool size and per-candidate truncation trade quality for latency:
    // JSCOUT_RERANK_TOP (default 50), JSCOUT_RERANK_CHARS (default 4000).
    if rerank && let Some(reranker) = Reranker::from_env() {
        let pool_n: usize = std::env::var("JSCOUT_RERANK_TOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50)
            .min(100);
        let max_chars: usize = std::env::var("JSCOUT_RERANK_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000);
        let top: Vec<(i64, String)> = fused
            .iter()
            .take(pool_n)
            .map(|(id, _)| reranker_document(conn, *id, max_chars))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        if !top.is_empty() {
            let t = std::time::Instant::now();
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
            if timing {
                eprintln!("timing: rerank({}) {:?}", top.len(), t.elapsed());
            }
        }
    }
    if file_roles.is_empty() {
        apply_repository_policy_penalty(conn, &mut fused)?;
    }
    let ranked = tiered_candidates(exact, &fused);

    let mut hits = Vec::new();
    let allowed_roles: HashSet<&str> = file_roles.iter().map(String::as_str).collect();
    for candidate in ranked {
        if let Some(hit) = load_hit(
            conn,
            candidate.chunk_id,
            candidate.score,
            candidate.match_reason,
            candidate.matched_identifiers,
        )? {
            if !allowed_roles.is_empty() && !allowed_roles.contains(hit.file_role.as_str()) {
                continue;
            }
            hits.push(hit);
            if hits.len() >= limit {
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

fn merge_reranked_prefix(fused: &[(i64, f64)], mut reranked: Vec<(i64, f64)>) -> Vec<(i64, f64)> {
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
    result: Result<Vec<(i64, f64)>>,
) -> RetrievalStatus {
    match result {
        Ok(ranking) => {
            rankings.push(ranking);
            RetrievalStatus::vector_active()
        }
        Err(error) => {
            eprintln!("vector search unavailable: {error}");
            RetrievalStatus::vector_degraded(embed::code_vector_failure_action(&error))
        }
    }
}

fn load_hit(
    conn: &Connection,
    chunk_id: i64,
    score: f64,
    match_reason: MatchReason,
    matched_identifiers: Vec<String>,
) -> Result<Option<Hit>> {
    let row = conn
        .query_row(
            "SELECT f.path, f.role, f.origin, c.kind, c.name, c.start_line, c.end_line,
                    c.content, c.symbols, c.file_id, policy.effective_role
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
                    r.get::<_, String>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<String>>(10)?,
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
        symbols,
        _file_id,
        repository_role,
    )) = row
    else {
        return Ok(None);
    };
    let anchors = project_chunk_anchors(conn, chunk_id, &file, name.as_deref())?;
    let file_anchor = format!("file:{file}");

    // Outgoing: what this chunk calls/renders (top few, certain only).
    let uses: Vec<String> = {
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT target_name || ' (' || kind || ')' FROM refs
             WHERE chunk_id = ?1 AND kind IN ('call','render','extend') AND confidence='certain'
             LIMIT 6",
        )?;
        let rows = stmt.query_map([chunk_id], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // Incoming: usages of symbols declared in this chunk, from other files.
    let mut used_by = Vec::new();
    for sym in symbols.split_whitespace().take(3) {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM refs WHERE target_name = ?1 AND chunk_id != ?2",
            rusqlite::params![sym, chunk_id],
            |r| r.get(0),
        )?;
        if n > 0 {
            used_by.push(format!("{sym}: {n} sites"));
        }
    }

    let snippet: String = content.lines().take(4).collect::<Vec<_>>().join("\n");
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
        snippet,
        snippet_truncated: false,
        anchors,
        file_anchor,
        uses,
        used_by,
        include_followups: true,
        include_neighborhood_followup: true,
    }))
}

fn apply_response_budget(result: &mut SearchResult, compact: bool) -> Result<()> {
    let byte_limit = result.response_budget.byte_limit;
    if byte_limit == 0 {
        anyhow::bail!("response byte limit must be greater than zero");
    }

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

    while settle_rendered_bytes(result, compact)? > byte_limit {
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
                result.response_budget.omitted_edges += 1;
                result.response_budget.omitted_nodes += prune_expansion_nodes(expansion);
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
                result.response_budget.omitted_nodes += 1;
                expansion.payload_bytes = expansion_payload_bytes(expansion, compact)?;
                continue;
            }
        }

        if compact
            && let Some(hit) = result
                .hits
                .iter_mut()
                .rev()
                .find(|hit| hit.include_followups && hit.anchors.len() <= 1)
        {
            hit.include_followups = false;
            result.response_budget.omitted_followups += 1;
            continue;
        }

        // Ranked hits are the primary product. Shed only lower-ranked hits;
        // the top hit remains even when the requested byte limit is too small.
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
        if let Some(hit) = result
            .hits
            .iter_mut()
            .rev()
            .find(|hit| hit.anchors.len() > 1)
        {
            hit.anchors.pop();
            continue;
        }

        let rendered = result.response_budget.rendered_bytes;
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

        let minimum = settle_rendered_bytes(result, compact)?;
        anyhow::bail!(
            "response byte limit {byte_limit} is below the minimum search envelope ({minimum} bytes)"
        );
    }

    settle_rendered_bytes(result, compact)?;
    Ok(())
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

fn capture_unbudgeted_bytes(result: &mut SearchResult, compact: bool) -> Result<usize> {
    for _ in 0..8 {
        let rendered = settle_rendered_bytes(result, compact)?;
        if result.response_budget.unbudgeted_bytes == rendered {
            return Ok(rendered);
        }
        result.response_budget.unbudgeted_bytes = rendered;
    }
    settle_rendered_bytes(result, compact)
}

fn rendered_bytes(result: &SearchResult, compact: bool) -> Result<usize> {
    if compact {
        crate::compact::search_rendered_bytes(result)
    } else {
        Ok(serde_json::to_string_pretty(result)?.len())
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

fn project_chunk_anchors(
    conn: &Connection,
    chunk_id: i64,
    file: &str,
    chunk_name: Option<&str>,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT g.node_key, s.name, s.scope_chain, c.scope_chain
         FROM chunks c
         JOIN symbols s ON s.file_id=c.file_id
           AND s.decl_start < c.end AND s.decl_end > c.start
         JOIN graph_nodes g ON g.native_table='symbols' AND g.native_id=s.id
         WHERE c.id=?1
         ORDER BY s.decl_start, s.decl_end, g.node_key",
    )?;
    let rows = stmt.query_map([chunk_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let candidates: Vec<(String, String, String, String)> =
        rows.collect::<std::result::Result<_, _>>()?;
    if let Some(name) = chunk_name {
        let exact: Vec<String> = candidates
            .iter()
            .filter(|(_, symbol_name, symbol_scope, chunk_scope)| {
                symbol_name == name && symbol_scope == chunk_scope
            })
            .map(|(key, _, _, _)| key.clone())
            .collect();
        if !exact.is_empty() {
            return Ok(dedup(exact));
        }
    }
    let overlaps = dedup(candidates.into_iter().map(|(key, _, _, _)| key).collect());
    if overlaps.is_empty() {
        Ok(vec![format!("file:{file}")])
    } else {
        Ok(overlaps)
    }
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
        || options.node_limit == 0
        || options.edge_limit == 0
        || options.byte_limit == 0
    {
        anyhow::bail!("expansion seed, node, edge, and byte limits must be greater than zero");
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

    let nodes_by_key = ranked_nodes
        .iter()
        .map(|node| (node.key.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let mut nodes = Vec::new();
    let mut selected_node_keys = HashSet::new();
    let mut edges = Vec::new();

    // Seeds are the required definitions for interpreting an expansion. Add
    // them first, then admit relations atomically with both endpoints. The old
    // nodes-first loop could exhaust the byte budget before a single edge.
    for seed in &seeds {
        let Some(node) = nodes_by_key.get(seed) else {
            continue;
        };
        let mut candidate_nodes = nodes.clone();
        candidate_nodes.push(node.clone());
        if candidate_nodes.len() <= options.node_limit
            && expansion_parts_bytes(&candidate_nodes, &edges, &seeds, compact)?
                <= options.byte_limit
        {
            selected_node_keys.insert(seed.clone());
            nodes = candidate_nodes;
        } else {
            truncated = true;
        }
    }

    for edge in ranked_edges {
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
        candidate_edges.push(edge);
        if expansion_parts_bytes(&candidate_nodes, &candidate_edges, &seeds, compact)?
            > options.byte_limit
        {
            truncated = true;
            continue;
        }
        nodes = candidate_nodes;
        selected_node_keys = candidate_keys;
        edges = candidate_edges;
    }

    // Use remaining space for high-relevance standalone definitions without
    // sacrificing any already-admitted relation.
    for node in ranked_nodes {
        if selected_node_keys.contains(&node.key) {
            continue;
        }
        let mut candidate_nodes = nodes.clone();
        candidate_nodes.push(node.clone());
        if candidate_nodes.len() <= options.node_limit
            && expansion_parts_bytes(&candidate_nodes, &edges, &seeds, compact)?
                <= options.byte_limit
        {
            selected_node_keys.insert(node.key.clone());
            nodes = candidate_nodes;
        } else {
            truncated = true;
        }
    }
    let payload_bytes = expansion_parts_bytes(&nodes, &edges, &seeds, compact)?;
    Ok(SearchExpansion {
        seeds,
        nodes,
        edges,
        node_limit: options.node_limit,
        edge_limit: options.edge_limit,
        byte_limit: options.byte_limit,
        file_roles: options.file_roles.clone(),
        file_origins: options.file_origins.clone(),
        payload_bytes,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs};

    use anyhow::Result;

    use super::{
        DEFAULT_MEMORY_GRAPH_DEPTH, DEFAULT_MEMORY_GRAPH_NODE_LIMIT, DEFAULT_RESPONSE_BYTE_LIMIT,
        ExpansionOptions, Hit, MatchReason, ResponseBudget, RetrievalStatus, SearchExpansion,
        SearchOptions, SearchResult, apply_repository_policy_penalty, apply_response_budget,
        candidate_pool_limits, exact_intent_tokens, merge_reranked_prefix,
        prefilter_ranking_by_role, record_vector_ranking, reranker_document, search,
        select_attached_memory, tiered_candidates,
    };
    use crate::{
        file_role, indexer, origin,
        semantic::{ArtifactRetrievalScore, SemanticArtifact, SemanticSupport},
        store,
        structural::{GraphEdge, GraphNode},
    };

    fn insert_repository_policy(
        conn: &rusqlite::Connection,
        file_id: i64,
        role: &str,
        suffix: &str,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind,status,gateway_protocol,provider,model,billing_path,
               prompt_version,source_snapshot,input_fingerprint,request_hash,
               config_json,started_at,completed_at
             ) VALUES('repository','completed',1,'test','test','custom','test',
                      'snapshot',?1,?1,'{}','now','now')",
            [format!("search-policy-{suffix}")],
        )?;
        let run_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO repository_classifications(
               run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
               explanation,citations_json,evidence_fingerprint,
               classification_fingerprint,source_snapshot,created_at
             ) VALUES(?1,?2,'area','{}',0,?3,'likely','test','[\"E001\"]',
                      ?2,?2,'snapshot','now')",
            rusqlite::params![run_id, format!("area:{suffix}"), role],
        )?;
        let classification_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO repository_file_policy(
               file_id,classification_id,subject_key,scope_role,effective_role,
               source_hash,depth
             ) VALUES(?1,?2,?3,?4,?4,'hash',0)",
            rusqlite::params![file_id, classification_id, format!("area:{suffix}"), role],
        )?;
        Ok(())
    }

    #[test]
    fn vector_retrieval_status_distinguishes_active_disabled_and_degraded() {
        let disabled = RetrievalStatus::vector_disabled();
        assert_eq!(disabled.vector, "disabled");
        assert_eq!(disabled.reranker, "disabled");
        assert!(disabled.vector_action.is_none());

        let mut rankings = Vec::new();
        let active = record_vector_ranking(&mut rankings, Ok(vec![(7, 0.9)]));
        assert_eq!(active.vector, "active");
        assert_eq!(rankings, vec![vec![(7, 0.9)]]);

        let degraded = record_vector_ranking(
            &mut rankings,
            Err(anyhow::anyhow!("profile is not materialized")),
        );
        assert_eq!(degraded.vector, "degraded");
        assert!(degraded.vector_action.is_some());

        let mut reranked = degraded;
        reranked.reranker_active();
        assert_eq!(reranked.reranker, "active");
        reranked.reranker_degraded();
        assert_eq!(reranked.reranker, "degraded");
    }

    #[test]
    fn vector_candidates_are_overfetched_before_role_filtering() {
        assert_eq!(candidate_pool_limits(8, false), (50, 50));
        assert_eq!(candidate_pool_limits(8, true), (50, 200));
    }

    #[test]
    fn reranking_a_prefix_preserves_every_unreranked_tail_candidate() {
        let fused = (1..=60).map(|id| (id, id as f64)).collect::<Vec<_>>();
        let reranked = (1..=50)
            .rev()
            .map(|id| (id, -(id as f64)))
            .collect::<Vec<_>>();
        let merged = merge_reranked_prefix(&fused, reranked);
        assert_eq!(merged.len(), 60);
        assert_eq!(merged[0].0, 50);
        assert_eq!(merged[49].0, 1);
        assert_eq!(
            merged[50..]
                .iter()
                .map(|(chunk_id, _)| *chunk_id)
                .collect::<Vec<_>>(),
            (51..=60).collect::<Vec<_>>()
        );
    }

    #[test]
    fn exact_identifier_intent_does_not_promote_plain_prose() {
        assert_eq!(exact_intent_tokens("insert"), ["insert"]);
        assert_eq!(exact_intent_tokens("insert()"), ["insert"]);
        assert_eq!(exact_intent_tokens("`insert`"), ["insert"]);
        assert_eq!(
            exact_intent_tokens(
                "find createRouteTypesManifest and NextTypesPlugin in root_layout files"
            ),
            ["createRouteTypesManifest", "NextTypesPlugin", "root_layout"]
        );
        assert!(exact_intent_tokens("development cache behavior").is_empty());
        assert_eq!(
            exact_intent_tokens("CreateRouteTypesManifest"),
            ["CreateRouteTypesManifest"]
        );
    }

    #[test]
    fn exact_tiers_survive_hostile_hybrid_order_and_cover_identifiers() {
        let exact = super::ExactIntentCandidates {
            identifiers: vec!["firstThing".into(), "SecondThing".into()],
            definitions: vec![vec![1, 4], vec![2, 5]],
            occurrences: vec![vec![6], vec![7]],
        };
        let ranked = tiered_candidates(exact, &[(9, 100.0), (7, 90.0), (2, -10.0)]);
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.chunk_id)
                .collect::<Vec<_>>(),
            [1, 2, 4, 5, 6, 7, 9]
        );
        assert!(
            ranked[..4]
                .iter()
                .all(|candidate| candidate.match_reason == MatchReason::ExactDefinition)
        );
        assert!(
            ranked[4..6]
                .iter()
                .all(|candidate| candidate.match_reason == MatchReason::ExactOccurrence)
        );
        assert_eq!(ranked[6].match_reason, MatchReason::Hybrid);
    }

    #[test]
    fn exact_identifier_search_precedes_examples_and_preserves_ambiguity() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("manifest.ts"),
            "export function createRouteTypesManifest() { return true; }\n\
             export function getRootParamsFromLayouts() { return {}; }\n\
             export const collectedRootParams = new Map();\n",
        )?;
        fs::write(
            repo.path().join("plugin-a.ts"),
            "export class NextTypesPlugin { apply() {} }\n",
        )?;
        fs::write(
            repo.path().join("plugin-b.ts"),
            "export class NextTypesPlugin { apply() { return 'second'; } }\n",
        )?;
        fs::write(
            repo.path().join("caller.ts"),
            "import { createRouteTypesManifest } from './manifest';\n\
             export function callManifest() { return createRouteTypesManifest(); }\n",
        )?;
        fs::write(
            repo.path().join("sitecore-example.ts"),
            "export const sitecoreExample = 'createRouteTypesManifest NextTypesPlugin collectedRootParams';\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let multi = search(
            &conn,
            None,
            "createRouteTypesManifest getRootParamsFromLayouts collectedRootParams NextTypesPlugin",
            &SearchOptions {
                limit: 4,
                include_memory: false,
                ..Default::default()
            },
        )?;
        assert_eq!(multi.hits.len(), 4);
        assert!(
            multi
                .hits
                .iter()
                .all(|hit| hit.match_reason == MatchReason::ExactDefinition)
        );
        let covered = multi
            .hits
            .iter()
            .flat_map(|hit| hit.matched_identifiers.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        assert_eq!(
            covered,
            HashSet::from([
                "createRouteTypesManifest",
                "getRootParamsFromLayouts",
                "collectedRootParams",
                "NextTypesPlugin",
            ])
        );
        assert!(
            multi
                .hits
                .iter()
                .all(|hit| hit.file != "sitecore-example.ts")
        );
        let compact = crate::compact::search_value(&multi);
        assert_eq!(compact["default_match"], "hybrid");
        assert_eq!(compact["hits"][0]["match"], "exact_definition");
        assert!(compact["hits"][0]["matched_identifiers"].is_array());

        let ambiguous = search(
            &conn,
            None,
            "NextTypesPlugin",
            &SearchOptions {
                limit: 2,
                include_memory: false,
                ..Default::default()
            },
        )?;
        assert_eq!(ambiguous.hits.len(), 2);
        assert!(ambiguous.hits.iter().all(|hit| {
            hit.match_reason == MatchReason::ExactDefinition
                && hit.name.as_deref() == Some("NextTypesPlugin")
        }));

        let occurrence = search(
            &conn,
            None,
            "createRouteTypesManifest",
            &SearchOptions {
                limit: 3,
                include_memory: false,
                ..Default::default()
            },
        )?;
        assert_eq!(
            occurrence.hits[0].match_reason,
            MatchReason::ExactDefinition
        );
        assert!(occurrence.hits.iter().any(|hit| {
            hit.file == "caller.ts" && hit.match_reason == MatchReason::ExactOccurrence
        }));
        Ok(())
    }

    #[test]
    fn search_projects_chunks_to_snapshot_scoped_anchors() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function greet(name) { return name; }\n",
        )?;
        fs::write(
            repo.path().join("b.ts"),
            "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let result = search(
            &conn,
            None,
            "greet",
            &SearchOptions {
                limit: 8,
                ..Default::default()
            },
        )?;
        assert_eq!(result.snapshot.len(), 64);
        let definition = result
            .hits
            .iter()
            .find(|hit| hit.file == "a.ts" && hit.name.as_deref() == Some("greet"))
            .expect("greet definition hit");
        assert_eq!(definition.file_anchor, "file:a.ts");
        assert_eq!(definition.anchors, vec!["sym:a.ts#::greet@1"]);
        assert!(result.expansion.is_none());
        Ok(())
    }

    #[test]
    fn expansion_uses_one_global_node_edge_and_byte_budget() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function greet(name) { return name; }\n",
        )?;
        fs::write(
            repo.path().join("b.ts"),
            "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let result = search(
            &conn,
            None,
            "greet run",
            &SearchOptions {
                limit: 8,
                expand: true,
                file_roles: Vec::new(),
                file_origins: origin::defaults(),
                include_memory: true,
                memory_limit: 4,
                memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
                memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
                rerank: true,
                compact: true,
                include_neighborhood_followups: true,
                response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
                expansion: ExpansionOptions {
                    depth: 1,
                    seed_limit: 3,
                    node_limit: 2,
                    edge_limit: 1,
                    byte_limit: 1_500,
                    min_confidence: "likely".into(),
                    file_roles: file_role::DEFAULT_EXPANSION
                        .iter()
                        .map(|role| (*role).to_string())
                        .collect(),
                    file_origins: origin::defaults(),
                },
            },
        )?;
        let expansion = result.expansion.expect("expansion context pack");
        assert!(expansion.nodes.len() <= 2);
        assert_eq!(expansion.edges.len(), 1);
        assert!(expansion.edges.iter().all(|edge| {
            expansion.nodes.iter().any(|node| node.key == edge.source)
                && expansion.nodes.iter().any(|node| node.key == edge.target)
        }));
        assert!(expansion.payload_bytes <= 1_500);
        assert!(expansion.truncated);

        let byte_starved = search(
            &conn,
            None,
            "greet",
            &SearchOptions {
                limit: 8,
                expand: true,
                file_roles: Vec::new(),
                file_origins: origin::defaults(),
                include_memory: true,
                memory_limit: 4,
                memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
                memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
                rerank: true,
                compact: false,
                include_neighborhood_followups: true,
                response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
                expansion: ExpansionOptions {
                    byte_limit: 1,
                    ..Default::default()
                },
            },
        )?
        .expansion
        .expect("expansion context pack");
        assert!(byte_starved.nodes.is_empty());
        assert!(byte_starved.edges.is_empty());
        assert!(byte_starved.payload_bytes <= 1);
        assert!(byte_starved.truncated);
        Ok(())
    }

    #[test]
    fn attached_memory_requires_direct_graph_or_artifact_relation_evidence() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("entry.ts"),
            "import { nearby } from './nearby';\nexport function entry() { return nearby(); }\n",
        )?;
        fs::write(
            repo.path().join("nearby.ts"),
            "export function nearby() { return 1; }\n",
        )?;
        fs::write(
            repo.path().join("unrelated.ts"),
            "export function unrelated() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let anchor = |name: &str| -> Result<String> {
            Ok(conn.query_row(
                "SELECT node_key FROM graph_nodes
                 WHERE node_kind='symbol' AND display_name=?1
                 ORDER BY node_key LIMIT 1",
                [name],
                |row| row.get(0),
            )?)
        };
        let entry = anchor("entry")?;
        let nearby = anchor("nearby")?;
        let unrelated = anchor("unrelated")?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        for id in [1_i64, 2, 3, 4] {
            conn.execute(
                "INSERT INTO semantic_artifacts(
                   id,artifact_type,canonical_name,body_json,model,prompt_version,
                   confidence,source_snapshot,created_at,input_fingerprint,artifact_fingerprint
                 ) VALUES(?1,'card',?2,'{}','test','test/v1','likely',?3,'now',?2,?2)",
                rusqlite::params![id, format!("artifact-{id}"), snapshot],
            )?;
        }
        conn.execute(
            "INSERT INTO semantic_relations(
               src_artifact_id,dst_artifact_id,relation,claim_path,confidence,dst_fingerprint
             ) VALUES(4,1,'related_to','/related','likely','artifact-1')",
            [],
        )?;

        let support = |anchor: &str, file: &str| SemanticSupport {
            claim_path: "/claim".into(),
            anchor: anchor.into(),
            relationship: "defining-evidence".into(),
            role: None,
            evidence_file: file.into(),
            evidence_start_line: 1,
            evidence_end_line: 1,
            source_hash: "h".repeat(64),
            context_hash: "c".repeat(64),
            confidence: "likely".into(),
            freshness: "fresh".into(),
        };
        let artifact = |id: i64, score: f64, supports: Vec<SemanticSupport>| -> SemanticArtifact {
            SemanticArtifact {
                id,
                supersedes: None,
                artifact_type: if id == 4 { "concept" } else { "card" }.into(),
                name: Some(format!("artifact-{id}")),
                trust: "untrusted-semantic-memory".into(),
                body: serde_json::json!({ "purpose": format!("artifact {id}") }),
                model: "test".into(),
                prompt_version: "test/v1".into(),
                confidence: "likely".into(),
                source_snapshot: snapshot.clone(),
                created_at: "now".into(),
                freshness: "fresh".into(),
                supports,
                retrieval_score: Some(ArtifactRetrievalScore {
                    rank_score: score,
                    lexical_score: Some(score),
                    vector_cosine: None,
                }),
            }
        };
        let hit = Hit {
            chunk_id: 1,
            file: "entry.ts".into(),
            file_role: "production".into(),
            repository_role: None,
            file_origin: "repository".into(),
            kind: "function".into(),
            name: Some("entry".into()),
            start_line: 2,
            end_line: 2,
            score: 1.0,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
            snippet: "entry() { return nearby(); }".into(),
            snippet_truncated: false,
            anchors: vec![entry.clone()],
            file_anchor: "file:entry.ts".into(),
            uses: Vec::new(),
            used_by: Vec::new(),
            include_followups: true,
            include_neighborhood_followup: true,
        };
        let candidates = vec![
            artifact(3, 1.0, vec![support(&unrelated, "unrelated.ts")]),
            artifact(2, 0.8, vec![support(&nearby, "nearby.ts")]),
            artifact(4, 0.9, Vec::new()),
            artifact(1, 0.1, vec![support(&entry, "entry.ts")]),
        ];
        let (selected, status) =
            select_attached_memory(&conn, candidates, &[hit], 4, 2, 2_000, &origin::defaults())?;
        assert_eq!(
            selected
                .iter()
                .map(|artifact| artifact.id)
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(status.status, "connected");
        assert_eq!(status.connected_candidates, 3);

        let disconnected = artifact(3, 1.0, vec![support(&unrelated, "unrelated.ts")]);
        let (_, status) = select_attached_memory(
            &conn,
            vec![disconnected],
            &[],
            4,
            2,
            2_000,
            &origin::defaults(),
        )?;
        assert_eq!(status.status, "no_connected_memory");
        assert_eq!(status.connected_candidates, 0);
        Ok(())
    }

    #[test]
    fn response_budget_caps_the_complete_rendered_search_envelope() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = format!("export const needle = '{}';\n", "x".repeat(12_000));
        fs::write(repo.path().join("large.ts"), source)?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let result = search(
            &conn,
            None,
            "needle",
            &SearchOptions {
                limit: 8,
                response_byte_limit: 2_000,
                ..Default::default()
            },
        )?;
        let rendered = serde_json::to_string_pretty(&result)?;
        assert!(rendered.len() <= 2_000);
        assert_eq!(rendered.len(), result.response_budget.rendered_bytes);
        assert!(result.response_budget.unbudgeted_bytes > rendered.len());
        assert!(result.response_budget.truncated);
        assert!(result.response_budget.truncated_snippets > 0);
        serde_json::from_str::<serde_json::Value>(&rendered)?;
        Ok(())
    }

    #[test]
    fn response_budget_removes_low_ranked_subgraphs_not_all_edges() -> Result<()> {
        let node = |key: &str, relevance: f64| GraphNode {
            key: key.into(),
            kind: "symbol".into(),
            display_name: key.into(),
            file: None,
            file_role: None,
            file_origin: None,
            line: None,
            meta: serde_json::json!({}),
            relevance,
        };
        let edge = |target: &str, relevance: f64, padding: usize| GraphEdge {
            source: "root".into(),
            target: target.into(),
            kind: "call".into(),
            confidence: "certain".into(),
            provenance: "test".into(),
            file: None,
            line: None,
            detail: serde_json::json!({ "padding": "x".repeat(padding) }),
            relevance,
        };
        let mut result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits: Vec::new(),
            semantic_artifacts: Vec::new(),
            semantic_retrieval: None,
            semantic_attachment: None,
            semantic_candidates: 0,
            semantic_selected: 0,
            expansion: Some(SearchExpansion {
                seeds: vec!["root".into()],
                nodes: vec![node("root", 1.0), node("high", 0.8), node("low", 0.1)],
                edges: vec![edge("high", 0.8, 0), edge("low", 0.1, 1_200)],
                node_limit: 3,
                edge_limit: 2,
                byte_limit: 10_000,
                file_roles: vec!["production".into(), "unknown".into()],
                file_origins: origin::defaults(),
                payload_bytes: 0,
                truncated: false,
            }),
            response_budget: ResponseBudget {
                byte_limit: 1_650,
                ..Default::default()
            },
        };

        apply_response_budget(&mut result, false)?;
        let expansion = result.expansion.expect("expansion");
        assert!(expansion.nodes.iter().any(|node| node.key == "high"));
        assert!(!expansion.nodes.iter().any(|node| node.key == "low"));
        assert_eq!(expansion.edges.len(), 1);
        assert_eq!(expansion.edges[0].target, "high");
        assert!(result.response_budget.rendered_bytes <= 1_650);
        Ok(())
    }

    #[test]
    fn response_budget_preserves_primary_code_before_memory() -> Result<()> {
        let mut result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits: vec![Hit {
                chunk_id: 1,
                file: "src/large.ts".into(),
                file_role: "production".into(),
                repository_role: None,
                file_origin: "repository".into(),
                kind: "function".into(),
                name: Some("largeHit".into()),
                start_line: 1,
                end_line: 200,
                score: 1.0,
                match_reason: MatchReason::Hybrid,
                matched_identifiers: Vec::new(),
                snippet: "x".repeat(8_000),
                snippet_truncated: false,
                anchors: vec!["sym:src/large.ts#::largeHit@1".into()],
                file_anchor: "file:src/large.ts".into(),
                uses: vec!["helper (call)".into()],
                used_by: vec!["caller: 1 site".into()],
                include_followups: true,
                include_neighborhood_followup: true,
            }],
            semantic_artifacts: vec![SemanticArtifact {
                id: 1,
                supersedes: None,
                artifact_type: "workflow".into(),
                name: Some("checkout lifecycle".into()),
                trust: "untrusted-semantic-memory".into(),
                body: serde_json::json!({
                    "participants": [{
                        "anchor": "sym:src/checkout.ts#::checkout@1",
                        "role": "workflow entry",
                        "scope": "defining"
                    }]
                }),
                model: "agent-reported".into(),
                prompt_version: "annotate/v2".into(),
                confidence: "likely".into(),
                source_snapshot: "s".repeat(64),
                created_at: "2026-08-09T00:00:00Z".into(),
                freshness: "fresh".into(),
                supports: Vec::new(),
                retrieval_score: Some(ArtifactRetrievalScore {
                    rank_score: 1.0,
                    lexical_score: Some(1.0),
                    vector_cosine: None,
                }),
            }],
            semantic_retrieval: None,
            semantic_attachment: None,
            semantic_candidates: 1,
            semantic_selected: 1,
            expansion: None,
            response_budget: ResponseBudget {
                byte_limit: 2_000,
                ..Default::default()
            },
        };

        apply_response_budget(&mut result, false)?;
        assert!(result.semantic_artifacts.is_empty());
        assert_eq!(result.response_budget.omitted_semantic_artifacts, 1);
        assert_eq!(result.hits.len(), 1);
        assert!(result.hits[0].snippet_truncated);
        assert!(result.response_budget.truncated);
        assert!(result.response_budget.rendered_bytes <= 2_000);
        Ok(())
    }

    #[test]
    fn compact_budget_sheds_followups_before_primary_hit_identity() -> Result<()> {
        let mut result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits: vec![Hit {
                chunk_id: 1,
                file: "src/target.ts".into(),
                file_role: "production".into(),
                repository_role: None,
                file_origin: "repository".into(),
                kind: "function".into(),
                name: Some("target".into()),
                start_line: 10,
                end_line: 12,
                score: 1.0,
                match_reason: MatchReason::Hybrid,
                matched_identifiers: Vec::new(),
                snippet: "export function target() { return 1; }".into(),
                snippet_truncated: false,
                anchors: vec!["sym:src/target.ts#::target@1".into()],
                file_anchor: "file:src/target.ts".into(),
                uses: Vec::new(),
                used_by: Vec::new(),
                include_followups: true,
                include_neighborhood_followup: true,
            }],
            semantic_artifacts: Vec::new(),
            semantic_retrieval: None,
            semantic_attachment: None,
            semantic_candidates: 0,
            semantic_selected: 0,
            expansion: None,
            response_budget: ResponseBudget {
                byte_limit: 500,
                ..Default::default()
            },
        };

        apply_response_budget(&mut result, true)?;
        assert_eq!(result.hits.len(), 1);
        assert!(!result.hits[0].include_followups);
        assert_eq!(result.response_budget.omitted_followups, 1);
        assert!(result.response_budget.rendered_bytes <= 500);
        Ok(())
    }

    #[test]
    fn search_caps_rendered_semantic_supports_even_under_a_large_byte_budget() -> Result<()> {
        let supports = (0..20)
            .map(|index| SemanticSupport {
                claim_path: format!("/claims/{index}"),
                anchor: format!("sym:src/workflow.ts#::step{index}@{}", index + 1),
                relationship: "supporting-stage-evidence".into(),
                role: Some(format!("workflow stage {index}")),
                evidence_file: "src/workflow.ts".into(),
                evidence_start_line: index + 1,
                evidence_end_line: index + 1,
                source_hash: "s".repeat(64),
                context_hash: "c".repeat(64),
                confidence: "likely".into(),
                freshness: "fresh".into(),
            })
            .collect();
        let artifact = SemanticArtifact {
            id: 1,
            supersedes: None,
            artifact_type: "workflow".into(),
            name: Some("large workflow".into()),
            trust: "untrusted-semantic-memory".into(),
            body: serde_json::json!({ "purpose": "exercise bounded rendering" }),
            model: "agent-reported".into(),
            prompt_version: "annotate/v2".into(),
            confidence: "likely".into(),
            source_snapshot: "s".repeat(64),
            created_at: "2026-08-13T00:00:00Z".into(),
            freshness: "fresh".into(),
            supports,
            retrieval_score: Some(ArtifactRetrievalScore {
                rank_score: 1.0,
                lexical_score: Some(1.0),
                vector_cosine: None,
            }),
        };
        let mut second_artifact = artifact.clone();
        second_artifact.id = 2;
        second_artifact.name = Some("second workflow".into());
        let mut result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits: Vec::new(),
            semantic_artifacts: vec![artifact, second_artifact],
            semantic_retrieval: None,
            semantic_attachment: None,
            semantic_candidates: 2,
            semantic_selected: 2,
            expansion: None,
            response_budget: ResponseBudget {
                byte_limit: 100_000,
                ..Default::default()
            },
        };

        apply_response_budget(&mut result, false)?;
        assert_eq!(
            result
                .semantic_artifacts
                .iter()
                .map(|artifact| artifact.supports.len())
                .sum::<usize>(),
            8
        );
        assert_eq!(result.semantic_artifacts[0].supports.len(), 4);
        assert_eq!(result.semantic_artifacts[1].supports.len(), 4);
        assert_eq!(result.response_budget.omitted_semantic_supports, 32);
        assert!(result.response_budget.truncated);
        assert!(result.response_budget.unbudgeted_bytes > result.response_budget.rendered_bytes);
        Ok(())
    }

    #[test]
    fn file_roles_tag_hits_and_filter_search_and_expansion_before_budgets() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("src"))?;
        fs::create_dir_all(repo.path().join("tests"))?;
        fs::write(
            repo.path().join("src/service.ts"),
            "export function performRoleFilteredWork() { return 1; }\n",
        )?;
        fs::write(
            repo.path().join("tests/service.test.ts"),
            "import { performRoleFilteredWork } from '../src/service';\nexport function exerciseRoleFilteredWork() { return performRoleFilteredWork(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let production_chunk = conn.query_row(
            "SELECT chunk.id FROM chunks chunk JOIN files file ON file.id=chunk.file_id
             WHERE file.path='src/service.ts' ORDER BY chunk.id LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let (_, reranker_text) =
            reranker_document(&conn, production_chunk, 4_000)?.expect("indexed reranker candidate");
        assert!(reranker_text.contains("path: src/service.ts"));
        assert!(reranker_text.contains("scope: src"));
        assert!(reranker_text.contains("symbol: performRoleFilteredWork"));
        assert!(reranker_text.contains("kind: function"));
        assert!(reranker_text.contains("role: production"));
        assert!(reranker_text.contains("origin: repository"));

        let test_chunk = conn.query_row(
            "SELECT chunk.id FROM chunks chunk JOIN files file ON file.id=chunk.file_id
             WHERE file.path='tests/service.test.ts' ORDER BY chunk.id LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let mut fused_candidates = vec![(test_chunk, 0.9), (production_chunk, 0.8)];
        prefilter_ranking_by_role(&conn, &mut fused_candidates, &["production".into()])?;
        assert_eq!(fused_candidates, vec![(production_chunk, 0.8)]);

        let test_only = search(
            &conn,
            None,
            "performRoleFilteredWork",
            &SearchOptions {
                file_roles: vec!["test".into()],
                ..Default::default()
            },
        )?;
        assert!(!test_only.hits.is_empty());
        assert!(test_only.hits.iter().all(|hit| hit.file_role == "test"));

        let production_expansion = search(
            &conn,
            None,
            "performRoleFilteredWork",
            &SearchOptions {
                expand: true,
                ..Default::default()
            },
        )?
        .expansion
        .expect("expansion context pack");
        assert_eq!(
            production_expansion.file_roles,
            vec!["production".to_string(), "unknown".to_string()]
        );
        assert!(production_expansion.nodes.iter().all(|node| {
            node.file_role
                .as_deref()
                .is_none_or(|role| matches!(role, "production" | "unknown"))
        }));

        let all_roles = search(
            &conn,
            None,
            "performRoleFilteredWork",
            &SearchOptions {
                expand: true,
                expansion: ExpansionOptions {
                    file_roles: Vec::new(),
                    ..Default::default()
                },
                ..Default::default()
            },
        )?
        .expansion
        .expect("expansion context pack");
        assert!(
            all_roles
                .nodes
                .iter()
                .any(|node| node.file_role.as_deref() == Some("test"))
        );
        Ok(())
    }

    #[test]
    fn fresh_repository_policy_ranks_and_describes_effective_roles() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("docs"))?;
        fs::create_dir_all(repo.path().join("src"))?;
        fs::write(
            repo.path().join("docs/runtime.ts"),
            "export function sharedReconNeedle() { return 'runtime'; }\n",
        )?;
        fs::write(
            repo.path().join("src/tool.ts"),
            "export function sharedReconNeedle() { return 'tooling'; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let (runtime_file, runtime_chunk): (i64, i64) = conn.query_row(
            "SELECT file.id, chunk.id FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             WHERE file.path='docs/runtime.ts' ORDER BY chunk.id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (tool_file, tool_chunk): (i64, i64) = conn.query_row(
            "SELECT file.id, chunk.id FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             WHERE file.path='src/tool.ts' ORDER BY chunk.id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        insert_repository_policy(&conn, runtime_file, "runtime", "runtime")?;
        insert_repository_policy(&conn, tool_file, "tooling", "tooling")?;

        let mut ranking = vec![(tool_chunk, 1.0), (runtime_chunk, 0.9)];
        apply_repository_policy_penalty(&conn, &mut ranking)?;
        assert_eq!(ranking[0].0, runtime_chunk);

        let (_, document) =
            reranker_document(&conn, runtime_chunk, 4_000)?.expect("runtime recon candidate");
        assert!(document.contains("role: runtime"));
        assert!(document.contains("deterministic_role: documentation"));

        let result = search(
            &conn,
            None,
            "sharedReconNeedle",
            &SearchOptions {
                limit: 2,
                ..Default::default()
            },
        )?;
        assert_eq!(result.hits[0].file, "docs/runtime.ts");
        assert_eq!(result.hits[0].repository_role.as_deref(), Some("runtime"));
        assert!(result.hits.iter().any(|hit| {
            hit.file == "src/tool.ts" && hit.repository_role.as_deref() == Some("tooling")
        }));
        Ok(())
    }
}
