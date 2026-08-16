use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::{embed, file_role, origin, semantic, store, structural};

type EdgeIdentity = (String, String, String, Option<String>, Option<i64>);

pub const DEFAULT_RESPONSE_BYTE_LIMIT: usize = 24_000;
pub const DEFAULT_RESULT_LIMIT: usize = 10;
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
    /// Apply the separately configured cross-encoder to the fused candidate
    /// pool. This is independent of whether vector retrieval is enabled.
    pub rerank: bool,
    /// Budget and render the agent-facing compact transport rather than the
    /// diagnostic representation.
    pub compact: bool,
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
            rerank: true,
            compact: false,
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
    pub snippet: String,
    pub snippet_truncated: bool,
    /// Snapshot-scoped structural handles projected from this retrieval chunk.
    pub anchors: Vec<String>,
    pub file_anchor: String,
    /// Graph context: symbols this chunk calls / renders (resolved names).
    pub uses: Vec<String>,
    /// Symbols declared here that other files use, with usage counts.
    pub used_by: Vec<String>,
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
    store::with_read_snapshot(conn, "jscout_search", || {
        let snapshot = structural::current_snapshot(conn)?;
        let (hits, retrieval) = ranked_hits(
            conn,
            provider,
            q,
            options.limit,
            &options.file_roles,
            &options.file_origins,
            options.rerank,
        )?;
        let (semantic_artifacts, semantic_retrieval, semantic_candidates) =
            if options.include_memory {
                let (artifacts, retrieval, candidates) =
                    semantic::search_with_provider(conn, provider, q, options.memory_limit)?;
                (artifacts, Some(retrieval), candidates)
            } else {
                (Vec::new(), None, 0)
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

    let mut hits = Vec::new();
    let allowed_roles: HashSet<&str> = file_roles.iter().map(String::as_str).collect();
    for (chunk_id, score) in fused {
        if let Some(hit) = load_hit(conn, chunk_id, score)? {
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

fn load_hit(conn: &Connection, chunk_id: i64, score: f64) -> Result<Option<Hit>> {
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
        snippet,
        snippet_truncated: false,
        anchors,
        file_anchor,
        uses,
        used_by,
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
    use std::fs;

    use anyhow::Result;

    use super::{
        DEFAULT_RESPONSE_BYTE_LIMIT, ExpansionOptions, Hit, ResponseBudget, RetrievalStatus,
        SearchExpansion, SearchOptions, SearchResult, apply_repository_policy_penalty,
        apply_response_budget, candidate_pool_limits, merge_reranked_prefix,
        prefilter_ranking_by_role, record_vector_ranking, reranker_document, search,
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
                rerank: true,
                compact: true,
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
                rerank: true,
                compact: false,
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
                snippet: "x".repeat(8_000),
                snippet_truncated: false,
                anchors: vec!["sym:src/large.ts#::largeHit@1".into()],
                file_anchor: "file:src/large.ts".into(),
                uses: vec!["helper (call)".into()],
                used_by: vec!["caller: 1 site".into()],
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
