use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use crate::embed;

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
    if std::env::var_os("JSRAG_TIMING").is_some() {
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
/// Configure with JSRAG_RERANK_URL (e.g. http://127.0.0.1:8792/rerank).
pub struct Reranker {
    url: String,
    model: Option<String>,
}

impl Reranker {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("JSRAG_RERANK_URL").ok()?;
        Some(Self { url, model: std::env::var("JSRAG_RERANK_MODEL").ok() })
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
    limit: usize,
) -> Result<Vec<Hit>> {
    let timing = std::env::var_os("JSRAG_TIMING").is_some();
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
    // JSRAG_RERANK_TOP (default 50), JSRAG_RERANK_CHARS (default 4000).
    if let Some(reranker) = Reranker::from_env() {
        let pool_n: usize = std::env::var("JSRAG_RERANK_TOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let max_chars: usize = std::env::var("JSRAG_RERANK_CHARS")
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
        uses,
        used_by,
    }))
}
