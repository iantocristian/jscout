use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const LOCAL_EMBED_MODEL: &str = "BAAI/bge-m3";
const DEFAULT_LOCAL_DEADLINE_MS: u64 = 120_000;
/// The document representation is part of cache identity. Versioning it keeps
/// vectors produced from an older representation from being silently reused.
const DOCUMENT_TEXT_FORMAT: &str = "content-v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Local,
    Voyage,
    OpenAi,
}

/// Embedding provider selection is explicit. API keys never select a provider,
/// and OPENAI_API_KEY is never forwarded to a custom endpoint.
pub struct Provider {
    pub name: String,
    pub model: String,
    url: String,
    key: Option<String>,
    protocol: Protocol,
    query_prefix: String,
}

#[derive(Clone, Debug)]
struct ProfileSpec {
    provider: String,
    model: String,
    fingerprint: String,
    config_json: String,
    dimensions: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    pub id: i64,
    pub dimensions: usize,
}

struct EmbeddingResponse {
    vectors: Vec<Vec<f32>>,
    profile_fingerprint: Option<String>,
}

fn default_query_prefix(model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    if model.contains("nomic-embed-code") || model.contains("coderankembed") {
        "Represent this query for searching relevant code: "
    } else if model.contains("qwen3-embedding") {
        "Instruct: Given a code search query, retrieve relevant code snippets that answer the query\nQuery: "
    } else {
        ""
    }
}

fn validate_endpoint(url: &str) -> Result<()> {
    let (scheme, rest) = url
        .split_once("://")
        .context("embedding endpoint must be an absolute http(s) URL")?;
    if !matches!(scheme, "http" | "https") {
        bail!("embedding endpoint must use http or https");
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        bail!("embedding endpoint must have a host and cannot contain credentials");
    }
    Ok(())
}

impl Provider {
    pub fn from_env() -> Result<Option<Self>> {
        let choice = std::env::var("JSCOUT_EMBED_PROVIDER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if choice.is_empty() || choice == "none" {
            return Ok(None);
        }

        let model_override = std::env::var("JSCOUT_EMBED_MODEL").ok();
        let query_prefix = |model: &str| {
            std::env::var("JSCOUT_QUERY_PREFIX")
                .unwrap_or_else(|_| default_query_prefix(model).to_string())
        };
        match choice.as_str() {
            "local" => {
                let model = model_override.unwrap_or_else(|| LOCAL_EMBED_MODEL.to_string());
                let base = crate::inference::base_url();
                validate_endpoint(&base)?;
                Ok(Some(Self {
                    name: "local".to_string(),
                    model,
                    url: format!("{base}/embed"),
                    key: None,
                    protocol: Protocol::Local,
                    query_prefix: String::new(),
                }))
            }
            "voyage" => {
                let key = std::env::var("VOYAGE_API_KEY")
                    .context("JSCOUT_EMBED_PROVIDER=voyage requires VOYAGE_API_KEY")?;
                let model = model_override.unwrap_or_else(|| "voyage-code-3".to_string());
                Ok(Some(Self {
                    name: "voyage".to_string(),
                    query_prefix: query_prefix(&model),
                    model,
                    url: "https://api.voyageai.com/v1/embeddings".to_string(),
                    key: Some(key),
                    protocol: Protocol::Voyage,
                }))
            }
            "openai" => {
                let custom_url = std::env::var("JSCOUT_EMBED_URL").ok();
                if let Some(url) = &custom_url {
                    validate_endpoint(url)?;
                }
                // A custom server has a separate credential namespace. This
                // prevents an OpenAI secret from leaking to LM Studio, vLLM,
                // a gateway, or a mistyped host.
                let key = if custom_url.is_some() {
                    std::env::var("JSCOUT_EMBED_KEY").ok()
                } else {
                    Some(
                        std::env::var("OPENAI_API_KEY")
                            .context("OpenAI embeddings require OPENAI_API_KEY")?,
                    )
                };
                let model = model_override.unwrap_or_else(|| "text-embedding-3-small".to_string());
                Ok(Some(Self {
                    name: "openai-compatible".to_string(),
                    query_prefix: query_prefix(&model),
                    model,
                    url: custom_url
                        .unwrap_or_else(|| "https://api.openai.com/v1/embeddings".to_string()),
                    key,
                    protocol: Protocol::OpenAi,
                }))
            }
            _ => bail!(
                "unsupported JSCOUT_EMBED_PROVIDER={choice}; expected local, voyage, openai, or none"
            ),
        }
    }

    fn profile(&self) -> Result<ProfileSpec> {
        let (configuration, dimensions) = match self.protocol {
            Protocol::Local => {
                let base = self
                    .url
                    .strip_suffix("/embed")
                    .context("invalid local inference URL")?;
                let response = crate::inference::get_json(&format!("{base}/configuration"))?;
                if response["provider"].as_str() != Some("local") {
                    bail!("unexpected local inference configuration: {response}");
                }
                let embedding = &response["embedding"];
                if embedding["model"].as_str() != Some(&self.model) {
                    bail!(
                        "local inference model mismatch: jscout requested {}, service provides {}",
                        self.model,
                        embedding["model"].as_str().unwrap_or("<missing>")
                    );
                }
                let dimensions = embedding["dimensions"]
                    .as_u64()
                    .context("local inference configuration has no embedding dimensions")?
                    as usize;
                (
                    json!({
                        "protocol": "jscout-local-v1",
                        "document_text": DOCUMENT_TEXT_FORMAT,
                        "embedding": embedding,
                    }),
                    Some(dimensions),
                )
            }
            Protocol::Voyage => (
                json!({
                    "protocol": "voyage-v1",
                    "document_text": DOCUMENT_TEXT_FORMAT,
                    "url": self.url,
                    "query_prefix": self.query_prefix,
                }),
                None,
            ),
            Protocol::OpenAi => (
                json!({
                    "protocol": "openai-embeddings-v1",
                    "document_text": DOCUMENT_TEXT_FORMAT,
                    "url": self.url,
                    "query_prefix": self.query_prefix,
                }),
                None,
            ),
        };
        let config_json = serde_json::to_string(&configuration)?;
        let fingerprint = profile_fingerprint(&self.name, &self.model, &config_json);
        Ok(ProfileSpec {
            provider: self.name.clone(),
            model: self.model.clone(),
            fingerprint,
            config_json,
            dimensions,
        })
    }

    fn post_with_retry(&self, body: &Value) -> Result<Value> {
        let mut last_error: Option<anyhow::Error> = None;
        let local_attempts = [(0u32, 0u64)];
        let remote_attempts = [(0u32, 0u64), (1, 2_000), (2, 8_000), (3, 20_000)];
        let attempts = if self.protocol == Protocol::Local {
            local_attempts.as_slice()
        } else {
            remote_attempts.as_slice()
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_millis(
                DEFAULT_LOCAL_DEADLINE_MS + 5_000,
            )))
            .build()
            .new_agent();
        for (attempt_index, &(attempt, delay_ms)) in attempts.iter().enumerate() {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            let mut request = agent
                .post(&self.url)
                .header("content-type", "application/json");
            if let Some(key) = &self.key {
                request = request.header("authorization", &format!("Bearer {key}"));
            }
            match request.send(body.to_string()) {
                Ok(mut response) => {
                    let text = response.body_mut().read_to_string()?;
                    return serde_json::from_str(&text)
                        .with_context(|| format!("invalid embedding response from {}", self.url));
                }
                Err(error) => {
                    if attempt_index + 1 < attempts.len() {
                        eprintln!(
                            "embed request failed (attempt {}): {error}; retrying",
                            attempt + 1
                        );
                    }
                    last_error = Some(error.into());
                }
            }
        }
        Err(last_error.expect("at least one embedding attempt"))
            .with_context(|| format!("embedding request to {} failed", self.url))
    }

    fn embed_documents(&self, texts: &[String]) -> Result<EmbeddingResponse> {
        self.embed_texts(texts, false)
    }

    fn embed_query(&self, text: &str) -> Result<EmbeddingResponse> {
        let text = format!("{}{}", self.query_prefix, text);
        self.embed_texts(&[text], true)
    }

    fn embed_texts(&self, texts: &[String], query: bool) -> Result<EmbeddingResponse> {
        let body = match self.protocol {
            Protocol::Local => json!({
                "model": self.model,
                "texts": texts,
                "deadline_ms": DEFAULT_LOCAL_DEADLINE_MS,
            }),
            Protocol::Voyage => json!({
                "model": self.model,
                "input": texts,
                "input_type": if query { "query" } else { "document" },
            }),
            Protocol::OpenAi => json!({ "model": self.model, "input": texts }),
        };
        let response = self.post_with_retry(&body)?;
        let (vectors, response_fingerprint) = if self.protocol == Protocol::Local {
            if response["provider"].as_str() != Some("local")
                || response["model"].as_str() != Some(&self.model)
            {
                bail!("local embedding response changed provider or model: {response}");
            }
            let vectors = parse_vectors(&response["vectors"])?;
            let dimensions = response["dimensions"]
                .as_u64()
                .context("local embedding response has no dimensions")?
                as usize;
            if vectors
                .first()
                .is_none_or(|vector| vector.len() != dimensions)
            {
                bail!("local embedding response dimension metadata does not match vectors");
            }
            let configuration = json!({
                "protocol": "jscout-local-v1",
                "document_text": DOCUMENT_TEXT_FORMAT,
                "embedding": {
                    "model": response["model"],
                    "dimensions": response["dimensions"],
                    "revision": response["revision"],
                    "configuration": response["configuration"],
                },
            });
            let config_json = serde_json::to_string(&configuration)?;
            (
                vectors,
                Some(profile_fingerprint(&self.name, &self.model, &config_json)),
            )
        } else {
            let data = response["data"]
                .as_array()
                .with_context(|| format!("unexpected embedding response: {response}"))?;
            (
                data.iter()
                    .map(|item| parse_vector(&item["embedding"]))
                    .collect::<Result<Vec<_>>>()?,
                None,
            )
        };
        validate_vectors(texts.len(), &vectors)?;
        Ok(EmbeddingResponse {
            vectors,
            profile_fingerprint: response_fingerprint,
        })
    }
}

fn parse_vectors(value: &Value) -> Result<Vec<Vec<f32>>> {
    value
        .as_array()
        .context("missing vectors array")?
        .iter()
        .map(parse_vector)
        .collect()
}

fn parse_vector(value: &Value) -> Result<Vec<f32>> {
    let values = value.as_array().context("missing embedding array")?;
    let vector = values
        .iter()
        .map(|item| {
            item.as_f64()
                .map(|number| number as f32)
                .context("embedding contains a non-numeric value")
        })
        .collect::<Result<Vec<_>>>()?;
    if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
        bail!("embedding is empty or contains a non-finite value");
    }
    Ok(vector)
}

fn validate_vectors(expected: usize, vectors: &[Vec<f32>]) -> Result<()> {
    if vectors.len() != expected {
        bail!(
            "embedding count mismatch: {expected} inputs, {} vectors",
            vectors.len()
        );
    }
    let dimensions = vectors.first().map(Vec::len).unwrap_or(0);
    if dimensions == 0 || vectors.iter().any(|vector| vector.len() != dimensions) {
        bail!("embedding response has empty or inconsistent dimensions");
    }
    Ok(())
}

fn profile_fingerprint(provider: &str, model: &str, config_json: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-embedding-profile\0");
    for value in [provider, model, config_json] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

pub fn vec_to_blob(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The content-addressed cache embeds content only. Path, scope, and symbol
/// metadata are occurrence-specific and therefore cannot be part of text keyed
/// solely by `(chunk_hash, profile_id)` without making duplicate occurrences
/// depend on an arbitrary representative path.
pub fn embed_text(content: &str) -> String {
    let mut text = content.to_string();
    if text.len() > 24_000 {
        let mut cut = 24_000;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

#[derive(Debug, PartialEq, Eq)]
struct MissingEmbeddingDocument {
    hash: String,
    content: String,
}

fn missing_embedding_documents(
    conn: &Connection,
    profile_fingerprint: &str,
    profile_id: Option<i64>,
    file_origins: &[String],
) -> Result<Vec<MissingEmbeddingDocument>> {
    let flags = origin_flags(file_origins);
    let mut statement = conn.prepare(
        "SELECT c.hash, MIN(c.content), COUNT(DISTINCT c.content)
         FROM chunks c JOIN files f ON c.file_id=f.id
         WHERE NOT EXISTS (
           SELECT 1 FROM embeddings e
           WHERE e.chunk_hash=c.hash
             AND e.profile_id=COALESCE(
               ?5,
               (SELECT id FROM embedding_profiles WHERE config_fingerprint=?1)
             )
         )
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
         GROUP BY c.hash
         ORDER BY c.hash",
    )?;
    let rows = statement.query_map(
        params![profile_fingerprint, flags.0, flags.1, flags.2, profile_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut documents = Vec::new();
    for row in rows {
        let (hash, content, distinct_contents) = row?;
        if distinct_contents != 1 {
            bail!(
                "chunk hash `{hash}` maps to {distinct_contents} different contents; refusing to cache an ambiguous embedding"
            );
        }
        documents.push(MissingEmbeddingDocument { hash, content });
    }
    Ok(documents)
}

pub fn embed_missing(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
) -> Result<(usize, usize)> {
    embed_missing_for_origins(conn, provider, batch_size, &crate::origin::defaults())
}

pub fn embed_missing_for_origins(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
    file_origins: &[String],
) -> Result<(usize, usize)> {
    if batch_size == 0 {
        bail!("embedding batch size must be positive");
    }
    crate::origin::validate_all(file_origins)?;
    let profile = provider.profile()?;
    let mut resolved = existing_profile(conn, &profile)?;
    let resolved_profile_id = resolved.as_ref().map(|profile| profile.id);
    let rows = missing_embedding_documents(
        conn,
        &profile.fingerprint,
        resolved_profile_id,
        file_origins,
    )?;

    let total = rows.len();
    let mut done = 0usize;
    // The local HTTP boundary accepts at most 500k characters. Sixteen fully
    // expanded 24k-character chunks remain inside that limit and the 4 MiB
    // request-body cap even for multibyte source.
    let request_batch_size = if provider.protocol == Protocol::Local {
        batch_size.min(16)
    } else {
        batch_size
    };
    for batch in rows.chunks(request_batch_size) {
        let texts = batch
            .iter()
            .map(|document| embed_text(&document.content))
            .collect::<Vec<_>>();
        let response = provider.embed_documents(&texts)?;
        validate_response_profile(&profile, &response)?;
        let dimensions = response.vectors[0].len();
        let current = ensure_profile(conn, &profile, dimensions)?;
        if let Some(previous) = &resolved
            && previous.id != current.id
        {
            bail!("embedding profile changed during one embed operation");
        }
        resolved = Some(current.clone());
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let write_result = (|| -> Result<()> {
            for (document, vector) in batch.iter().zip(&response.vectors) {
                if vector.len() != current.dimensions {
                    bail!("embedding dimensions changed during one response");
                }
                conn.execute(
                    "INSERT OR IGNORE INTO embeddings(chunk_hash, profile_id, vec)
                     VALUES(?1, ?2, ?3)",
                    params![document.hash, current.id, vec_to_blob(vector)],
                )?;
            }
            Ok(())
        })();
        match write_result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        done += batch.len();
        eprintln!("embedded {done}/{total}");
    }
    if let Some(profile) = resolved {
        sync_vector_index(conn, Some(profile.id))?;
    }
    Ok((done, total))
}

fn existing_profile(conn: &Connection, profile: &ProfileSpec) -> Result<Option<ResolvedProfile>> {
    let exact = conn
        .query_row(
            "SELECT id, dimensions FROM embedding_profiles WHERE config_fingerprint=?1",
            [&profile.fingerprint],
            |row| {
                Ok(ResolvedProfile {
                    id: row.get(0)?,
                    dimensions: row.get::<_, i64>(1)? as usize,
                })
            },
        )
        .optional()?;
    if exact.is_some() {
        return Ok(exact);
    }

    // Profiles created before device was removed from local cache identity
    // remain reusable. Device is diagnostic; dtype and every output-affecting
    // model setting remain in the fingerprint. Refuse an ambiguous legacy
    // database instead of silently choosing between multiple candidates.
    let expected: Value = serde_json::from_str(&profile.config_json)?;
    if expected["protocol"].as_str() != Some("jscout-local-v1") {
        return Ok(None);
    }
    let mut statement = conn.prepare(
        "SELECT id, dimensions, config_json
         FROM embedding_profiles WHERE provider=?1 AND model=?2 ORDER BY id",
    )?;
    let rows = statement.query_map(params![profile.provider, profile.model], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)? as usize,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut compatible = Vec::new();
    for row in rows {
        let (id, dimensions, config_json) = row?;
        let mut stored: Value = serde_json::from_str(&config_json)?;
        if stored["protocol"].as_str() == Some("jscout-local-v1") {
            stored.as_object_mut().map(|object| object.remove("device"));
            if stored == expected {
                compatible.push(ResolvedProfile { id, dimensions });
            }
        }
    }
    match compatible.len() {
        0 => Ok(None),
        1 => Ok(compatible.pop()),
        count => bail!(
            "{count} legacy local embedding profiles differ only by device; run `jscout embed` with a fresh database or remove the obsolete profiles explicitly"
        ),
    }
}

fn ensure_profile(
    conn: &Connection,
    profile: &ProfileSpec,
    dimensions: usize,
) -> Result<ResolvedProfile> {
    if dimensions == 0
        || profile
            .dimensions
            .is_some_and(|expected| expected != dimensions)
    {
        bail!(
            "embedding dimension mismatch: configuration={:?}, response={dimensions}",
            profile.dimensions
        );
    }
    if let Some(resolved) = existing_profile(conn, profile)? {
        if resolved.dimensions != dimensions {
            bail!("stored embedding profile has incompatible dimensions");
        }
        ensure_vector_table(conn, dimensions)?;
        return Ok(resolved);
    }
    conn.execute(
        "INSERT INTO embedding_profiles(
           provider, model, config_fingerprint, dimensions, config_json
         ) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(config_fingerprint) DO NOTHING",
        params![
            profile.provider,
            profile.model,
            profile.fingerprint,
            dimensions as i64,
            profile.config_json,
        ],
    )?;
    let resolved = existing_profile(conn, profile)?.context("embedding profile was not stored")?;
    if resolved.dimensions != dimensions {
        bail!("stored embedding profile has incompatible dimensions");
    }
    ensure_vector_table(conn, dimensions)?;
    Ok(resolved)
}

fn vector_table(dimensions: usize) -> Result<String> {
    if dimensions == 0 || dimensions > 8_192 {
        bail!("unsupported embedding dimensions: {dimensions}");
    }
    Ok(format!("vec_embeddings_{dimensions}"))
}

fn ensure_vector_table(conn: &Connection, dimensions: usize) -> Result<String> {
    let table = vector_table(dimensions)?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(
           embedding FLOAT[{dimensions}] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY,
           origin TEXT PARTITION KEY
         );"
    ))?;
    Ok(table)
}

pub fn sync_vector_index(conn: &Connection, only_profile: Option<i64>) -> Result<()> {
    let profiles = {
        let mut statement = conn.prepare(
            "SELECT id, dimensions FROM embedding_profiles
             WHERE ?1 IS NULL OR id=?1 ORDER BY id",
        )?;
        let rows = statement.query_map([only_profile], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let profiles = profiles
        .into_iter()
        .map(|(profile_id, dimensions)| Ok((profile_id, ensure_vector_table(conn, dimensions)?)))
        .collect::<Result<Vec<_>>>()?;

    // A savepoint keeps the explicit embed/repair operation atomic and nests
    // safely if a future write workflow already owns a transaction.
    conn.execute_batch("SAVEPOINT jscout_vector_sync")?;
    let sync_result = (|| -> Result<()> {
        for (profile_id, table) in profiles {
            conn.execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE profile_id=?1
                       AND rowid NOT IN (SELECT id FROM embedding_index_entries WHERE profile_id=?1)"
                ),
                [profile_id],
            )?;

            materialize_profile(conn, profile_id, &table)?;

            // Repair a virtual row lost by an older, non-transactional build
            // without discarding the durable occurrence identity.
            let missing_vectors = {
                let mut statement = conn.prepare(&format!(
                    "SELECT i.id, f.origin, e.vec
                     FROM embedding_index_entries i
                     JOIN chunks c ON c.id=i.chunk_id
                     JOIN files f ON f.id=c.file_id
                     JOIN embeddings e ON e.chunk_hash=c.hash AND e.profile_id=i.profile_id
                     LEFT JOIN {table} v ON v.rowid=i.id
                     WHERE i.profile_id=?1 AND v.rowid IS NULL"
                ))?;
                let rows = statement.query_map([profile_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            for (row_id, origin, vector) in missing_vectors {
                conn.execute(
                    &format!(
                        "INSERT INTO {table}(rowid, embedding, profile_id, origin)
                         VALUES(?1, ?2, ?3, ?4)"
                    ),
                    params![row_id, vector, profile_id, origin],
                )?;
            }

            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [vector_sync_key(profile_id)],
            )?;
        }
        Ok(())
    })();
    match sync_result {
        Ok(()) => conn.execute_batch("RELEASE jscout_vector_sync")?,
        Err(error) => {
            let _ =
                conn.execute_batch("ROLLBACK TO jscout_vector_sync; RELEASE jscout_vector_sync");
            return Err(error);
        }
    }
    Ok(())
}

fn materialize_profile(conn: &Connection, profile_id: i64, table: &str) -> Result<()> {
    let missing_chunks = {
        let mut statement = conn.prepare(
            "SELECT c.id, f.origin, e.vec
             FROM chunks c
             JOIN files f ON f.id=c.file_id
             JOIN embeddings e ON e.chunk_hash=c.hash AND e.profile_id=?1
             LEFT JOIN embedding_index_entries i
               ON i.chunk_id=c.id AND i.profile_id=e.profile_id
             WHERE i.id IS NULL",
        )?;
        let rows = statement.query_map([profile_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (chunk_id, origin, vector) in missing_chunks {
        conn.execute(
            "INSERT INTO embedding_index_entries(chunk_id, profile_id) VALUES(?1, ?2)",
            params![chunk_id, profile_id],
        )?;
        let row_id = conn.last_insert_rowid();
        conn.execute(
            &format!(
                "INSERT INTO {table}(rowid, embedding, profile_id, origin)
                 VALUES(?1, ?2, ?3, ?4)"
            ),
            params![row_id, vector, profile_id, origin],
        )?;
    }
    Ok(())
}

/// Materialize newly indexed chunk occurrences that can reuse durable cached
/// embeddings. Indexing calls this after canonical chunk changes; it does not
/// perform the expensive legacy virtual-row audit owned by `jscout embed`.
pub fn materialize_cached_embeddings(conn: &Connection) -> Result<()> {
    let profiles = {
        let mut statement =
            conn.prepare("SELECT id, dimensions FROM embedding_profiles ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if profiles.is_empty() {
        return Ok(());
    }
    let profiles = profiles
        .into_iter()
        .map(|(profile_id, dimensions)| Ok((profile_id, ensure_vector_table(conn, dimensions)?)))
        .collect::<Result<Vec<_>>>()?;
    conn.execute_batch("SAVEPOINT jscout_vector_materialize")?;
    let result = (|| -> Result<()> {
        for (profile_id, table) in profiles {
            materialize_profile(conn, profile_id, &table)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE jscout_vector_materialize")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO jscout_vector_materialize; RELEASE jscout_vector_materialize",
            );
            return Err(error);
        }
    }
    Ok(())
}

fn vector_sync_key(profile_id: i64) -> String {
    format!("embedding_index_synced_v1:{profile_id}")
}

/// The virtual index is transactionally maintained after its first complete
/// repair. A regular-table anti-join cheaply detects newly indexed chunk
/// occurrences that can reuse cached embeddings; it avoids auditing every
/// sqlite-vec row on every search.
fn vector_index_needs_sync(conn: &Connection, profile_id: i64) -> Result<bool> {
    let has_completed_sync = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM meta WHERE key=?1)",
        [vector_sync_key(profile_id)],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_completed_sync {
        return Ok(true);
    }
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1
           FROM chunks c
           JOIN embeddings e ON e.chunk_hash=c.hash AND e.profile_id=?1
           LEFT JOIN embedding_index_entries i
             ON i.chunk_id=c.id AND i.profile_id=e.profile_id
           WHERE i.id IS NULL
           LIMIT 1
         )",
        [profile_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(Into::into)
}

fn ready_search_profile(conn: &Connection, spec: &ProfileSpec) -> Result<ResolvedProfile> {
    let profile = existing_profile(conn, spec)?.with_context(|| {
        format!(
            "embedding profile `{}` is not materialized; run `jscout embed <root>`",
            spec.model
        )
    })?;
    if vector_index_needs_sync(conn, profile.id)? {
        bail!("vector index is not ready; run `jscout embed <root>` after indexing")
    }
    let table = vector_table(profile.dimensions)?;
    let table_exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [&table],
        |row| row.get::<_, bool>(0),
    )?;
    if !table_exists {
        bail!("vector index table is missing; run `jscout embed <root>` to repair it")
    }
    Ok(profile)
}

pub fn vector_search(
    conn: &Connection,
    provider: &Provider,
    query: &str,
    limit: usize,
    file_origins: &[String],
) -> Result<Vec<(i64, f64)>> {
    let timing = std::env::var_os("JSCOUT_TIMING").is_some();
    let total_started = std::time::Instant::now();
    let started = std::time::Instant::now();
    let spec = provider.profile()?;
    if timing {
        eprintln!("timing:     vector profile {:?}", started.elapsed());
    }
    let started = std::time::Instant::now();
    let profile = ready_search_profile(conn, &spec)?;
    if timing {
        eprintln!("timing:     vector index readiness {:?}", started.elapsed());
    }
    let started = std::time::Instant::now();
    let response = provider.embed_query(query)?;
    if timing {
        eprintln!("timing:     vector query embedding {:?}", started.elapsed());
    }
    validate_response_profile(&spec, &response)?;
    let vector = &response.vectors[0];
    if vector.len() != profile.dimensions {
        bail!("stored embedding profile has incompatible dimensions");
    }
    let scores = exact_vector_search(conn, &profile, vector, limit, file_origins)?;
    if timing {
        eprintln!("timing:     vector total {:?}", total_started.elapsed());
    }
    Ok(scores)
}

fn exact_vector_search(
    conn: &Connection,
    profile: &ResolvedProfile,
    vector: &[f32],
    limit: usize,
    file_origins: &[String],
) -> Result<Vec<(i64, f64)>> {
    let timing = std::env::var_os("JSCOUT_TIMING").is_some();
    let table = vector_table(profile.dimensions)?;
    let mut scores = Vec::new();
    let started = std::time::Instant::now();
    for origin in file_origins {
        let mut statement = conn.prepare(&format!(
            "SELECT i.chunk_id, v.distance
             FROM {table} v
             JOIN embedding_index_entries i ON i.id=v.rowid
             WHERE v.embedding MATCH ?1
               AND v.k=?2
               AND v.profile_id=?3
               AND v.origin=?4
             ORDER BY v.distance"
        ))?;
        let rows = statement.query_map(
            params![vec_to_blob(vector), limit.max(1) as i64, profile.id, origin],
            |row| {
                let distance = row.get::<_, f64>(1)?;
                Ok((row.get::<_, i64>(0)?, 1.0 - distance))
            },
        )?;
        scores.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
    }
    scores.sort_by(|left, right| right.1.total_cmp(&left.1));
    scores.truncate(limit);
    if timing {
        eprintln!("timing:     vector exact KNN {:?}", started.elapsed());
    }
    Ok(scores)
}

pub fn delete_vector_rows_for_file(conn: &Connection, file_id: i64) -> Result<()> {
    let rows = {
        let mut statement = conn.prepare(
            "SELECT i.id, p.dimensions
             FROM embedding_index_entries i
             JOIN chunks c ON c.id=i.chunk_id
             JOIN embedding_profiles p ON p.id=i.profile_id
             WHERE c.file_id=?1",
        )?;
        let rows = statement.query_map([file_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (row_id, dimensions) in rows {
        let table = ensure_vector_table(conn, dimensions)?;
        conn.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row_id])?;
    }
    Ok(())
}

pub fn clear_vector_rows(conn: &Connection) -> Result<()> {
    let dimensions = {
        let mut statement = conn.prepare("SELECT DISTINCT dimensions FROM embedding_profiles")?;
        let rows = statement.query_map([], |row| Ok(row.get::<_, i64>(0)? as usize))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for dimension in dimensions {
        let table = ensure_vector_table(conn, dimension)?;
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

fn origin_flags(origins: &[String]) -> (bool, bool, bool) {
    (
        origins.iter().any(|origin| origin == "repository"),
        origins.iter().any(|origin| origin == "workspace"),
        origins.iter().any(|origin| origin == "dependency"),
    )
}

fn validate_response_profile(profile: &ProfileSpec, response: &EmbeddingResponse) -> Result<()> {
    if let Some(fingerprint) = &response.profile_fingerprint
        && fingerprint != &profile.fingerprint
    {
        bail!(
            "local inference configuration changed between discovery and embedding; retry the operation"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DOCUMENT_TEXT_FORMAT, ProfileSpec, Protocol, Provider, embed_text, ensure_profile,
        exact_vector_search, existing_profile, materialize_cached_embeddings,
        missing_embedding_documents, profile_fingerprint, ready_search_profile, sync_vector_index,
        validate_endpoint, vec_to_blob, vector_index_needs_sync, vector_table,
    };

    #[test]
    fn profile_fingerprint_changes_with_configuration() {
        let first = profile_fingerprint("local", "m", r#"{"dtype":"float16"}"#);
        let second = profile_fingerprint("local", "m", r#"{"dtype":"float32"}"#);
        assert_ne!(first, second);
    }

    #[test]
    fn device_only_legacy_profile_is_reused_without_duplication() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let connection = crate::store::open(directory.path())?;
        let embedding = serde_json::json!({
            "model": "BAAI/bge-m3",
            "dimensions": 2,
            "revision": "pinned",
            "configuration": {
                "pooling": "cls",
                "normalized": true,
                "max_length": 4096,
                "dtype": "float16"
            }
        });
        let legacy_config = serde_json::json!({
            "protocol": "jscout-local-v1",
            "device": "mps",
            "document_text": DOCUMENT_TEXT_FORMAT,
            "embedding": embedding
        })
        .to_string();
        connection.execute(
            "INSERT INTO embedding_profiles(
               provider, model, config_fingerprint, dimensions, config_json
             ) VALUES('local', 'BAAI/bge-m3', ?1, 2, ?2)",
            rusqlite::params![
                profile_fingerprint("local", "BAAI/bge-m3", &legacy_config),
                legacy_config
            ],
        )?;
        let legacy_id = connection.last_insert_rowid();
        let config_json = serde_json::json!({
            "protocol": "jscout-local-v1",
            "document_text": DOCUMENT_TEXT_FORMAT,
            "embedding": embedding
        })
        .to_string();
        let spec = ProfileSpec {
            provider: "local".into(),
            model: "BAAI/bge-m3".into(),
            fingerprint: profile_fingerprint("local", "BAAI/bge-m3", &config_json),
            config_json,
            dimensions: Some(2),
        };

        assert_eq!(existing_profile(&connection, &spec)?.unwrap().id, legacy_id);
        assert_eq!(ensure_profile(&connection, &spec, 2)?.id, legacy_id);
        let profiles: i64 =
            connection.query_row("SELECT count(*) FROM embedding_profiles", [], |row| {
                row.get(0)
            })?;
        assert_eq!(profiles, 1);
        Ok(())
    }

    #[test]
    fn document_embedding_text_is_content_addressed_and_utf8_bounded() {
        assert_eq!(
            embed_text("export const answer = 42;"),
            "export const answer = 42;"
        );
        let long = "é".repeat(20_000);
        let embedded = embed_text(&long);
        assert!(embedded.len() <= 24_000);
        assert!(embedded.is_char_boundary(embedded.len()));
        assert!(!embedded.contains("// file:"));
    }

    #[test]
    fn embedding_profile_versions_the_document_text_format() -> anyhow::Result<()> {
        let provider = Provider {
            name: "openai-compatible".into(),
            model: "tiny".into(),
            url: "https://example.test/v1/embeddings".into(),
            key: None,
            protocol: Protocol::OpenAi,
            query_prefix: String::new(),
        };
        let profile = provider.profile()?;
        let config: serde_json::Value = serde_json::from_str(&profile.config_json)?;
        assert_eq!(config["document_text"], DOCUMENT_TEXT_FORMAT);

        let old_config = serde_json::json!({
            "protocol": "openai-embeddings-v1",
            "url": "https://example.test/v1/embeddings",
            "query_prefix": ""
        })
        .to_string();
        assert_ne!(
            profile.fingerprint,
            profile_fingerprint("openai-compatible", "tiny", &old_config)
        );
        Ok(())
    }

    #[test]
    fn missing_embeddings_are_selected_once_per_content_hash() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let connection = crate::store::open(directory.path())?;
        for (path, hash, content) in [
            ("a.ts", "same", "export const x = 1;"),
            ("nested/a.ts", "same", "export const x = 1;"),
            ("b.ts", "other", "export const y = 2;"),
        ] {
            connection.execute(
                "INSERT INTO files(path, hash, role, origin) VALUES(?1, ?2, 'production', 'repository')",
                rusqlite::params![path, format!("file-{path}")],
            )?;
            let file_id = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO chunks(
                   file_id, kind, scope_chain, symbols, start, end,
                   start_line, end_line, hash, content
                 ) VALUES(?1, 'module', '', '', 0, 1, 1, 1, ?2, ?3)",
                rusqlite::params![file_id, hash, content],
            )?;
        }

        let documents = missing_embedding_documents(
            &connection,
            "missing-profile",
            None,
            &["repository".into()],
        )?;
        assert_eq!(documents.len(), 2);
        assert_eq!(
            documents
                .iter()
                .map(|document| document.hash.as_str())
                .collect::<Vec<_>>(),
            ["other", "same"]
        );
        Ok(())
    }

    #[test]
    fn embedding_endpoints_reject_credentials_and_non_http_schemes() {
        assert!(validate_endpoint("http://127.0.0.1:8000/v1/embeddings").is_ok());
        assert!(validate_endpoint("https://gateway.example/v1/embeddings").is_ok());
        assert!(validate_endpoint("https://secret@gateway.example/v1/embeddings").is_err());
        assert!(validate_endpoint("file:///tmp/embeddings").is_err());
    }

    #[test]
    fn sqlite_vec_materializes_current_chunk_occurrences() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let connection = crate::store::open(directory.path())?;
        connection.execute(
            "INSERT INTO files(path, hash, role, origin) VALUES('a.ts', 'f', 'production', 'repository')",
            [],
        )?;
        let file_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO chunks(
               file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
             ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
            [file_id],
        )?;
        let chunk_id = connection.last_insert_rowid();
        let config_json = "{}".to_string();
        let spec = ProfileSpec {
            provider: "test".to_string(),
            model: "tiny".to_string(),
            fingerprint: profile_fingerprint("test", "tiny", &config_json),
            config_json,
            dimensions: Some(2),
        };
        let profile = ensure_profile(&connection, &spec, 2)?;
        connection.execute(
            "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
            rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        sync_vector_index(&connection, Some(profile.id))?;
        let table = vector_table(2)?;
        let found: (i64, f64) = connection.query_row(
            &format!(
                "SELECT i.chunk_id, v.distance FROM {table} v
                 JOIN embedding_index_entries i ON i.id=v.rowid
                 WHERE v.embedding MATCH ?1 AND v.k=1
                   AND v.profile_id=?2 AND v.origin='repository'"
            ),
            rusqlite::params![vec_to_blob(&[1.0, 0.0]), profile.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(found.0, chunk_id);
        assert!(found.1 < 0.0001);

        connection.pragma_update(None, "query_only", true)?;
        assert_eq!(ready_search_profile(&connection, &spec)?.id, profile.id);
        connection.pragma_update(None, "query_only", false)?;

        assert!(!vector_index_needs_sync(&connection, profile.id)?);
        connection.execute(
            "INSERT INTO chunks(
               file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
             ) VALUES(?1, 'function', '', '', 2, 3, 2, 2, 'same', 'alpha')",
            [file_id],
        )?;
        assert!(
            vector_index_needs_sync(&connection, profile.id)?,
            "a new occurrence of cached content must invalidate materialization"
        );
        materialize_cached_embeddings(&connection)?;
        assert!(!vector_index_needs_sync(&connection, profile.id)?);
        let materialized: i64 = connection.query_row(
            "SELECT count(*) FROM embedding_index_entries WHERE profile_id=?1",
            [profile.id],
            |row| row.get(0),
        )?;
        assert_eq!(materialized, 2);

        crate::store::delete_file(&connection, file_id)?;
        let vector_rows: i64 =
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        assert_eq!(vector_rows, 0, "file deletion must purge virtual rows");
        let cache_rows: i64 =
            connection.query_row("SELECT count(*) FROM embeddings", [], |row| row.get(0))?;
        assert_eq!(cache_rows, 1, "content-addressed cache should survive");
        Ok(())
    }

    #[test]
    fn vector_search_database_path_is_read_only() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let connection = crate::store::open(directory.path())?;
        connection.execute(
            "INSERT INTO files(path, hash, role, origin)
             VALUES('a.ts', 'f', 'production', 'repository')",
            [],
        )?;
        let file_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO chunks(
               file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
             ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
            [file_id],
        )?;
        let chunk_id = connection.last_insert_rowid();

        let config_json = r#"{"protocol":"openai-embeddings-v1"}"#.to_string();
        let spec = ProfileSpec {
            provider: "openai-compatible".into(),
            model: "tiny".into(),
            fingerprint: profile_fingerprint("openai-compatible", "tiny", &config_json),
            config_json,
            dimensions: None,
        };
        let profile = ensure_profile(&connection, &spec, 2)?;
        connection.execute(
            "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
            rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        sync_vector_index(&connection, Some(profile.id))?;

        connection.pragma_update(None, "query_only", true)?;
        let ready = ready_search_profile(&connection, &spec)?;
        let results =
            exact_vector_search(&connection, &ready, &[1.0, 0.0], 1, &["repository".into()])?;
        assert_eq!(results.first().map(|result| result.0), Some(chunk_id));
        Ok(())
    }
}
