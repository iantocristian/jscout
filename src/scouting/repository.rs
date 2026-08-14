//! G13 repository reconnaissance: deterministic scope/project subjects,
//! bounded evidence packs, a closed model contract, and immutable policy
//! classifications. Current heuristic file roles are intentionally absent
//! from every pack.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ledger::{RunClaim, RunOutcome, RunSpec};
use super::{
    BatchSkip, ORPHAN_SWEEP_MINUTES, PreparationCache, ScoutBatchReport, ScoutReport,
    enforce_context_budget, reserve_output_and_measure,
};
use crate::checker::protocol::{ConfigurationProblem, ProjectSummary};
use crate::llm::config::{ModelSpec, RequestPolicy};
use crate::llm::protocol::{
    ChatMessage, CompleteRequest, PROTOCOL_VERSION, ProviderOptions, SubmitTool,
};
use crate::llm::{GatewayError, LlmGateway};
use crate::recon::{self, MemberFile, SubjectSelector, SubjectState};
use crate::{scouting::ledger, structural};

pub const PROMPT_VERSION: &str = "repository-recon/v1";
pub const SUBMIT_TOOL_NAME: &str = "submit_repository_classification";
pub const DEFAULT_MAX_SUBJECTS: usize = 256;
pub const DEFAULT_MAX_DEPTH: usize = 3;

const MAX_EXPLANATION_CHARS: usize = 600;
const MAX_CITATIONS: usize = 8;
const MAX_OUTLINES_PER_FILE: usize = 3;
const MAX_DISK_EVIDENCE_CHARS: usize = 12_000;

#[derive(Debug, Clone)]
pub struct RepositoryScoutOptions {
    pub model: ModelSpec,
    pub reasoning: Option<String>,
    pub service_tier: Option<String>,
    pub policy: RequestPolicy,
    pub rebuild: bool,
    pub max_subjects: usize,
    pub max_depth: usize,
}

#[derive(Debug, Clone)]
pub struct RepositoryPlanningOptions<'a> {
    pub max_subjects: usize,
    pub max_depth: usize,
    pub checker_timeout: Duration,
    pub checker_sidecar: Option<&'a Path>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryRunConfig {
    pub subject_key: String,
    pub service_tier: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceItem {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryEvidencePack {
    pub algorithm: &'static str,
    pub subject_key: String,
    pub subject_kind: String,
    pub member_count: usize,
    pub language_counts: BTreeMap<String, usize>,
    pub chunk_kind_counts: BTreeMap<String, usize>,
    pub items: Vec<EvidenceItem>,
    #[serde(skip)]
    pub rendered: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentClassification {
    pub id: i64,
    pub role: String,
    pub confidence: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryPlanItem {
    pub subject_key: String,
    pub subject_kind: String,
    pub display_name: String,
    pub parent_subject_key: Option<String>,
    pub depth: usize,
    pub selector: SubjectSelector,
    pub evidence_fingerprint: String,
    pub member_count: usize,
    pub evidence: RepositoryEvidencePack,
    pub current_classification: Option<CurrentClassification>,
    pub downstream_policy: String,
    pub potential_children: Vec<String>,
    #[serde(skip)]
    pub state: SubjectState,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryPlan {
    pub snapshot: String,
    pub max_subjects: usize,
    pub max_depth: usize,
    pub subject_limit_reached: bool,
    pub configured_projects: usize,
    pub configuration_problems: Vec<ConfigurationProblem>,
    pub items: Vec<RepositoryPlanItem>,
    pub omitted_subjects: Vec<BatchSkip>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    pub role: String,
    pub confidence: String,
    pub explanation: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedClassification {
    pub role: String,
    pub confidence: String,
    pub explanation: String,
    pub citations: Vec<String>,
}

#[derive(Debug)]
struct PreparedRepository {
    item: RepositoryPlanItem,
    request: CompleteRequest,
    spec: RunSpec,
}

pub fn submit_tool_schema(evidence_ids: &[String]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["role", "confidence", "explanation", "evidence"],
        "properties": {
            "role": {
                "type": "string",
                "enum": recon::ROLES,
                "description": "One role for the exact subject; mixed means subdivide, unknown means evidence is insufficient"
            },
            "confidence": {
                "type": "string",
                "enum": ["likely", "possible"]
            },
            "explanation": {
                "type": "string",
                "minLength": 3,
                "maxLength": MAX_EXPLANATION_CHARS
            },
            "evidence": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CITATIONS,
                "uniqueItems": true,
                "items": { "type": "string", "enum": evidence_ids }
            }
        }
    })
}

pub fn validate(
    submission: &Submission,
    evidence: &RepositoryEvidencePack,
) -> Result<ValidatedClassification> {
    if !recon::ROLES.contains(&submission.role.as_str()) {
        bail!("unknown repository role `{}`", submission.role);
    }
    if !matches!(submission.confidence.as_str(), "likely" | "possible") {
        bail!("repository confidence must be likely or possible");
    }
    if submission.role == "unknown" && submission.confidence != "possible" {
        bail!("unknown classifications must be possible");
    }
    let explanation = submission.explanation.trim();
    if explanation.chars().count() < 3 {
        bail!("repository classification explanation is empty");
    }
    if explanation.chars().count() > MAX_EXPLANATION_CHARS {
        bail!("repository explanation exceeds {MAX_EXPLANATION_CHARS} characters");
    }
    if submission.evidence.is_empty() || submission.evidence.len() > MAX_CITATIONS {
        bail!("repository classification requires 1-{MAX_CITATIONS} evidence citations");
    }
    let known = evidence
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut unique = BTreeSet::new();
    for citation in &submission.evidence {
        if !known.contains(citation.as_str()) {
            bail!("classification cites unknown evidence `{citation}`");
        }
        if !unique.insert(citation.clone()) {
            bail!("classification repeats evidence `{citation}`");
        }
    }
    Ok(ValidatedClassification {
        role: submission.role.clone(),
        confidence: submission.confidence.clone(),
        explanation: explanation.to_string(),
        citations: unique.into_iter().collect(),
    })
}

pub fn plan(
    root: &Path,
    conn: &Connection,
    options: &RepositoryPlanningOptions<'_>,
) -> Result<RepositoryPlan> {
    if options.max_subjects == 0 {
        bail!("--max-subjects must be greater than zero");
    }
    let snapshot = structural::current_snapshot(conn)?;
    let mut subjects = discover_scope_subjects(root, conn)?;
    let (projects, configuration_problems, configured_projects) =
        discover_project_subjects(root, conn, options)?;
    subjects.extend(projects);
    subjects.sort_by(|left, right| {
        subject_priority(&left.subject_kind)
            .cmp(&subject_priority(&right.subject_kind))
            .then(left.subject_key.cmp(&right.subject_key))
    });

    let subject_limit_reached = subjects.len() > options.max_subjects;
    let mut omitted_subjects = Vec::new();
    if subject_limit_reached {
        for subject in subjects.drain(options.max_subjects..) {
            omitted_subjects.push(BatchSkip {
                subject: subject.subject_key,
                reason: format!("--max-subjects {} reached", options.max_subjects),
            });
        }
    }
    let mut items = Vec::new();
    for discovered in subjects {
        items.push(complete_plan_item(root, conn, discovered)?);
    }
    Ok(RepositoryPlan {
        snapshot,
        max_subjects: options.max_subjects,
        max_depth: options.max_depth,
        subject_limit_reached,
        configured_projects,
        configuration_problems,
        items,
        omitted_subjects,
    })
}

#[derive(Debug)]
struct DiscoveredSubject {
    subject_key: String,
    subject_kind: String,
    display_name: String,
    parent_subject_key: Option<String>,
    depth: usize,
    state: SubjectState,
}

fn discover_scope_subjects(root: &Path, conn: &Connection) -> Result<Vec<DiscoveredSubject>> {
    let mut subjects = Vec::new();
    let packages = {
        let mut statement = conn.prepare(
            "SELECT name, locator FROM package_instances
             WHERE origin='workspace' ORDER BY locator, name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (name, package_root) in packages {
        let subject_key = format!("package:{package_root}");
        let selector = SubjectSelector::WorkspaceArea {
            package_root: package_root.clone(),
            scope: package_root.clone(),
            direct_only: false,
        };
        let state = recon::build_scope_state(root, conn, subject_key.clone(), selector)?;
        if state.members.is_empty() {
            continue;
        }
        subjects.push(DiscoveredSubject {
            subject_key,
            subject_kind: "package".into(),
            display_name: format!("{name} ({package_root})"),
            parent_subject_key: None,
            depth: 0,
            state,
        });
    }

    let mut unowned = BTreeSet::new();
    let mut statement = conn.prepare(
        "SELECT path FROM files
         WHERE origin='repository' AND package_instance_id IS NULL
         ORDER BY path",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for path in rows {
        let path = path?;
        let scope = path.split_once('/').map_or(".", |(head, _)| head);
        unowned.insert(scope.to_string());
    }
    for scope in unowned {
        let subject_key = format!("area:repository:{scope}");
        let selector = SubjectSelector::RepositoryArea {
            scope: scope.clone(),
            direct_only: scope == ".",
        };
        let state = recon::build_scope_state(root, conn, subject_key.clone(), selector)?;
        if state.members.is_empty() {
            continue;
        }
        subjects.push(DiscoveredSubject {
            subject_key,
            subject_kind: "area".into(),
            display_name: if scope == "." {
                "repository root files".into()
            } else {
                scope
            },
            parent_subject_key: None,
            depth: 0,
            state,
        });
    }
    Ok(subjects)
}

fn discover_project_subjects(
    root: &Path,
    conn: &Connection,
    options: &RepositoryPlanningOptions<'_>,
) -> Result<(Vec<DiscoveredSubject>, Vec<ConfigurationProblem>, usize)> {
    let files = all_first_party_files(conn)?;
    let by_path = files
        .iter()
        .cloned()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let mut checker = crate::checker::launch(root, options.checker_sidecar)?;
    let capabilities = checker.capabilities(options.checker_timeout)?;
    let project_ids = capabilities
        .projects
        .iter()
        .map(|project| project.project_id.clone())
        .collect::<BTreeSet<_>>();
    let mut ownership = BTreeMap::<String, BTreeSet<String>>::new();
    for batch in files.chunks(512) {
        let result = checker.plan_members(
            batch.iter().map(|file| file.path.clone()).collect(),
            options.checker_timeout,
        )?;
        for file in result.files {
            for project in file
                .project_ids
                .into_iter()
                .chain(file.excluded_project_ids)
            {
                if project_ids.contains(&project) {
                    ownership
                        .entry(project)
                        .or_default()
                        .insert(file.file.clone());
                }
            }
        }
    }

    let configured_projects = capabilities.projects.len();
    let configuration_problems = capabilities.configuration_problems;
    let mut subjects = Vec::new();
    for ProjectSummary {
        project_id,
        file_count,
        membership_fingerprint,
        config_fingerprint,
        ..
    } in capabilities.projects
    {
        let members = ownership
            .remove(&project_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|path| by_path.get(&path).cloned())
            .collect::<Vec<_>>();
        let subject_key = format!("project:{project_id}");
        let state = recon::build_project_state(
            root,
            subject_key.clone(),
            project_id.clone(),
            membership_fingerprint,
            config_fingerprint,
            members,
        )?;
        subjects.push(DiscoveredSubject {
            subject_key,
            subject_kind: "project".into(),
            display_name: format!("{project_id} ({file_count} checker files)"),
            parent_subject_key: None,
            depth: 0,
            state,
        });
    }
    Ok((subjects, configuration_problems, configured_projects))
}

fn all_first_party_files(conn: &Connection) -> Result<Vec<MemberFile>> {
    let mut statement = conn.prepare(
        "SELECT id, path, hash FROM files
         WHERE origin IN ('repository','workspace') ORDER BY path",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(MemberFile {
            id: row.get(0)?,
            path: row.get(1)?,
            hash: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn complete_plan_item(
    root: &Path,
    conn: &Connection,
    subject: DiscoveredSubject,
) -> Result<RepositoryPlanItem> {
    debug_assert_eq!(subject.subject_key, subject.state.subject_key);
    let evidence = build_evidence(root, conn, &subject)?;
    let current_classification = current_classification(
        conn,
        &subject.subject_key,
        &subject.state.evidence_fingerprint,
    )?;
    let downstream_policy = match &current_classification {
        Some(current) if current.confidence == "likely" && recon::is_auxiliary(&current.role) => {
            format!("auxiliary penalty/exclusion from {}", current.role)
        }
        Some(current) if current.confidence == "likely" && current.role == "runtime" => {
            "runtime inclusion".into()
        }
        _ => "neutral inclusion".into(),
    };
    let potential_children = subdivisions(&subject.state)
        .into_iter()
        .filter_map(|child| child_identity(&subject.state.selector, &child).map(|(key, _)| key))
        .collect();
    Ok(RepositoryPlanItem {
        subject_key: subject.subject_key,
        subject_kind: subject.subject_kind,
        display_name: subject.display_name,
        parent_subject_key: subject.parent_subject_key,
        depth: subject.depth,
        selector: subject.state.selector.clone(),
        evidence_fingerprint: subject.state.evidence_fingerprint.clone(),
        member_count: subject.state.members.len(),
        evidence,
        current_classification,
        downstream_policy,
        potential_children,
        state: subject.state,
    })
}

fn current_classification(
    conn: &Connection,
    subject_key: &str,
    evidence_fingerprint: &str,
) -> Result<Option<CurrentClassification>> {
    Ok(conn
        .query_row(
            "SELECT classification.id, classification.role, classification.confidence,
                    run.provider || ':' || run.model
             FROM repository_classifications classification
             JOIN scout_runs run ON run.id=classification.run_id
             WHERE classification.subject_key=?1
               AND classification.evidence_fingerprint=?2
               AND run.status='completed'
             ORDER BY classification.id DESC LIMIT 1",
            params![subject_key, evidence_fingerprint],
            |row| {
                Ok(CurrentClassification {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    confidence: row.get(2)?,
                    model: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn build_evidence(
    root: &Path,
    conn: &Connection,
    subject: &DiscoveredSubject,
) -> Result<RepositoryEvidencePack> {
    let mut language_counts = BTreeMap::new();
    for member in &subject.state.members {
        let extension = Path::new(&member.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("none")
            .to_ascii_lowercase();
        *language_counts.entry(extension).or_insert(0) += 1;
    }
    let representatives =
        recon::representative_members(&subject.state.members, recon::REPRESENTATIVE_FILE_LIMIT);
    let member_ids = representatives
        .iter()
        .map(|member| member.id)
        .collect::<Vec<_>>();
    let chunk_kind_counts = chunk_kind_counts(conn, &member_ids)?;
    let mut items = Vec::new();
    push_item(
        &mut items,
        "aggregate",
        None,
        None,
        None,
        format!(
            "{} indexed files; languages/extensions: {}; sampled AST chunk kinds: {}",
            subject.state.members.len(),
            render_counts(&language_counts),
            render_counts(&chunk_kind_counts),
        ),
    );

    for input in &subject.state.disk_inputs {
        let path = root.join(&input.path);
        let bytes = fs::read(&path)
            .with_context(|| format!("read reconnaissance evidence {}", path.display()))?;
        let content = String::from_utf8_lossy(&bytes);
        let content = truncate_chars(&content, MAX_DISK_EVIDENCE_CHARS);
        let line_count = content.lines().count().max(1);
        push_item(
            &mut items,
            "configuration",
            Some(input.path.clone()),
            Some(1),
            Some(line_count),
            content,
        );
    }

    for member in representatives {
        add_file_evidence(conn, member, &mut items)?;
    }
    let mut pack = RepositoryEvidencePack {
        algorithm: recon::EVIDENCE_ALGORITHM,
        subject_key: subject.subject_key.clone(),
        subject_kind: subject.subject_kind.clone(),
        member_count: subject.state.members.len(),
        language_counts,
        chunk_kind_counts,
        items,
        rendered: String::new(),
    };
    pack.rendered = serde_json::to_string_pretty(&pack)?;
    Ok(pack)
}

fn chunk_kind_counts(conn: &Connection, member_ids: &[i64]) -> Result<BTreeMap<String, usize>> {
    if member_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ids = serde_json::to_string(member_ids)?;
    let mut statement = conn.prepare(
        "SELECT chunk.kind, count(*) FROM chunks chunk
         WHERE chunk.file_id IN (SELECT value FROM json_each(?1))
         GROUP BY chunk.kind ORDER BY chunk.kind",
    )?;
    let rows = statement.query_map([ids], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn add_file_evidence(
    conn: &Connection,
    member: &MemberFile,
    items: &mut Vec<EvidenceItem>,
) -> Result<()> {
    let mut outlines = conn.prepare_cached(
        "SELECT name, kind, line, exported, scope_chain
         FROM symbols WHERE file_id=?1 AND (exported=1 OR scope_chain='')
         ORDER BY exported DESC, line, name LIMIT ?2",
    )?;
    let rows = outlines.query_map(params![member.id, MAX_OUTLINES_PER_FILE as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? as usize,
            row.get::<_, bool>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (name, kind, line, exported, scope) = row?;
        push_item(
            items,
            "outline",
            Some(member.path.clone()),
            Some(line),
            Some(line),
            format!(
                "{} {kind} `{name}`{}",
                if exported { "exported" } else { "root" },
                if scope.is_empty() {
                    String::new()
                } else {
                    format!(" in {scope}")
                }
            ),
        );
    }

    let imports = string_rows(
        conn,
        "SELECT DISTINCT request FROM imports WHERE file_id=?1 ORDER BY request LIMIT 8",
        member.id,
    )?;
    let exports = string_rows(
        conn,
        "SELECT DISTINCT export_name FROM exports WHERE file_id=?1 ORDER BY export_name LIMIT 8",
        member.id,
    )?;
    if !imports.is_empty() || !exports.is_empty() {
        push_item(
            items,
            "module_boundary",
            Some(member.path.clone()),
            None,
            None,
            format!(
                "imports [{}]; exports [{}]",
                imports.join(", "),
                exports.join(", ")
            ),
        );
    }

    let mut entities = conn.prepare_cached(
        "SELECT occurrence.line, entity.plane, entity.entity_type, entity.name, occurrence.role
         FROM entity_occurrences occurrence
         JOIN entities entity ON entity.id=occurrence.entity_id
         WHERE occurrence.file_id=?1
         ORDER BY occurrence.line, entity.entity_type, entity.name LIMIT 6",
    )?;
    let rows = entities.query_map([member.id], |row| {
        Ok(format!(
            "line {}: {}/{} `{}` ({})",
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let entities = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if !entities.is_empty() {
        push_item(
            items,
            "entities",
            Some(member.path.clone()),
            None,
            None,
            entities.join("; "),
        );
    }
    Ok(())
}

fn string_rows(conn: &Connection, sql: &str, file_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([file_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn push_item(
    items: &mut Vec<EvidenceItem>,
    kind: &str,
    source: Option<String>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    content: String,
) {
    items.push(EvidenceItem {
        id: format!("E{:03}", items.len() + 1),
        kind: kind.into(),
        source,
        start_line,
        end_line,
        content,
    });
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_string()
    } else {
        value.chars().take(limit).collect::<String>() + "\n[truncated]"
    }
}

fn subject_priority(kind: &str) -> usize {
    match kind {
        "package" => 0,
        "area" => 1,
        "project" => 2,
        _ => 3,
    }
}

fn build_request(item: &RepositoryPlanItem, options: &RepositoryScoutOptions) -> CompleteRequest {
    let evidence_ids = item
        .evidence
        .items
        .iter()
        .map(|evidence| evidence.id.clone())
        .collect::<Vec<_>>();
    let system = "You classify one exact repository scope or TypeScript project from a deterministic evidence pack. Current jscout path-role labels are withheld. Choose runtime, tooling, documentation, test, generated, mixed, or unknown. Use mixed only when materially different child areas coexist and subdivision is necessary; use unknown/possible when the pack is insufficient. Never infer runtime behavior from a directory or config name alone. Cite only supplied evidence IDs and submit exactly once through the tool.".to_string();
    let user = format!(
        "Subject: {}\nKind: {}\nDisplay: {}\nMembers: {}\n\n{}",
        item.subject_key,
        item.subject_kind,
        item.display_name,
        item.member_count,
        item.evidence.rendered,
    );
    CompleteRequest {
        model: options.model.spec.clone(),
        reasoning: options.reasoning.clone(),
        system: Some(system),
        messages: vec![ChatMessage {
            role: "user",
            content: user,
        }],
        tool: SubmitTool {
            name: SUBMIT_TOOL_NAME.into(),
            description:
                "Submit the evidence-backed classification for this exact repository subject".into(),
            parameters: submit_tool_schema(&evidence_ids),
        },
        timeout_ms: Some(options.policy.timeout.as_millis() as u64),
        max_tokens: None,
        session_id: None,
        provider_options: options.service_tier.as_ref().map(|tier| ProviderOptions {
            service_tier: Some(tier.clone()),
        }),
    }
}

fn prepare(
    gateway: &mut dyn LlmGateway,
    cache: &mut PreparationCache,
    item: RepositoryPlanItem,
    options: &RepositoryScoutOptions,
    source_snapshot: &str,
) -> Result<PreparedRepository> {
    let mut request = build_request(&item, options);
    let capabilities = cache.model(gateway, &options.model)?;
    enforce_context_budget(
        &capabilities,
        &mut request,
        item.evidence.items.len(),
        1,
        &options.policy,
        &options.model.spec,
    )?;
    let input_fingerprint =
        input_fingerprint(&item, &request, options, capabilities.base_url.as_deref())?;
    let request_hash = blake3::hash(serde_json::to_string(&request)?.as_bytes())
        .to_hex()
        .to_string();
    let spec = RunSpec {
        scout_kind: "repository".into(),
        gateway_protocol: PROTOCOL_VERSION,
        provider: options.model.provider.clone(),
        model: options.model.model_id.clone(),
        billing_path: cache.billing_path(gateway, &options.model)?,
        reasoning: options.reasoning.clone(),
        prompt_version: PROMPT_VERSION.into(),
        source_snapshot: source_snapshot.to_string(),
        input_fingerprint,
        request_hash,
        config_json: serde_json::to_string(&RepositoryRunConfig {
            subject_key: item.subject_key.clone(),
            service_tier: options.service_tier.clone(),
            base_url: capabilities.base_url.clone(),
        })?,
        supersedes_artifact_id: None,
    };
    Ok(PreparedRepository {
        item,
        request,
        spec,
    })
}

fn input_fingerprint(
    item: &RepositoryPlanItem,
    request: &CompleteRequest,
    options: &RepositoryScoutOptions,
    base_url: Option<&str>,
) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-repository-recon-input-v1\0");
    for part in [
        item.subject_key.as_str(),
        item.evidence_fingerprint.as_str(),
        item.evidence.rendered.as_str(),
        PROMPT_VERSION,
        options.model.spec.as_str(),
        options.reasoning.as_deref().unwrap_or(""),
        options.service_tier.as_deref().unwrap_or(""),
        base_url.unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(&PROTOCOL_VERSION.to_le_bytes());
    hasher.update(&request.max_tokens.unwrap_or_default().to_le_bytes());
    hasher.update(serde_json::to_string(&request.tool.parameters)?.as_bytes());
    if let Some(system) = &request.system {
        hasher.update(system.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn dry_run_report(
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    plan: &RepositoryPlan,
    options: &RepositoryScoutOptions,
) -> Result<Value> {
    let mut rendered = serde_json::to_value(plan)?;
    let mut calls_planned = 0;
    let mut over_budget = 0;
    let mut reusable_items = 0;
    let mut cache = PreparationCache::default();
    if let Some(items) = rendered.get_mut("items").and_then(Value::as_array_mut) {
        for (output, item) in items.iter_mut().zip(&plan.items) {
            let mut request = build_request(item, options);
            let (_, request_bytes) = reserve_output_and_measure(&mut request, 1, None)?;
            let mut over = request_bytes > options.policy.context_bytes;
            let mut reusable = false;
            if !over {
                match prepare(gateway, &mut cache, item.clone(), options, &plan.snapshot) {
                    Ok(prepared) => {
                        reusable = !options.rebuild
                            && ledger::reusable_run(conn, &prepared.spec)?.is_some();
                    }
                    Err(error)
                        if error
                            .downcast_ref::<super::ContextBudgetExceeded>()
                            .is_some() =>
                    {
                        over = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            let would_call = !over && !reusable && calls_planned < options.policy.max_calls;
            if over {
                over_budget += 1;
            } else if reusable {
                reusable_items += 1;
            } else if would_call {
                calls_planned += 1;
            }
            output["request_bytes"] = request_bytes.into();
            output["over_context_bytes"] = over.into();
            output["reusable"] = reusable.into();
            output["would_call"] = would_call.into();
            output["subdivision"] = match item.subject_kind.as_str() {
                "package" | "area" if item.depth < options.max_depth => {
                    "on a mixed result, immediate child directories are planned deterministically"
                }
                "package" | "area" => "depth bound reached; mixed remains neutral",
                _ => "projects do not subdivide",
            }
            .into();
        }
    }
    Ok(json!({
        "dry_run": true,
        "max_calls": options.policy.max_calls,
        "max_subjects": options.max_subjects,
        "max_depth": options.max_depth,
        "context_bytes": options.policy.context_bytes,
        "calls_planned": calls_planned,
        "reusable_items": reusable_items,
        "over_context_bytes_items": over_budget,
        "notes": [
            "current_classification reports subject-local freshness independent of the structural snapshot",
            "reusable and would_call use the resolved gateway/model-policy fingerprint; dry-run makes no provider generation calls",
            "mixed subdivision shares both --max-subjects and --max-calls and is visible only after a result exists",
        ],
        "plan": rendered,
    }))
}

pub fn execute(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &RepositoryScoutOptions,
    plan: RepositoryPlan,
) -> Result<ScoutBatchReport> {
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;
    let snapshot = plan.snapshot;
    let mut queue = VecDeque::from(plan.items);
    let mut seen = queue
        .iter()
        .map(|item| item.subject_key.clone())
        .collect::<BTreeSet<_>>();
    let mut subject_count = queue.len();
    let mut cache = PreparationCache::default();
    let mut report = ScoutBatchReport {
        auto_limit_reached: plan.subject_limit_reached,
        skipped_unresolvable: plan.omitted_subjects,
        ..ScoutBatchReport::default()
    };

    while let Some(item) = queue.pop_front() {
        let subject_key = item.subject_key.clone();
        let prepared = match prepare(gateway, &mut cache, item.clone(), options, &snapshot) {
            Ok(prepared) => prepared,
            Err(error)
                if error
                    .downcast_ref::<super::ContextBudgetExceeded>()
                    .is_some() =>
            {
                report.skipped_over_budget.push(BatchSkip {
                    subject: subject_key,
                    reason: error.to_string(),
                });
                if item.depth < options.max_depth {
                    for child in subdivide(root, conn, &item)? {
                        if !seen.insert(child.subject_key.clone()) {
                            continue;
                        }
                        if subject_count >= options.max_subjects {
                            report.auto_limit_reached = true;
                            report.skipped_unresolvable.push(BatchSkip {
                                subject: child.subject_key,
                                reason: format!("--max-subjects {} reached", options.max_subjects),
                            });
                            continue;
                        }
                        subject_count += 1;
                        queue.push_back(child);
                    }
                } else {
                    report.auto_limit_reached = true;
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        let reusable = !options.rebuild && ledger::reusable_run(conn, &prepared.spec)?.is_some();
        if !reusable && report.model_calls >= options.policy.max_calls {
            report.skipped_for_call_budget += 1;
            continue;
        }
        let subdivision_parent = prepared.item.clone();
        let (scout_report, role) = execute_one(root, conn, gateway, options, prepared, &snapshot)?;
        if scout_report.status != "reused" {
            report.model_calls += 1;
        }
        let mixed = role.as_deref() == Some("mixed");
        report.reports.push(scout_report);
        if mixed {
            if subdivision_parent.depth >= options.max_depth {
                report.auto_limit_reached = true;
                continue;
            }
            for child in subdivide(root, conn, &subdivision_parent)? {
                if !seen.insert(child.subject_key.clone()) {
                    continue;
                }
                if subject_count >= options.max_subjects {
                    report.auto_limit_reached = true;
                    report.skipped_unresolvable.push(BatchSkip {
                        subject: child.subject_key,
                        reason: format!("--max-subjects {} reached", options.max_subjects),
                    });
                    continue;
                }
                subject_count += 1;
                queue.push_back(child);
            }
        }
    }
    recon::reconcile_file_policy(root, conn)?;
    Ok(report)
}

fn execute_one(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &RepositoryScoutOptions,
    prepared: PreparedRepository,
    snapshot: &str,
) -> Result<(ScoutReport, Option<String>)> {
    let PreparedRepository {
        item,
        request,
        spec,
    } = prepared;
    let run_id = match ledger::claim_run(conn, &spec, options.rebuild)? {
        RunClaim::Reused(run_id) => {
            let (report, role) = reuse_report(conn, run_id, &item, &spec)?;
            return Ok((report, Some(role)));
        }
        RunClaim::Claimed { run_id, .. } => run_id,
    };
    let outcome = match gateway.complete(&request, options.policy.timeout) {
        Ok(outcome) => outcome,
        Err(error) => {
            let status = if matches!(error, GatewayError::Canceled(_)) {
                RunOutcome::Canceled
            } else {
                RunOutcome::Failed
            };
            ledger::finish_run(conn, run_id, status, None, Some(&error.code()))?;
            return Err(anyhow::Error::from(error)).context("repository scout completion failed");
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
        params![run_id, outcome.started.billing_path],
    )?;
    if outcome.tool_call.name != SUBMIT_TOOL_NAME {
        ledger::finish_run(
            conn,
            run_id,
            RunOutcome::Failed,
            Some(&usage_json),
            Some("tool_contract"),
        )?;
        return Ok((
            ScoutReport {
                kind: "repository".into(),
                subject: item.subject_key,
                run_id,
                status: "failed".into(),
                started: Some(outcome.started.clone()),
                artifact_id: None,
                candidate_count: 1,
                decisions: BTreeMap::new(),
                usage: Some(outcome.usage),
                billing_path: outcome.started.billing_path,
                incomplete_reason: None,
                failure: Some(format!(
                    "model called unexpected tool `{}`",
                    outcome.tool_call.name
                )),
            },
            None,
        ));
    }
    let validated = match serde_json::from_value::<Submission>(outcome.tool_call.arguments.clone())
        .context("submission does not match the repository output contract")
        .and_then(|submission| validate(&submission, &item.evidence))
    {
        Ok(validated) => validated,
        Err(error) => {
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Failed,
                Some(&usage_json),
                Some("validation"),
            )?;
            return Ok((
                ScoutReport {
                    kind: "repository".into(),
                    subject: item.subject_key,
                    run_id,
                    status: "failed".into(),
                    started: Some(outcome.started.clone()),
                    artifact_id: None,
                    candidate_count: 1,
                    decisions: BTreeMap::new(),
                    usage: Some(outcome.usage),
                    billing_path: outcome.started.billing_path,
                    incomplete_reason: None,
                    failure: Some(error.to_string()),
                },
                None,
            ));
        }
    };

    let incomplete_reason = match refresh_state(root, conn, &item) {
        Ok(current) if current.evidence_fingerprint == item.evidence_fingerprint => None,
        Ok(_) => Some("subject evidence changed during scouting; nothing was published".into()),
        Err(error) => Some(format!(
            "subject evidence could not be rechecked after scouting; nothing was published: {error}"
        )),
    };
    if let Some(incomplete_reason) = incomplete_reason {
        ledger::finish_run(
            conn,
            run_id,
            RunOutcome::Incomplete,
            Some(&usage_json),
            Some("inputs_changed"),
        )?;
        return Ok((
            ScoutReport {
                kind: "repository".into(),
                subject: item.subject_key,
                run_id,
                status: "incomplete".into(),
                started: Some(outcome.started.clone()),
                artifact_id: None,
                candidate_count: 1,
                decisions: BTreeMap::new(),
                usage: Some(outcome.usage),
                billing_path: outcome.started.billing_path,
                incomplete_reason: Some(incomplete_reason),
                failure: None,
            },
            None,
        ));
    }
    let citations_json = serde_json::to_string(&validated.citations)?;
    let citations = validated
        .citations
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let cited_evidence_json = serde_json::to_string(
        &item
            .evidence
            .items
            .iter()
            .filter(|evidence| citations.contains(evidence.id.as_str()))
            .collect::<Vec<_>>(),
    )?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let published = (|| -> Result<()> {
        recon::persist_classification(
            conn,
            &recon::NewClassification {
                run_id,
                subject_key: &item.subject_key,
                subject_kind: &item.subject_kind,
                selector: &item.selector,
                parent_subject_key: item.parent_subject_key.as_deref(),
                depth: item.depth,
                role: &validated.role,
                confidence: &validated.confidence,
                explanation: &validated.explanation,
                citations_json: &citations_json,
                cited_evidence_json: &cited_evidence_json,
                evidence_fingerprint: &item.evidence_fingerprint,
                classification_fingerprint: &spec.input_fingerprint,
                source_snapshot: snapshot,
            },
        )?;
        ledger::finish_run(conn, run_id, RunOutcome::Completed, Some(&usage_json), None)?;
        Ok(())
    })();
    match published {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Incomplete,
                Some(&usage_json),
                Some("publication_recheck"),
            )?;
            return Err(error);
        }
    }
    let mut decisions = BTreeMap::new();
    decisions.insert(validated.role.clone(), 1);
    Ok((
        ScoutReport {
            kind: "repository".into(),
            subject: item.subject_key,
            run_id,
            status: "completed".into(),
            started: Some(outcome.started.clone()),
            artifact_id: None,
            candidate_count: 1,
            decisions,
            usage: Some(outcome.usage),
            billing_path: outcome.started.billing_path,
            incomplete_reason: None,
            failure: None,
        },
        Some(validated.role),
    ))
}

fn reuse_report(
    conn: &Connection,
    run_id: i64,
    item: &RepositoryPlanItem,
    spec: &RunSpec,
) -> Result<(ScoutReport, String)> {
    let role: String = conn.query_row(
        "SELECT role FROM repository_classifications WHERE run_id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    let mut decisions = BTreeMap::new();
    decisions.insert(role.clone(), 1);
    Ok((
        ScoutReport {
            kind: "repository".into(),
            subject: item.subject_key.clone(),
            run_id,
            status: "reused".into(),
            started: None,
            artifact_id: None,
            candidate_count: 1,
            decisions,
            usage: None,
            billing_path: spec.billing_path.clone(),
            incomplete_reason: None,
            failure: None,
        },
        role,
    ))
}

fn refresh_state(
    root: &Path,
    conn: &Connection,
    item: &RepositoryPlanItem,
) -> Result<SubjectState> {
    match &item.selector {
        SubjectSelector::Project {
            config,
            membership_fingerprint,
            config_fingerprint,
        } => {
            let mut members = Vec::new();
            for original in &item.state.members {
                if let Some(member) = conn
                    .query_row(
                        "SELECT id, path, hash FROM files WHERE path=?1",
                        [&original.path],
                        |row| {
                            Ok(MemberFile {
                                id: row.get(0)?,
                                path: row.get(1)?,
                                hash: row.get(2)?,
                            })
                        },
                    )
                    .optional()?
                {
                    members.push(member);
                }
            }
            recon::build_project_state(
                root,
                item.subject_key.clone(),
                config.clone(),
                membership_fingerprint.clone(),
                config_fingerprint.clone(),
                members,
            )
        }
        selector => {
            recon::build_scope_state(root, conn, item.subject_key.clone(), selector.clone())
        }
    }
}

fn subdivide(
    root: &Path,
    conn: &Connection,
    parent: &RepositoryPlanItem,
) -> Result<Vec<RepositoryPlanItem>> {
    let mut children = Vec::new();
    for child in subdivisions(&parent.state) {
        let Some((subject_key, selector)) = child_identity(&parent.selector, &child) else {
            continue;
        };
        let state = recon::build_scope_state(root, conn, subject_key.clone(), selector)?;
        if state.members.is_empty() {
            continue;
        }
        children.push(complete_plan_item(
            root,
            conn,
            DiscoveredSubject {
                subject_key,
                subject_kind: "area".into(),
                display_name: child.display_name,
                parent_subject_key: Some(parent.subject_key.clone()),
                depth: parent.depth + 1,
                state,
            },
        )?);
    }
    Ok(children)
}

#[derive(Debug)]
struct Subdivision {
    scope: String,
    direct_only: bool,
    display_name: String,
}

fn subdivisions(state: &SubjectState) -> Vec<Subdivision> {
    let already_direct = match &state.selector {
        SubjectSelector::RepositoryArea { direct_only, .. }
        | SubjectSelector::WorkspaceArea { direct_only, .. } => *direct_only,
        SubjectSelector::Project { .. } => return Vec::new(),
    };
    if already_direct {
        return Vec::new();
    }
    let Some(scope) = state.selector.scope() else {
        return Vec::new();
    };
    let prefix = if scope == "." {
        String::new()
    } else {
        format!("{scope}/")
    };
    let mut children = BTreeSet::new();
    let mut has_direct_files = false;
    for member in &state.members {
        let Some(relative) = member.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some((child, _)) = relative.split_once('/') else {
            has_direct_files = true;
            continue;
        };
        children.insert(if scope == "." {
            child.to_string()
        } else {
            format!("{scope}/{child}")
        });
    }
    let mut subdivisions = children
        .into_iter()
        .map(|scope| Subdivision {
            display_name: scope.clone(),
            scope,
            direct_only: false,
        })
        .collect::<Vec<_>>();
    // A mixed parent can contain meaningful files directly alongside child
    // directories. Give those files a bounded residual subject so they do
    // not remain permanently neutral merely because they have no directory
    // component to recurse into.
    if has_direct_files && !subdivisions.is_empty() {
        subdivisions.insert(
            0,
            Subdivision {
                scope: scope.to_string(),
                direct_only: true,
                display_name: format!("{scope} (direct files)"),
            },
        );
    }
    subdivisions
}

fn child_identity(
    selector: &SubjectSelector,
    child: &Subdivision,
) -> Option<(String, SubjectSelector)> {
    let suffix = if child.direct_only { ":direct" } else { "" };
    match selector {
        SubjectSelector::RepositoryArea { .. } => Some((
            format!("area:repository:{}{suffix}", child.scope),
            SubjectSelector::RepositoryArea {
                scope: child.scope.clone(),
                direct_only: child.direct_only,
            },
        )),
        SubjectSelector::WorkspaceArea { package_root, .. } => Some((
            format!("area:workspace:{package_root}:{}{suffix}", child.scope),
            SubjectSelector::WorkspaceArea {
                package_root: package_root.clone(),
                scope: child.scope.clone(),
                direct_only: child.direct_only,
            },
        )),
        SubjectSelector::Project { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::json;

    use super::{EvidenceItem, RepositoryEvidencePack, Submission, validate};

    fn evidence() -> RepositoryEvidencePack {
        RepositoryEvidencePack {
            algorithm: crate::recon::EVIDENCE_ALGORITHM,
            subject_key: "area:repository:docs".into(),
            subject_kind: "area".into(),
            member_count: 2,
            language_counts: Default::default(),
            chunk_kind_counts: Default::default(),
            items: vec![EvidenceItem {
                id: "E001".into(),
                kind: "outline".into(),
                source: Some("docs/read.ts".into()),
                start_line: Some(1),
                end_line: Some(1),
                content: "exported function renderGuide".into(),
            }],
            rendered: String::new(),
        }
    }

    #[test]
    fn repository_output_is_closed_and_evidence_backed() -> Result<()> {
        let submission: Submission = serde_json::from_value(json!({
            "role": "documentation",
            "confidence": "likely",
            "explanation": "the scope exports guide rendering",
            "evidence": ["E001"],
        }))?;
        let validated = validate(&submission, &evidence())?;
        assert_eq!(validated.role, "documentation");

        let mut bad = submission;
        bad.evidence = vec!["E999".into()];
        assert!(validate(&bad, &evidence()).is_err());
        Ok(())
    }

    #[test]
    fn unknown_cannot_claim_likely_confidence() -> Result<()> {
        let submission: Submission = serde_json::from_value(json!({
            "role": "unknown",
            "confidence": "likely",
            "explanation": "not enough evidence",
            "evidence": ["E001"],
        }))?;
        assert!(validate(&submission, &evidence()).is_err());
        Ok(())
    }
}
