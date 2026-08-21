use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::protocol::{
    DeclarationSite, FileOwnership, InputValidation, MemberQuery, ProjectAnswer, ProjectSummary,
    TypeScriptIdentity,
};

#[derive(Debug, Clone)]
pub struct EnrichOptions<'a> {
    pub database: Option<&'a Path>,
    pub sidecar: Option<&'a Path>,
    pub node: &'a str,
    pub timeout: Duration,
    pub files: Vec<String>,
    pub packages: Vec<String>,
    pub members: Vec<String>,
    pub roles: Vec<String>,
    pub max_occurrences: Option<usize>,
    pub include_all: bool,
    pub dry_run: bool,
    /// Internal watcher policy: seed a new snapshot batch from the previous
    /// active batch when project and fact identities still validate.
    pub carry_forward: bool,
    /// Discard exact-snapshot reuse, staging resume, and watch carry. Exposed
    /// as manual `enrich --full` and used by the watch drift-flush timer.
    pub force_full: bool,
    /// Current watcher event paths used only to order delta work.
    pub dirty_files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EnrichReport {
    pub snapshot: String,
    pub batch_id: i64,
    pub occurrences_queried: usize,
    pub occurrences_discovered: usize,
    pub occurrences_eligible: usize,
    pub occurrences_selected: usize,
    pub occurrences_omitted: usize,
    pub occurrences_deprioritized_builtin_receiver: usize,
    pub occurrences_skipped_foreign_namesake: usize,
    pub occurrences_resumed: usize,
    pub request_batches: usize,
    pub unknown_answers: usize,
    pub unknown_projects: Vec<String>,
    pub unmapped_declarations: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unmapped_declaration_contexts: BTreeMap<String, usize>,
    pub facts_published: usize,
    pub checker_version: String,
    pub checker_source: String,
    pub projects: usize,
    pub projects_carried: usize,
    pub occurrences_carried: usize,
    pub configured_projects: usize,
    pub configuration_problems: usize,
    /// Eligible source files whose member-call candidates have no configured
    /// TypeScript-project owner. These files remain fully indexed; this is
    /// only the checker-enrichment coverage boundary.
    pub files_without_configured_project: usize,
    pub occurrences_without_configured_project: usize,
    pub occurrences_skipped_inferred_project: usize,
    pub occurrences_avoided_by_tooling_filter: usize,
    pub occurrences_using_tooling_fallback: usize,
    pub project_decisions: Vec<ProjectDecision>,
    pub dry_run: bool,
    pub peak_rss_bytes: u64,
    pub peak_heap_bytes: u64,
}

#[derive(Debug)]
struct ProjectFailure {
    project_id: String,
    retryable: bool,
}

#[derive(Debug)]
struct PartialEnrichmentError {
    batch_id: i64,
    facts_published: usize,
    failures: Vec<ProjectFailure>,
}

impl PartialEnrichmentError {
    fn retryable(&self) -> bool {
        self.failures.iter().any(|failure| failure.retryable)
    }
}

impl fmt::Display for PartialEnrichmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failed = self
            .failures
            .iter()
            .map(|failure| failure.project_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            formatter,
            "checker enrichment activated partial batch {} with {} staged fact(s), but {} project(s) failed: {}; unresolved ownership remains possible",
            self.batch_id,
            self.facts_published,
            self.failures.len(),
            failed
        )?;
        if self.retryable() {
            formatter.write_str("; transient failures may be resumed")
        } else {
            formatter.write_str("; deterministic failures wait for changed inputs")
        }
    }
}

impl std::error::Error for PartialEnrichmentError {}

/// A partial batch with only deterministic per-project failures is usable and
/// should not enter watch's tight phase-retry loop. A later structural
/// generation or periodic reconciliation naturally attempts those projects
/// again.
pub fn is_terminal_partial_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<PartialEnrichmentError>()
        .is_some_and(|partial| !partial.retryable())
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDecision {
    pub project_id: String,
    pub purpose: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub purpose_reasons: Vec<String>,
    pub selected_occurrences: usize,
    pub excluded_occurrences: usize,
    pub fallback_occurrences: usize,
}

struct ProjectPlanning {
    projects: BTreeMap<String, Vec<Occurrence>>,
    decisions: Vec<ProjectDecision>,
    occurrences_avoided_by_tooling_filter: usize,
    occurrences_using_tooling_fallback: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct InferredProjectCoverage {
    files_without_configured_project: usize,
    occurrences_without_configured_project: usize,
    occurrences_skipped_inferred_project: usize,
}

#[derive(Debug, Clone)]
struct Occurrence {
    id: i64,
    file_id: i64,
    file: String,
    hash: String,
    call_start: i64,
    call_end: i64,
    receiver_start: i64,
    receiver_end: i64,
    property_start: i64,
    property_end: i64,
    member: String,
    role: String,
    package: String,
    boundary_rank: i64,
    deterministically_resolved: bool,
    builtin_receiver: bool,
    runtime_namesake: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OccurrenceIdentity {
    file: String,
    hash: String,
    call_start: i64,
    call_end: i64,
    receiver_start: i64,
    receiver_end: i64,
    property_start: i64,
    property_end: i64,
}

impl From<&Occurrence> for OccurrenceIdentity {
    fn from(occurrence: &Occurrence) -> Self {
        Self {
            file: occurrence.file.clone(),
            hash: occurrence.hash.clone(),
            call_start: occurrence.call_start,
            call_end: occurrence.call_end,
            receiver_start: occurrence.receiver_start,
            receiver_end: occurrence.receiver_end,
            property_start: occurrence.property_start,
            property_end: occurrence.property_end,
        }
    }
}

#[derive(Debug)]
struct CarryCoverage {
    input_fingerprint: String,
    status: String,
}

#[derive(Debug, Clone)]
struct CarryFact {
    receiver_type: Option<String>,
    target: Target,
    confidence: String,
    input_fingerprint: String,
}

#[derive(Debug, Default)]
struct CarryOutcome {
    projects_carried: usize,
    occurrences_carried: usize,
    projects_requiring_check: BTreeSet<String>,
}

struct StagingPlan<'a> {
    snapshot: &'a str,
    plan_fingerprint: &'a str,
    checker: &'a TypeScriptIdentity,
    protocol: u32,
    selected_occurrences: usize,
    projects: &'a BTreeMap<String, Vec<Occurrence>>,
    project_fingerprints: &'a BTreeMap<String, String>,
    force_new: bool,
}

#[derive(Debug, Clone)]
struct Target {
    anchor: String,
    fingerprint: String,
}

#[derive(Debug, Clone)]
struct PendingFact {
    occurrence: Occurrence,
    project_id: String,
    receiver_type: Option<String>,
    target: Target,
    confidence: String,
    input_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ValidatedInput {
    kind: String,
    path: String,
    source_hash: String,
}

/// One enrichment invocation observes each checker input at most once. Shared
/// TypeScript libraries appear in many project manifests; caching the first
/// digest avoids re-reading and re-hashing the same bytes for every project.
struct InputFreshnessCache<'a> {
    root: &'a Path,
    digests: HashMap<PathBuf, Option<String>>,
}

impl<'a> InputFreshnessCache<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            digests: HashMap::new(),
        }
    }

    fn matches(&mut self, input: &ValidatedInput) -> bool {
        let path = input_path(self.root, input);
        self.digests
            .entry(path.clone())
            .or_insert_with(|| {
                fs::read(path)
                    .ok()
                    .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
            })
            .as_deref()
            .is_some_and(|hash| hash == input.source_hash)
    }
}

#[derive(Debug, Clone)]
struct PendingProjectAnswer {
    occurrence: Occurrence,
    project_id: String,
    input_fingerprint: String,
    status: &'static str,
}

/// What one occurrence's whole checker answer contributed.
#[derive(Debug, Default)]
struct OccurrenceOutcome {
    facts: Vec<PendingFact>,
    projects: Vec<PendingProjectAnswer>,
    unknown_answers: usize,
    unmapped_declarations: usize,
    unmapped_declaration_contexts: BTreeMap<String, usize>,
}

#[cfg(test)]
struct PublishPlan<'a> {
    snapshot: &'a str,
    checker: &'a TypeScriptIdentity,
    protocol: u32,
    input_fingerprint: &'a str,
    facts: &'a [PendingFact],
    projects: &'a [PendingProjectAnswer],
    inputs: &'a [ValidatedInput],
}

pub fn enrich(root: &Path, options: &EnrichOptions<'_>) -> Result<EnrichReport> {
    if options.timeout.is_zero() {
        bail!("--timeout must be greater than zero seconds");
    }
    if options.max_occurrences == Some(0) {
        bail!("--max-occurrences must be greater than zero");
    }
    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    let conn = match options.database {
        Some(path) => crate::store::open_path(path)?,
        None => crate::store::open(&canonical_root)?,
    };
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let discovered = load_occurrences(&conn)?;
    let occurrences_discovered = discovered.len();
    let (eligible, selection) = select_eligible(discovered, options);
    let occurrences_eligible = eligible.len();
    if eligible.is_empty() {
        return Ok(EnrichReport {
            snapshot,
            batch_id: 0,
            occurrences_queried: 0,
            occurrences_discovered,
            occurrences_eligible,
            occurrences_selected: 0,
            occurrences_omitted: 0,
            occurrences_deprioritized_builtin_receiver: selection.builtin_receiver,
            occurrences_skipped_foreign_namesake: selection.foreign_namesake,
            occurrences_resumed: 0,
            request_batches: 0,
            unknown_answers: 0,
            unknown_projects: Vec::new(),
            unmapped_declarations: 0,
            unmapped_declaration_contexts: BTreeMap::new(),
            facts_published: 0,
            checker_version: "not-invoked".into(),
            checker_source: "not-invoked".into(),
            projects: 0,
            projects_carried: 0,
            occurrences_carried: 0,
            configured_projects: 0,
            configuration_problems: 0,
            files_without_configured_project: 0,
            occurrences_without_configured_project: 0,
            occurrences_skipped_inferred_project: 0,
            occurrences_avoided_by_tooling_filter: 0,
            occurrences_using_tooling_fallback: 0,
            project_decisions: Vec::new(),
            dry_run: options.dry_run,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }
    super::process::begin_interrupt_scope().context("failed to install checker Ctrl-C handler")?;

    let mut planner = super::launch(&canonical_root, options.sidecar, None, options.node)?;
    planner
        .register_interrupts()
        .context("failed to install checker Ctrl-C handler")?;
    let dirty_files = current_dirty_source_files(&conn, &options.dirty_files)?;
    let mut planning_files = eligible
        .iter()
        .map(|occurrence| occurrence.file.clone())
        .collect::<BTreeSet<_>>();
    planning_files.extend(dirty_files.iter().cloned());
    let mut ownership =
        planner.plan_members(planning_files.into_iter().collect(), options.timeout)?;
    let protocol = planner.versions.protocol;
    drop(planner);
    apply_repository_project_policy(&conn, &mut ownership.files, &mut ownership.projects)?;
    let (eligible, inferred_coverage) =
        gate_inferred_projects(eligible, &ownership.files, options.include_all)?;
    let ordered = spread_occurrences(eligible);
    let selected = match options.max_occurrences {
        Some(limit) => ordered.into_iter().take(limit).collect::<Vec<_>>(),
        None => ordered,
    };
    if selected.is_empty() {
        return Ok(EnrichReport {
            snapshot,
            batch_id: 0,
            occurrences_queried: 0,
            occurrences_discovered,
            occurrences_eligible,
            occurrences_selected: 0,
            occurrences_omitted: occurrences_eligible,
            occurrences_deprioritized_builtin_receiver: selection.builtin_receiver,
            occurrences_skipped_foreign_namesake: selection.foreign_namesake,
            occurrences_resumed: 0,
            request_batches: 0,
            unknown_answers: 0,
            unknown_projects: Vec::new(),
            unmapped_declarations: 0,
            unmapped_declaration_contexts: BTreeMap::new(),
            facts_published: 0,
            checker_version: ownership.typescript.version,
            checker_source: ownership.typescript.source,
            projects: 0,
            projects_carried: 0,
            occurrences_carried: 0,
            configured_projects: ownership.projects.len(),
            configuration_problems: ownership.configuration_problems.len(),
            files_without_configured_project: inferred_coverage.files_without_configured_project,
            occurrences_without_configured_project: inferred_coverage
                .occurrences_without_configured_project,
            occurrences_skipped_inferred_project: inferred_coverage
                .occurrences_skipped_inferred_project,
            occurrences_avoided_by_tooling_filter: 0,
            occurrences_using_tooling_fallback: 0,
            project_decisions: Vec::new(),
            dry_run: options.dry_run,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }
    verify_selected_sources(&canonical_root, &conn, &selected)?;
    let ProjectPlanning {
        projects: project_plan,
        decisions: project_decisions,
        occurrences_avoided_by_tooling_filter,
        occurrences_using_tooling_fallback,
    } = build_project_plan(
        &selected,
        &ownership.files,
        &ownership.projects,
        options.include_all,
    )?;
    let plan_fingerprint = plan_fingerprint(
        &snapshot,
        &selected,
        &project_plan,
        &ownership.typescript,
        protocol,
        options,
    );
    let project_fingerprints = project_planning_fingerprints(
        &project_plan,
        &ownership.projects,
        &ownership.typescript,
        protocol,
    );
    let dirty_projects = dirty_projects(&ownership.files, &dirty_files, options.include_all);

    if options.dry_run {
        return Ok(EnrichReport {
            snapshot,
            batch_id: 0,
            occurrences_queried: 0,
            occurrences_discovered,
            occurrences_eligible,
            occurrences_selected: selected.len(),
            occurrences_omitted: occurrences_eligible.saturating_sub(selected.len()),
            occurrences_deprioritized_builtin_receiver: selection.builtin_receiver,
            occurrences_skipped_foreign_namesake: selection.foreign_namesake,
            occurrences_resumed: 0,
            request_batches: 0,
            unknown_answers: 0,
            unknown_projects: Vec::new(),
            unmapped_declarations: 0,
            unmapped_declaration_contexts: BTreeMap::new(),
            facts_published: 0,
            checker_version: ownership.typescript.version,
            checker_source: ownership.typescript.source,
            projects: project_plan.len(),
            projects_carried: 0,
            occurrences_carried: 0,
            configured_projects: ownership.projects.len(),
            configuration_problems: ownership.configuration_problems.len(),
            files_without_configured_project: inferred_coverage.files_without_configured_project,
            occurrences_without_configured_project: inferred_coverage
                .occurrences_without_configured_project,
            occurrences_skipped_inferred_project: inferred_coverage
                .occurrences_skipped_inferred_project,
            occurrences_avoided_by_tooling_filter,
            occurrences_using_tooling_fallback,
            project_decisions,
            dry_run: true,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }

    let mut input_freshness = InputFreshnessCache::new(&canonical_root);
    if !options.force_full
        && let Some(batch_id) = reusable_active_batch(
            &conn,
            &snapshot,
            &plan_fingerprint,
            project_plan.keys(),
            &mut input_freshness,
        )?
    {
        let facts_published = conn.query_row(
            "SELECT count(*) FROM checker_enrichments WHERE batch_id=?1",
            [batch_id],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let unknown_projects = load_unknown_projects(&conn, batch_id)?;
        return Ok(EnrichReport {
            snapshot,
            batch_id,
            occurrences_queried: 0,
            occurrences_discovered,
            occurrences_eligible,
            occurrences_selected: selected.len(),
            occurrences_omitted: occurrences_eligible.saturating_sub(selected.len()),
            occurrences_deprioritized_builtin_receiver: selection.builtin_receiver,
            occurrences_skipped_foreign_namesake: selection.foreign_namesake,
            occurrences_resumed: selected.len(),
            request_batches: 0,
            unknown_answers: 0,
            unknown_projects,
            unmapped_declarations: 0,
            unmapped_declaration_contexts: BTreeMap::new(),
            facts_published,
            checker_version: ownership.typescript.version,
            checker_source: ownership.typescript.source,
            projects: project_plan.len(),
            projects_carried: 0,
            occurrences_carried: 0,
            configured_projects: ownership.projects.len(),
            configuration_problems: ownership.configuration_problems.len(),
            files_without_configured_project: inferred_coverage.files_without_configured_project,
            occurrences_without_configured_project: inferred_coverage
                .occurrences_without_configured_project,
            occurrences_skipped_inferred_project: inferred_coverage
                .occurrences_skipped_inferred_project,
            occurrences_avoided_by_tooling_filter,
            occurrences_using_tooling_fallback,
            project_decisions,
            dry_run: false,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }
    if !options.carry_forward
        && deactivate_stale_active_batch(&conn, &snapshot, &mut input_freshness)?
    {
        crate::structural::rebuild_projection(&conn, &snapshot)?;
    }

    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: &plan_fingerprint,
            checker: &ownership.typescript,
            protocol,
            selected_occurrences: selected.len(),
            projects: &project_plan,
            project_fingerprints: &project_fingerprints,
            force_new: options.force_full,
        },
    )?;
    let carry = if options.carry_forward && !options.force_full {
        carry_forward_projects(
            &conn,
            batch_id,
            &ownership.typescript,
            protocol,
            &project_plan,
            &project_fingerprints,
            &mut input_freshness,
        )?
    } else {
        CarryOutcome::default()
    };
    if carry.occurrences_carried != 0 {
        eprintln!(
            "checker enrichment: carried {}/{} occurrences across {}/{} projects",
            carry.occurrences_carried,
            selected.len(),
            carry.projects_carried,
            project_plan.len()
        );
    }
    let mut priority_projects = dirty_projects;
    priority_projects.extend(carry.projects_requiring_check.iter().cloned());
    let mut occurrences_queried = 0;
    let mut occurrences_resumed = 0;
    let mut request_batches = 0;
    let mut unknown_answers = 0;
    let mut unmapped_declarations = 0;
    let mut unmapped_declaration_contexts = BTreeMap::<String, usize>::new();
    let mut peak_rss_bytes = 0;
    let mut peak_heap_bytes = 0;
    let mut failed_projects = Vec::<ProjectFailure>::new();

    for (project_index, (project_id, occurrences)) in
        projects_in_execution_order(&project_plan, &priority_projects)
            .into_iter()
            .enumerate()
    {
        if super::process::cancellation_pending() {
            bail!("checker enrichment interrupted; staged work retained");
        }
        if project_complete_and_fresh(&conn, batch_id, project_id, &mut input_freshness)? {
            occurrences_resumed += occurrences.len();
            continue;
        }
        let mut completed = completed_occurrences(&conn, batch_id, project_id)?;
        if completed.len() == occurrences.len() {
            reset_project_staging(&conn, batch_id, project_id)?;
            completed.clear();
        }
        let mut pending = occurrences
            .iter()
            .filter(|occurrence| !completed.contains(&occurrence.id))
            .cloned()
            .collect::<Vec<_>>();
        pending.sort_by(|left, right| {
            (!dirty_files.contains(&left.file))
                .cmp(&(!dirty_files.contains(&right.file)))
                .then_with(|| {
                    (&left.file, left.call_start, left.id).cmp(&(
                        &right.file,
                        right.call_start,
                        right.id,
                    ))
                })
        });
        occurrences_resumed += completed.len();
        mark_project_pending(&conn, batch_id, project_id)?;
        eprintln!(
            "checker enrichment: project {}/{} {} ({} pending, {} resumed)",
            project_index + 1,
            project_plan.len(),
            project_id,
            pending.len(),
            completed.len()
        );
        let project_result = execute_project(
            &canonical_root,
            options,
            &conn,
            batch_id,
            project_id,
            &pending,
            &ownership.typescript,
            &mut request_batches,
            &mut occurrences_queried,
            &mut unknown_answers,
            &mut unmapped_declarations,
            &mut unmapped_declaration_contexts,
            &mut peak_rss_bytes,
            &mut peak_heap_bytes,
        );
        if let Err(error) = project_result {
            if project_was_interrupted(&error) {
                bail!(
                    "checker enrichment interrupted during project {project_id}; staged work retained: {error:#}"
                );
            }
            let message = error.to_string();
            let retryable = project_failure_is_retryable(&error);
            mark_project_failed(&conn, batch_id, project_id, occurrences, &message)?;
            eprintln!(
                "checker enrichment: project {project_id} failed disposition={}: {error:#}",
                if retryable { "retryable" } else { "terminal" }
            );
            failed_projects.push(ProjectFailure {
                project_id: project_id.clone(),
                retryable,
            });
        }
    }

    if super::process::cancellation_pending() {
        bail!("checker enrichment interrupted before activation; staged work retained");
    }

    if !failed_projects.is_empty() {
        let facts_published =
            activate_staging_batch(&canonical_root, &conn, batch_id, &snapshot, true)?;
        crate::structural::rebuild_projection(&conn, &snapshot)?;
        return Err(PartialEnrichmentError {
            batch_id,
            facts_published,
            failures: failed_projects,
        }
        .into());
    }

    let facts_published =
        activate_staging_batch(&canonical_root, &conn, batch_id, &snapshot, false)?;
    crate::structural::rebuild_projection(&conn, &snapshot)?;

    Ok(EnrichReport {
        snapshot,
        batch_id,
        occurrences_queried,
        occurrences_discovered,
        occurrences_eligible,
        occurrences_selected: selected.len(),
        occurrences_omitted: occurrences_eligible.saturating_sub(selected.len()),
        occurrences_deprioritized_builtin_receiver: selection.builtin_receiver,
        occurrences_skipped_foreign_namesake: selection.foreign_namesake,
        occurrences_resumed,
        request_batches,
        unknown_answers,
        unknown_projects: load_unknown_projects(&conn, batch_id)?,
        unmapped_declarations,
        unmapped_declaration_contexts,
        facts_published,
        checker_version: ownership.typescript.version,
        checker_source: ownership.typescript.source,
        projects: project_plan.len(),
        projects_carried: carry.projects_carried,
        occurrences_carried: carry.occurrences_carried,
        configured_projects: ownership.projects.len(),
        configuration_problems: ownership.configuration_problems.len(),
        files_without_configured_project: inferred_coverage.files_without_configured_project,
        occurrences_without_configured_project: inferred_coverage
            .occurrences_without_configured_project,
        occurrences_skipped_inferred_project: inferred_coverage
            .occurrences_skipped_inferred_project,
        occurrences_avoided_by_tooling_filter,
        occurrences_using_tooling_fallback,
        project_decisions,
        dry_run: false,
        peak_rss_bytes,
        peak_heap_bytes,
    })
}

fn project_was_interrupted(error: &anyhow::Error) -> bool {
    super::process::cancellation_pending() || canceled_checker_error(error)
}

fn canceled_checker_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<super::process::CheckerError>()
        .is_some_and(|error| matches!(error, super::process::CheckerError::Canceled(_)))
}

fn project_failure_is_retryable(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<super::process::CheckerError>() {
        Some(super::process::CheckerError::Remote { code, .. }) => {
            matches!(
                code.as_str(),
                "busy"
                    | "EIO"
                    | "EINTR"
                    | "EAGAIN"
                    | "ENOMEM"
                    | "EBUSY"
                    | "EMFILE"
                    | "ENFILE"
                    | "ETIMEDOUT"
                    | "ENETDOWN"
                    | "ENETUNREACH"
                    | "ENETRESET"
                    | "ECONNABORTED"
                    | "ECONNRESET"
                    | "ENOBUFS"
                    | "ESTALE"
            )
        }
        // Launch/request transport failures may heal without a new structural
        // snapshot. A child exit is a crash (including V8 heap aborts), so it
        // follows the project-terminal partial/resume path instead of the
        // watcher's uncapped phase-retry loop. Protocol, cancellation, and
        // unknown local errors also stay terminal.
        Some(
            super::process::CheckerError::Spawn(_)
            | super::process::CheckerError::Io(_)
            | super::process::CheckerError::Timeout(_),
        ) => true,
        Some(
            super::process::CheckerError::Protocol(_)
            | super::process::CheckerError::ChildExited(_)
            | super::process::CheckerError::Canceled(_),
        )
        | None => false,
    }
}

/// ECMAScript / host names that usually resolve into the TypeScript standard
/// library or `@types`. This is an ordering hint only: project-wide ambient
/// declarations can make a file-local unbound name resolve into repository
/// code.
const BUILTIN_RECEIVER_GLOBALS: &[&str] = &[
    "console",
    "JSON",
    "Math",
    "Object",
    "Array",
    "String",
    "Number",
    "Boolean",
    "Symbol",
    "Reflect",
    "Proxy",
    "Promise",
    "Date",
    "RegExp",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "EvalError",
    "URIError",
    "AggregateError",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "WeakRef",
    "FinalizationRegistry",
    "ArrayBuffer",
    "SharedArrayBuffer",
    "DataView",
    "Atomics",
    "Intl",
    "BigInt",
    "globalThis",
    "window",
    "document",
    "navigator",
    "performance",
    "process",
    "Buffer",
    "require",
    "module",
    "exports",
    "URL",
    "URLSearchParams",
    "TextEncoder",
    "TextDecoder",
    "AbortController",
    "AbortSignal",
    "Headers",
    "Response",
    "Request",
    "FormData",
    "Blob",
    "Event",
    "EventTarget",
    "crypto",
    "fetch",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "Uint8Array",
    "Uint16Array",
    "Uint32Array",
    "Uint8ClampedArray",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

/// Node core module spellings used by the advisory ordering hint. Bare
/// specifiers can be shadowed by tsconfig paths and file-level import names can
/// be shadowed lexically, so this list must never drive hard exclusion.
const NODE_CORE_MODULES: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

fn load_occurrences(conn: &Connection) -> Result<Vec<Occurrence>> {
    let deterministically_resolved = conn
        .prepare(
            "SELECT DISTINCT CAST(json_extract(detail_json, '$.memberCallId') AS INTEGER)
             FROM resolved_edges
             WHERE confidence IN ('certain', 'likely') AND provenance!='checker'
               AND json_type(detail_json, '$.memberCallId')='integer'",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    let mut statement = conn.prepare(
        "SELECT call.rowid, file.id, file.path, file.hash,
                call.start, call.end, call.receiver_start, call.receiver_end,
                call.property_start, call.property_end, call.prop, file.role,
                COALESCE(package.name,
                  CASE WHEN instr(file.path, '/') > 0
                       THEN substr(file.path, 1, instr(file.path, '/') - 1)
                       ELSE '(root)' END),
                CASE WHEN EXISTS(
                  SELECT 1 FROM symbols owner
                  WHERE owner.file_id=file.id AND owner.exported=1
                    AND owner.start<=call.start AND owner.end>=call.start
                ) OR EXISTS(
                  SELECT 1 FROM entity_occurrences entity
                  WHERE entity.file_id=file.id
                    AND entity.start<=call.start AND entity.end>=call.start
                ) THEN 0 ELSE 1 END,
                CASE WHEN (
                  call.object IN (SELECT value FROM json_each(?1))
                  AND call.receiver_unbound=1
                ) OR EXISTS(
                  SELECT 1 FROM imports binding
                  WHERE binding.file_id=file.id AND binding.local_name=call.object
                    AND (binding.request IN (SELECT value FROM json_each(?2))
                      OR binding.request LIKE 'node:%')
                ) THEN 1 ELSE 0 END,
                CASE WHEN EXISTS(
                  SELECT 1 FROM symbols target
                  JOIN files target_file ON target_file.id=target.file_id
                  LEFT JOIN repository_file_policy target_policy
                    ON target_policy.file_id=target_file.id
                  WHERE target.name=call.prop
                    AND (target_policy.effective_role='runtime'
                      OR (target_policy.file_id IS NULL
                        AND target_file.role IN ('production', 'unknown')))
                ) THEN 1 ELSE 0 END
         FROM member_calls call
         JOIN files file ON file.id=call.file_id
         LEFT JOIN package_instances package ON package.id=file.package_instance_id
         WHERE file.origin IN ('repository', 'workspace')
           AND call.end > call.start
           AND call.receiver_end > call.receiver_start
           AND call.property_end > call.property_start
           AND EXISTS(SELECT 1 FROM symbols target WHERE target.name=call.prop)
         ORDER BY file.path, call.start, call.rowid",
    )?;
    let globals = serde_json::to_string(BUILTIN_RECEIVER_GLOBALS)?;
    let core_modules = serde_json::to_string(NODE_CORE_MODULES)?;
    let rows = statement.query_map(params![globals, core_modules], |row| {
        Ok(Occurrence {
            id: row.get(0)?,
            file_id: row.get(1)?,
            file: row.get(2)?,
            hash: row.get(3)?,
            call_start: row.get(4)?,
            call_end: row.get(5)?,
            receiver_start: row.get(6)?,
            receiver_end: row.get(7)?,
            property_start: row.get(8)?,
            property_end: row.get(9)?,
            member: row.get(10)?,
            role: row.get(11)?,
            package: row.get(12)?,
            boundary_rank: row.get(13)?,
            deterministically_resolved: deterministically_resolved.contains(&row.get(0)?),
            builtin_receiver: row.get::<_, i64>(14)? != 0,
            runtime_namesake: row.get::<_, i64>(15)? != 0,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SelectionStats {
    builtin_receiver: usize,
    foreign_namesake: usize,
}

fn select_eligible(
    occurrences: Vec<Occurrence>,
    options: &EnrichOptions<'_>,
) -> (Vec<Occurrence>, SelectionStats) {
    let mut stats = SelectionStats::default();
    let eligible = occurrences
        .into_iter()
        .filter(|occurrence| {
            let in_scope = (!occurrence.deterministically_resolved || options.include_all)
                && (if options.roles.is_empty() {
                    options.include_all
                        || matches!(occurrence.role.as_str(), "production" | "unknown")
                } else {
                    options.roles.contains(&occurrence.role)
                })
                && (options.files.is_empty()
                    || options.files.iter().any(|file| {
                        occurrence.file == *file
                            || occurrence
                                .file
                                .strip_prefix(file)
                                .is_some_and(|suffix| suffix.starts_with('/'))
                    }))
                && (options.packages.is_empty() || options.packages.contains(&occurrence.package))
                && (options.members.is_empty() || options.members.contains(&occurrence.member));
            if !in_scope {
                return false;
            }
            // A runtime namesake is a necessary precondition for the default
            // runtime graph to anchor a fact. Builtin-looking receivers are
            // only an ordering hint: file-local scope and import spelling
            // cannot prove that project-wide TypeScript resolution lands in
            // lib/@types, so they must never remove an occurrence.
            if !options.include_all && !occurrence.runtime_namesake {
                stats.foreign_namesake += 1;
                return false;
            }
            if occurrence.builtin_receiver {
                stats.builtin_receiver += 1;
            }
            true
        })
        .collect();
    (eligible, stats)
}

/// Stable rank-tier spreading. This ordering matters for visible progress and
/// for an explicitly capped diagnostic run, but uncapped manual enrichment
/// still consumes every eligible occurrence.
fn spread_occurrences(occurrences: Vec<Occurrence>) -> Vec<Occurrence> {
    let mut tiers =
        BTreeMap::<(i64, bool), BTreeMap<String, BTreeMap<String, VecDeque<Occurrence>>>>::new();
    for occurrence in occurrences {
        tiers
            // Within the structural boundary tier, process ordinary receivers
            // before builtin-looking ones. An uncapped run still consumes both.
            .entry((occurrence.boundary_rank, occurrence.builtin_receiver))
            .or_default()
            .entry(occurrence.package.clone())
            .or_default()
            .entry(occurrence.file.clone())
            .or_default()
            .push_back(occurrence);
    }
    let mut ordered = Vec::new();
    for (_, packages) in tiers {
        let mut active_packages = packages
            .into_iter()
            .map(|(package, files)| {
                (
                    package,
                    files
                        .into_values()
                        .collect::<VecDeque<VecDeque<Occurrence>>>(),
                )
            })
            .collect::<VecDeque<_>>();
        while let Some((package, mut files)) = active_packages.pop_front() {
            let mut occurrences = files
                .pop_front()
                .expect("active packages contain non-empty file queues");
            ordered.push(
                occurrences
                    .pop_front()
                    .expect("active files contain an occurrence"),
            );
            if !occurrences.is_empty() {
                files.push_back(occurrences);
            }
            if !files.is_empty() {
                active_packages.push_back((package, files));
            }
        }
    }
    ordered
}

fn verify_selected_sources(root: &Path, conn: &Connection, selected: &[Occurrence]) -> Result<()> {
    let mut files = BTreeMap::new();
    for occurrence in selected {
        files
            .entry((occurrence.file_id, occurrence.file.as_str()))
            .or_insert(occurrence.hash.as_str());
    }
    for ((file_id, display), hash) in files {
        let path = crate::store::file_source_path(conn, root, file_id)?;
        verify_source_hash(&path, hash, display)?;
    }
    Ok(())
}

fn is_inferred_project(project_id: &str) -> bool {
    project_id.starts_with("inferred:")
}

/// Keep the checker boundary explicit without removing files from any other
/// index plane. Ownership planning still sees every otherwise-eligible file so
/// the report can distinguish an inferred-project skip from role, name, or
/// operator-cap filtering. `--all` is the deliberate escape hatch.
fn gate_inferred_projects(
    occurrences: Vec<Occurrence>,
    ownership: &[FileOwnership],
    include_all: bool,
) -> Result<(Vec<Occurrence>, InferredProjectCoverage)> {
    let owners = ownership
        .iter()
        .map(|entry| (entry.file.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut files_without_configured_project = BTreeSet::new();
    let mut kept = Vec::with_capacity(occurrences.len());
    let mut coverage = InferredProjectCoverage::default();
    for occurrence in occurrences {
        let ownership = owners.get(occurrence.file.as_str()).with_context(|| {
            format!("checker omitted project ownership for {}", occurrence.file)
        })?;
        if ownership.project_ids.is_empty() {
            bail!("checker returned no owning project for {}", occurrence.file);
        }
        let without_configured_project = ownership
            .project_ids
            .iter()
            .all(|project_id| is_inferred_project(project_id));
        if without_configured_project {
            files_without_configured_project.insert(occurrence.file.clone());
            coverage.occurrences_without_configured_project += 1;
            if !include_all {
                coverage.occurrences_skipped_inferred_project += 1;
                continue;
            }
        }
        kept.push(occurrence);
    }
    coverage.files_without_configured_project = files_without_configured_project.len();
    Ok((kept, coverage))
}

fn build_project_plan(
    selected: &[Occurrence],
    ownership: &[FileOwnership],
    projects: &[ProjectSummary],
    include_inferred: bool,
) -> Result<ProjectPlanning> {
    let owners = ownership
        .iter()
        .map(|entry| (entry.file.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let summaries = projects
        .iter()
        .map(|project| (project.project_id.as_str(), project))
        .collect::<HashMap<_, _>>();
    let mut plan = BTreeMap::<String, Vec<Occurrence>>::new();
    let mut decisions = BTreeMap::<String, ProjectDecision>::new();
    let mut occurrences_avoided_by_tooling_filter = 0;
    let mut occurrences_using_tooling_fallback = 0;
    for occurrence in selected {
        let ownership = owners.get(occurrence.file.as_str()).with_context(|| {
            format!("checker omitted project ownership for {}", occurrence.file)
        })?;
        if ownership.project_ids.is_empty() {
            bail!("checker returned no owning project for {}", occurrence.file);
        }
        if ownership
            .project_ids
            .iter()
            .any(|project_id| ownership.excluded_project_ids.contains(project_id))
        {
            bail!(
                "checker selected and excluded the same project owner for {}",
                occurrence.file
            );
        }
        if ownership.tooling_fallback {
            occurrences_using_tooling_fallback += 1;
        }
        for project_id in &ownership.project_ids {
            if is_inferred_project(project_id) && !include_inferred {
                continue;
            }
            plan.entry(project_id.clone())
                .or_default()
                .push(occurrence.clone());
            let decision = project_decision(&mut decisions, &summaries, project_id);
            decision.selected_occurrences += 1;
            if ownership.tooling_fallback {
                decision.fallback_occurrences += 1;
            }
        }
        for project_id in &ownership.excluded_project_ids {
            let decision = project_decision(&mut decisions, &summaries, project_id);
            decision.excluded_occurrences += 1;
            occurrences_avoided_by_tooling_filter += 1;
        }
    }
    for occurrences in plan.values_mut() {
        occurrences.sort_by(|left, right| {
            (&left.file, left.call_start, left.id).cmp(&(&right.file, right.call_start, right.id))
        });
    }
    Ok(ProjectPlanning {
        projects: plan,
        decisions: decisions.into_values().collect(),
        occurrences_avoided_by_tooling_filter,
        occurrences_using_tooling_fallback,
    })
}

fn apply_repository_project_policy(
    conn: &Connection,
    ownership: &mut [FileOwnership],
    projects: &mut [ProjectSummary],
) -> Result<()> {
    let mut policies = HashMap::<String, String>::new();
    for project in projects.iter_mut() {
        let Some(policy) = crate::recon::project_policy(
            conn,
            &project.project_id,
            &project.membership_fingerprint,
            &project.config_fingerprint,
        )?
        else {
            continue;
        };
        project.purpose.clone_from(&policy.role);
        project
            .purpose_reasons
            .push(format!("repository-recon:{}", policy.classification_id));
        policies.insert(project.project_id.clone(), policy.role);
    }
    for file in ownership {
        let all = file
            .project_ids
            .iter()
            .chain(&file.excluded_project_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        if all.is_empty() {
            continue;
        }
        let mut selected = BTreeSet::new();
        let mut excluded = BTreeSet::new();
        for project in &all {
            match policies.get(project).map(String::as_str) {
                Some("runtime") => {
                    selected.insert(project.clone());
                }
                Some(role) if crate::recon::is_auxiliary(role) => {
                    excluded.insert(project.clone());
                }
                _ if file.project_ids.contains(project) => {
                    selected.insert(project.clone());
                }
                _ => {
                    excluded.insert(project.clone());
                }
            }
        }
        if selected.is_empty() {
            selected = all;
            excluded.clear();
            file.tooling_fallback = true;
        } else {
            file.tooling_fallback = false;
        }
        file.project_ids = selected.into_iter().collect();
        file.excluded_project_ids = excluded.into_iter().collect();
    }
    Ok(())
}

fn project_decision<'a>(
    decisions: &'a mut BTreeMap<String, ProjectDecision>,
    summaries: &HashMap<&str, &ProjectSummary>,
    project_id: &str,
) -> &'a mut ProjectDecision {
    decisions.entry(project_id.to_string()).or_insert_with(|| {
        let summary = summaries.get(project_id);
        ProjectDecision {
            project_id: project_id.to_string(),
            purpose: summary.map_or_else(|| "inferred".into(), |summary| summary.purpose.clone()),
            purpose_reasons: summary
                .map_or_else(Vec::new, |summary| summary.purpose_reasons.clone()),
            selected_occurrences: 0,
            excluded_occurrences: 0,
            fallback_occurrences: 0,
        }
    })
}

fn current_dirty_source_files(
    conn: &Connection,
    dirty_files: &[String],
) -> Result<BTreeSet<String>> {
    if dirty_files.is_empty() {
        return Ok(BTreeSet::new());
    }
    let requested = dirty_files.iter().collect::<BTreeSet<_>>();
    let mut statement = conn.prepare(
        "SELECT path FROM files
         WHERE origin IN ('repository', 'workspace')
         ORDER BY path",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| requested.contains(path))
        .collect())
}

fn dirty_projects(
    ownership: &[FileOwnership],
    dirty_files: &BTreeSet<String>,
    include_inferred: bool,
) -> BTreeSet<String> {
    ownership
        .iter()
        .filter(|entry| dirty_files.contains(&entry.file))
        .flat_map(|entry| entry.project_ids.iter())
        .filter(|project| include_inferred || !is_inferred_project(project))
        .cloned()
        .collect()
}

fn projects_in_execution_order<'a>(
    projects: &'a BTreeMap<String, Vec<Occurrence>>,
    priority_projects: &BTreeSet<String>,
) -> Vec<(&'a String, &'a Vec<Occurrence>)> {
    let mut ordered = projects.iter().collect::<Vec<_>>();
    ordered.sort_by(|(left, _), (right, _)| {
        (!priority_projects.contains(*left))
            .cmp(&(!priority_projects.contains(*right)))
            .then_with(|| {
                is_inferred_project(left)
                    .cmp(&is_inferred_project(right))
                    .then_with(|| left.cmp(right))
            })
    });
    ordered
}

fn project_planning_fingerprints(
    projects: &BTreeMap<String, Vec<Occurrence>>,
    summaries: &[ProjectSummary],
    checker: &TypeScriptIdentity,
    protocol: u32,
) -> BTreeMap<String, String> {
    let summaries = summaries
        .iter()
        .map(|summary| (summary.project_id.as_str(), summary))
        .collect::<HashMap<_, _>>();
    projects
        .iter()
        .map(|(project_id, occurrences)| {
            let summary = summaries.get(project_id.as_str());
            let membership = summary.map_or_else(
                || {
                    occurrences
                        .iter()
                        .map(|occurrence| occurrence.file.as_str())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join("\0")
                },
                |summary| summary.membership_fingerprint.clone(),
            );
            let config = summary.map_or("", |summary| summary.config_fingerprint.as_str());
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"jscout-checker-project-plan-v1\0");
            for value in [
                project_id.as_str(),
                checker.version.as_str(),
                checker.source.as_str(),
                &protocol.to_string(),
                membership.as_str(),
                config,
            ] {
                hasher.update(value.as_bytes());
                hasher.update(b"\0");
            }
            (project_id.clone(), hasher.finalize().to_hex().to_string())
        })
        .collect()
}

fn plan_fingerprint(
    snapshot: &str,
    selected: &[Occurrence],
    projects: &BTreeMap<String, Vec<Occurrence>>,
    checker: &TypeScriptIdentity,
    protocol: u32,
    options: &EnrichOptions<'_>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-plan-v2\0");
    for value in [
        snapshot,
        &checker.version,
        &checker.source,
        &protocol.to_string(),
        if options.include_all {
            "all"
        } else {
            "default"
        },
        &options
            .max_occurrences
            .map_or_else(|| "uncapped".into(), |value| value.to_string()),
    ] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    for occurrence in selected {
        hasher.update(occurrence.id.to_string().as_bytes());
        hasher.update(b"\0");
    }
    for (project, occurrences) in projects {
        hasher.update(project.as_bytes());
        hasher.update(b"\0");
        for occurrence in occurrences {
            hasher.update(occurrence.id.to_string().as_bytes());
            hasher.update(b"\0");
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn reusable_active_batch<'a>(
    conn: &Connection,
    snapshot: &str,
    plan_fingerprint: &str,
    project_ids: impl Iterator<Item = &'a String>,
    input_freshness: &mut InputFreshnessCache,
) -> Result<Option<i64>> {
    let project_ids = project_ids.collect::<Vec<_>>();
    let Some(batch_id) = conn
        .query_row(
            "SELECT id FROM checker_enrichment_batches
             WHERE source_snapshot=?1 AND plan_fingerprint=?2 AND active=1",
            params![snapshot, plan_fingerprint],
            |row| row.get(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let complete: i64 = conn.query_row(
        "SELECT count(*) FROM checker_project_runs
         WHERE batch_id=?1 AND status='completed'
           AND completed_occurrences=selected_occurrences",
        [batch_id],
        |row| row.get(0),
    )?;
    if complete as usize != project_ids.len() {
        return Ok(None);
    }
    for project_id in project_ids {
        if !project_complete_and_fresh(conn, batch_id, project_id, input_freshness)? {
            return Ok(None);
        }
    }
    Ok(Some(batch_id))
}

fn deactivate_stale_active_batch(
    conn: &Connection,
    snapshot: &str,
    input_freshness: &mut InputFreshnessCache,
) -> Result<bool> {
    let Some(batch_id) = conn
        .query_row(
            "SELECT id FROM checker_enrichment_batches
             WHERE active=1 AND source_snapshot=?1",
            [snapshot],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let projects = conn
        .prepare(
            "SELECT project_id FROM checker_project_runs
             WHERE batch_id=?1 AND status='completed' ORDER BY project_id",
        )?
        .query_map([batch_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut stale = false;
    for project_id in projects {
        if !project_complete_and_fresh(conn, batch_id, &project_id, input_freshness)? {
            stale = true;
            break;
        }
    }
    if !stale {
        return Ok(false);
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = conn.execute(
        "UPDATE checker_enrichment_batches SET active=0
         WHERE id=?1 AND active=1 AND source_snapshot=?2",
        params![batch_id, snapshot],
    );
    match result {
        Ok(changed) => {
            conn.execute_batch("COMMIT")?;
            Ok(changed == 1)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error.into())
        }
    }
}

fn open_staging_batch(conn: &Connection, plan: &StagingPlan<'_>) -> Result<i64> {
    if !plan.force_new
        && let Some(batch_id) = conn
            .query_row(
                "SELECT id FROM checker_enrichment_batches
             WHERE source_snapshot=?1 AND plan_fingerprint=?2
               AND checker_version=?3 AND checker_source=?4
               AND sidecar_protocol=?5
               AND (active=0 OR EXISTS(
                 SELECT 1 FROM checker_project_runs run
                 WHERE run.batch_id=checker_enrichment_batches.id
                   AND run.status!='completed'
               ))
             ORDER BY active DESC, id DESC LIMIT 1",
                params![
                    plan.snapshot,
                    plan.plan_fingerprint,
                    plan.checker.version,
                    plan.checker.source,
                    plan.protocol
                ],
                |row| row.get(0),
            )
            .optional()?
    {
        return Ok(batch_id);
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<i64> {
        conn.execute("DELETE FROM checker_enrichment_batches WHERE active=0", [])?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, plan_fingerprint,
               selected_occurrences, total_projects, created_at, active
             ) VALUES(?1,?2,?3,'',?4,?5,?6,?7,datetime('now'),0)",
            params![
                plan.snapshot,
                plan.checker.version,
                plan.checker.source,
                plan.protocol,
                plan.plan_fingerprint,
                plan.selected_occurrences as i64,
                plan.projects.len() as i64
            ],
        )?;
        let batch_id = conn.last_insert_rowid();
        let mut insert = conn.prepare_cached(
            "INSERT INTO checker_project_runs(
               batch_id, project_id, status, selected_occurrences,
               completed_occurrences, planning_fingerprint, updated_at
             ) VALUES(?1,?2,'pending',?3,0,?4,datetime('now'))",
        )?;
        for (project_id, occurrences) in plan.projects {
            let fingerprint = plan
                .project_fingerprints
                .get(project_id)
                .with_context(|| format!("missing planning fingerprint for {project_id}"))?;
            insert.execute(params![
                batch_id,
                project_id,
                occurrences.len() as i64,
                fingerprint
            ])?;
        }
        Ok(batch_id)
    })();
    match result {
        Ok(batch_id) => {
            conn.execute_batch("COMMIT")?;
            Ok(batch_id)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Seed a new-snapshot staging batch from the previous active batch. Project
/// fingerprints authorize consideration; exact source occurrence identity and
/// current target fingerprints authorize each carried answer. If one owner of
/// an occurrence cannot carry, every owner is re-queried so cross-project
/// ambiguity can be recomputed from one coherent answer set.
fn carry_forward_projects(
    conn: &Connection,
    batch_id: i64,
    checker: &TypeScriptIdentity,
    protocol: u32,
    projects: &BTreeMap<String, Vec<Occurrence>>,
    project_fingerprints: &BTreeMap<String, String>,
    input_freshness: &mut InputFreshnessCache,
) -> Result<CarryOutcome> {
    let already_staged: i64 = conn.query_row(
        "SELECT count(*) FROM checker_occurrence_projects WHERE batch_id=?1",
        [batch_id],
        |row| row.get(0),
    )?;
    if already_staged != 0 {
        return Ok(CarryOutcome::default());
    }
    let Some(previous_batch) = conn
        .query_row(
            "SELECT id FROM checker_enrichment_batches
             WHERE active=1 AND id!=?1
               AND checker_version=?2 AND checker_source=?3
               AND sidecar_protocol=?4",
            params![batch_id, checker.version, checker.source, protocol],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(CarryOutcome::default());
    };

    let previous_projects = conn
        .prepare(
            "SELECT project_id, planning_fingerprint
             FROM checker_project_runs
             WHERE batch_id=?1 AND status='completed'",
        )?
        .query_map([previous_batch], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    // Preserve external checker-input watch coverage for fully carried
    // projects. Repository inputs are deliberately not copied: source edits
    // are handled fact-by-fact instead of invalidating the entire Program.
    let mut previous_external_inputs = BTreeMap::<String, Vec<ValidatedInput>>::new();
    let mut statement = conn.prepare(
        "SELECT project_id, input_kind, input_path, source_hash
         FROM checker_project_inputs
         WHERE batch_id=?1 AND input_kind='absolute'
         ORDER BY project_id, input_path",
    )?;
    for row in statement.query_map([previous_batch], |row| {
        Ok((
            row.get::<_, String>(0)?,
            ValidatedInput {
                kind: row.get(1)?,
                path: row.get(2)?,
                source_hash: row.get(3)?,
            },
        ))
    })? {
        let (project, input) = row?;
        previous_external_inputs
            .entry(project)
            .or_default()
            .push(input);
    }
    let external_inputs_fresh = projects
        .keys()
        .map(|project| {
            let fresh = previous_external_inputs
                .get(project)
                .into_iter()
                .flatten()
                .all(|input| input_freshness.matches(input));
            (project.clone(), fresh)
        })
        .collect::<BTreeMap<_, _>>();
    let mut projects_requiring_check = projects
        .keys()
        .filter(|project| {
            previous_projects.get(*project) != project_fingerprints.get(*project)
                || !external_inputs_fresh
                    .get(*project)
                    .copied()
                    .unwrap_or(false)
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut previous_coverage = BTreeMap::<(OccurrenceIdentity, String), CarryCoverage>::new();
    let mut statement = conn.prepare(
        "SELECT source_file, source_hash, call_start, call_end,
                receiver_start, receiver_end, property_start, property_end,
                project_id, checker_input_fingerprint, status
         FROM checker_occurrence_projects WHERE batch_id=?1
         ORDER BY source_file, call_start, project_id",
    )?;
    for row in statement.query_map([previous_batch], |row| {
        Ok((
            OccurrenceIdentity {
                file: row.get(0)?,
                hash: row.get(1)?,
                call_start: row.get(2)?,
                call_end: row.get(3)?,
                receiver_start: row.get(4)?,
                receiver_end: row.get(5)?,
                property_start: row.get(6)?,
                property_end: row.get(7)?,
            },
            row.get::<_, String>(8)?,
            CarryCoverage {
                input_fingerprint: row.get(9)?,
                status: row.get(10)?,
            },
        ))
    })? {
        let (identity, project, coverage) = row?;
        if previous_coverage
            .insert((identity, project), coverage)
            .is_some()
        {
            bail!("previous checker batch contains duplicate occurrence coverage");
        }
    }

    let mut previous_facts = BTreeMap::<(OccurrenceIdentity, String), Vec<CarryFact>>::new();
    let mut statement = conn.prepare(
        "SELECT source_file, source_hash, call_start, call_end,
                receiver_start, receiver_end, property_start, property_end,
                project_id, receiver_type, target_anchor, target_fingerprint,
                confidence, checker_input_fingerprint
         FROM checker_enrichments WHERE batch_id=?1
         ORDER BY source_file, call_start, project_id, target_anchor",
    )?;
    for row in statement.query_map([previous_batch], |row| {
        Ok((
            OccurrenceIdentity {
                file: row.get(0)?,
                hash: row.get(1)?,
                call_start: row.get(2)?,
                call_end: row.get(3)?,
                receiver_start: row.get(4)?,
                receiver_end: row.get(5)?,
                property_start: row.get(6)?,
                property_end: row.get(7)?,
            },
            row.get::<_, String>(8)?,
            CarryFact {
                receiver_type: row.get(9)?,
                target: Target {
                    anchor: row.get(10)?,
                    fingerprint: row.get(11)?,
                },
                confidence: row.get(12)?,
                input_fingerprint: row.get(13)?,
            },
        ))
    })? {
        let (identity, project, fact) = row?;
        previous_facts
            .entry((identity, project))
            .or_default()
            .push(fact);
    }

    let mut current_occurrences = BTreeMap::<i64, Occurrence>::new();
    let mut current_owners = BTreeMap::<i64, BTreeSet<String>>::new();
    for (project, occurrences) in projects {
        for occurrence in occurrences {
            current_occurrences
                .entry(occurrence.id)
                .or_insert_with(|| occurrence.clone());
            current_owners
                .entry(occurrence.id)
                .or_default()
                .insert(project.clone());
        }
    }

    let mut carried = Vec::<(Occurrence, String, CarryCoverage, Vec<CarryFact>)>::new();
    let mut occurrences_carried = 0;
    for (occurrence_id, occurrence) in current_occurrences {
        let identity = OccurrenceIdentity::from(&occurrence);
        let owners = current_owners
            .get(&occurrence_id)
            .context("checker project plan omitted occurrence owners")?;
        let mut answers = Vec::new();
        let mut can_carry = true;
        for project in owners {
            let current_fingerprint = project_fingerprints
                .get(project)
                .with_context(|| format!("missing planning fingerprint for {project}"))?;
            if previous_projects.get(project) != Some(current_fingerprint) {
                can_carry = false;
                break;
            }
            if !external_inputs_fresh.get(project).copied().unwrap_or(false) {
                can_carry = false;
                break;
            }
            let key = (identity.clone(), project.clone());
            let Some(coverage) = previous_coverage.get(&key) else {
                can_carry = false;
                break;
            };
            if !matches!(coverage.status.as_str(), "resolved" | "unknown") {
                can_carry = false;
                break;
            }
            let facts = previous_facts.get(&key).cloned().unwrap_or_default();
            if coverage.status == "unknown" && !facts.is_empty() {
                can_carry = false;
                break;
            }
            for fact in &facts {
                if !target_is_current(conn, &fact.target)? {
                    can_carry = false;
                    break;
                }
            }
            if !can_carry {
                break;
            }
            answers.push((project.clone(), coverage, facts));
        }
        if !can_carry {
            projects_requiring_check.extend(owners.iter().cloned());
            continue;
        }
        occurrences_carried += 1;
        for (project, coverage, facts) in answers {
            carried.push((
                occurrence.clone(),
                project,
                CarryCoverage {
                    input_fingerprint: coverage.input_fingerprint.clone(),
                    status: coverage.status.clone(),
                },
                facts,
            ));
        }
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<CarryOutcome> {
        let mut insert_coverage = conn.prepare_cached(
            "INSERT INTO checker_occurrence_projects(
               batch_id, member_call_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;
        let mut insert_fact = conn.prepare_cached(
            "INSERT INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
               'checker',?17
             )",
        )?;
        let mut insert_input = conn.prepare_cached(
            "INSERT INTO checker_project_inputs(
               batch_id, project_id, input_kind, input_path, source_hash
             ) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for (occurrence, project, coverage, facts) in &carried {
            insert_coverage.execute(params![
                batch_id,
                occurrence.id,
                occurrence.file,
                occurrence.hash,
                occurrence.call_start,
                occurrence.call_end,
                occurrence.receiver_start,
                occurrence.receiver_end,
                occurrence.property_start,
                occurrence.property_end,
                project,
                coverage.input_fingerprint,
                coverage.status,
            ])?;
            for fact in facts {
                insert_fact.execute(params![
                    batch_id,
                    occurrence.id,
                    occurrence.file_id,
                    occurrence.file,
                    occurrence.hash,
                    occurrence.call_start,
                    occurrence.call_end,
                    occurrence.receiver_start,
                    occurrence.receiver_end,
                    occurrence.property_start,
                    occurrence.property_end,
                    project,
                    fact.receiver_type,
                    fact.target.anchor,
                    fact.target.fingerprint,
                    fact.confidence,
                    fact.input_fingerprint,
                ])?;
            }
        }

        let mut projects_carried = 0;
        for (project, occurrences) in projects {
            let completed: i64 = conn.query_row(
                "SELECT count(*) FROM checker_occurrence_projects
                 WHERE batch_id=?1 AND project_id=?2 AND status!='failed'",
                params![batch_id, project],
                |row| row.get(0),
            )?;
            if completed as usize == occurrences.len() {
                let fingerprint = project_fingerprints
                    .get(project)
                    .with_context(|| format!("missing planning fingerprint for {project}"))?;
                conn.execute(
                    "UPDATE checker_project_runs
                     SET status='completed', completed_occurrences=?3,
                         checker_input_fingerprint=?4, execution_kind='carried',
                         error=NULL, updated_at=datetime('now')
                     WHERE batch_id=?1 AND project_id=?2",
                    params![batch_id, project, completed, fingerprint],
                )?;
                projects_carried += 1;
            } else {
                conn.execute(
                    "UPDATE checker_project_runs
                     SET completed_occurrences=?3,
                         execution_kind=CASE WHEN ?3>0 THEN 'mixed' ELSE 'checked' END,
                         updated_at=datetime('now')
                     WHERE batch_id=?1 AND project_id=?2",
                    params![batch_id, project, completed],
                )?;
            }
            if completed > 0 {
                for input in previous_external_inputs.get(project).into_iter().flatten() {
                    insert_input.execute(params![
                        batch_id,
                        project,
                        input.kind,
                        input.path,
                        input.source_hash,
                    ])?;
                }
            }
        }
        Ok(CarryOutcome {
            projects_carried,
            occurrences_carried,
            projects_requiring_check,
        })
    })();
    match result {
        Ok(outcome) => {
            conn.execute_batch("COMMIT")?;
            Ok(outcome)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn project_complete_and_fresh(
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
    input_freshness: &mut InputFreshnessCache,
) -> Result<bool> {
    let run = conn
        .query_row(
            "SELECT status, execution_kind FROM checker_project_runs
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((status, execution_kind)) = run else {
        return Ok(false);
    };
    if status != "completed" {
        return Ok(false);
    }
    let mut statement = conn.prepare(
        "SELECT input_kind, input_path, source_hash FROM checker_project_inputs
         WHERE batch_id=?1 AND project_id=?2 ORDER BY input_kind, input_path",
    )?;
    let inputs = statement
        .query_map(params![batch_id, project_id], |row| {
            Ok(ValidatedInput {
                kind: row.get(0)?,
                path: row.get(1)?,
                source_hash: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if inputs.is_empty() {
        return Ok(execution_kind == "carried");
    }
    Ok(inputs.iter().all(|input| input_freshness.matches(input)))
}

fn reset_project_staging(conn: &Connection, batch_id: i64, project_id: &str) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        conn.execute(
            "DELETE FROM checker_enrichments WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
        )?;
        conn.execute(
            "DELETE FROM checker_occurrence_projects WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
        )?;
        conn.execute(
            "DELETE FROM checker_project_inputs WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
        )?;
        conn.execute(
            "UPDATE checker_project_runs
             SET status='pending', completed_occurrences=0,
                 checker_input_fingerprint=NULL, execution_kind='checked',
                 error=NULL, updated_at=datetime('now')
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn completed_occurrences(
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
) -> Result<BTreeSet<i64>> {
    let mut statement = conn.prepare(
        "SELECT member_call_id FROM checker_occurrence_projects
         WHERE batch_id=?1 AND project_id=?2 AND status!='failed'
         ORDER BY member_call_id",
    )?;
    let rows = statement.query_map(params![batch_id, project_id], |row| row.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn mark_project_pending(conn: &Connection, batch_id: i64, project_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE checker_project_runs
         SET status='pending', error=NULL, updated_at=datetime('now')
         WHERE batch_id=?1 AND project_id=?2",
        params![batch_id, project_id],
    )?;
    Ok(())
}

fn mark_project_failed(
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
    occurrences: &[Occurrence],
    error: &str,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let mut insert = conn.prepare_cached(
            "INSERT OR IGNORE INTO checker_occurrence_projects(
               batch_id, member_call_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'','failed')",
        )?;
        for occurrence in occurrences {
            insert.execute(params![
                batch_id,
                occurrence.id,
                occurrence.file,
                occurrence.hash,
                occurrence.call_start,
                occurrence.call_end,
                occurrence.receiver_start,
                occurrence.receiver_end,
                occurrence.property_start,
                occurrence.property_end,
                project_id,
            ])?;
        }
        conn.execute(
            "UPDATE checker_project_runs
             SET status='failed', error=?3, updated_at=datetime('now')
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id, error],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "project execution updates caller-owned progress and resource counters in place"
)]
fn execute_project(
    root: &Path,
    options: &EnrichOptions<'_>,
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
    occurrences: &[Occurrence],
    expected_identity: &TypeScriptIdentity,
    request_batches: &mut usize,
    occurrences_queried: &mut usize,
    unknown_answers: &mut usize,
    unmapped_declarations: &mut usize,
    unmapped_declaration_contexts: &mut BTreeMap<String, usize>,
    peak_rss_bytes: &mut u64,
    peak_heap_bytes: &mut u64,
) -> Result<()> {
    const MAX_BATCH_ITEMS: usize = 128;
    const MAX_REQUEST_BYTES: usize = 1024 * 1024;

    let mut checker = super::launch(root, options.sidecar, None, options.node)?;
    checker
        .register_interrupts()
        .context("failed to install checker Ctrl-C handler")?;
    let mut project_fingerprint: Option<String> = None;
    let mut project_peak_rss_bytes = 0;
    let mut project_peak_heap_bytes = 0;
    let mut offset = 0;
    while offset < occurrences.len() {
        let mut end = (offset + MAX_BATCH_ITEMS).min(occurrences.len());
        while end > offset + 1 {
            let queries = occurrences[offset..end]
                .iter()
                .map(member_query)
                .collect::<Vec<_>>();
            let encoded = super::protocol::encode(
                "size-check",
                &super::protocol::Outbound::ResolveMembers {
                    project_id: project_id.to_string(),
                    queries,
                },
            )?;
            if encoded.len() <= MAX_REQUEST_BYTES {
                break;
            }
            end = offset + (end - offset) / 2;
        }
        let result = loop {
            let batch = &occurrences[offset..end];
            *request_batches += 1;
            match checker.resolve_members(
                project_id.to_string(),
                batch.iter().map(member_query).collect(),
                options.timeout,
            ) {
                Ok(result) => break result,
                Err(super::process::CheckerError::Remote { code, .. })
                    if code == "oversized_batch" && end > offset + 1 =>
                {
                    end = offset + (end - offset) / 2;
                }
                Err(error) => return Err(error.into()),
            }
        };
        let batch = &occurrences[offset..end];
        *occurrences_queried += batch.len();
        if result.project_id != project_id
            || result.typescript.version != expected_identity.version
            || result.typescript.source != expected_identity.source
            || result.results.len() != batch.len()
        {
            bail!("checker returned an inconsistent project batch for {project_id}");
        }
        if let Some(expected) = &project_fingerprint {
            if expected != &result.checker_input_fingerprint {
                bail!("checker input fingerprint changed during project {project_id}");
            }
        } else {
            project_fingerprint = Some(result.checker_input_fingerprint.clone());
        }
        project_peak_rss_bytes = project_peak_rss_bytes.max(result.resources.rss_bytes);
        project_peak_heap_bytes = project_peak_heap_bytes.max(result.resources.heap_used_bytes);
        *peak_rss_bytes = (*peak_rss_bytes).max(project_peak_rss_bytes);
        *peak_heap_bytes = (*peak_heap_bytes).max(project_peak_heap_bytes);
        let mut facts = Vec::new();
        let mut projects = Vec::new();
        for (occurrence, item) in batch.iter().zip(&result.results) {
            if item.indexed_hash != occurrence.hash || item.source_hash != occurrence.hash {
                bail!(
                    "checker returned a source identity that does not match indexed file {}",
                    occurrence.file
                );
            }
            if item.answer.project_id != project_id
                || item.answer.checker_input_fingerprint != result.checker_input_fingerprint
            {
                bail!("checker returned a mismatched answer for project {project_id}");
            }
            let outcome = map_occurrence(conn, occurrence, std::slice::from_ref(&item.answer))?;
            *unknown_answers += outcome.unknown_answers;
            *unmapped_declarations += outcome.unmapped_declarations;
            for (context, count) in outcome.unmapped_declaration_contexts {
                *unmapped_declaration_contexts.entry(context).or_default() += count;
            }
            facts.extend(outcome.facts);
            projects.extend(outcome.projects);
        }
        stage_batch(conn, batch_id, project_id, batch, &facts, &projects)?;
        offset = end;
        eprintln!(
            "checker enrichment: {project_id} staged {}/{} occurrences; rss={} MiB heap={}/{} MiB",
            offset,
            occurrences.len(),
            result.resources.rss_bytes / (1024 * 1024),
            result.resources.heap_used_bytes / (1024 * 1024),
            result.resources.heap_total_bytes / (1024 * 1024)
        );
    }
    let fingerprint =
        project_fingerprint.context("checker project completed without an input fingerprint")?;
    let validation =
        checker.validate_project(project_id.to_string(), fingerprint.clone(), options.timeout)?;
    if validation.project_id != project_id
        || validation.fingerprint.as_deref() != Some(fingerprint.as_str())
        || !validation.valid
    {
        bail!("checker inputs changed while project {project_id} was running");
    }
    complete_project(
        root,
        conn,
        batch_id,
        project_id,
        &fingerprint,
        &validation.inputs,
        project_peak_rss_bytes,
        project_peak_heap_bytes,
    )
}

fn member_query(occurrence: &Occurrence) -> MemberQuery {
    MemberQuery {
        file: occurrence.file.clone(),
        indexed_hash: occurrence.hash.clone(),
        call_start: occurrence.call_start,
        call_end: occurrence.call_end,
        receiver_start: occurrence.receiver_start,
        receiver_end: occurrence.receiver_end,
        property_start: occurrence.property_start,
        property_end: occurrence.property_end,
    }
}

fn stage_batch(
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
    occurrences: &[Occurrence],
    facts: &[PendingFact],
    projects: &[PendingProjectAnswer],
) -> Result<()> {
    if projects.len() != occurrences.len()
        || projects
            .iter()
            .zip(occurrences)
            .any(|(project, occurrence)| project.occurrence.id != occurrence.id)
    {
        bail!("checker batch did not cover every occurrence in request order");
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let mut insert = conn.prepare_cached(
            "INSERT OR REPLACE INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'checker',?17
             )",
        )?;
        for fact in facts {
            insert.execute(params![
                batch_id,
                fact.occurrence.id,
                fact.occurrence.file_id,
                fact.occurrence.file,
                fact.occurrence.hash,
                fact.occurrence.call_start,
                fact.occurrence.call_end,
                fact.occurrence.receiver_start,
                fact.occurrence.receiver_end,
                fact.occurrence.property_start,
                fact.occurrence.property_end,
                fact.project_id,
                fact.receiver_type,
                fact.target.anchor,
                fact.target.fingerprint,
                fact.confidence,
                fact.input_fingerprint,
            ])?;
        }
        let mut insert_project = conn.prepare_cached(
            "INSERT OR REPLACE INTO checker_occurrence_projects(
               batch_id, member_call_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;
        for project in projects {
            insert_project.execute(params![
                batch_id,
                project.occurrence.id,
                project.occurrence.file,
                project.occurrence.hash,
                project.occurrence.call_start,
                project.occurrence.call_end,
                project.occurrence.receiver_start,
                project.occurrence.receiver_end,
                project.occurrence.property_start,
                project.occurrence.property_end,
                project.project_id,
                project.input_fingerprint,
                project.status,
            ])?;
        }
        let completed: i64 = conn.query_row(
            "SELECT count(*) FROM checker_occurrence_projects
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
            |row| row.get(0),
        )?;
        conn.execute(
            "UPDATE checker_project_runs
             SET completed_occurrences=?3, updated_at=datetime('now')
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id, completed],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "completion transaction needs project identity, validated inputs, and resource peaks"
)]
fn complete_project(
    root: &Path,
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
    fingerprint: &str,
    inputs: &[super::protocol::CheckerInputFile],
    peak_rss_bytes: u64,
    peak_heap_bytes: u64,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        conn.execute(
            "DELETE FROM checker_project_inputs WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO checker_project_inputs(
               batch_id, project_id, input_kind, input_path, source_hash
             ) VALUES(?1,?2,?3,?4,?5)",
        )?;
        let mut seen = BTreeMap::<(String, String), String>::new();
        for input in inputs {
            let path = Path::new(&input.path);
            if !path.is_absolute() {
                bail!("checker returned a non-absolute project input path");
            }
            let (kind, stored_path) = match path.strip_prefix(root) {
                Ok(relative) => (
                    "repository".to_string(),
                    relative
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                ),
                Err(_) => ("absolute".to_string(), input.path.clone()),
            };
            let key = (kind, stored_path);
            if let Some(previous) = seen.insert(key, input.source_hash.clone())
                && previous != input.source_hash
            {
                bail!("checker returned conflicting hashes for one project input");
            }
        }
        for ((kind, path), hash) in seen {
            insert.execute(params![batch_id, project_id, kind, path, hash])?;
        }
        // A partially carried project is checked as one coherent TypeScript
        // Program. Rebind its retained rows to that exact input identity so
        // the completed run does not mix planning and checker fingerprints.
        conn.execute(
            "UPDATE checker_enrichments
             SET checker_input_fingerprint=?3
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id, fingerprint],
        )?;
        conn.execute(
            "UPDATE checker_occurrence_projects
             SET checker_input_fingerprint=?3
             WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id, fingerprint],
        )?;
        conn.execute(
            "UPDATE checker_project_runs
             SET status='completed', checker_input_fingerprint=?3,
                 execution_kind=CASE
                   WHEN execution_kind='mixed' THEN 'mixed' ELSE 'checked' END,
                 peak_rss_bytes=?4,
                 peak_heap_bytes=?5, error=NULL,
                 updated_at=datetime('now')
             WHERE batch_id=?1 AND project_id=?2
               AND completed_occurrences=selected_occurrences",
            params![
                batch_id,
                project_id,
                fingerprint,
                peak_rss_bytes as i64,
                peak_heap_bytes as i64
            ],
        )?;
        if conn.changes() != 1 {
            bail!("project {project_id} did not stage every selected occurrence");
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn activate_staging_batch(
    root: &Path,
    conn: &Connection,
    batch_id: i64,
    snapshot: &str,
    allow_failed_projects: bool,
) -> Result<usize> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<usize> {
        if crate::structural::current_snapshot(conn)? != snapshot {
            bail!("structural snapshot changed while enrichment was running; staged work retained");
        }
        let pending: i64 = conn.query_row(
            "SELECT count(*) FROM checker_project_runs
             WHERE batch_id=?1
               AND status='pending'",
            [batch_id],
            |row| row.get(0),
        )?;
        let failed: i64 = conn.query_row(
            "SELECT count(*) FROM checker_project_runs
             WHERE batch_id=?1 AND status='failed'",
            [batch_id],
            |row| row.get(0),
        )?;
        if pending != 0 || (!allow_failed_projects && failed != 0) {
            bail!("checker staging batch has {pending} pending and {failed} failed project(s)");
        }
        let completed: i64 = conn.query_row(
            "SELECT count(*) FROM checker_project_runs
             WHERE batch_id=?1 AND status='completed'",
            [batch_id],
            |row| row.get(0),
        )?;
        if completed == 0 {
            bail!(
                "checker staging batch has no completed projects; the previously active batch was retained"
            );
        }
        let staged_facts: i64 = conn.query_row(
            "SELECT count(*) FROM checker_enrichments WHERE batch_id=?1",
            [batch_id],
            |row| row.get(0),
        )?;
        let previous_active: i64 = conn.query_row(
            "SELECT count(*) FROM checker_enrichment_batches
             WHERE active=1 AND source_snapshot=?1 AND id!=?2",
            params![snapshot, batch_id],
            |row| row.get(0),
        )?;
        if staged_facts == 0 && previous_active != 0 {
            bail!(
                "checker staging batch has no targeted facts; the previously active batch was retained"
            );
        }
        let malformed_completed: i64 = conn.query_row(
            "SELECT count(*) FROM checker_project_runs
             WHERE batch_id=?1 AND status='completed'
               AND completed_occurrences!=selected_occurrences",
            [batch_id],
            |row| row.get(0),
        )?;
        if malformed_completed != 0 {
            bail!("checker staging batch has incomplete projects marked completed");
        }

        let mut input_statement = conn.prepare(
            "SELECT input_kind, input_path, source_hash
             FROM checker_project_inputs WHERE batch_id=?1
             ORDER BY input_kind, input_path",
        )?;
        let input_rows = input_statement
            .query_map([batch_id], |row| {
                Ok(ValidatedInput {
                    kind: row.get(0)?,
                    path: row.get(1)?,
                    source_hash: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut inputs = BTreeMap::<(String, String), String>::new();
        for input in input_rows {
            let key = (input.kind, input.path);
            if let Some(previous) = inputs.insert(key, input.source_hash.clone())
                && previous != input.source_hash
            {
                bail!("checker projects disagreed on one input hash; staged work retained");
            }
        }
        for ((kind, path), source_hash) in inputs {
            let input = ValidatedInput {
                kind,
                path,
                source_hash,
            };
            verify_source_hash(&input_path(root, &input), &input.source_hash, &input.path)?;
        }

        let mut targets = conn.prepare(
            "SELECT DISTINCT enrichment.target_anchor, enrichment.target_fingerprint
             FROM checker_enrichments enrichment WHERE enrichment.batch_id=?1",
        )?;
        for target in targets.query_map([batch_id], |row| {
            Ok(Target {
                anchor: row.get(0)?,
                fingerprint: row.get(1)?,
            })
        })? {
            recheck_target(conn, &target?)?;
        }

        // Cross-project ambiguity is known only after every owner has
        // completed. A different mapped target in any owner demotes every
        // surviving candidate for that occurrence.
        conn.execute(
            "UPDATE checker_enrichments
             SET confidence='possible'
             WHERE batch_id=?1 AND member_call_id IN (
               SELECT member_call_id FROM checker_enrichments
               WHERE batch_id=?1
               GROUP BY member_call_id
               HAVING count(DISTINCT target_anchor)>1
                  OR min(CASE confidence WHEN 'possible' THEN 0 ELSE 1 END)=0
             )",
            [batch_id],
        )?;

        let fingerprints = conn
            .prepare(
                "SELECT project_id, checker_input_fingerprint
                 FROM checker_project_runs
                 WHERE batch_id=?1 AND status='completed' ORDER BY project_id",
            )?
            .query_map([batch_id], |row| {
                Ok(InputValidation {
                    project_id: row.get(0)?,
                    file: String::new(),
                    fingerprint: row.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let input_fingerprint = batch_fingerprint(&fingerprints);
        conn.execute(
            "UPDATE checker_enrichment_batches
             SET active=0 WHERE active=1 AND id!=?1",
            [batch_id],
        )?;
        conn.execute(
            "UPDATE checker_enrichment_batches
             SET active=1, checker_input_fingerprint=?2 WHERE id=?1",
            params![batch_id, input_fingerprint],
        )?;
        if conn.changes() != 1 {
            bail!("checker staging batch disappeared before activation");
        }
        conn.execute(
            "DELETE FROM checker_enrichment_batches WHERE id!=?1 AND active=0",
            [batch_id],
        )?;
        let facts = conn.query_row(
            "SELECT count(*) FROM checker_enrichments WHERE batch_id=?1",
            [batch_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(facts as usize)
    })();
    match result {
        Ok(facts) => {
            conn.execute_batch("COMMIT")?;
            Ok(facts)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn load_unknown_projects(conn: &Connection, batch_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT project_id FROM checker_occurrence_projects
         WHERE batch_id=?1 AND status='unknown' ORDER BY project_id",
    )?;
    let rows = statement.query_map([batch_id], |row| row.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn verify_source_hash(path: &Path, expected: &str, display: &str) -> Result<()> {
    let bytes =
        fs::read(path).with_context(|| format!("could not read indexed source {display}"))?;
    let actual = blake3::hash(&bytes).to_hex().to_string();
    if actual != expected {
        bail!("indexed source changed since the structural snapshot: {display}");
    }
    Ok(())
}

/// Turn one occurrence's whole checker answer into pending facts.
///
/// Ambiguity is judged over the checker's WHOLE answer, not over the subset
/// that happened to map: a valid declaration jscout cannot anchor (an
/// interface member, a `.d.ts` overload, or a declaration outside the root
/// still means the resolved candidate set was ambiguous. Owning projects that
/// return `unknown` are retained as coverage metadata but do not contradict a
/// clean resolution from another project.
fn map_occurrence(
    conn: &Connection,
    occurrence: &Occurrence,
    answers: &[ProjectAnswer],
) -> Result<OccurrenceOutcome> {
    let mut outcome = OccurrenceOutcome::default();
    for answer in answers {
        let status = if answer.status == "resolved" {
            "resolved"
        } else {
            "unknown"
        };
        outcome.projects.push(PendingProjectAnswer {
            occurrence: occurrence.clone(),
            project_id: answer.project_id.clone(),
            input_fingerprint: answer.checker_input_fingerprint.clone(),
            status,
        });
        if answer.status != "resolved" {
            outcome.unknown_answers += 1;
            continue;
        }
        for declaration in &answer.declarations {
            // A declaration the sidecar attributes to lib/@types/vendored/
            // outside files can never anchor to an indexed symbol; skip the
            // lookup but keep counting it as unmapped so the per-occurrence
            // ambiguity predicate (`unmapped == 0`) is unchanged.
            if let Some(context) = declaration.context.as_deref()
                && context != "repo"
            {
                outcome.unmapped_declarations += 1;
                *outcome
                    .unmapped_declaration_contexts
                    .entry(context.to_string())
                    .or_default() += 1;
                continue;
            }
            match map_declaration(conn, &occurrence.member, declaration)? {
                Some(target) => outcome.facts.push(PendingFact {
                    occurrence: occurrence.clone(),
                    project_id: answer.project_id.clone(),
                    receiver_type: answer.receiver_type.clone(),
                    target,
                    confidence: String::new(),
                    input_fingerprint: answer.checker_input_fingerprint.clone(),
                }),
                None => {
                    outcome.unmapped_declarations += 1;
                    let key = if declaration.file.is_none() {
                        "outside"
                    } else {
                        "repo_unanchored"
                    };
                    *outcome
                        .unmapped_declaration_contexts
                        .entry(key.to_string())
                        .or_default() += 1;
                }
            }
        }
    }
    outcome.facts.sort_by(|left, right| {
        (&left.project_id, &left.target.anchor).cmp(&(&right.project_id, &right.target.anchor))
    });
    outcome.facts.dedup_by(|left, right| {
        left.project_id == right.project_id && left.target.anchor == right.target.anchor
    });
    let target_count = outcome
        .facts
        .iter()
        .map(|fact| fact.target.anchor.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let unambiguous = target_count == 1 && outcome.unmapped_declarations == 0;
    for fact in &mut outcome.facts {
        fact.confidence = if unambiguous { "likely" } else { "possible" }.into();
    }
    Ok(outcome)
}

fn map_declaration(
    conn: &Connection,
    member: &str,
    declaration: &DeclarationSite,
) -> Result<Option<Target>> {
    if declaration.outside_root {
        return Ok(None);
    }
    let Some(path) = declaration.file.as_deref() else {
        return Ok(None);
    };
    // Containment alone fabricates targets: any declaration nested inside an
    // indexed symbol's body would anchor to its container (an object-literal
    // method inside a function became a `likely` self-edge on the function).
    // The mapped symbol must BE the member's declaration: same name, and the
    // tightest span containing the checker's declaration node.
    let mut statement = conn.prepare(
        "SELECT node.node_key, file.hash, symbol.decl_start, symbol.decl_end
         FROM symbols symbol
         JOIN files file ON file.id=symbol.file_id
         JOIN graph_nodes node ON node.native_table='symbols' AND node.native_id=symbol.id
         WHERE file.path=?1
           AND symbol.name=?4
           AND symbol.decl_start<=?2 AND symbol.decl_end>=?3
         ORDER BY (symbol.decl_end-symbol.decl_start), node.node_key",
    )?;
    let rows = statement.query_map(
        params![path, declaration.start, declaration.end, member],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )?;
    let mut candidates = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let tightest = candidates[0].3 - candidates[0].2;
    candidates.retain(|candidate| candidate.3 - candidate.2 == tightest);
    if candidates.len() != 1 || candidates[0].1 != declaration.source_hash {
        return Ok(None);
    }
    let (anchor, source_hash, start, end) = candidates.remove(0);
    Ok(Some(Target {
        fingerprint: target_fingerprint(&anchor, &source_hash, start, end),
        anchor,
    }))
}

pub(crate) fn target_fingerprint(anchor: &str, source_hash: &str, start: i64, end: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-target-v1\0");
    for value in [anchor, source_hash, &start.to_string(), &end.to_string()] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn batch_fingerprint(entries: &[InputValidation]) -> String {
    let mut values = entries
        .iter()
        .map(|entry| {
            format!(
                "{}\0{}\0{}",
                entry.project_id, entry.file, entry.fingerprint
            )
        })
        .collect::<Vec<_>>();
    values.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-batch-v1\0");
    for value in values {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
fn publish(root: &Path, conn: &Connection, plan: &PublishPlan<'_>) -> Result<i64> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<i64> {
        let current = crate::structural::current_snapshot(conn)?;
        if current != plan.snapshot {
            bail!("structural snapshot changed while enrichment was running; published nothing");
        }
        for fact in plan.facts {
            recheck_occurrence(root, conn, &fact.occurrence)?;
            recheck_target(conn, &fact.target)?;
        }
        for project in plan.projects {
            recheck_occurrence(root, conn, &project.occurrence)?;
        }
        for input in plan.inputs {
            let path = input_path(root, input);
            verify_source_hash(&path, &input.source_hash, &input.path)?;
        }
        conn.execute(
            "UPDATE checker_enrichment_batches SET active=0 WHERE active=1",
            [],
        )?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1,?2,?3,?4,?5,datetime('now'),1)",
            params![
                plan.snapshot,
                plan.checker.version,
                plan.checker.source,
                plan.input_fingerprint,
                plan.protocol
            ],
        )?;
        let batch_id = conn.last_insert_rowid();
        // Nothing reads a retired batch: projection keys on `active=1` and the
        // exact source snapshot. Drop the superseded one and its facts so a
        // repeatedly enriched repository keeps one batch instead of one per
        // pass.
        conn.execute("DELETE FROM checker_enrichment_batches WHERE active=0", [])?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'checker',?17
             )",
        )?;
        for fact in plan.facts {
            insert.execute(params![
                batch_id,
                fact.occurrence.id,
                fact.occurrence.file_id,
                fact.occurrence.file,
                fact.occurrence.hash,
                fact.occurrence.call_start,
                fact.occurrence.call_end,
                fact.occurrence.receiver_start,
                fact.occurrence.receiver_end,
                fact.occurrence.property_start,
                fact.occurrence.property_end,
                fact.project_id,
                fact.receiver_type,
                fact.target.anchor,
                fact.target.fingerprint,
                fact.confidence,
                fact.input_fingerprint,
            ])?;
        }
        let mut insert_project = conn.prepare_cached(
            "INSERT INTO checker_occurrence_projects(
               batch_id, member_call_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;
        for project in plan.projects {
            insert_project.execute(params![
                batch_id,
                project.occurrence.id,
                project.occurrence.file,
                project.occurrence.hash,
                project.occurrence.call_start,
                project.occurrence.call_end,
                project.occurrence.receiver_start,
                project.occurrence.receiver_end,
                project.occurrence.property_start,
                project.occurrence.property_end,
                project.project_id,
                project.input_fingerprint,
                project.status,
            ])?;
        }
        Ok(batch_id)
    })();
    match result {
        Ok(batch_id) => {
            conn.execute_batch("COMMIT")?;
            Ok(batch_id)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn input_path(root: &Path, input: &ValidatedInput) -> PathBuf {
    if input.kind == "repository" {
        root.join(&input.path)
    } else {
        PathBuf::from(&input.path)
    }
}

#[cfg(test)]
fn recheck_occurrence(root: &Path, conn: &Connection, occurrence: &Occurrence) -> Result<()> {
    let current = conn
        .query_row(
            "SELECT file.id, file.hash, call.start, call.end,
                    call.receiver_start, call.receiver_end,
                    call.property_start, call.property_end
             FROM member_calls call JOIN files file ON file.id=call.file_id
             WHERE call.rowid=?1 AND file.path=?2",
            params![occurrence.id, occurrence.file],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    let expected = (
        occurrence.file_id,
        occurrence.hash.clone(),
        occurrence.call_start,
        occurrence.call_end,
        occurrence.receiver_start,
        occurrence.receiver_end,
        occurrence.property_start,
        occurrence.property_end,
    );
    if current != Some(expected) {
        bail!("member-call occurrence changed while enrichment was running; published nothing");
    }
    let path = crate::store::file_source_path(conn, root, occurrence.file_id)?;
    verify_source_hash(&path, &occurrence.hash, &occurrence.file)
}

fn recheck_target(conn: &Connection, target: &Target) -> Result<()> {
    if !target_is_current(conn, target)? {
        bail!("checker target changed while enrichment was running; published nothing");
    }
    Ok(())
}

fn target_is_current(conn: &Connection, target: &Target) -> Result<bool> {
    let current = conn
        .query_row(
            "SELECT file.hash, symbol.decl_start, symbol.decl_end
             FROM graph_nodes node
             JOIN symbols symbol ON node.native_table='symbols' AND node.native_id=symbol.id
             JOIN files file ON file.id=symbol.file_id
             WHERE node.node_key=?1",
            [&target.anchor],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((hash, start, end)) = current else {
        return Ok(false);
    };
    Ok(target_fingerprint(&target.anchor, &hash, start, end) == target.fingerprint)
}

#[cfg(test)]
mod tests;
