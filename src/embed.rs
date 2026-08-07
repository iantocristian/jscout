use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde_json::json;

/// A pluggable embedding backend. Selected from environment:
/// - JSRAG_EMBED_PROVIDER=voyage|openai|none (optional override)
/// - VOYAGE_API_KEY (voyage-code-3)
/// - OPENAI_API_KEY, or JSRAG_EMBED_URL(+JSRAG_EMBED_KEY) for any
///   OpenAI-compatible endpoint (Ollama, LM Studio, vLLM...)
/// - JSRAG_EMBED_MODEL overrides the model name
pub struct Provider {
    pub name: String,
    pub model: String,
    url: String,
    key: Option<String>,
    /// voyage uses {input, model}, openai-compat uses {input, model} too but
    /// different auth header casing; kept uniform, distinguished by name.
    is_voyage: bool,
    /// Asymmetric-retrieval prefix prepended to queries only (never documents).
    /// nomic-embed-code / CodeRankEmbed style; override with JSRAG_QUERY_PREFIX.
    query_prefix: String,
}

fn default_query_prefix(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("nomic-embed-code") || m.contains("coderankembed") {
        "Represent this query for searching relevant code: "
    } else if m.contains("qwen3-embedding") {
        "Instruct: Given a code search query, retrieve relevant code snippets that answer the query\nQuery: "
    } else {
        ""
    }
}

impl Provider {
    pub fn from_env() -> Option<Provider> {
        let choice = std::env::var("JSRAG_EMBED_PROVIDER").unwrap_or_default();
        let voyage_key = std::env::var("VOYAGE_API_KEY").ok();
        let openai_key = std::env::var("OPENAI_API_KEY").ok();
        let custom_url = std::env::var("JSRAG_EMBED_URL").ok();
        let model_override = std::env::var("JSRAG_EMBED_MODEL").ok();

        let pick_voyage = choice == "voyage" || (choice.is_empty() && voyage_key.is_some());
        let pick_openai =
            choice == "openai" || (choice.is_empty() && (openai_key.is_some() || custom_url.is_some()));

        if choice == "none" {
            return None;
        }
        let query_prefix = |model: &str| {
            std::env::var("JSRAG_QUERY_PREFIX")
                .unwrap_or_else(|_| default_query_prefix(model).to_string())
        };
        if pick_voyage && voyage_key.is_some() {
            let model = model_override.unwrap_or_else(|| "voyage-code-3".into());
            return Some(Provider {
                query_prefix: query_prefix(&model),
                name: "voyage".into(),
                model,
                url: "https://api.voyageai.com/v1/embeddings".into(),
                key: voyage_key,
                is_voyage: true,
            });
        }
        if pick_openai {
            let model = model_override.unwrap_or_else(|| "text-embedding-3-small".into());
            return Some(Provider {
                query_prefix: query_prefix(&model),
                name: "openai-compat".into(),
                model,
                url: custom_url
                    .unwrap_or_else(|| "https://api.openai.com/v1/embeddings".into()),
                key: std::env::var("JSRAG_EMBED_KEY").ok().or(openai_key),
                is_voyage: false,
            });
        }
        None
    }

    /// POST with retries: local servers (LM Studio, Ollama) JIT-load models
    /// and can transiently 400/503 while switching; give them a moment.
    fn post_with_retry(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let mut last_err: Option<anyhow::Error> = None;
        // Model JIT-loads can take tens of seconds; back off generously.
        for (attempt, delay_ms) in [(0u32, 0u64), (1, 2000), (2, 8000), (3, 20000)] {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            let mut req = ureq::post(&self.url).header("content-type", "application/json");
            if let Some(k) = &self.key {
                req = req.header("authorization", &format!("Bearer {k}"));
            }
            match req.send(body.to_string()) {
                Ok(mut resp) => {
                    let text = resp.body_mut().read_to_string()?;
                    return Ok(serde_json::from_str(&text)?);
                }
                Err(e) => {
                    if attempt < 3 {
                        eprintln!("embed request failed (attempt {}): {e}; retrying", attempt + 1);
                    }
                    last_err = Some(e.into());
                }
            }
        }
        Err(last_err.unwrap()).with_context(|| format!("embedding request to {} failed", self.url))
    }

    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut body = json!({ "model": self.model, "input": texts });
        if self.is_voyage {
            body["input_type"] = json!("document");
        }
        let parsed = self.post_with_retry(&body)?;
        let data = parsed["data"]
            .as_array()
            .with_context(|| format!("unexpected embedding response: {parsed}"))?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let v: Vec<f32> = item["embedding"]
                .as_array()
                .context("missing embedding array")?
                .iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect();
            out.push(v);
        }
        if out.len() != texts.len() {
            bail!("embedding count mismatch: {} in, {} out", texts.len(), out.len());
        }
        Ok(out)
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let text = format!("{}{}", self.query_prefix, text);
        let mut body = json!({ "model": self.model, "input": [text] });
        if self.is_voyage {
            body["input_type"] = json!("query");
        }
        let parsed = self.post_with_retry(&body)?;
        let v = parsed["data"][0]["embedding"]
            .as_array()
            .with_context(|| format!("unexpected embedding response: {parsed}"))?
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect();
        Ok(v)
    }
}

pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// The text actually embedded: a small contextual header + the chunk body.
pub fn embed_text(path: &str, scope: &str, name: Option<&str>, imports: &str, content: &str) -> String {
    let mut header = format!("// file: {path}\n");
    if !scope.is_empty() {
        header.push_str(&format!("// scope: {scope}\n"));
    }
    if let Some(n) = name {
        header.push_str(&format!("// symbol: {n}\n"));
    }
    if !imports.is_empty() {
        header.push_str(&format!("// imports: {imports}\n"));
    }
    let mut text = header + content;
    // Keep well under typical 32k-token model context; ~24k chars ≈ 6k tokens.
    if text.len() > 24_000 {
        let mut cut = 24_000;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// Embed all chunks that don't yet have an embedding under this model.
/// Embeddings are keyed by chunk content hash, so unchanged chunks are
/// never re-embedded across re-indexes.
pub fn embed_missing(conn: &Connection, provider: &Provider, batch_size: usize) -> Result<(usize, usize)> {
    let rows: Vec<(String, String, String, Option<String>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.hash, f.path, c.scope_chain, c.name, c.content
             FROM chunks c JOIN files f ON c.file_id = f.id
             WHERE NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.chunk_hash = c.hash AND e.model = ?1)",
        )?;
        let r = stmt.query_map([&provider.model], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        r.collect::<std::result::Result<_, _>>()?
    };
    let total = rows.len();
    let mut done = 0usize;
    for batch in rows.chunks(batch_size) {
        let texts: Vec<String> = batch
            .iter()
            .map(|(_, path, scope, name, content)| {
                embed_text(path, scope, name.as_deref(), "", content)
            })
            .collect();
        let vecs = provider.embed(&texts)?;
        conn.execute_batch("BEGIN")?;
        for ((hash, ..), vec) in batch.iter().zip(&vecs) {
            conn.execute(
                "INSERT OR REPLACE INTO embeddings(chunk_hash, model, dim, vec) VALUES(?1,?2,?3,?4)",
                params![hash, provider.model, vec.len() as i64, vec_to_blob(vec)],
            )?;
        }
        conn.execute_batch("COMMIT")?;
        done += batch.len();
        eprintln!("embedded {done}/{total}");
    }
    Ok((done, total))
}
