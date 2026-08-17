//! Task-scoped design-before-edit memory. A design starts from a bounded
//! deterministic/hybrid localization pass, asks the model for mechanisms and
//! cure semantics rather than code, and publishes only evidence-backed claims.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::evidence::{self, EvidencePack};
use super::ledger::{self, ClassificationRow, RunClaim, RunOutcome, RunSpec};
use super::{
    ORPHAN_SWEEP_MINUTES, PreparationCache, ScoutReport, failed_report, gateway_timeout_report,
    publish_terminal, remote_timeout, reused, scout_report,
};
use crate::embed;
use crate::llm::config::{ModelSpec, RequestPolicy};
use crate::llm::protocol::{
    ChatMessage, CompleteRequest, PROTOCOL_VERSION, ProviderOptions, SubmitTool,
};
use crate::llm::{GatewayError, LlmGateway};
use crate::semantic::{self, AnnotateInput, SupportInput, WorkflowCandidate};
use crate::{origin, recon, search, store, structural};

pub const PROMPT_VERSION: &str = "design-scout/v1";
pub const PLANNING_VERSION: &str = "design-evidence/v3";
pub const SUBMIT_TOOL_NAME: &str = "submit_task_design";
pub const DEFAULT_SEARCH_LIMIT: usize = 12;
pub const DEFAULT_SEED_LIMIT: usize = 6;
pub const DEFAULT_FILE_LIMIT: usize = 12;
pub const DEFAULT_GRAPH_DEPTH: usize = 2;
pub const DEFAULT_GRAPH_NODE_LIMIT: usize = 500;
pub const DEFAULT_GRAPH_EDGE_LIMIT: usize = 2_000;
pub const DEFAULT_CANDIDATE_LIMIT: usize = 40;
pub const DEFAULT_RESPONSE_BYTES: usize = 32_000;

const MAX_TASK_CHARS: usize = 8_000;
const MAX_SEARCH_LIMIT: usize = 100;
const MAX_SEED_LIMIT: usize = 64;
const MAX_FILE_LIMIT: usize = 64;
const MAX_GRAPH_NODE_LIMIT: usize = 20_000;
const MAX_GRAPH_EDGE_LIMIT: usize = 100_000;
const MAX_CANDIDATE_LIMIT: usize = 128;
const MAX_CLAIM_CHARS: usize = 1_200;
const MAX_SHORT_CLAIM_CHARS: usize = 500;
const MAX_ITEMS: usize = 8;
const MAX_EVIDENCE_PER_CLAIM: usize = 2;
const MAX_INCOMPLETE_REASON_CHARS: usize = 500;
const STRUCTURAL_CONTEXT_ANCHOR_LIMIT: usize = 8;
const SEMANTIC_LEAD_LIMIT: usize = 4;
const SEMANTIC_LEAD_CHARS: usize = 600;

#[derive(Debug, Clone)]
pub struct DesignScoutOptions {
    pub task: String,
    pub seeds: Vec<String>,
    pub search_limit: usize,
    pub seed_limit: usize,
    pub file_limit: usize,
    pub graph_depth: usize,
    pub graph_node_limit: usize,
    pub graph_edge_limit: usize,
    pub candidate_limit: usize,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
    pub model: ModelSpec,
    pub reasoning: Option<String>,
    pub service_tier: Option<String>,
    pub policy: RequestPolicy,
    pub rebuild: bool,
    pub supersedes_artifact_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesignRunConfig {
    pub task: String,
    pub seeds: Vec<String>,
    pub search_limit: usize,
    pub seed_limit: usize,
    pub file_limit: usize,
    pub graph_depth: usize,
    pub graph_node_limit: usize,
    pub graph_edge_limit: usize,
    pub candidate_limit: usize,
    #[serde(default)]
    pub file_roles: Vec<String>,
    #[serde(default = "origin::defaults")]
    pub file_origins: Vec<String>,
    pub context_bytes: usize,
    #[serde(default)]
    pub search_hits: usize,
    #[serde(default)]
    pub semantic_leads: usize,
    #[serde(default)]
    pub candidates_selected: usize,
    #[serde(default)]
    pub evidence_files_selected: usize,
    #[serde(default)]
    pub graph_nodes_visited: usize,
    #[serde(default)]
    pub graph_edges_traversed: usize,
    #[serde(default)]
    pub graph_truncated: bool,
    #[serde(default)]
    pub candidates_truncated: bool,
    #[serde(default)]
    pub files_truncated: bool,
    pub service_tier: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesignPlanReport {
    pub task_key: String,
    pub task: String,
    pub snapshot: String,
    pub seeds: Vec<String>,
    pub candidates: Vec<WorkflowCandidate>,
    pub evidence_files: Vec<String>,
    pub evidence_file_policy: Vec<DesignEvidenceFile>,
    pub search_hits: usize,
    pub semantic_leads: usize,
    pub graph_nodes_visited: usize,
    pub graph_edges_traversed: usize,
    pub graph_truncated: bool,
    pub candidates_truncated: bool,
    pub files_truncated: bool,
    pub search_retrieval: search::RetrievalStatus,
    pub semantic_retrieval: Option<semantic::ArtifactRetrievalStatus>,
    pub planning_version: &'static str,
    pub bounds: DesignBounds,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesignBounds {
    pub search_results: usize,
    pub seeds: usize,
    pub evidence_files: usize,
    pub graph_depth: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub candidates: usize,
    pub context_bytes: usize,
    pub model_calls: usize,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesignEvidenceFile {
    pub path: String,
    pub role: String,
    pub origin: String,
    pub runtime_evidence: bool,
}

pub struct DesignPlan {
    pub report: DesignPlanReport,
    evidence: EvidencePack,
}

pub(super) struct PreparedDesign {
    plan: DesignPlan,
    request: CompleteRequest,
    pub(super) spec: RunSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvidenceRef {
    pub anchor: String,
    pub start_line: i64,
    pub end_line: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    pub text: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetectionSignal {
    pub signal: String,
    pub channel: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Touchpoint {
    pub anchor: String,
    pub responsibility: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Propagation {
    pub from_anchor: String,
    pub to_anchor: String,
    pub description: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    pub resolution: String,
    #[serde(default)]
    pub candidate_mechanisms: Vec<Claim>,
    #[serde(default)]
    pub selected_mechanism: Option<Claim>,
    #[serde(default)]
    pub detection_signals: Vec<DetectionSignal>,
    #[serde(default)]
    pub cure_semantics: Option<Claim>,
    #[serde(default)]
    pub touchpoints: Vec<Touchpoint>,
    #[serde(default)]
    pub propagation: Vec<Propagation>,
    #[serde(default)]
    pub invariants: Vec<Claim>,
    #[serde(default)]
    pub validation_oracle: Vec<Claim>,
    #[serde(default)]
    pub risks: Vec<Claim>,
    #[serde(default)]
    pub unresolved_questions: Vec<Claim>,
    #[serde(default)]
    pub unresolved_reason: Option<Claim>,
    #[serde(default)]
    pub incomplete_reason: Option<String>,
}

pub struct ValidatedDesign {
    pub body: Value,
    pub supports: Vec<SupportInput>,
    pub confidence: String,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DesignSelector {
    Id(i64),
    Task(String),
}

pub fn normalize_task(task: &str) -> Result<String> {
    let normalized = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        bail!("design task must not be empty");
    }
    if normalized.chars().count() < 3 {
        bail!("design task must contain at least 3 characters");
    }
    if normalized.chars().count() > MAX_TASK_CHARS {
        bail!("design task exceeds {MAX_TASK_CHARS} characters");
    }
    Ok(normalized)
}

pub fn task_key(task: &str) -> Result<String> {
    let normalized = normalize_task(task)?;
    Ok(format!(
        "task:{}",
        blake3::hash(normalized.as_bytes()).to_hex()
    ))
}

pub fn plan(
    root: &Path,
    conn: &Connection,
    provider: Option<&embed::Provider>,
    options: &DesignScoutOptions,
) -> Result<DesignPlan> {
    validate_options(options)?;
    store::with_read_snapshot(conn, "jscout_design_plan", || {
        plan_in_snapshot(root, conn, provider, options)
    })
}

fn plan_in_snapshot(
    root: &Path,
    conn: &Connection,
    provider: Option<&embed::Provider>,
    options: &DesignScoutOptions,
) -> Result<DesignPlan> {
    let task = normalize_task(&options.task)?;
    let task_key = task_key(&task)?;
    let snapshot = structural::current_snapshot(conn)?;
    let search_result = search::search(
        conn,
        provider,
        &task,
        &search::SearchOptions {
            limit: options.search_limit,
            file_roles: options.file_roles.clone(),
            file_origins: options.file_origins.clone(),
            response_byte_limit: options.policy.context_bytes.max(64_000),
            include_memory: true,
            memory_limit: SEMANTIC_LEAD_LIMIT,
            compact: false,
            ..search::SearchOptions::default()
        },
    )?;
    if search_result.snapshot != snapshot {
        bail!("structural snapshot changed during design localization");
    }

    let mut seeds = Vec::new();
    let mut seen_seeds = HashSet::new();
    for seed in &options.seeds {
        if !seed.starts_with("sym:") {
            bail!("design seeds must be exact current `sym:` anchors, received `{seed}`");
        }
        let resolved =
            structural::resolve_current_anchor_in_origins(conn, seed, &options.file_origins)?;
        if resolved != *seed {
            bail!("design seeds must be exact current anchors; use `{resolved}`");
        }
        let candidate = semantic::symbol_candidate(root, conn, &resolved)?
            .with_context(|| format!("design seed `{resolved}` is not a file-backed symbol"))?;
        let (allowed, _) = file_allowed(conn, &candidate.file, options)?;
        if !allowed {
            bail!("design seed `{resolved}` is excluded by the file-role/origin policy");
        }
        if seen_seeds.insert(resolved.clone()) {
            seeds.push(resolved);
        }
    }
    for hit in &search_result.hits {
        for anchor in &hit.anchors {
            if seeds.len() >= options.seed_limit {
                break;
            }
            if anchor.starts_with("sym:") && seen_seeds.insert(anchor.clone()) {
                seeds.push(anchor.clone());
            }
        }
    }
    if seeds.is_empty() {
        bail!(
            "the task localized no exact symbol seeds; refine the task or provide --seed with a current search anchor"
        );
    }
    let mandatory_seed_anchors = if options.seeds.is_empty() {
        seeds.iter().take(1).cloned().collect::<HashSet<_>>()
    } else {
        options.seeds.iter().cloned().collect::<HashSet<_>>()
    };
    if seeds.len() > options.seed_limit {
        bail!(
            "{} explicit design seeds exceed the configured seed limit {}",
            seeds.len(),
            options.seed_limit
        );
    }
    if seeds.len() > options.graph_node_limit
        || seeds.len() > options.graph_edge_limit
        || seeds.len() > options.candidate_limit
    {
        bail!("graph-node, graph-edge, and candidate limits must each retain every design seed");
    }

    let mut ranked = HashMap::<String, (WorkflowCandidate, bool, usize)>::new();
    let mut graph_nodes_visited = 0_usize;
    let mut graph_edges_traversed = 0_usize;
    let mut graph_truncated = false;
    for (seed_rank, seed) in seeds.iter().enumerate() {
        let seeds_remaining = seeds.len() - seed_rank;
        let nodes_remaining = options.graph_node_limit.saturating_sub(graph_nodes_visited);
        let edges_remaining = options
            .graph_edge_limit
            .saturating_sub(graph_edges_traversed);
        let per_seed_nodes = nodes_remaining.div_ceil(seeds_remaining).max(1);
        let per_seed_edges = edges_remaining.div_ceil(seeds_remaining).max(1);
        let neighborhood = structural::workflow_neighborhood(
            conn,
            seed,
            options.graph_depth,
            per_seed_nodes,
            per_seed_edges,
            &options.file_origins,
        )?;
        graph_nodes_visited = graph_nodes_visited.saturating_add(neighborhood.nodes.len());
        graph_edges_traversed = graph_edges_traversed.saturating_add(neighborhood.traversed_edges);
        graph_truncated |= neighborhood.truncated;
        for node in neighborhood.nodes {
            if node.kind != "symbol" {
                continue;
            }
            let is_seed = seen_seeds.contains(&node.key);
            let (allowed, runtime) = file_allowed(
                conn,
                node.file
                    .as_deref()
                    .context("file-backed design symbol is missing its file")?,
                options,
            )?;
            if !allowed || (!runtime && !is_seed) {
                continue;
            }
            let Some(mut candidate) = semantic::symbol_candidate(root, conn, &node.key)? else {
                continue;
            };
            candidate.seed = is_seed;
            candidate.relevance = node.relevance;
            retain_candidate(&mut ranked, candidate, runtime, seed_rank);
        }
    }

    // Retrieval hits may contribute an oracle/test/config symbol outside the
    // runtime-only workflow plane. They remain role-labelled in the evidence
    // pack and rank after runtime candidates unless they are explicit seeds.
    for (rank, hit) in search_result.hits.iter().enumerate() {
        for anchor in &hit.anchors {
            let Some(mut candidate) = semantic::symbol_candidate(root, conn, anchor)? else {
                continue;
            };
            candidate.seed = seen_seeds.contains(anchor);
            candidate.relevance = 0.75 / (rank + 1) as f64;
            let (allowed, runtime) = file_allowed(conn, &candidate.file, options)?;
            if !allowed {
                continue;
            }
            retain_candidate(&mut ranked, candidate, runtime, seeds.len() + rank);
        }
    }

    // A seed must survive even if the workflow traversal stopped at a policy
    // boundary before returning it.
    for (rank, seed) in seeds.iter().enumerate() {
        if ranked.contains_key(seed) {
            continue;
        }
        let mut candidate = semantic::symbol_candidate(root, conn, seed)?
            .with_context(|| format!("design seed `{seed}` disappeared"))?;
        candidate.seed = true;
        candidate.relevance = 1.0;
        retain_candidate(&mut ranked, candidate, true, rank);
    }

    let mut ranked = ranked.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .seed
            .cmp(&left.0.seed)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.0.relevance.total_cmp(&left.0.relevance))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.anchor.cmp(&right.0.anchor))
    });
    let mut candidates_truncated = ranked.len() > options.candidate_limit;
    ranked.truncate(options.candidate_limit);

    let seed_files = ranked
        .iter()
        .filter(|(candidate, _, _)| candidate.seed)
        .map(|(candidate, _, _)| candidate.file.as_str())
        .collect::<HashSet<_>>();
    if seed_files.len() > options.file_limit {
        bail!(
            "design seed files ({}) exceed the configured file limit {}",
            seed_files.len(),
            options.file_limit
        );
    }
    let mut files = HashSet::new();
    let mut candidates = Vec::new();
    let mut files_truncated = false;
    for (candidate, _, _) in ranked {
        if files.contains(candidate.file.as_str()) || files.len() < options.file_limit {
            files.insert(candidate.file.clone());
            candidates.push(candidate);
        } else {
            files_truncated = true;
        }
    }
    if candidates.is_empty() {
        bail!("design localization produced no evidence candidates");
    }

    // `context_bytes` is an input-selection budget, not merely a late refusal
    // threshold. Drop the weakest optional candidates until the complete
    // request fits. Explicit seeds always survive; when localization supplied
    // every seed, the strongest one is retained as the mandatory root.
    let (mut evidence, mut evidence_file_policy) = build_design_evidence(
        root,
        conn,
        &candidates,
        &seeds,
        &search_result.semantic_artifacts,
    )?;
    loop {
        let mut request = build_request_parts(&task, &candidates, &evidence, options)?;
        let (_, request_bytes) =
            super::reserve_output_and_measure(&mut request, candidates.len().max(1), None)?;
        if request_bytes <= options.policy.context_bytes {
            break;
        }
        let Some(remove_at) = candidates
            .iter()
            .rposition(|candidate| !mandatory_seed_anchors.contains(&candidate.anchor))
        else {
            bail!(
                "mandatory design evidence requires {request_bytes} bytes, exceeding the configured context budget {}; increase context_bytes or use narrower seed declarations",
                options.policy.context_bytes
            );
        };
        let removed = candidates.remove(remove_at);
        if removed.seed {
            seeds.retain(|seed| seed != &removed.anchor);
        }
        candidates_truncated = true;
        if !candidates
            .iter()
            .any(|candidate| candidate.file == removed.file)
        {
            files_truncated = true;
        }
        (evidence, evidence_file_policy) = build_design_evidence(
            root,
            conn,
            &candidates,
            &seeds,
            &search_result.semantic_artifacts,
        )?;
    }

    let report = DesignPlanReport {
        task_key,
        task,
        snapshot,
        seeds,
        candidates,
        evidence_files: evidence.files.keys().cloned().collect(),
        evidence_file_policy,
        search_hits: search_result.hits.len(),
        semantic_leads: search_result.semantic_artifacts.len(),
        graph_nodes_visited,
        graph_edges_traversed,
        graph_truncated,
        candidates_truncated,
        files_truncated,
        search_retrieval: search_result.retrieval,
        semantic_retrieval: search_result.semantic_retrieval,
        planning_version: PLANNING_VERSION,
        bounds: DesignBounds {
            search_results: options.search_limit,
            seeds: options.seed_limit,
            evidence_files: options.file_limit,
            graph_depth: options.graph_depth,
            graph_nodes: options.graph_node_limit,
            graph_edges: options.graph_edge_limit,
            candidates: options.candidate_limit,
            context_bytes: options.policy.context_bytes,
            model_calls: 1,
            file_roles: options.file_roles.clone(),
            file_origins: options.file_origins.clone(),
        },
    };
    Ok(DesignPlan { report, evidence })
}

fn retain_candidate(
    candidates: &mut HashMap<String, (WorkflowCandidate, bool, usize)>,
    candidate: WorkflowCandidate,
    runtime: bool,
    order: usize,
) {
    candidates
        .entry(candidate.anchor.clone())
        .and_modify(|existing| {
            existing.0.seed |= candidate.seed;
            existing.0.relevance = existing.0.relevance.max(candidate.relevance);
            existing.1 |= runtime;
            existing.2 = existing.2.min(order);
        })
        .or_insert((candidate, runtime, order));
}

fn file_allowed(
    conn: &Connection,
    path: &str,
    options: &DesignScoutOptions,
) -> Result<(bool, bool)> {
    let (role, file_origin): (Option<String>, String) = conn.query_row(
        "SELECT role, origin FROM files WHERE path=?1",
        [path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let allowed = options
        .file_origins
        .iter()
        .any(|value| value == &file_origin)
        && (options.file_roles.is_empty()
            || role
                .as_ref()
                .is_some_and(|role| options.file_roles.iter().any(|value| value == role)));
    let runtime = recon::effective_runtime(conn, Some(path), role.as_deref())?;
    Ok((allowed, runtime))
}

fn semantic_lead_preview(body: &Value) -> String {
    let preferred = body
        .get("description")
        .or_else(|| body.get("purpose"))
        .or_else(|| body.get("overview"))
        .or_else(|| body.get("definition"))
        .or_else(|| body.get("claim"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    let mut preview = preferred
        .chars()
        .take(SEMANTIC_LEAD_CHARS)
        .collect::<String>();
    if preferred.chars().count() > SEMANTIC_LEAD_CHARS {
        preview.push('…');
    }
    preview
}

fn build_design_evidence(
    root: &Path,
    conn: &Connection,
    candidates: &[WorkflowCandidate],
    seeds: &[String],
    semantic_artifacts: &[semantic::SemanticArtifact],
) -> Result<(EvidencePack, Vec<DesignEvidenceFile>)> {
    let mut evidence =
        evidence::build_titled_design(root, conn, candidates, "Design evidence candidates")?;
    let mut evidence_file_policy = Vec::new();
    for path in evidence.files.keys() {
        let (role, file_origin): (String, String) = conn.query_row(
            "SELECT role, origin FROM files WHERE path=?1",
            [path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        evidence_file_policy.push(DesignEvidenceFile {
            path: path.clone(),
            runtime_evidence: recon::effective_runtime(conn, Some(path), Some(&role))?,
            role,
            origin: file_origin,
        });
    }
    evidence.rendered.push_str(
        "\n## Evidence file policy\n\nRuntime evidence may support implementation claims. Test, fixture, generated, or documentary evidence may establish behavior or the oracle but cannot alone prove an implementation claim.\n",
    );
    for file in &evidence_file_policy {
        evidence.rendered.push_str(&format!(
            "- {}: role={}, origin={}, runtime_evidence={}\n",
            file.path, file.role, file.origin, file.runtime_evidence
        ));
    }
    evidence.rendered.push_str(
        "\n## Structural context\n\nThese are deterministic likely/certain graph facts for the strongest seeds. They are localization context, not model-authored claims.\n",
    );
    for seed in seeds.iter().take(STRUCTURAL_CONTEXT_ANCHOR_LIMIT) {
        let (context, _) = evidence::structural_context(conn, seed)?;
        evidence
            .rendered
            .push_str(&format!("\n### Seed `{seed}`\n{context}"));
    }
    if !semantic_artifacts.is_empty() {
        evidence.rendered.push_str(
            "\n## Existing semantic leads\n\nUntrusted repository memory for localization only. Every design claim still requires exact numbered source evidence.\n",
        );
        for artifact in semantic_artifacts.iter().take(SEMANTIC_LEAD_LIMIT) {
            evidence.rendered.push_str(&format!(
                "- artifact {} [{}; {}] {}: {}\n",
                artifact.id,
                artifact.artifact_type,
                artifact.freshness,
                artifact.name.as_deref().unwrap_or("unnamed"),
                semantic_lead_preview(&artifact.body),
            ));
        }
    }
    Ok((evidence, evidence_file_policy))
}

fn validate_options(options: &DesignScoutOptions) -> Result<()> {
    normalize_task(&options.task)?;
    crate::file_role::validate_all(&options.file_roles)?;
    origin::validate_all(&options.file_origins)?;
    for (name, value, maximum) in [
        ("search limit", options.search_limit, MAX_SEARCH_LIMIT),
        ("seed limit", options.seed_limit, MAX_SEED_LIMIT),
        ("file limit", options.file_limit, MAX_FILE_LIMIT),
        (
            "graph node limit",
            options.graph_node_limit,
            MAX_GRAPH_NODE_LIMIT,
        ),
        (
            "graph edge limit",
            options.graph_edge_limit,
            MAX_GRAPH_EDGE_LIMIT,
        ),
        (
            "candidate limit",
            options.candidate_limit,
            MAX_CANDIDATE_LIMIT,
        ),
    ] {
        if value == 0 || value > maximum {
            bail!("design {name} must be between 1 and {maximum}");
        }
    }
    if options.graph_depth == 0 || options.graph_depth > 3 {
        bail!("design graph depth must be between 1 and 3");
    }
    if options.seeds.len() > options.seed_limit {
        bail!("explicit design seeds exceed the configured seed limit");
    }
    Ok(())
}

pub fn dry_run_report(plan: &DesignPlan, options: &DesignScoutOptions) -> Result<Value> {
    let mut request = build_request(plan, options)?;
    let (_, request_bytes) =
        super::reserve_output_and_measure(&mut request, plan.report.candidates.len().max(1), None)?;
    Ok(json!({
        "dry_run": true,
        "would_call": request_bytes <= options.policy.context_bytes,
        "calls_planned": usize::from(request_bytes <= options.policy.context_bytes),
        "request_bytes": request_bytes,
        "context_bytes": options.policy.context_bytes,
        "model": options.model.spec,
        "plan": plan.report,
        "notes": [
            "dry-run makes no model call and writes no ledger row",
            "model context-window validation runs when the gateway is available",
            "every limit is explicit; truncation is reported in the plan",
        ],
    }))
}

pub fn scout(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &DesignScoutOptions,
    plan: DesignPlan,
) -> Result<ScoutReport> {
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;
    let prepared = prepare(gateway, &mut PreparationCache::default(), plan, options)?;
    execute(root, conn, gateway, options, prepared, true)
}

pub(super) fn prepare(
    gateway: &mut dyn LlmGateway,
    cache: &mut PreparationCache,
    plan: DesignPlan,
    options: &DesignScoutOptions,
) -> Result<PreparedDesign> {
    let mut request = build_request(&plan, options)?;
    let capabilities = cache.model(gateway, &options.model)?;
    super::enforce_context_budget(
        &capabilities,
        &mut request,
        plan.evidence.files.len(),
        plan.report.candidates.len().max(1),
        &options.policy,
        &options.model.spec,
    )?;
    let fingerprint = input_fingerprint(&plan, &request, options, capabilities.base_url.as_deref());
    let request_hash = blake3::hash(serde_json::to_string(&request)?.as_bytes())
        .to_hex()
        .to_string();
    let config_json = serde_json::to_string(&DesignRunConfig {
        task: plan.report.task.clone(),
        seeds: plan.report.seeds.clone(),
        search_limit: options.search_limit,
        seed_limit: options.seed_limit,
        file_limit: options.file_limit,
        graph_depth: options.graph_depth,
        graph_node_limit: options.graph_node_limit,
        graph_edge_limit: options.graph_edge_limit,
        candidate_limit: options.candidate_limit,
        file_roles: options.file_roles.clone(),
        file_origins: options.file_origins.clone(),
        context_bytes: options.policy.context_bytes,
        search_hits: plan.report.search_hits,
        semantic_leads: plan.report.semantic_leads,
        candidates_selected: plan.report.candidates.len(),
        evidence_files_selected: plan.report.evidence_files.len(),
        graph_nodes_visited: plan.report.graph_nodes_visited,
        graph_edges_traversed: plan.report.graph_edges_traversed,
        graph_truncated: plan.report.graph_truncated,
        candidates_truncated: plan.report.candidates_truncated,
        files_truncated: plan.report.files_truncated,
        service_tier: options.service_tier.clone(),
        base_url: capabilities.base_url.clone(),
    })?;
    let spec = RunSpec {
        scout_kind: "design".into(),
        gateway_protocol: PROTOCOL_VERSION,
        provider: options.model.provider.clone(),
        model: options.model.model_id.clone(),
        billing_path: cache.billing_path(gateway, &options.model)?,
        reasoning: options.reasoning.clone(),
        prompt_version: PROMPT_VERSION.into(),
        source_snapshot: plan.report.snapshot.clone(),
        input_fingerprint: fingerprint,
        request_hash,
        config_json,
        supersedes_artifact_id: options.supersedes_artifact_id,
    };
    Ok(PreparedDesign {
        plan,
        request,
        spec,
    })
}

pub(super) fn execute(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &DesignScoutOptions,
    prepared: PreparedDesign,
    allow_new_call: bool,
) -> Result<ScoutReport> {
    let PreparedDesign {
        plan,
        request,
        spec,
    } = prepared;
    let subject = plan.report.task_key.clone();
    let planned_predecessor = current_design_for_key(conn, &subject)?;
    if let Some(expected) = options.supersedes_artifact_id
        && planned_predecessor != Some(expected)
    {
        bail!(
            "design task lineage changed before refresh (expected artifact {expected}, current {:?})",
            planned_predecessor
        );
    }
    if !allow_new_call && (options.rebuild || ledger::reusable_run(conn, &spec)?.is_none()) {
        bail!("design call budget exhausted before a non-reusable run");
    }
    let input_fingerprint = spec.input_fingerprint.clone();
    let (run_id, claimed_predecessor) = match ledger::claim_run(conn, &spec, options.rebuild)? {
        RunClaim::Reused(run_id) => {
            return reused(
                conn,
                run_id,
                "design",
                subject,
                plan.report.candidates.len(),
                &spec,
            );
        }
        RunClaim::Claimed {
            run_id,
            supersedes_artifact_id,
        } => (run_id, supersedes_artifact_id),
    };
    if claimed_predecessor.is_some() && claimed_predecessor != planned_predecessor {
        ledger::finish_run(
            conn,
            run_id,
            RunOutcome::Incomplete,
            None,
            Some("inputs_changed"),
        )?;
        bail!("design lineage changed while claiming the run");
    }

    let outcome = match gateway.complete(&request, options.policy.timeout) {
        Ok(outcome) => outcome,
        Err(error) => {
            let (status, code) = match &error {
                GatewayError::Canceled(_) => (RunOutcome::Canceled, error.code()),
                other => (RunOutcome::Failed, other.code()),
            };
            ledger::finish_run(conn, run_id, status, None, Some(&code))?;
            if remote_timeout(&error) {
                return Ok(gateway_timeout_report(
                    run_id,
                    &spec,
                    subject,
                    plan.report.candidates.len(),
                    error,
                ));
            }
            return Err(anyhow::Error::from(error)).context("gateway completion failed");
        }
    };
    let usage_json = serde_json::to_string(&json!({
        "usage": outcome.usage,
        "stop_reason": outcome.stop_reason,
        "attempts": outcome.attempts,
        "response_model": outcome.response_model,
        "base_url": outcome.started.base_url,
    }))?;
    conn.execute(
        "UPDATE scout_runs SET billing_path=?2 WHERE id=?1",
        rusqlite::params![run_id, outcome.started.billing_path],
    )?;

    let submission: Submission = match serde_json::from_value(outcome.tool_call.arguments.clone()) {
        Ok(submission) if outcome.tool_call.name == SUBMIT_TOOL_NAME => submission,
        Ok(_) => {
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Failed,
                Some(&usage_json),
                Some("tool_contract"),
            )?;
            return Ok(failed_report(
                run_id,
                "design",
                subject,
                plan.report.candidates.len(),
                outcome.usage,
                outcome.started,
                format!(
                    "model called an unexpected tool `{}`",
                    outcome.tool_call.name
                ),
            ));
        }
        Err(error) => {
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Failed,
                Some(&usage_json),
                Some("schema"),
            )?;
            return Ok(failed_report(
                run_id,
                "design",
                subject,
                plan.report.candidates.len(),
                outcome.usage,
                outcome.started,
                format!("submission does not match the design output contract: {error}"),
            ));
        }
    };

    let validated = match validate(conn, &submission, &plan.report.candidates, &plan.evidence) {
        Ok(validated) => validated,
        Err(error) => {
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Failed,
                Some(&usage_json),
                Some("validation"),
            )?;
            return Ok(failed_report(
                run_id,
                "design",
                subject,
                plan.report.candidates.len(),
                outcome.usage,
                outcome.started,
                format!("submission failed design validation: {error:#}"),
            ));
        }
    };
    if let Some(reason) = &validated.incomplete {
        publish_terminal(
            conn,
            run_id,
            RunOutcome::Incomplete,
            &usage_json,
            Some("model_incomplete"),
            &validated.classifications,
        )?;
        return Ok(scout_report(
            run_id,
            "incomplete",
            "design",
            subject,
            plan.report.candidates.len(),
            None,
            &validated.classifications,
            Some(outcome.usage),
            Some(outcome.started),
            Some(reason.clone()),
        ));
    }

    let annotate_input = AnnotateInput {
        artifact_type: "design".into(),
        name: Some(plan.report.task_key.clone()),
        body: validated.body,
        supports: validated.supports,
        confidence: validated.confidence,
        snapshot: plan.report.snapshot.clone(),
        supersedes: claimed_predecessor.or(planned_predecessor),
    };
    let (snapshot, supports) = match semantic::validate_annotate_input(root, conn, &annotate_input)
    {
        Ok(parts) => parts,
        Err(error) => {
            publish_terminal(
                conn,
                run_id,
                RunOutcome::Incomplete,
                &usage_json,
                Some("inputs_changed"),
                &validated.classifications,
            )?;
            return Err(error).context(
                "repository changed between design evidence construction and publication; re-index and retry",
            );
        }
    };

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let published = (|| -> Result<i64> {
        if structural::current_snapshot(conn)? != plan.report.snapshot {
            bail!("structural snapshot changed during design publication");
        }
        for (file, evidence) in &plan.evidence.files {
            let indexed: String =
                conn.query_row("SELECT hash FROM files WHERE path=?1", [file], |row| {
                    row.get(0)
                })?;
            if indexed != evidence.hash {
                bail!("design evidence file `{file}` changed during publication");
            }
        }
        if current_design_for_key(conn, &plan.report.task_key)? != planned_predecessor {
            bail!("another design changed this task lineage during publication");
        }
        let artifact_id = semantic::persist_validated_artifact(
            conn,
            &annotate_input,
            &snapshot,
            &supports,
            &[],
            &semantic::ArtifactProvenance {
                model: &options.model.spec,
                prompt_version: PROMPT_VERSION,
                scout_run_id: Some(run_id),
                input_fingerprint: Some(&input_fingerprint),
            },
        )?;
        if let Some(previous) = annotate_input.supersedes {
            ledger::retire_generating_run(conn, previous)?;
        }
        ledger::record_classifications(conn, run_id, &validated.classifications)?;
        ledger::finish_run(conn, run_id, RunOutcome::Completed, Some(&usage_json), None)?;
        Ok(artifact_id)
    })();
    let artifact_id = match published {
        Ok(artifact_id) => {
            conn.execute_batch("COMMIT")?;
            artifact_id
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Incomplete,
                Some(&usage_json),
                Some("publication_recheck"),
            )?;
            return Err(error).context("design publication recheck failed; nothing was published");
        }
    };

    Ok(scout_report(
        run_id,
        "completed",
        "design",
        subject,
        plan.report.candidates.len(),
        Some(artifact_id),
        &validated.classifications,
        Some(outcome.usage),
        Some(outcome.started),
        None,
    ))
}

fn current_design_for_key(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT artifact.id FROM semantic_artifacts artifact
             WHERE artifact.artifact_type='design' AND artifact.canonical_name=?1
               AND NOT EXISTS(
                 SELECT 1 FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id
               )
             ORDER BY artifact.id DESC LIMIT 1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

fn build_request(plan: &DesignPlan, options: &DesignScoutOptions) -> Result<CompleteRequest> {
    build_request_parts(
        &plan.report.task,
        &plan.report.candidates,
        &plan.evidence,
        options,
    )
}

fn build_request_parts(
    task: &str,
    candidates: &[WorkflowCandidate],
    evidence: &EvidencePack,
    options: &DesignScoutOptions,
) -> Result<CompleteRequest> {
    let anchors = candidates
        .iter()
        .map(|candidate| candidate.anchor.clone())
        .collect::<Vec<_>>();
    let system = "You are the read-only design phase for a coding task. Produce a defect/feature mechanism and an implementation design, never a patch. Do not write replacement code or a diff. Work in this order: competing mechanisms and falsification evidence; selected mechanism or an explicit unresolved set; runtime detection signals and their observation channels; cure semantics and invariants; affected symbol touchpoints; cross-file state/control/data propagation; validation oracle and risks. Every claim must cite exact numbered source evidence from the supplied candidate files. Test evidence may establish required behavior but cannot by itself prove an implementation claim. Use only anchors from the candidate list. If the evidence cannot support a useful design, return an incomplete_reason and no claims."
        .to_string();
    let user = format!(
        "Task:\n{}\n\nCandidate anchors are closed to this list:\n{}\n\nProduce the design before any editing begins.\n\n{}",
        task,
        anchors
            .iter()
            .map(|anchor| format!("- {anchor}"))
            .collect::<Vec<_>>()
            .join("\n"),
        evidence.rendered,
    );
    Ok(CompleteRequest {
        model: options.model.spec.clone(),
        reasoning: options.reasoning.clone(),
        system: Some(system),
        messages: vec![ChatMessage {
            role: "user",
            content: user,
        }],
        tool: SubmitTool {
            name: SUBMIT_TOOL_NAME.into(),
            description: "Submit an evidence-backed task design without code or diffs".into(),
            parameters: submit_tool_schema(&anchors),
        },
        timeout_ms: Some(options.policy.timeout.as_millis() as u64),
        max_tokens: None,
        session_id: None,
        provider_options: options.service_tier.as_ref().map(|tier| ProviderOptions {
            service_tier: Some(tier.clone()),
        }),
    })
}

fn input_fingerprint(
    plan: &DesignPlan,
    request: &CompleteRequest,
    options: &DesignScoutOptions,
    base_url: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-design-scout-input-v1\0");
    for part in [
        plan.report.task.as_str(),
        &plan.report.seeds.join("\u{1}"),
        &plan
            .report
            .candidates
            .iter()
            .map(|candidate| candidate.anchor.as_str())
            .collect::<Vec<_>>()
            .join("\u{1}"),
        &plan.evidence.rendered,
        PLANNING_VERSION,
        PROMPT_VERSION,
        &options.model.spec,
        options.reasoning.as_deref().unwrap_or(""),
        options.service_tier.as_deref().unwrap_or(""),
        base_url.unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    for value in [
        options.search_limit,
        options.seed_limit,
        options.file_limit,
        options.graph_depth,
        options.graph_node_limit,
        options.graph_edge_limit,
        options.candidate_limit,
    ] {
        hasher.update(&value.to_le_bytes());
    }
    for value in &options.file_roles {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    for value in &options.file_origins {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(&PROTOCOL_VERSION.to_le_bytes());
    hasher.update(&request.max_tokens.unwrap_or_default().to_le_bytes());
    if let Ok(schema) = serde_json::to_string(&request.tool.parameters) {
        hasher.update(schema.as_bytes());
    }
    if let Some(system) = &request.system {
        hasher.update(system.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub fn submit_tool_schema(anchors: &[String]) -> Value {
    let claim = claim_schema(10, MAX_CLAIM_CHARS, anchors);
    let short_claim = claim_schema(3, MAX_SHORT_CLAIM_CHARS, anchors);
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resolution"],
        "properties": {
            "resolution": { "type": "string", "enum": ["resolved", "unresolved", "incomplete"] },
            "candidate_mechanisms": { "type": "array", "maxItems": 4, "items": claim.clone() },
            "selected_mechanism": { "anyOf": [claim.clone(), { "type": "null" }] },
            "detection_signals": {
                "type": "array", "maxItems": 6,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["signal", "channel", "evidence"],
                    "properties": {
                        "signal": { "type": "string", "minLength": 3, "maxLength": MAX_SHORT_CLAIM_CHARS },
                        "channel": { "type": "string", "minLength": 3, "maxLength": MAX_SHORT_CLAIM_CHARS },
                        "evidence": evidence_schema(anchors),
                    }
                }
            },
            "cure_semantics": { "anyOf": [claim.clone(), { "type": "null" }] },
            "touchpoints": {
                "type": "array", "maxItems": 12,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["anchor", "responsibility", "evidence"],
                    "properties": {
                        "anchor": { "type": "string", "enum": anchors },
                        "responsibility": { "type": "string", "minLength": 3, "maxLength": MAX_SHORT_CLAIM_CHARS },
                        "evidence": evidence_schema(anchors),
                    }
                }
            },
            "propagation": {
                "type": "array", "maxItems": 8,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["from_anchor", "to_anchor", "description", "evidence"],
                    "properties": {
                        "from_anchor": { "type": "string", "enum": anchors },
                        "to_anchor": { "type": "string", "enum": anchors },
                        "description": { "type": "string", "minLength": 3, "maxLength": MAX_SHORT_CLAIM_CHARS },
                        "evidence": evidence_schema(anchors),
                    }
                }
            },
            "invariants": { "type": "array", "maxItems": MAX_ITEMS, "items": short_claim.clone() },
            "validation_oracle": { "type": "array", "maxItems": MAX_ITEMS, "items": short_claim.clone() },
            "risks": { "type": "array", "maxItems": 6, "items": short_claim.clone() },
            "unresolved_questions": { "type": "array", "maxItems": 6, "items": short_claim },
            "unresolved_reason": { "anyOf": [claim, { "type": "null" }] },
            "incomplete_reason": { "type": ["string", "null"], "maxLength": MAX_INCOMPLETE_REASON_CHARS },
        }
    })
}

fn claim_schema(min_length: usize, max_length: usize, anchors: &[String]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["text", "evidence"],
        "properties": {
            "text": { "type": "string", "minLength": min_length, "maxLength": max_length },
            "evidence": evidence_schema(anchors),
        }
    })
}

fn evidence_schema(anchors: &[String]) -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "maxItems": MAX_EVIDENCE_PER_CLAIM,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["anchor", "start_line", "end_line"],
            "properties": {
                "anchor": { "type": "string", "enum": anchors },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
            }
        }
    })
}

pub fn validate(
    conn: &Connection,
    submission: &Submission,
    candidates: &[WorkflowCandidate],
    evidence: &EvidencePack,
) -> Result<ValidatedDesign> {
    if let Some(reason) = submission.incomplete_reason.as_deref() {
        let reason = reason.trim();
        if submission.resolution != "incomplete" || reason.is_empty() {
            bail!("incomplete_reason requires resolution `incomplete` and a non-empty reason");
        }
        if !(3..=MAX_INCOMPLETE_REASON_CHARS).contains(&reason.chars().count()) {
            bail!("incomplete_reason must contain 3..={MAX_INCOMPLETE_REASON_CHARS} characters");
        }
        if has_claims(submission) {
            bail!("an incomplete design must not carry design claims");
        }
        return Ok(ValidatedDesign {
            body: json!({}),
            supports: Vec::new(),
            confidence: "possible".into(),
            classifications: candidates
                .iter()
                .map(|candidate| ClassificationRow {
                    anchor_key: candidate.anchor.clone(),
                    decision: "excluded".into(),
                    role: None,
                    evidence_json: json!({ "reason": reason }).to_string(),
                })
                .collect(),
            incomplete: Some(reason.to_string()),
        });
    }
    if submission.resolution == "incomplete" {
        bail!("resolution `incomplete` requires incomplete_reason");
    }
    if !matches!(submission.resolution.as_str(), "resolved" | "unresolved") {
        bail!("design resolution must be resolved, unresolved, or incomplete");
    }
    for (label, count, maximum) in [
        (
            "candidate mechanisms",
            submission.candidate_mechanisms.len(),
            4,
        ),
        ("detection signals", submission.detection_signals.len(), 6),
        ("touchpoints", submission.touchpoints.len(), 12),
        ("propagation steps", submission.propagation.len(), 8),
        ("invariants", submission.invariants.len(), MAX_ITEMS),
        (
            "validation oracles",
            submission.validation_oracle.len(),
            MAX_ITEMS,
        ),
        ("risks", submission.risks.len(), 6),
        (
            "unresolved questions",
            submission.unresolved_questions.len(),
            6,
        ),
    ] {
        if count > maximum {
            bail!("design allows at most {maximum} {label}");
        }
    }
    if submission.candidate_mechanisms.is_empty() {
        bail!("a design requires at least one candidate mechanism");
    }
    if submission.resolution == "resolved" {
        if submission.selected_mechanism.is_none()
            || submission.cure_semantics.is_none()
            || submission.detection_signals.is_empty()
            || submission.touchpoints.is_empty()
            || submission.validation_oracle.is_empty()
        {
            bail!(
                "a resolved design requires selected mechanism, detection signals, cure semantics, touchpoints, and validation oracle"
            );
        }
        if submission.unresolved_reason.is_some() {
            bail!("a resolved design must not carry unresolved_reason");
        }
    } else {
        if submission.selected_mechanism.is_some() || submission.cure_semantics.is_some() {
            bail!("an unresolved design must not select a mechanism or cure");
        }
        if submission.unresolved_reason.is_none() {
            bail!("an unresolved design requires an evidence-backed unresolved_reason");
        }
    }

    let candidate_by_anchor = candidates
        .iter()
        .map(|candidate| (candidate.anchor.as_str(), candidate))
        .collect::<HashMap<_, _>>();
    let mut body = Map::new();
    body.insert("resolution".into(), json!(submission.resolution));
    let mut supports = Vec::new();
    let mut cited = HashMap::<String, Vec<(i64, i64)>>::new();

    let mechanisms = validate_claim_list(
        "/candidate_mechanisms",
        &submission.candidate_mechanisms,
        &candidate_by_anchor,
        evidence,
        &mut supports,
        &mut cited,
    )?;
    body.insert("candidate_mechanisms".into(), Value::Array(mechanisms));
    if let Some(claim) = &submission.selected_mechanism {
        body.insert(
            "selected_mechanism".into(),
            json!(validate_claim(
                "/selected_mechanism",
                claim,
                &candidate_by_anchor,
                evidence,
                &mut supports,
                &mut cited,
            )?),
        );
    }

    let mut signals = Vec::new();
    for (index, signal) in submission.detection_signals.iter().enumerate() {
        validate_text(&signal.signal, MAX_SHORT_CLAIM_CHARS, "detection signal")?;
        validate_text(&signal.channel, MAX_SHORT_CLAIM_CHARS, "detection channel")?;
        push_supports(
            &format!("/detection_signals/{index}/signal"),
            &signal.evidence,
            &candidate_by_anchor,
            evidence,
            &mut supports,
            &mut cited,
        )?;
        push_supports(
            &format!("/detection_signals/{index}/channel"),
            &signal.evidence,
            &candidate_by_anchor,
            evidence,
            &mut supports,
            &mut cited,
        )?;
        signals.push(json!({ "signal": signal.signal.trim(), "channel": signal.channel.trim() }));
    }
    if !signals.is_empty() {
        body.insert("detection_signals".into(), Value::Array(signals));
    }
    if let Some(claim) = &submission.cure_semantics {
        body.insert(
            "cure_semantics".into(),
            json!(validate_claim(
                "/cure_semantics",
                claim,
                &candidate_by_anchor,
                evidence,
                &mut supports,
                &mut cited,
            )?),
        );
    }

    let mut touchpoints = Vec::new();
    let mut seen_touchpoints = HashSet::new();
    let mut defining = HashMap::<String, String>::new();
    for (index, touchpoint) in submission.touchpoints.iter().enumerate() {
        let candidate = candidate_by_anchor
            .get(touchpoint.anchor.as_str())
            .with_context(|| {
                format!(
                    "touchpoint anchor `{}` is not a candidate",
                    touchpoint.anchor
                )
            })?;
        if !seen_touchpoints.insert(touchpoint.anchor.as_str()) {
            bail!("touchpoint anchor `{}` is duplicated", touchpoint.anchor);
        }
        validate_text(
            &touchpoint.responsibility,
            MAX_SHORT_CLAIM_CHARS,
            "touchpoint responsibility",
        )?;
        if !touchpoint
            .evidence
            .iter()
            .any(|reference| reference.anchor == touchpoint.anchor)
        {
            bail!(
                "touchpoint `{}` requires evidence on its own anchor",
                touchpoint.anchor
            );
        }
        push_supports(
            &format!("/touchpoints/{index}/responsibility"),
            &touchpoint.evidence,
            &candidate_by_anchor,
            evidence,
            &mut supports,
            &mut cited,
        )?;
        defining.insert(
            touchpoint.anchor.clone(),
            touchpoint.responsibility.trim().to_string(),
        );
        touchpoints.push(json!({
            "anchor": candidate.anchor,
            "responsibility": touchpoint.responsibility.trim(),
        }));
    }
    if !touchpoints.is_empty() {
        body.insert("touchpoints".into(), Value::Array(touchpoints));
    }

    let mut propagation = Vec::new();
    let mut seen_propagation = HashSet::new();
    for (index, step) in submission.propagation.iter().enumerate() {
        if !candidate_by_anchor.contains_key(step.from_anchor.as_str())
            || !candidate_by_anchor.contains_key(step.to_anchor.as_str())
        {
            bail!("propagation endpoints must both be candidate anchors");
        }
        if !seen_propagation.insert((step.from_anchor.as_str(), step.to_anchor.as_str())) {
            bail!(
                "propagation step {} -> {} is duplicated",
                step.from_anchor,
                step.to_anchor
            );
        }
        validate_text(
            &step.description,
            MAX_SHORT_CLAIM_CHARS,
            "propagation description",
        )?;
        push_supports(
            &format!("/propagation/{index}/description"),
            &step.evidence,
            &candidate_by_anchor,
            evidence,
            &mut supports,
            &mut cited,
        )?;
        propagation.push(json!({
            "from_anchor": step.from_anchor,
            "to_anchor": step.to_anchor,
            "description": step.description.trim(),
        }));
    }
    if !propagation.is_empty() {
        body.insert("propagation".into(), Value::Array(propagation));
    }

    for (field, claims) in [
        ("invariants", &submission.invariants),
        ("validation_oracle", &submission.validation_oracle),
        ("risks", &submission.risks),
        ("unresolved_questions", &submission.unresolved_questions),
    ] {
        let values = validate_claim_list(
            &format!("/{field}"),
            claims,
            &candidate_by_anchor,
            evidence,
            &mut supports,
            &mut cited,
        )?;
        if !values.is_empty() {
            body.insert(field.into(), Value::Array(values));
        }
    }
    if let Some(claim) = &submission.unresolved_reason {
        body.insert(
            "unresolved_reason".into(),
            json!(validate_claim(
                "/unresolved_reason",
                claim,
                &candidate_by_anchor,
                evidence,
                &mut supports,
                &mut cited,
            )?),
        );
    }

    validate_implementation_evidence(conn, &supports)?;

    let classifications = candidates
        .iter()
        .map(|candidate| {
            if let Some(role) = defining.get(&candidate.anchor) {
                ClassificationRow {
                    anchor_key: candidate.anchor.clone(),
                    decision: "defining".into(),
                    role: Some(role.clone()),
                    evidence_json: serde_json::to_string(
                        cited
                            .get(&candidate.anchor)
                            .cloned()
                            .unwrap_or_default()
                            .as_slice(),
                    )
                    .unwrap_or_else(|_| "[]".into()),
                }
            } else if let Some(ranges) = cited.get(&candidate.anchor) {
                ClassificationRow {
                    anchor_key: candidate.anchor.clone(),
                    decision: "supporting".into(),
                    role: Some("design evidence".into()),
                    evidence_json: serde_json::to_string(ranges).unwrap_or_else(|_| "[]".into()),
                }
            } else {
                ClassificationRow {
                    anchor_key: candidate.anchor.clone(),
                    decision: "excluded".into(),
                    role: None,
                    evidence_json: json!({ "reason": "not cited by the design" }).to_string(),
                }
            }
        })
        .collect();
    Ok(ValidatedDesign {
        body: Value::Object(body),
        supports,
        confidence: if submission.resolution == "resolved" {
            "likely".into()
        } else {
            "possible".into()
        },
        classifications,
        incomplete: None,
    })
}

fn validate_implementation_evidence(conn: &Connection, supports: &[SupportInput]) -> Result<()> {
    let mut claims = supports
        .iter()
        .filter(|support| {
            matches!(
                support.claim_path.as_str(),
                "/selected_mechanism" | "/cure_semantics"
            ) || [
                "/candidate_mechanisms/",
                "/detection_signals/",
                "/touchpoints/",
                "/propagation/",
                "/invariants/",
            ]
            .iter()
            .any(|prefix| support.claim_path.starts_with(prefix))
        })
        .map(|support| support.claim_path.as_str())
        .collect::<Vec<_>>();
    claims.sort_unstable();
    claims.dedup();
    for claim_path in claims {
        let mut has_runtime = false;
        for support in supports
            .iter()
            .filter(|support| support.claim_path == claim_path)
        {
            let role: Option<String> = conn
                .query_row(
                    "SELECT role FROM files WHERE path=?1",
                    [&support.evidence_file],
                    |row| row.get(0),
                )
                .optional()?;
            if recon::effective_runtime(conn, Some(&support.evidence_file), role.as_deref())? {
                has_runtime = true;
                break;
            }
        }
        if !has_runtime {
            bail!(
                "design implementation claim `{claim_path}` requires at least one runtime/production evidence range; tests may establish only behavior or the validation oracle"
            );
        }
    }
    Ok(())
}

fn has_claims(submission: &Submission) -> bool {
    !submission.candidate_mechanisms.is_empty()
        || submission.selected_mechanism.is_some()
        || !submission.detection_signals.is_empty()
        || submission.cure_semantics.is_some()
        || !submission.touchpoints.is_empty()
        || !submission.propagation.is_empty()
        || !submission.invariants.is_empty()
        || !submission.validation_oracle.is_empty()
        || !submission.risks.is_empty()
        || !submission.unresolved_questions.is_empty()
        || submission.unresolved_reason.is_some()
}

fn validate_claim_list(
    path: &str,
    claims: &[Claim],
    candidates: &HashMap<&str, &WorkflowCandidate>,
    evidence: &EvidencePack,
    supports: &mut Vec<SupportInput>,
    cited: &mut HashMap<String, Vec<(i64, i64)>>,
) -> Result<Vec<Value>> {
    claims
        .iter()
        .enumerate()
        .map(|(index, claim)| {
            validate_claim(
                &format!("{path}/{index}"),
                claim,
                candidates,
                evidence,
                supports,
                cited,
            )
            .map(Value::String)
        })
        .collect()
}

fn validate_claim(
    path: &str,
    claim: &Claim,
    candidates: &HashMap<&str, &WorkflowCandidate>,
    evidence: &EvidencePack,
    supports: &mut Vec<SupportInput>,
    cited: &mut HashMap<String, Vec<(i64, i64)>>,
) -> Result<String> {
    validate_text(&claim.text, MAX_CLAIM_CHARS, "design claim")?;
    push_supports(path, &claim.evidence, candidates, evidence, supports, cited)?;
    Ok(claim.text.trim().to_string())
}

fn validate_text(text: &str, maximum: usize, label: &str) -> Result<()> {
    let length = text.trim().chars().count();
    if length < 3 || length > maximum {
        bail!("{label} must contain 3..={maximum} characters");
    }
    Ok(())
}

fn push_supports(
    path: &str,
    references: &[EvidenceRef],
    candidates: &HashMap<&str, &WorkflowCandidate>,
    evidence: &EvidencePack,
    supports: &mut Vec<SupportInput>,
    cited: &mut HashMap<String, Vec<(i64, i64)>>,
) -> Result<()> {
    if references.is_empty() || references.len() > MAX_EVIDENCE_PER_CLAIM {
        bail!("design claim `{path}` requires 1..={MAX_EVIDENCE_PER_CLAIM} evidence ranges");
    }
    let mut seen = HashSet::new();
    for reference in references {
        let candidate = candidates.get(reference.anchor.as_str()).with_context(|| {
            format!(
                "design evidence anchor `{}` is not a candidate",
                reference.anchor
            )
        })?;
        let file = evidence.files.get(&candidate.file).with_context(|| {
            format!(
                "design evidence file `{}` is missing from the pack",
                candidate.file
            )
        })?;
        if reference.start_line <= 0
            || reference.end_line < reference.start_line
            || reference.end_line > file.line_count
        {
            bail!(
                "design evidence {}:{}-{} is outside the numbered source",
                candidate.file,
                reference.start_line,
                reference.end_line
            );
        }
        if !seen.insert((
            reference.anchor.as_str(),
            reference.start_line,
            reference.end_line,
        )) {
            bail!("design claim `{path}` repeats an evidence range");
        }
        supports.push(SupportInput {
            claim_path: path.into(),
            anchor: reference.anchor.clone(),
            role: None,
            evidence_file: candidate.file.clone(),
            evidence_start_line: reference.start_line,
            evidence_end_line: reference.end_line,
            confidence: "likely".into(),
        });
        cited
            .entry(reference.anchor.clone())
            .or_default()
            .push((reference.start_line, reference.end_line));
    }
    Ok(())
}

pub fn render_design(conn: &Connection, artifact_id: i64, response_bytes: usize) -> Result<String> {
    if response_bytes < 512 {
        bail!("design response byte limit must be at least 512");
    }
    let artifact = load_design(conn, DesignSelector::Id(artifact_id))?;
    let config = design_config_for_artifact(conn, artifact.id)?;
    let task = config.task.clone();
    let resolution = artifact
        .body
        .get("resolution")
        .and_then(Value::as_str)
        .unwrap_or("unresolved");
    let mut body = artifact
        .body
        .as_object()
        .cloned()
        .context("design body must be an object")?;
    let mut supports = artifact
        .supports
        .iter()
        .map(|support| {
            json!([
                support.claim_path,
                support.anchor,
                format!(
                    "{}:{}-{}",
                    support.evidence_file, support.evidence_start_line, support.evidence_end_line
                ),
                support.freshness,
            ])
        })
        .collect::<Vec<_>>();
    let mut omitted_supports = 0_usize;
    let mut omitted_body_fields = Vec::new();
    loop {
        let value = json!({
            "design_id": artifact.id,
            "task": task,
            "freshness": artifact.freshness,
            "confidence": artifact.confidence,
            "bounds": {
                "search_results": config.search_limit,
                "seeds": config.seed_limit,
                "evidence_files": config.file_limit,
                "graph_depth": config.graph_depth,
                "graph_nodes": config.graph_node_limit,
                "graph_edges": config.graph_edge_limit,
                "candidates": config.candidate_limit,
                "file_roles": config.file_roles,
                "file_origins": config.file_origins,
                "context_bytes": config.context_bytes,
                "model_calls": 1,
                "response_bytes": response_bytes,
            },
            "localization": {
                "search_hits": config.search_hits,
                "semantic_leads": config.semantic_leads,
                "candidates_selected": config.candidates_selected,
                "evidence_files_selected": config.evidence_files_selected,
                "graph_nodes_visited": config.graph_nodes_visited,
                "graph_edges_traversed": config.graph_edges_traversed,
                "graph_truncated": config.graph_truncated,
                "candidates_truncated": config.candidates_truncated,
                "files_truncated": config.files_truncated,
            },
            "body": body,
            "supports": supports,
            "omitted_supports": omitted_supports,
            "omitted_body_fields": omitted_body_fields,
            "next": {
                "tool": "implementation_brief",
                "arguments": { "design_id": artifact.id }
            },
        });
        if let Some(rendered) = settle_response(value, response_bytes)? {
            return Ok(rendered);
        }
        if supports.pop().is_some() {
            omitted_supports += 1;
            continue;
        }
        let optional_order = if resolution == "unresolved" {
            ["unresolved_questions", "risks", "propagation", ""]
        } else {
            [
                "unresolved_questions",
                "risks",
                "propagation",
                "candidate_mechanisms",
            ]
        };
        if let Some(field) = optional_order
            .into_iter()
            .find(|field| !field.is_empty() && body.remove(*field).is_some())
        {
            omitted_body_fields.push(field);
            continue;
        }
        let minimal = json!({
            "design_id": artifact.id,
            "task": task,
            "freshness": artifact.freshness,
            "confidence": artifact.confidence,
            "design_omitted": true,
            "reason": "the requested response budget cannot contain the design core; activate the artifact with a larger implementation_brief response_bytes budget",
            "next": {
                "tool": "implementation_brief",
                "arguments": { "design_id": artifact.id }
            },
        });
        return settle_response(minimal, response_bytes)?.with_context(|| {
            format!(
                "design response byte limit {response_bytes} is below the minimum artifact handoff envelope"
            )
        });
    }
}

pub fn implementation_brief(
    conn: &Connection,
    selector: DesignSelector,
    response_bytes: usize,
) -> Result<String> {
    if response_bytes < 512 {
        bail!("implementation brief byte limit must be at least 512");
    }
    let artifact = load_design(conn, selector)?;
    let config = design_config_for_artifact(conn, artifact.id)?;
    let task = config.task;
    let current_snapshot = structural::current_snapshot(conn)?;
    let resolution = artifact
        .body
        .get("resolution")
        .and_then(Value::as_str)
        .unwrap_or("unresolved");
    let mut core = Map::new();
    for field in [
        "selected_mechanism",
        "detection_signals",
        "cure_semantics",
        "touchpoints",
        "invariants",
        "validation_oracle",
    ] {
        if let Some(value) = artifact.body.get(field) {
            core.insert(field.into(), value.clone());
        }
    }
    if resolution == "unresolved" {
        if let Some(value) = artifact.body.get("candidate_mechanisms") {
            core.insert("candidate_mechanisms".into(), value.clone());
        }
        if let Some(value) = artifact.body.get("unresolved_reason") {
            core.insert("unresolved_reason".into(), value.clone());
        }
    }
    let mut optional = Map::new();
    for field in [
        "candidate_mechanisms",
        "propagation",
        "risks",
        "unresolved_questions",
    ] {
        if core.contains_key(field) {
            continue;
        }
        if let Some(value) = artifact.body.get(field) {
            optional.insert(field.into(), value.clone());
        }
    }
    let mut followups =
        touchpoint_followups(conn, &artifact, &current_snapshot, &config.file_origins)?;
    let mut omitted_optional = Vec::new();
    let mut omitted_followups = 0_usize;
    loop {
        let value = json!({
            "design_id": artifact.id,
            "task": task,
            "resolution": resolution,
            "freshness": artifact.freshness,
            "historical_design": artifact.freshness != "fresh",
            "confidence": artifact.confidence,
            "design": core,
            "optional": optional,
            "followups": followups,
            "omitted": {
                "optional_fields": omitted_optional,
                "followups": omitted_followups,
            },
        });
        if let Some(rendered) = settle_response(value, response_bytes)? {
            return Ok(rendered);
        }
        let field = [
            "unresolved_questions",
            "risks",
            "candidate_mechanisms",
            "propagation",
        ]
        .into_iter()
        .find(|field| optional.remove(*field).is_some());
        if let Some(field) = field {
            omitted_optional.push(field);
            continue;
        }
        if followups.pop().is_some() {
            omitted_followups += 1;
            continue;
        }
        bail!(
            "implementation brief core cannot fit the complete {response_bytes}-byte response budget"
        );
    }
}

fn load_design(conn: &Connection, selector: DesignSelector) -> Result<semantic::SemanticArtifact> {
    let id = match selector {
        DesignSelector::Id(id) => id,
        DesignSelector::Task(task) => {
            let key = task_key(&task)?;
            current_design_for_key(conn, &key)?
                .with_context(|| format!("no current design exists for task key `{key}`"))?
        }
    };
    let artifact = semantic::load_artifact(conn, id)?
        .with_context(|| format!("design artifact {id} does not exist"))?;
    if artifact.artifact_type != "design" {
        bail!(
            "semantic artifact {id} is `{}`, not a design",
            artifact.artifact_type
        );
    }
    Ok(artifact)
}

fn design_config_for_artifact(conn: &Connection, artifact_id: i64) -> Result<DesignRunConfig> {
    let config_json: String = conn.query_row(
        "SELECT run.config_json FROM semantic_artifacts artifact
         JOIN scout_runs run ON run.id=artifact.scout_run_id
         WHERE artifact.id=?1 AND run.scout_kind='design'",
        [artifact_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&config_json)?)
}

fn touchpoint_followups(
    conn: &Connection,
    artifact: &semantic::SemanticArtifact,
    current_snapshot: &str,
    file_origins: &[String],
) -> Result<Vec<Value>> {
    let Some(touchpoints) = artifact.body.get("touchpoints").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut followups = Vec::new();
    for touchpoint in touchpoints {
        let Some(original_anchor) = touchpoint.get("anchor").and_then(Value::as_str) else {
            continue;
        };
        match structural::resolve_anchor_in_origins(
            conn,
            original_anchor,
            Some(&artifact.source_snapshot),
            file_origins,
        ) {
            Ok((anchor, status)) => {
                let origin: String = conn
                    .query_row(
                        "SELECT file.origin FROM graph_nodes node
                         JOIN files file ON file.id=node.file_id
                         WHERE node.node_key=?1",
                        [&anchor],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|_| "repository".into());
                followups.push(json!({
                    "original_anchor": original_anchor,
                    "status": status,
                    "tools": ["definition", "who_uses", "neighborhood"],
                    "arguments": {
                        "anchor": anchor,
                        "snapshot": current_snapshot,
                        "origins": [origin],
                    }
                }));
            }
            Err(error) => followups.push(json!({
                "original_anchor": original_anchor,
                "status": "unresolved",
                "reason": error.to_string(),
            })),
        }
    }
    Ok(followups)
}

fn settle_response(mut value: Value, byte_limit: usize) -> Result<Option<String>> {
    let object = value
        .as_object_mut()
        .context("bounded design response must be an object")?;
    object.insert(
        "response".into(),
        json!({ "byte_limit": byte_limit, "rendered_bytes": 0 }),
    );
    for _ in 0..8 {
        let rendered = serde_json::to_string(&value)?;
        if rendered.len() > byte_limit {
            return Ok(None);
        }
        let recorded = value["response"]["rendered_bytes"].as_u64().unwrap_or(0) as usize;
        if recorded == rendered.len() {
            return Ok(Some(rendered));
        }
        value["response"]["rendered_bytes"] = rendered.len().into();
    }
    let rendered = serde_json::to_string(&value)?;
    Ok((rendered.len() <= byte_limit).then_some(rendered))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use anyhow::Result;
    use serde_json::{Value, json};

    use super::{DesignScoutOptions, DesignSelector};
    use crate::llm::config::{ModelSpec, RequestPolicy};
    use crate::llm::protocol::{
        CompleteRequest, ModelCapabilities, ProviderSummary, ToolCall, Usage,
    };
    use crate::llm::{CompletionOutcome, GatewayError, LlmGateway, StartedInfo};
    use crate::{indexer, search, store};

    struct FakeGateway {
        results: VecDeque<std::result::Result<CompletionOutcome, GatewayError>>,
        calls: usize,
    }

    impl FakeGateway {
        fn new(results: Vec<std::result::Result<CompletionOutcome, GatewayError>>) -> Self {
            Self {
                results: results.into(),
                calls: 0,
            }
        }
    }

    impl LlmGateway for FakeGateway {
        fn capabilities(
            &mut self,
            model: Option<&str>,
        ) -> std::result::Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError>
        {
            Ok((
                ProviderSummary {
                    builtin: 1,
                    custom: Vec::new(),
                },
                model.map(|_| ModelCapabilities {
                    provider: "faux".into(),
                    model: "model".into(),
                    api: "faux".into(),
                    base_url: Some("https://gateway.invalid/v1".into()),
                    context_window: Some(200_000),
                    max_tokens: Some(32_000),
                    reasoning: true,
                    supports_service_tier: false,
                    supports_tools: true,
                    billing_path: Some("api".into()),
                    auth_configured: true,
                    auth_type: Some("api_key".into()),
                    auth_source: Some("test".into()),
                }),
            ))
        }

        fn complete(
            &mut self,
            _request: &CompleteRequest,
            _timeout: Duration,
        ) -> std::result::Result<CompletionOutcome, GatewayError> {
            self.calls += 1;
            self.results
                .pop_front()
                .expect("unexpected design completion")
        }
    }

    fn options(task: &str) -> DesignScoutOptions {
        DesignScoutOptions {
            task: task.into(),
            seeds: Vec::new(),
            search_limit: super::DEFAULT_SEARCH_LIMIT,
            seed_limit: super::DEFAULT_SEED_LIMIT,
            file_limit: super::DEFAULT_FILE_LIMIT,
            graph_depth: super::DEFAULT_GRAPH_DEPTH,
            graph_node_limit: super::DEFAULT_GRAPH_NODE_LIMIT,
            graph_edge_limit: super::DEFAULT_GRAPH_EDGE_LIMIT,
            candidate_limit: super::DEFAULT_CANDIDATE_LIMIT,
            file_roles: Vec::new(),
            file_origins: crate::origin::defaults(),
            model: ModelSpec::parse("faux:model").expect("model"),
            reasoning: Some("high".into()),
            service_tier: None,
            policy: RequestPolicy::new(30, 1, 240_000).expect("policy"),
            rebuild: false,
            supersedes_artifact_id: None,
        }
    }

    fn outcome(anchor: &str, line: i64) -> CompletionOutcome {
        let evidence = json!([{
            "anchor": anchor,
            "start_line": line,
            "end_line": line,
        }]);
        CompletionOutcome {
            started: StartedInfo {
                provider: "faux".into(),
                model: "model".into(),
                api: "faux".into(),
                base_url: Some("https://gateway.invalid/v1".into()),
                billing_path: "api".into(),
                auth_source: "test".into(),
            },
            tool_call: ToolCall {
                name: super::SUBMIT_TOOL_NAME.into(),
                arguments: json!({
                    "resolution": "resolved",
                    "candidate_mechanisms": [{
                        "text": "The stale cache entry survives after the prediction is rejected.",
                        "evidence": evidence,
                    }],
                    "selected_mechanism": {
                        "text": "The rejection path leaves the cached prediction reachable.",
                        "evidence": evidence,
                    },
                    "detection_signals": [{
                        "signal": "A rejected prediction remains cached.",
                        "channel": "The rejection branch observes the failed prediction.",
                        "evidence": evidence,
                    }],
                    "cure_semantics": {
                        "text": "Expire the cached prediction immediately when rejection is observed.",
                        "evidence": evidence,
                    },
                    "touchpoints": [{
                        "anchor": anchor,
                        "responsibility": "Observe rejection and expire the cached prediction.",
                        "evidence": evidence,
                    }],
                    "propagation": [],
                    "invariants": [{
                        "text": "Rejected predictions are never returned by a later request.",
                        "evidence": evidence,
                    }],
                    "validation_oracle": [{
                        "text": "Reject a prediction and assert the next request recomputes it.",
                        "evidence": evidence,
                    }],
                    "risks": [],
                    "unresolved_questions": [],
                }),
            },
            stop_reason: "toolUse".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                reasoning_tokens: None,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 150,
                cost_total: 0.0,
            },
            attempts: 1,
            response_model: Some("model".into()),
        }
    }

    #[test]
    fn context_budget_prunes_optional_design_evidence_before_the_model_call() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let mut source = String::new();
        for function in 0..12 {
            source.push_str(&format!("export function part{function}() {{\n"));
            for line in 0..80 {
                source.push_str(&format!(
                    "  const value{line} = 'candidate-{function}-line-{line}-padding-padding-padding';\n"
                ));
            }
            source.push_str("  return 1;\n}\n");
        }
        source.push_str("export function root() {\n  return ");
        source.push_str(
            &(0..12)
                .map(|function| format!("part{function}()"))
                .collect::<Vec<_>>()
                .join(" + "),
        );
        source.push_str(";\n}\n");
        std::fs::write(repo.path().join("flow.ts"), source)?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let mut options = options("Understand the complete flow before changing its behavior");
        options.seeds = vec!["sym:flow.ts#::root@1".into()];
        options.policy = RequestPolicy::new(30, 1, 48_000)?;
        let plan = super::plan(repo.path(), &conn, None, &options)?;
        let report = super::dry_run_report(&plan, &options)?;

        assert_eq!(report["would_call"], true);
        assert!(report["request_bytes"].as_u64().unwrap() <= 48_000);
        assert!(plan.report.candidates_truncated);
        assert!(
            plan.report
                .candidates
                .iter()
                .any(|candidate| candidate.anchor == "sym:flow.ts#::root@1")
        );
        Ok(())
    }

    #[test]
    fn anchor_free_task_publishes_reuses_and_requires_explicit_brief_activation() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("cache.ts"),
            "// A refused speculative result must be removed so a later request recomputes instead of serving stale state.\nexport function rejectPrediction(cache: Map<string, string>, key: string) {\n  cache.delete(key);\n}\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let task = "When a speculative result is refused, the later request must recompute instead of serving stale state";
        let options = options(task);
        let plan = super::plan(repo.path(), &conn, None, &options)?;
        assert!(!plan.report.candidates.is_empty());
        assert!(
            plan.report
                .seeds
                .iter()
                .all(|seed| seed.starts_with("sym:"))
        );
        let anchor = plan.report.candidates[0].anchor.clone();
        let line = plan.report.candidates[0].evidence_start_line.max(1);
        let mut gateway = FakeGateway::new(vec![Ok(outcome(&anchor, line))]);
        let report = super::scout(repo.path(), &conn, &mut gateway, &options, plan)?;
        assert_eq!(report.status, "completed");
        let artifact_id = report.artifact_id.expect("published design");
        let minimal: Value = serde_json::from_str(&super::render_design(&conn, artifact_id, 512)?)?;
        assert_eq!(minimal["design_id"], artifact_id);
        assert_eq!(minimal["design_omitted"], true);

        let search = search::search(
            &conn,
            None,
            "stale prediction cache",
            &search::SearchOptions {
                include_memory: true,
                ..search::SearchOptions::default()
            },
        )?;
        assert!(
            search
                .semantic_artifacts
                .iter()
                .all(|artifact| artifact.id != artifact_id)
        );
        let broad_memory = crate::semantic_query::query(
            repo.path(),
            &conn,
            None,
            &crate::semantic_query::QueryOptions::default(),
        )?;
        assert!(
            broad_memory
                .semantic_artifacts
                .iter()
                .all(|artifact| artifact.id != artifact_id)
        );
        let exact_memory = crate::semantic_query::query(
            repo.path(),
            &conn,
            None,
            &crate::semantic_query::QueryOptions {
                artifact_id: Some(artifact_id),
                query: "unrelated ranking text".into(),
                ..crate::semantic_query::QueryOptions::default()
            },
        )?;
        assert_eq!(exact_memory.semantic_artifacts[0].id, artifact_id);

        let brief = super::implementation_brief(&conn, DesignSelector::Task(task.into()), 16_000)?;
        let brief: Value = serde_json::from_str(&brief)?;
        assert_eq!(brief["design_id"], artifact_id);
        assert_eq!(
            brief["design"]["cure_semantics"],
            "Expire the cached prediction immediately when rejection is observed."
        );
        assert_eq!(brief["followups"][0]["arguments"]["anchor"], anchor);

        let plan = super::plan(repo.path(), &conn, None, &options)?;
        let reused = super::scout(repo.path(), &conn, &mut gateway, &options, plan)?;
        assert_eq!(reused.status, "reused");
        assert_eq!(reused.artifact_id, Some(artifact_id));
        assert_eq!(gateway.calls, 1);

        std::fs::write(
            repo.path().join("cache.ts"),
            "// A refused speculative result must be expired so a later request recomputes instead of serving stale state.\nexport function rejectPrediction(cache: Map<string, string>, key: string) {\n  cache.delete(key);\n}\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        let stale = crate::semantic::load_artifact(&conn, artifact_id)?.expect("old design");
        assert_eq!(stale.freshness, "stale");
        gateway.results.push_back(Ok(outcome(&anchor, line)));
        let selection = crate::scouting::refresh::select(&conn, &[artifact_id])?;
        let refreshed = crate::scouting::scout_refresh(
            repo.path(),
            &conn,
            &mut gateway,
            selection,
            RequestPolicy::new(30, 1, 240_000)?,
        )?;
        assert_eq!(refreshed.reports.len(), 1);
        assert_eq!(refreshed.reports[0].status, "completed");
        assert_ne!(refreshed.reports[0].artifact_id, Some(artifact_id));
        assert_eq!(gateway.calls, 2);
        Ok(())
    }

    #[test]
    fn incomplete_design_publishes_no_artifact() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("cache.ts"),
            "// Cached results are invalidated after rejection.\nexport const cache = new Map();\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let options = options("Find where cached results are invalidated after rejection");
        let plan = super::plan(repo.path(), &conn, None, &options)?;
        let mut result = outcome(
            &plan.report.candidates[0].anchor,
            plan.report.candidates[0].evidence_start_line.max(1),
        );
        result.tool_call.arguments = json!({
            "resolution": "incomplete",
            "incomplete_reason": "The bounded evidence does not contain the rejection path."
        });
        let mut gateway = FakeGateway::new(vec![Ok(result)]);
        let report = super::scout(repo.path(), &conn, &mut gateway, &options, plan)?;
        assert_eq!(report.status, "incomplete");
        assert!(report.artifact_id.is_none());
        let artifacts: i64 = conn.query_row(
            "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='design'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(artifacts, 0);
        Ok(())
    }

    #[test]
    fn unresolved_schema_failure_and_cancellation_never_publish_partial_designs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("cache.ts"),
            "// Cached results are invalidated after a rejected speculative response.\nexport function invalidate(cache: Map<string, string>, key: string) { cache.delete(key); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let unresolved_options = options(
            "Determine how rejected speculative responses should invalidate cached results",
        );
        let plan = super::plan(repo.path(), &conn, None, &unresolved_options)?;
        let anchor = plan.report.candidates[0].anchor.clone();
        let line = plan.report.candidates[0].evidence_start_line.max(1);
        let evidence = json!([{
            "anchor": anchor,
            "start_line": line,
            "end_line": line,
        }]);
        let mut unresolved = outcome(&anchor, line);
        unresolved.tool_call.arguments = json!({
            "resolution": "unresolved",
            "candidate_mechanisms": [{
                "text": "Either rejection bypasses invalidation or a later write restores the entry.",
                "evidence": evidence,
            }],
            "unresolved_reason": {
                "text": "The bounded evidence does not show which path executes after invalidation.",
                "evidence": evidence,
            }
        });
        let mut gateway = FakeGateway::new(vec![Ok(unresolved)]);
        let report = super::scout(repo.path(), &conn, &mut gateway, &unresolved_options, plan)?;
        let artifact_id = report.artifact_id.expect("unresolved artifact");
        let artifact = crate::semantic::load_artifact(&conn, artifact_id)?.expect("artifact");
        assert_eq!(artifact.confidence, "possible");
        assert_eq!(artifact.body["resolution"], "unresolved");

        let invalid_options = options("Explain invalidation after a rejected cached response");
        let invalid_plan = super::plan(repo.path(), &conn, None, &invalid_options)?;
        let mut invalid = outcome(&anchor, line);
        invalid.tool_call.arguments["touchpoints"][0]["anchor"] = json!("sym:missing.ts#::x@1");
        gateway.results.push_back(Ok(invalid));
        let failed = super::scout(
            repo.path(),
            &conn,
            &mut gateway,
            &invalid_options,
            invalid_plan,
        )?;
        assert_eq!(failed.status, "failed");
        assert!(failed.artifact_id.is_none());

        let canceled_options = options("Verify invalidation after a rejected cached response");
        let canceled_plan = super::plan(repo.path(), &conn, None, &canceled_options)?;
        gateway
            .results
            .push_back(Err(GatewayError::Canceled("test cancellation".into())));
        super::scout(
            repo.path(),
            &conn,
            &mut gateway,
            &canceled_options,
            canceled_plan,
        )
        .expect_err("canceled design call must abort");

        let artifacts: i64 = conn.query_row(
            "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='design'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(artifacts, 1, "failed/canceled runs must publish nothing");
        let statuses = conn
            .prepare("SELECT status FROM scout_runs WHERE scout_kind='design' ORDER BY id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(statuses, vec!["completed", "failed", "canceled"]);
        Ok(())
    }

    #[test]
    fn behavioral_tests_cannot_be_the_only_evidence_for_an_implementation_design() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("cache.test.ts"),
            "// Rejected cached responses must be invalidated.\nexport function rejectionOracle() { return 'recompute'; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let options = options("Rejected cached responses must be invalidated and recomputed");
        let plan = super::plan(repo.path(), &conn, None, &options)?;
        let anchor = plan.report.candidates[0].anchor.clone();
        let line = plan.report.candidates[0].evidence_start_line.max(1);
        let mut gateway = FakeGateway::new(vec![Ok(outcome(&anchor, line))]);
        let report = super::scout(repo.path(), &conn, &mut gateway, &options, plan)?;
        assert_eq!(report.status, "failed");
        assert!(
            report
                .failure
                .as_deref()
                .unwrap_or_default()
                .contains("runtime/production evidence")
        );
        assert!(report.artifact_id.is_none());
        Ok(())
    }
}
