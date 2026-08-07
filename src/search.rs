use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::{embed, store, structural};

type EdgeIdentity = (String, String, String, Option<String>, Option<i64>);

#[derive(Debug, Clone)]
pub struct ExpansionOptions {
    pub depth: usize,
    pub seed_limit: usize,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub byte_limit: usize,
    pub min_confidence: String,
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub expand: bool,
    pub expansion: ExpansionOptions,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { limit: 8, expand: false, expansion: ExpansionOptions::default() }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SearchResult {
    pub snapshot: String,
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion: Option<SearchExpansion>,
}

#[derive(Debug, serde::Serialize)]
pub struct SearchExpansion {
    pub seeds: Vec<String>,
    pub nodes: Vec<structural::GraphNode>,
    pub edges: Vec<structural::GraphEdge>,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub byte_limit: usize,
    pub payload_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub file: String,
    pub kind: String,
    pub name: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub score: f64,
    pub snippet: String,
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

fn bm25_ranking(conn: &Connection, q: &str, limit: usize) -> Result<Vec<(i64, f64)>> {
    let fq = fts_query(q);
    if fq.is_empty() {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT rowid, bm25(chunks_fts, 2.0, 4.0, 3.0, 1.0) AS r
         FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY r LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![fq, limit as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn vector_ranking(
    conn: &Connection,
    provider: &embed::Provider,
    q: &str,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let t = std::time::Instant::now();
    let qv = provider.embed_query(q)?;
    if std::env::var_os("JSCOUT_TIMING").is_some() {
        eprintln!("timing:   embed-query {:?}", t.elapsed());
    }
    // Brute-force cosine over all embedded chunks; dedup hash -> best chunk.
    let mut stmt = conn.prepare(
        "SELECT c.id, e.vec FROM chunks c JOIN embeddings e ON e.chunk_hash = c.hash AND e.model = ?1",
    )?;
    let rows = stmt.query_map([&provider.model], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut scored: Vec<(i64, f64)> = rows
        .filter_map(|r| r.ok())
        .map(|(id, blob)| (id, embed::cosine(&qv, &embed::blob_to_vec(&blob)) as f64))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    Ok(scored)
}

/// Optional cross-encoder rerank stage (dms-style service:
/// POST {query, candidates:[{id,text}]} -> {scores:[{id,score}]}).
/// Configure with JSCOUT_RERANK_URL (e.g. http://127.0.0.1:8792/rerank).
pub struct Reranker {
    url: String,
    model: Option<String>,
}

impl Reranker {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("JSCOUT_RERANK_URL").ok()?;
        Some(Self { url, model: std::env::var("JSCOUT_RERANK_MODEL").ok() })
    }

    fn rerank(&self, query: &str, candidates: &[(i64, String)]) -> Result<Vec<(i64, f64)>> {
        let mut body = serde_json::json!({
            "query": query,
            "candidates": candidates
                .iter()
                .map(|(id, text)| serde_json::json!({ "id": id.to_string(), "text": text }))
                .collect::<Vec<_>>(),
        });
        if let Some(m) = &self.model {
            body["model"] = serde_json::json!(m);
        }
        let mut resp = ureq::post(&self.url)
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
    store::with_read_snapshot(conn, "jscout_search", || {
        let snapshot = structural::current_snapshot(conn)?;
        let hits = ranked_hits(conn, provider, q, options.limit)?;
        let expansion = options
            .expand
            .then(|| expand_hits(conn, &snapshot, &hits, &options.expansion))
            .transpose()?;
        Ok(SearchResult { snapshot, hits, expansion })
    })
}

fn ranked_hits(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    q: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    let timing = std::env::var_os("JSCOUT_TIMING").is_some();
    let pool = limit.max(10) * 5;
    let t0 = std::time::Instant::now();
    let mut rankings = vec![bm25_ranking(conn, q, pool)?];
    if timing {
        eprintln!("timing: bm25 {:?}", t0.elapsed());
    }
    if let Some(p) = provider {
        let t = std::time::Instant::now();
        match vector_ranking(conn, p, q, pool) {
            Ok(r) => rankings.push(r),
            Err(e) => eprintln!("vector search unavailable: {e}"),
        }
        if timing {
            eprintln!("timing: embed-query+vector-scan {:?}", t.elapsed());
        }
    }
    let mut fused = rrf(&rankings, 60.0);

    // Cross-encoder rerank of the candidate pool, when a service is configured.
    // Pool size and per-candidate truncation trade quality for latency:
    // JSCOUT_RERANK_TOP (default 50), JSCOUT_RERANK_CHARS (default 4000).
    if let Some(reranker) = Reranker::from_env() {
        let pool_n: usize = std::env::var("JSCOUT_RERANK_TOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let max_chars: usize = std::env::var("JSCOUT_RERANK_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000);
        let top: Vec<(i64, String)> = fused
            .iter()
            .take(pool_n)
            .filter_map(|(id, _)| {
                conn.query_row("SELECT content FROM chunks WHERE id = ?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .ok()
                .map(|mut text| {
                    if text.len() > max_chars {
                        let mut cut = max_chars;
                        while !text.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        text.truncate(cut);
                    }
                    (*id, text)
                })
            })
            .collect();
        if !top.is_empty() {
            let t = std::time::Instant::now();
            match reranker.rerank(q, &top) {
                Ok(reranked) if !reranked.is_empty() => fused = reranked,
                Ok(_) => {}
                Err(e) => eprintln!("rerank unavailable, using RRF order: {e}"),
            }
            if timing {
                eprintln!("timing: rerank({}) {:?}", top.len(), t.elapsed());
            }
        }
    }

    let mut hits = Vec::new();
    for (chunk_id, score) in fused.into_iter().take(limit) {
        if let Some(hit) = load_hit(conn, chunk_id, score)? {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn load_hit(conn: &Connection, chunk_id: i64, score: f64) -> Result<Option<Hit>> {
    let row = conn
        .query_row(
            "SELECT f.path, c.kind, c.name, c.start_line, c.end_line, c.content, c.symbols, c.file_id
             FROM chunks c JOIN files f ON c.file_id = f.id WHERE c.id = ?1",
            [chunk_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )
        .ok();
    let Some((file, kind, name, start_line, end_line, content, symbols, _file_id)) = row else {
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
        kind,
        name,
        start_line,
        end_line,
        score,
        snippet,
        anchors,
        file_anchor,
        uses,
        used_by,
    }))
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
    values.into_iter().filter(|value| seen.insert(value.clone())).collect()
}

fn expand_hits(
    conn: &Connection,
    snapshot: &str,
    hits: &[Hit],
    options: &ExpansionOptions,
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
    // Prefer declaration anchors across the ranked set. Import/module chunks
    // often outrank the implementation they mention; letting those file
    // fallbacks consume every seed would turn expansion into an import listing.
    for file_fallbacks in [false, true] {
        for hit in hits {
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

    let mut nodes: HashMap<String, structural::GraphNode> = HashMap::new();
    let mut edges: HashMap<EdgeIdentity, structural::GraphEdge> = HashMap::new();
    let mut payload_bytes = 0_usize;
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
            },
        )?;
        truncated |= neighborhood.truncated;
        for node in neighborhood.nodes {
            if nodes.contains_key(&node.key) {
                continue;
            }
            let bytes = serde_json::to_vec(&node)?.len() + usize::from(!nodes.is_empty());
            if nodes.len() >= options.node_limit || payload_bytes + bytes > options.byte_limit {
                truncated = true;
                continue;
            }
            payload_bytes += bytes;
            nodes.insert(node.key.clone(), node);
        }
        for edge in neighborhood.edges {
            if !nodes.contains_key(&edge.source) || !nodes.contains_key(&edge.target) {
                truncated = true;
                continue;
            }
            let key = (
                edge.source.clone(),
                edge.target.clone(),
                edge.kind.clone(),
                edge.file.clone(),
                edge.line,
            );
            if edges.contains_key(&key) {
                continue;
            }
            let bytes = serde_json::to_vec(&edge)?.len() + usize::from(!edges.is_empty());
            if edges.len() >= options.edge_limit || payload_bytes + bytes > options.byte_limit {
                truncated = true;
                continue;
            }
            payload_bytes += bytes;
            edges.insert(key, edge);
        }
    }

    let mut nodes: Vec<_> = nodes.into_values().collect();
    nodes.sort_by(|a, b| a.key.cmp(&b.key));
    let mut edges: Vec<_> = edges.into_values().collect();
    edges.sort_by(|a, b| {
        (&a.source, &a.kind, &a.target, &a.file, a.line)
            .cmp(&(&b.source, &b.kind, &b.target, &b.file, b.line))
    });
    Ok(SearchExpansion {
        seeds,
        nodes,
        edges,
        node_limit: options.node_limit,
        edge_limit: options.edge_limit,
        byte_limit: options.byte_limit,
        payload_bytes,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::{ExpansionOptions, SearchOptions, search};
    use crate::{indexer, store};

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
            &SearchOptions { limit: 8, ..Default::default() },
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
                expansion: ExpansionOptions {
                    depth: 1,
                    seed_limit: 3,
                    node_limit: 2,
                    edge_limit: 1,
                    byte_limit: 1_500,
                    min_confidence: "likely".into(),
                },
            },
        )?;
        let expansion = result.expansion.expect("expansion context pack");
        assert!(expansion.nodes.len() <= 2);
        assert!(expansion.edges.len() <= 1);
        assert!(expansion.payload_bytes <= 1_500);
        assert!(expansion.truncated);

        let byte_starved = search(
            &conn,
            None,
            "greet",
            &SearchOptions {
                limit: 8,
                expand: true,
                expansion: ExpansionOptions { byte_limit: 1, ..Default::default() },
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
}
