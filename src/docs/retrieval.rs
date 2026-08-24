use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::json;

use super::CHUNK_FORMAT_VERSION;
use super::corpus::{CapturedFile, capture_file};
use super::store::{DocsStore, SearchHit};
use crate::embed::{
    ProfileSpec, Provider, ResolvedProfile, validate_response_profile, vec_to_blob,
};
use crate::search::Reranker;

const RRF_K: f64 = 60.0;
const MAX_VECTOR_DIMENSIONS: usize = 8_192;
const SQLITE_VEC_MAX_K: usize = 4_096;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EmbedReport {
    pub snapshot_id: i64,
    pub profile_id: Option<i64>,
    pub profile_fingerprint: String,
    pub dimensions: Option<usize>,
    pub embeddable_occurrences: usize,
    pub unique_representations: usize,
    pub missing_before: usize,
    pub embedded: usize,
    pub cached_reused: usize,
    pub occurrences_materialized: usize,
    pub generation_published: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchOptions {
    pub limit: usize,
    pub response_bytes: usize,
    pub output: SearchOutput,
    pub vector: bool,
    pub vector_required: bool,
    pub rerank: bool,
    pub freshness: bool,
    pub max_rank_movement: usize,
    #[serde(skip_serializing)]
    pub reranker: Option<Reranker>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            response_bytes: 24_000,
            output: SearchOutput::Compact,
            vector: true,
            vector_required: false,
            rerank: true,
            freshness: false,
            max_rank_movement: 2,
            reranker: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchOutput {
    Compact,
    Pretty,
    Human,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VectorStatus {
    Disabled,
    NotConfigured,
    NotReady,
    Active,
    Degraded,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RerankerStatus {
    Disabled,
    NotConfigured,
    SkippedEmpty,
    Active,
    Degraded,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RetrievalDiagnostics {
    pub lexical_candidates: usize,
    pub vector_status: VectorStatus,
    pub vector_candidates: usize,
    pub vector_profile_id: Option<i64>,
    pub vector_dimensions: Option<usize>,
    pub vector_detail: Option<String>,
    pub rrf_k: f64,
    pub fused_candidates: usize,
    pub reranker_status: RerankerStatus,
    pub reranker_candidates: usize,
    pub reranker_detail: Option<String>,
    pub freshness_applied: bool,
    pub freshness_moved: usize,
    pub total_candidates: usize,
    pub budget_dropped: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RetrievalHit {
    #[serde(flatten)]
    pub document: SearchHit,
    pub rank: usize,
    pub base_rank: usize,
    /// Positive values are promotions relative to the relevance-only rank.
    pub movement: i64,
    pub lexical_score: Option<f64>,
    pub vector_score: Option<f64>,
    pub content: String,
    pub source_state: SourceState,
    pub source_detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Current,
    SourceMismatch,
}

impl SourceState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::SourceMismatch => "source_mismatch",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResponse {
    pub snapshot_id: i64,
    pub hits: Vec<RetrievalHit>,
    pub diagnostics: RetrievalDiagnostics,
    pub truncated: bool,
}

pub fn compact_search_string(result: &SearchResponse) -> Result<String> {
    let hits = result
        .hits
        .iter()
        .map(|hit| {
            json!({
                "rank": hit.rank,
                "base_rank": hit.base_rank,
                "movement": hit.movement,
                "path": hit.document.path,
                "title": hit.document.title,
                "heading": hit.document.breadcrumb,
                "lines": [hit.document.start_line, hit.document.end_line],
                "source_bytes": [hit.document.source_start, hit.document.source_end],
                "content": hit.content,
                "snapshot": hit.document.snapshot_id,
                "file_hash": hit.document.file_hash,
                "source_state": hit.source_state,
                "source_detail": hit.source_detail,
                "freshness": {
                    "basis": hit.document.freshness_basis,
                    "observed_snapshot": hit.document.freshness_sequence,
                    "observed_at": hit.document.freshness_observed_at,
                    "value": freshness_value(&hit.document),
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string(&json!({
        "snapshot": result.snapshot_id,
        "hits": hits,
        "truncated": result.truncated,
        "retrieval": {
            "vector": result.diagnostics.vector_status,
            "reranker": result.diagnostics.reranker_status,
            "freshness_applied": result.diagnostics.freshness_applied,
        },
    }))?)
}

pub fn human_search_string(result: &SearchResponse) -> String {
    let mut output = format!(
        "documentation snapshot {}: {} hits (vector={:?}, reranker={:?}, truncated={}, budget_dropped={})\n",
        result.snapshot_id,
        result.hits.len(),
        result.diagnostics.vector_status,
        result.diagnostics.reranker_status,
        result.truncated,
        result.diagnostics.budget_dropped,
    );
    for hit in &result.hits {
        let freshness = freshness_value(&hit.document).unwrap_or_else(|| "unknown".to_owned());
        let _ = writeln!(
            output,
            "\n{}. {}:{}-{}  {}\nsnapshot={} file_hash={} freshness_basis={} freshness_value={} base_rank={} movement={} source_state={}{}\n{}",
            hit.rank,
            hit.document.path,
            hit.document.start_line,
            hit.document.end_line,
            hit.document.breadcrumb,
            hit.document.snapshot_id,
            hit.document.file_hash,
            hit.document.freshness_basis,
            freshness,
            hit.base_rank,
            hit.movement,
            hit.source_state.as_str(),
            hit.source_detail
                .as_deref()
                .map_or_else(String::new, |detail| format!(" source_detail={detail}")),
            hit.content,
        );
    }
    output
}

#[derive(Debug, Clone)]
struct EmbeddingDocument {
    identity: Vec<u8>,
    text: String,
    occurrences: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    path: String,
    source_start: u64,
    source_end: u64,
    body_hash: [u8; 32],
}

#[derive(Debug, Clone)]
struct RankedCandidate {
    chunk_id: i64,
    score: f64,
    base_rank: usize,
}

#[derive(Debug, Clone)]
struct VectorCandidate {
    chunk_id: i64,
    distance: f64,
    source_key: SourceKey,
}

struct SearchState {
    lexical_ranking: Vec<(i64, f64)>,
    vector_ranking: Vec<(i64, f64)>,
    hits_by_id: HashMap<i64, SearchHit>,
    vector_status: VectorStatus,
    vector_profile_id: Option<i64>,
    vector_dimensions: Option<usize>,
    vector_detail: Option<String>,
}

/// Embed every missing representation in the current documentation snapshot.
/// Each completed provider batch is committed to the content-addressed cache.
/// The sqlite-vec occurrence projection and readiness marker are published in
/// one final transaction only after every current representation is cached.
pub fn embed_current(
    store: &mut DocsStore,
    provider: &Provider,
    batch: usize,
) -> Result<EmbedReport> {
    ensure!(
        batch > 0,
        "documentation embedding batch size must be positive"
    );
    let snapshot_id = store
        .current_snapshot_id()?
        .context("documentation database has no current snapshot; run `jscout docs index`")?;
    let profile_spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
    let profile_fingerprint = profile_spec.fingerprint.clone();
    let documents = embedding_documents(store.connection(), snapshot_id)?;
    let embeddable_occurrences = documents.iter().map(|row| row.occurrences).sum();

    let mut profile = existing_profile(store.connection(), &profile_spec)?;
    let missing = missing_documents(
        store.connection(),
        profile.as_ref().map(|profile| profile.id),
        &documents,
    )?;
    let missing_before = missing.len();
    let cached_reused = documents.len().saturating_sub(missing_before);
    let mut embedded = 0usize;

    // Keep provider requests beneath the local service's 500k-character and
    // 4 MiB body limits even when the configured batch is larger.
    let request_batch = batch.min(16);
    for rows in missing.chunks(request_batch) {
        let texts = rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>();
        let response = provider.embed_documents_for(&texts, CHUNK_FORMAT_VERSION)?;
        validate_response_profile(&profile_spec, &response)?;
        let dimensions = response
            .vectors
            .first()
            .map(Vec::len)
            .context("embedding provider returned no documentation vectors")?;
        let resolved = ensure_profile(store.connection(), &profile_spec, dimensions)?;
        if let Some(previous) = profile.as_ref() {
            ensure!(
                previous.id == resolved.id && previous.dimensions == resolved.dimensions,
                "embedding profile changed during one documentation embed operation"
            );
        }
        profile = Some(resolved.clone());

        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (document, vector) in rows.iter().zip(&response.vectors) {
            ensure!(
                vector.len() == resolved.dimensions,
                "embedding dimensions changed during one documentation response"
            );
            transaction.execute(
                "INSERT OR IGNORE INTO doc_embeddings(embedding_identity, profile_id, vec)
                 VALUES(?1, ?2, ?3)",
                params![document.identity, resolved.id, vec_to_blob(vector)],
            )?;
        }
        transaction.commit()?;
        embedded += rows.len();
    }

    let (occurrences_materialized, generation_published) = match profile.as_ref() {
        Some(profile) if !documents.is_empty() => {
            let materialized = materialize_current_generation(store, snapshot_id, profile)?;
            (materialized, true)
        }
        _ => (0, false),
    };

    Ok(EmbedReport {
        snapshot_id,
        profile_id: profile.as_ref().map(|profile| profile.id),
        profile_fingerprint,
        dimensions: profile.as_ref().map(|profile| profile.dimensions),
        embeddable_occurrences,
        unique_representations: documents.len(),
        missing_before,
        embedded,
        cached_reused,
        occurrences_materialized,
        generation_published,
    })
}

pub fn search(
    store: &DocsStore,
    provider: Option<&Provider>,
    query: &str,
    options: &SearchOptions,
) -> Result<SearchResponse> {
    ensure!(
        !query.trim().is_empty(),
        "documentation query must not be empty"
    );
    ensure!(
        options.limit > 0,
        "documentation search limit must be positive"
    );
    ensure!(
        options.response_bytes > 0,
        "documentation response byte limit must be positive"
    );
    if options.vector_required && !options.vector {
        bail!("vector participation cannot be both required and disabled");
    }

    // Search spans the current pointer, FTS, vector readiness, vector hits,
    // and result hydration. Keep them on one WAL snapshot so a concurrent
    // publication cannot mix two documentation generations.
    let _read_snapshot = store
        .connection()
        .unchecked_transaction()
        .context("begin documentation search read snapshot")?;

    let snapshot_id = store
        .current_snapshot_id()?
        .context("documentation database has no current snapshot; run `jscout docs index`")?;
    let candidate_limit = options
        .limit
        .saturating_add(options.max_rank_movement)
        .max(options.limit);
    let lexical_query = fts_query(query);
    let lexical_hits = if lexical_query.is_empty() {
        Vec::new()
    } else {
        store.lexical_search(&lexical_query, candidate_limit)?
    };
    let lexical_ranking = lexical_hits
        .iter()
        .map(|hit| (hit.chunk_id, hit.score))
        .collect::<Vec<_>>();

    let mut hits_by_id = lexical_hits
        .into_iter()
        .map(|hit| (hit.chunk_id, hit))
        .collect::<HashMap<_, _>>();
    let mut vector_status = if options.vector {
        VectorStatus::NotConfigured
    } else {
        VectorStatus::Disabled
    };
    let mut vector_detail = None;
    let mut vector_profile_id = None;
    let mut vector_dimensions = None;
    let mut vector_ranking = Vec::new();

    if options.vector {
        let Some(provider) = provider else {
            if options.vector_required {
                bail!("vector participation was required, but no embedding provider is configured");
            }
            return finish_search(
                store,
                snapshot_id,
                query,
                options,
                SearchState {
                    lexical_ranking,
                    vector_ranking,
                    hits_by_id,
                    vector_status,
                    vector_profile_id,
                    vector_dimensions,
                    vector_detail,
                },
            );
        };

        let profile_spec = match provider.profile_for(CHUNK_FORMAT_VERSION) {
            Ok(profile) => profile,
            Err(error) if !options.vector_required => {
                vector_status = VectorStatus::Degraded;
                vector_detail = Some(error.to_string());
                return finish_search(
                    store,
                    snapshot_id,
                    query,
                    options,
                    SearchState {
                        lexical_ranking,
                        vector_ranking,
                        hits_by_id,
                        vector_status,
                        vector_profile_id,
                        vector_dimensions,
                        vector_detail,
                    },
                );
            }
            Err(error) => {
                return Err(error).context("resolve required documentation embedding profile");
            }
        };
        let Some(profile) = existing_profile(store.connection(), &profile_spec)? else {
            vector_status = VectorStatus::NotReady;
            vector_detail = Some("the configured profile has no documentation embeddings".into());
            if options.vector_required {
                bail!(
                    "vector participation was required, but the configured documentation profile is not embedded"
                );
            }
            return finish_search(
                store,
                snapshot_id,
                query,
                options,
                SearchState {
                    lexical_ranking,
                    vector_ranking,
                    hits_by_id,
                    vector_status,
                    vector_profile_id,
                    vector_dimensions,
                    vector_detail,
                },
            );
        };
        vector_profile_id = Some(profile.id);
        vector_dimensions = Some(profile.dimensions);
        if generation_is_ready(store.connection(), snapshot_id, &profile)? {
            match vector_search(
                store.connection(),
                provider,
                &profile_spec,
                &profile,
                snapshot_id,
                query,
                candidate_limit,
            ) {
                Ok(ranking) => {
                    vector_status = VectorStatus::Active;
                    vector_ranking = ranking;
                }
                Err(error) if !options.vector_required => {
                    vector_status = VectorStatus::Degraded;
                    vector_detail = Some(error.to_string());
                }
                Err(error) => {
                    return Err(error).context("required documentation vector retrieval failed");
                }
            }
        } else {
            vector_status = VectorStatus::NotReady;
            vector_detail =
                Some("the current documentation snapshot has no complete vector generation".into());
            if options.vector_required {
                bail!(
                    "vector participation was required, but the current documentation vector generation is not ready"
                );
            }
        }
    }

    for &(chunk_id, _) in &vector_ranking {
        if let std::collections::hash_map::Entry::Vacant(entry) = hits_by_id.entry(chunk_id) {
            entry.insert(load_hit(store.connection(), snapshot_id, chunk_id, 0.0)?);
        }
    }

    finish_search(
        store,
        snapshot_id,
        query,
        options,
        SearchState {
            lexical_ranking,
            vector_ranking,
            hits_by_id,
            vector_status,
            vector_profile_id,
            vector_dimensions,
            vector_detail,
        },
    )
}

fn finish_search(
    store: &DocsStore,
    snapshot_id: i64,
    query: &str,
    options: &SearchOptions,
    state: SearchState,
) -> Result<SearchResponse> {
    let SearchState {
        mut lexical_ranking,
        mut vector_ranking,
        mut hits_by_id,
        vector_status,
        vector_profile_id,
        vector_dimensions,
        vector_detail,
    } = state;
    for &(chunk_id, _) in lexical_ranking.iter().chain(&vector_ranking) {
        if let std::collections::hash_map::Entry::Vacant(entry) = hits_by_id.entry(chunk_id) {
            entry.insert(load_hit(store.connection(), snapshot_id, chunk_id, 0.0)?);
        }
    }
    let source_keys = hits_by_id
        .iter()
        .map(|(&chunk_id, hit)| (chunk_id, source_key(hit)))
        .collect::<HashMap<_, _>>();
    stable_score_sort(&mut lexical_ranking, &source_keys);
    stable_score_sort(&mut vector_ranking, &source_keys);

    let lexical_scores = lexical_ranking.iter().copied().collect::<HashMap<_, _>>();
    let vector_scores = vector_ranking.iter().copied().collect::<HashMap<_, _>>();
    let rankings = if vector_status == VectorStatus::Active {
        vec![lexical_ranking.as_slice(), vector_ranking.as_slice()]
    } else {
        vec![lexical_ranking.as_slice()]
    };
    let fused = reciprocal_rank_fusion(&rankings, &source_keys);
    let mut ranked = fused
        .into_iter()
        .map(|(chunk_id, score)| RankedCandidate {
            chunk_id,
            score,
            base_rank: 0,
        })
        .collect::<Vec<_>>();

    let mut reranker_status = if options.rerank {
        RerankerStatus::NotConfigured
    } else {
        RerankerStatus::Disabled
    };
    let mut reranker_detail = None;
    let mut reranker_candidates = 0usize;
    if options.rerank && ranked.is_empty() {
        reranker_status = RerankerStatus::SkippedEmpty;
    } else if options.rerank
        && let Some(reranker) = options.reranker.as_ref()
    {
        let documents = ranked
            .iter()
            .take(reranker.candidate_limit())
            .map(|candidate| {
                let hit = &hits_by_id[&candidate.chunk_id];
                let mut document =
                    format!("{}\n{}\n\n{}", hit.path, hit.breadcrumb, hit.rendered_body);
                reranker.truncate_document(&mut document);
                (candidate.chunk_id, document)
            })
            .collect::<Vec<_>>();
        reranker_candidates = documents.len();
        match reranker.rerank(query, &documents) {
            Ok(order) => {
                let fused = ranked
                    .iter()
                    .map(|candidate| (candidate.chunk_id, candidate.score))
                    .collect::<Vec<_>>();
                ranked = crate::search::merge_reranked_prefix(&fused, order)
                    .into_iter()
                    .map(|(chunk_id, score)| RankedCandidate {
                        chunk_id,
                        score,
                        base_rank: 0,
                    })
                    .collect();
                reranker_status = RerankerStatus::Active;
            }
            Err(error) => {
                reranker_status = RerankerStatus::Degraded;
                reranker_detail = Some(error.to_string());
            }
        }
    }

    for (position, candidate) in ranked.iter_mut().enumerate() {
        candidate.base_rank = position + 1;
    }
    let freshness_moved = if options.freshness {
        apply_observed_freshness(&mut ranked, &hits_by_id, options.max_rank_movement)
    } else {
        0
    };
    let total_candidates = ranked.len();
    let mut hits = ranked
        .into_iter()
        .take(options.limit)
        .enumerate()
        .map(|(position, candidate)| {
            let mut document = hits_by_id
                .remove(&candidate.chunk_id)
                .expect("ranked documentation hit was loaded");
            document.score = candidate.score;
            RetrievalHit {
                content: document.rendered_body.clone(),
                source_state: SourceState::SourceMismatch,
                source_detail: Some("not_resolved".to_owned()),
                document,
                rank: position + 1,
                base_rank: candidate.base_rank,
                movement: candidate.base_rank as i64 - (position + 1) as i64,
                lexical_score: lexical_scores.get(&candidate.chunk_id).copied(),
                vector_score: vector_scores.get(&candidate.chunk_id).copied(),
            }
        })
        .collect::<Vec<_>>();
    resolve_hit_sources(store.canonical_root(), &mut hits);

    let diagnostics = RetrievalDiagnostics {
        lexical_candidates: lexical_scores.len(),
        vector_status,
        vector_candidates: vector_scores.len(),
        vector_profile_id,
        vector_dimensions,
        vector_detail,
        rrf_k: RRF_K,
        fused_candidates: total_candidates,
        reranker_status,
        reranker_candidates,
        reranker_detail,
        freshness_applied: options.freshness,
        freshness_moved,
        total_candidates,
        budget_dropped: 0,
    };
    apply_response_budget(
        SearchResponse {
            snapshot_id,
            hits,
            diagnostics,
            truncated: false,
        },
        options.response_bytes,
        options.output,
    )
}

fn embedding_documents(conn: &Connection, snapshot_id: i64) -> Result<Vec<EmbeddingDocument>> {
    let invalid: i64 = conn.query_row(
        "SELECT COUNT(*) FROM doc_chunks
         WHERE snapshot_id=?1 AND stub=0
           AND (embedding_identity IS NULL OR provider_text IS NULL)",
        [snapshot_id],
        |row| row.get(0),
    )?;
    ensure!(
        invalid == 0,
        "current documentation snapshot has {invalid} non-stub chunks without embedding identity/text"
    );

    let mut statement = conn.prepare(
        "SELECT embedding_identity, MIN(provider_text), COUNT(DISTINCT provider_text), COUNT(*)
         FROM doc_chunks
         WHERE snapshot_id=?1
           AND embedding_identity IS NOT NULL
           AND provider_text IS NOT NULL
         GROUP BY embedding_identity
         ORDER BY embedding_identity",
    )?;
    let rows = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut documents = Vec::new();
    for row in rows {
        let (identity, text, distinct_texts, occurrences) = row?;
        ensure!(
            identity.len() == 32,
            "documentation embedding identity is not 32 bytes"
        );
        ensure!(
            distinct_texts == 1,
            "one documentation embedding identity maps to multiple provider texts"
        );
        documents.push(EmbeddingDocument {
            identity,
            text,
            occurrences: usize::try_from(occurrences)
                .context("documentation embedding occurrence count is negative")?,
        });
    }
    Ok(documents)
}

fn missing_documents(
    conn: &Connection,
    profile_id: Option<i64>,
    documents: &[EmbeddingDocument],
) -> Result<Vec<EmbeddingDocument>> {
    let Some(profile_id) = profile_id else {
        return Ok(documents.to_vec());
    };
    let mut exists = conn.prepare(
        "SELECT EXISTS(
           SELECT 1 FROM doc_embeddings
           WHERE embedding_identity=?1 AND profile_id=?2
         )",
    )?;
    let mut missing = Vec::new();
    for document in documents {
        let cached: bool =
            exists.query_row(params![document.identity, profile_id], |row| row.get(0))?;
        if !cached {
            missing.push(document.clone());
        }
    }
    Ok(missing)
}

fn existing_profile(conn: &Connection, spec: &ProfileSpec) -> Result<Option<ResolvedProfile>> {
    let fingerprint = fingerprint_blob(&spec.fingerprint)?;
    let stored = conn
        .query_row(
            "SELECT id, dimensions, provider, model, config_json
             FROM doc_embedding_profiles WHERE config_fingerprint=?1",
            [fingerprint],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, dimensions, provider, model, config_json)) = stored else {
        return Ok(None);
    };
    ensure!(
        provider == spec.provider && model == spec.model && config_json == spec.config_json,
        "stored documentation embedding profile fingerprint has incompatible configuration"
    );
    let dimensions = usize::try_from(dimensions)
        .context("stored documentation embedding dimensions are negative")?;
    validate_dimensions(dimensions)?;
    if let Some(expected) = spec.dimensions {
        ensure!(
            expected == dimensions,
            "stored documentation embedding profile has incompatible dimensions"
        );
    }
    Ok(Some(ResolvedProfile { id, dimensions }))
}

fn ensure_profile(
    conn: &Connection,
    spec: &ProfileSpec,
    dimensions: usize,
) -> Result<ResolvedProfile> {
    validate_dimensions(dimensions)?;
    if let Some(expected) = spec.dimensions {
        ensure!(
            expected == dimensions,
            "documentation embedding dimension mismatch: configuration={expected}, response={dimensions}"
        );
    }
    if let Some(profile) = existing_profile(conn, spec)? {
        ensure!(
            profile.dimensions == dimensions,
            "stored documentation embedding profile has incompatible dimensions"
        );
        return Ok(profile);
    }
    let fingerprint = fingerprint_blob(&spec.fingerprint)?;
    conn.execute(
        "INSERT INTO doc_embedding_profiles(
           provider, model, config_fingerprint, dimensions, config_json
         ) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(config_fingerprint) DO NOTHING",
        params![
            spec.provider,
            spec.model,
            fingerprint,
            dimensions as i64,
            spec.config_json
        ],
    )?;
    existing_profile(conn, spec)?.context("documentation embedding profile was not stored")
}

fn materialize_current_generation(
    store: &mut DocsStore,
    snapshot_id: i64,
    profile: &ResolvedProfile,
) -> Result<usize> {
    let table = ensure_vector_table(store.connection(), profile.dimensions)?;
    let expected_blob_len = profile
        .dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .context("documentation embedding dimensions overflow")?;
    let transaction = store
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_snapshot = transaction
        .query_row(
            "SELECT snapshot_id FROM doc_current_snapshot WHERE singleton=1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    ensure!(
        current_snapshot == Some(snapshot_id),
        "documentation snapshot advanced while embeddings were being prepared; cached batches were kept, rerun `jscout docs embed`"
    );
    transaction.execute(
        "DELETE FROM doc_vector_generations WHERE profile_id=?1",
        [profile.id],
    )?;
    transaction.execute(
        &format!("DELETE FROM {table} WHERE profile_id=?1"),
        [profile.id],
    )?;
    transaction.execute(
        "DELETE FROM doc_embedding_index_entries WHERE profile_id=?1",
        [profile.id],
    )?;

    let rows = {
        let mut statement = transaction.prepare(
            "SELECT c.id, c.embedding_identity, e.vec
             FROM doc_chunks c
             JOIN doc_embeddings e
               ON e.embedding_identity=c.embedding_identity AND e.profile_id=?2
             WHERE c.snapshot_id=?1
               AND c.embedding_identity IS NOT NULL
               AND c.provider_text IS NOT NULL
             ORDER BY c.id",
        )?;
        let mapped = statement.query_map(params![snapshot_id, profile.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let expected: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM doc_chunks
         WHERE snapshot_id=?1
           AND embedding_identity IS NOT NULL
           AND provider_text IS NOT NULL",
        [snapshot_id],
        |row| row.get(0),
    )?;
    ensure!(
        usize::try_from(expected).ok() == Some(rows.len()),
        "not every current documentation representation has a cached vector"
    );
    for (chunk_id, identity, vector) in &rows {
        ensure!(
            vector.len() == expected_blob_len,
            "cached documentation vector has incompatible dimensions"
        );
        transaction.execute(
            "INSERT INTO doc_embedding_index_entries(
               chunk_id, embedding_identity, profile_id
             ) VALUES(?1, ?2, ?3)",
            params![chunk_id, identity, profile.id],
        )?;
        let row_id = transaction.last_insert_rowid();
        transaction.execute(
            &format!(
                "INSERT INTO {table}(rowid, embedding, profile_id, snapshot_id)
                 VALUES(?1, ?2, ?3, ?4)"
            ),
            params![row_id, vector, profile.id, snapshot_id],
        )?;
    }
    transaction.execute(
        "INSERT INTO doc_vector_generations(
           snapshot_id, profile_id, dimensions, chunk_format, ready_at
         ) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            snapshot_id,
            profile.id,
            profile.dimensions as i64,
            CHUNK_FORMAT_VERSION,
            unix_timestamp()?
        ],
    )?;
    transaction.commit()?;
    Ok(rows.len())
}

fn ensure_vector_table(conn: &Connection, dimensions: usize) -> Result<String> {
    validate_dimensions(dimensions)?;
    let table = vector_table(dimensions);
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(
           embedding FLOAT[{dimensions}] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY,
           snapshot_id INTEGER PARTITION KEY
         );"
    ))?;
    Ok(table)
}

fn generation_is_ready(
    conn: &Connection,
    snapshot_id: i64,
    profile: &ResolvedProfile,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM doc_vector_generations
           WHERE snapshot_id=?1 AND profile_id=?2 AND dimensions=?3 AND chunk_format=?4
         )",
        params![
            snapshot_id,
            profile.id,
            profile.dimensions as i64,
            CHUNK_FORMAT_VERSION
        ],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn vector_search(
    conn: &Connection,
    provider: &Provider,
    profile_spec: &ProfileSpec,
    profile: &ResolvedProfile,
    snapshot_id: i64,
    query: &str,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let table = vector_table(profile.dimensions);
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
         )",
        [&table],
        |row| row.get(0),
    )?;
    ensure!(
        exists,
        "documentation vector generation has no sqlite-vec table"
    );
    let response = provider.embed_query_for(query, CHUNK_FORMAT_VERSION)?;
    validate_response_profile(profile_spec, &response)?;
    let vector = response
        .vectors
        .first()
        .context("embedding provider returned no documentation query vector")?;
    ensure!(
        response.vectors.len() == 1 && vector.len() == profile.dimensions,
        "documentation query vector has incompatible dimensions"
    );

    let occurrence_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM doc_embedding_index_entries e
         JOIN doc_chunks c ON c.id=e.chunk_id
         WHERE e.profile_id=?1 AND c.snapshot_id=?2",
        params![profile.id, snapshot_id],
        |row| row.get(0),
    )?;
    if occurrence_count == 0 {
        return Ok(Vec::new());
    }
    let occurrence_count = usize::try_from(occurrence_count)
        .context("documentation vector occurrence count is negative")?;
    adaptive_vector_search(
        conn,
        &table,
        profile.id,
        snapshot_id,
        &vec_to_blob(vector),
        occurrence_count,
        limit,
    )
}

fn adaptive_vector_search(
    conn: &Connection,
    table: &str,
    profile_id: i64,
    snapshot_id: i64,
    query_vector: &[u8],
    occurrence_count: usize,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let target = limit.min(occurrence_count);
    if target == 0 {
        return Ok(Vec::new());
    }

    // sqlite-vec caps KNN queries at 4096. Usually limit+1 is sufficient: the
    // extra row proves whether the distance tie at the requested cutoff is
    // closed. Grow K only while that tie reaches the end of the fetched set.
    // If the tie is still open at sqlite-vec's cap, compute every distance
    // from the content-addressed cache so the source-key tie break sees the
    // complete equivalence class.
    if target >= SQLITE_VEC_MAX_K && occurrence_count > SQLITE_VEC_MAX_K {
        return full_distance_vector_search(
            conn,
            profile_id,
            snapshot_id,
            query_vector,
            occurrence_count,
            limit,
        );
    }

    let mut k = target
        .saturating_add(1)
        .min(occurrence_count)
        .min(SQLITE_VEC_MAX_K);
    loop {
        let candidates =
            knn_vector_candidates(conn, table, profile_id, snapshot_id, query_vector, k)?;
        ensure!(
            candidates.len() == k,
            "documentation vector generation is incomplete: requested {k} candidates, found {}",
            candidates.len()
        );

        if k == occurrence_count || cutoff_tie_is_closed(&candidates, target) {
            return Ok(finalize_vector_ranking(candidates, limit));
        }
        if k == SQLITE_VEC_MAX_K {
            return full_distance_vector_search(
                conn,
                profile_id,
                snapshot_id,
                query_vector,
                occurrence_count,
                limit,
            );
        }
        k = k
            .saturating_mul(2)
            .min(occurrence_count)
            .min(SQLITE_VEC_MAX_K);
    }
}

fn knn_vector_candidates(
    conn: &Connection,
    table: &str,
    profile_id: i64,
    snapshot_id: i64,
    query_vector: &[u8],
    k: usize,
) -> Result<Vec<VectorCandidate>> {
    let vector_k =
        i64::try_from(k).context("documentation vector K does not fit SQLite INTEGER")?;
    let mut statement = conn.prepare(&format!(
        "SELECT e.chunk_id, v.distance, c.path, c.source_start, c.source_end,
                c.rendered_body_hash
         FROM {table} v
         JOIN doc_embedding_index_entries e ON e.id=v.rowid
         JOIN doc_chunks c ON c.id=e.chunk_id
         WHERE v.embedding MATCH ?1
           AND v.k=?2
           AND v.profile_id=?3
           AND v.snapshot_id=?4
           AND c.snapshot_id=?4
         ORDER BY v.distance"
    ))?;
    let rows = statement.query_map(
        params![query_vector, vector_k, profile_id, snapshot_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        },
    )?;
    let mut candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(vector_candidate_from_row)
        .collect::<Result<Vec<_>>>()?;
    sort_vector_candidates(&mut candidates);
    Ok(candidates)
}

fn full_distance_vector_search(
    conn: &Connection,
    profile_id: i64,
    snapshot_id: i64,
    query_vector: &[u8],
    occurrence_count: usize,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let mut statement = conn.prepare(
        "SELECT e.chunk_id, vec_distance_cosine(d.vec, ?1), c.path,
                c.source_start, c.source_end, c.rendered_body_hash
         FROM doc_embedding_index_entries e
         JOIN doc_chunks c ON c.id=e.chunk_id
         JOIN doc_embeddings d
           ON d.embedding_identity=e.embedding_identity
          AND d.profile_id=e.profile_id
         WHERE e.profile_id=?2 AND c.snapshot_id=?3",
    )?;
    let rows = statement.query_map(params![query_vector, profile_id, snapshot_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Vec<u8>>(5)?,
        ))
    })?;
    let candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(vector_candidate_from_row)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        candidates.len() == occurrence_count,
        "documentation vector cache is incomplete: expected {occurrence_count} candidates, found {}",
        candidates.len()
    );
    Ok(finalize_vector_ranking(candidates, limit))
}

fn vector_candidate_from_row(
    (chunk_id, distance, path, source_start, source_end, body_hash): (
        i64,
        f64,
        String,
        i64,
        i64,
        Vec<u8>,
    ),
) -> Result<VectorCandidate> {
    Ok(VectorCandidate {
        chunk_id,
        distance,
        source_key: SourceKey {
            path,
            source_start: u64::try_from(source_start)
                .context("negative documentation vector source start")?,
            source_end: u64::try_from(source_end)
                .context("negative documentation vector source end")?,
            body_hash: body_hash
                .try_into()
                .map_err(|_| anyhow::anyhow!("documentation vector body hash is not 32 bytes"))?,
        },
    })
}

fn cutoff_tie_is_closed(candidates: &[VectorCandidate], target: usize) -> bool {
    debug_assert!(target > 0 && target < candidates.len());
    candidates[target - 1]
        .distance
        .total_cmp(&candidates[candidates.len() - 1].distance)
        .is_lt()
}

fn finalize_vector_ranking(mut candidates: Vec<VectorCandidate>, limit: usize) -> Vec<(i64, f64)> {
    sort_vector_candidates(&mut candidates);
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|candidate| (candidate.chunk_id, 1.0 - candidate.distance))
        .collect()
}

fn sort_vector_candidates(candidates: &mut [VectorCandidate]) {
    candidates.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.source_key.cmp(&right.source_key))
    });
}

fn load_hit(conn: &Connection, snapshot_id: i64, chunk_id: i64, score: f64) -> Result<SearchHit> {
    let raw = conn
        .query_row(
            "SELECT c.id, c.snapshot_id, c.path, f.title, c.breadcrumb,
                    c.nearest_heading, c.rendered_body, c.source_start, c.source_end,
                    c.start_line, c.end_line, lower(hex(f.file_hash)), f.byte_len,
                    c.embedding_identity, c.stub, c.freshness_basis,
                    c.freshness_sequence, freshness_snapshot.observed_at
             FROM doc_chunks c
             JOIN doc_files f ON f.path=c.path
             LEFT JOIN doc_snapshots freshness_snapshot
               ON freshness_snapshot.id=c.freshness_sequence
             WHERE c.id=?1 AND c.snapshot_id=?2",
            params![chunk_id, snapshot_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, Option<Vec<u8>>>(13)?,
                    row.get::<_, bool>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, Option<i64>>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                ))
            },
        )
        .with_context(|| format!("load current documentation chunk {chunk_id}"))?;
    Ok(SearchHit {
        chunk_id: raw.0,
        snapshot_id: raw.1,
        path: raw.2,
        title: raw.3,
        breadcrumb: raw.4,
        nearest_heading: raw.5,
        rendered_body: raw.6,
        source_start: u64::try_from(raw.7).context("negative documentation source start")?,
        source_end: u64::try_from(raw.8).context("negative documentation source end")?,
        start_line: u32::try_from(raw.9).context("invalid documentation start line")?,
        end_line: u32::try_from(raw.10).context("invalid documentation end line")?,
        file_hash: raw.11,
        file_byte_len: u64::try_from(raw.12).context("negative documentation file byte length")?,
        embedding_identity: raw.13.unwrap_or_default(),
        score,
        stub: raw.14,
        freshness_basis: raw.15,
        freshness_sequence: raw.16,
        freshness_observed_at: raw.17,
    })
}

enum CapturedSource {
    Current(Vec<u8>),
    Mismatch(String),
}

fn resolve_hit_sources(root: &Path, hits: &mut [RetrievalHit]) {
    let mut captured = HashMap::<String, CapturedSource>::new();
    for hit in hits {
        let capture = captured
            .entry(hit.document.path.clone())
            .or_insert_with(|| {
                capture_source(
                    root,
                    &hit.document.path,
                    &hit.document.file_hash,
                    hit.document.file_byte_len,
                )
            });
        match capture {
            CapturedSource::Current(bytes) => {
                let Some(range) = source_range(&hit.document, bytes.len()) else {
                    hit.source_detail = Some("invalid_indexed_span".to_owned());
                    continue;
                };
                match std::str::from_utf8(&bytes[range]) {
                    Ok(source) => {
                        hit.content = source.to_owned();
                        hit.source_state = SourceState::Current;
                        hit.source_detail = None;
                    }
                    Err(_) => {
                        hit.source_detail = Some("invalid_utf8_slice".to_owned());
                    }
                }
            }
            CapturedSource::Mismatch(detail) => {
                hit.source_detail = Some(detail.clone());
            }
        }
    }
}

fn capture_source(
    root: &Path,
    relative: &str,
    expected_hash: &str,
    indexed_byte_len: u64,
) -> CapturedSource {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return CapturedSource::Mismatch("invalid_indexed_path".to_owned());
    }
    match capture_file(&root.join(path), indexed_byte_len) {
        Ok(CapturedFile::Bytes(bytes))
            if blake3::hash(&bytes)
                .to_hex()
                .as_str()
                .eq_ignore_ascii_case(expected_hash) =>
        {
            CapturedSource::Current(bytes)
        }
        Ok(CapturedFile::Bytes(_)) => CapturedSource::Mismatch("hash_mismatch".to_owned()),
        Ok(CapturedFile::Oversized) => CapturedSource::Mismatch("oversized".to_owned()),
        Ok(CapturedFile::NotRegular) => CapturedSource::Mismatch("not_regular".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CapturedSource::Mismatch("missing".to_owned())
        }
        Err(_) => CapturedSource::Mismatch("unreadable".to_owned()),
    }
}

fn source_range(hit: &SearchHit, file_len: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(hit.source_start).ok()?;
    let end = usize::try_from(hit.source_end).ok()?;
    (start <= end && end <= file_len).then_some(start..end)
}

fn freshness_value(hit: &SearchHit) -> Option<String> {
    match (
        hit.freshness_basis.as_str(),
        hit.freshness_sequence,
        hit.freshness_observed_at,
    ) {
        ("observed", Some(sequence), Some(observed_at)) => Some(format!(
            "observed in documentation snapshot {sequence} at Unix timestamp {observed_at}"
        )),
        _ => None,
    }
}

fn reciprocal_rank_fusion(
    rankings: &[&[(i64, f64)]],
    source_keys: &HashMap<i64, SourceKey>,
) -> Vec<(i64, f64)> {
    let mut scores = HashMap::<i64, f64>::new();
    for ranking in rankings {
        for (position, (chunk_id, _)) in ranking.iter().enumerate() {
            *scores.entry(*chunk_id).or_default() += 1.0 / (RRF_K + (position + 1) as f64);
        }
    }
    let mut fused = scores.into_iter().collect::<Vec<_>>();
    stable_score_sort(&mut fused, source_keys);
    fused
}

fn stable_score_sort(ranking: &mut [(i64, f64)], source_keys: &HashMap<i64, SourceKey>) {
    ranking.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| source_keys[&left.0].cmp(&source_keys[&right.0]))
    });
}

fn apply_observed_freshness(
    ranking: &mut [RankedCandidate],
    hits: &HashMap<i64, SearchHit>,
    max_movement: usize,
) -> usize {
    if ranking.len() < 2 || max_movement == 0 {
        return 0;
    }
    loop {
        let mut swapped = false;
        for position in 0..ranking.len() - 1 {
            let upper = &ranking[position];
            let lower = &ranking[position + 1];
            if !strictly_newer_observed(&hits[&lower.chunk_id], &hits[&upper.chunk_id]) {
                continue;
            }
            let lower_new_rank = position + 1;
            let upper_new_rank = position + 2;
            if lower.base_rank.abs_diff(lower_new_rank) <= max_movement
                && upper.base_rank.abs_diff(upper_new_rank) <= max_movement
            {
                ranking.swap(position, position + 1);
                swapped = true;
            }
        }
        if !swapped {
            break;
        }
    }
    ranking
        .iter()
        .enumerate()
        .filter(|(position, candidate)| candidate.base_rank != position + 1)
        .count()
}

fn strictly_newer_observed(lower: &SearchHit, upper: &SearchHit) -> bool {
    lower.freshness_basis == "observed"
        && upper.freshness_basis == "observed"
        && lower
            .freshness_sequence
            .zip(upper.freshness_sequence)
            .is_some_and(|(lower, upper)| lower > upper)
}

fn apply_response_budget(
    mut response: SearchResponse,
    byte_limit: usize,
    output: SearchOutput,
) -> Result<SearchResponse> {
    let rendered_len = |response: &SearchResponse| -> Result<usize> {
        match output {
            SearchOutput::Compact => Ok(compact_search_string(response)?.len()),
            SearchOutput::Pretty => Ok(serde_json::to_string_pretty(response)?.len() + 1),
            SearchOutput::Human => Ok(human_search_string(response).len()),
        }
    };
    while rendered_len(&response)? > byte_limit {
        if response.hits.pop().is_none() {
            let minimum = rendered_len(&response)?;
            bail!(
                "response_budget_too_small: documentation response byte limit {byte_limit} cannot fit response metadata; minimum_bytes={minimum}"
            );
        }
        response.truncated = true;
        response.diagnostics.budget_dropped += 1;
    }
    Ok(response)
}

fn source_key(hit: &SearchHit) -> SourceKey {
    SourceKey {
        path: hit.path.clone(),
        source_start: hit.source_start,
        source_end: hit.source_end,
        body_hash: *blake3::hash(hit.rendered_body.as_bytes()).as_bytes(),
    }
}

/// Treat user input as terms rather than exposing the FTS5 query language.
fn fts_query(query: &str) -> String {
    query
        .split(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '$')
        })
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn vector_table(dimensions: usize) -> String {
    format!("vec_doc_embeddings_{dimensions}")
}

fn validate_dimensions(dimensions: usize) -> Result<()> {
    ensure!(
        (1..=MAX_VECTOR_DIMENSIONS).contains(&dimensions),
        "unsupported documentation embedding dimensions: {dimensions}"
    );
    Ok(())
}

fn fingerprint_blob(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len() == 64,
        "documentation embedding profile fingerprint is not 32-byte hex"
    );
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("documentation embedding profile fingerprint is not hexadecimal"),
    }
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock predates Unix epoch")?
        .as_secs();
    i64::try_from(seconds).context("system timestamp does not fit SQLite INTEGER")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: i64, path: &str, basis: &str, sequence: Option<i64>) -> SearchHit {
        SearchHit {
            chunk_id: id,
            snapshot_id: 1,
            path: path.to_string(),
            title: String::new(),
            breadcrumb: String::new(),
            nearest_heading: None,
            rendered_body: format!("body {id}"),
            source_start: 0,
            source_end: 10,
            start_line: 1,
            end_line: 1,
            file_hash: "00".repeat(32),
            file_byte_len: 10,
            embedding_identity: vec![id as u8; 32],
            score: 0.0,
            stub: false,
            freshness_basis: basis.to_string(),
            freshness_sequence: sequence,
            freshness_observed_at: sequence,
        }
    }

    #[test]
    fn fts_input_is_quoted_terms_not_query_syntax() {
        assert_eq!(
            fts_query("API/v2 OR foo-bar"),
            "\"API\" OR \"v2\" OR \"OR\" OR \"foo\" OR \"bar\""
        );
        assert!(fts_query("---").is_empty());
    }

    #[test]
    fn rrf_exact_ties_use_source_key_not_insertion_order() {
        let first = hit(1, "z.md", "unknown", None);
        let second = hit(2, "a.md", "unknown", None);
        let keys = HashMap::from([(1, source_key(&first)), (2, source_key(&second))]);
        let left = [(1, 10.0)];
        let right = [(2, 20.0)];
        let fused = reciprocal_rank_fusion(&[&left, &right], &keys);
        assert_eq!(fused.iter().map(|row| row.0).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(fused[0].1, 1.0 / 61.0);
    }

    #[test]
    fn vector_cutoff_expands_only_when_the_exact_tie_is_open() {
        let candidate = |chunk_id, distance| VectorCandidate {
            chunk_id,
            distance,
            source_key: SourceKey {
                path: format!("{chunk_id}.md"),
                source_start: 0,
                source_end: 1,
                body_hash: [0; 32],
            },
        };
        assert!(!cutoff_tie_is_closed(
            &[candidate(1, 0.25), candidate(2, 0.25)],
            1
        ));
        assert!(cutoff_tie_is_closed(
            &[candidate(1, 0.25), candidate(2, 0.5)],
            1
        ));
    }

    #[test]
    fn observed_freshness_is_bounded_and_unknown_never_moves() {
        let hits = HashMap::from([
            (1, hit(1, "a.md", "observed", Some(1))),
            (2, hit(2, "b.md", "observed", Some(3))),
            (3, hit(3, "c.md", "observed", Some(4))),
            (4, hit(4, "d.md", "unknown", None)),
        ]);
        let mut ranking = (1..=4)
            .map(|chunk_id| RankedCandidate {
                chunk_id,
                score: 0.0,
                base_rank: chunk_id as usize,
            })
            .collect::<Vec<_>>();
        let moved = apply_observed_freshness(&mut ranking, &hits, 1);
        assert_eq!(
            ranking.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [2, 1, 3, 4]
        );
        assert_eq!(moved, 2);
        for (position, candidate) in ranking.iter().enumerate() {
            assert!(candidate.base_rank.abs_diff(position + 1) <= 1);
        }
    }

    #[test]
    fn fingerprint_hex_decodes_to_exact_bytes() -> Result<()> {
        let value = format!("{}{}", "00".repeat(31), "ff");
        let bytes = fingerprint_blob(&value)?;
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[31], 255);
        Ok(())
    }

    #[test]
    fn human_budget_uses_the_exact_renderer_and_reports_truncation() -> Result<()> {
        let document = hit(1, "guide.md", "observed", Some(7));
        let response = SearchResponse {
            snapshot_id: 9,
            hits: vec![RetrievalHit {
                document: document.clone(),
                rank: 1,
                base_rank: 2,
                movement: 1,
                lexical_score: Some(1.0),
                vector_score: None,
                content: "current source".to_owned(),
                source_state: SourceState::Current,
                source_detail: None,
            }],
            diagnostics: RetrievalDiagnostics {
                lexical_candidates: 1,
                vector_status: VectorStatus::Disabled,
                vector_candidates: 0,
                vector_profile_id: None,
                vector_dimensions: None,
                vector_detail: None,
                rrf_k: RRF_K,
                fused_candidates: 1,
                reranker_status: RerankerStatus::Disabled,
                reranker_candidates: 0,
                reranker_detail: None,
                freshness_applied: true,
                freshness_moved: 1,
                total_candidates: 1,
                budget_dropped: 0,
            },
            truncated: false,
        };
        let full_len = human_search_string(&response).len();
        let budgeted = apply_response_budget(response, full_len - 1, SearchOutput::Human)?;
        assert!(budgeted.truncated);
        assert!(budgeted.hits.is_empty());
        assert!(human_search_string(&budgeted).len() < full_len);

        let compact: serde_json::Value =
            serde_json::from_str(&compact_search_string(&SearchResponse {
                hits: vec![RetrievalHit {
                    document,
                    rank: 1,
                    base_rank: 1,
                    movement: 0,
                    lexical_score: Some(1.0),
                    vector_score: None,
                    content: "current source".to_owned(),
                    source_state: SourceState::Current,
                    source_detail: None,
                }],
                ..budgeted
            })?)?;
        assert_eq!(compact["hits"][0]["source_state"], "current");
        assert!(
            compact["hits"][0]["freshness"]["value"]
                .as_str()
                .unwrap()
                .contains("snapshot 7")
        );
        Ok(())
    }

    #[test]
    fn checkout_capture_is_bounded_and_does_not_follow_symlinks() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("guide.md");
        std::fs::write(&path, "current")?;
        let expected = blake3::hash(b"current").to_hex().to_string();
        assert!(matches!(
            capture_source(root.path(), "guide.md", &expected, 7),
            CapturedSource::Current(bytes) if bytes == b"current"
        ));

        std::fs::write(&path, "current plus unbounded replacement")?;
        assert!(matches!(
            capture_source(root.path(), "guide.md", &expected, 7),
            CapturedSource::Mismatch(detail) if detail == "oversized"
        ));

        #[cfg(unix)]
        {
            std::fs::remove_file(&path)?;
            let outside = root.path().join("outside.md");
            std::fs::write(&outside, "current")?;
            std::os::unix::fs::symlink(&outside, &path)?;
            assert!(matches!(
                capture_source(root.path(), "guide.md", &expected, 7),
                CapturedSource::Mismatch(detail) if detail == "unreadable"
            ));
        }
        Ok(())
    }

    #[test]
    fn vector_search_resolves_open_ties_beyond_sqlite_vec_k_cap() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut store = DocsStore::open(
            root.path(),
            Some(std::path::Path::new("docs.sqlite")),
            Some(std::path::Path::new("code.sqlite")),
        )?;
        let snapshot_id = store.connection().query_row(
            "INSERT INTO doc_snapshots(
               observed_at, corpus_fingerprint, chunk_format, inventory_count,
               indexed_file_count, rejection_count
             ) VALUES(1, zeroblob(32), ?1, 1, 1, 0)
             RETURNING id",
            [CHUNK_FORMAT_VERSION],
            |row| row.get::<_, i64>(0),
        )?;
        store.connection().execute(
            "INSERT INTO doc_files(
               path, snapshot_id, file_hash, byte_len, line_count, title,
               description, tags_json, front_matter_state
             ) VALUES('ties.md', ?1, zeroblob(32), 1, 1, 'ties', NULL, '[]', 'absent')",
            [snapshot_id],
        )?;
        let spec = ProfileSpec {
            provider: "test".into(),
            model: "test-2d".into(),
            fingerprint: "22".repeat(32),
            config_json: "{}".into(),
            dimensions: Some(2),
        };
        let profile = ensure_profile(store.connection(), &spec, 2)?;
        let table = ensure_vector_table(store.connection(), 2)?;
        let identity = vec![7_u8; 32];
        let tied_vector = vec_to_blob(&[1.0, 0.0]);
        store.connection().execute(
            "INSERT INTO doc_embeddings(embedding_identity, profile_id, vec)
             VALUES(?1, ?2, ?3)",
            params![identity, profile.id, tied_vector],
        )?;

        let occurrence_count = SQLITE_VEC_MAX_K + 1;
        let transaction = store.connection_mut().transaction()?;
        let mut expected_first = 0;
        for ordinal in 0..occurrence_count {
            // Reverse source offsets so the normative source-key winner is the
            // final inserted row, which a capped rowid-ordered tie can omit.
            let source_offset = i64::try_from(occurrence_count - ordinal)?;
            transaction.execute(
                "INSERT INTO doc_chunks(
                   snapshot_id, path, chunk_order, source_start, source_end,
                   start_line, end_line, breadcrumb, nearest_heading,
                   rendered_body, provider_text, rendered_body_hash,
                   embedding_identity, stub, freshness_basis, freshness_sequence
                 ) VALUES(?1, 'ties.md', ?2, ?3, ?3, 1, 1, '', NULL,
                          'same', 'same', zeroblob(32), ?4, 0, 'unknown', NULL)",
                params![snapshot_id, ordinal as i64, source_offset, identity],
            )?;
            let chunk_id = transaction.last_insert_rowid();
            expected_first = chunk_id;
            transaction.execute(
                "INSERT INTO doc_embedding_index_entries(
                   chunk_id, embedding_identity, profile_id
                 ) VALUES(?1, ?2, ?3)",
                params![chunk_id, identity, profile.id],
            )?;
            let row_id = transaction.last_insert_rowid();
            transaction.execute(
                &format!(
                    "INSERT INTO {table}(rowid, embedding, profile_id, snapshot_id)
                     VALUES(?1, ?2, ?3, ?4)"
                ),
                params![row_id, tied_vector, profile.id, snapshot_id],
            )?;
        }
        transaction.commit()?;

        let ranking = adaptive_vector_search(
            store.connection(),
            &table,
            profile.id,
            snapshot_id,
            &vec_to_blob(&[1.0, 0.0]),
            occurrence_count,
            1,
        )?;
        assert_eq!(ranking, vec![(expected_first, 1.0)]);
        Ok(())
    }

    #[test]
    fn materialization_publishes_readiness_and_lexical_fallback_searches() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("README.md"),
            "# Vector guide\n\nUse the current vector configuration.\n",
        )?;
        let corpus =
            crate::docs::corpus::scan(root.path(), &crate::docs::corpus::CorpusOptions::default())?;
        let mut store = DocsStore::open(
            root.path(),
            Some(std::path::Path::new("docs.sqlite")),
            Some(std::path::Path::new("code.sqlite")),
        )?;
        let publication = store.publish(&corpus)?;
        let spec = ProfileSpec {
            provider: "test".into(),
            model: "test-2d".into(),
            fingerprint: "11".repeat(32),
            config_json: "{}".into(),
            dimensions: Some(2),
        };
        let profile = ensure_profile(store.connection(), &spec, 2)?;
        let identity: Vec<u8> = store.connection().query_row(
            "SELECT embedding_identity FROM doc_chunks WHERE embedding_identity IS NOT NULL LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        store.connection().execute(
            "INSERT INTO doc_embeddings(embedding_identity, profile_id, vec)
             VALUES(?1, ?2, ?3)",
            params![identity, profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        let materialized =
            materialize_current_generation(&mut store, publication.snapshot_id, &profile)?;
        assert_eq!(materialized, 1);
        assert!(generation_is_ready(
            store.connection(),
            publication.snapshot_id,
            &profile
        )?);
        let nearest: i64 = store.connection().query_row(
            &format!(
                "SELECT e.chunk_id
                 FROM {} v
                 JOIN doc_embedding_index_entries e ON e.id=v.rowid
                 WHERE v.embedding MATCH ?1
                   AND v.k=1
                   AND v.profile_id=?2
                   AND v.snapshot_id=?3",
                vector_table(2)
            ),
            params![
                vec_to_blob(&[1.0, 0.0]),
                profile.id,
                publication.snapshot_id
            ],
            |row| row.get(0),
        )?;
        assert!(nearest > 0);

        let replacement = store.publish(&corpus)?;
        assert_eq!(
            materialize_current_generation(&mut store, replacement.snapshot_id, &profile)?,
            1
        );
        let stale = materialize_current_generation(&mut store, publication.snapshot_id, &profile)
            .unwrap_err();
        assert!(stale.to_string().contains("snapshot advanced"));
        assert!(generation_is_ready(
            store.connection(),
            replacement.snapshot_id,
            &profile
        )?);

        let response = search(
            &store,
            None,
            "current vector",
            &SearchOptions {
                vector: false,
                rerank: false,
                freshness: false,
                response_bytes: 100_000,
                ..SearchOptions::default()
            },
        )?;
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].document.path, "README.md");
        assert_eq!(response.diagnostics.vector_status, VectorStatus::Disabled);
        Ok(())
    }
}
