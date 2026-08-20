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
    pub timeout: Duration,
    pub files: Vec<String>,
    pub packages: Vec<String>,
    pub members: Vec<String>,
    pub roles: Vec<String>,
    pub max_occurrences: Option<usize>,
    pub include_all: bool,
    pub dry_run: bool,
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
    pub configured_projects: usize,
    pub configuration_problems: usize,
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
            configured_projects: 0,
            configuration_problems: 0,
            occurrences_avoided_by_tooling_filter: 0,
            occurrences_using_tooling_fallback: 0,
            project_decisions: Vec::new(),
            dry_run: options.dry_run,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }
    super::process::begin_interrupt_scope().context("failed to install checker Ctrl-C handler")?;
    verify_selected_sources(&canonical_root, &conn, &selected)?;

    let mut planner = super::launch(&canonical_root, options.sidecar)?;
    planner
        .register_interrupts()
        .context("failed to install checker Ctrl-C handler")?;
    let mut ownership = planner.plan_members(
        selected
            .iter()
            .map(|occurrence| occurrence.file.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        options.timeout,
    )?;
    let protocol = planner.versions.protocol;
    drop(planner);
    apply_repository_project_policy(&conn, &mut ownership.files, &mut ownership.projects)?;
    let ProjectPlanning {
        projects: project_plan,
        decisions: project_decisions,
        occurrences_avoided_by_tooling_filter,
        occurrences_using_tooling_fallback,
    } = build_project_plan(&selected, &ownership.files, &ownership.projects)?;
    let plan_fingerprint = plan_fingerprint(
        &snapshot,
        &selected,
        &project_plan,
        &ownership.typescript,
        protocol,
        options,
    );

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
            configured_projects: ownership.projects.len(),
            configuration_problems: ownership.configuration_problems.len(),
            occurrences_avoided_by_tooling_filter,
            occurrences_using_tooling_fallback,
            project_decisions,
            dry_run: true,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }

    if let Some(batch_id) = reusable_active_batch(
        &canonical_root,
        &conn,
        &snapshot,
        &plan_fingerprint,
        project_plan.keys(),
    )? {
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
            configured_projects: ownership.projects.len(),
            configuration_problems: ownership.configuration_problems.len(),
            occurrences_avoided_by_tooling_filter,
            occurrences_using_tooling_fallback,
            project_decisions,
            dry_run: false,
            peak_rss_bytes: 0,
            peak_heap_bytes: 0,
        });
    }
    if deactivate_stale_active_batch(&canonical_root, &conn, &snapshot)? {
        crate::structural::rebuild_projection(&conn, &snapshot)?;
    }

    let batch_id = open_staging_batch(
        &conn,
        &snapshot,
        &plan_fingerprint,
        &ownership.typescript,
        protocol,
        selected.len(),
        &project_plan,
    )?;
    let mut occurrences_queried = 0;
    let mut occurrences_resumed = 0;
    let mut request_batches = 0;
    let mut unknown_answers = 0;
    let mut unmapped_declarations = 0;
    let mut unmapped_declaration_contexts = BTreeMap::<String, usize>::new();
    let mut peak_rss_bytes = 0;
    let mut peak_heap_bytes = 0;
    let mut failed_projects = Vec::<ProjectFailure>::new();

    for (project_index, (project_id, occurrences)) in project_plan.iter().enumerate() {
        if super::process::cancellation_pending() {
            bail!("checker enrichment interrupted; staged work retained");
        }
        if project_complete_and_fresh(&canonical_root, &conn, batch_id, project_id)? {
            occurrences_resumed += occurrences.len();
            continue;
        }
        let mut completed = completed_occurrences(&conn, batch_id, project_id)?;
        if completed.len() == occurrences.len() {
            reset_project_staging(&conn, batch_id, project_id)?;
            completed.clear();
        }
        let pending = occurrences
            .iter()
            .filter(|occurrence| !completed.contains(&occurrence.id))
            .cloned()
            .collect::<Vec<_>>();
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
        configured_projects: ownership.projects.len(),
        configuration_problems: ownership.configuration_problems.len(),
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
                    | "checker_crash"
                    | "checker_exit"
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
        // Process/transport and local storage failures may heal without a new
        // structural snapshot. Unknown local errors stay fail-closed.
        _ => true,
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

fn build_project_plan(
    selected: &[Occurrence],
    ownership: &[FileOwnership],
    projects: &[ProjectSummary],
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
        project.purpose = policy.role.clone();
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
    root: &Path,
    conn: &Connection,
    snapshot: &str,
    plan_fingerprint: &str,
    project_ids: impl Iterator<Item = &'a String>,
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
    let mut statement = conn.prepare(
        "SELECT input_kind, input_path, source_hash FROM checker_project_inputs
         WHERE batch_id=?1 ORDER BY input_kind, input_path",
    )?;
    let mut inputs = BTreeMap::<(String, String), String>::new();
    for row in statement.query_map([batch_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (kind, path, hash) = row?;
        if let Some(previous) = inputs.insert((kind, path), hash.clone())
            && previous != hash
        {
            return Ok(None);
        }
    }
    if inputs.is_empty() {
        return Ok(None);
    }
    for ((kind, path), source_hash) in inputs {
        let input = ValidatedInput {
            kind,
            path,
            source_hash,
        };
        let fresh = fs::read(input_path(root, &input))
            .map(|bytes| blake3::hash(&bytes).to_hex().as_str() == input.source_hash)
            .unwrap_or(false);
        if !fresh {
            return Ok(None);
        }
    }
    Ok(Some(batch_id))
}

fn deactivate_stale_active_batch(root: &Path, conn: &Connection, snapshot: &str) -> Result<bool> {
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
        if !project_complete_and_fresh(root, conn, batch_id, &project_id)? {
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

fn open_staging_batch(
    conn: &Connection,
    snapshot: &str,
    plan_fingerprint: &str,
    checker: &TypeScriptIdentity,
    protocol: u32,
    selected_occurrences: usize,
    projects: &BTreeMap<String, Vec<Occurrence>>,
) -> Result<i64> {
    if let Some(batch_id) = conn
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
                snapshot,
                plan_fingerprint,
                checker.version,
                checker.source,
                protocol
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
                snapshot,
                checker.version,
                checker.source,
                protocol,
                plan_fingerprint,
                selected_occurrences as i64,
                projects.len() as i64
            ],
        )?;
        let batch_id = conn.last_insert_rowid();
        let mut insert = conn.prepare_cached(
            "INSERT INTO checker_project_runs(
               batch_id, project_id, status, selected_occurrences,
               completed_occurrences, updated_at
             ) VALUES(?1,?2,'pending',?3,0,datetime('now'))",
        )?;
        for (project_id, occurrences) in projects {
            insert.execute(params![batch_id, project_id, occurrences.len() as i64])?;
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

fn project_complete_and_fresh(
    root: &Path,
    conn: &Connection,
    batch_id: i64,
    project_id: &str,
) -> Result<bool> {
    let status = conn
        .query_row(
            "SELECT status FROM checker_project_runs WHERE batch_id=?1 AND project_id=?2",
            params![batch_id, project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if status.as_deref() != Some("completed") {
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
        return Ok(false);
    }
    Ok(inputs.iter().all(|input| {
        fs::read(input_path(root, input))
            .map(|bytes| blake3::hash(&bytes).to_hex().as_str() == input.source_hash)
            .unwrap_or(false)
    }))
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
                 checker_input_fingerprint=NULL, error=NULL, updated_at=datetime('now')
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
               batch_id, member_call_id, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,'','failed')",
        )?;
        for occurrence in occurrences {
            insert.execute(params![batch_id, occurrence.id, project_id])?;
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

#[allow(clippy::too_many_arguments)]
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

    let mut checker = super::launch(root, options.sidecar)?;
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
               batch_id, member_call_id, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for project in projects {
            insert_project.execute(params![
                batch_id,
                project.occurrence.id,
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

#[allow(clippy::too_many_arguments)]
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
        conn.execute(
            "UPDATE checker_project_runs
             SET status='completed', checker_input_fingerprint=?3,
                 peak_rss_bytes=?4, peak_heap_bytes=?5, error=NULL,
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
               batch_id, member_call_id, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for project in plan.projects {
            insert_project.execute(params![
                batch_id,
                project.occurrence.id,
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
        bail!("checker target anchor disappeared while enrichment was running; published nothing");
    };
    if target_fingerprint(&target.anchor, &hash, start, end) != target.fingerprint {
        bail!("checker target changed while enrichment was running; published nothing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    fn planned_occurrence(
        id: i64,
        package: &str,
        file: &str,
        role: &str,
        boundary_rank: i64,
    ) -> Occurrence {
        Occurrence {
            id,
            file_id: id,
            file: file.into(),
            hash: "hash".into(),
            call_start: id,
            call_end: id + 10,
            receiver_start: id,
            receiver_end: id + 3,
            property_start: id + 4,
            property_end: id + 9,
            member: "run".into(),
            role: role.into(),
            package: package.into(),
            boundary_rank,
            deterministically_resolved: false,
            builtin_receiver: false,
            runtime_namesake: true,
        }
    }

    fn options() -> EnrichOptions<'static> {
        EnrichOptions {
            database: None,
            sidecar: None,
            timeout: Duration::from_secs(1),
            files: Vec::new(),
            packages: Vec::new(),
            members: Vec::new(),
            roles: Vec::new(),
            max_occurrences: None,
            include_all: false,
            dry_run: false,
        }
    }

    #[test]
    fn spread_order_covers_packages_and_files_before_repeating_a_prefix() {
        let occurrences = vec![
            planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0),
            planned_occurrence(2, "alpha", "alpha/a.ts", "production", 0),
            planned_occurrence(3, "alpha", "alpha/b.ts", "production", 0),
            planned_occurrence(4, "beta", "beta/a.ts", "production", 0),
            planned_occurrence(5, "beta", "beta/a.ts", "production", 0),
            planned_occurrence(6, "gamma", "gamma/a.ts", "production", 1),
        ];
        let ordered = spread_occurrences(occurrences);
        assert_eq!(
            ordered.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, 4, 3, 5, 2, 6]
        );
    }

    #[test]
    fn default_eligibility_excludes_nonproduction_and_exact_resolver_answers() {
        let mut resolved = planned_occurrence(2, "alpha", "alpha/b.ts", "production", 0);
        resolved.deterministically_resolved = true;
        let candidates = vec![
            planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0),
            resolved,
            planned_occurrence(3, "alpha", "alpha/test.ts", "test", 0),
        ];
        assert_eq!(
            select_eligible(candidates.clone(), &options())
                .0
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        let mut all = options();
        all.include_all = true;
        assert_eq!(select_eligible(candidates, &all).0.len(), 3);
    }

    #[test]
    fn builtin_receivers_are_deprioritized_while_foreign_namesakes_are_excluded() {
        let mut builtin = planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0);
        builtin.builtin_receiver = true;
        let mut foreign = planned_occurrence(2, "alpha", "alpha/b.ts", "production", 0);
        foreign.runtime_namesake = false;
        let keep = planned_occurrence(3, "alpha", "alpha/c.ts", "production", 0);
        let candidates = vec![builtin, foreign, keep];

        let (eligible, skips) = select_eligible(candidates.clone(), &options());
        assert_eq!(
            eligible.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(skips.builtin_receiver, 1);
        assert_eq!(skips.foreign_namesake, 1);
        assert_eq!(
            spread_occurrences(eligible)
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![3, 1]
        );

        let mut all = options();
        all.include_all = true;
        let (eligible, skips) = select_eligible(candidates, &all);
        assert_eq!(eligible.len(), 3);
        assert_eq!(skips.builtin_receiver, 1);
        assert_eq!(skips.foreign_namesake, 0);
    }

    #[test]
    fn tooling_ownership_is_excluded_with_a_non_tooling_owner_and_retained_as_fallback()
    -> Result<()> {
        let selected = vec![
            planned_occurrence(1, "root", "shared.ts", "production", 0),
            planned_occurrence(2, "root", "lint-only.ts", "production", 0),
        ];
        let ownership = vec![
            FileOwnership {
                file: "shared.ts".into(),
                project_ids: vec!["tsconfig.json".into()],
                excluded_project_ids: vec!["tsconfig.eslint.json".into()],
                tooling_fallback: false,
            },
            FileOwnership {
                file: "lint-only.ts".into(),
                project_ids: vec!["tsconfig.eslint.json".into()],
                excluded_project_ids: Vec::new(),
                tooling_fallback: true,
            },
        ];
        let projects = vec![
            ProjectSummary {
                project_id: "tsconfig.eslint.json".into(),
                file_count: 2,
                purpose: "tooling".into(),
                purpose_reasons: vec!["tooling-filename".into()],
                membership_fingerprint: String::new(),
                config_fingerprint: String::new(),
            },
            ProjectSummary {
                project_id: "tsconfig.json".into(),
                file_count: 1,
                purpose: "general".into(),
                purpose_reasons: Vec::new(),
                membership_fingerprint: String::new(),
                config_fingerprint: String::new(),
            },
        ];

        let planning = build_project_plan(&selected, &ownership, &projects)?;
        assert_eq!(planning.occurrences_avoided_by_tooling_filter, 1);
        assert_eq!(planning.occurrences_using_tooling_fallback, 1);
        assert_eq!(planning.projects["tsconfig.json"][0].file, "shared.ts");
        assert_eq!(
            planning.projects["tsconfig.eslint.json"][0].file,
            "lint-only.ts"
        );
        let tooling = planning
            .decisions
            .iter()
            .find(|decision| decision.project_id == "tsconfig.eslint.json")
            .expect("tooling decision");
        assert_eq!(tooling.selected_occurrences, 1);
        assert_eq!(tooling.excluded_occurrences, 1);
        assert_eq!(tooling.fallback_occurrences, 1);
        Ok(())
    }

    #[test]
    fn repository_project_policy_overrides_heuristics_but_preserves_sole_owner_fallback()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let conn = crate::store::open(directory.path())?;
        for (project_id, role, suffix) in [
            ("tsconfig.runtime.json", "runtime", "runtime"),
            ("tsconfig.tool.json", "tooling", "tooling"),
        ] {
            conn.execute(
                "INSERT INTO scout_runs(
                   scout_kind,status,gateway_protocol,provider,model,billing_path,
                   prompt_version,source_snapshot,input_fingerprint,request_hash,
                   config_json,started_at,completed_at
                 ) VALUES('repository','completed',1,'test','test','custom','test',
                          'snapshot',?1,?1,'{}','now','now')",
                [format!("project-policy-{suffix}")],
            )?;
            let selector = serde_json::to_string(&crate::recon::SubjectSelector::Project {
                config: project_id.into(),
                membership_fingerprint: format!("members-{suffix}"),
                config_fingerprint: format!("config-{suffix}"),
            })?;
            conn.execute(
                "INSERT INTO repository_classifications(
                   run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
                   explanation,citations_json,evidence_fingerprint,
                   classification_fingerprint,source_snapshot,created_at
                 ) VALUES(?1,?2,'project',?3,0,?4,'likely','test','[\"E001\"]',
                          ?2,?2,'snapshot','now')",
                rusqlite::params![
                    conn.last_insert_rowid(),
                    format!("project:{project_id}"),
                    selector,
                    role,
                ],
            )?;
        }
        let mut projects = vec![
            ProjectSummary {
                project_id: "tsconfig.runtime.json".into(),
                file_count: 1,
                purpose: "tooling".into(),
                purpose_reasons: vec!["filename".into()],
                membership_fingerprint: "members-runtime".into(),
                config_fingerprint: "config-runtime".into(),
            },
            ProjectSummary {
                project_id: "tsconfig.tool.json".into(),
                file_count: 2,
                purpose: "general".into(),
                purpose_reasons: Vec::new(),
                membership_fingerprint: "members-tooling".into(),
                config_fingerprint: "config-tooling".into(),
            },
        ];
        let mut ownership = vec![
            FileOwnership {
                file: "shared.ts".into(),
                project_ids: vec!["tsconfig.tool.json".into()],
                excluded_project_ids: vec!["tsconfig.runtime.json".into()],
                tooling_fallback: false,
            },
            FileOwnership {
                file: "tool-only.ts".into(),
                project_ids: vec!["tsconfig.tool.json".into()],
                excluded_project_ids: Vec::new(),
                tooling_fallback: false,
            },
        ];

        apply_repository_project_policy(&conn, &mut ownership, &mut projects)?;

        assert_eq!(ownership[0].project_ids, ["tsconfig.runtime.json"]);
        assert_eq!(ownership[0].excluded_project_ids, ["tsconfig.tool.json"]);
        assert!(!ownership[0].tooling_fallback);
        assert_eq!(ownership[1].project_ids, ["tsconfig.tool.json"]);
        assert!(ownership[1].excluded_project_ids.is_empty());
        assert!(ownership[1].tooling_fallback);
        assert_eq!(projects[0].purpose, "runtime");
        assert_eq!(projects[1].purpose, "tooling");
        assert!(projects.iter().all(|project| {
            project
                .purpose_reasons
                .iter()
                .any(|reason| reason.starts_with("repository-recon:"))
        }));

        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind,status,gateway_protocol,provider,model,billing_path,
               prompt_version,source_snapshot,input_fingerprint,request_hash,
               config_json,started_at,completed_at
             ) VALUES('repository','completed',1,'test','test','custom','test',
                      'snapshot','project-policy-neutral','neutral','{}','now','now')",
            [],
        )?;
        let selector = serde_json::to_string(&crate::recon::SubjectSelector::Project {
            config: "tsconfig.runtime.json".into(),
            membership_fingerprint: "members-runtime".into(),
            config_fingerprint: "config-runtime".into(),
        })?;
        conn.execute(
            "INSERT INTO repository_classifications(
               run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
               explanation,citations_json,evidence_fingerprint,
               classification_fingerprint,source_snapshot,created_at
             ) VALUES(?1,'project:tsconfig.runtime.json','project',?2,0,'unknown',
                      'possible','insufficient','[\"E001\"]','neutral','neutral',
                      'snapshot','now')",
            rusqlite::params![conn.last_insert_rowid(), selector],
        )?;
        let mut neutral_projects = vec![ProjectSummary {
            project_id: "tsconfig.runtime.json".into(),
            file_count: 1,
            purpose: "tooling".into(),
            purpose_reasons: vec!["filename".into()],
            membership_fingerprint: "members-runtime".into(),
            config_fingerprint: "config-runtime".into(),
        }];
        let mut neutral_ownership = vec![FileOwnership {
            file: "only.ts".into(),
            project_ids: Vec::new(),
            excluded_project_ids: vec!["tsconfig.runtime.json".into()],
            tooling_fallback: false,
        }];
        apply_repository_project_policy(&conn, &mut neutral_ownership, &mut neutral_projects)?;
        assert_eq!(neutral_projects[0].purpose, "tooling");
        assert_eq!(neutral_projects[0].purpose_reasons, ["filename"]);
        Ok(())
    }

    #[test]
    fn checker_cancellation_is_an_operation_interrupt_not_a_failed_project() {
        let canceled = anyhow::Error::new(super::super::process::CheckerError::Canceled(
            "requested".into(),
        ));
        assert!(canceled_checker_error(&canceled));

        let failed = anyhow::Error::new(super::super::process::CheckerError::Remote {
            code: "checker_crash".into(),
            message: "worker failed".into(),
        });
        assert!(!canceled_checker_error(&failed));
    }

    #[test]
    fn partial_failure_retry_policy_separates_project_state_from_transport_failure() {
        let mismatch = anyhow::Error::new(super::super::process::CheckerError::Remote {
            code: "project_mismatch".into(),
            message: "not an effective member".into(),
        });
        assert!(!project_failure_is_retryable(&mismatch));

        let crash = anyhow::Error::new(super::super::process::CheckerError::Remote {
            code: "checker_crash".into(),
            message: "worker failed".into(),
        });
        assert!(project_failure_is_retryable(&crash));

        let exhausted = anyhow::Error::new(super::super::process::CheckerError::Remote {
            code: "EMFILE".into(),
            message: "too many open files".into(),
        });
        assert!(project_failure_is_retryable(&exhausted));

        let timeout = anyhow::Error::new(super::super::process::CheckerError::Timeout(
            Duration::from_secs(1),
        ));
        assert!(project_failure_is_retryable(&timeout));

        let terminal_partial = anyhow::Error::new(PartialEnrichmentError {
            batch_id: 1,
            facts_published: 10,
            failures: vec![ProjectFailure {
                project_id: "tsconfig.json".into(),
                retryable: false,
            }],
        });
        assert!(is_terminal_partial_failure(&terminal_partial));

        let retryable_partial = anyhow::Error::new(PartialEnrichmentError {
            batch_id: 2,
            facts_published: 10,
            failures: vec![ProjectFailure {
                project_id: "tsconfig.json".into(),
                retryable: true,
            }],
        });
        assert!(!is_terminal_partial_failure(&retryable_partial));
    }

    #[test]
    fn builtin_receiver_and_runtime_namesake_flags_come_from_the_index() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("src"))?;
        fs::create_dir_all(repo.path().join("tests"))?;
        fs::write(
            repo.path().join("src/app.ts"),
            "import path from \"path\";\n\
             import { PathShim } from \"./path-shim\";\n\
             import { helper } from \"./helper\";\n\
             export class Service { run(): void {} }\n\
             export class Loader { ensureUserland(): void {} }\n\
             export function work(service: Service): void {\n\
               console.log(\"x\");\n\
               path.join(\"a\", \"b\");\n\
               service.run();\n\
               helper.probe();\n\
             }\n\
             export function ambient(): void { console.send(); }\n\
             export function shadowedImport(path: PathShim): void { path.join(); }\n\
             export function route(module: Loader): void {\n\
               module.ensureUserland();\n\
             }\n\
             export function hot(): void {\n\
               module.reload();\n\
             }\n",
        )?;
        fs::write(
            repo.path().join("src/globals.ts"),
            "class RepoConsole { send(): void {} }\n\
             declare const console: RepoConsole;\n",
        )?;
        fs::write(
            repo.path().join("src/path-shim.ts"),
            "export class PathShim { join(): void {} }\n\
             export default new PathShim();\n",
        )?;
        fs::write(
            repo.path().join("tsconfig.json"),
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"path\":[\"./src/path-shim.ts\"]}},\"include\":[\"src/**/*.ts\"]}",
        )?;
        fs::write(
            repo.path().join("src/shadow.ts"),
            "export const console = { log(_message: string): void {} };\n\
             export function shadowed(): void { console.log(\"y\"); }\n",
        )?;
        fs::write(
            repo.path().join("src/helper.ts"),
            "export const helper = { probe(): void {} };\n",
        )?;
        // Test-role namesakes satisfy the raw same-name floor for every
        // member below without making any of them runtime-anchorable.
        fs::write(
            repo.path().join("tests/namesakes.test.ts"),
            "export function log(): void {}\n\
             export function join(): void {}\n\
             export function probe(): void {}\n\
             export function reload(): void {}\n",
        )?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;

        let calls = load_occurrences(&conn)?;
        let flags = |file: &str, member: &str| {
            let occurrence = calls
                .iter()
                .find(|occurrence| occurrence.file == file && occurrence.member == member)
                .unwrap_or_else(|| panic!("occurrence {file}:{member} discovered"));
            (occurrence.builtin_receiver, occurrence.runtime_namesake)
        };
        // Bare global receiver, unbound in the scope tree.
        assert_eq!(flags("src/app.ts", "log"), (true, false));
        // Request spelling alone labels a paths-mapped import and a lexically
        // shadowed import as builtin-looking. This is advisory only: both have
        // an indexed runtime namesake and must remain eligible.
        let joins = calls
            .iter()
            .filter(|occurrence| occurrence.file == "src/app.ts" && occurrence.member == "join")
            .collect::<Vec<_>>();
        assert_eq!(joins.len(), 2);
        assert!(joins.iter().all(|occurrence| occurrence.builtin_receiver));
        assert!(joins.iter().all(|occurrence| occurrence.runtime_namesake));
        // A project-wide ambient global is also unbound in the per-file Oxc
        // scope even though TypeScript can resolve its repository declaration.
        assert_eq!(flags("src/app.ts", "send"), (true, true));
        // Ordinary local receiver with a production-class namesake.
        assert_eq!(flags("src/app.ts", "run"), (false, true));
        // Namesake exists only in a test file.
        assert_eq!(flags("src/app.ts", "probe"), (false, false));
        // `module` as a function parameter is a binding, not the CommonJS
        // global: the gate must not fire (the Next.js app-route.ts case).
        assert_eq!(flags("src/app.ts", "ensureUserland"), (false, true));
        // `module` with no binding anywhere is the real global.
        assert_eq!(flags("src/app.ts", "reload"), (true, false));
        // A same-file symbol named after the global keeps the occurrence.
        assert!(!flags("src/shadow.ts", "log").0);

        let (eligible, skips) = select_eligible(calls, &options());
        let mut eligible_members = eligible
            .iter()
            .map(|occurrence| occurrence.member.as_str())
            .collect::<Vec<_>>();
        eligible_members.sort_unstable();
        assert_eq!(
            eligible_members,
            vec!["ensureUserland", "join", "join", "run", "send"]
        );
        assert_eq!(skips.builtin_receiver, 3);
        assert_eq!(skips.foreign_namesake, 4);
        Ok(())
    }

    #[test]
    fn empty_filtered_plan_is_a_successful_noop_without_launching_the_checker() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("src"))?;
        fs::create_dir_all(repo.path().join("tests"))?;
        fs::write(
            repo.path().join("src/app.ts"),
            "export function run(): void { console.log('x'); }\n",
        )?;
        fs::write(
            repo.path().join("tests/log.test.ts"),
            "export function log(): void {}\n",
        )?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        drop(conn);

        let report = enrich(repo.path(), &options())?;
        assert_eq!(report.occurrences_discovered, 1);
        assert_eq!(report.occurrences_eligible, 0);
        assert_eq!(report.occurrences_selected, 0);
        assert_eq!(report.occurrences_skipped_foreign_namesake, 1);
        assert_eq!(report.checker_source, "not-invoked");
        assert_eq!(report.batch_id, 0);
        Ok(())
    }

    #[test]
    fn non_repo_declaration_contexts_skip_mapping_but_stay_unmapped() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "export class CardTable { insert(): void {} }\n",
        )?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let calls = load_occurrences(&conn)?;
        assert!(calls.is_empty(), "fixture has no member calls");
        let occurrence = planned_occurrence(1, "(root)", "main.ts", "production", 0);

        let answer = ProjectAnswer {
            project_id: "tsconfig.json".into(),
            status: "resolved".into(),
            receiver_type: None,
            declarations: vec![
                DeclarationSite {
                    file: None,
                    outside_root: true,
                    start: 0,
                    end: 4,
                    source_hash: "lib".into(),
                    context: Some("lib".into()),
                },
                DeclarationSite {
                    file: Some("node_modules/@types/thing/index.d.ts".into()),
                    outside_root: false,
                    start: 0,
                    end: 4,
                    source_hash: "types".into(),
                    context: Some("types".into()),
                },
            ],
            checker_input_fingerprint: "inputs".into(),
        };
        let outcome = map_occurrence(&conn, &occurrence, std::slice::from_ref(&answer))?;
        assert!(outcome.facts.is_empty());
        assert_eq!(outcome.unmapped_declarations, 2);
        assert_eq!(
            outcome.unmapped_declaration_contexts,
            BTreeMap::from([("lib".to_string(), 1), ("types".to_string(), 1)])
        );

        // An old sidecar sends no context; an outside declaration is still
        // attributed rather than mislabelled as a repository anchoring gap.
        let legacy = ProjectAnswer {
            project_id: "tsconfig.json".into(),
            status: "resolved".into(),
            receiver_type: None,
            declarations: vec![DeclarationSite {
                file: None,
                outside_root: true,
                start: 0,
                end: 4,
                source_hash: "lib".into(),
                context: None,
            }],
            checker_input_fingerprint: "inputs".into(),
        };
        let outcome = map_occurrence(&conn, &occurrence, std::slice::from_ref(&legacy))?;
        assert!(outcome.facts.is_empty());
        assert_eq!(outcome.unmapped_declarations, 1);
        assert_eq!(
            outcome.unmapped_declaration_contexts,
            BTreeMap::from([("outside".to_string(), 1)])
        );
        Ok(())
    }

    #[test]
    fn namespace_member_resolved_by_the_structural_graph_is_not_requeried() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("library.ts"),
            "export function run(): void {}\n",
        )?;
        fs::write(
            repo.path().join("main.ts"),
            "import * as library from './library';\nlibrary.run();\n",
        )?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;

        let calls = load_occurrences(&conn)?;
        let call = calls
            .iter()
            .find(|occurrence| occurrence.member == "run")
            .expect("namespace member call");
        assert!(call.deterministically_resolved);
        let call_id = call.id;
        assert!(select_eligible(calls.clone(), &options()).0.is_empty());

        let mut all = options();
        all.include_all = true;
        assert_eq!(select_eligible(calls, &all).0.len(), 1);
        let bound_edges: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges
             WHERE confidence IN ('certain', 'likely')
               AND CAST(json_extract(detail_json, '$.memberCallId') AS INTEGER)=?1",
            [call_id],
            |row| row.get(0),
        )?;
        assert_eq!(bound_edges, 1);
        Ok(())
    }

    #[test]
    fn manual_default_keeps_all_one_hundred_fifty_thousand_eligible_occurrences() {
        let occurrences = (0..150_000)
            .map(|id| {
                planned_occurrence(
                    id,
                    &format!("package-{}", id % 64),
                    &format!("package-{}/file-{}.ts", id % 64, id % 1024),
                    "production",
                    id % 2,
                )
            })
            .collect::<Vec<_>>();
        let (eligible, _) = select_eligible(occurrences, &options());
        assert_eq!(eligible.len(), 150_000);
        assert_eq!(spread_occurrences(eligible).len(), 150_000);
    }

    #[test]
    fn staged_batches_survive_connection_reopen_for_resume() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      declare const card: CardTable; card.insert();\n";
        let (conn, _) = indexed(repo.path(), source)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let occurrence = occurrence(&conn)?;
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };
        let projects = BTreeMap::from([("tsconfig.json".into(), vec![occurrence.clone()])]);
        let batch_id = open_staging_batch(&conn, &snapshot, "plan", &identity, 2, 1, &projects)?;
        let answer = ProjectAnswer {
            project_id: "tsconfig.json".into(),
            status: "unknown".into(),
            receiver_type: None,
            declarations: Vec::new(),
            checker_input_fingerprint: "inputs".into(),
        };
        let outcome = map_occurrence(&conn, &occurrence, &[answer])?;
        stage_batch(
            &conn,
            batch_id,
            "tsconfig.json",
            std::slice::from_ref(&occurrence),
            &outcome.facts,
            &outcome.projects,
        )?;
        drop(conn);

        let reopened = crate::store::open(repo.path())?;
        assert_eq!(
            completed_occurrences(&reopened, batch_id, "tsconfig.json")?,
            BTreeSet::from([occurrence.id])
        );
        let active: i64 = reopened.query_row(
            "SELECT active FROM checker_enrichment_batches WHERE id=?1",
            [batch_id],
            |row| row.get(0),
        )?;
        assert_eq!(active, 0, "staged progress must remain non-public");
        Ok(())
    }

    #[test]
    fn failed_owner_activates_only_completed_projects_as_possible_and_remains_resumable()
    -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      declare const card: CardTable; card.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let occurrence = occurrence(&conn)?;
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };
        let projects = BTreeMap::from([
            ("tsconfig.good.json".into(), vec![occurrence.clone()]),
            ("tsconfig.failed.json".into(), vec![occurrence.clone()]),
        ]);
        let batch_id = open_staging_batch(&conn, &snapshot, "partial", &identity, 2, 1, &projects)?;

        let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
        let good = project_answer("tsconfig.good.json", vec![declaration]);
        let outcome = map_occurrence(&conn, &occurrence, &[good])?;
        stage_batch(
            &conn,
            batch_id,
            "tsconfig.good.json",
            std::slice::from_ref(&occurrence),
            &outcome.facts,
            &outcome.projects,
        )?;
        complete_project(
            repo.path(),
            &conn,
            batch_id,
            "tsconfig.good.json",
            "tsconfig.good.json-inputs",
            &[super::super::protocol::CheckerInputFile {
                path: repo.path().join("main.ts").to_string_lossy().into_owned(),
                source_hash: hash,
            }],
            1,
            1,
        )?;
        mark_project_failed(
            &conn,
            batch_id,
            "tsconfig.failed.json",
            std::slice::from_ref(&occurrence),
            "synthetic failure",
        )?;

        assert_eq!(
            activate_staging_batch(repo.path(), &conn, batch_id, &snapshot, true)?,
            1
        );
        crate::structural::rebuild_projection(&conn, &snapshot)?;
        let (confidence, detail): (String, String) = conn.query_row(
            "SELECT confidence, detail_json FROM resolved_edges
             WHERE provenance='checker' AND source_ref_id=?1",
            [occurrence.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(confidence, "possible");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&detail)?["failedProjects"],
            serde_json::json!(["tsconfig.failed.json"])
        );
        assert_eq!(
            open_staging_batch(&conn, &snapshot, "partial", &identity, 2, 1, &projects)?,
            batch_id,
            "the partial active batch must remain the resume target"
        );
        assert!(completed_occurrences(&conn, batch_id, "tsconfig.failed.json")?.is_empty());
        Ok(())
    }

    #[test]
    fn all_failed_batch_cannot_replace_a_healthy_active_batch() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      declare const card: CardTable; card.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let occurrence = occurrence(&conn)?;
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };

        let healthy_projects =
            BTreeMap::from([("tsconfig.healthy.json".into(), vec![occurrence.clone()])]);
        let healthy_batch = open_staging_batch(
            &conn,
            &snapshot,
            "healthy-plan",
            &identity,
            2,
            1,
            &healthy_projects,
        )?;
        let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
        let answer = project_answer("tsconfig.healthy.json", vec![declaration]);
        let outcome = map_occurrence(&conn, &occurrence, &[answer])?;
        stage_batch(
            &conn,
            healthy_batch,
            "tsconfig.healthy.json",
            std::slice::from_ref(&occurrence),
            &outcome.facts,
            &outcome.projects,
        )?;
        complete_project(
            repo.path(),
            &conn,
            healthy_batch,
            "tsconfig.healthy.json",
            "tsconfig.healthy.json-inputs",
            &[super::super::protocol::CheckerInputFile {
                path: repo.path().join("main.ts").to_string_lossy().into_owned(),
                source_hash: hash,
            }],
            1,
            1,
        )?;
        assert_eq!(
            activate_staging_batch(repo.path(), &conn, healthy_batch, &snapshot, false)?,
            1
        );
        crate::structural::rebuild_projection(&conn, &snapshot)?;

        let failed_projects =
            BTreeMap::from([("tsconfig.failed.json".into(), vec![occurrence.clone()])]);
        let failed_batch = open_staging_batch(
            &conn,
            &snapshot,
            "failed-plan",
            &identity,
            2,
            1,
            &failed_projects,
        )?;
        mark_project_failed(
            &conn,
            failed_batch,
            "tsconfig.failed.json",
            std::slice::from_ref(&occurrence),
            "synthetic failure",
        )?;
        let error = activate_staging_batch(repo.path(), &conn, failed_batch, &snapshot, true)
            .expect_err("an all-failed batch must not activate");
        assert!(error.to_string().contains("no completed projects"));

        let active_batch: i64 = conn.query_row(
            "SELECT id FROM checker_enrichment_batches WHERE active=1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(active_batch, healthy_batch);
        let live_edges: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(live_edges, 1);
        Ok(())
    }

    /// Index one file and hand back its connection plus its indexed hash.
    fn indexed(repo: &Path, source: &str) -> Result<(Connection, String)> {
        fs::write(repo.join("main.ts"), source)?;
        let conn = crate::store::open(repo)?;
        crate::indexer::index_repo(repo, &conn)?;
        let hash = conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
            row.get::<_, String>(0)
        })?;
        Ok((conn, hash))
    }

    /// The checker reports a declaration by its *name* span; `at` locates the
    /// same name the same way the sidecar's `declarationResult` does.
    fn declaration_at(source: &str, anchor: &str, name: &str, hash: &str) -> DeclarationSite {
        let start = source.find(anchor).expect("anchor present") as i64;
        DeclarationSite {
            file: Some("main.ts".into()),
            outside_root: false,
            start,
            end: start + name.len() as i64,
            source_hash: hash.to_string(),
            context: Some("repo".into()),
        }
    }

    fn answer(declarations: Vec<DeclarationSite>) -> ProjectAnswer {
        ProjectAnswer {
            project_id: "tsconfig.json".into(),
            status: "resolved".into(),
            receiver_type: Some("CardTable".into()),
            declarations,
            checker_input_fingerprint: "inputs".into(),
        }
    }

    fn project_answer(project_id: &str, declarations: Vec<DeclarationSite>) -> ProjectAnswer {
        ProjectAnswer {
            project_id: project_id.into(),
            checker_input_fingerprint: format!("{project_id}-inputs"),
            ..answer(declarations)
        }
    }

    fn occurrence(conn: &Connection) -> Result<Occurrence> {
        Ok(load_occurrences(conn)?
            .into_iter()
            .find(|occurrence| occurrence.member == "insert")
            .expect("indexed insert occurrence"))
    }

    /// Anchoring by span containment alone let any declaration nested inside an
    /// indexed symbol's body claim that symbol: an object-literal method inside
    /// a function published a fabricated `likely` self-edge on the function.
    /// Mapping must require the symbol to BE the member's declaration.
    #[test]
    fn an_object_literal_method_inside_a_function_maps_to_nothing_instead_of_its_container()
    -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export function caller(): void {\n\
                      \x20 const rows = { insert(): void {} };\n\
                      \x20 rows.insert();\n\
                      }\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let literal_method_start =
            source.rfind("insert(): void {}").expect("literal method") as i64;
        let declaration = DeclarationSite {
            file: Some("main.ts".into()),
            outside_root: false,
            start: literal_method_start,
            end: literal_method_start + "insert".len() as i64,
            source_hash: hash,
            context: Some("repo".into()),
        };

        // The containment trap is live: indexed symbols really do enclose this
        // declaration, so refusing it is a decision and not an accident.
        let containers: i64 = conn.query_row(
            "SELECT count(*) FROM symbols symbol JOIN files file ON file.id=symbol.file_id
             WHERE file.path='main.ts'
               AND symbol.decl_start<=?1 AND symbol.decl_end>=?2",
            params![declaration.start, declaration.end],
            |row| row.get(0),
        )?;
        assert!(containers > 0, "the enclosing symbol must exist");

        assert!(map_declaration(&conn, "insert", &declaration)?.is_none());
        let outcome = map_occurrence(&conn, &occurrence(&conn)?, &[answer(vec![declaration])])?;
        assert!(
            outcome.facts.is_empty(),
            "an unindexed declaration must publish no edge, least of all a self-edge"
        );
        assert_eq!(outcome.unmapped_declarations, 1);
        Ok(())
    }

    /// Ambiguity is judged over the checker's whole answer. Two valid targets
    /// where only one maps (jscout indexes no members for erased interfaces)
    /// must not collapse into a lone arbitrary `likely` edge.
    #[test]
    fn a_declaration_that_cannot_map_keeps_every_surviving_edge_possible() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export interface Vendor { insert(): void }\n\
                      declare const target: CardTable | Vendor;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
        assert!(
            map_declaration(&conn, "insert", &class_member)?.is_some(),
            "the class method is the mappable target"
        );
        assert!(
            map_declaration(&conn, "insert", &interface_member)?.is_none(),
            "the erased interface member has no indexed anchor"
        );
        let occurrence = occurrence(&conn)?;

        let ambiguous = map_occurrence(
            &conn,
            &occurrence,
            &[answer(vec![class_member.clone(), interface_member])],
        )?;
        assert_eq!(ambiguous.facts.len(), 1);
        assert_eq!(ambiguous.unmapped_declarations, 1);
        assert_eq!(
            ambiguous.facts[0].confidence, "possible",
            "a target the checker saw but jscout could not map still means ambiguity"
        );

        // Control: the same lone survivor is `likely` only when the checker
        // itself named exactly one target.
        let unambiguous = map_occurrence(&conn, &occurrence, &[answer(vec![class_member])])?;
        assert_eq!(unambiguous.facts.len(), 1);
        assert_eq!(unambiguous.unmapped_declarations, 0);
        assert_eq!(unambiguous.facts[0].confidence, "likely");
        Ok(())
    }

    #[test]
    fn later_project_cannot_upgrade_an_earlier_ambiguous_answer() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export interface Vendor { insert(): void }\n\
                      declare const target: CardTable | Vendor;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
        let occurrence = occurrence(&conn)?;

        for answers in [
            vec![
                project_answer(
                    "tsconfig.a.json",
                    vec![class_member.clone(), interface_member.clone()],
                ),
                project_answer("tsconfig.b.json", vec![class_member.clone()]),
            ],
            vec![
                project_answer("tsconfig.b.json", vec![class_member.clone()]),
                project_answer(
                    "tsconfig.a.json",
                    vec![class_member.clone(), interface_member.clone()],
                ),
            ],
        ] {
            let outcome = map_occurrence(&conn, &occurrence, &answers)?;
            assert_eq!(outcome.facts.len(), 2);
            assert_eq!(outcome.unmapped_declarations, 1);
            assert!(
                outcome
                    .facts
                    .iter()
                    .all(|fact| fact.confidence == "possible"),
                "confidence must be independent of project answer order"
            );
        }
        Ok(())
    }

    #[test]
    fn an_unknown_owning_project_is_visible_without_demoting_a_clean_resolution() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      declare const target: CardTable;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let occurrence = occurrence(&conn)?;
        let unknown = ProjectAnswer {
            project_id: "tsconfig.unknown.json".into(),
            status: "unknown".into(),
            receiver_type: None,
            declarations: Vec::new(),
            checker_input_fingerprint: "unknown-inputs".into(),
        };
        let outcome = map_occurrence(
            &conn,
            &occurrence,
            &[
                project_answer("tsconfig.resolved.json", vec![class_member]),
                unknown,
            ],
        )?;
        assert_eq!(outcome.unknown_answers, 1);
        assert_eq!(outcome.facts.len(), 1);
        assert_eq!(outcome.facts[0].confidence, "likely");
        assert_eq!(outcome.projects.len(), 2);
        assert!(outcome.projects.iter().any(|project| {
            project.project_id == "tsconfig.unknown.json" && project.status == "unknown"
        }));
        Ok(())
    }

    #[test]
    fn raced_snapshot_or_checker_input_publishes_nothing() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1,'5.9.3','bundled','previous',1,datetime('now'),1)",
            [&snapshot],
        )?;
        let previous_batch = conn.last_insert_rowid();
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };

        let stale_snapshot = publish(
            repo.path(),
            &conn,
            &PublishPlan {
                snapshot: "stale",
                checker: &identity,
                protocol: 1,
                input_fingerprint: "new",
                facts: &[],
                projects: &[],
                inputs: &[],
            },
        )
        .expect_err("snapshot race");
        assert!(stale_snapshot.to_string().contains("snapshot changed"));

        let ambient = repo.path().join("ambient.d.ts");
        fs::write(&ambient, "declare const version: 1;\n")?;
        let expected = blake3::hash(&fs::read(&ambient)?).to_hex().to_string();
        fs::write(&ambient, "declare const version: 2;\n")?;
        let raced_input = publish(
            repo.path(),
            &conn,
            &PublishPlan {
                snapshot: &snapshot,
                checker: &identity,
                protocol: 1,
                input_fingerprint: "new",
                facts: &[],
                projects: &[],
                inputs: &[ValidatedInput {
                    kind: "absolute".into(),
                    path: ambient.to_string_lossy().into_owned(),
                    source_hash: expected,
                }],
            },
        )
        .expect_err("input race");
        assert!(raced_input.to_string().contains("changed since"));

        let batches: (i64, i64) = conn.query_row(
            "SELECT count(*), sum(active) FROM checker_enrichment_batches",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let active: i64 = conn.query_row(
            "SELECT active FROM checker_enrichment_batches WHERE id=?1",
            [previous_batch],
            |row| row.get(0),
        )?;
        assert_eq!(batches, (1, 1));
        assert_eq!(active, 1);
        Ok(())
    }

    /// Nothing reads a superseded batch, so a repeatedly enriched repository
    /// must not accumulate one dead batch and its facts per pass.
    #[test]
    fn publishing_a_batch_prunes_the_one_it_supersedes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };
        let plan = |fingerprint: &'static str| PublishPlan {
            snapshot: &snapshot,
            checker: &identity,
            protocol: 1,
            input_fingerprint: fingerprint,
            facts: &[],
            projects: &[],
            inputs: &[],
        };

        let first = publish(repo.path(), &conn, &plan("one"))?;
        let second = publish(repo.path(), &conn, &plan("two"))?;
        let third = publish(repo.path(), &conn, &plan("three"))?;
        assert_ne!(first, second);

        let surviving: Vec<(i64, i64)> = conn
            .prepare("SELECT id, active FROM checker_enrichment_batches ORDER BY id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(surviving, vec![(third, 1)]);
        Ok(())
    }
}
