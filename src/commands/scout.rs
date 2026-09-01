use std::path::Path;

use anyhow::Result;

use crate::{config, llm, scouting};

use super::core::open_database_for_write;

fn launch_scout_gateway(
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    call_capacity: usize,
) -> Result<llm::process::ProcessGatewayPool> {
    llm::process::ProcessGatewayPool::launch(
        gateway_path,
        runtime,
        runtime
            .effective
            .llm
            .max_concurrency
            .min(call_capacity.max(1)),
    )
}

pub(super) fn cmd_scout_workflows(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::WorkflowScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::workflows(
        root,
        &conn,
        &options.seeds,
        options.depth,
        options.candidate_limit,
    )?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string(&scouting::dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let call_capacity = options.policy.max_calls.min(plan.items.len());
    let mut gateway = launch_scout_gateway(gateway_path, runtime, call_capacity)?;
    let batch = scouting::scout_workflow_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

pub(super) struct RepositoryScoutCommandOptions<'a> {
    pub(super) dry_run: bool,
    pub(super) warn_subjects: usize,
    pub(super) planning: scouting::repository::RepositoryPlanningOptions<'a>,
    pub(super) scout: scouting::repository::RepositoryScoutOptions,
}

pub(super) fn cmd_scout_repository(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    options: RepositoryScoutCommandOptions<'_>,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::repository::plan(root, &conn, &options.planning)?;
    let initial_subjects = plan.items.len();
    if initial_subjects > options.warn_subjects {
        eprintln!(
            "warning: repository scout discovered {initial_subjects} initial subjects (warning threshold {}); no subjects will be truncated",
            options.warn_subjects
        );
    }
    if options.dry_run {
        let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
        println!(
            "{}",
            serde_json::to_string(&scouting::repository::dry_run_report(
                &conn,
                &mut gateway,
                &plan,
                &options.scout,
            )?)?
        );
        return Ok(());
    }
    let mut gateway = launch_scout_gateway(gateway_path, runtime, options.scout.policy.max_calls)?;
    let batch = scouting::repository::execute(root, &conn, &mut gateway, &options.scout, plan)?;
    if let Some(subjects) = batch.subjects_considered
        && initial_subjects <= options.warn_subjects
        && subjects > options.warn_subjects
    {
        eprintln!(
            "warning: mixed-scope subdivision increased repository scouting to {subjects} subjects (warning threshold {}); no subjects were truncated",
            options.warn_subjects
        );
    }
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

pub(super) fn cmd_scout_summaries(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::SummaryScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string(&scouting::summary_dry_run_report(root, &conn, &options)?)?
        );
        return Ok(());
    }
    let mut gateway = launch_scout_gateway(gateway_path, runtime, options.policy.max_calls)?;
    let batch = scouting::scout_summaries(root, &conn, &mut gateway, &options)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

pub(super) fn cmd_scout_cards(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::CardScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::cards_with_selectors(
        root,
        &conn,
        &scouting::plan::CardSelectors {
            anchors: options.anchors.clone(),
            files: options.files.clone(),
            reconnaissance_subjects: options.reconnaissance_subjects.clone(),
        },
    )?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string(&scouting::card_dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let call_capacity = options.policy.max_calls.min(plan.items.len());
    let mut gateway = launch_scout_gateway(gateway_path, runtime, call_capacity)?;
    let batch = scouting::scout_card_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

pub(super) fn cmd_scout_concepts(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::ConceptScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::concepts(&conn, &options.terms)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string(&scouting::concept_dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let call_capacity = options.policy.max_calls.min(plan.items.len());
    let mut gateway = launch_scout_gateway(gateway_path, runtime, call_capacity)?;
    let batch = scouting::scout_concept_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

pub(super) fn cmd_scout_refresh(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    artifacts: &[i64],
    dry_run: bool,
    policy: llm::config::RequestPolicy,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let selection = scouting::refresh::select(&conn, artifacts)?;
    if dry_run {
        let plans = scouting::plan_refresh(root, &conn, &selection)?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "dry_run": true,
                "max_calls": policy.max_calls,
                "max_concurrency": policy.max_concurrency,
                "context_bytes": policy.context_bytes,
                "selection": selection.summary,
                "plans": plans,
            }))?
        );
        return Ok(());
    }
    if !selection.summary.skipped_fresh.is_empty() {
        println!(
            "skipped fresh artifacts: {:?}",
            selection.summary.skipped_fresh
        );
    }
    if !selection.summary.unsupported_legacy.is_empty() {
        println!(
            "cannot refresh pre-G5 artifacts without recorded configuration: {:?}",
            selection.summary.unsupported_legacy
        );
    }
    if selection.targets.is_empty() {
        println!("no stale or degraded generated workflows, cards, or summaries to refresh");
        return Ok(());
    }
    let call_capacity = policy.max_calls.min(selection.targets.len());
    let mut gateway = launch_scout_gateway(gateway_path, runtime, call_capacity)?;
    let batch = scouting::scout_refresh(root, &conn, &mut gateway, selection, policy)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

/// Failed subjects are printed AND fail the process: scripts and agents key
/// on exit status. Incomplete refusals and reported policy skips are designed
/// outcomes and exit zero.
fn scout_batch_exit(batch: &scouting::ScoutBatchReport) -> Result<()> {
    let failed = batch
        .reports
        .iter()
        .filter(|report| report.status == "failed")
        .count();
    if failed > 0 {
        anyhow::bail!(
            "{failed} of {} scouting subject(s) failed; see the report above",
            batch.reports.len()
        );
    }
    Ok(())
}

fn print_scout_batch(batch: &scouting::ScoutBatchReport) {
    if let Some(subjects) = batch.subjects_considered {
        println!("subjects considered: {subjects}");
    }
    for report in &batch.reports {
        println!(
            "run {} [{}]: {} ({} candidates, billing path {})",
            report.run_id, report.kind, report.status, report.candidate_count, report.billing_path
        );
        println!("  subject: {}", report.subject);
        if let Some(started) = &report.started {
            println!(
                "  model: {}:{} via {} (auth {})",
                started.provider, started.model, started.api, started.auth_source
            );
        }
        for (decision, count) in &report.decisions {
            println!("  {decision}: {count}");
        }
        if let Some(usage) = &report.usage {
            println!(
                "  usage: {} in / {} out / {} total tokens",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            );
        }
        if let Some(reason) = &report.incomplete_reason {
            println!("  incomplete: {reason}");
        }
        if let Some(failure) = &report.failure {
            println!("  failed: {failure}");
        }
        if let Some(artifact) = report.artifact_id {
            println!("  artifact: {artifact}");
        }
    }
    println!(
        "model calls: {}; reports: {}; failed subjects: {}; duplicate boundaries: {}; skipped by call budget: {}; over budget: {}; unresolvable: {}; unscoutable subjects: {}",
        batch.model_calls,
        batch.reports.len(),
        batch
            .reports
            .iter()
            .filter(|report| report.status == "failed")
            .count(),
        batch.duplicate_candidate_sets_skipped,
        batch.skipped_for_call_budget,
        batch.skipped_over_budget.len(),
        batch.skipped_unresolvable.len(),
        batch.skipped_unscoutable,
    );
    if batch.auto_limit_reached {
        println!("automatic selection reached its deterministic limit");
    }
    for (scope, coverage) in &batch.card_scope_coverage {
        println!(
            "  card scope {scope}: discovered {}; selected {}; omitted {}; reused {}; calls {}; completed {}; incomplete {}; failed {}; skipped call/context {}/{}",
            coverage.discovered,
            coverage.selected,
            coverage.omitted,
            coverage.reused,
            coverage.model_calls,
            coverage.completed,
            coverage.incomplete,
            coverage.failed,
            coverage.skipped_call_budget,
            coverage.skipped_context_budget,
        );
    }
    for skipped in &batch.skipped_over_budget {
        println!(
            "  skipped over budget: {}: {}",
            skipped.subject, skipped.reason
        );
    }
    for skipped in &batch.skipped_unresolvable {
        println!(
            "  skipped unresolvable: {}: {}",
            skipped.subject, skipped.reason
        );
    }
}
