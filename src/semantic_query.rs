//! Bounded semantic-memory retrieval and exact evidence drill-down.
//!
//! This surface is deliberately separate from code ranking: artifacts,
//! artifact relations, and source evidence are independently labelled
//! sections with one whole-response byte budget. Generated prose remains
//! untrusted data and every source excerpt is hash-checked against the index.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use crate::{origin, semantic, store, structural};

pub const DEFAULT_RESPONSE_BYTE_LIMIT: usize = 24_000;
pub const DEFAULT_SOURCE_BYTE_LIMIT: usize = 2_000;
pub const MAX_ARTIFACT_LIMIT: usize = 100;
pub const MAX_RELATION_LIMIT: usize = 200;
pub const MAX_SOURCE_LIMIT: usize = 100;
const MAX_SOURCE_BYTE_LIMIT: usize = 16_000;
const EVIDENCE_RELATION_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub query: String,
    pub artifact_id: Option<i64>,
    pub anchor: Option<String>,
    pub related_to: Option<i64>,
    pub artifact_types: Vec<String>,
    pub freshness: Vec<String>,
    pub include_superseded: bool,
    pub limit: usize,
    pub supports_per_artifact: usize,
    pub relation_limit: usize,
    pub include_source: bool,
    pub source_limit: usize,
    pub source_byte_limit: usize,
    pub file_origins: Vec<String>,
    pub response_byte_limit: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            artifact_id: None,
            anchor: None,
            related_to: None,
            artifact_types: Vec::new(),
            freshness: Vec::new(),
            include_superseded: false,
            limit: 20,
            supports_per_artifact: 8,
            relation_limit: 40,
            include_source: false,
            source_limit: 12,
            source_byte_limit: DEFAULT_SOURCE_BYTE_LIMIT,
            file_origins: origin::defaults(),
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactView {
    pub id: i64,
    pub supersedes: Option<i64>,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub current: bool,
    pub superseded_by: Option<i64>,
    pub trust: String,
    pub body: Value,
    pub model: String,
    pub prompt_version: String,
    pub confidence: String,
    pub source_snapshot: String,
    pub created_at: String,
    pub freshness: String,
    pub relevance: f64,
    pub support_count: usize,
    pub supports: Vec<semantic::SemanticSupport>,
    pub supports_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactHeader {
    pub id: i64,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub current: bool,
    pub confidence: String,
    pub freshness: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedArtifact {
    pub root_artifact_id: i64,
    pub direction: String,
    pub relation: String,
    pub claim_path: String,
    pub confidence: String,
    pub pinned_fingerprint: String,
    pub artifact: ArtifactHeader,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceEvidence {
    pub root_artifact_id: i64,
    pub evidence_artifact_id: i64,
    pub via_artifact_ids: Vec<i64>,
    pub via_relations: Vec<EvidenceHop>,
    pub claim_path: String,
    pub relationship: String,
    pub anchor: String,
    pub file: String,
    pub file_role: String,
    pub file_origin: String,
    pub lines: [i64; 2],
    pub confidence: String,
    pub support_freshness: String,
    pub source_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub source_original_bytes: usize,
    pub source_rendered_bytes: usize,
    pub source_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceHop {
    pub artifact_id: i64,
    pub relation: String,
    pub claim_path: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResponseBudget {
    pub byte_limit: usize,
    pub rendered_bytes: usize,
    pub unbudgeted_bytes: usize,
    pub truncated: bool,
    pub omitted_artifacts: usize,
    pub omitted_relations: usize,
    pub omitted_sources: usize,
    pub omitted_sources_by_origin: usize,
    pub truncated_sources: usize,
    pub omitted_supports: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub snapshot: String,
    pub matched_artifacts: usize,
    pub semantic_artifacts: Vec<ArtifactView>,
    pub related_artifacts: Vec<RelatedArtifact>,
    pub source_evidence: Vec<SourceEvidence>,
    pub response_budget: ResponseBudget,
}

#[derive(Debug)]
struct Candidate {
    id: i64,
    artifact_type: String,
    name: Option<String>,
    body_json: String,
    current: bool,
    superseded_by: Option<i64>,
    relevance: f64,
}

pub fn query(root: &Path, conn: &Connection, options: &QueryOptions) -> Result<QueryResult> {
    validate_options(options)?;
    store::with_read_snapshot(conn, "jscout_semantic_query", || {
        let snapshot = structural::current_snapshot(conn)?;
        let mut candidates = candidates(conn, options)?;
        rank_candidates(&mut candidates, &options.query);
        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        let relevance = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.relevance))
            .collect::<HashMap<_, _>>();
        let current = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.current))
            .collect::<HashMap<_, _>>();
        let superseded_by = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.superseded_by))
            .collect::<HashMap<_, _>>();

        let mut loaded = semantic::load_artifacts(conn, &candidate_ids)?;
        loaded.retain(|artifact| {
            options.freshness.is_empty()
                || options
                    .freshness
                    .iter()
                    .any(|value| value == &artifact.freshness)
        });
        let matched_artifacts = loaded.len();
        loaded.truncate(options.limit);
        let omitted_artifacts = matched_artifacts.saturating_sub(loaded.len());
        for artifact in &mut loaded {
            artifact.relevance = relevance.get(&artifact.id).copied().unwrap_or_default();
        }

        let selected_ids = loaded
            .iter()
            .map(|artifact| artifact.id)
            .collect::<Vec<_>>();
        let (related_artifacts, omitted_relations) =
            related_artifacts(conn, &selected_ids, options.relation_limit)?;
        let (source_evidence, omitted_sources_by_origin, omitted_sources) =
            if options.include_source {
                source_evidence(root, conn, &selected_ids, options)?
            } else {
                (Vec::new(), 0, 0)
            };
        let semantic_artifacts: Vec<ArtifactView> = loaded
            .into_iter()
            .map(|artifact| {
                artifact_view(
                    artifact,
                    &current,
                    &superseded_by,
                    options.supports_per_artifact,
                )
            })
            .collect();
        let omitted_supports = semantic_artifacts
            .iter()
            .map(|artifact| {
                artifact
                    .support_count
                    .saturating_sub(artifact.supports.len())
            })
            .sum();
        let truncated_sources = source_evidence
            .iter()
            .filter(|source| source.source_truncated)
            .count();
        let initially_truncated = omitted_artifacts > 0
            || omitted_relations > 0
            || omitted_sources > 0
            || omitted_sources_by_origin > 0
            || omitted_supports > 0
            || truncated_sources > 0;
        let mut result = QueryResult {
            snapshot,
            matched_artifacts,
            semantic_artifacts,
            related_artifacts,
            source_evidence,
            response_budget: ResponseBudget {
                byte_limit: options.response_byte_limit,
                truncated: initially_truncated,
                omitted_artifacts,
                omitted_relations,
                omitted_sources,
                omitted_sources_by_origin,
                truncated_sources,
                omitted_supports,
                ..Default::default()
            },
        };
        apply_response_budget(&mut result)?;
        Ok(result)
    })
}

fn validate_options(options: &QueryOptions) -> Result<()> {
    if options.limit == 0 || options.limit > MAX_ARTIFACT_LIMIT {
        bail!("semantic artifact limit must be between 1 and {MAX_ARTIFACT_LIMIT}");
    }
    if options.supports_per_artifact == 0 || options.supports_per_artifact > 64 {
        bail!("supports per artifact must be between 1 and 64");
    }
    if options.relation_limit == 0 || options.relation_limit > MAX_RELATION_LIMIT {
        bail!("semantic relation limit must be between 1 and {MAX_RELATION_LIMIT}");
    }
    if options.source_limit == 0 || options.source_limit > MAX_SOURCE_LIMIT {
        bail!("semantic source limit must be between 1 and {MAX_SOURCE_LIMIT}");
    }
    if options.source_byte_limit == 0 || options.source_byte_limit > MAX_SOURCE_BYTE_LIMIT {
        bail!("source byte limit must be between 1 and {MAX_SOURCE_BYTE_LIMIT}");
    }
    if options.response_byte_limit == 0 {
        bail!("response byte limit must be greater than zero");
    }
    origin::validate_all(&options.file_origins)?;
    if let Some(value) = options
        .freshness
        .iter()
        .find(|value| !matches!(value.as_str(), "fresh" | "degraded" | "stale"))
    {
        bail!("unknown semantic freshness `{value}`");
    }
    if let Some(value) = options.artifact_types.iter().find(|value| {
        !matches!(
            value.as_str(),
            "workflow" | "card" | "concept" | "summary" | "annotation"
        )
    }) {
        bail!("unknown semantic artifact type `{value}`");
    }
    Ok(())
}

fn candidates(conn: &Connection, options: &QueryOptions) -> Result<Vec<Candidate>> {
    let mut statement = conn.prepare(
        "SELECT artifact.id, artifact.artifact_type, artifact.canonical_name,
                artifact.body_json,
                NOT EXISTS(
                  SELECT 1 FROM semantic_artifacts successor
                  WHERE successor.supersedes_artifact_id=artifact.id
                ) AS current,
                (SELECT successor.id FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id) AS superseded_by
         FROM semantic_artifacts artifact
         WHERE (?1 IS NULL OR artifact.id=?1)
           AND (?2 IS NULL OR EXISTS(
             SELECT 1 FROM semantic_supports support
             WHERE support.artifact_id=artifact.id AND support.anchor_key=?2
           ))
           AND (?3 IS NULL OR EXISTS(
             SELECT 1 FROM semantic_relations relation
             WHERE (relation.src_artifact_id=artifact.id AND relation.dst_artifact_id=?3)
                OR (relation.dst_artifact_id=artifact.id AND relation.src_artifact_id=?3)
           ))
         ORDER BY artifact.id DESC",
    )?;
    let rows = statement.query_map(
        rusqlite::params![options.artifact_id, options.anchor, options.related_to],
        |row| {
            Ok(Candidate {
                id: row.get(0)?,
                artifact_type: row.get(1)?,
                name: row.get(2)?,
                body_json: row.get(3)?,
                current: row.get(4)?,
                superseded_by: row.get(5)?,
                relevance: 0.0,
            })
        },
    )?;
    let mut candidates = Vec::new();
    for row in rows {
        let candidate = row?;
        if options.artifact_id.is_none() && !options.include_superseded && !candidate.current {
            continue;
        }
        if !options.artifact_types.is_empty()
            && !options
                .artifact_types
                .iter()
                .any(|value| value == &candidate.artifact_type)
        {
            continue;
        }
        candidates.push(candidate);
    }
    if let Some(id) = options.artifact_id
        && candidates.is_empty()
    {
        bail!("semantic artifact {id} does not exist or is excluded by the type filter");
    }
    Ok(candidates)
}

fn rank_candidates(candidates: &mut Vec<Candidate>, query: &str) {
    let tokens = tokens(query);
    candidates.retain_mut(|candidate| {
        if tokens.is_empty() {
            return true;
        }
        let name = candidate.name.as_deref().unwrap_or_default().to_lowercase();
        let body = candidate.body_json.to_lowercase();
        let matches = tokens
            .iter()
            .filter(|token| name.contains(token.as_str()) || body.contains(token.as_str()))
            .count();
        if matches == 0 {
            return false;
        }
        let exact_name = usize::from(!name.is_empty() && query.eq_ignore_ascii_case(&name));
        candidate.relevance =
            ((matches + exact_name * 4) as f64 / tokens.len().max(1) as f64).min(1.0);
        true
    });
    candidates.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn tokens(query: &str) -> Vec<String> {
    query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '$'
        })
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

fn artifact_view(
    mut artifact: semantic::SemanticArtifact,
    current: &HashMap<i64, bool>,
    superseded_by: &HashMap<i64, Option<i64>>,
    support_limit: usize,
) -> ArtifactView {
    let support_count = artifact.supports.len();
    artifact.supports.truncate(support_limit);
    ArtifactView {
        id: artifact.id,
        supersedes: artifact.supersedes,
        artifact_type: artifact.artifact_type,
        name: artifact.name,
        current: current.get(&artifact.id).copied().unwrap_or(false),
        superseded_by: superseded_by.get(&artifact.id).copied().flatten(),
        trust: artifact.trust,
        body: artifact.body,
        model: artifact.model,
        prompt_version: artifact.prompt_version,
        confidence: artifact.confidence,
        source_snapshot: artifact.source_snapshot,
        created_at: artifact.created_at,
        freshness: artifact.freshness,
        relevance: artifact.relevance,
        support_count,
        supports_truncated: support_count > artifact.supports.len(),
        supports: artifact.supports,
    }
}

fn related_artifacts(
    conn: &Connection,
    root_ids: &[i64],
    limit: usize,
) -> Result<(Vec<RelatedArtifact>, usize)> {
    #[derive(Debug)]
    struct Row {
        root_id: i64,
        direction: String,
        relation: String,
        claim_path: String,
        confidence: String,
        pinned_fingerprint: String,
        related_id: i64,
        current: bool,
    }

    let mut rows = Vec::new();
    let mut matched = 0_usize;
    let mut statement = conn.prepare_cached(
        "SELECT relation.src_artifact_id, relation.dst_artifact_id,
                relation.relation, relation.claim_path, relation.confidence,
                relation.dst_fingerprint,
                NOT EXISTS(
                  SELECT 1 FROM semantic_artifacts successor
                  WHERE successor.supersedes_artifact_id=
                    CASE WHEN relation.src_artifact_id=?1
                         THEN relation.dst_artifact_id ELSE relation.src_artifact_id END
                ) AS related_current
         FROM semantic_relations relation
         WHERE relation.src_artifact_id=?1 OR relation.dst_artifact_id=?1
         ORDER BY relation.relation, relation.claim_path,
                  relation.src_artifact_id, relation.dst_artifact_id",
    )?;
    for &root_id in root_ids {
        let mapped = statement.query_map([root_id], |row| {
            let source: i64 = row.get(0)?;
            let target: i64 = row.get(1)?;
            Ok(Row {
                root_id,
                direction: if source == root_id { "out" } else { "in" }.into(),
                relation: row.get(2)?,
                claim_path: row.get(3)?,
                confidence: row.get(4)?,
                pinned_fingerprint: row.get(5)?,
                related_id: if source == root_id { target } else { source },
                current: row.get(6)?,
            })
        })?;
        for row in mapped {
            let row = row?;
            matched += 1;
            if rows.len() < limit {
                rows.push(row);
            }
        }
    }
    let omitted = matched.saturating_sub(rows.len());
    let related_ids = rows
        .iter()
        .map(|row| row.related_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let loaded = semantic::load_artifacts(conn, &related_ids)?;
    let loaded = loaded
        .into_iter()
        .map(|artifact| (artifact.id, artifact))
        .collect::<HashMap<_, _>>();
    let mut related = Vec::new();
    for row in rows {
        let Some(artifact) = loaded.get(&row.related_id) else {
            continue;
        };
        related.push(RelatedArtifact {
            root_artifact_id: row.root_id,
            direction: row.direction,
            relation: row.relation,
            claim_path: row.claim_path,
            confidence: row.confidence,
            pinned_fingerprint: row.pinned_fingerprint,
            artifact: ArtifactHeader {
                id: artifact.id,
                artifact_type: artifact.artifact_type.clone(),
                name: artifact.name.clone(),
                current: row.current,
                confidence: artifact.confidence.clone(),
                freshness: artifact.freshness.clone(),
            },
        });
    }
    Ok((related, omitted))
}

fn source_evidence(
    root: &Path,
    conn: &Connection,
    root_ids: &[i64],
    options: &QueryOptions,
) -> Result<(Vec<SourceEvidence>, usize, usize)> {
    let allowed_origins = options
        .file_origins
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut evidence = Vec::new();
    let mut omitted_by_origin = 0;
    let mut observed: usize = 0;
    for &root_id in root_ids {
        let paths = evidence_paths(conn, root_id)?;
        let ids = paths.keys().copied().collect::<Vec<_>>();
        let artifacts = semantic::load_artifacts(conn, &ids)?;
        for artifact in artifacts {
            let path = paths.get(&artifact.id).cloned().unwrap_or_default();
            for support in artifact.supports {
                observed += 1;
                let Some(file) = evidence_file(conn, &support.evidence_file)? else {
                    if evidence.len() < options.source_limit {
                        evidence.push(unavailable_source(
                            root_id,
                            artifact.id,
                            path.clone(),
                            support,
                            "missing-index-file",
                        ));
                    }
                    continue;
                };
                if !allowed_origins.contains(file.origin.as_str()) {
                    omitted_by_origin += 1;
                    continue;
                }
                if evidence.len() >= options.source_limit {
                    continue;
                }
                evidence.push(render_source_evidence(
                    root,
                    conn,
                    root_id,
                    artifact.id,
                    path.clone(),
                    support,
                    file,
                    options.source_byte_limit,
                ));
            }
        }
    }
    let omitted = observed
        .saturating_sub(omitted_by_origin)
        .saturating_sub(evidence.len());
    Ok((evidence, omitted_by_origin, omitted))
}

fn evidence_paths(conn: &Connection, root_id: i64) -> Result<BTreeMap<i64, Vec<EvidenceHop>>> {
    let mut paths = BTreeMap::from([(root_id, Vec::new())]);
    let mut queue = VecDeque::from([(root_id, 0_usize)]);
    while let Some((artifact_id, depth)) = queue.pop_front() {
        if depth >= EVIDENCE_RELATION_DEPTH {
            continue;
        }
        let mut statement = conn.prepare_cached(
            "SELECT dst_artifact_id, relation, claim_path FROM semantic_relations
             WHERE src_artifact_id=?1
             ORDER BY dst_artifact_id, relation, claim_path",
        )?;
        let rows = statement.query_map([artifact_id], |row| {
            Ok(EvidenceHop {
                artifact_id: row.get(0)?,
                relation: row.get(1)?,
                claim_path: row.get(2)?,
            })
        })?;
        let parent_path = paths.get(&artifact_id).cloned().unwrap_or_default();
        for row in rows {
            let hop = row?;
            let child_id = hop.artifact_id;
            if paths.contains_key(&child_id) {
                continue;
            }
            let mut path = parent_path.clone();
            path.push(hop);
            paths.insert(child_id, path);
            queue.push_back((child_id, depth + 1));
        }
    }
    Ok(paths)
}

struct EvidenceFile {
    id: i64,
    indexed_hash: String,
    role: String,
    origin: String,
}

fn evidence_file(conn: &Connection, path: &str) -> Result<Option<EvidenceFile>> {
    Ok(conn
        .query_row(
            "SELECT id, hash, role, origin FROM files WHERE path=?1",
            [path],
            |row| {
                Ok(EvidenceFile {
                    id: row.get(0)?,
                    indexed_hash: row.get(1)?,
                    role: row.get(2)?,
                    origin: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn unavailable_source(
    root_artifact_id: i64,
    evidence_artifact_id: i64,
    via_relations: Vec<EvidenceHop>,
    support: semantic::SemanticSupport,
    status: &str,
) -> SourceEvidence {
    SourceEvidence {
        root_artifact_id,
        evidence_artifact_id,
        via_artifact_ids: via_relations.iter().map(|hop| hop.artifact_id).collect(),
        via_relations,
        claim_path: support.claim_path,
        relationship: support.relationship,
        anchor: support.anchor,
        file: support.evidence_file,
        file_role: "unknown".into(),
        file_origin: "unknown".into(),
        lines: [support.evidence_start_line, support.evidence_end_line],
        confidence: support.confidence,
        support_freshness: support.freshness,
        source_status: status.into(),
        source: None,
        source_original_bytes: 0,
        source_rendered_bytes: 0,
        source_truncated: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_source_evidence(
    root: &Path,
    conn: &Connection,
    root_artifact_id: i64,
    evidence_artifact_id: i64,
    via_relations: Vec<EvidenceHop>,
    support: semantic::SemanticSupport,
    file: EvidenceFile,
    byte_limit: usize,
) -> SourceEvidence {
    let mut result = SourceEvidence {
        root_artifact_id,
        evidence_artifact_id,
        via_artifact_ids: via_relations.iter().map(|hop| hop.artifact_id).collect(),
        via_relations,
        claim_path: support.claim_path,
        relationship: support.relationship,
        anchor: support.anchor,
        file: support.evidence_file.clone(),
        file_role: file.role,
        file_origin: file.origin,
        lines: [support.evidence_start_line, support.evidence_end_line],
        confidence: support.confidence,
        support_freshness: support.freshness.clone(),
        source_status: support.freshness,
        source: None,
        source_original_bytes: 0,
        source_rendered_bytes: 0,
        source_truncated: false,
    };
    let path = match store::file_source_path(conn, root, file.id) {
        Ok(path) => path,
        Err(_) => {
            result.source_status = "unavailable".into();
            return result;
        }
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(_) => {
            result.source_status = "unavailable".into();
            return result;
        }
    };
    if blake3::hash(source.as_bytes()).to_hex().as_str() != file.indexed_hash {
        result.source_status = "index-stale".into();
        return result;
    }
    let Some(mut excerpt) = line_excerpt(
        &source,
        support.evidence_start_line,
        support.evidence_end_line,
    ) else {
        result.source_status = "invalid-span".into();
        return result;
    };
    result.source_original_bytes = excerpt.len();
    truncate_utf8(&mut excerpt, byte_limit);
    result.source_rendered_bytes = excerpt.len();
    result.source_truncated = result.source_rendered_bytes < result.source_original_bytes;
    // Context drift changes the artifact's structural interpretation, not the
    // source bytes in this exact span. Return hash-verified source and keep the
    // independent support_freshness label visible to the caller.
    result.source_status = "current-source".into();
    result.source = Some(excerpt);
    result
}

fn line_excerpt(source: &str, start_line: i64, end_line: i64) -> Option<String> {
    let start = usize::try_from(start_line.checked_sub(1)?).ok()?;
    let end = usize::try_from(end_line).ok()?;
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    if start >= lines.len() || end > lines.len() || start >= end {
        return None;
    }
    Some(lines[start..end].concat())
}

fn apply_response_budget(result: &mut QueryResult) -> Result<()> {
    let byte_limit = result.response_budget.byte_limit;
    settle_unbudgeted_bytes(result)?;
    while result.response_budget.rendered_bytes > byte_limit {
        result.response_budget.truncated = true;
        let rendered = result.response_budget.rendered_bytes;
        if let Some(source) = result
            .source_evidence
            .iter_mut()
            .filter(|source| source.source.as_ref().is_some_and(|text| !text.is_empty()))
            .max_by_key(|source| source.source_rendered_bytes)
        {
            let overshoot = rendered.saturating_sub(byte_limit);
            let text = source.source.as_mut().expect("filtered source");
            let target = text.len().saturating_sub(overshoot.max(128));
            if target < text.len() {
                truncate_utf8(text, target);
                source.source_rendered_bytes = text.len();
                if !source.source_truncated {
                    source.source_truncated = true;
                    result.response_budget.truncated_sources += 1;
                }
                settle_rendered_bytes(result)?;
                continue;
            }
        }
        if result.related_artifacts.pop().is_some() {
            result.response_budget.omitted_relations += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if result.source_evidence.len() > 1 {
            result.source_evidence.pop();
            result.response_budget.omitted_sources += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if let Some(artifact) = result
            .semantic_artifacts
            .iter_mut()
            .rev()
            .find(|artifact| !artifact.supports.is_empty())
        {
            artifact.supports.pop();
            artifact.supports_truncated = true;
            result.response_budget.omitted_supports += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if result.semantic_artifacts.len() > 1 {
            result.semantic_artifacts.pop();
            result.response_budget.omitted_artifacts += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if result.source_evidence.pop().is_some() {
            result.response_budget.omitted_sources += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        let minimum = settle_rendered_bytes(result)?;
        bail!(
            "response byte limit {byte_limit} is below the minimum semantic response ({minimum} bytes)"
        );
    }
    Ok(())
}

fn settle_unbudgeted_bytes(result: &mut QueryResult) -> Result<()> {
    for _ in 0..8 {
        let rendered = settle_rendered_bytes(result)?;
        if result.response_budget.unbudgeted_bytes == rendered {
            return Ok(());
        }
        result.response_budget.unbudgeted_bytes = rendered;
    }
    settle_rendered_bytes(result)?;
    Ok(())
}

fn settle_rendered_bytes(result: &mut QueryResult) -> Result<usize> {
    for _ in 0..8 {
        let rendered = serde_json::to_string_pretty(result)?.len();
        if result.response_budget.rendered_bytes == rendered {
            return Ok(rendered);
        }
        result.response_budget.rendered_bytes = rendered;
    }
    Ok(serde_json::to_string_pretty(result)?.len())
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::json;

    use super::{QueryOptions, line_excerpt, query};
    use crate::{indexer, semantic, store};

    #[test]
    fn semantic_query_filters_relates_and_drills_to_exact_source() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("flow.ts"),
            "export function finish() { return 1; }\n\
             export function start() { return finish(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let start = crate::structural::resolve_current_anchor(&conn, "flow.ts:start")?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let card = semantic::annotate(
            repo.path(),
            &conn,
            &semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(start.clone()),
                body: json!({ "purpose": "starts the settlement flow" }),
                supports: vec![semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: start,
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot,
                supersedes: None,
            },
        )?;

        let result = query(
            repo.path(),
            &conn,
            &QueryOptions {
                query: "settlement".into(),
                artifact_types: vec!["card".into()],
                include_source: true,
                ..Default::default()
            },
        )?;
        assert_eq!(result.matched_artifacts, 1);
        assert_eq!(result.semantic_artifacts[0].id, card.id);
        assert!(result.semantic_artifacts[0].current);
        assert_eq!(result.semantic_artifacts[0].freshness, "fresh");
        assert_eq!(result.source_evidence.len(), 1);
        assert_eq!(result.source_evidence[0].source_status, "current-source");
        assert!(
            result.source_evidence[0]
                .source
                .as_deref()
                .is_some_and(|source| source.contains("function start"))
        );
        assert!(result.response_budget.rendered_bytes <= 24_000);
        assert_eq!(
            result.response_budget.unbudgeted_bytes,
            result.response_budget.rendered_bytes
        );
        Ok(())
    }

    #[test]
    fn line_excerpt_is_inclusive_and_rejects_invalid_spans() {
        assert_eq!(line_excerpt("a\nb\nc\n", 2, 3).as_deref(), Some("b\nc\n"));
        assert!(line_excerpt("a\n", 0, 1).is_none());
        assert!(line_excerpt("a\n", 2, 2).is_none());
    }
}
