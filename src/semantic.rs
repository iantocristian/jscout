use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{embed, structural};

const MAX_BODY_BYTES: usize = 12_000;
// Generated workflows may carry four evidence spans for each of 31
// candidates, plus workflow-level name/description supports.
const MAX_SUPPORTS: usize = 160;
pub const MAX_WORKFLOW_CANDIDATES: usize = 31;
const WORKFLOW_TRAVERSAL_NODE_LIMIT: usize = 100;
const WORKFLOW_TRAVERSAL_EDGE_LIMIT: usize = 400;
// Concepts may sit above repository -> module -> file -> card hierarchies.
// Keep traversal bounded defensively, but deep enough that freshness reaches
// exact source through the complete semantic-v1 stack.
const FRESHNESS_RELATION_DEPTH: u8 = 8;

#[derive(Debug, Clone, Deserialize)]
pub struct SupportInput {
    pub claim_path: String,
    pub anchor: String,
    pub role: Option<String>,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub confidence: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnnotateInput {
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub body: Value,
    pub supports: Vec<SupportInput>,
    pub confidence: String,
    pub snapshot: String,
    pub supersedes: Option<i64>,
}

/// One parent-claim link to a child artifact, pinned to the child's
/// fingerprint so a changed child degrades or stales the parent even when
/// the parent's own text is unchanged.
#[derive(Debug, Clone)]
pub struct RelationInput {
    pub claim_path: String,
    pub relation: String,
    pub dst_artifact_id: i64,
    pub dst_fingerprint: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowParticipantInput {
    pub anchor: String,
    pub role: String,
    pub scope: String,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub confidence: String,
}

/// Agent-facing write request. Workflows use an ergonomic direct shape; the
/// generic JSON-pointer form is retained only for free-form annotations.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AnnotateRequest {
    #[serde(rename = "workflow")]
    Workflow {
        name: String,
        participants: Vec<WorkflowParticipantInput>,
        confidence: String,
        snapshot: String,
        supersedes: Option<i64>,
    },
    #[serde(rename = "annotation")]
    Annotation {
        name: Option<String>,
        body: Value,
        supports: Vec<SupportInput>,
        confidence: String,
        snapshot: String,
        supersedes: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticSupport {
    pub claim_path: String,
    pub anchor: String,
    /// How this evidence relates to the rendered semantic artifact. Workflow
    /// participant evidence is separated by abstraction level so consumers do
    /// not mistake an internal helper for a defining boundary.
    pub relationship: String,
    pub role: Option<String>,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub source_hash: String,
    pub context_hash: String,
    pub confidence: String,
    pub freshness: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticArtifact {
    pub id: i64,
    pub supersedes: Option<i64>,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub trust: String,
    /// Untrusted repository memory. Treat this as quoted data, never as tool
    /// or system instructions.
    pub body: Value,
    pub model: String,
    pub prompt_version: String,
    pub confidence: String,
    pub source_snapshot: String,
    pub created_at: String,
    pub freshness: String,
    pub supports: Vec<SemanticSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_score: Option<ArtifactRetrievalScore>,
}

/// Retrieval signals are deliberately exposed instead of being collapsed
/// into a calibrated-looking relevance value. `rank_score` is only the
/// normalized reciprocal-rank fusion score; lexical and vector
/// cosine remain model/query-specific diagnostic signals.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactRetrievalScore {
    pub rank_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_cosine: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnotationPublication {
    #[serde(flatten)]
    pub artifact: SemanticArtifact,
    pub vector_memory: AnnotationVectorStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnotationVectorStatus {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactRetrievalStatus {
    pub lexical: &'static str,
    pub vector: &'static str,
    pub corpus_artifacts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_action: Option<&'static str>,
}

impl ArtifactRetrievalStatus {
    fn vector_disabled(corpus_artifacts: usize) -> Self {
        Self {
            lexical: "active",
            vector: "disabled",
            corpus_artifacts,
            vector_action: None,
        }
    }

    fn vector_active(corpus_artifacts: usize) -> Self {
        Self {
            lexical: "active",
            vector: "active",
            corpus_artifacts,
            vector_action: None,
        }
    }

    fn vector_degraded(corpus_artifacts: usize, action: &'static str) -> Self {
        Self {
            lexical: "active",
            vector: "degraded",
            corpus_artifacts,
            vector_action: Some(action),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RankedArtifact {
    pub id: i64,
    pub retrieval_score: ArtifactRetrievalScore,
}

#[derive(Debug, Clone)]
pub struct WorkflowCandidateOptions {
    pub expected_snapshot: Option<String>,
    pub depth: usize,
    pub candidate_limit: usize,
}

impl Default for WorkflowCandidateOptions {
    fn default() -> Self {
        Self {
            expected_snapshot: None,
            depth: 2,
            candidate_limit: MAX_WORKFLOW_CANDIDATES,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCandidate {
    pub anchor: String,
    pub display_name: String,
    pub file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub relevance: f64,
    pub seed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCandidateSet {
    pub snapshot: String,
    pub seeds: Vec<String>,
    pub direction: String,
    pub depth: usize,
    pub min_confidence: String,
    pub candidate_limit: usize,
    pub fingerprint: String,
    pub traversal_truncated: bool,
    pub candidate_truncated: bool,
    pub candidates: Vec<WorkflowCandidate>,
}

pub(crate) struct ValidatedSupport {
    pub(crate) claim_path: String,
    pub(crate) anchor: String,
    pub(crate) role: Option<String>,
    pub(crate) evidence_file: String,
    pub(crate) evidence_start_line: i64,
    pub(crate) evidence_end_line: i64,
    pub(crate) source_hash: String,
    pub(crate) context_hash: String,
    pub(crate) confidence: String,
}

pub fn workflow_candidates(
    root: &Path,
    conn: &Connection,
    seeds: &[String],
    options: &WorkflowCandidateOptions,
) -> Result<WorkflowCandidateSet> {
    if seeds.is_empty() {
        bail!("workflow candidates require at least one seed anchor");
    }
    if options.depth == 0 || options.depth > 3 {
        bail!("workflow candidate depth must be between 1 and 3");
    }
    if options.candidate_limit == 0 || options.candidate_limit > MAX_WORKFLOW_CANDIDATES {
        bail!("workflow candidate limit must be between 1 and {MAX_WORKFLOW_CANDIDATES}");
    }
    let snapshot = structural::current_snapshot(conn)?;
    if options
        .expected_snapshot
        .as_deref()
        .is_some_and(|value| value != snapshot)
    {
        bail!("workflow candidate snapshot is stale; current snapshot is {snapshot}");
    }
    let mut resolved_seeds = seeds
        .iter()
        .map(|seed| {
            structural::resolve_current_anchor_in_origins(conn, seed, &crate::origin::defaults())
        })
        .collect::<Result<Vec<_>>>()?;
    resolved_seeds.sort();
    resolved_seeds.dedup();

    let mut nodes: HashMap<String, (structural::GraphNode, bool)> = HashMap::new();
    let mut traversal_truncated = false;
    for seed in &resolved_seeds {
        let neighborhood = structural::workflow_neighborhood(
            conn,
            seed,
            options.depth,
            WORKFLOW_TRAVERSAL_NODE_LIMIT,
            WORKFLOW_TRAVERSAL_EDGE_LIMIT,
            &crate::origin::defaults(),
        )?;
        traversal_truncated |= neighborhood.truncated;
        for node in neighborhood.nodes {
            if node.kind != "symbol"
                || !crate::recon::effective_runtime(
                    conn,
                    node.file.as_deref(),
                    node.file_role.as_deref(),
                )?
            {
                continue;
            }
            let is_seed = resolved_seeds.contains(&node.key);
            nodes
                .entry(node.key.clone())
                .and_modify(|(existing, existing_seed)| {
                    existing.relevance = existing.relevance.max(node.relevance);
                    *existing_seed |= is_seed;
                })
                .or_insert((node, is_seed));
        }
    }
    let mut candidates = nodes
        .into_values()
        .map(|(node, seed)| workflow_candidate(root, conn, node, seed))
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| {
        right
            .seed
            .cmp(&left.seed)
            .then_with(|| right.relevance.total_cmp(&left.relevance))
            .then_with(|| left.anchor.cmp(&right.anchor))
    });
    let candidate_truncated = candidates.len() > options.candidate_limit;
    candidates.truncate(options.candidate_limit);
    let fingerprint = workflow_candidate_fingerprint(
        &snapshot,
        &resolved_seeds,
        options.depth,
        options.candidate_limit,
        &candidates,
    );
    Ok(WorkflowCandidateSet {
        snapshot,
        seeds: resolved_seeds,
        direction: "both".into(),
        depth: options.depth,
        min_confidence: "likely".into(),
        candidate_limit: options.candidate_limit,
        fingerprint,
        traversal_truncated,
        candidate_truncated,
        candidates,
    })
}

fn workflow_candidate(
    root: &Path,
    conn: &Connection,
    node: structural::GraphNode,
    seed: bool,
) -> Result<WorkflowCandidate> {
    let (file, start_line, declaration_end): (String, i64, i64) = conn.query_row(
        "SELECT f.path, s.line, s.decl_end
         FROM graph_nodes g
         JOIN symbols s ON g.native_table='symbols' AND g.native_id=s.id
         JOIN files f ON f.id=s.file_id
         WHERE g.node_key=?1",
        [&node.key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let source = std::fs::read_to_string(root.join(&file))
        .with_context(|| format!("read workflow candidate file `{file}`"))?;
    Ok(WorkflowCandidate {
        anchor: node.key,
        display_name: node.display_name,
        file,
        evidence_start_line: start_line,
        evidence_end_line: declaration_end_line(&source, start_line, declaration_end),
        relevance: node.relevance,
        seed,
    })
}

/// The declaration span of one anchor, resolved exactly as a workflow
/// candidate. Card planning needs the same file and line arithmetic for a
/// single symbol without a traversal. `None` means the anchor is not a
/// file-backed symbol in the current snapshot.
pub(crate) fn symbol_candidate(
    root: &Path,
    conn: &Connection,
    anchor: &str,
) -> Result<Option<WorkflowCandidate>> {
    let row: Option<(String, String, i64, i64)> = conn
        .query_row(
            "SELECT g.display_name, f.path, s.line, s.decl_end
             FROM graph_nodes g
             JOIN symbols s ON g.native_table='symbols' AND g.native_id=s.id
             JOIN files f ON f.id=s.file_id
             WHERE g.node_key=?1",
            [anchor],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((display_name, file, start_line, declaration_end)) = row else {
        return Ok(None);
    };
    let source = std::fs::read_to_string(root.join(&file))
        .with_context(|| format!("read card subject file `{file}`"))?;
    Ok(Some(WorkflowCandidate {
        anchor: anchor.to_string(),
        display_name,
        file,
        evidence_start_line: start_line,
        evidence_end_line: declaration_end_line(&source, start_line, declaration_end),
        relevance: 1.0,
        seed: false,
    }))
}

fn declaration_end_line(source: &str, start_line: i64, declaration_end: i64) -> i64 {
    let declaration_end = usize::try_from(declaration_end)
        .unwrap_or(source.len())
        .min(source.len());
    source
        .get(..declaration_end)
        .map_or(start_line, |prefix| prefix.lines().count() as i64)
        .max(start_line)
}

fn workflow_candidate_fingerprint(
    snapshot: &str,
    seeds: &[String],
    depth: usize,
    candidate_limit: usize,
    candidates: &[WorkflowCandidate],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-workflow-candidates-v1\0");
    for value in [snapshot, "both", "likely"] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(depth.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(candidate_limit.to_string().as_bytes());
    for seed in seeds {
        hasher.update(b"\0seed\0");
        hasher.update(seed.as_bytes());
    }
    for candidate in candidates {
        hasher.update(b"\0candidate\0");
        hasher.update(candidate.anchor.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub fn annotate_request(
    root: &Path,
    conn: &Connection,
    request: AnnotateRequest,
) -> Result<SemanticArtifact> {
    let input = match request {
        AnnotateRequest::Workflow {
            name,
            participants,
            confidence,
            snapshot,
            supersedes,
        } => workflow_request(name, None, participants, confidence, snapshot, supersedes)?,
        AnnotateRequest::Annotation {
            name,
            body,
            supports,
            confidence,
            snapshot,
            supersedes,
        } => AnnotateInput {
            artifact_type: "annotation".into(),
            name,
            body,
            supports,
            confidence,
            snapshot,
            supersedes,
        },
    };
    annotate(root, conn, &input)
}

/// Publish one interactive agent write and, only when this provider's
/// semantic vector index was healthy before the write, top it up immediately.
/// The annotation itself remains successful if inference later fails: retrying
/// the write would create duplicate memory, while the returned status and the
/// next retrieval both expose the degraded vector plane.
pub fn annotate_request_with_provider(
    root: &Path,
    conn: &Connection,
    provider: Option<&embed::Provider>,
    request: AnnotateRequest,
) -> Result<AnnotationPublication> {
    let ready_before_write = provider.map(|provider| {
        embed::semantic_vector_index_ready(conn, provider).map_err(|error| {
            eprintln!("semantic vector preflight unavailable: {error}");
            error
        })
    });
    let artifact = annotate_request(root, conn, request)?;
    let vector_memory = match (provider, ready_before_write) {
        (None, _) => AnnotationVectorStatus {
            status: "disabled",
            action: None,
        },
        (Some(_), Some(Ok(false))) => AnnotationVectorStatus {
            status: "degraded",
            action: Some("run jscout embed <root> --semantic-only"),
        },
        (Some(_), Some(Err(ref error))) => AnnotationVectorStatus {
            status: "degraded",
            action: Some(embed::semantic_vector_failure_action(error)),
        },
        (Some(provider), Some(Ok(true))) => {
            match embed::embed_semantic_missing(conn, provider, 16) {
                Ok(_) => AnnotationVectorStatus {
                    status: "active",
                    action: None,
                },
                Err(error) => {
                    eprintln!("semantic vector refresh after annotation failed: {error}");
                    AnnotationVectorStatus {
                        status: "degraded",
                        action: Some(
                            "repair the configured embedding service, then run jscout embed <root> --semantic-only",
                        ),
                    }
                }
            }
        }
        (Some(_), None) => unreachable!("provider presence determines preflight presence"),
    };
    Ok(AnnotationPublication {
        artifact,
        vector_memory,
    })
}

pub(crate) fn workflow_request(
    name: String,
    description: Option<String>,
    participants: Vec<WorkflowParticipantInput>,
    confidence: String,
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<AnnotateInput> {
    let name_evidence = participants
        .iter()
        .find(|participant| participant.scope == "defining")
        .context("workflow request requires at least one defining participant")?;
    let participant_body: Vec<Value> = participants
        .iter()
        .map(|participant| {
            serde_json::json!({
                "anchor": participant.anchor,
                "role": participant.role,
                "scope": participant.scope,
            })
        })
        .collect();
    let support = |claim_path: String, participant: &WorkflowParticipantInput| SupportInput {
        claim_path,
        anchor: participant.anchor.clone(),
        role: None,
        evidence_file: participant.evidence_file.clone(),
        evidence_start_line: participant.evidence_start_line,
        evidence_end_line: participant.evidence_end_line,
        confidence: participant.confidence.clone(),
    };
    let mut supports = Vec::with_capacity(participants.len() + 2);
    supports.push(support("/name".into(), name_evidence));
    let body = match &description {
        Some(text) => {
            supports.push(support("/description".into(), name_evidence));
            serde_json::json!({ "description": text, "participants": participant_body })
        }
        None => serde_json::json!({ "participants": participant_body }),
    };
    for (index, participant) in participants.iter().enumerate() {
        supports.push(support(format!("/participants/{index}/role"), participant));
    }
    Ok(AnnotateInput {
        artifact_type: "workflow".into(),
        name: Some(name),
        body,
        supports,
        confidence,
        snapshot,
        supersedes,
    })
}

pub fn annotate(root: &Path, conn: &Connection, input: &AnnotateInput) -> Result<SemanticArtifact> {
    let (snapshot, supports) = validate_annotate_input(root, conn, input)?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let inserted = persist_validated_artifact(
        conn,
        input,
        &snapshot,
        &supports,
        &[],
        &ArtifactProvenance {
            model: "agent-reported",
            prompt_version: "annotate/v2",
            scout_run_id: None,
            input_fingerprint: None,
        },
    );
    let artifact_id = match inserted {
        Ok(id) => {
            conn.execute_batch("COMMIT")?;
            id
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    };
    load_artifact(conn, artifact_id)?.context("inserted semantic artifact is missing")
}

/// Generated-artifact provenance. Agent annotations carry no run identity;
/// scouted artifacts must supply both run id and input fingerprint.
pub(crate) struct ArtifactProvenance<'a> {
    pub(crate) model: &'a str,
    pub(crate) prompt_version: &'a str,
    pub(crate) scout_run_id: Option<i64>,
    pub(crate) input_fingerprint: Option<&'a str>,
}

/// Shared validation for every artifact write path: body/support shape,
/// snapshot currency, supersession rules, exact current anchors, on-disk
/// file-hash currency, and span bounds.
pub(crate) fn validate_annotate_input(
    root: &Path,
    conn: &Connection,
    input: &AnnotateInput,
) -> Result<(String, Vec<ValidatedSupport>)> {
    validate_input(input)?;
    let snapshot = structural::current_snapshot(conn)?;
    if input.snapshot != snapshot {
        bail!("annotation snapshot is stale; current snapshot is {snapshot}");
    }
    if let Some(supersedes) = input.supersedes {
        let prior_type: Option<String> = conn
            .query_row(
                "SELECT artifact_type FROM semantic_artifacts WHERE id=?1",
                [supersedes],
                |row| row.get(0),
            )
            .optional()?;
        let Some(prior_type) = prior_type else {
            bail!("superseded semantic artifact {supersedes} does not exist");
        };
        if prior_type != input.artifact_type {
            bail!("a semantic artifact may only supersede the same artifact type");
        }
        let already_superseded: i64 = conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM semantic_artifacts WHERE supersedes_artifact_id=?1
             )",
            [supersedes],
            |row| row.get(0),
        )?;
        if already_superseded != 0 {
            bail!("semantic artifact {supersedes} was already superseded");
        }
    }

    let mut seen = HashSet::new();
    let mut supports = Vec::with_capacity(input.supports.len());
    for support in &input.supports {
        validate_confidence(&support.confidence)?;
        if confidence_rank(&support.confidence) < confidence_rank(&input.confidence) {
            bail!(
                "artifact confidence {} exceeds support confidence {}",
                input.confidence,
                support.confidence
            );
        }
        if support.claim_path != "/name" && input.body.pointer(&support.claim_path).is_none() {
            bail!(
                "support claim_path `{}` does not exist in body",
                support.claim_path
            );
        }
        let anchor = structural::resolve_current_anchor(conn, &support.anchor)?;
        if anchor != support.anchor {
            bail!("semantic supports require exact current anchors; use `{anchor}`");
        }
        let anchor_file: Option<String> = conn
            .query_row(
                "SELECT f.path FROM graph_nodes g LEFT JOIN files f ON g.file_id=f.id
                 WHERE g.node_key=?1",
                [&anchor],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let anchor_file =
            anchor_file.with_context(|| format!("support anchor `{anchor}` is not file-backed"))?;
        if anchor_file != support.evidence_file {
            bail!(
                "support anchor `{anchor}` belongs to `{anchor_file}`, not `{}`",
                support.evidence_file
            );
        }
        if support.evidence_start_line <= 0
            || support.evidence_end_line < support.evidence_start_line
        {
            bail!("support evidence lines must be a positive ordered span");
        }
        let source_hash: String = conn
            .query_row(
                "SELECT hash FROM files WHERE path=?1",
                [&support.evidence_file],
                |row| row.get(0),
            )
            .with_context(|| format!("support file `{}` is not indexed", support.evidence_file))?;
        let source = std::fs::read_to_string(root.join(&support.evidence_file))
            .with_context(|| format!("read support file `{}`", support.evidence_file))?;
        if blake3::hash(source.as_bytes()).to_hex().as_str() != source_hash {
            bail!(
                "support file `{}` changed since indexing",
                support.evidence_file
            );
        }
        let line_count = source.lines().count() as i64;
        if support.evidence_end_line > line_count {
            bail!(
                "support span {}-{} exceeds `{}` line count {line_count}",
                support.evidence_start_line,
                support.evidence_end_line,
                support.evidence_file
            );
        }
        let identity = (
            support.claim_path.clone(),
            anchor.clone(),
            support.evidence_file.clone(),
            support.evidence_start_line,
            support.evidence_end_line,
        );
        if !seen.insert(identity) {
            bail!("duplicate semantic support for `{}`", support.claim_path);
        }
        let role = if support.claim_path.starts_with("/participants/")
            && support.claim_path.ends_with("/role")
        {
            input
                .body
                .pointer(&support.claim_path)
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            support.role.clone()
        };
        supports.push(ValidatedSupport {
            claim_path: support.claim_path.clone(),
            context_hash: context_hash(conn, &anchor)?,
            anchor,
            role,
            evidence_file: support.evidence_file.clone(),
            evidence_start_line: support.evidence_start_line,
            evidence_end_line: support.evidence_end_line,
            source_hash,
            confidence: support.confidence.clone(),
        });
    }

    Ok((snapshot, supports))
}

/// Insert the artifact, its fingerprint, and its supports. No transaction
/// control: callers own BEGIN/COMMIT so publication can be atomic with the
/// run ledger.
pub(crate) fn persist_validated_artifact(
    conn: &Connection,
    input: &AnnotateInput,
    snapshot: &str,
    supports: &[ValidatedSupport],
    relations: &[RelationInput],
    provenance: &ArtifactProvenance<'_>,
) -> Result<i64> {
    validate_relation_contract(conn, input, relations)?;
    let body_json = serde_json::to_string(&input.body)?;
    let mut support_rows: Vec<Vec<String>> = supports
        .iter()
        .map(|support| {
            vec![
                support.claim_path.clone(),
                support.anchor.clone(),
                support.role.clone().unwrap_or_default(),
                support.evidence_file.clone(),
                support.evidence_start_line.to_string(),
                support.evidence_end_line.to_string(),
                support.source_hash.clone(),
                support.context_hash.clone(),
                support.confidence.clone(),
            ]
        })
        .collect();
    // Relations are evidence identity for parent artifacts: a summary's
    // fingerprint must change when its view of any child changes.
    support_rows.extend(relations.iter().map(|relation| {
        vec![
            format!("relation:{}", relation.relation),
            relation.claim_path.clone(),
            relation.dst_fingerprint.clone(),
            relation.confidence.clone(),
        ]
    }));
    let artifact_fingerprint = crate::store::artifact_fingerprint(
        &crate::store::ArtifactIdentity {
            artifact_type: &input.artifact_type,
            canonical_name: input.name.as_deref(),
            body_json: &body_json,
            model: provenance.model,
            prompt_version: provenance.prompt_version,
            confidence: &input.confidence,
            source_snapshot: snapshot,
        },
        &mut support_rows,
    );
    (|| -> Result<i64> {
        conn.execute(
            "INSERT INTO semantic_artifacts(
               supersedes_artifact_id, artifact_type, canonical_name, body_json,
               model, prompt_version, confidence, source_snapshot, created_at,
               scout_run_id, input_fingerprint, artifact_fingerprint
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,
                      strftime('%Y-%m-%dT%H:%M:%fZ','now'),?9,?10,?11)",
            params![
                input.supersedes,
                input.artifact_type,
                input.name,
                body_json,
                provenance.model,
                provenance.prompt_version,
                input.confidence,
                snapshot,
                provenance.scout_run_id,
                provenance.input_fingerprint,
                artifact_fingerprint,
            ],
        )?;
        let artifact_id = conn.last_insert_rowid();
        crate::embed::mark_semantic_vector_index_stale(conn)?;
        let mut statement = conn.prepare_cached(
            "INSERT INTO semantic_supports(
               artifact_id, claim_path, anchor_key, role, evidence_file,
               evidence_start_line, evidence_end_line, source_hash, context_hash, confidence
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?;
        for support in supports {
            statement.execute(params![
                artifact_id,
                support.claim_path,
                support.anchor,
                support.role,
                support.evidence_file,
                support.evidence_start_line,
                support.evidence_end_line,
                support.source_hash,
                support.context_hash,
                support.confidence,
            ])?;
        }
        let mut relation_statement = conn.prepare_cached(
            "INSERT INTO semantic_relations(
               src_artifact_id, dst_artifact_id, relation, claim_path,
               confidence, dst_fingerprint
             ) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        for relation in relations {
            relation_statement.execute(params![
                artifact_id,
                relation.dst_artifact_id,
                relation.relation,
                relation.claim_path,
                relation.confidence,
                relation.dst_fingerprint,
            ])?;
        }
        Ok(artifact_id)
    })()
}

fn validate_relation_contract(
    conn: &Connection,
    input: &AnnotateInput,
    relations: &[RelationInput],
) -> Result<()> {
    for relation in relations {
        validate_confidence(&relation.confidence)?;
        if confidence_rank(&relation.confidence) < confidence_rank(&input.confidence) {
            bail!(
                "artifact confidence {} exceeds relation confidence {}",
                input.confidence,
                relation.confidence
            );
        }
        if !relation.claim_path.is_empty() && input.body.pointer(&relation.claim_path).is_none() {
            bail!(
                "relation claim_path `{}` does not exist in body",
                relation.claim_path
            );
        }
        let child: Option<(Option<String>, String)> = conn
            .query_row(
                "SELECT artifact_fingerprint, confidence FROM semantic_artifacts WHERE id=?1",
                [relation.dst_artifact_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((fingerprint, child_confidence)) = child else {
            bail!(
                "semantic relation target {} does not exist",
                relation.dst_artifact_id
            );
        };
        if fingerprint.as_deref() != Some(relation.dst_fingerprint.as_str()) {
            bail!(
                "semantic relation target {} fingerprint changed",
                relation.dst_artifact_id
            );
        }
        if confidence_rank(&relation.confidence) > confidence_rank(&child_confidence) {
            bail!(
                "relation confidence {} exceeds child artifact confidence {}",
                relation.confidence,
                child_confidence
            );
        }
    }
    // Generated concepts are relation-backed and carry no direct supports.
    // Retain read/write compatibility with legacy direct-support concept rows,
    // but never allow a new relation-backed row with an uncovered claim.
    if input.artifact_type == "concept" && input.supports.is_empty() {
        let mut required_claims = Vec::new();
        collect_claim_paths(&input.body, "", &mut required_claims);
        for claim_path in required_claims {
            if !relations.iter().any(|relation| {
                relation.relation == "related_to" && relation.claim_path == claim_path
            }) {
                bail!("concept claim `{claim_path}` requires a fingerprinted child relation");
            }
        }
        if !relations
            .iter()
            .any(|relation| relation.relation == "related_to" && relation.claim_path.is_empty())
        {
            bail!("concept publication requires whole-input child dependencies");
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ArtifactRankingCandidate {
    id: i64,
    artifact_type: String,
    name: Option<String>,
    body_json: String,
}

pub(crate) fn rank_artifacts(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    query: &str,
    include_superseded: bool,
    vector_limit: usize,
) -> Result<(Vec<RankedArtifact>, ArtifactRetrievalStatus)> {
    let mut statement = conn.prepare(
        "SELECT artifact.id, artifact.artifact_type, artifact.canonical_name,
                artifact.body_json
         FROM semantic_artifacts artifact
         WHERE artifact.artifact_type!='design'
           AND (?1 OR NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           ))
         ORDER BY artifact.id DESC",
    )?;
    let rows = statement
        .query_map([include_superseded], |row| {
            Ok(ArtifactRankingCandidate {
                id: row.get(0)?,
                artifact_type: row.get(1)?,
                name: row.get(2)?,
                body_json: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let corpus_artifacts = rows.len();
    if query.trim().is_empty() {
        return Ok((
            rows.into_iter()
                .map(|candidate| RankedArtifact {
                    id: candidate.id,
                    retrieval_score: ArtifactRetrievalScore {
                        rank_score: 0.0,
                        lexical_score: None,
                        vector_cosine: None,
                    },
                })
                .collect(),
            ArtifactRetrievalStatus::vector_disabled(corpus_artifacts),
        ));
    }

    let raw_tokens = semantic_tokens(&query.to_lowercase());
    let normalized_concept_query = crate::scouting::concept::normalize(query);
    let concept_tokens = semantic_tokens(&normalized_concept_query);
    let mut lexical = rows
        .iter()
        .filter_map(|candidate| {
            lexical_artifact_relevance(
                candidate,
                query,
                &raw_tokens,
                &normalized_concept_query,
                &concept_tokens,
            )
            .map(|score| (candidate.id, score))
        })
        .collect::<Vec<_>>();
    lexical.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.0.cmp(&left.0))
    });

    let lexical_scores = lexical.iter().copied().collect::<HashMap<_, _>>();
    let mut retrieval = ArtifactRetrievalStatus::vector_disabled(corpus_artifacts);
    let vector = if let Some(provider) = provider {
        match embed::semantic_vector_search(conn, provider, query, vector_limit.clamp(100, 1_000)) {
            Ok(ranking) => {
                retrieval = ArtifactRetrievalStatus::vector_active(corpus_artifacts);
                ranking
            }
            Err(error) => {
                retrieval = ArtifactRetrievalStatus::vector_degraded(
                    corpus_artifacts,
                    embed::semantic_vector_failure_action(&error),
                );
                eprintln!("semantic vector retrieval unavailable, using lexical order: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let vector_scores = vector.iter().copied().collect::<HashMap<_, _>>();

    let allowed = rows
        .iter()
        .map(|candidate| candidate.id)
        .collect::<HashSet<_>>();
    let rankings = [lexical, vector]
        .into_iter()
        .map(|ranking| {
            ranking
                .into_iter()
                .filter(|(id, _)| allowed.contains(id))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut fused = HashMap::<i64, f64>::new();
    for ranking in rankings {
        for (rank, (id, _)) in ranking.into_iter().enumerate() {
            *fused.entry(id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
        }
    }
    let maximum = fused.values().copied().fold(0.0_f64, f64::max);
    let mut ranked = fused
        .into_iter()
        .map(|(id, score)| RankedArtifact {
            id,
            retrieval_score: ArtifactRetrievalScore {
                rank_score: if maximum > 0.0 { score / maximum } else { 0.0 },
                lexical_score: lexical_scores.get(&id).copied(),
                vector_cosine: vector_scores.get(&id).copied(),
            },
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .retrieval_score
            .rank_score
            .total_cmp(&left.retrieval_score.rank_score)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok((ranked, retrieval))
}

fn lexical_artifact_relevance(
    candidate: &ArtifactRankingCandidate,
    query: &str,
    raw_tokens: &[String],
    normalized_concept_query: &str,
    concept_tokens: &[String],
) -> Option<f64> {
    let (name, body, exact_name, tokens) = if candidate.artifact_type == "concept" {
        let name =
            crate::scouting::concept::normalize(candidate.name.as_deref().unwrap_or_default());
        let body = serde_json::from_str::<Value>(&candidate.body_json)
            .ok()
            .map(|body| {
                let definition = body
                    .get("definition")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let aliases = body
                    .get("aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                crate::scouting::concept::normalize(&format!("{definition} {aliases}"))
            })
            .unwrap_or_else(|| crate::scouting::concept::normalize(&candidate.body_json));
        let exact = !name.is_empty() && normalized_concept_query == name;
        (name, body, exact, concept_tokens)
    } else {
        let name = candidate.name.as_deref().unwrap_or_default().to_lowercase();
        let exact = !name.is_empty() && query.eq_ignore_ascii_case(&name);
        (name, candidate.body_json.to_lowercase(), exact, raw_tokens)
    };
    if tokens.is_empty() {
        return exact_name.then_some(1.0);
    }
    let matches = tokens
        .iter()
        .filter(|token| name.contains(token.as_str()) || body.contains(token.as_str()))
        .count();
    if matches == 0 {
        return None;
    }
    Some(((matches + usize::from(exact_name) * 4) as f64 / tokens.len() as f64).min(1.0))
}

fn semantic_tokens(query: &str) -> Vec<String> {
    query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '$'
        })
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

pub fn search_with_provider(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    query: &str,
    limit: usize,
) -> Result<(Vec<SemanticArtifact>, ArtifactRetrievalStatus, usize)> {
    if limit == 0 {
        let corpus_artifacts = conn.query_row(
            "SELECT count(*) FROM semantic_artifacts artifact
             WHERE NOT EXISTS(
               SELECT 1 FROM semantic_artifacts successor
               WHERE successor.supersedes_artifact_id=artifact.id
             )",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        return Ok((
            Vec::new(),
            ArtifactRetrievalStatus::vector_disabled(corpus_artifacts),
            0,
        ));
    }
    let (mut ranked, retrieval) = rank_artifacts(conn, provider, query, false, limit * 5)?;
    let candidate_pool = ranked.len();
    ranked.truncate(limit);
    let ids = ranked
        .iter()
        .map(|artifact| artifact.id)
        .collect::<Vec<_>>();
    let retrieval_scores = ranked
        .into_iter()
        .map(|artifact| (artifact.id, artifact.retrieval_score))
        .collect::<HashMap<_, _>>();
    let mut artifacts = load_artifacts(conn, &ids)?;
    for artifact in &mut artifacts {
        artifact.retrieval_score = retrieval_scores.get(&artifact.id).cloned();
    }
    Ok((artifacts, retrieval, candidate_pool))
}

#[cfg(test)]
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SemanticArtifact>> {
    search_with_provider(conn, None, query, limit).map(|(artifacts, _, _)| artifacts)
}

pub(crate) fn load_artifact(conn: &Connection, id: i64) -> Result<Option<SemanticArtifact>> {
    load_artifact_at_depth(
        conn,
        id,
        FRESHNESS_RELATION_DEPTH,
        &mut FreshnessContext::default(),
    )
}

/// Load several artifacts against one freshness context. Semantic retrieval
/// commonly returns parents that share children; carrying one memo across the
/// batch avoids recomputing the same hierarchy for every result while keeping
/// the complete query inside its caller's SQLite read snapshot.
pub(crate) fn load_artifacts(conn: &Connection, ids: &[i64]) -> Result<Vec<SemanticArtifact>> {
    let mut context = FreshnessContext::default();
    let mut artifacts = Vec::with_capacity(ids.len());
    for &id in ids {
        if let Some(artifact) =
            load_artifact_at_depth(conn, id, FRESHNESS_RELATION_DEPTH, &mut context)?
        {
            context.memo.insert(id, artifact.freshness.clone());
            artifacts.push(artifact);
        }
    }
    Ok(artifacts)
}

/// Shared state for one top-level artifact load: memoized child freshness
/// plus the lazily built workspace ownership table needed to recompute a
/// summary scope's expected child set.
#[derive(Default)]
struct FreshnessContext {
    memo: HashMap<i64, String>,
    packages: Option<Vec<(String, String)>>,
    concept_children: Option<BTreeMap<String, std::collections::BTreeSet<i64>>>,
}

impl FreshnessContext {
    fn packages(&mut self, conn: &Connection) -> Result<&[(String, String)]> {
        if self.packages.is_none() {
            let root: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
                    row.get(0)
                })
                .optional()?;
            let root = root.context(
                "summary freshness requires the repository root recorded by `jscout index`",
            )?;
            let root = std::path::Path::new(&root);
            if !root.is_dir() {
                bail!(
                    "summary freshness cannot resolve recorded repository root `{}`; \
                     re-index this checkout or query with its original index",
                    root.display()
                );
            }
            self.packages = Some(crate::scouting::plan::package_prefixes(root));
        }
        Ok(self.packages.as_deref().expect("just populated"))
    }

    fn concept_children(
        &mut self,
        conn: &Connection,
    ) -> Result<&BTreeMap<String, std::collections::BTreeSet<i64>>> {
        if self.concept_children.is_none() {
            self.concept_children = Some(current_concept_children(conn)?);
        }
        Ok(self.concept_children.as_ref().expect("just populated"))
    }
}

/// The deterministic child-artifact set a summary scope would be planned
/// over right now. Any addition, removal, or replacement relative to the
/// summary's stored input dependencies must stale it: children arrive
/// incrementally under call budgets, and a summary that silently misses a
/// new child is wrong even though every pinned child is unchanged.
pub(crate) fn expected_summary_child_ids(
    conn: &Connection,
    packages: &[(String, String)],
    level: &str,
    scope_key: &str,
) -> Result<std::collections::BTreeSet<i64>> {
    let mut expected = std::collections::BTreeSet::new();
    match level {
        "file" => {
            let file = scope_key.strip_prefix("file:").unwrap_or(scope_key);
            let mut statement = conn.prepare_cached(
                "SELECT DISTINCT artifact.id FROM semantic_artifacts artifact
                 JOIN semantic_supports support ON support.artifact_id=artifact.id
                 WHERE artifact.artifact_type IN ('card','workflow')
                   AND support.evidence_file=?1
                   AND artifact.artifact_fingerprint IS NOT NULL
                   AND NOT EXISTS(
                     SELECT 1 FROM semantic_artifacts successor
                     WHERE successor.supersedes_artifact_id=artifact.id
                   )",
            )?;
            let rows = statement.query_map([file], |row| row.get::<_, i64>(0))?;
            for row in rows {
                expected.insert(row?);
            }
        }
        "module" | "repository" => {
            let mut statement = conn.prepare_cached(
                "SELECT artifact.id, artifact.canonical_name FROM semantic_artifacts artifact
                 WHERE artifact.artifact_type='summary'
                   AND artifact.artifact_fingerprint IS NOT NULL
                   AND NOT EXISTS(
                     SELECT 1 FROM semantic_artifacts successor
                     WHERE successor.supersedes_artifact_id=artifact.id
                   )",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (id, name) = row?;
                let Some(name) = name else { continue };
                if level == "module" {
                    let package = scope_key.strip_prefix("module:").unwrap_or(scope_key);
                    if let Some(file) = name.strip_prefix("file:")
                        && crate::scouting::plan::owning_package(packages, file) == Some(package)
                    {
                        expected.insert(id);
                    }
                } else if name.starts_with("module:") {
                    expected.insert(id);
                } else if let Some(file) = name.strip_prefix("file:")
                    && crate::scouting::plan::owning_package(packages, file).is_none()
                {
                    expected.insert(id);
                }
            }
        }
        _ => {}
    }
    Ok(expected)
}

/// True when a summary's stored input dependencies (empty claim path) match
/// the scope's current expected child set exactly.
pub(crate) fn summary_child_set_current(
    conn: &Connection,
    packages: &[(String, String)],
    artifact_id: i64,
    level: &str,
    scope_key: &str,
) -> Result<bool> {
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT dst_artifact_id FROM semantic_relations
         WHERE src_artifact_id=?1 AND claim_path=''",
    )?;
    let rows = statement.query_map([artifact_id], |row| row.get::<_, i64>(0))?;
    let stored = rows.collect::<std::result::Result<std::collections::BTreeSet<i64>, _>>()?;
    Ok(stored == expected_summary_child_ids(conn, packages, level, scope_key)?)
}

/// Current workflow/card artifacts that establish one exact normalized
/// concept vocabulary key. Workflow names and card domain terms are the only
/// admitted vocabulary sources; free-form descriptions and card prose never
/// become concept inputs implicitly.
pub(crate) fn expected_concept_child_ids(
    conn: &Connection,
    normalized_key: &str,
) -> Result<std::collections::BTreeSet<i64>> {
    Ok(current_concept_children(conn)?
        .remove(normalized_key)
        .unwrap_or_default())
}

/// Build every current exact vocabulary group once. Batch semantic retrieval
/// may load many concepts; caching this map in `FreshnessContext` avoids
/// reparsing all card bodies independently for each concept.
fn current_concept_children(
    conn: &Connection,
) -> Result<BTreeMap<String, std::collections::BTreeSet<i64>>> {
    let mut expected: BTreeMap<String, std::collections::BTreeSet<i64>> = BTreeMap::new();

    let mut workflows = conn.prepare_cached(
        "SELECT artifact.id, artifact.canonical_name
         FROM semantic_artifacts artifact
         WHERE artifact.artifact_type='workflow'
           AND artifact.artifact_fingerprint IS NOT NULL
           AND EXISTS(
             SELECT 1 FROM semantic_supports support
             WHERE support.artifact_id=artifact.id AND support.claim_path='/name'
           )
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           )",
    )?;
    let rows = workflows.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, name) = row?;
        if let Some(name) = name {
            let normalized = crate::scouting::concept::normalize(&name);
            if !normalized.is_empty() {
                expected.entry(normalized).or_default().insert(id);
            }
        }
    }

    let mut cards = conn.prepare_cached(
        "SELECT DISTINCT artifact.id, artifact.body_json, support.claim_path
         FROM semantic_artifacts artifact
         JOIN semantic_supports support ON support.artifact_id=artifact.id
         WHERE artifact.artifact_type='card'
           AND artifact.artifact_fingerprint IS NOT NULL
           AND support.claim_path LIKE '/domain_terms/%'
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           )",
    )?;
    let rows = cards.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, body_json, claim_path) = row?;
        let body: Value = serde_json::from_str(&body_json)
            .with_context(|| format!("semantic card {id} has invalid body JSON"))?;
        let Some(index) = concept_domain_term_index(&claim_path) else {
            continue;
        };
        let Some(term) = body
            .get("domain_terms")
            .and_then(Value::as_array)
            .and_then(|terms| terms.get(index))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let normalized = crate::scouting::concept::normalize(term);
        if !normalized.is_empty() {
            expected.entry(normalized).or_default().insert(id);
        }
    }
    Ok(expected)
}

/// Only canonical array-element pointers emitted by the card contract are
/// admitted. Prefix-like or nested paths must not widen concept vocabulary.
fn concept_domain_term_index(claim_path: &str) -> Option<usize> {
    let index = claim_path.strip_prefix("/domain_terms/")?;
    if index.is_empty() || index.contains('/') {
        return None;
    }
    let parsed = index.parse::<usize>().ok()?;
    (parsed.to_string() == index).then_some(parsed)
}

/// True when a concept's whole-input dependency rows cover the complete
/// current exact vocabulary group. This detects a matching workflow/card
/// published after the concept without conflating near-duplicate terms.
#[cfg(test)]
pub(crate) fn concept_child_set_current(
    conn: &Connection,
    artifact_id: i64,
    normalized_key: &str,
) -> Result<bool> {
    let expected = expected_concept_child_ids(conn, normalized_key)?;
    concept_child_set_matches(conn, artifact_id, &expected)
}

fn concept_child_set_matches(
    conn: &Connection,
    artifact_id: i64,
    expected: &std::collections::BTreeSet<i64>,
) -> Result<bool> {
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT dst_artifact_id FROM semantic_relations
         WHERE src_artifact_id=?1 AND relation='related_to' AND claim_path=''",
    )?;
    let rows = statement.query_map([artifact_id], |row| row.get::<_, i64>(0))?;
    let stored = rows.collect::<std::result::Result<std::collections::BTreeSet<i64>, _>>()?;
    Ok(&stored == expected)
}

fn load_artifact_at_depth(
    conn: &Connection,
    id: i64,
    depth: u8,
    context: &mut FreshnessContext,
) -> Result<Option<SemanticArtifact>> {
    let row = conn
        .query_row(
            "SELECT id, supersedes_artifact_id, artifact_type, canonical_name, body_json,
                    model, prompt_version, confidence, source_snapshot, created_at
             FROM semantic_artifacts WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        supersedes,
        artifact_type,
        name,
        body_json,
        model,
        prompt_version,
        confidence,
        source_snapshot,
        created_at,
    )) = row
    else {
        return Ok(None);
    };
    let body: Value = serde_json::from_str(&body_json)
        .with_context(|| format!("semantic artifact {id} has invalid body JSON"))?;
    let mut statement = conn.prepare(
        "SELECT claim_path, anchor_key, role, evidence_file, evidence_start_line,
                evidence_end_line, source_hash, context_hash, confidence
         FROM semantic_supports WHERE artifact_id=?1
         ORDER BY claim_path, anchor_key, evidence_file, evidence_start_line",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let mut supports = Vec::new();
    for row in rows {
        let (
            claim_path,
            anchor,
            role,
            evidence_file,
            evidence_start_line,
            evidence_end_line,
            source_hash,
            stored_context_hash,
            support_confidence,
        ) = row?;
        let current_source_hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM files WHERE path=?1",
                [&evidence_file],
                |row| row.get(0),
            )
            .optional()?;
        let freshness = if current_source_hash.as_deref() != Some(source_hash.as_str()) {
            "source-stale"
        } else if context_hash(conn, &anchor).ok().as_deref() != Some(stored_context_hash.as_str())
        {
            "context-stale"
        } else {
            "fresh"
        };
        supports.push(SemanticSupport {
            relationship: support_relationship(&artifact_type, &body, &claim_path),
            claim_path,
            anchor,
            role,
            evidence_file,
            evidence_start_line,
            evidence_end_line,
            source_hash,
            context_hash: stored_context_hash,
            confidence: support_confidence,
            freshness: freshness.to_string(),
        });
    }
    let fresh_supports = supports
        .iter()
        .filter(|support| support.freshness == "fresh")
        .count();
    let own_freshness = if fresh_supports == supports.len() {
        "fresh"
    } else if artifact_type == "workflow" && fresh_supports > 0 {
        "degraded"
    } else {
        "stale"
    };
    let freshness = child_adjusted_freshness(
        conn,
        id,
        &artifact_type,
        &name,
        &body,
        own_freshness,
        depth,
        context,
    )?;
    Ok(Some(SemanticArtifact {
        id,
        supersedes,
        artifact_type,
        name,
        trust: "untrusted-semantic-memory".into(),
        body,
        model,
        prompt_version,
        confidence,
        source_snapshot,
        created_at,
        freshness,
        supports,
        retrieval_score: None,
    }))
}

/// Fold pinned child fingerprints into the parent's freshness: a missing,
/// superseded, or changed child stales the parent; a current child that is
/// itself no longer fresh degrades it — even when the parent's own text and
/// direct supports are unchanged. Depth bounds recursion defensively while
/// still covering a concept above repository -> module -> file -> card.
#[allow(clippy::too_many_arguments)]
fn child_adjusted_freshness(
    conn: &Connection,
    artifact_id: i64,
    artifact_type: &str,
    name: &Option<String>,
    body: &Value,
    own_freshness: &str,
    depth: u8,
    context: &mut FreshnessContext,
) -> Result<String> {
    let rank = |label: &str| match label {
        "fresh" => 0_u8,
        "degraded" => 1,
        _ => 2,
    };
    let mut worst = rank(own_freshness);
    // A summary must cover the scope's CURRENT deterministic child set, not
    // just keep its pinned children unchanged: a child added after
    // publication stales it even though every stored dependency is intact.
    if artifact_type == "summary" && worst < 2 {
        let level = body
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let scope = name.clone().unwrap_or_default();
        let packages = if matches!(level.as_str(), "module" | "repository") {
            context.packages(conn)?.to_vec()
        } else {
            Vec::new()
        };
        if !summary_child_set_current(conn, &packages, artifact_id, &level, &scope)? {
            worst = 2;
        }
    }
    if artifact_type == "concept" && worst < 2 {
        let normalized_key = name.as_deref().unwrap_or_default();
        let expected = context
            .concept_children(conn)?
            .get(normalized_key)
            .cloned()
            .unwrap_or_default();
        if !concept_child_set_matches(conn, artifact_id, &expected)? {
            worst = 2;
        }
    }
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT dst_artifact_id, dst_fingerprint FROM semantic_relations
         WHERE src_artifact_id=?1 ORDER BY dst_artifact_id",
    )?;
    let children = statement
        .query_map([artifact_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (child_id, pinned_fingerprint) in children {
        if worst == 2 {
            break;
        }
        let current: Option<Option<String>> = conn
            .query_row(
                "SELECT artifact.artifact_fingerprint FROM semantic_artifacts artifact
                 WHERE artifact.id=?1 AND NOT EXISTS(
                   SELECT 1 FROM semantic_artifacts successor
                   WHERE successor.supersedes_artifact_id=artifact.id
                 )",
                [child_id],
                |row| row.get(0),
            )
            .optional()?;
        match current.flatten() {
            Some(fingerprint) if fingerprint == pinned_fingerprint => {
                if depth == 0 {
                    continue;
                }
                // Memoized across one top-level load: a child cited by many
                // claims (or shared across a search pass) is computed once.
                let child_freshness = match context.memo.get(&child_id) {
                    Some(label) => Some(label.clone()),
                    None => {
                        let label = load_artifact_at_depth(conn, child_id, depth - 1, context)?
                            .map(|child| child.freshness);
                        if let Some(label) = &label {
                            context.memo.insert(child_id, label.clone());
                        }
                        label
                    }
                };
                match child_freshness.as_deref() {
                    Some("fresh") => {}
                    Some(_) => worst = worst.max(1),
                    None => worst = 2,
                }
            }
            _ => worst = 2,
        }
    }
    Ok(match worst {
        0 => "fresh".into(),
        1 => "degraded".into(),
        _ => "stale".into(),
    })
}

fn validate_input(input: &AnnotateInput) -> Result<()> {
    validate_confidence(&input.confidence)?;
    if !matches!(
        input.artifact_type.as_str(),
        "workflow" | "annotation" | "card" | "summary" | "concept" | "design"
    ) {
        bail!(
            "semantic artifact type must be one of: workflow, annotation, card, summary, concept, design"
        );
    }
    if !input.body.is_object() {
        bail!("semantic artifact body must be a JSON object");
    }
    if serde_json::to_vec(&input.body)?.len() > MAX_BODY_BYTES {
        bail!("semantic artifact body exceeds {MAX_BODY_BYTES} bytes");
    }
    // Parent semantic artifacts are relation-backed. Their generated claims
    // are grounded in fingerprinted child artifacts; attaching a child's leaf
    // span directly to parent prose would erase that semantic hop.
    if matches!(input.artifact_type.as_str(), "summary" | "concept")
        && input.supports.len() > MAX_SUPPORTS
    {
        bail!("semantic artifacts allow at most {MAX_SUPPORTS} supports");
    }
    if input.artifact_type == "summary" {
        let Some(scope) = input.name.as_deref().filter(|name| !name.is_empty()) else {
            bail!("summary artifacts require their scope key as the name");
        };
        let level = input.body.get("level").and_then(Value::as_str);
        match (level, scope) {
            (Some("file"), scope) if scope.starts_with("file:") => {}
            (Some("module"), scope) if scope.starts_with("module:") => {}
            (Some("repository"), "repo") => {}
            _ => bail!("summary level must be file, module, or repository and match its scope key"),
        }
        if input
            .body
            .get("overview")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            bail!("summary body requires a non-empty string `overview`");
        }
        return Ok(());
    }
    if input.artifact_type != "concept"
        && (input.supports.is_empty() || input.supports.len() > MAX_SUPPORTS)
    {
        bail!("semantic artifacts require 1..={MAX_SUPPORTS} supports");
    }
    let mut required_claims = Vec::new();
    collect_claim_paths(&input.body, "", &mut required_claims);
    required_claims.retain(|claim_path| {
        !(claim_path.starts_with("/participants/")
            && (claim_path.ends_with("/anchor") || claim_path.ends_with("/scope")))
            && !(input.artifact_type == "design"
                && (claim_path == "/resolution"
                    || (claim_path.starts_with("/touchpoints/")
                        && claim_path.ends_with("/anchor"))
                    || (claim_path.starts_with("/propagation/")
                        && (claim_path.ends_with("/from_anchor")
                            || claim_path.ends_with("/to_anchor")))))
    });
    if input.artifact_type != "concept" {
        for claim_path in &required_claims {
            if !input
                .supports
                .iter()
                .any(|support| support.claim_path == *claim_path)
            {
                bail!("semantic claim `{claim_path}` requires evidence support");
            }
        }
    }
    if input.artifact_type == "concept" {
        let body = input
            .body
            .as_object()
            .expect("semantic body was validated as an object");
        if body.len() != 2 || !body.contains_key("definition") || !body.contains_key("aliases") {
            bail!("concept body allows exactly `definition` and `aliases`");
        }
        let Some(name) = input.name.as_deref().filter(|name| !name.is_empty()) else {
            bail!("concept artifacts require a normalized canonical name");
        };
        if crate::scouting::concept::normalize(name) != name {
            bail!("concept canonical name must use the current concept normalizer");
        }
        if input
            .body
            .get("definition")
            .and_then(Value::as_str)
            .is_none_or(|definition| definition.trim().is_empty())
        {
            bail!("concept body requires a non-empty string `definition`");
        }
        let aliases = input
            .body
            .get("aliases")
            .and_then(Value::as_array)
            .context("concept body requires an `aliases` array")?;
        if aliases.is_empty() {
            bail!("concept body requires at least one alias");
        }
        let mut seen = HashSet::new();
        for alias in aliases {
            let alias = alias
                .as_str()
                .filter(|alias| !alias.trim().is_empty())
                .context("concept aliases must be non-empty strings")?;
            if crate::scouting::concept::normalize(alias) != name {
                bail!("concept alias `{alias}` does not normalize to `{name}`");
            }
            if !seen.insert(crate::scouting::concept::normalize_display(alias)) {
                bail!("concept alias `{alias}` is duplicated");
            }
        }
        return Ok(());
    }
    if input.artifact_type == "annotation" {
        if input.body.get("claim").and_then(Value::as_str).is_none() {
            bail!("annotation body requires a string `claim`");
        }
        return Ok(());
    }
    // A card is about exactly one symbol: its canonical name is that anchor
    // and every claim is supported from the subject's own declaration.
    if input.artifact_type == "card" {
        let Some(subject) = input.name.as_deref().filter(|name| !name.is_empty()) else {
            bail!("card artifacts require the subject anchor as their name");
        };
        if input
            .body
            .get("purpose")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            bail!("card body requires a non-empty string `purpose`");
        }
        if let Some(support) = input
            .supports
            .iter()
            .find(|support| support.anchor != subject)
        {
            bail!(
                "card supports must cite the subject anchor `{subject}`, not `{}`",
                support.anchor
            );
        }
        return Ok(());
    }
    if input.artifact_type == "design" {
        let Some(task_key) = input
            .name
            .as_deref()
            .filter(|name| name.starts_with("task:"))
        else {
            bail!("design artifacts require a hashed task key as their name");
        };
        if task_key.len() != "task:".len() + 64
            || !task_key["task:".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!("design task keys must contain a complete BLAKE3 hash");
        }
        if !matches!(
            input.body.get("resolution").and_then(Value::as_str),
            Some("resolved" | "unresolved")
        ) {
            bail!("design body requires resolution `resolved` or `unresolved`");
        }
        return Ok(());
    }
    if input.name.as_deref().is_none_or(str::is_empty) {
        bail!("workflow artifacts require a name");
    }
    let participants = input.body["participants"]
        .as_array()
        .context("workflow body requires a participants array")?;
    if participants.is_empty() {
        bail!("workflow body requires at least one participant");
    }
    if !input
        .supports
        .iter()
        .any(|support| support.claim_path == "/name")
    {
        bail!("workflow name requires a support with claim_path `/name`");
    }
    let mut participant_anchors = HashSet::new();
    let mut defining_participants = 0;
    for (index, participant) in participants.iter().enumerate() {
        let anchor = participant["anchor"]
            .as_str()
            .filter(|anchor| !anchor.is_empty())
            .with_context(|| format!("workflow participant {index} requires an anchor"))?;
        if !participant_anchors.insert(anchor) {
            bail!("workflow participant anchor `{anchor}` is duplicated");
        }
        if participant["role"].as_str().is_none_or(str::is_empty) {
            bail!("workflow participant {index} requires a role");
        }
        match participant["scope"].as_str() {
            Some("defining") => defining_participants += 1,
            Some("supporting") => {}
            _ => bail!("workflow participant {index} requires scope `defining` or `supporting`"),
        }
        let claim_path = format!("/participants/{index}/role");
        if !input
            .supports
            .iter()
            .any(|support| support.claim_path == claim_path && support.anchor == anchor)
        {
            bail!("workflow participant {index} requires support `{claim_path}` with its anchor");
        }
    }
    if defining_participants == 0 {
        bail!("workflow body requires at least one defining participant");
    }
    Ok(())
}

fn support_relationship(artifact_type: &str, body: &Value, claim_path: &str) -> String {
    if claim_path == "/name" {
        return "artifact-name-evidence".into();
    }
    if artifact_type != "workflow" || !claim_path.starts_with("/participants/") {
        return "claim-evidence".into();
    }
    let mut segments = claim_path.split('/');
    let _root = segments.next();
    let _participants = segments.next();
    let Some(index) = segments.next() else {
        return "claim-evidence".into();
    };
    match body
        .pointer(&format!("/participants/{index}/scope"))
        .and_then(Value::as_str)
    {
        Some("defining") => "defining-participant-evidence".into(),
        Some("supporting") => "supporting-participant-evidence".into(),
        _ => "legacy-participant-evidence".into(),
    }
}

fn collect_claim_paths(value: &Value, prefix: &str, output: &mut Vec<String>) {
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (key, value) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                collect_claim_paths(value, &format!("{prefix}/{escaped}"), output);
            }
        }
        Value::Array(values) if !values.is_empty() => {
            for (index, value) in values.iter().enumerate() {
                collect_claim_paths(value, &format!("{prefix}/{index}"), output);
            }
        }
        _ => output.push(prefix.to_string()),
    }
}

fn validate_confidence(confidence: &str) -> Result<()> {
    if !matches!(confidence, "likely" | "possible") {
        bail!("semantic confidence must be one of: likely, possible");
    }
    Ok(())
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "likely" => 1,
        _ => 0,
    }
}

pub(crate) fn context_hash(conn: &Connection, anchor: &str) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-semantic-context-v1\0");
    hasher.update(anchor.as_bytes());
    let mut statement = conn.prepare(
        "SELECT e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                COALESCE(src_file.hash,''), COALESCE(dst_file.hash,'')
         FROM resolved_edges e
         LEFT JOIN graph_nodes src ON src.node_key=e.src_key
         LEFT JOIN files src_file ON src.file_id=src_file.id
         LEFT JOIN graph_nodes dst ON dst.node_key=e.dst_key
         LEFT JOIN files dst_file ON dst.file_id=dst_file.id
         WHERE e.src_key=?1 OR e.dst_key=?1
         ORDER BY e.src_key, e.dst_key, e.kind, e.confidence, e.provenance, e.id",
    )?;
    let rows = statement.query_map([anchor], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    for row in rows {
        let values = row?;
        for value in [
            values.0, values.1, values.2, values.3, values.4, values.5, values.6,
        ] {
            hasher.update(b"\0");
            hasher.update(value.as_bytes());
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::json;

    use super::{
        AnnotateInput, SupportInput, WorkflowCandidateOptions, annotate, search,
        support_relationship, workflow_candidates,
    };
    use crate::{indexer, store, structural};

    fn support(claim_path: &str, anchor: &str, file: &str) -> SupportInput {
        SupportInput {
            claim_path: claim_path.into(),
            anchor: anchor.into(),
            role: None,
            evidence_file: file.into(),
            evidence_start_line: 1,
            evidence_end_line: 1,
            confidence: "likely".into(),
        }
    }

    fn publish_workflow(
        root: &std::path::Path,
        conn: &rusqlite::Connection,
        anchor: &str,
        name: &str,
        supersedes: Option<i64>,
    ) -> Result<super::SemanticArtifact> {
        annotate(
            root,
            conn,
            &AnnotateInput {
                artifact_type: "workflow".into(),
                name: Some(name.into()),
                body: json!({
                    "participants": [{
                        "anchor": anchor,
                        "role": "establishes vocabulary",
                        "scope": "defining",
                    }],
                }),
                supports: vec![
                    support("/name", anchor, "domain.ts"),
                    support("/participants/0/role", anchor, "domain.ts"),
                ],
                confidence: "likely".into(),
                snapshot: structural::current_snapshot(conn)?,
                supersedes,
            },
        )
    }

    fn publish_card(
        root: &std::path::Path,
        conn: &rusqlite::Connection,
        anchor: &str,
        terms: &[&str],
        supersedes: Option<i64>,
    ) -> Result<super::SemanticArtifact> {
        let mut supports = vec![support("/purpose", anchor, "domain.ts")];
        supports.extend(
            terms
                .iter()
                .enumerate()
                .map(|(index, _)| support(&format!("/domain_terms/{index}"), anchor, "domain.ts")),
        );
        annotate(
            root,
            conn,
            &AnnotateInput {
                artifact_type: "card".into(),
                name: Some(anchor.into()),
                body: json!({
                    "purpose": "establishes repository vocabulary",
                    "domain_terms": terms,
                }),
                supports,
                confidence: "likely".into(),
                snapshot: structural::current_snapshot(conn)?,
                supersedes,
            },
        )
    }

    fn artifact_fingerprint(conn: &rusqlite::Connection, id: i64) -> Result<String> {
        Ok(conn.query_row(
            "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
            [id],
            |row| row.get(0),
        )?)
    }

    fn persist_test_concept(
        root: &std::path::Path,
        conn: &rusqlite::Connection,
        anchor: &str,
        children: &[i64],
        whole_dependencies: bool,
    ) -> Result<i64> {
        let input = AnnotateInput {
            artifact_type: "concept".into(),
            name: Some("invoice settlement".into()),
            body: json!({
                "definition": "The repository-specific invoice settlement boundary",
                "aliases": ["Invoice Settlement"],
            }),
            supports: vec![
                support("/definition", anchor, "domain.ts"),
                support("/aliases/0", anchor, "domain.ts"),
            ],
            confidence: "likely".into(),
            snapshot: structural::current_snapshot(conn)?,
            supersedes: None,
        };
        let (snapshot, supports) = super::validate_annotate_input(root, conn, &input)?;
        let mut relations = Vec::new();
        for &child in children {
            let fingerprint = artifact_fingerprint(conn, child)?;
            relations.push(super::RelationInput {
                claim_path: "/definition".into(),
                relation: "related_to".into(),
                dst_artifact_id: child,
                dst_fingerprint: fingerprint.clone(),
                confidence: "likely".into(),
            });
            if whole_dependencies {
                relations.push(super::RelationInput {
                    claim_path: String::new(),
                    relation: "related_to".into(),
                    dst_artifact_id: child,
                    dst_fingerprint: fingerprint,
                    confidence: "likely".into(),
                });
            }
        }
        super::persist_validated_artifact(
            conn,
            &input,
            &snapshot,
            &supports,
            &relations,
            &super::ArtifactProvenance {
                model: "test",
                prompt_version: "concept-scout/v1",
                scout_run_id: None,
                input_fingerprint: None,
            },
        )
    }

    #[test]
    fn concept_semantic_shape_is_closed_normalized_and_supported() -> Result<()> {
        let base = AnnotateInput {
            artifact_type: "concept".into(),
            name: Some("invoice settlement".into()),
            body: json!({
                "definition": "The invoice settlement boundary in this repository",
                "aliases": ["Invoice Settlement", "invoice settlement"],
            }),
            supports: vec![
                support("/definition", "anchor", "domain.ts"),
                support("/aliases/0", "anchor", "domain.ts"),
                support("/aliases/1", "anchor", "domain.ts"),
            ],
            confidence: "likely".into(),
            snapshot: "snapshot".into(),
            supersedes: None,
        };
        super::validate_input(&base)?;

        let noncanonical_name = AnnotateInput {
            name: Some("Invoice  Settlement".into()),
            ..base.clone()
        };
        assert!(
            super::validate_input(&noncanonical_name)
                .unwrap_err()
                .to_string()
                .contains("current concept normalizer")
        );

        let near_alias = AnnotateInput {
            body: json!({
                "definition": "The invoice settlement boundary in this repository",
                "aliases": ["invoice-settlement"],
            }),
            supports: vec![
                support("/definition", "anchor", "domain.ts"),
                support("/aliases/0", "anchor", "domain.ts"),
            ],
            ..base.clone()
        };
        assert!(
            super::validate_input(&near_alias)
                .unwrap_err()
                .to_string()
                .contains("does not normalize")
        );

        let duplicate_display = AnnotateInput {
            body: json!({
                "definition": "The invoice settlement boundary in this repository",
                "aliases": ["Invoice\u{3000}Settlement", "Invoice Settlement"],
            }),
            ..base.clone()
        };
        assert!(
            super::validate_input(&duplicate_display)
                .unwrap_err()
                .to_string()
                .contains("duplicated")
        );

        let extra_field = AnnotateInput {
            body: json!({
                "definition": "The invoice settlement boundary in this repository",
                "aliases": ["Invoice Settlement", "invoice settlement"],
                "model_note": "not part of the persisted concept contract",
            }),
            supports: base
                .supports
                .iter()
                .cloned()
                .chain(std::iter::once(support(
                    "/model_note",
                    "anchor",
                    "domain.ts",
                )))
                .collect(),
            ..base
        };
        assert!(
            super::validate_input(&extra_field)
                .unwrap_err()
                .to_string()
                .contains("exactly `definition` and `aliases`")
        );
        Ok(())
    }

    #[test]
    fn expected_concept_children_use_only_exact_supported_current_vocabulary() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("domain.ts"),
            "export function alpha() { return 1; }\n\
             export function beta() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:domain.ts#::alpha@1";
        let beta = "sym:domain.ts#::beta@1";

        let old_workflow = publish_workflow(repo.path(), &conn, alpha, "Invoice Settlement", None)?;
        let card = publish_card(
            repo.path(),
            &conn,
            beta,
            &["invoice settlement", "invoice-settlement"],
            None,
        )?;
        annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("free prose".into()),
                body: json!({"claim": "invoice settlement appears only as prose"}),
                supports: vec![support("/claim", alpha, "domain.ts")],
                confidence: "likely".into(),
                snapshot: structural::current_snapshot(&conn)?,
                supersedes: None,
            },
        )?;

        // A prefix-like support path is not the exact card array-element
        // pointer admitted by deterministic planning.
        conn.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type,canonical_name,body_json,model,prompt_version,confidence,
               source_snapshot,created_at,artifact_fingerprint
             ) VALUES('card',?1,?2,'test','test/v1','likely',?3,
                      '2026-08-12T00:00:00Z','malformed-fingerprint')",
            rusqlite::params![
                alpha,
                json!({
                    "purpose": "malformed historical row",
                    "domain_terms": ["invoice settlement"],
                })
                .to_string(),
                structural::current_snapshot(&conn)?,
            ],
        )?;
        let malformed_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
               evidence_end_line,source_hash,context_hash,confidence
             ) VALUES(?1,'/domain_terms/0/nested',?2,'domain.ts',1,1,
                      'source','context','likely')",
            rusqlite::params![malformed_id, alpha],
        )?;

        assert_eq!(
            super::expected_concept_child_ids(&conn, "invoice settlement")?,
            [old_workflow.id, card.id].into_iter().collect()
        );
        assert_eq!(
            super::expected_concept_child_ids(&conn, "invoice-settlement")?,
            [card.id].into_iter().collect()
        );
        assert!(super::expected_concept_child_ids(&conn, "appears only as prose")?.is_empty());

        let replacement = publish_workflow(
            repo.path(),
            &conn,
            alpha,
            "Payment Completion",
            Some(old_workflow.id),
        )?;
        assert_eq!(
            super::expected_concept_child_ids(&conn, "invoice settlement")?,
            [card.id].into_iter().collect()
        );
        assert_eq!(
            super::expected_concept_child_ids(&conn, "payment completion")?,
            [replacement.id].into_iter().collect()
        );
        Ok(())
    }

    #[test]
    fn concept_freshness_tracks_exact_whole_child_set_additions_and_removals() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("domain.ts"),
            "export function alpha() { return 1; }\n\
             export function beta() { return 2; }\n\
             export function gamma() { return 3; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:domain.ts#::alpha@1";
        let beta = "sym:domain.ts#::beta@1";
        let gamma = "sym:domain.ts#::gamma@1";
        let workflow = publish_workflow(repo.path(), &conn, alpha, "Invoice Settlement", None)?;

        let claim_only = persist_test_concept(repo.path(), &conn, alpha, &[workflow.id], false)?;
        assert!(
            !super::concept_child_set_current(&conn, claim_only, "invoice settlement")?,
            "claim relations do not substitute for whole-input dependencies"
        );

        let first = persist_test_concept(repo.path(), &conn, alpha, &[workflow.id], true)?;
        assert!(super::concept_child_set_current(
            &conn,
            first,
            "invoice settlement"
        )?);
        assert_eq!(
            super::load_artifact(&conn, first)?
                .expect("concept exists")
                .freshness,
            "fresh"
        );

        // Punctuation is preserved by the normalizer, so a near variant does
        // not widen this concept's child set.
        publish_card(repo.path(), &conn, beta, &["invoice-settlement"], None)?;
        assert_eq!(
            super::load_artifact(&conn, first)?
                .expect("concept exists")
                .freshness,
            "fresh"
        );

        let matching = publish_card(repo.path(), &conn, gamma, &["invoice settlement"], None)?;
        assert_eq!(
            super::load_artifact(&conn, first)?
                .expect("concept exists")
                .freshness,
            "stale",
            "a newly added matching child invalidates the old exhaustive set"
        );

        let second =
            persist_test_concept(repo.path(), &conn, alpha, &[workflow.id, matching.id], true)?;
        assert_eq!(
            super::load_artifact(&conn, second)?
                .expect("concept exists")
                .freshness,
            "fresh"
        );

        publish_card(
            repo.path(),
            &conn,
            gamma,
            &["invoice-settlement"],
            Some(matching.id),
        )?;
        assert_eq!(
            super::load_artifact(&conn, second)?
                .expect("concept exists")
                .freshness,
            "stale",
            "removing a matching current child invalidates the stored exhaustive set"
        );
        Ok(())
    }

    #[test]
    fn semantic_search_does_not_hide_matches_older_than_two_hundred_rows() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export const a = 1;\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        for index in 0..=200 {
            let body = if index == 0 {
                json!({ "claim": "the uniquely ancient needle" })
            } else {
                json!({ "claim": format!("ordinary memory {index}") })
            };
            conn.execute(
                "INSERT INTO semantic_artifacts(
                   artifact_type, canonical_name, body_json, model, prompt_version,
                   confidence, source_snapshot, created_at, artifact_fingerprint
                 ) VALUES('annotation', ?1, ?2, 'test', 'test/v1', 'likely', ?3,
                          strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?4)",
                rusqlite::params![
                    format!("memory-{index}"),
                    body.to_string(),
                    snapshot,
                    format!("fingerprint-{index}"),
                ],
            )?;
        }

        let found = search(&conn, "uniquely ancient needle", 4)?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name.as_deref(), Some("memory-0"));
        Ok(())
    }

    #[test]
    fn summaries_degrade_and_stale_with_their_children() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let alpha = "sym:a.ts#::alpha@1";

        let card = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "card".into(),
                name: Some(alpha.into()),
                body: json!({ "purpose": "starts the alpha flow" }),
                supports: vec![support("/purpose", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )?;
        let card_fingerprint: String = conn.query_row(
            "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
            [card.id],
            |row| row.get(0),
        )?;

        let summary_input = AnnotateInput {
            artifact_type: "summary".into(),
            name: Some("file:a.ts".into()),
            body: json!({
                "level": "file",
                "scope": "file:a.ts",
                "overview": "hosts the alpha entry point",
            }),
            supports: Vec::new(),
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        };
        let (current_snapshot, supports) =
            super::validate_annotate_input(repo.path(), &conn, &summary_input)?;
        let parent_id = super::persist_validated_artifact(
            &conn,
            &summary_input,
            &current_snapshot,
            &supports,
            &[
                super::RelationInput {
                    claim_path: "/overview".into(),
                    relation: "summarizes".into(),
                    dst_artifact_id: card.id,
                    dst_fingerprint: card_fingerprint.clone(),
                    confidence: "likely".into(),
                },
                // Real publication always records the whole-artifact input
                // dependency; the expected-child-set check requires it.
                super::RelationInput {
                    claim_path: String::new(),
                    relation: "summarizes".into(),
                    dst_artifact_id: card.id,
                    dst_fingerprint: card_fingerprint,
                    confidence: "likely".into(),
                },
            ],
            &super::ArtifactProvenance {
                model: "test",
                prompt_version: "summary-scout/v1",
                scout_run_id: None,
                input_fingerprint: None,
            },
        )?;
        let freshness = |id: i64| -> Result<String> {
            Ok(super::load_artifact(&conn, id)?
                .expect("artifact exists")
                .freshness)
        };
        assert_eq!(freshness(parent_id)?, "fresh");

        // The child's own source drifts: the child is still current with the
        // pinned fingerprint but no longer fresh, so the parent degrades even
        // though its own text and relations are unchanged.
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 2; }\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        assert_ne!(freshness(card.id)?, "fresh");
        assert_eq!(freshness(parent_id)?, "degraded");

        // The child is superseded: the parent's pinned fingerprint no longer
        // names a current artifact, so the parent is stale outright.
        let successor_snapshot = structural::current_snapshot(&conn)?;
        annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "card".into(),
                name: Some(alpha.into()),
                body: json!({ "purpose": "starts the revised alpha flow" }),
                supports: vec![support("/purpose", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: successor_snapshot,
                supersedes: Some(card.id),
            },
        )?;
        assert_eq!(freshness(parent_id)?, "stale");
        Ok(())
    }

    #[test]
    fn workflow_round_trips_with_evidence_and_degrades_after_supported_source_changes() -> Result<()>
    {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        fs::write(
            repo.path().join("b.ts"),
            "export function beta() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let beta = "sym:b.ts#::beta@1";
        let input = AnnotateInput {
            artifact_type: "workflow".into(),
            name: Some("handoff workflow".into()),
            body: json!({
                "participants": [
                    { "anchor": alpha, "role": "starts handoff", "scope": "defining" },
                    { "anchor": beta, "role": "finishes handoff", "scope": "supporting" }
                ]
            }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
                support("/participants/1/role", beta, "b.ts"),
            ],
            confidence: "likely".into(),
            snapshot,
            supersedes: None,
        };
        let artifact = annotate(repo.path(), &conn, &input)?;
        assert_eq!(artifact.freshness, "fresh");
        assert_eq!(artifact.trust, "untrusted-semantic-memory");
        assert_eq!(artifact.model, "agent-reported");
        assert_eq!(artifact.prompt_version, "annotate/v2");
        assert_eq!(artifact.supports.len(), 3);
        assert_eq!(artifact.supports[0].relationship, "artifact-name-evidence");
        assert!(
            artifact
                .supports
                .iter()
                .any(|support| { support.relationship == "defining-participant-evidence" })
        );
        assert!(
            artifact
                .supports
                .iter()
                .any(|support| { support.relationship == "supporting-participant-evidence" })
        );
        assert_eq!(
            support_relationship(
                "workflow",
                &json!({ "participants": [{ "anchor": alpha, "role": "legacy" }] }),
                "/participants/0/role",
            ),
            "legacy-participant-evidence"
        );
        assert_eq!(search(&conn, "handoff", 4)?.len(), 1);

        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 3; }\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        let stale = search(&conn, "handoff", 4)?;
        assert_eq!(stale[0].freshness, "degraded");
        assert!(
            stale[0]
                .supports
                .iter()
                .any(|support| support.freshness == "source-stale")
        );
        assert!(
            stale[0]
                .supports
                .iter()
                .any(|support| support.freshness == "fresh")
        );
        Ok(())
    }

    #[test]
    fn workflow_requires_unique_scoped_participants_and_one_defining_boundary() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let snapshot = structural::current_snapshot(&conn)?;
        let make_input = |participants| AnnotateInput {
            artifact_type: "workflow".into(),
            name: Some("scoped workflow".into()),
            body: json!({ "participants": participants }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
            ],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        };

        let missing_scope = make_input(json!([
            { "anchor": alpha, "role": "entry" }
        ]));
        assert!(
            annotate(repo.path(), &conn, &missing_scope)
                .unwrap_err()
                .to_string()
                .contains("requires scope")
        );

        let only_supporting = make_input(json!([
            { "anchor": alpha, "role": "helper", "scope": "supporting" }
        ]));
        assert!(
            annotate(repo.path(), &conn, &only_supporting)
                .unwrap_err()
                .to_string()
                .contains("at least one defining participant")
        );

        let duplicate = AnnotateInput {
            body: json!({
                "participants": [
                    { "anchor": alpha, "role": "entry", "scope": "defining" },
                    { "anchor": alpha, "role": "helper", "scope": "supporting" }
                ]
            }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
                support("/participants/1/role", alpha, "a.ts"),
            ],
            ..make_input(json!([]))
        };
        assert!(
            annotate(repo.path(), &conn, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicated")
        );
        Ok(())
    }

    #[test]
    fn workflow_candidates_expand_ranked_production_symbols_and_fingerprint_the_set() -> Result<()>
    {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("entry.ts"),
            "import { middle } from './middle';\nexport function entry() { return middle(); }\n",
        )?;
        fs::write(
            repo.path().join("middle.ts"),
            "import { leaf } from './leaf';\nexport function middle() { return leaf(); }\n",
        )?;
        fs::write(
            repo.path().join("leaf.ts"),
            "export function leaf() { return 1; }\n",
        )?;
        fs::write(
            repo.path().join("entry.test.ts"),
            "export function testHelper() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let entry = "sym:entry.ts#::entry@1".to_string();
        let leaf = "sym:leaf.ts#::leaf@1".to_string();
        let options = WorkflowCandidateOptions::default();
        let result =
            workflow_candidates(repo.path(), &conn, &[entry.clone(), leaf.clone()], &options)?;
        assert_eq!(result.candidates.len(), 3);
        assert!(!result.traversal_truncated);
        assert!(!result.candidate_truncated);
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| !candidate.file.contains("test"))
        );
        assert!(result.candidates.iter().any(|candidate| {
            candidate.display_name == "middle" && candidate.evidence_end_line >= 2
        }));

        let reversed = workflow_candidates(repo.path(), &conn, &[leaf, entry], &options)?;
        assert_eq!(result.fingerprint, reversed.fingerprint);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| &candidate.anchor)
                .collect::<Vec<_>>(),
            reversed
                .candidates
                .iter()
                .map(|candidate| &candidate.anchor)
                .collect::<Vec<_>>(),
        );

        let limited = workflow_candidates(
            repo.path(),
            &conn,
            &["entry".into()],
            &WorkflowCandidateOptions {
                candidate_limit: 2,
                ..Default::default()
            },
        )?;
        assert_eq!(limited.candidates.len(), 2);
        assert!(limited.candidate_truncated);

        let file_seed = workflow_candidates(repo.path(), &conn, &["entry.ts".into()], &options)
            .expect_err("file seeds must not silently choose an operation");
        assert!(
            file_seed
                .to_string()
                .contains("workflow seed must resolve to a symbol anchor")
        );
        Ok(())
    }

    #[test]
    fn workflow_candidates_keep_seed_symbols_under_singular_doc_directories() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("src/doc"))?;
        fs::write(
            repo.path().join("src/doc/job.ts"),
            "export function dispatchJob() { return handleJob(); }\n\
             export function handleJob() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let role: String = conn.query_row(
            "SELECT role FROM files WHERE path='src/doc/job.ts'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(role, "production");

        let seed = "sym:src/doc/job.ts#::dispatchJob@1";
        let candidates = workflow_candidates(
            repo.path(),
            &conn,
            &[seed.into()],
            &WorkflowCandidateOptions::default(),
        )?;
        let candidate = candidates
            .candidates
            .iter()
            .find(|candidate| candidate.anchor == seed)
            .expect("the production seed must remain in its own candidate set");
        assert!(candidate.seed);
        assert_eq!(candidate.file, "src/doc/job.ts");
        Ok(())
    }

    #[test]
    fn annotate_rejects_untrusted_confidence_bad_spans_and_stale_snapshots() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let base = AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("claim".into()),
            body: json!({ "claim": "alpha participates in a workflow" }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "certain".into(),
            snapshot: structural::current_snapshot(&conn)?,
            supersedes: None,
        };
        assert!(
            annotate(repo.path(), &conn, &base)
                .unwrap_err()
                .to_string()
                .contains("likely")
        );

        let bad_span = AnnotateInput {
            confidence: "likely".into(),
            supports: vec![SupportInput {
                evidence_end_line: 99,
                ..support("/claim", alpha, "a.ts")
            }],
            ..base.clone()
        };
        assert!(
            annotate(repo.path(), &conn, &bad_span)
                .unwrap_err()
                .to_string()
                .contains("line count")
        );

        let stale_snapshot = AnnotateInput {
            snapshot: "0".repeat(64),
            supports: vec![support("/claim", alpha, "a.ts")],
            ..bad_span
        };
        assert!(
            annotate(repo.path(), &conn, &stale_snapshot)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM semantic_artifacts", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[test]
    fn superseding_annotation_hides_prior_record_from_default_search() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let first = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({ "claim": "alpha returns one" }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )?;
        let second = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({ "claim": "alpha is the handoff entry" }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot,
                supersedes: Some(first.id),
            },
        )?;
        let results = search(&conn, "alpha", 4)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, second.id);
        assert_eq!(results[0].supersedes, Some(first.id));
        Ok(())
    }

    #[test]
    fn every_semantic_leaf_claim_requires_evidence_support() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("a.ts"),
            "export function alpha() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let result = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({
                    "claim": "alpha returns one",
                    "unsupported_detail": "alpha also starts the handoff"
                }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: structural::current_snapshot(&conn)?,
                supersedes: None,
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("/unsupported_detail")
        );
        Ok(())
    }
}
