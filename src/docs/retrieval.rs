use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::json;

use super::CHUNK_FORMAT_VERSION;
use super::corpus::{CapturedFile, capture_file};
use super::freshness::{FreshnessBasis, FreshnessValue};
use super::store::{self, SearchHit};
use crate::embed::{
    ProfileSpec, Provider, ResolvedProfile, validate_response_profile, vec_to_blob,
};
use crate::publication::{Identities, Plane, ResponseIdentity};
use crate::search::Reranker;

const RRF_K: f64 = 60.0;
const MAX_VECTOR_DIMENSIONS: usize = 8_192;
const SQLITE_VEC_MAX_K: usize = 4_096;
const MAX_CAPTURE_BYTES: u64 = 4_194_304;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EmbedReport {
    pub snapshot: String,
    pub publication_snapshot: String,
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
    #[serde(skip_serializing)]
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
    Debug,
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessStatus {
    Disabled,
    Active,
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
    pub freshness_status: FreshnessStatus,
    pub max_rank_movement: usize,
    pub freshness_changed_candidates: usize,
    pub total_candidates: usize,
    pub budget_dropped: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RetrievalHit {
    #[serde(flatten)]
    pub document: SearchHit,
    pub rank: usize,
    pub lexical_score: Option<f64>,
    pub vector_score: Option<f64>,
    pub freshness_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness_secondary_value: Option<String>,
    pub base_rank: usize,
    /// `base_rank - rank`; positive values are promotions.
    pub movement: i64,
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
    pub snapshot: String,
    pub publication_snapshot: String,
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
                "description": hit.document.description,
                "tags": hit.document.tags,
                "heading": hit.document.breadcrumb,
                "lines": [hit.document.start_line, hit.document.end_line],
                "content": hit.content,
                "freshness_basis": hit.document.freshness_basis,
                "freshness_value": hit.freshness_value,
                "freshness_secondary_value": hit.freshness_secondary_value,
                "source_state": hit.source_state,
                "source_detail": hit.source_detail,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string(&json!({
        "snapshot": result.snapshot,
        "publication_snapshot": result.publication_snapshot,
        "hits": hits,
        "truncated": result.truncated,
        "retrieval": {
            "vector": result.diagnostics.vector_status,
            "reranker": result.diagnostics.reranker_status,
            "freshness": result.diagnostics.freshness_status,
            "max_rank_movement": result.diagnostics.max_rank_movement,
        },
    }))?)
}

pub fn human_search_string(result: &SearchResponse) -> String {
    let mut output = format!(
        "documentation snapshot {} publication_snapshot={}: {} hits (vector={:?}, reranker={:?}, freshness={:?}, max_rank_movement={}, truncated={}, budget_dropped={})\n",
        result.snapshot,
        result.publication_snapshot,
        result.hits.len(),
        result.diagnostics.vector_status,
        result.diagnostics.reranker_status,
        result.diagnostics.freshness_status,
        result.diagnostics.max_rank_movement,
        result.truncated,
        result.diagnostics.budget_dropped,
    );
    for hit in &result.hits {
        let tags = serde_json::to_string(&hit.document.tags)
            .expect("documentation string tags always serialize as JSON");
        let _ = writeln!(
            output,
            "\n{}. {}:{}-{}  {}\ntitle={} description={} tags={}\nfreshness={} changed={} base_rank={} movement={}{}\nsource_state={}{}\n{}",
            hit.rank,
            hit.document.path,
            hit.document.start_line,
            hit.document.end_line,
            hit.document.breadcrumb,
            hit.document.title,
            hit.document.description.as_deref().unwrap_or("-"),
            tags,
            hit.document.freshness_basis,
            hit.freshness_value.as_deref().unwrap_or("unknown"),
            hit.base_rank,
            hit.movement,
            hit.freshness_secondary_value
                .as_deref()
                .map_or_else(String::new, |value| format!(" previous={value}")),
            hit.source_state.as_str(),
            hit.source_detail
                .as_deref()
                .map_or_else(String::new, |detail| format!(" source_detail={detail}")),
            hit.content,
        );
    }
    output
}

fn reranker_document(hit: &SearchHit) -> String {
    let tags = serde_json::to_string(&hit.tags)
        .expect("documentation string tags always serialize as JSON");
    format!(
        "path: {}\ntitle: {}\ndescription: {}\ntags: {}\nbreadcrumb: {}\n\n{}",
        hit.path,
        hit.title,
        hit.description.as_deref().unwrap_or_default(),
        tags,
        hit.breadcrumb,
        hit.rendered_body,
    )
}

fn freshness_basis_for_hit(hit: &SearchHit) -> FreshnessBasis {
    let basis = match hit.freshness_basis.as_str() {
        "git" => hit
            .freshness_author_time
            .map_or(FreshnessBasis::Unknown, |author_time| FreshnessBasis::Git {
                author_time,
            }),
        "working_tree" => FreshnessBasis::WorkingTree {
            latest_committed_author_time: hit.freshness_author_time,
        },
        // Observation history is deliberately deferred. Keeping the wire name
        // reserved does not make rows without its clock comparable.
        "observed" | "unknown" => FreshnessBasis::Unknown,
        _ => FreshnessBasis::Unknown,
    };
    debug_assert!(
        basis.wire_name() == hit.freshness_basis || matches!(basis, FreshnessBasis::Unknown)
    );
    basis
}

fn freshness_values(hit: &SearchHit) -> (Option<String>, Option<String>) {
    let basis = freshness_basis_for_hit(hit);
    match (basis.value(), basis) {
        (Some(FreshnessValue::GitAuthorTime(author_time)), _) => {
            (Some(format_unix_seconds(author_time)), None)
        }
        (
            Some(FreshnessValue::Uncommitted),
            FreshnessBasis::WorkingTree {
                latest_committed_author_time,
            },
        ) => (
            Some("uncommitted".to_owned()),
            latest_committed_author_time.map(format_unix_seconds),
        ),
        (Some(FreshnessValue::Observed { .. }) | None, _) => (None, None),
        (Some(FreshnessValue::Uncommitted), _) => (None, None),
    }
}

fn format_unix_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Gregorian civil date conversion for days since 1970-01-01. This is kept
// local so the wire format does not depend on process locale or Git's display
// settings.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[derive(Debug, Clone)]
struct EmbeddingDocument {
    identity: String,
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

/// Embed missing documentation representations into the shared durable cache,
/// then atomically materialize current docs occurrences and readiness after
/// rechecking the documentation digest.
pub fn embed_current(conn: &Connection, provider: &Provider, batch: usize) -> Result<EmbedReport> {
    ensure!(
        batch > 0,
        "documentation embedding batch size must be positive"
    );
    crate::store::validate_published_contracts(conn)?;
    let snapshot = Identities::read(conn)?
        .response(Plane::Documentation)
        .snapshot;
    let profile_spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
    let profile_fingerprint = profile_spec.fingerprint.clone();
    let documents = embedding_documents(conn)?;
    let embeddable_occurrences = documents.iter().map(|row| row.occurrences).sum();

    let mut profile = existing_profile(conn, &profile_spec)?;
    let missing = missing_documents(conn, profile.as_ref().map(|value| value.id), &documents)?;
    let missing_before = missing.len();
    let cached_reused = documents.len().saturating_sub(missing_before);
    let mut embedded = 0usize;

    // Stay beneath the local service's request limits even when CLI config is
    // larger; completed batches remain useful content-addressed cache rows.
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
        let resolved = ensure_profile(conn, &profile_spec, dimensions)?;
        if let Some(previous) = profile.as_ref() {
            ensure!(
                previous.id == resolved.id && previous.dimensions == resolved.dimensions,
                "embedding profile changed during one documentation embed operation"
            );
        }
        profile = Some(resolved.clone());

        conn.execute_batch("SAVEPOINT jscout_docs_embedding_batch")?;
        let stored = (|| -> Result<()> {
            let expected_bytes = resolved
                .dimensions
                .checked_mul(std::mem::size_of::<f32>())
                .context("documentation embedding dimensions overflow")?;
            for (row, vector) in rows.iter().zip(&response.vectors) {
                let blob = vec_to_blob(vector);
                ensure!(
                    blob.len() == expected_bytes,
                    "documentation embedding vector has incompatible dimensions"
                );
                conn.execute(
                    "INSERT INTO embeddings(chunk_hash,profile_id,vec)
                     VALUES(?1,?2,?3)
                     ON CONFLICT(chunk_hash,profile_id) DO NOTHING",
                    params![row.identity, resolved.id, blob],
                )?;
            }
            Ok(())
        })();
        match stored {
            Ok(()) => conn.execute_batch("RELEASE jscout_docs_embedding_batch")?,
            Err(error) => {
                let _ = conn.execute_batch(
                    "ROLLBACK TO jscout_docs_embedding_batch; RELEASE jscout_docs_embedding_batch",
                );
                return Err(error);
            }
        }
        embedded += rows.len();
    }

    let (occurrences_materialized, generation_published, identity) = match profile.as_ref() {
        Some(profile) if !documents.is_empty() => {
            let (count, identity) = materialize_current_generation(conn, &snapshot, profile)?;
            (count, true, identity)
        }
        _ => {
            let identity = Identities::read(conn)?.response(Plane::Documentation);
            ensure!(
                identity.snapshot == snapshot,
                "documentation digest advanced while embeddings were being prepared; rerun `jscout docs embed`"
            );
            (0, false, identity)
        }
    };

    Ok(EmbedReport {
        snapshot: identity.snapshot,
        publication_snapshot: identity.publication_snapshot,
        profile_id: profile.as_ref().map(|value| value.id),
        profile_fingerprint,
        dimensions: profile.as_ref().map(|value| value.dimensions),
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
    conn: &Connection,
    root: &Path,
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
    ensure!(
        (1..=3).contains(&options.max_rank_movement),
        "documentation max rank movement must be between 1 and 3"
    );
    if options.vector_required && !options.vector {
        bail!("vector participation cannot be both required and disabled");
    }

    crate::store::with_read_snapshot(conn, "jscout_docs_search", || {
        search_inner(conn, root, provider, query, options)
    })
}

fn search_inner(
    conn: &Connection,
    root: &Path,
    provider: Option<&Provider>,
    query: &str,
    options: &SearchOptions,
) -> Result<SearchResponse> {
    crate::store::validate_published_contracts(conn)?;
    let identity = Identities::read(conn)?.response(Plane::Documentation);
    let snapshot = identity.snapshot.clone();
    if options.freshness {
        let (provenance_enabled, provenance_format): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT
                   (SELECT value FROM meta WHERE key=?1),
                   (SELECT value FROM meta
                    WHERE key='documentation_provenance_format_version')",
                [super::PROVENANCE_ENABLED_META_KEY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        if provenance_enabled.as_deref() != Some("true") {
            bail!(
                "documentation freshness is enabled, but provenance is not indexed; run `jscout index`"
            );
        }
        let provenance_format = provenance_format.as_deref().unwrap_or("missing");
        if provenance_format != super::PROVENANCE_FORMAT_VERSION {
            bail!(
                "documentation freshness provenance uses format {provenance_format}, but this jscout requires {}; run `jscout index`",
                super::PROVENANCE_FORMAT_VERSION,
            );
        }
    }
    let reranker_pool = options
        .reranker
        .as_ref()
        .map_or(0, Reranker::candidate_limit);
    let candidate_limit = options
        .limit
        .saturating_mul(8)
        .max(reranker_pool)
        .max(options.limit.saturating_add(if options.freshness {
            options.max_rank_movement
        } else {
            0
        }))
        .max(options.limit);
    let lexical_hits = store::lexical_search(conn, query, candidate_limit)?;
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
        if let Some(provider) = provider {
            match resolve_vector_ranking(conn, provider, &snapshot, query, candidate_limit) {
                Ok(VectorResolution::Ready { profile, ranking }) => {
                    vector_status = VectorStatus::Active;
                    vector_profile_id = Some(profile.id);
                    vector_dimensions = Some(profile.dimensions);
                    vector_ranking = ranking;
                }
                Ok(VectorResolution::NotReady { profile, detail }) => {
                    vector_status = VectorStatus::NotReady;
                    vector_profile_id = profile.as_ref().map(|value| value.id);
                    vector_dimensions = profile.as_ref().map(|value| value.dimensions);
                    vector_detail = Some(detail);
                    if options.vector_required {
                        bail!(
                            "vector participation was required, but the current documentation vector generation is not ready"
                        );
                    }
                }
                Err(error) if !options.vector_required => {
                    vector_status = VectorStatus::Degraded;
                    vector_detail = Some(error.to_string());
                }
                Err(error) => {
                    return Err(error).context("required documentation vector retrieval failed");
                }
            }
        } else if options.vector_required {
            bail!("vector participation was required, but no embedding provider is configured");
        }
    }

    for &(chunk_id, _) in &vector_ranking {
        if let std::collections::hash_map::Entry::Vacant(entry) = hits_by_id.entry(chunk_id) {
            entry.insert(store::load_hit(conn, chunk_id, 0.0)?);
        }
    }

    finish_search(
        root,
        query,
        options,
        identity,
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

enum VectorResolution {
    Ready {
        profile: ResolvedProfile,
        ranking: Vec<(i64, f64)>,
    },
    NotReady {
        profile: Option<ResolvedProfile>,
        detail: String,
    },
}

fn resolve_vector_ranking(
    conn: &Connection,
    provider: &Provider,
    snapshot: &str,
    query: &str,
    limit: usize,
) -> Result<VectorResolution> {
    let profile_spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
    let Some(profile) = existing_profile(conn, &profile_spec)? else {
        return Ok(VectorResolution::NotReady {
            profile: None,
            detail: "the configured profile has no documentation embeddings".into(),
        });
    };
    if !generation_is_ready(conn, snapshot, &profile)? {
        return Ok(VectorResolution::NotReady {
            profile: Some(profile),
            detail: "the current documentation digest has no complete vector generation".into(),
        });
    }
    let ranking = vector_search(conn, provider, &profile_spec, &profile, query, limit)?;
    Ok(VectorResolution::Ready { profile, ranking })
}

fn finish_search(
    root: &Path,
    query: &str,
    options: &SearchOptions,
    identity: ResponseIdentity,
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
    let mut ranked = reciprocal_rank_fusion(&rankings, &source_keys);
    let fused_candidates = ranked.len();

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
            .map(|(chunk_id, _)| {
                let hit = &hits_by_id[chunk_id];
                let mut document = reranker_document(hit);
                reranker.truncate_document(&mut document);
                (*chunk_id, document)
            })
            .collect::<Vec<_>>();
        reranker_candidates = documents.len();
        match reranker.rerank(query, &documents) {
            Ok(order) => {
                ranked = crate::search::merge_reranked_prefix(&ranked, order);
                reranker_status = RerankerStatus::Active;
            }
            Err(error) => {
                reranker_status = RerankerStatus::Degraded;
                reranker_detail = Some(error.to_string());
            }
        }
    }

    let freshness_status = if options.freshness {
        FreshnessStatus::Active
    } else {
        FreshnessStatus::Disabled
    };
    let movement = super::freshness::reorder(
        &mut ranked,
        if options.freshness {
            options.max_rank_movement
        } else {
            0
        },
        |(chunk_id, _)| freshness_basis_for_hit(&hits_by_id[chunk_id]),
    );
    let freshness_changed_candidates = movement.iter().filter(|rank| rank.movement != 0).count();
    let total_candidates = ranked.len();
    let mut hits = ranked
        .into_iter()
        .zip(movement)
        .take(options.limit)
        .enumerate()
        .map(|(position, ((chunk_id, score), rank_movement))| {
            let mut document = hits_by_id
                .remove(&chunk_id)
                .expect("ranked documentation hit was loaded");
            document.score = score;
            let (freshness_value, freshness_secondary_value) = freshness_values(&document);
            RetrievalHit {
                content: document.rendered_body.clone(),
                source_state: SourceState::SourceMismatch,
                source_detail: Some("not_resolved".to_owned()),
                document,
                rank: position + 1,
                lexical_score: lexical_scores.get(&chunk_id).copied(),
                vector_score: vector_scores.get(&chunk_id).copied(),
                freshness_value,
                freshness_secondary_value,
                base_rank: rank_movement.base_rank,
                movement: rank_movement.movement,
            }
        })
        .collect::<Vec<_>>();
    resolve_hit_sources(root, &mut hits);

    let diagnostics = RetrievalDiagnostics {
        lexical_candidates: lexical_scores.len(),
        vector_status,
        vector_candidates: vector_scores.len(),
        vector_profile_id,
        vector_dimensions,
        vector_detail,
        rrf_k: RRF_K,
        fused_candidates,
        reranker_status,
        reranker_candidates,
        reranker_detail,
        freshness_status,
        max_rank_movement: options.max_rank_movement,
        freshness_changed_candidates,
        total_candidates,
        budget_dropped: 0,
    };
    apply_response_budget(
        SearchResponse {
            snapshot: identity.snapshot,
            publication_snapshot: identity.publication_snapshot,
            hits,
            diagnostics,
            truncated: false,
        },
        options.response_bytes,
        options.output,
    )
}

fn embedding_documents(conn: &Connection) -> Result<Vec<EmbeddingDocument>> {
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationVector);
    let invalid: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM chunks c
         JOIN files f ON f.id=c.file_id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
           AND c.kind!='markdown_document'
           AND m.embedding_identity IS NULL",
        [&eligible_formats],
        |row| row.get(0),
    )?;
    ensure!(
        invalid == 0,
        "current documentation projection has body chunks without embedding identities"
    );
    let mut statement = conn.prepare(
        "SELECT m.embedding_identity,
                MIN(CASE
                      WHEN m.nearest_heading IS NULL THEN docs_fts.body
                      ELSE m.nearest_heading || '\n\n' || docs_fts.body
                    END),
                COUNT(*),
                COUNT(DISTINCT CASE
                      WHEN m.nearest_heading IS NULL THEN docs_fts.body
                      ELSE m.nearest_heading || '\n\n' || docs_fts.body
                    END)
         FROM doc_chunk_meta m
         JOIN docs_fts ON docs_fts.rowid=m.chunk_id
         JOIN chunks c ON c.id=m.chunk_id
         JOIN files f ON f.id=c.file_id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
           AND m.embedding_identity IS NOT NULL
         GROUP BY m.embedding_identity
         ORDER BY m.embedding_identity",
    )?;
    let rows = statement.query_map([&eligible_formats], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (identity, text, occurrences, distinct_texts) = row?;
        ensure!(
            distinct_texts == 1,
            "documentation embedding identity `{identity}` maps to multiple provider texts"
        );
        Ok(EmbeddingDocument {
            identity,
            text,
            occurrences: usize::try_from(occurrences)
                .context("documentation occurrence count is negative")?,
        })
    })
    .collect()
}

fn missing_documents(
    conn: &Connection,
    profile_id: Option<i64>,
    documents: &[EmbeddingDocument],
) -> Result<Vec<EmbeddingDocument>> {
    let cached = if let Some(profile_id) = profile_id {
        let mut statement = conn
            .prepare("SELECT chunk_hash FROM embeddings WHERE profile_id=?1 ORDER BY chunk_hash")?;
        let rows = statement.query_map([profile_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?
    } else {
        std::collections::HashSet::new()
    };
    Ok(documents
        .iter()
        .filter(|row| !cached.contains(&row.identity))
        .cloned()
        .collect())
}

fn existing_profile(conn: &Connection, spec: &ProfileSpec) -> Result<Option<ResolvedProfile>> {
    let row = conn
        .query_row(
            "SELECT id,provider,model,dimensions,config_json
             FROM embedding_profiles WHERE config_fingerprint=?1",
            [&spec.fingerprint],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, provider, model, dimensions, config_json)) = row else {
        return Ok(None);
    };
    ensure!(
        provider == spec.provider && model == spec.model && config_json == spec.config_json,
        "documentation embedding profile fingerprint collision"
    );
    let dimensions =
        usize::try_from(dimensions).context("documentation embedding dimensions are negative")?;
    validate_dimensions(dimensions)?;
    if let Some(expected) = spec.dimensions {
        ensure!(
            dimensions == expected,
            "configured documentation embedding dimensions changed"
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
            dimensions == expected,
            "documentation embedding response dimensions do not match the configured profile"
        );
    }
    if let Some(existing) = existing_profile(conn, spec)? {
        ensure!(
            existing.dimensions == dimensions,
            "documentation embedding response dimensions changed for an existing profile"
        );
        return Ok(existing);
    }
    conn.execute(
        "INSERT INTO embedding_profiles(provider,model,config_fingerprint,dimensions,config_json)
         VALUES(?1,?2,?3,?4,?5)",
        params![
            spec.provider,
            spec.model,
            spec.fingerprint,
            i64::try_from(dimensions).context("embedding dimensions do not fit SQLite")?,
            spec.config_json
        ],
    )?;
    existing_profile(conn, spec)?.context("documentation embedding profile was not stored")
}

fn materialize_current_generation(
    conn: &Connection,
    snapshot: &str,
    profile: &ResolvedProfile,
) -> Result<(usize, ResponseIdentity)> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(usize, ResponseIdentity)> {
        crate::store::validate_published_contracts(conn)?;
        let identity = Identities::read(conn)?.response(Plane::Documentation);
        ensure!(
            identity.snapshot == snapshot,
            "documentation digest advanced while embeddings were being prepared; cached batches were kept, rerun `jscout docs embed`"
        );
        let count = rebuild_profile_generation_from_cache(conn, snapshot, profile)?.context(
            "not every current documentation representation has a cached valid-dimension vector",
        )?;
        Ok((count, identity))
    })();
    match result {
        Ok(count) => {
            conn.execute_batch("COMMIT")?;
            Ok(count)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Rebuild current-documentation-digest vector occurrences from the shared
/// durable embedding cache. This is deliberately provider-free and nests under
/// the indexer's outer publication transaction.
///
/// A profile is marked ready only when every current embeddable documentation
/// occurrence has a cached vector of the profile's declared dimensions.
/// Incomplete caches are an ordinary not-ready state: stale occurrences and
/// generation markers are removed, while indexing continues successfully.
pub(crate) fn cached_generation_rematerialization_needed(
    conn: &Connection,
    documentation_digest: &str,
) -> Result<bool> {
    for profile in documentation_profiles(conn)? {
        if !generation_is_ready(conn, documentation_digest, &profile)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn rematerialize_cached_generations(conn: &Connection, snapshot: &str) -> Result<()> {
    conn.execute_batch("SAVEPOINT jscout_docs_cached_rematerialize")?;
    let result = (|| -> Result<()> {
        ensure!(
            store::current_snapshot(conn)? == snapshot,
            "documentation cached rematerialization requires the newly published documentation digest"
        );
        clear_all_materialized_generations(conn)?;
        for profile in documentation_profiles(conn)? {
            let _ = rebuild_profile_generation_from_cache(conn, snapshot, &profile)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE jscout_docs_cached_rematerialize")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO jscout_docs_cached_rematerialize; RELEASE jscout_docs_cached_rematerialize",
            );
            return Err(error);
        }
    }
    Ok(())
}

fn documentation_profiles(conn: &Connection) -> Result<Vec<ResolvedProfile>> {
    let mut statement =
        conn.prepare("SELECT id,dimensions,config_json FROM embedding_profiles ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut profiles = Vec::new();
    for row in rows {
        let (id, dimensions, config_json) = row?;
        let Ok(configuration) = serde_json::from_str::<serde_json::Value>(&config_json) else {
            continue;
        };
        if configuration["document_text"].as_str() != Some(CHUNK_FORMAT_VERSION) {
            continue;
        }
        let dimensions = usize::try_from(dimensions)
            .context("documentation embedding dimensions are negative")?;
        validate_dimensions(dimensions)?;
        profiles.push(ResolvedProfile { id, dimensions });
    }
    Ok(profiles)
}

fn clear_all_materialized_generations(conn: &Connection) -> Result<()> {
    let tables = {
        let mut statement = conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table'
               AND name GLOB 'vec_doc_embeddings_[0-9]*'
               AND substr(name, length('vec_doc_embeddings_') + 1)
                   NOT GLOB '*[^0-9]*'
             ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for table in tables {
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }
    conn.execute("DELETE FROM doc_vector_generations", [])?;
    conn.execute("DELETE FROM doc_embedding_index_entries", [])?;
    Ok(())
}

fn rebuild_profile_generation_from_cache(
    conn: &Connection,
    snapshot: &str,
    profile: &ResolvedProfile,
) -> Result<Option<usize>> {
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationVector);
    let table = ensure_vector_table(conn, profile.dimensions)?;
    conn.execute(
        "DELETE FROM doc_vector_generations WHERE profile_id=?1",
        [profile.id],
    )?;
    conn.execute(
        &format!("DELETE FROM {table} WHERE profile_id=?1"),
        [profile.id],
    )?;
    conn.execute(
        "DELETE FROM doc_embedding_index_entries WHERE profile_id=?1",
        [profile.id],
    )?;

    let rows = {
        let mut statement = conn.prepare(
            "SELECT m.chunk_id, e.vec
             FROM doc_chunk_meta m
             JOIN chunks c ON c.id=m.chunk_id
             JOIN files f ON f.id=c.file_id
             JOIN embeddings e
               ON e.chunk_hash=m.embedding_identity AND e.profile_id=?1
             WHERE f.corpus='docs'
               AND f.format IN (SELECT value FROM json_each(?2))
               AND m.embedding_identity IS NOT NULL
             ORDER BY m.chunk_id",
        )?;
        let rows = statement.query_map(params![profile.id, &eligible_formats], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let expected: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM doc_chunk_meta m
         JOIN chunks c ON c.id=m.chunk_id
         JOIN files f ON f.id=c.file_id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
           AND m.embedding_identity IS NOT NULL",
        [&eligible_formats],
        |row| row.get(0),
    )?;
    let expected = usize::try_from(expected)
        .context("documentation embedding occurrence count is negative")?;
    let expected_blob_len = profile
        .dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .context("documentation embedding dimensions overflow")?;
    if expected == 0
        || rows.len() != expected
        || rows
            .iter()
            .any(|(_, vector)| vector.len() != expected_blob_len)
    {
        return Ok(None);
    }

    for (chunk_id, vector) in &rows {
        conn.execute(
            "INSERT INTO doc_embedding_index_entries(chunk_id,profile_id)
             VALUES(?1,?2)",
            params![chunk_id, profile.id],
        )?;
        let row_id = conn.last_insert_rowid();
        conn.execute(
            &format!("INSERT INTO {table}(rowid,embedding,profile_id) VALUES(?1,?2,?3)"),
            params![row_id, vector, profile.id],
        )?;
    }
    conn.execute(
        "INSERT INTO doc_vector_generations(
           snapshot,profile_id,dimensions,chunk_format_version
         ) VALUES(?1,?2,?3,?4)",
        params![
            snapshot,
            profile.id,
            i64::try_from(profile.dimensions).context("embedding dimensions do not fit SQLite")?,
            CHUNK_FORMAT_VERSION
        ],
    )?;
    Ok(Some(rows.len()))
}

fn ensure_vector_table(conn: &Connection, dimensions: usize) -> Result<String> {
    validate_dimensions(dimensions)?;
    let table = vector_table(dimensions);
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(
           embedding FLOAT[{dimensions}] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY
         );"
    ))?;
    Ok(table)
}

fn generation_is_ready(
    conn: &Connection,
    snapshot: &str,
    profile: &ResolvedProfile,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM doc_vector_generations
           WHERE snapshot=?1 AND profile_id=?2 AND dimensions=?3
             AND chunk_format_version=?4
         )",
        params![
            snapshot,
            profile.id,
            i64::try_from(profile.dimensions).context("embedding dimensions do not fit SQLite")?,
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
    query: &str,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationVector);
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
    let query_blob = vec_to_blob(vector);

    // The materialized documentation table must contain only eligible
    // occurrences. Count it unfiltered so an obsolete/ineligible entry makes
    // readiness fail closed instead of being hidden by the registry filter.
    let occurrence_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1",
        [profile.id],
        |row| row.get(0),
    )?;
    let expected: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM doc_chunk_meta m
         JOIN chunks c ON c.id=m.chunk_id
         JOIN files f ON f.id=c.file_id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
           AND m.embedding_identity IS NOT NULL",
        [&eligible_formats],
        |row| row.get(0),
    )?;
    ensure!(
        occurrence_count == expected,
        "documentation vector generation is incomplete"
    );
    if occurrence_count == 0 {
        return Ok(Vec::new());
    }
    let occurrence_count = usize::try_from(occurrence_count)
        .context("documentation vector occurrence count is negative")?;
    let candidates = if occurrence_count <= SQLITE_VEC_MAX_K {
        knn_vector_candidates(
            conn,
            &table,
            profile.id,
            &query_blob,
            occurrence_count,
            &eligible_formats,
        )?
    } else {
        full_distance_vector_search(
            conn,
            profile.id,
            &query_blob,
            occurrence_count,
            &eligible_formats,
        )?
    };
    Ok(finalize_vector_ranking(candidates, limit))
}

fn knn_vector_candidates(
    conn: &Connection,
    table: &str,
    profile_id: i64,
    query_vector: &[u8],
    k: usize,
    eligible_formats: &str,
) -> Result<Vec<VectorCandidate>> {
    let mut statement = conn.prepare(&format!(
        "SELECT e.chunk_id, v.distance, f.path, c.start, c.end, docs_fts.body
         FROM {table} v
         JOIN doc_embedding_index_entries e ON e.id=v.rowid
         JOIN chunks c ON c.id=e.chunk_id
         JOIN files f ON f.id=c.file_id
         JOIN docs_fts ON docs_fts.rowid=c.id
         WHERE v.embedding MATCH ?1 AND v.k=?2 AND v.profile_id=?3
           AND f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?4))
         ORDER BY v.distance"
    ))?;
    let rows = statement.query_map(
        params![
            query_vector,
            i64::try_from(k).context("documentation vector K does not fit SQLite")?,
            profile_id,
            eligible_formats,
        ],
        vector_candidate_from_row,
    )?;
    let candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        candidates.len() == k,
        "documentation vector generation is incomplete: expected {k} candidates, found {}",
        candidates.len()
    );
    Ok(candidates)
}

fn full_distance_vector_search(
    conn: &Connection,
    profile_id: i64,
    query_vector: &[u8],
    expected: usize,
    eligible_formats: &str,
) -> Result<Vec<VectorCandidate>> {
    let mut statement = conn.prepare(
        "SELECT entry.chunk_id, vec_distance_cosine(e.vec,?1), f.path,
                c.start, c.end, docs_fts.body
         FROM doc_embedding_index_entries entry
         JOIN doc_chunk_meta m ON m.chunk_id=entry.chunk_id
         JOIN embeddings e
           ON e.chunk_hash=m.embedding_identity AND e.profile_id=entry.profile_id
         JOIN chunks c ON c.id=entry.chunk_id
         JOIN files f ON f.id=c.file_id
         JOIN docs_fts ON docs_fts.rowid=c.id
         WHERE entry.profile_id=?2
           AND f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?3))",
    )?;
    let rows = statement.query_map(
        params![query_vector, profile_id, eligible_formats],
        vector_candidate_from_row,
    )?;
    let candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        candidates.len() == expected,
        "documentation vector cache is incomplete: expected {expected} candidates, found {}",
        candidates.len()
    );
    Ok(candidates)
}

fn vector_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VectorCandidate> {
    let source_start = row.get::<_, i64>(3)?;
    let source_end = row.get::<_, i64>(4)?;
    let body = row.get::<_, String>(5)?;
    Ok(VectorCandidate {
        chunk_id: row.get(0)?,
        distance: row.get(1)?,
        source_key: SourceKey {
            path: row.get(2)?,
            source_start: u64::try_from(source_start).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
            source_end: u64::try_from(source_end).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
            body_hash: *blake3::hash(body.as_bytes()).as_bytes(),
        },
    })
}

fn finalize_vector_ranking(mut candidates: Vec<VectorCandidate>, limit: usize) -> Vec<(i64, f64)> {
    candidates.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.source_key.cmp(&right.source_key))
    });
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|candidate| (candidate.chunk_id, 1.0 - candidate.distance))
        .collect()
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
            .or_insert_with(|| capture_source(root, &hit.document.path, &hit.document.file_hash));
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
                    Err(_) => hit.source_detail = Some("invalid_utf8_slice".to_owned()),
                }
            }
            CapturedSource::Mismatch(detail) => hit.source_detail = Some(detail.clone()),
        }
    }
}

fn capture_source(root: &Path, relative: &str, expected_hash: &str) -> CapturedSource {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return CapturedSource::Mismatch("invalid_indexed_path".to_owned());
    }
    match capture_file(&root.join(path), MAX_CAPTURE_BYTES) {
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

fn apply_response_budget(
    mut response: SearchResponse,
    byte_limit: usize,
    output: SearchOutput,
) -> Result<SearchResponse> {
    let rendered_len = |response: &SearchResponse| -> Result<usize> {
        match output {
            SearchOutput::Compact => Ok(compact_search_string(response)?.len()),
            SearchOutput::Debug => Ok(serde_json::to_string(response)?.len()),
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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    use anyhow::{Context, Result};
    use rusqlite::params;
    use serde_json::Value;

    use super::*;

    fn hit(id: i64, path: &str) -> SearchHit {
        SearchHit {
            chunk_id: id,
            path: path.into(),
            title: path.into(),
            description: None,
            tags: Vec::new(),
            breadcrumb: String::new(),
            nearest_heading: None,
            rendered_body: "body".into(),
            source_start: 0,
            source_end: 4,
            start_line: 1,
            end_line: 1,
            file_hash: blake3::hash(b"body").to_hex().to_string(),
            embedding_identity: Some(format!("id-{id}")),
            freshness_basis: "unknown".into(),
            freshness_author_time: None,
            freshness_committer_time: None,
            freshness_detail: None,
            score: 0.0,
            stub: false,
        }
    }

    fn retrieval_hit(id: i64, path: &str, content: &str) -> RetrievalHit {
        let mut document = hit(id, path);
        document.rendered_body = content.to_owned();
        document.source_end = content.len() as u64;
        document.end_line = 1;
        RetrievalHit {
            document,
            rank: id as usize,
            lexical_score: Some(1.0 / id as f64),
            vector_score: None,
            freshness_value: None,
            freshness_secondary_value: None,
            base_rank: id as usize,
            movement: 0,
            content: content.to_owned(),
            source_state: SourceState::SourceMismatch,
            source_detail: Some("hash_mismatch".into()),
        }
    }

    fn diagnostics() -> RetrievalDiagnostics {
        RetrievalDiagnostics {
            lexical_candidates: 2,
            vector_status: VectorStatus::NotReady,
            vector_candidates: 0,
            vector_profile_id: Some(1),
            vector_dimensions: Some(2),
            vector_detail: Some("current generation incomplete".into()),
            rrf_k: RRF_K,
            fused_candidates: 2,
            reranker_status: RerankerStatus::Disabled,
            reranker_candidates: 0,
            reranker_detail: None,
            freshness_status: FreshnessStatus::Disabled,
            max_rank_movement: 2,
            freshness_changed_candidates: 0,
            total_candidates: 2,
            budget_dropped: 0,
        }
    }

    fn response() -> SearchResponse {
        SearchResponse {
            snapshot: "shared-snapshot".into(),
            publication_snapshot: "publication-snapshot".into(),
            hits: vec![
                retrieval_hit(1, "a.md", &"first ".repeat(80)),
                retrieval_hit(2, "b.md", &"second ".repeat(80)),
            ],
            diagnostics: diagnostics(),
            truncated: false,
        }
    }

    fn rendered_response_len(response: &SearchResponse, output: SearchOutput) -> Result<usize> {
        match output {
            SearchOutput::Compact => Ok(compact_search_string(response)?.len()),
            SearchOutput::Debug => Ok(serde_json::to_string(response)?.len()),
            SearchOutput::Human => Ok(human_search_string(response).len()),
        }
    }

    fn test_provider(endpoint: String) -> Result<Provider> {
        Provider::from_settings(
            &crate::config::EmbeddingSettings {
                provider: Some("openai".into()),
                model: Some("fake-docs-2d".into()),
                revision: None,
                url: Some(endpoint),
                api_key_env: Some("JSCOUT_TEST_UNUSED_EMBED_KEY".into()),
                query_prefix: Some(String::new()),
                batch: 16,
                origins: crate::origin::defaults(),
            },
            &crate::config::InferenceSettings {
                url: "http://127.0.0.1:1".into(),
                host: "127.0.0.1".into(),
                port: 1,
                project: None,
                uv: "uv".into(),
                allow_remote: false,
                batch_size: 16,
                max_length: 4096,
                model_cache_root: None,
            },
        )?
        .context("test embedding provider was not configured")
    }

    fn spawn_openai_embedding_server(
        request_count: usize,
    ) -> Result<(
        String,
        std::thread::JoinHandle<Result<Vec<serde_json::Value>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || {
            let mut requests = Vec::with_capacity(request_count);
            for _ in 0..request_count {
                let (stream, _) = listener.accept()?;
                requests.push(serve_openai_embedding_request(stream)?);
            }
            Ok(requests)
        });
        Ok((format!("http://{address}/v1/embeddings"), server))
    }

    fn serve_openai_embedding_request(stream: TcpStream) -> Result<Value> {
        let mut reader = embedding_request_reader(stream)?;
        let request = read_openai_embedding_request(&mut reader)?;
        let input_count = request["input"]
            .as_array()
            .context("OpenAI embedding request input is not an array")?
            .len();
        let response = serde_json::json!({
            "object": "list",
            "model": "fake-docs-2d",
            "data": (0..input_count)
                .map(|index| serde_json::json!({
                    "object": "embedding",
                    "index": index,
                    "embedding": [1.0, 0.0],
                }))
                .collect::<Vec<_>>(),
        })
        .to_string();
        write!(
            reader.get_mut(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )?;
        reader.get_mut().flush()?;
        Ok(request)
    }

    fn embedding_request_reader(stream: TcpStream) -> Result<BufReader<TcpStream>> {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        Ok(BufReader::new(stream))
    }

    fn read_openai_embedding_request(reader: &mut BufReader<TcpStream>) -> Result<Value> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            anyhow::ensure!(bytes > 0, "embedding request ended before its headers");
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
        let content_length = content_length.context("embedding request has no content length")?;
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body)?;
        serde_json::from_slice(&body).map_err(Into::into)
    }

    fn serve_openai_embedding_failure(stream: TcpStream) -> Result<Value> {
        let mut reader = embedding_request_reader(stream)?;
        let request = read_openai_embedding_request(&mut reader)?;
        // A syntactically valid but contract-invalid success response reaches
        // provider validation without triggering transport retries. This
        // models a terminal provider batch failure without adding 30 seconds
        // of retry backoff to the acceptance test.
        let response = serde_json::json!({ "data": "invalid" }).to_string();
        write!(
            reader.get_mut(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )?;
        reader.get_mut().flush()?;
        Ok(request)
    }

    fn spawn_transient_embedding_failure_server() -> Result<(
        String,
        std::thread::JoinHandle<Result<Vec<serde_json::Value>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || {
            let (first, _) = listener.accept()?;
            let first = serve_openai_embedding_request(first)?;
            let (second, _) = listener.accept()?;
            let second = serve_openai_embedding_failure(second)?;
            let (retry, _) = listener.accept()?;
            let retry = serve_openai_embedding_request(retry)?;
            Ok(vec![first, second, retry])
        });
        Ok((format!("http://{address}/v1/embeddings"), server))
    }

    fn indexed_document() -> Result<(tempfile::TempDir, Connection)> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("README.md"),
            "# Deployment\n\nUse the blue release channel.\n",
        )?;
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;
        Ok((root, conn))
    }

    #[test]
    fn mismatched_document_contract_blocks_status_search_and_embed_until_reindex() -> Result<()> {
        let (root, conn) = indexed_document()?;
        conn.execute(
            "UPDATE meta SET value='documentation-v0'
             WHERE key='documentation_chunk_format_version'",
            [],
        )?;

        let status_error = store::status(&conn).unwrap_err();
        assert!(
            status_error
                .to_string()
                .contains("documentation chunk format documentation-v0")
        );
        let options = SearchOptions {
            vector: false,
            rerank: false,
            ..SearchOptions::default()
        };
        let search_error = search(&conn, root.path(), None, "blue", &options).unwrap_err();
        assert!(
            search_error
                .to_string()
                .contains("documentation chunk format documentation-v0")
        );
        let unreachable = test_provider("http://127.0.0.1:1/v1/embeddings".into())?;
        let embed_error = embed_current(&conn, &unreachable, 16).unwrap_err();
        assert!(
            embed_error
                .to_string()
                .contains("documentation chunk format documentation-v0"),
            "mismatched documentation reached the provider instead of failing closed: {embed_error:#}"
        );
        let profiles: i64 =
            conn.query_row("SELECT count(*) FROM embedding_profiles", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            profiles, 0,
            "mismatched docs must not create an embedding profile"
        );

        crate::indexer::index_repo(root.path(), &conn)?;
        assert_eq!(store::status(&conn)?.indexed_file_count, 1);
        assert_eq!(
            search(&conn, root.path(), None, "blue", &options)?
                .hits
                .len(),
            1
        );

        let (endpoint, server) = spawn_openai_embedding_server(1)?;
        let provider = test_provider(endpoint)?;
        let report = embed_current(&conn, &provider, 16)?;
        assert!(report.generation_published);
        let requests = server
            .join()
            .map_err(|_| anyhow::anyhow!("fake embedding server panicked"))??;
        assert_eq!(requests.len(), 1);
        Ok(())
    }

    #[test]
    fn reciprocal_rank_fusion_uses_sixty_and_source_key_ties() {
        let keys = [
            (1, source_key(&hit(1, "b.md"))),
            (2, source_key(&hit(2, "a.md"))),
        ]
        .into_iter()
        .collect();
        let left = vec![(1, 9.0), (2, 8.0)];
        let right = vec![(2, 9.0), (1, 8.0)];
        let fused = reciprocal_rank_fusion(&[&left, &right], &keys);
        assert_eq!(fused[0].0, 2);
        assert_eq!(fused[0].1, 1.0 / 61.0 + 1.0 / 62.0);
    }

    #[test]
    fn exact_score_ties_use_the_complete_normative_source_key() {
        let mut late_start = hit(1, "a.md");
        late_start.source_start = 1;
        late_start.source_end = 2;

        let mut late_end = hit(2, "a.md");
        late_end.source_end = 5;

        let mut first_body = hit(3, "a.md");
        first_body.rendered_body = "alpha".into();
        first_body.source_end = 4;

        let mut second_body = hit(4, "a.md");
        second_body.rendered_body = "beta".into();
        second_body.source_end = 4;

        let keys = [late_start, late_end, first_body, second_body]
            .into_iter()
            .map(|candidate| (candidate.chunk_id, source_key(&candidate)))
            .collect::<HashMap<_, _>>();
        let expected_bodies = if keys[&3] < keys[&4] { [3, 4] } else { [4, 3] };
        let expected = [expected_bodies[0], expected_bodies[1], 2, 1];

        let mut forward = vec![(1, 1.0), (2, 1.0), (3, 1.0), (4, 1.0)];
        let mut reverse = forward.iter().copied().rev().collect::<Vec<_>>();
        stable_score_sort(&mut forward, &keys);
        stable_score_sort(&mut reverse, &keys);

        assert_eq!(
            forward.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(reverse, forward);
    }

    #[test]
    fn freshness_runs_after_relevance_and_can_promote_the_limit_boundary() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("obsolete.md"), "body")?;
        std::fs::write(root.path().join("current.md"), "body")?;
        let mut obsolete = hit(1, "obsolete.md");
        obsolete.freshness_basis = "git".into();
        obsolete.freshness_author_time = Some(10);
        let mut current = hit(2, "current.md");
        current.freshness_basis = "git".into();
        current.freshness_author_time = Some(20);
        let state = || SearchState {
            lexical_ranking: vec![(1, 2.0), (2, 1.0)],
            vector_ranking: Vec::new(),
            hits_by_id: [(1, obsolete.clone()), (2, current.clone())]
                .into_iter()
                .collect(),
            vector_status: VectorStatus::Disabled,
            vector_profile_id: None,
            vector_dimensions: None,
            vector_detail: None,
        };
        let enabled = finish_search(
            root.path(),
            "guidance",
            &SearchOptions {
                limit: 1,
                response_bytes: usize::MAX,
                output: SearchOutput::Debug,
                vector: false,
                rerank: false,
                freshness: true,
                max_rank_movement: 1,
                ..SearchOptions::default()
            },
            ResponseIdentity {
                snapshot: "snapshot".into(),
                publication_snapshot: "publication".into(),
            },
            state(),
        )?;
        assert_eq!(enabled.hits[0].document.path, "current.md");
        assert_eq!(enabled.hits[0].rank, 1);
        assert_eq!(enabled.hits[0].base_rank, 2);
        assert_eq!(enabled.hits[0].movement, 1);
        assert_eq!(
            enabled.diagnostics.freshness_status,
            FreshnessStatus::Active
        );

        let disabled = finish_search(
            root.path(),
            "guidance",
            &SearchOptions {
                limit: 1,
                response_bytes: usize::MAX,
                output: SearchOutput::Debug,
                vector: false,
                rerank: false,
                freshness: false,
                max_rank_movement: 1,
                ..SearchOptions::default()
            },
            ResponseIdentity {
                snapshot: "snapshot".into(),
                publication_snapshot: "publication".into(),
            },
            state(),
        )?;
        assert_eq!(disabled.hits[0].document.path, "obsolete.md");
        assert_eq!(disabled.hits[0].base_rank, 1);
        assert_eq!(disabled.hits[0].movement, 0);
        assert_eq!(
            disabled.diagnostics.freshness_status,
            FreshnessStatus::Disabled
        );
        Ok(())
    }

    #[test]
    fn freshness_timestamp_wire_format_is_utc_and_locale_independent() {
        assert_eq!(format_unix_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_seconds(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_unix_seconds(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn response_budget_accounts_for_each_complete_output_and_reports_its_floor() -> Result<()> {
        for output in [
            SearchOutput::Compact,
            SearchOutput::Debug,
            SearchOutput::Human,
        ] {
            let original = response();
            let mut expected = original.clone();
            expected.hits.pop();
            expected.truncated = true;
            expected.diagnostics.budget_dropped = 1;
            let one_hit_budget = rendered_response_len(&expected, output)?;

            let bounded = apply_response_budget(original, one_hit_budget, output)?;
            assert_eq!(bounded, expected, "unexpected bounded {output:?} response");
            assert!(rendered_response_len(&bounded, output)? <= one_hit_budget);

            let empty = SearchResponse {
                snapshot: "shared-snapshot".into(),
                publication_snapshot: "publication-snapshot".into(),
                hits: Vec::new(),
                diagnostics: diagnostics(),
                truncated: false,
            };
            let floor = rendered_response_len(&empty, output)?;
            assert_eq!(
                apply_response_budget(empty.clone(), floor, output)?,
                empty,
                "the exact {output:?} metadata floor must fit"
            );
            let error = apply_response_budget(empty, floor - 1, output).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("minimum_bytes={floor}")),
                "unexpected {output:?} floor error: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn compact_and_human_outputs_retain_recoverable_document_metadata() -> Result<()> {
        let mut result = response();
        result.hits.truncate(1);
        let hit = &mut result.hits[0];
        hit.document.title = "Release Guide".into();
        hit.document.description = Some("Current deployment instructions".into());
        hit.document.tags = vec!["alpha".into(), "comma,tag".into(), String::new()];
        hit.document.breadcrumb = "Deploy > Production".into();

        let compact: serde_json::Value = serde_json::from_str(&compact_search_string(&result)?)?;
        assert_eq!(compact["snapshot"], "shared-snapshot");
        assert_eq!(compact["publication_snapshot"], "publication-snapshot");
        assert_eq!(compact["hits"][0]["title"], "Release Guide");
        assert_eq!(
            compact["hits"][0]["description"],
            "Current deployment instructions"
        );
        assert_eq!(
            compact["hits"][0]["tags"],
            serde_json::json!(["alpha", "comma,tag", ""])
        );
        assert_eq!(compact["hits"][0]["heading"], "Deploy > Production");
        for private in [
            "snapshot",
            "file_hash",
            "source_start",
            "source_end",
            "source_bytes",
        ] {
            assert!(
                compact["hits"][0].get(private).is_none(),
                "compact hit serialized {private}"
            );
        }

        let wire = serde_json::to_value(&result)?;
        assert_eq!(wire["snapshot"], "shared-snapshot");
        assert_eq!(wire["publication_snapshot"], "publication-snapshot");
        assert!(wire.get("response_bytes").is_none());
        for private in ["snapshot", "file_hash", "source_start", "source_end"] {
            assert!(
                wire["hits"][0].get(private).is_none(),
                "debug hit serialized {private}"
            );
        }
        assert!(
            serde_json::to_value(SearchOptions::default())?
                .get("response_bytes")
                .is_none()
        );

        let human = human_search_string(&result);
        assert!(human.contains("shared-snapshot publication_snapshot=publication-snapshot"));
        assert!(human.contains("title=Release Guide"));
        assert!(human.contains("description=Current deployment instructions"));
        assert!(human.contains(r#"tags=["alpha","comma,tag",""]"#));
        assert!(human.contains("Deploy > Production"));
        assert!(!human.contains("file_hash="));
        assert!(!human.contains("source_bytes="));
        Ok(())
    }

    #[test]
    fn reranker_input_contains_normative_metadata_without_snapshot_or_freshness() {
        let mut document = hit(7, "guides/release.md");
        document.title = "Release Guide".into();
        document.description = Some("Current deployment instructions".into());
        document.tags = vec!["alpha".into(), "comma,tag".into()];
        document.breadcrumb = "Deploy > Production".into();
        document.rendered_body = "Use the blue channel.".into();
        document.file_hash = "must-not-reach-reranker".into();

        assert_eq!(
            reranker_document(&document),
            "path: guides/release.md\n\
             title: Release Guide\n\
             description: Current deployment instructions\n\
             tags: [\"alpha\",\"comma,tag\"]\n\
             breadcrumb: Deploy > Production\n\n\
             Use the blue channel."
        );
        let rendered = reranker_document(&document);
        assert!(!rendered.contains("must-not-reach-reranker"));
        assert!(!rendered.contains("freshness"));
        assert!(!rendered.contains("timestamp"));
    }

    #[test]
    fn required_vectors_error_when_the_provider_or_current_generation_is_absent() -> Result<()> {
        let (root, conn) = indexed_document()?;
        let required = SearchOptions {
            limit: 5,
            response_bytes: 100_000,
            vector: true,
            vector_required: true,
            rerank: false,
            ..SearchOptions::default()
        };

        let error = search(&conn, root.path(), None, "blue release", &required).unwrap_err();
        assert!(error.to_string().contains("no embedding provider"));

        let provider = test_provider("http://127.0.0.1:1/v1/embeddings".into())?;
        let error = search(
            &conn,
            root.path(),
            Some(&provider),
            "blue release",
            &required,
        )
        .unwrap_err();
        assert!(error.to_string().contains("generation is not ready"));
        Ok(())
    }

    #[test]
    fn incomplete_generation_degrades_to_bm25_and_rematerialization_clears_it() -> Result<()> {
        let (root, conn) = indexed_document()?;
        let snapshot = store::current_snapshot(&conn)?;
        let provider = test_provider("http://127.0.0.1:1/v1/embeddings".into())?;
        let spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
        let profile = ensure_profile(&conn, &spec, 2)?;
        conn.execute(
            "INSERT INTO doc_vector_generations(
               snapshot,profile_id,dimensions,chunk_format_version
             ) VALUES(?1,?2,2,?3)",
            params![snapshot, profile.id, CHUNK_FORMAT_VERSION],
        )?;

        let options = SearchOptions {
            limit: 5,
            response_bytes: 100_000,
            vector: true,
            vector_required: false,
            rerank: false,
            ..SearchOptions::default()
        };
        let degraded = search(
            &conn,
            root.path(),
            Some(&provider),
            "blue release",
            &options,
        )?;
        assert_eq!(degraded.diagnostics.vector_status, VectorStatus::Degraded);
        assert_eq!(degraded.hits.len(), 1);
        assert!(degraded.hits[0].lexical_score.is_some());
        assert!(degraded.hits[0].vector_score.is_none());

        rematerialize_cached_generations(&conn, &snapshot)?;
        assert!(!generation_is_ready(&conn, &snapshot, &profile)?);
        let not_ready = search(
            &conn,
            root.path(),
            Some(&provider),
            "blue release",
            &options,
        )?;
        assert_eq!(not_ready.diagnostics.vector_status, VectorStatus::NotReady);
        assert_eq!(not_ready.hits.len(), 1);
        Ok(())
    }

    #[test]
    fn index_rematerializes_complete_cache_without_a_prior_generation() -> Result<()> {
        let (root, conn) = indexed_document()?;
        let snapshot = store::current_snapshot(&conn)?;
        let provider = test_provider("http://127.0.0.1:1/v1/embeddings".into())?;
        let spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
        let profile = ensure_profile(&conn, &spec, 2)?;
        let embedding_identity: String = conn.query_row(
            "SELECT embedding_identity FROM doc_chunk_meta
             WHERE embedding_identity IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES(?1,?2,?3)",
            params![embedding_identity, profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        assert!(!generation_is_ready(&conn, &snapshot, &profile)?);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1",
                [profile.id],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );

        crate::indexer::index_repo(root.path(), &conn)?;

        assert_eq!(store::current_snapshot(&conn)?, snapshot);
        assert!(generation_is_ready(&conn, &snapshot, &profile)?);
        let materialized: (i64, i64) = conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1),
               (SELECT COUNT(*) FROM vec_doc_embeddings_2 WHERE profile_id=?1)",
            [profile.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(materialized, (1, 1));
        Ok(())
    }

    #[test]
    fn deleting_an_unembedded_document_republishes_cached_readiness() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("README.md"),
            "# Deployment\n\nUse the blue release channel.\n",
        )?;
        std::fs::write(root.path().join("NOTES.md"), "# Notes\n")?;
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let initial_snapshot = store::current_snapshot(&conn)?;

        let stub_identity: Option<String> = conn.query_row(
            "SELECT m.embedding_identity
             FROM doc_chunk_meta m
             JOIN chunks c ON c.id=m.chunk_id
             JOIN files f ON f.id=c.file_id
             WHERE f.path='NOTES.md'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            stub_identity, None,
            "heading-only documents are not embedded"
        );

        let identity: String = conn.query_row(
            "SELECT m.embedding_identity
             FROM doc_chunk_meta m
             JOIN chunks c ON c.id=m.chunk_id
             JOIN files f ON f.id=c.file_id
             WHERE f.path='README.md'",
            [],
            |row| row.get(0),
        )?;
        let provider = test_provider("http://127.0.0.1:1/v1/embeddings".into())?;
        let spec = provider.profile_for(CHUNK_FORMAT_VERSION)?;
        let profile = ensure_profile(&conn, &spec, 2)?;
        conn.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES(?1,?2,?3)",
            params![identity, profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        rematerialize_cached_generations(&conn, &initial_snapshot)?;
        assert!(generation_is_ready(&conn, &initial_snapshot, &profile)?);

        std::fs::remove_file(root.path().join("NOTES.md"))?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let updated_snapshot = store::current_snapshot(&conn)?;
        assert_ne!(updated_snapshot, initial_snapshot);
        assert!(generation_is_ready(&conn, &updated_snapshot, &profile)?);
        let state: (i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM embeddings WHERE profile_id=?1),
               (SELECT count(*) FROM doc_embedding_index_entries WHERE profile_id=?1),
               (SELECT count(*) FROM doc_vector_generations WHERE profile_id=?1)",
            [profile.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(state, (1, 1, 1));
        Ok(())
    }

    #[test]
    fn provider_batch_failure_keeps_cache_but_never_publishes_partial_readiness() -> Result<()> {
        let root = tempfile::tempdir()?;
        for index in 0..17 {
            std::fs::write(
                root.path().join(format!("guide-{index:02}.md")),
                format!("# Guide {index}\n\nUnique deployment instruction {index}.\n"),
            )?;
        }
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let (endpoint, server) = spawn_transient_embedding_failure_server()?;
        let provider = test_provider(endpoint)?;

        let error = embed_current(&conn, &provider, 16).unwrap_err();
        assert!(error.to_string().contains("unexpected embedding response"));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM embeddings", [], |row| {
                row.get::<_, i64>(0)
            })?,
            16
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM doc_vector_generations", [], |row| {
                row.get::<_, i64>(0)
            })?,
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM doc_embedding_index_entries",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );

        let recovered = embed_current(&conn, &provider, 16)?;
        assert_eq!(recovered.cached_reused, 16);
        assert_eq!(recovered.embedded, 1);
        assert_eq!(recovered.occurrences_materialized, 17);
        assert!(recovered.generation_published);
        let requests = server
            .join()
            .map_err(|_| anyhow::anyhow!("fake embedding server panicked"))??;
        assert_eq!(
            requests
                .iter()
                .map(|request| request["input"].as_array().map_or(0, Vec::len))
                .collect::<Vec<_>>(),
            [16, 1, 1]
        );
        Ok(())
    }

    #[test]
    fn source_resolution_hashes_and_slices_one_capture() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("guide.md"), "body")?;
        let mut result = RetrievalHit {
            document: hit(1, "guide.md"),
            rank: 1,
            lexical_score: Some(1.0),
            vector_score: None,
            freshness_value: None,
            freshness_secondary_value: None,
            base_rank: 1,
            movement: 0,
            content: "rendered".into(),
            source_state: SourceState::SourceMismatch,
            source_detail: Some("not_resolved".into()),
        };
        resolve_hit_sources(root.path(), std::slice::from_mut(&mut result));
        assert_eq!(result.source_state, SourceState::Current);
        assert_eq!(result.content, "body");

        std::fs::write(root.path().join("guide.md"), "changed")?;
        result.content = "rendered".into();
        result.source_state = SourceState::SourceMismatch;
        resolve_hit_sources(root.path(), std::slice::from_mut(&mut result));
        assert_eq!(result.source_state, SourceState::SourceMismatch);
        assert_eq!(result.content, "rendered");
        Ok(())
    }

    #[test]
    fn nul_body_keeps_embedding_identity_aligned_with_provider_text() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("guide.md"), b"# Guide\n\nalpha\0omega\n")?;
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;

        let (identity, heading, body): (String, Option<String>, String) = conn.query_row(
            "SELECT m.embedding_identity,m.nearest_heading,docs_fts.body
             FROM doc_chunk_meta m
             JOIN docs_fts ON docs_fts.rowid=m.chunk_id
             WHERE m.embedding_identity IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert!(body.contains('\0'));
        assert_eq!(
            identity,
            crate::docs::corpus::embedding_identity(heading.as_deref(), &body)
        );

        let documents = embedding_documents(&conn)?;
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].identity, identity);
        assert_eq!(documents[0].text, "Guide\n\nalpha\0omega");
        Ok(())
    }

    #[test]
    fn documentation_vectors_follow_registry_eligibility_not_docs_corpus() -> Result<()> {
        let (root, conn) = indexed_document()?;
        let snapshot = store::current_snapshot(&conn)?;
        let (eligible_chunk_id, eligible_identity): (i64, String) = conn.query_row(
            "SELECT m.chunk_id, m.embedding_identity
             FROM doc_chunk_meta m
             JOIN chunks c ON c.id=m.chunk_id
             JOIN files f ON f.id=c.file_id
             WHERE f.path='README.md' AND m.embedding_identity IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        // This deliberately violates the registry's Rust corpus assignment
        // while satisfying the schema-level docs-sidecar invariant. Any
        // capability inferred from `corpus='docs'` would admit it.
        let ineligible_body = "registryIneligibleVectorNeedle";
        let ineligible_identity = "ineligible-rust-doc-vector";
        conn.execute(
            "INSERT INTO files(path,hash,corpus,format,role,origin)
             VALUES('ineligible.md',?1,'docs','rust','documentation','repository')",
            [blake3::hash(ineligible_body.as_bytes()).to_hex().as_str()],
        )?;
        let ineligible_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks(
               file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'markdown_section',NULL,'','',0,?2,1,1,?3,?4)",
            params![
                ineligible_file_id,
                i64::try_from(ineligible_body.len())?,
                blake3::hash(ineligible_body.as_bytes()).to_hex().as_str(),
                ineligible_body,
            ],
        )?;
        let ineligible_chunk_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO doc_chunk_meta(
               chunk_id,title,breadcrumb,nearest_heading,ordinal,
               embedding_identity,front_matter_state
             ) VALUES(?1,'Ineligible','','Ineligible',0,?2,'none')",
            params![ineligible_chunk_id, ineligible_identity],
        )?;
        conn.execute(
            "INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
             VALUES(?1,'Ineligible','','',?2,'ineligible.md')",
            params![ineligible_chunk_id, ineligible_body],
        )?;

        let documents = embedding_documents(&conn)?;
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].identity, eligible_identity);
        assert!(!documents[0].text.contains(ineligible_body));

        // One document request, one successful query request, and one query
        // request whose stale materialization is rejected below.
        let (endpoint, server) = spawn_openai_embedding_server(3)?;
        let provider = test_provider(endpoint)?;
        let report = embed_current(&conn, &provider, 16)?;
        assert_eq!(report.unique_representations, 1);
        assert_eq!(report.embeddable_occurrences, 1);
        assert_eq!(report.occurrences_materialized, 1);
        let profile_id = report.profile_id.context("missing docs profile")?;
        let dimensions = report.dimensions.context("missing docs dimensions")?;
        assert_eq!(dimensions, 2);

        // Even a durable cache entry for the ineligible representation must
        // not be rematerialized into the documentation sqlite-vec table.
        conn.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES(?1,?2,?3)",
            params![ineligible_identity, profile_id, vec_to_blob(&[0.0, 1.0]),],
        )?;
        rematerialize_cached_generations(&conn, &snapshot)?;
        let profile = ResolvedProfile {
            id: profile_id,
            dimensions,
        };
        assert!(generation_is_ready(&conn, &snapshot, &profile)?);
        let materialized = conn
            .prepare(
                "SELECT entry.chunk_id, f.path
                 FROM doc_embedding_index_entries entry
                 JOIN chunks c ON c.id=entry.chunk_id
                 JOIN files f ON f.id=c.file_id
                 WHERE entry.profile_id=?1
                 ORDER BY entry.chunk_id",
            )?
            .query_map([profile_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(materialized, vec![(eligible_chunk_id, "README.md".into())]);

        let required_vectors = SearchOptions {
            limit: 5,
            response_bytes: 100_000,
            vector: true,
            vector_required: true,
            rerank: false,
            ..SearchOptions::default()
        };
        let result = search(
            &conn,
            root.path(),
            Some(&provider),
            "zzRegistryVectorQueryOnly",
            &required_vectors,
        )?;
        assert_eq!(result.diagnostics.vector_status, VectorStatus::Active);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].document.path, "README.md");
        assert!(result.hits[0].vector_score.is_some());
        assert!(result.hits[0].lexical_score.is_none());

        // Simulate an obsolete materialized occurrence from a formerly
        // eligible format. Full-distance candidates filter it, KNN refuses an
        // underfilled eligible result set, and the public readiness check
        // rejects the contaminated dedicated table before either can leak it.
        conn.execute(
            "INSERT INTO doc_embedding_index_entries(chunk_id,profile_id) VALUES(?1,?2)",
            params![ineligible_chunk_id, profile_id],
        )?;
        let ineligible_entry_id = conn.last_insert_rowid();
        let table = vector_table(dimensions);
        conn.execute(
            &format!("INSERT INTO {table}(rowid,embedding,profile_id) VALUES(?1,?2,?3)"),
            params![ineligible_entry_id, vec_to_blob(&[0.0, 1.0]), profile_id,],
        )?;
        let eligible_formats =
            crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationVector);
        let full = full_distance_vector_search(
            &conn,
            profile_id,
            &vec_to_blob(&[1.0, 0.0]),
            1,
            &eligible_formats,
        )?;
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].chunk_id, eligible_chunk_id);
        let knn_error = knn_vector_candidates(
            &conn,
            &table,
            profile_id,
            &vec_to_blob(&[1.0, 0.0]),
            2,
            &eligible_formats,
        )
        .unwrap_err();
        assert!(
            knn_error
                .to_string()
                .contains("expected 2 candidates, found 1")
        );

        assert!(generation_is_ready(&conn, &snapshot, &profile)?);
        let error = search(
            &conn,
            root.path(),
            Some(&provider),
            "zzRegistryVectorQueryOnly",
            &required_vectors,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("documentation vector generation is incomplete"),
            "unexpected stale-generation error: {error:#}"
        );

        let requests = server
            .join()
            .map_err(|_| anyhow::anyhow!("fake embedding server panicked"))??;
        let document_inputs = requests[0]["input"]
            .as_array()
            .context("document embedding request input is not an array")?;
        assert_eq!(document_inputs.len(), 1);
        assert!(document_inputs[0].as_str().is_some_and(|input| {
            input.contains("blue release") && !input.contains(ineligible_body)
        }));
        Ok(())
    }

    #[test]
    fn final_materialization_rechecks_docs_and_reports_the_current_publication() -> Result<()> {
        let root = tempfile::tempdir()?;
        let conn = crate::store::open(root.path())?;
        crate::publication::Identities::publish_test(&conn, "code", "s1", "provenance")?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('extraction_version',?1)",
            [crate::entity::EXTRACTION_VERSION],
        )?;
        conn.execute(
            "INSERT INTO meta(key,value)
             VALUES('documentation_chunk_format_version',?1)",
            [CHUNK_FORMAT_VERSION],
        )?;
        for format in crate::formats::ALL {
            conn.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)",
                rusqlite::params![
                    crate::formats::contract_meta_key(format),
                    format.extractor_version
                ],
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key,value)
             VALUES('documentation_provenance_format_version',?1)",
            [crate::docs::PROVENANCE_FORMAT_VERSION],
        )?;
        conn.execute(
            "INSERT INTO files(path,hash,role,corpus,format)
             VALUES('guide.md','hash','documentation','docs','markdown')",
            [],
        )?;
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks(file_id,kind,name,symbols,start,end,start_line,end_line,hash,content)
             VALUES(?1,'markdown_section',NULL,'',0,4,1,1,'source','body')",
            [file_id],
        )?;
        let chunk_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO doc_chunk_meta(chunk_id,title,breadcrumb,ordinal,embedding_identity,front_matter_state)
             VALUES(?1,'Guide','',0,'doc-id','none')",
            [chunk_id],
        )?;
        conn.execute(
            "INSERT INTO embedding_profiles(provider,model,config_fingerprint,dimensions,config_json)
             VALUES('test','test','profile',2,'{}')",
            [],
        )?;
        let profile = ResolvedProfile {
            id: conn.last_insert_rowid(),
            dimensions: 2,
        };
        conn.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES('doc-id',?1,?2)",
            params![profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
        let (materialized, identity) = materialize_current_generation(&conn, "s1", &profile)?;
        assert_eq!(materialized, 1);
        assert_eq!(identity.snapshot, "s1");
        assert_eq!(
            identity.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );

        crate::publication::Identities::publish_test(&conn, "code-2", "s1", "provenance")?;
        let (materialized, code_only_identity) =
            materialize_current_generation(&conn, "s1", &profile)?;
        assert_eq!(materialized, 1);
        assert_eq!(code_only_identity.snapshot, "s1");
        assert_ne!(
            code_only_identity.publication_snapshot,
            identity.publication_snapshot
        );
        assert_eq!(
            code_only_identity.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );

        crate::publication::Identities::publish_test(&conn, "code", "s2", "provenance")?;
        let error = materialize_current_generation(&conn, "s1", &profile).unwrap_err();
        assert!(error.to_string().contains("documentation digest advanced"));
        let still_ready: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM doc_vector_generations WHERE snapshot='s1')",
            [],
            |row| row.get(0),
        )?;
        assert!(still_ready);
        Ok(())
    }

    #[test]
    fn provider_backed_docs_vectors_use_shared_cache_and_stay_out_of_code_indexes() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("README.md"),
            "# Unified vectors\n\nThe current guide explains orbital indexing.\n",
        )?;
        std::fs::write(
            root.path().join("app.ts"),
            "export const orbit = 'code corpus';\n",
        )?;
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let snapshot = store::current_snapshot(&conn)?;
        let code_snapshot = crate::structural::current_snapshot(&conn)?;

        // One document request plus two query requests. Index refreshes below
        // have no provider handle and must rematerialize exclusively from the
        // durable shared cache.
        let (endpoint, server) = spawn_openai_embedding_server(3)?;
        let provider = Provider::from_settings(
            &crate::config::EmbeddingSettings {
                provider: Some("openai".into()),
                model: Some("fake-docs-2d".into()),
                revision: None,
                url: Some(endpoint),
                api_key_env: Some("JSCOUT_TEST_UNUSED_EMBED_KEY".into()),
                query_prefix: Some(String::new()),
                batch: 16,
                origins: crate::origin::defaults(),
            },
            &crate::config::InferenceSettings {
                url: "http://127.0.0.1:1".into(),
                host: "127.0.0.1".into(),
                port: 1,
                project: None,
                uv: "uv".into(),
                allow_remote: false,
                batch_size: 16,
                max_length: 4096,
                model_cache_root: None,
            },
        )?
        .context("test embedding provider was not configured")?;

        let initial = embed_current(&conn, &provider, 16)?;
        assert_eq!(initial.snapshot, snapshot);
        assert_eq!(
            initial.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );
        assert_eq!(initial.embedded, 1);
        assert_eq!(initial.cached_reused, 0);
        assert_eq!(initial.occurrences_materialized, 1);
        assert!(initial.generation_published);
        let profile_id = initial.profile_id.context("missing docs profile")?;

        let cached = embed_current(&conn, &provider, 16)?;
        assert_eq!(
            cached.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );
        assert_eq!(cached.profile_id, Some(profile_id));
        assert_eq!(cached.missing_before, 0);
        assert_eq!(cached.embedded, 0);
        assert_eq!(cached.cached_reused, 1);
        assert_eq!(cached.occurrences_materialized, 1);

        let generation: (String, i64, i64, String) = conn.query_row(
            "SELECT snapshot,profile_id,dimensions,chunk_format_version
             FROM doc_vector_generations WHERE profile_id=?1",
            [profile_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            generation,
            (snapshot.clone(), profile_id, 2, CHUNK_FORMAT_VERSION.into())
        );
        let cached_docs: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM embeddings e
             JOIN doc_chunk_meta m ON m.embedding_identity=e.chunk_hash
             WHERE e.profile_id=?1",
            [profile_id],
            |row| row.get(0),
        )?;
        assert_eq!(cached_docs, 1);
        let materialized_docs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vec_doc_embeddings_2 WHERE profile_id=?1",
            [profile_id],
            |row| row.get(0),
        )?;
        assert_eq!(materialized_docs, 1);
        let unchanged_occurrence_id: i64 = conn.query_row(
            "SELECT id FROM doc_embedding_index_entries WHERE profile_id=?1",
            [profile_id],
            |row| row.get(0),
        )?;
        crate::indexer::index_repo(root.path(), &conn)?;
        assert_eq!(store::current_snapshot(&conn)?, snapshot);
        assert_eq!(
            conn.query_row(
                "SELECT id FROM doc_embedding_index_entries WHERE profile_id=?1",
                [profile_id],
                |row| row.get::<_, i64>(0),
            )?,
            unchanged_occurrence_id,
            "a true no-op index must not rebuild the active docs generation"
        );

        std::fs::write(
            root.path().join("app.ts"),
            "export const orbit = 'changed code corpus';\n",
        )?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let code_changed_snapshot = crate::structural::current_snapshot(&conn)?;
        assert_ne!(code_changed_snapshot, code_snapshot);
        assert_eq!(store::current_snapshot(&conn)?, snapshot);
        assert!(generation_is_ready(
            &conn,
            &snapshot,
            &ResolvedProfile {
                id: profile_id,
                dimensions: 2,
            }
        )?);
        assert_eq!(
            conn.query_row(
                "SELECT id FROM doc_embedding_index_entries WHERE profile_id=?1",
                [profile_id],
                |row| row.get::<_, i64>(0),
            )?,
            unchanged_occurrence_id,
            "a code-only publication must not rebuild documentation occurrences"
        );
        assert_eq!(
            conn.query_row(
                "SELECT snapshot,profile_id,dimensions,chunk_format_version
                 FROM doc_vector_generations WHERE profile_id=?1",
                [profile_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?,
            generation,
            "a code-only publication must leave documentation readiness untouched"
        );

        let refresh = crate::indexer::refresh_repo_with_options(
            root.path(),
            &conn,
            &crate::indexer::IndexOptions::default(),
        )?;
        assert!(
            refresh.extraction_reset,
            "this regression must exercise the destructive full-refresh path"
        );
        let rebuilt_snapshot = store::current_snapshot(&conn)?;
        assert_eq!(rebuilt_snapshot, snapshot);
        assert!(generation_is_ready(
            &conn,
            &rebuilt_snapshot,
            &ResolvedProfile {
                id: profile_id,
                dimensions: 2,
            }
        )?);
        let rematerialized: (i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM doc_vector_generations WHERE profile_id=?1),
               (SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1),
               (SELECT COUNT(*) FROM vec_doc_embeddings_2 WHERE profile_id=?1)",
            [profile_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            rematerialized,
            (1, 1, 1),
            "an identical full refresh must rebuild docs vector readiness and occurrences"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM embeddings WHERE profile_id=?1",
                [profile_id],
                |row| row.get::<_, i64>(0),
            )?,
            1,
            "full refresh must preserve the durable documentation cache"
        );

        let fused = search(
            &conn,
            root.path(),
            Some(&provider),
            "orbital indexing",
            &SearchOptions {
                limit: 5,
                response_bytes: 100_000,
                vector_required: true,
                rerank: false,
                ..SearchOptions::default()
            },
        )?;
        assert_eq!(fused.diagnostics.vector_status, VectorStatus::Active);
        assert_eq!(fused.hits.len(), 1);
        assert!(fused.hits[0].lexical_score.is_some());
        assert!(fused.hits[0].vector_score.is_some());

        let response = search(
            &conn,
            root.path(),
            Some(&provider),
            "a phrase absent from every document",
            &SearchOptions {
                limit: 5,
                response_bytes: 100_000,
                vector_required: true,
                rerank: false,
                ..SearchOptions::default()
            },
        )?;
        assert_eq!(response.snapshot, rebuilt_snapshot);
        assert_eq!(
            response.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );
        assert_eq!(response.diagnostics.lexical_candidates, 0);
        assert_eq!(response.diagnostics.vector_status, VectorStatus::Active);
        assert_eq!(response.diagnostics.vector_candidates, 1);
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].document.path, "README.md");
        assert!(response.hits[0].vector_score.is_some());

        let docs_in_code_fts: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM chunks_fts
             JOIN doc_chunk_meta m ON m.chunk_id=chunks_fts.rowid",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(docs_in_code_fts, 0);
        let docs_in_code_vectors: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM embedding_index_entries e
             JOIN doc_chunk_meta m ON m.chunk_id=e.chunk_id",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(docs_in_code_vectors, 0);

        std::fs::rename(root.path().join("README.md"), root.path().join("GUIDE.md"))?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let renamed_docs_snapshot = store::current_snapshot(&conn)?;
        assert_ne!(renamed_docs_snapshot, rebuilt_snapshot);
        assert!(generation_is_ready(
            &conn,
            &renamed_docs_snapshot,
            &ResolvedProfile {
                id: profile_id,
                dimensions: 2,
            }
        )?);
        let renamed_materialization: (i64, i64, String) = conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1),
               (SELECT COUNT(*) FROM vec_doc_embeddings_2 WHERE profile_id=?1),
               (SELECT f.path
                  FROM doc_embedding_index_entries entry
                  JOIN chunks chunk ON chunk.id=entry.chunk_id
                  JOIN files f ON f.id=chunk.file_id
                 WHERE entry.profile_id=?1)",
            [profile_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(renamed_materialization, (1, 1, "GUIDE.md".into()));

        std::fs::write(
            root.path().join("GUIDE.md"),
            "# Unified vectors\n\nA new uncached documentation identity.\n",
        )?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let changed_docs_snapshot = store::current_snapshot(&conn)?;
        assert!(!generation_is_ready(
            &conn,
            &changed_docs_snapshot,
            &ResolvedProfile {
                id: profile_id,
                dimensions: 2,
            }
        )?);
        let stale_occurrences: i64 = conn.query_row(
            "SELECT COUNT(*) FROM doc_embedding_index_entries WHERE profile_id=?1",
            [profile_id],
            |row| row.get(0),
        )?;
        assert_eq!(stale_occurrences, 0);

        let requests = server
            .join()
            .map_err(|_| anyhow::anyhow!("fake embedding server panicked"))??;
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["input"].as_array().map(Vec::len), Some(1));
        assert!(
            requests[0]["input"][0]
                .as_str()
                .is_some_and(|text| text.contains("orbital indexing"))
        );
        assert_eq!(
            requests[1]["input"],
            serde_json::json!(["orbital indexing"])
        );
        assert_eq!(
            requests[2]["input"],
            serde_json::json!(["a phrase absent from every document"])
        );
        Ok(())
    }
}
