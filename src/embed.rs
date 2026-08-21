use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const DEFAULT_LOCAL_DEADLINE_MS: u64 = 120_000;
/// The document representation is part of cache identity. Versioning it keeps
/// vectors produced from an older representation from being silently reused.
const DOCUMENT_TEXT_FORMAT: &str = "content-v2";
const SEMANTIC_DOCUMENT_TEXT_FORMAT: &str = "semantic-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Local,
    Voyage,
    OpenAi,
}

/// Embedding provider selection is explicit. API keys never select a provider,
/// and OPENAI_API_KEY is never forwarded to a custom endpoint.
#[derive(Clone)]
pub struct Provider {
    pub name: String,
    pub model: String,
    url: String,
    key: Option<String>,
    protocol: Protocol,
    query_prefix: String,
    revision: Option<String>,
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

#[derive(Clone, Copy, Debug, Default)]
pub struct VectorSearchTimings {
    pub embedding_query: std::time::Duration,
    pub vector_index: std::time::Duration,
}

#[derive(Debug)]
pub struct VectorSearchResult {
    pub ranking: Vec<(i64, f64)>,
    pub timings: VectorSearchTimings,
}

#[derive(Debug, Clone, Copy)]
enum VectorFailureKind {
    Inference,
    Index,
}

#[derive(Debug)]
struct VectorFailure {
    plane: &'static str,
    kind: VectorFailureKind,
    source: anyhow::Error,
}

impl std::fmt::Display for VectorFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            VectorFailureKind::Inference => {
                write!(
                    formatter,
                    "{} embedding service unavailable: {}",
                    self.plane, self.source
                )
            }
            VectorFailureKind::Index => {
                write!(
                    formatter,
                    "{} vector index unavailable: {}",
                    self.plane, self.source
                )
            }
        }
    }
}

impl std::error::Error for VectorFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn vector_failure(
    plane: &'static str,
    kind: VectorFailureKind,
    source: anyhow::Error,
) -> anyhow::Error {
    VectorFailure {
        plane,
        kind,
        source,
    }
    .into()
}

fn vector_failure_action(error: &anyhow::Error, index_action: &'static str) -> &'static str {
    match error
        .downcast_ref::<VectorFailure>()
        .map(|failure| failure.kind)
    {
        Some(VectorFailureKind::Inference) => {
            "start or repair the configured embedding service, then retry"
        }
        _ => index_action,
    }
}

fn semantic_vector_failure(kind: VectorFailureKind, source: anyhow::Error) -> anyhow::Error {
    vector_failure("semantic", kind, source)
}

pub(crate) fn semantic_vector_failure_action(error: &anyhow::Error) -> &'static str {
    vector_failure_action(error, "run jscout embed <root> --semantic-only")
}

pub(crate) fn code_vector_failure_action(error: &anyhow::Error) -> &'static str {
    vector_failure_action(error, "run jscout embed <root>")
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
    pub fn from_settings(
        embedding: &crate::config::EmbeddingSettings,
        inference: &crate::config::InferenceSettings,
    ) -> Result<Option<Self>> {
        let Some(choice) = embedding.provider.as_deref() else {
            return Ok(None);
        };
        let model = embedding
            .model
            .clone()
            .with_context(|| format!("embedding.model is required for provider {choice}"))?;
        let query_prefix = embedding
            .query_prefix
            .clone()
            .unwrap_or_else(|| default_query_prefix(&model).to_string());
        match choice {
            "local" => {
                let base = inference.url.trim_end_matches('/').to_string();
                validate_endpoint(&base)?;
                Ok(Some(Self {
                    name: "local".to_string(),
                    model,
                    url: format!("{base}/embed"),
                    key: None,
                    protocol: Protocol::Local,
                    query_prefix: String::new(),
                    revision: embedding.revision.clone(),
                }))
            }
            "voyage" => {
                let key_env = embedding.api_key_env.as_deref().unwrap_or("VOYAGE_API_KEY");
                let key = std::env::var(key_env).with_context(|| {
                    format!("embedding.provider=voyage requires secret environment {key_env}")
                })?;
                Ok(Some(Self {
                    name: "voyage".to_string(),
                    query_prefix,
                    model,
                    url: "https://api.voyageai.com/v1/embeddings".to_string(),
                    key: Some(key),
                    protocol: Protocol::Voyage,
                    revision: embedding.revision.clone(),
                }))
            }
            "openai" => {
                let custom_url = embedding.url.clone();
                if let Some(url) = &custom_url {
                    validate_endpoint(url)?;
                }
                // A custom server has a separate credential namespace. This
                // prevents an OpenAI secret from leaking to LM Studio, vLLM,
                // a gateway, or a mistyped host.
                let key_env = embedding.api_key_env.as_deref().unwrap_or_else(|| {
                    if custom_url.is_some() {
                        "JSCOUT_EMBED_KEY"
                    } else {
                        "OPENAI_API_KEY"
                    }
                });
                let key = if custom_url.is_some() {
                    std::env::var(key_env).ok()
                } else {
                    Some(std::env::var(key_env).with_context(|| {
                        format!("OpenAI embeddings require secret environment {key_env}")
                    })?)
                };
                Ok(Some(Self {
                    name: "openai-compatible".to_string(),
                    query_prefix,
                    model,
                    url: custom_url
                        .unwrap_or_else(|| "https://api.openai.com/v1/embeddings".to_string()),
                    key,
                    protocol: Protocol::OpenAi,
                    revision: embedding.revision.clone(),
                }))
            }
            _ => bail!(
                "unsupported embedding.provider={choice}; expected local, voyage, openai, or none"
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
                if let Some(revision) = &self.revision
                    && embedding["revision"].as_str() != Some(revision)
                {
                    bail!(
                        "local inference revision mismatch: jscout requested {}, service provides {}",
                        revision,
                        embedding["revision"].as_str().unwrap_or("<missing>")
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
                    "revision": self.revision,
                }),
                None,
            ),
            Protocol::OpenAi => (
                json!({
                    "protocol": "openai-embeddings-v1",
                    "document_text": DOCUMENT_TEXT_FORMAT,
                    "url": self.url,
                    "query_prefix": self.query_prefix,
                    "revision": self.revision,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmbeddingPassReport {
    pub embedded: usize,
    pub missing: usize,
    pub cached_reused: usize,
    pub occurrences_synced: usize,
    pub canceled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SemanticEmbeddingDocument {
    artifact_id: i64,
    document_hash: String,
    content: String,
}

fn semantic_embedding_documents(conn: &Connection) -> Result<Vec<SemanticEmbeddingDocument>> {
    let mut statement = conn.prepare(
        "SELECT artifact.id, artifact.artifact_type, artifact.canonical_name,
                artifact.body_json,
                COALESCE((
                  SELECT group_concat(anchor_key, char(10)) FROM (
                    SELECT DISTINCT support.anchor_key AS anchor_key
                    FROM semantic_supports support
                    WHERE support.artifact_id=artifact.id
                    ORDER BY support.anchor_key
                  )
                ), '')
         FROM semantic_artifacts artifact
         WHERE NOT EXISTS(
           SELECT 1 FROM semantic_artifacts successor
           WHERE successor.supersedes_artifact_id=artifact.id
         )
         ORDER BY artifact.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut documents = Vec::new();
    for row in rows {
        let (artifact_id, artifact_type, name, body_json, anchors) = row?;
        // Put the stable identity and evidence vocabulary before generated
        // prose. Large workflow bodies are bounded below, so placing anchors
        // last would systematically truncate the most useful bridge back to
        // code.
        let content = embed_text(&format!(
            "semantic artifact\ntype: {artifact_type}\nname: {}\nanchors:\n{anchors}\nbody: {body_json}",
            name.as_deref().unwrap_or_default()
        ));
        let mut hasher = blake3::Hasher::new();
        hasher.update(SEMANTIC_DOCUMENT_TEXT_FORMAT.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        documents.push(SemanticEmbeddingDocument {
            artifact_id,
            document_hash: hasher.finalize().to_hex().to_string(),
            content,
        });
    }
    Ok(documents)
}

fn missing_semantic_documents(
    conn: &Connection,
    profile_id: Option<i64>,
    documents: &[SemanticEmbeddingDocument],
) -> Result<Vec<SemanticEmbeddingDocument>> {
    let cached = if let Some(profile_id) = profile_id {
        let mut statement =
            conn.prepare("SELECT document_hash FROM semantic_embeddings WHERE profile_id=?1")?;
        let rows = statement.query_map([profile_id], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<std::collections::HashSet<_>, _>>()?
    } else {
        std::collections::HashSet::new()
    };
    let mut seen = std::collections::HashSet::new();
    Ok(documents
        .iter()
        .filter(|document| {
            !cached.contains(&document.document_hash) && seen.insert(document.document_hash.clone())
        })
        .cloned()
        .collect())
}

fn missing_embedding_documents(
    conn: &Connection,
    profile_fingerprint: &str,
    profile_id: Option<i64>,
    file_origins: &[String],
    product_only: bool,
) -> Result<Vec<MissingEmbeddingDocument>> {
    let flags = origin_flags(file_origins);
    let mut statement = conn.prepare(
        "SELECT c.hash, MIN(c.content), COUNT(DISTINCT c.content)
         FROM chunks c JOIN files f ON c.file_id=f.id
         LEFT JOIN repository_file_policy policy ON policy.file_id=f.id
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
           AND (NOT ?6 OR policy.effective_role='runtime'
             OR (policy.file_id IS NULL AND f.role IN ('production','unknown')))
         GROUP BY c.hash
         ORDER BY c.hash",
    )?;
    let rows = statement.query_map(
        params![
            profile_fingerprint,
            flags.0,
            flags.1,
            flags.2,
            profile_id,
            product_only,
        ],
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

fn selected_embedding_document_count(
    conn: &Connection,
    file_origins: &[String],
    product_only: bool,
) -> Result<usize> {
    let flags = origin_flags(file_origins);
    conn.query_row(
        "SELECT COUNT(*) FROM (
           SELECT c.hash
           FROM chunks c JOIN files f ON c.file_id=f.id
           LEFT JOIN repository_file_policy policy ON policy.file_id=f.id
           WHERE ((?1 AND f.origin='repository')
             OR (?2 AND f.origin='workspace')
             OR (?3 AND f.origin='dependency'))
             AND (NOT ?4 OR policy.effective_role='runtime'
               OR (policy.file_id IS NULL AND f.role IN ('production','unknown')))
           GROUP BY c.hash
         )",
        params![flags.0, flags.1, flags.2, product_only],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as usize)
    .map_err(Into::into)
}

fn selected_embedding_occurrence_count(
    conn: &Connection,
    profile_id: i64,
    file_origins: &[String],
    product_only: bool,
) -> Result<usize> {
    let flags = origin_flags(file_origins);
    conn.query_row(
        "SELECT COUNT(*)
         FROM chunks c
         JOIN files f ON c.file_id=f.id
         LEFT JOIN repository_file_policy policy ON policy.file_id=f.id
         JOIN embedding_index_entries entry
           ON entry.chunk_id=c.id AND entry.profile_id=?1
         WHERE ((?2 AND f.origin='repository')
           OR (?3 AND f.origin='workspace')
           OR (?4 AND f.origin='dependency'))
           AND (NOT ?5 OR policy.effective_role='runtime'
             OR (policy.file_id IS NULL AND f.role IN ('production','unknown')))",
        params![profile_id, flags.0, flags.1, flags.2, product_only],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as usize)
    .map_err(Into::into)
}

pub fn embed_missing_for_selection_report(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
    file_origins: &[String],
    product_only: bool,
) -> Result<EmbeddingPassReport> {
    embed_missing_for_selection_interruptible(
        conn,
        provider,
        batch_size,
        file_origins,
        product_only,
        || false,
    )
}

/// Embed missing default-origin chunks, stopping between provider batches when
/// the caller reports that this snapshot generation has been superseded.
/// Completed vectors remain in the durable content cache and are reused by the
/// next full refresh.
pub fn embed_missing_interruptible(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
    product_only: bool,
    should_cancel: impl FnMut() -> bool,
) -> Result<EmbeddingPassReport> {
    embed_missing_for_selection_interruptible(
        conn,
        provider,
        batch_size,
        &crate::origin::defaults(),
        product_only,
        should_cancel,
    )
}

fn embed_missing_for_selection_interruptible(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
    file_origins: &[String],
    product_only: bool,
    mut should_cancel: impl FnMut() -> bool,
) -> Result<EmbeddingPassReport> {
    if batch_size == 0 {
        bail!("embedding batch size must be positive");
    }
    crate::origin::validate_all(file_origins)?;
    if should_cancel() {
        return Ok(EmbeddingPassReport {
            canceled: true,
            ..EmbeddingPassReport::default()
        });
    }
    let profile = provider.profile()?;
    let mut resolved = existing_profile(conn, &profile)?;
    let resolved_profile_id = resolved.as_ref().map(|profile| profile.id);
    let rows = missing_embedding_documents(
        conn,
        &profile.fingerprint,
        resolved_profile_id,
        file_origins,
        product_only,
    )?;

    let eligible = selected_embedding_document_count(conn, file_origins, product_only)?;
    let total = rows.len();
    let cached_reused = eligible.saturating_sub(total);
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
        if should_cancel() {
            return Ok(EmbeddingPassReport {
                embedded: done,
                missing: total,
                cached_reused,
                occurrences_synced: 0,
                canceled: true,
            });
        }
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
    if should_cancel() {
        return Ok(EmbeddingPassReport {
            embedded: done,
            missing: total,
            cached_reused,
            occurrences_synced: 0,
            canceled: true,
        });
    }
    let occurrences_synced = if let Some(profile) = resolved {
        sync_vector_index(conn, Some(profile.id))?;
        selected_embedding_occurrence_count(conn, profile.id, file_origins, product_only)?
    } else {
        0
    };
    Ok(EmbeddingPassReport {
        embedded: done,
        missing: total,
        cached_reused,
        occurrences_synced,
        canceled: false,
    })
}

/// Embed current, non-superseded semantic artifacts and materialize their
/// sqlite-vec occurrence index. The durable cache is keyed by the exact
/// bounded semantic document, not by artifact id, so an unchanged generated
/// explanation can reuse its vector after a disposable source reindex.
pub fn embed_semantic_missing(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
) -> Result<(usize, usize)> {
    let report = embed_semantic_missing_interruptible(conn, provider, batch_size, || false)?;
    Ok((report.embedded, report.missing))
}

pub fn embed_semantic_missing_report(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
) -> Result<EmbeddingPassReport> {
    embed_semantic_missing_interruptible(conn, provider, batch_size, || false)
}

pub fn embed_semantic_missing_interruptible(
    conn: &Connection,
    provider: &Provider,
    batch_size: usize,
    mut should_cancel: impl FnMut() -> bool,
) -> Result<EmbeddingPassReport> {
    if batch_size == 0 {
        bail!("embedding batch size must be positive");
    }
    if should_cancel() {
        return Ok(EmbeddingPassReport {
            canceled: true,
            ..EmbeddingPassReport::default()
        });
    }
    let profile = provider.profile()?;
    let mut resolved = existing_profile(conn, &profile)?;
    let documents = semantic_embedding_documents(conn)?;
    let rows = missing_semantic_documents(
        conn,
        resolved.as_ref().map(|profile| profile.id),
        &documents,
    )?;
    let unique_documents = documents
        .iter()
        .map(|document| document.document_hash.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let total = rows.len();
    let cached_reused = unique_documents.saturating_sub(total);
    let mut done = 0usize;
    let request_batch_size = if provider.protocol == Protocol::Local {
        batch_size.min(16)
    } else {
        batch_size
    };
    for batch in rows.chunks(request_batch_size) {
        if should_cancel() {
            return Ok(EmbeddingPassReport {
                embedded: done,
                missing: total,
                cached_reused,
                occurrences_synced: 0,
                canceled: true,
            });
        }
        let texts = batch
            .iter()
            .map(|document| document.content.clone())
            .collect::<Vec<_>>();
        let response = provider.embed_documents(&texts)?;
        validate_response_profile(&profile, &response)?;
        let dimensions = response.vectors[0].len();
        let current = ensure_profile(conn, &profile, dimensions)?;
        if let Some(previous) = &resolved
            && previous.id != current.id
        {
            bail!("embedding profile changed during one semantic embed operation");
        }
        resolved = Some(current.clone());
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let write_result = (|| -> Result<()> {
            for (document, vector) in batch.iter().zip(&response.vectors) {
                if vector.len() != current.dimensions {
                    bail!("embedding dimensions changed during one response");
                }
                conn.execute(
                    "INSERT OR IGNORE INTO semantic_embeddings(document_hash, profile_id, vec)
                     VALUES(?1, ?2, ?3)",
                    params![document.document_hash, current.id, vec_to_blob(vector)],
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
        eprintln!("embedded semantic memory {done}/{total}");
    }
    if should_cancel() {
        return Ok(EmbeddingPassReport {
            embedded: done,
            missing: total,
            cached_reused,
            occurrences_synced: 0,
            canceled: true,
        });
    }
    let mut occurrences_synced = 0;
    if let Some(profile) = resolved {
        sync_semantic_vector_index(conn, &profile, &documents)?;
        occurrences_synced = documents.len();
    }
    Ok(EmbeddingPassReport {
        embedded: done,
        missing: total,
        cached_reused,
        occurrences_synced,
        canceled: false,
    })
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

fn semantic_vector_table(dimensions: usize) -> Result<String> {
    if dimensions == 0 || dimensions > 8_192 {
        bail!("unsupported embedding dimensions: {dimensions}");
    }
    Ok(format!("vec_semantic_embeddings_{dimensions}"))
}

fn ensure_semantic_vector_table(conn: &Connection, dimensions: usize) -> Result<String> {
    let table = semantic_vector_table(dimensions)?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(
           embedding FLOAT[{dimensions}] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY
         );"
    ))?;
    Ok(table)
}

fn semantic_vector_sync_key(profile_id: i64) -> String {
    format!("semantic_embedding_index_synced_v1:{profile_id}")
}

pub(crate) fn mark_semantic_vector_index_stale(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM meta WHERE key LIKE 'semantic_embedding_index_synced_v1:%'",
        [],
    )?;
    Ok(())
}

fn sync_semantic_vector_index(
    conn: &Connection,
    profile: &ResolvedProfile,
    documents: &[SemanticEmbeddingDocument],
) -> Result<()> {
    let table = ensure_semantic_vector_table(conn, profile.dimensions)?;
    let current = documents
        .iter()
        .map(|document| (document.artifact_id, document.document_hash.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    conn.execute_batch("SAVEPOINT jscout_semantic_vector_sync")?;
    let result = (|| -> Result<()> {
        let indexed = {
            let mut statement = conn.prepare(
                "SELECT id, artifact_id, document_hash
                 FROM semantic_embedding_index_entries
                 WHERE profile_id=?1",
            )?;
            let rows = statement.query_map([profile.id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (row_id, artifact_id, document_hash) in indexed {
            if current.get(&artifact_id).copied() != Some(document_hash.as_str()) {
                conn.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row_id])?;
                conn.execute(
                    "DELETE FROM semantic_embedding_index_entries WHERE id=?1",
                    [row_id],
                )?;
            }
        }

        conn.execute(
            &format!(
                "DELETE FROM {table}
                 WHERE profile_id=?1
                   AND rowid NOT IN (
                     SELECT id FROM semantic_embedding_index_entries WHERE profile_id=?1
                   )"
            ),
            [profile.id],
        )?;

        for document in documents {
            let vector: Vec<u8> = conn.query_row(
                "SELECT vec FROM semantic_embeddings
                 WHERE document_hash=?1 AND profile_id=?2",
                params![document.document_hash, profile.id],
                |row| row.get(0),
            )?;
            let row_id = match conn
                .query_row(
                    "SELECT id FROM semantic_embedding_index_entries
                     WHERE artifact_id=?1 AND profile_id=?2",
                    params![document.artifact_id, profile.id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            {
                Some(row_id) => row_id,
                None => {
                    conn.execute(
                        "INSERT INTO semantic_embedding_index_entries(
                           artifact_id, profile_id, document_hash
                         ) VALUES(?1, ?2, ?3)",
                        params![document.artifact_id, profile.id, document.document_hash],
                    )?;
                    conn.last_insert_rowid()
                }
            };
            let virtual_exists = conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE rowid=?1)"),
                [row_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !virtual_exists {
                conn.execute(
                    &format!(
                        "INSERT INTO {table}(rowid, embedding, profile_id)
                         VALUES(?1, ?2, ?3)"
                    ),
                    params![row_id, vector, profile.id],
                )?;
            }
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, '1')
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [semantic_vector_sync_key(profile.id)],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE jscout_semantic_vector_sync")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO jscout_semantic_vector_sync; RELEASE jscout_semantic_vector_sync",
            );
            return Err(error);
        }
    }
    Ok(())
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

fn semantic_vector_index_needs_sync(conn: &Connection, profile_id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM meta WHERE key=?1)",
        [semantic_vector_sync_key(profile_id)],
        |row| row.get::<_, bool>(0),
    )
    .map_err(Into::into)
}

fn ready_semantic_search_profile(conn: &Connection, spec: &ProfileSpec) -> Result<ResolvedProfile> {
    let profile = existing_profile(conn, spec)?.with_context(|| {
        format!(
            "embedding profile `{}` is not materialized; run `jscout embed <root> --semantic`",
            spec.model
        )
    })?;
    if semantic_vector_index_needs_sync(conn, profile.id)? {
        bail!("semantic vector index is not ready; run `jscout embed <root> --semantic`")
    }
    let table = semantic_vector_table(profile.dimensions)?;
    let table_exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [&table],
        |row| row.get::<_, bool>(0),
    )?;
    if !table_exists {
        bail!("semantic vector index table is missing; run `jscout embed <root> --semantic`")
    }
    if semantic_vector_index_has_gaps(conn, profile.id, &table)? {
        bail!(
            "semantic vector index is incomplete; run `jscout embed <root> --semantic` to repair it"
        )
    }
    Ok(profile)
}

/// Check whether one configured profile can be topped up after an interactive
/// annotation. This does not create tables or repair rows; callers use it
/// before the write so a repository without semantic vectors does not trigger
/// an unexpected full-corpus embed.
pub fn semantic_vector_index_ready(conn: &Connection, provider: &Provider) -> Result<bool> {
    let spec = provider
        .profile()
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Inference, error))?;
    let Some(profile) = existing_profile(conn, &spec)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?
    else {
        return Ok(false);
    };
    if semantic_vector_index_needs_sync(conn, profile.id)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?
    {
        return Ok(false);
    }
    let table = semantic_vector_table(profile.dimensions)?;
    let table_exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [&table],
            |row| row.get::<_, bool>(0),
        )
        .map_err(anyhow::Error::from)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?;
    let has_gaps = semantic_vector_index_has_gaps(conn, profile.id, &table)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?;
    Ok(table_exists && !has_gaps)
}

fn semantic_vector_index_has_gaps(conn: &Connection, profile_id: i64, table: &str) -> Result<bool> {
    conn.query_row(
        &format!(
            "SELECT
               EXISTS(
                 SELECT 1
                 FROM semantic_embedding_index_entries entry
                 LEFT JOIN {table} vector ON vector.rowid=entry.id
                 WHERE entry.profile_id=?1 AND vector.rowid IS NULL
               )
               OR EXISTS(
                 SELECT 1
                 FROM {table} vector
                 LEFT JOIN semantic_embedding_index_entries entry ON entry.id=vector.rowid
                 WHERE vector.profile_id=?1 AND entry.id IS NULL
               )"
        ),
        [profile_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(Into::into)
}

pub fn semantic_vector_search(
    conn: &Connection,
    provider: &Provider,
    query: &str,
    limit: usize,
) -> Result<VectorSearchResult> {
    let index_started = std::time::Instant::now();
    let spec = provider
        .profile()
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Inference, error))?;
    let profile = ready_semantic_search_profile(conn, &spec)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?;
    let mut vector_index = index_started.elapsed();
    let embedding_started = std::time::Instant::now();
    let response = provider
        .embed_query(query)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Inference, error))?;
    let embedding_query = embedding_started.elapsed();
    validate_response_profile(&spec, &response)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Inference, error))?;
    let vector = &response.vectors[0];
    if vector.len() != profile.dimensions {
        return Err(semantic_vector_failure(
            VectorFailureKind::Index,
            anyhow::anyhow!("stored embedding profile has incompatible dimensions"),
        ));
    }
    let index_started = std::time::Instant::now();
    let ranking = exact_semantic_vector_search(conn, &profile, vector, limit)
        .map_err(|error| semantic_vector_failure(VectorFailureKind::Index, error))?;
    vector_index += index_started.elapsed();
    Ok(VectorSearchResult {
        ranking,
        timings: VectorSearchTimings {
            embedding_query,
            vector_index,
        },
    })
}

fn exact_semantic_vector_search(
    conn: &Connection,
    profile: &ResolvedProfile,
    vector: &[f32],
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let table = semantic_vector_table(profile.dimensions)?;
    let candidate_limit = limit.max(1).saturating_mul(4).min(4_096);
    let mut statement = conn.prepare(&format!(
        "SELECT entry.artifact_id, vector.distance
         FROM {table} vector
         JOIN semantic_embedding_index_entries entry ON entry.id=vector.rowid
         WHERE vector.embedding MATCH ?1
           AND vector.k=?2
           AND vector.profile_id=?3
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=entry.artifact_id
           )
         ORDER BY vector.distance"
    ))?;
    let rows = statement.query_map(
        params![vec_to_blob(vector), candidate_limit as i64, profile.id],
        |row| {
            let distance = row.get::<_, f64>(1)?;
            Ok((row.get::<_, i64>(0)?, 1.0 - distance))
        },
    )?;
    let mut scores = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    scores.truncate(limit);
    Ok(scores)
}

pub fn vector_search(
    conn: &Connection,
    provider: &Provider,
    query: &str,
    limit: usize,
    file_origins: &[String],
) -> Result<VectorSearchResult> {
    let index_started = std::time::Instant::now();
    let spec = provider
        .profile()
        .map_err(|error| vector_failure("code", VectorFailureKind::Inference, error))?;
    let profile = ready_search_profile(conn, &spec)
        .map_err(|error| vector_failure("code", VectorFailureKind::Index, error))?;
    let mut vector_index = index_started.elapsed();
    let embedding_started = std::time::Instant::now();
    let response = provider
        .embed_query(query)
        .map_err(|error| vector_failure("code", VectorFailureKind::Inference, error))?;
    let embedding_query = embedding_started.elapsed();
    validate_response_profile(&spec, &response)
        .map_err(|error| vector_failure("code", VectorFailureKind::Inference, error))?;
    let vector = &response.vectors[0];
    if vector.len() != profile.dimensions {
        return Err(vector_failure(
            "code",
            VectorFailureKind::Index,
            anyhow::anyhow!("stored embedding profile has incompatible dimensions"),
        ));
    }
    let index_started = std::time::Instant::now();
    let ranking = exact_vector_search(conn, &profile, vector, limit, file_origins)
        .map_err(|error| vector_failure("code", VectorFailureKind::Index, error))?;
    vector_index += index_started.elapsed();
    Ok(VectorSearchResult {
        ranking,
        timings: VectorSearchTimings {
            embedding_query,
            vector_index,
        },
    })
}

fn exact_vector_search(
    conn: &Connection,
    profile: &ResolvedProfile,
    vector: &[f32],
    limit: usize,
    file_origins: &[String],
) -> Result<Vec<(i64, f64)>> {
    let table = vector_table(profile.dimensions)?;
    let mut scores = Vec::new();
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
mod tests;
