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
use serde::ser::{SerializeMap, Serializer};
use serde_json::Value;

use crate::{embed, origin, recon, semantic, store, structural};

pub const DEFAULT_RESPONSE_BYTE_LIMIT: usize = 24_000;
pub const DEFAULT_SOURCE_BYTE_LIMIT: usize = 2_000;
pub const MAX_ARTIFACT_LIMIT: usize = 100;
pub const MAX_RELATION_LIMIT: usize = 200;
pub const MAX_SOURCE_LIMIT: usize = 100;
pub const DEFAULT_CONCEPT_TAG_LIMIT: usize = 40;
pub const MAX_CONCEPT_TAG_LIMIT: usize = 200;
const MAX_SOURCE_BYTE_LIMIT: usize = 16_000;
const EVIDENCE_RELATION_DEPTH: usize = 8;
const MAX_EVIDENCE_RELATION_PATHS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactViewMode {
    Compact,
    Body,
    Full,
}

impl ArtifactViewMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "compact" => Ok(Self::Compact),
            "body" => Ok(Self::Body),
            "full" => Ok(Self::Full),
            _ => bail!("semantic artifact view must be compact, body, or full"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Body => "body",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub query: String,
    pub artifact_id: Option<i64>,
    pub anchor: Option<String>,
    pub file: Option<String>,
    pub reconnaissance_subject: Option<String>,
    pub related_to: Option<i64>,
    pub artifact_types: Vec<String>,
    pub freshness: Vec<String>,
    pub include_superseded: bool,
    pub limit: usize,
    pub supports_per_artifact: usize,
    pub relation_limit: usize,
    pub concept_tag_limit: usize,
    pub include_source: bool,
    pub source_limit: usize,
    pub source_byte_limit: usize,
    pub file_origins: Vec<String>,
    pub response_byte_limit: usize,
    pub evidence_relation_depth: usize,
    pub artifact_view: ArtifactViewMode,
    pub debug: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            artifact_id: None,
            anchor: None,
            file: None,
            reconnaissance_subject: None,
            related_to: None,
            artifact_types: Vec::new(),
            freshness: Vec::new(),
            include_superseded: false,
            limit: 20,
            supports_per_artifact: 8,
            relation_limit: 40,
            concept_tag_limit: DEFAULT_CONCEPT_TAG_LIMIT,
            include_source: false,
            source_limit: 12,
            source_byte_limit: DEFAULT_SOURCE_BYTE_LIMIT,
            file_origins: origin::defaults(),
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
            evidence_relation_depth: EVIDENCE_RELATION_DEPTH,
            // Library callers retain the historical diagnostic projection.
            // Agent-facing CLI/MCP surfaces explicitly select compact by
            // default for exact artifact reads.
            artifact_view: ArtifactViewMode::Full,
            debug: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactView {
    pub id: i64,
    pub supersedes: Option<i64>,
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
    pub retrieval_score: Option<semantic::ArtifactRetrievalScore>,
    pub support_count: usize,
    pub supports: Vec<semantic::SemanticSupport>,
    pub supports_truncated: bool,
    view: ArtifactViewMode,
}

impl Serialize for ArtifactView {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        if self.view == ArtifactViewMode::Full {
            map.serialize_entry("supersedes", &self.supersedes)?;
        }
        map.serialize_entry("type", &self.artifact_type)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("current", &self.current)?;
        map.serialize_entry("superseded_by", &self.superseded_by)?;
        map.serialize_entry("trust", &self.trust)?;

        match self.view {
            ArtifactViewMode::Compact => {
                if let Some((key, value)) = primary_artifact_claim(&self.artifact_type, &self.body)
                {
                    map.serialize_entry(key, &value)?;
                }
                if self.artifact_type == "workflow" {
                    let participants = defining_participants(&self.body);
                    if !participants.is_empty() {
                        map.serialize_entry("defining_participants", &participants)?;
                    }
                }
            }
            ArtifactViewMode::Body | ArtifactViewMode::Full => {
                map.serialize_entry("body", &self.body)?;
            }
        }

        if self.view == ArtifactViewMode::Full {
            map.serialize_entry("model", &self.model)?;
            map.serialize_entry("prompt_version", &self.prompt_version)?;
        }
        map.serialize_entry("confidence", &self.confidence)?;
        if self.view == ArtifactViewMode::Full {
            map.serialize_entry("source_snapshot", &self.source_snapshot)?;
            map.serialize_entry("created_at", &self.created_at)?;
        }
        map.serialize_entry("freshness", &self.freshness)?;
        if self.view == ArtifactViewMode::Full
            && let Some(score) = &self.retrieval_score
        {
            map.serialize_entry("retrieval_score", score)?;
        }
        map.serialize_entry("support_count", &self.support_count)?;
        match self.view {
            ArtifactViewMode::Compact => {}
            ArtifactViewMode::Body => {
                let evidence = self
                    .supports
                    .iter()
                    .map(compact_support)
                    .collect::<Vec<_>>();
                map.serialize_entry("evidence", &evidence)?;
            }
            ArtifactViewMode::Full => map.serialize_entry("supports", &self.supports)?,
        }
        map.serialize_entry("supports_truncated", &self.supports_truncated)?;
        map.end()
    }
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
pub struct ArtifactSupportHandle {
    pub anchor: String,
    pub file: String,
    pub lines: [i64; 2],
    pub freshness: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactFollowup {
    pub tool: &'static str,
    pub arguments: ArtifactFollowupArguments,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactFollowupArguments {
    pub artifact: i64,
    pub view: &'static str,
}

#[derive(Debug, Clone)]
pub struct ArtifactHandle {
    pub id: i64,
    pub artifact_type: String,
    pub name: Option<String>,
    pub current: bool,
    pub confidence: String,
    pub freshness: String,
    pub support_count: usize,
    pub supports: Vec<ArtifactSupportHandle>,
    pub supports_truncated: bool,
    pub selection_reason: String,
    pub retrieval_score: Option<semantic::ArtifactRetrievalScore>,
    pub followup: ArtifactFollowup,
    render_score: bool,
}

impl Serialize for ArtifactHandle {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("type", &self.artifact_type)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("current", &self.current)?;
        map.serialize_entry("confidence", &self.confidence)?;
        map.serialize_entry("freshness", &self.freshness)?;
        map.serialize_entry("support_count", &self.support_count)?;
        map.serialize_entry("supports", &self.supports)?;
        map.serialize_entry("supports_truncated", &self.supports_truncated)?;
        map.serialize_entry("selection_reason", &self.selection_reason)?;
        if self.render_score
            && let Some(score) = &self.retrieval_score
        {
            map.serialize_entry("retrieval_score", score)?;
        }
        map.serialize_entry("followup", &self.followup)?;
        map.end()
    }
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

/// A deterministic association from one selected concept to either a file or
/// one current indexed chunk. File tags are emitted independently of chunk
/// overlap, so `level = "file"` always has null chunk fields.
#[derive(Debug, Clone, Serialize)]
pub struct ConceptTag {
    pub concept_artifact_id: i64,
    pub concept_name: Option<String>,
    pub file: String,
    pub level: String,
    pub chunk_id: Option<i64>,
    pub chunk_kind: Option<String>,
    pub chunk_name: Option<String>,
    pub chunk_scope: Option<String>,
    pub chunk_lines: Option<[i64; 2]>,
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
    pub omitted_concept_tags: usize,
    pub relation_depth_truncated: bool,
    pub omitted_relation_branches: usize,
    pub relation_paths_truncated: bool,
    pub relation_cycles_skipped: usize,
}

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub snapshot: String,
    /// `discovery` returns compact handles; `artifact_detail` is the only mode
    /// that returns full semantic bodies and relation/source payloads.
    pub mode: &'static str,
    /// `no_supported_memory` is a successful localized lookup with no direct
    /// evidence match. Unsupported analogies are never used as filler.
    pub status: &'static str,
    pub resolved_evidence_scope: Vec<String>,
    pub retrieval: semantic::ArtifactRetrievalStatus,
    /// Ranked candidates after exact type/anchor/relation/freshness filters,
    /// before the caller's result and byte budgets. This is not a calibrated
    /// count of semantically relevant artifacts.
    pub candidate_artifacts: usize,
    pub matched_concept_tags: usize,
    pub artifact_handles: Vec<ArtifactHandle>,
    pub semantic_artifacts: Vec<ArtifactView>,
    pub related_artifacts: Vec<RelatedArtifact>,
    pub concept_tags: Vec<ConceptTag>,
    pub source_evidence: Vec<SourceEvidence>,
    pub response_budget: ResponseBudget,
    artifact_view: ArtifactViewMode,
    debug: bool,
}

impl QueryResult {
    fn render_diagnostics(&self) -> bool {
        self.debug
            || (self.mode == "artifact_detail" && self.artifact_view == ArtifactViewMode::Full)
    }

    fn retrieval_is_actionable(&self) -> bool {
        self.retrieval.lexical != "active"
            || matches!(self.retrieval.vector, "degraded" | "failed")
            || self.retrieval.vector_action.is_some()
    }
}

impl Serialize for QueryResult {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let diagnostic = self.render_diagnostics();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("snapshot", &self.snapshot)?;
        map.serialize_entry("mode", &self.mode)?;
        map.serialize_entry("status", &self.status)?;
        if self.mode == "artifact_detail" && !diagnostic {
            map.serialize_entry("view", self.artifact_view.as_str())?;
        }
        if !self.resolved_evidence_scope.is_empty() {
            map.serialize_entry("resolved_evidence_scope", &self.resolved_evidence_scope)?;
        }
        if diagnostic || self.retrieval_is_actionable() {
            map.serialize_entry("retrieval", &self.retrieval)?;
        }
        if diagnostic {
            map.serialize_entry("candidate_artifacts", &self.candidate_artifacts)?;
            map.serialize_entry("matched_concept_tags", &self.matched_concept_tags)?;
        }
        if diagnostic || !self.artifact_handles.is_empty() {
            map.serialize_entry("artifact_handles", &self.artifact_handles)?;
        }
        if diagnostic || !self.semantic_artifacts.is_empty() {
            map.serialize_entry("semantic_artifacts", &self.semantic_artifacts)?;
        }
        if diagnostic || !self.related_artifacts.is_empty() {
            map.serialize_entry("related_artifacts", &self.related_artifacts)?;
        }
        if diagnostic || !self.concept_tags.is_empty() {
            map.serialize_entry("concept_tags", &self.concept_tags)?;
        }
        if diagnostic || !self.source_evidence.is_empty() {
            map.serialize_entry("source_evidence", &self.source_evidence)?;
        }
        if diagnostic {
            map.serialize_entry("response_budget", &self.response_budget)?;
        } else if self.response_budget.truncated {
            map.serialize_entry(
                "response_budget",
                &compact_response_budget(&self.response_budget),
            )?;
        }
        map.end()
    }
}

fn compact_response_budget(budget: &ResponseBudget) -> Value {
    let mut value = serde_json::Map::new();
    value.insert("rendered_bytes".into(), Value::from(budget.rendered_bytes));
    value.insert("truncated".into(), Value::Bool(true));
    let mut omitted = serde_json::Map::new();
    for (name, count) in [
        ("artifacts", budget.omitted_artifacts),
        ("relations", budget.omitted_relations),
        ("sources", budget.omitted_sources),
        ("sources_by_origin", budget.omitted_sources_by_origin),
        ("supports", budget.omitted_supports),
        ("concept_tags", budget.omitted_concept_tags),
        ("relation_branches", budget.omitted_relation_branches),
    ] {
        if count > 0 {
            omitted.insert(name.into(), Value::from(count));
        }
    }
    if !omitted.is_empty() {
        value.insert("omitted".into(), Value::Object(omitted));
    }
    if budget.truncated_sources > 0 {
        value.insert(
            "truncated_sources".into(),
            Value::from(budget.truncated_sources),
        );
    }
    if budget.relation_depth_truncated {
        value.insert("relation_depth_truncated".into(), Value::Bool(true));
    }
    if budget.relation_paths_truncated {
        value.insert("relation_paths_truncated".into(), Value::Bool(true));
    }
    if budget.relation_cycles_skipped > 0 {
        value.insert(
            "relation_cycles_skipped".into(),
            Value::from(budget.relation_cycles_skipped),
        );
    }
    Value::Object(value)
}

#[derive(Debug)]
struct Candidate {
    id: i64,
    artifact_type: String,
    current: bool,
    superseded_by: Option<i64>,
    retrieval_score: Option<semantic::ArtifactRetrievalScore>,
    retrieval_rank: usize,
    support_tier: u8,
    selection_reason: String,
}

#[derive(Debug, Default)]
struct SupportScope {
    anchor: Option<String>,
    file: Option<String>,
    reconnaissance_subject: Option<String>,
    subject_files: HashSet<String>,
}

impl SupportScope {
    fn localized(&self) -> bool {
        self.anchor.is_some() || self.file.is_some() || self.reconnaissance_subject.is_some()
    }

    fn labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        if let Some(anchor) = &self.anchor {
            labels.push(format!("anchor:{anchor}"));
        }
        if let Some(file) = &self.file {
            labels.push(format!("file:{file}"));
        }
        if let Some(subject) = &self.reconnaissance_subject {
            labels.push(format!("subject:{subject}"));
        }
        labels
    }
}

pub fn query(
    root: &Path,
    conn: &Connection,
    provider: Option<&embed::Provider>,
    options: &QueryOptions,
) -> Result<QueryResult> {
    validate_options(options)?;
    store::with_read_snapshot(conn, "jscout_semantic_query", || {
        let snapshot = structural::current_snapshot(conn)?;
        let support_scope = resolve_support_scope(conn, options)?;
        let localized = support_scope.localized();
        let detail_mode = options.artifact_id.is_some();
        let mut candidates = candidates(conn, options)?;
        if localized {
            apply_support_scope(conn, &mut candidates, &support_scope)?;
        }
        let ranking_limit = if localized || detail_mode {
            options.limit.saturating_mul(5).max(candidates.len())
        } else {
            options.limit.saturating_mul(5)
        };
        let (ranking, retrieval) = semantic::rank_artifacts(
            conn,
            provider,
            &options.query,
            options.include_superseded,
            ranking_limit,
        )?;
        apply_ranking(
            &mut candidates,
            &ranking,
            &options.query,
            localized || detail_mode,
        );
        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        let retrieval_scores = candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .retrieval_score
                    .clone()
                    .map(|score| (candidate.id, score))
            })
            .collect::<HashMap<_, _>>();
        let current = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.current))
            .collect::<HashMap<_, _>>();
        let superseded_by = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.superseded_by))
            .collect::<HashMap<_, _>>();
        let selection_reasons = candidates
            .iter()
            .map(|candidate| (candidate.id, candidate.selection_reason.clone()))
            .collect::<HashMap<_, _>>();

        let mut loaded = semantic::load_artifacts(conn, &candidate_ids)?;
        loaded.retain(|artifact| {
            options.freshness.is_empty()
                || options
                    .freshness
                    .iter()
                    .any(|value| value == &artifact.freshness)
        });
        let candidate_artifacts = loaded.len();
        loaded.truncate(options.limit);
        let omitted_artifacts = candidate_artifacts.saturating_sub(loaded.len());
        for artifact in &mut loaded {
            artifact.retrieval_score = retrieval_scores.get(&artifact.id).cloned();
        }

        let selected_ids = loaded
            .iter()
            .map(|artifact| artifact.id)
            .collect::<Vec<_>>();
        let full_artifact_view = detail_mode && options.artifact_view == ArtifactViewMode::Full;
        let (related_artifacts, omitted_relations) = if detail_mode {
            related_artifacts(
                conn,
                &selected_ids,
                if full_artifact_view {
                    options.relation_limit
                } else {
                    0
                },
            )?
        } else {
            (Vec::new(), 0)
        };
        let source_result = if detail_mode && options.include_source && options.source_limit > 0 {
            source_evidence(root, conn, &selected_ids, options)?
        } else {
            SourceEvidenceResult::default()
        };
        let SourceEvidenceResult {
            evidence: source_evidence,
            omitted_by_origin: omitted_sources_by_origin,
            omitted: omitted_sources,
            relation_depth_truncated,
            omitted_relation_branches,
            relation_paths_truncated,
            relation_cycles_skipped,
        } = source_result;
        let mut concept_tags = if full_artifact_view {
            concept_tags(conn, &loaded, &current, &options.file_origins)?
        } else {
            Vec::new()
        };
        let matched_concept_tags = concept_tags.len();
        concept_tags.truncate(options.concept_tag_limit);
        let omitted_concept_tags = matched_concept_tags.saturating_sub(concept_tags.len());
        let (artifact_handles, semantic_artifacts) = if detail_mode {
            (
                Vec::new(),
                loaded
                    .into_iter()
                    .map(|artifact| {
                        artifact_view(
                            artifact,
                            &current,
                            &superseded_by,
                            options.supports_per_artifact,
                            options.artifact_view,
                        )
                    })
                    .collect(),
            )
        } else {
            (
                loaded
                    .into_iter()
                    .map(|artifact| {
                        artifact_handle(
                            artifact,
                            &current,
                            &selection_reasons,
                            options.supports_per_artifact.min(2),
                            options.debug,
                        )
                    })
                    .collect(),
                Vec::new(),
            )
        };
        let omitted_supports = semantic_artifacts
            .iter()
            .map(|artifact| {
                artifact
                    .support_count
                    .saturating_sub(artifact.supports.len())
            })
            .sum::<usize>()
            + artifact_handles
                .iter()
                .map(|artifact| {
                    artifact
                        .support_count
                        .saturating_sub(artifact.supports.len())
                })
                .sum::<usize>();
        let truncated_sources = source_evidence
            .iter()
            .filter(|source| source.source_truncated)
            .count();
        let initially_truncated = omitted_artifacts > 0
            || omitted_relations > 0
            || omitted_sources > 0
            || omitted_sources_by_origin > 0
            || omitted_supports > 0
            || omitted_concept_tags > 0
            || truncated_sources > 0
            || relation_depth_truncated
            || relation_paths_truncated;
        let mut result = QueryResult {
            snapshot,
            mode: if detail_mode {
                "artifact_detail"
            } else {
                "discovery"
            },
            status: if localized && candidate_artifacts == 0 {
                "no_supported_memory"
            } else {
                "results"
            },
            resolved_evidence_scope: support_scope.labels(),
            retrieval,
            candidate_artifacts,
            matched_concept_tags,
            artifact_handles,
            semantic_artifacts,
            related_artifacts,
            concept_tags,
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
                omitted_concept_tags,
                relation_depth_truncated,
                omitted_relation_branches,
                relation_paths_truncated,
                relation_cycles_skipped,
                ..Default::default()
            },
            artifact_view: options.artifact_view,
            debug: options.debug,
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
    if options.concept_tag_limit == 0 || options.concept_tag_limit > MAX_CONCEPT_TAG_LIMIT {
        bail!("concept tag limit must be between 1 and {MAX_CONCEPT_TAG_LIMIT}");
    }
    if options.source_limit > MAX_SOURCE_LIMIT {
        bail!("semantic source limit must be between 0 and {MAX_SOURCE_LIMIT}");
    }
    if options.source_byte_limit == 0 || options.source_byte_limit > MAX_SOURCE_BYTE_LIMIT {
        bail!("source byte limit must be between 1 and {MAX_SOURCE_BYTE_LIMIT}");
    }
    if options.response_byte_limit == 0 {
        bail!("response byte limit must be greater than zero");
    }
    if options.include_source && options.artifact_id.is_none() {
        bail!("semantic source evidence requires an exact artifact id drill-down");
    }
    if options.evidence_relation_depth == 0 || options.evidence_relation_depth > 32 {
        bail!("evidence relation depth must be between 1 and 32");
    }
    origin::validate_all(&options.file_origins)?;
    if options
        .file
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        bail!("semantic evidence file must not be empty");
    }
    if options
        .reconnaissance_subject
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        bail!("semantic reconnaissance subject must not be empty");
    }
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

fn resolve_support_scope(conn: &Connection, options: &QueryOptions) -> Result<SupportScope> {
    let anchor = options
        .anchor
        .as_deref()
        .map(|anchor| {
            structural::resolve_current_anchor_in_origins(conn, anchor, &options.file_origins)
        })
        .transpose()?;
    let file = options
        .file
        .as_deref()
        .map(|file| file.strip_prefix("./").unwrap_or(file).to_string());
    if let Some(file) = &file {
        let origins_json = serde_json::to_string(&options.file_origins)?;
        let indexed = conn
            .query_row(
                "SELECT 1 FROM files
                 WHERE path=?1 AND origin IN (SELECT value FROM json_each(?2))",
                rusqlite::params![file, origins_json],
                |_| Ok(()),
            )
            .optional()?;
        if indexed.is_none() {
            bail!("semantic evidence file `{file}` is not indexed in the requested origins");
        }
    }
    let reconnaissance_subject = options.reconnaissance_subject.clone();
    let subject_files = if let Some(subject) = &reconnaissance_subject {
        let origins_json = serde_json::to_string(&options.file_origins)?;
        let allowed = conn
            .prepare(
                "SELECT path FROM files
                 WHERE origin IN (SELECT value FROM json_each(?1))",
            )?
            .query_map([origins_json], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        recon::current_subject_members(conn, subject)?
            .into_iter()
            .filter_map(|member| allowed.contains(&member.path).then_some(member.path))
            .collect()
    } else {
        HashSet::new()
    };
    Ok(SupportScope {
        anchor,
        file,
        reconnaissance_subject,
        subject_files,
    })
}

fn apply_support_scope(
    conn: &Connection,
    candidates: &mut Vec<Candidate>,
    scope: &SupportScope,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    let candidate_ids = serde_json::to_string(
        &candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
    )?;
    let mut supports = conn.prepare(
        "SELECT artifact_id, anchor_key, evidence_file
         FROM semantic_supports
         WHERE artifact_id IN (SELECT value FROM json_each(?1))
         ORDER BY artifact_id, anchor_key, evidence_file,
                  evidence_start_line, evidence_end_line",
    )?;
    let rows = supports.query_map([candidate_ids], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut matches = HashMap::<i64, (u8, String)>::new();
    for row in rows {
        let (artifact_id, anchor, file) = row?;
        let match_reason = if scope.anchor.as_deref() == Some(anchor.as_str()) {
            Some((0, "exact_anchor_support".to_string()))
        } else if scope.file.as_deref() == Some(file.as_str()) {
            Some((1, "exact_file_support".to_string()))
        } else if scope.subject_files.contains(&file) {
            Some((2, "reconnaissance_scope_support".to_string()))
        } else {
            None
        };
        if let Some(match_reason) = match_reason
            && matches
                .get(&artifact_id)
                .is_none_or(|(best, _)| match_reason.0 < *best)
        {
            matches.insert(artifact_id, match_reason);
        }
    }
    candidates.retain_mut(|candidate| {
        let Some((tier, reason)) = matches.get(&candidate.id) else {
            return false;
        };
        candidate.support_tier = *tier;
        candidate.selection_reason = reason.clone();
        true
    });
    Ok(())
}

fn candidates(conn: &Connection, options: &QueryOptions) -> Result<Vec<Candidate>> {
    let mut statement = conn.prepare(
        "SELECT artifact.id, artifact.artifact_type,
                NOT EXISTS(
                  SELECT 1 FROM semantic_artifacts successor
                  WHERE successor.supersedes_artifact_id=artifact.id
                ) AS current,
                (SELECT successor.id FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id) AS superseded_by
         FROM semantic_artifacts artifact
         WHERE (?1 IS NULL OR artifact.id=?1)
           AND (?2 IS NULL OR EXISTS(
             SELECT 1 FROM semantic_relations relation
             WHERE (relation.src_artifact_id=artifact.id AND relation.dst_artifact_id=?2)
                OR (relation.dst_artifact_id=artifact.id AND relation.src_artifact_id=?2)
           ))
         ORDER BY artifact.id DESC",
    )?;
    let rows = statement.query_map(
        rusqlite::params![options.artifact_id, options.related_to],
        |row| {
            Ok(Candidate {
                id: row.get(0)?,
                artifact_type: row.get(1)?,
                current: row.get(2)?,
                superseded_by: row.get(3)?,
                retrieval_score: None,
                retrieval_rank: usize::MAX,
                support_tier: u8::MAX,
                selection_reason: if options.query.trim().is_empty() {
                    "recent".into()
                } else {
                    "semantic_match".into()
                },
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

fn apply_ranking(
    candidates: &mut Vec<Candidate>,
    ranking: &[semantic::RankedArtifact],
    query: &str,
    preserve_unranked: bool,
) {
    if query.trim().is_empty() {
        candidates.sort_by(|left, right| {
            left.support_tier
                .cmp(&right.support_tier)
                .then_with(|| right.id.cmp(&left.id))
        });
        return;
    }
    let ranks = ranking
        .iter()
        .enumerate()
        .map(|(rank, artifact)| (artifact.id, (rank, artifact.retrieval_score.clone())))
        .collect::<HashMap<_, _>>();
    candidates.retain_mut(|candidate| match ranks.get(&candidate.id) {
        Some((rank, retrieval_score)) => {
            candidate.retrieval_rank = *rank;
            candidate.retrieval_score = Some(retrieval_score.clone());
            if candidate.support_tier == u8::MAX {
                candidate.selection_reason = retrieval_reason(retrieval_score);
            }
            true
        }
        None => preserve_unranked,
    });
    candidates.sort_by(|left, right| {
        left.support_tier
            .cmp(&right.support_tier)
            .then_with(|| left.retrieval_rank.cmp(&right.retrieval_rank))
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn retrieval_reason(score: &semantic::ArtifactRetrievalScore) -> String {
    match (score.lexical_score.is_some(), score.vector_cosine.is_some()) {
        (true, true) => "lexical_vector_match",
        (true, false) => "lexical_match",
        (false, true) => "vector_match",
        (false, false) => "recent",
    }
    .into()
}

fn artifact_view(
    mut artifact: semantic::SemanticArtifact,
    current: &HashMap<i64, bool>,
    superseded_by: &HashMap<i64, Option<i64>>,
    support_limit: usize,
    view: ArtifactViewMode,
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
        retrieval_score: artifact.retrieval_score,
        support_count,
        supports_truncated: support_count > artifact.supports.len(),
        supports: artifact.supports,
        view,
    }
}

fn primary_artifact_claim(artifact_type: &str, body: &Value) -> Option<(&'static str, Value)> {
    for (source, rendered) in [
        ("description", "description"),
        ("purpose", "primary_claim"),
        ("overview", "primary_claim"),
        ("definition", "primary_claim"),
        ("claim", "primary_claim"),
        ("architectural_role", "primary_claim"),
    ] {
        if let Some(value) = body.get(source).filter(|value| value.is_string()) {
            return Some((rendered, value.clone()));
        }
    }
    if artifact_type == "workflow" {
        let roles = defining_participants(body)
            .into_iter()
            .filter_map(|participant| participant["role"].as_str())
            .collect::<Vec<_>>();
        if !roles.is_empty() {
            return Some(("primary_claim", Value::String(roles.join(" → "))));
        }
    }
    None
}

fn defining_participants(body: &Value) -> Vec<&Value> {
    body.get("participants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|participant| participant["scope"] == "defining")
        .collect()
}

fn compact_support(support: &semantic::SemanticSupport) -> ArtifactSupportHandle {
    ArtifactSupportHandle {
        anchor: support.anchor.clone(),
        file: support.evidence_file.clone(),
        lines: [support.evidence_start_line, support.evidence_end_line],
        freshness: support.freshness.clone(),
    }
}

fn artifact_handle(
    mut artifact: semantic::SemanticArtifact,
    current: &HashMap<i64, bool>,
    selection_reasons: &HashMap<i64, String>,
    support_limit: usize,
    debug: bool,
) -> ArtifactHandle {
    let support_count = artifact.supports.len();
    artifact.supports.truncate(support_limit);
    ArtifactHandle {
        id: artifact.id,
        artifact_type: artifact.artifact_type,
        name: artifact.name,
        current: current.get(&artifact.id).copied().unwrap_or(false),
        confidence: artifact.confidence,
        freshness: artifact.freshness,
        support_count,
        supports_truncated: support_count > artifact.supports.len(),
        supports: artifact
            .supports
            .into_iter()
            .map(|support| ArtifactSupportHandle {
                anchor: support.anchor,
                file: support.evidence_file,
                lines: [support.evidence_start_line, support.evidence_end_line],
                freshness: support.freshness,
            })
            .collect(),
        selection_reason: selection_reasons
            .get(&artifact.id)
            .cloned()
            .unwrap_or_else(|| "recent".into()),
        retrieval_score: artifact.retrieval_score,
        followup: ArtifactFollowup {
            tool: "semantic_memory",
            arguments: ArtifactFollowupArguments {
                artifact: artifact.id,
                view: "body",
            },
        },
        render_score: debug,
    }
}

fn concept_tags(
    conn: &Connection,
    artifacts: &[semantic::SemanticArtifact],
    current: &HashMap<i64, bool>,
    allowed_origins: &[String],
) -> Result<Vec<ConceptTag>> {
    let allowed_origins = allowed_origins
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut indexed_origins = HashMap::<String, Option<String>>::new();
    let mut origin_for_file = conn.prepare_cached("SELECT origin FROM files WHERE path=?1")?;
    let mut tags = BTreeMap::<(i64, String, i64), ConceptTag>::new();
    let mut chunks = conn.prepare_cached(
        "SELECT chunk.id, chunk.kind, chunk.name, chunk.scope_chain,
                chunk.start_line, chunk.end_line
         FROM chunks chunk
         JOIN files file ON file.id=chunk.file_id
         WHERE file.path=?1
           AND chunk.start_line<=?2
           AND chunk.end_line>=?3
           AND file.origin=?4
         ORDER BY chunk.start_line, chunk.end_line, chunk.id",
    )?;
    for artifact in artifacts {
        if artifact.artifact_type != "concept"
            || artifact.freshness != "fresh"
            || !current.get(&artifact.id).copied().unwrap_or(false)
        {
            continue;
        }
        let mut child_ids = Vec::new();
        let mut relation_statement = conn.prepare_cached(
            "SELECT DISTINCT dst_artifact_id FROM semantic_relations
             WHERE src_artifact_id=?1 AND relation='related_to' AND claim_path<>''
             ORDER BY dst_artifact_id",
        )?;
        let rows = relation_statement.query_map([artifact.id], |row| row.get::<_, i64>(0))?;
        for row in rows {
            child_ids.push(row?);
        }
        // Legacy direct-support concepts remain readable. New generated
        // concepts reach exact source through fingerprinted child relations.
        let support_artifacts = if child_ids.is_empty() {
            vec![artifact.clone()]
        } else {
            semantic::load_artifacts(conn, &child_ids)?
        };
        for support in support_artifacts
            .iter()
            .flat_map(|child| &child.supports)
            .filter(|support| support.freshness == "fresh")
        {
            let indexed_origin = match indexed_origins.get(&support.evidence_file) {
                Some(origin) => origin.clone(),
                None => {
                    let origin = origin_for_file
                        .query_row([&support.evidence_file], |row| row.get::<_, String>(0))
                        .optional()?;
                    indexed_origins.insert(support.evidence_file.clone(), origin.clone());
                    origin
                }
            };
            let Some(indexed_origin) = indexed_origin else {
                continue;
            };
            if !allowed_origins.contains(indexed_origin.as_str()) {
                continue;
            }
            let file_key = (artifact.id, support.evidence_file.clone(), 0);
            tags.entry(file_key).or_insert_with(|| ConceptTag {
                concept_artifact_id: artifact.id,
                concept_name: artifact.name.clone(),
                file: support.evidence_file.clone(),
                level: "file".into(),
                chunk_id: None,
                chunk_kind: None,
                chunk_name: None,
                chunk_scope: None,
                chunk_lines: None,
            });
            let rows = chunks.query_map(
                rusqlite::params![
                    support.evidence_file,
                    support.evidence_end_line,
                    support.evidence_start_line,
                    indexed_origin,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )?;
            for row in rows {
                let (chunk_id, kind, name, scope, start_line, end_line) = row?;
                let key = (artifact.id, support.evidence_file.clone(), chunk_id);
                tags.entry(key).or_insert_with(|| ConceptTag {
                    concept_artifact_id: artifact.id,
                    concept_name: artifact.name.clone(),
                    file: support.evidence_file.clone(),
                    level: "chunk".into(),
                    chunk_id: Some(chunk_id),
                    chunk_kind: Some(kind),
                    chunk_name: name,
                    chunk_scope: Some(scope),
                    chunk_lines: Some([start_line, end_line]),
                });
            }
        }
    }
    let ranks = artifacts
        .iter()
        .enumerate()
        .map(|(rank, artifact)| (artifact.id, rank))
        .collect::<HashMap<_, _>>();
    let mut tags = tags.into_values().collect::<Vec<_>>();
    tags.sort_by(|left, right| {
        // Preserve broad file coverage before chunk refinements, then retain
        // the semantic result ranking inside each level.
        left.chunk_id
            .is_some()
            .cmp(&right.chunk_id.is_some())
            .then_with(|| {
                ranks
                    .get(&left.concept_artifact_id)
                    .copied()
                    .unwrap_or(usize::MAX)
                    .cmp(
                        &ranks
                            .get(&right.concept_artifact_id)
                            .copied()
                            .unwrap_or(usize::MAX),
                    )
            })
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.chunk_lines.cmp(&right.chunk_lines))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    Ok(tags)
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

#[derive(Default)]
struct SourceEvidenceResult {
    evidence: Vec<SourceEvidence>,
    omitted_by_origin: usize,
    omitted: usize,
    relation_depth_truncated: bool,
    omitted_relation_branches: usize,
    relation_paths_truncated: bool,
    relation_cycles_skipped: usize,
}

fn source_evidence(
    root: &Path,
    conn: &Connection,
    root_ids: &[i64],
    options: &QueryOptions,
) -> Result<SourceEvidenceResult> {
    let allowed_origins = options
        .file_origins
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut evidence = Vec::new();
    let mut omitted_by_origin = 0;
    let mut observed: usize = 0;
    let mut relation_depth_truncated = false;
    let mut omitted_relation_branches = 0;
    let mut relation_paths_truncated = false;
    let mut relation_cycles_skipped = 0;
    for &root_id in root_ids {
        let paths = evidence_paths(conn, root_id, options.evidence_relation_depth)?;
        relation_depth_truncated |= paths.truncated;
        omitted_relation_branches += paths.omitted_branches;
        relation_paths_truncated |= paths.paths_truncated;
        relation_cycles_skipped += paths.cycles_skipped;
        let ids = paths.paths.keys().copied().collect::<Vec<_>>();
        let artifacts = semantic::load_artifacts(conn, &ids)?;
        for artifact in artifacts {
            let artifact_paths = paths.paths.get(&artifact.id).cloned().unwrap_or_default();
            for support in artifact.supports {
                for path in &artifact_paths {
                    observed += 1;
                    let Some(file) = evidence_file(conn, &support.evidence_file)? else {
                        if evidence.len() < options.source_limit {
                            evidence.push(unavailable_source(
                                root_id,
                                artifact.id,
                                path.clone(),
                                support.clone(),
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
                        support.clone(),
                        file,
                        options.source_byte_limit,
                    ));
                }
            }
        }
    }
    let omitted = observed
        .saturating_sub(omitted_by_origin)
        .saturating_sub(evidence.len());
    Ok(SourceEvidenceResult {
        evidence,
        omitted_by_origin,
        omitted,
        relation_depth_truncated,
        omitted_relation_branches,
        relation_paths_truncated,
        relation_cycles_skipped,
    })
}

struct EvidencePaths {
    paths: BTreeMap<i64, Vec<Vec<EvidenceHop>>>,
    truncated: bool,
    omitted_branches: usize,
    paths_truncated: bool,
    cycles_skipped: usize,
}

fn evidence_paths(conn: &Connection, root_id: i64, max_depth: usize) -> Result<EvidencePaths> {
    let mut paths = BTreeMap::from([(root_id, vec![Vec::new()])]);
    let mut queue = VecDeque::from([(root_id, 0_usize, Vec::new())]);
    let mut truncated = false;
    let mut omitted_branches = 0;
    let mut paths_truncated = false;
    let mut cycles_skipped = 0;
    let mut path_count = 1;
    while let Some((artifact_id, depth, parent_path)) = queue.pop_front() {
        if depth >= max_depth {
            let deeper: i64 = conn.query_row(
                "SELECT count(*) FROM semantic_relations
                 WHERE src_artifact_id=?1 AND claim_path<>''",
                [artifact_id],
                |row| row.get(0),
            )?;
            if deeper > 0 {
                truncated = true;
                omitted_branches += usize::try_from(deeper).unwrap_or(usize::MAX);
            }
            continue;
        }
        let mut statement = conn.prepare_cached(
            "SELECT dst_artifact_id, relation, claim_path FROM semantic_relations
             WHERE src_artifact_id=?1 AND claim_path<>''
             ORDER BY dst_artifact_id, relation, claim_path",
        )?;
        let rows = statement.query_map([artifact_id], |row| {
            Ok(EvidenceHop {
                artifact_id: row.get(0)?,
                relation: row.get(1)?,
                claim_path: row.get(2)?,
            })
        })?;
        for row in rows {
            let hop = row?;
            let child_id = hop.artifact_id;
            if child_id == root_id
                || parent_path
                    .iter()
                    .any(|existing: &EvidenceHop| existing.artifact_id == child_id)
            {
                cycles_skipped += 1;
                continue;
            }
            let mut path = parent_path.clone();
            path.push(hop);
            if path_count >= MAX_EVIDENCE_RELATION_PATHS {
                paths_truncated = true;
                omitted_branches += 1;
                continue;
            }
            path_count += 1;
            paths.entry(child_id).or_default().push(path.clone());
            queue.push_back((child_id, depth + 1, path));
        }
    }
    Ok(EvidencePaths {
        paths,
        truncated,
        omitted_branches,
        paths_truncated,
        cycles_skipped,
    })
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

#[expect(clippy::too_many_arguments)]
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
        source_status: support.freshness.clone(),
        source: None,
        source_original_bytes: 0,
        source_rendered_bytes: 0,
        source_truncated: false,
    };
    let Ok(path) = store::file_source_path(conn, root, file.id) else {
        result.source_status = "unavailable".into();
        return result;
    };
    let Ok(source) = std::fs::read_to_string(&path) else {
        result.source_status = "unavailable".into();
        return result;
    };
    if blake3::hash(source.as_bytes()).to_hex().as_str() != file.indexed_hash {
        result.source_status = "index-stale".into();
        return result;
    }
    if support.freshness == "source-stale" {
        // The indexed file is current, but this support was pinned to older
        // source bytes. Recorded line numbers no longer identify exact
        // evidence, so never render current text under the old claim.
        result.source_status = "source-stale".into();
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
        if let Some(handle) = result
            .artifact_handles
            .iter_mut()
            .rev()
            .find(|handle| !handle.supports.is_empty())
        {
            handle.supports.pop();
            handle.supports_truncated = true;
            result.response_budget.omitted_supports += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if result.artifact_handles.len() > 1 {
            result.artifact_handles.pop();
            result.response_budget.omitted_artifacts += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
        if result.concept_tags.pop().is_some() {
            result.response_budget.omitted_concept_tags += 1;
            settle_rendered_bytes(result)?;
            continue;
        }
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
        if result.artifact_view != ArtifactViewMode::Compact
            && let Some(artifact) = result
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
mod tests;
