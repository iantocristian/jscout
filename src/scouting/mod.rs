//! Semantic scouting: generative runs over deterministic candidates.
//!
//! Every model call is recorded in the run ledger before it can publish
//! anything; failed, incomplete, and canceled runs stay attributable and
//! never create semantic artifacts. The model cannot add anchors: candidate
//! expansion is a Rust change, not model improvisation.

pub mod evidence;
pub mod ledger;
pub mod plan;
pub mod refresh;
pub mod workflow;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::llm::config::{ModelSpec, RequestPolicy};
use crate::llm::protocol::{
    ChatMessage, CompleteRequest, ModelCapabilities, PROTOCOL_VERSION, ProviderOptions,
    ProviderSummary, SubmitTool, Usage,
};
use crate::llm::{GatewayError, LlmGateway};
use crate::semantic::{self, WorkflowCandidateSet};
use crate::structural;

use evidence::EvidencePack;
use ledger::{ClassificationRow, RunClaim, RunOutcome, RunSpec};

const ORPHAN_SWEEP_MINUTES: i64 = 24 * 60;
const BASE_OUTPUT_TOKENS: u64 = 2_048;
const OUTPUT_TOKENS_PER_CANDIDATE: u64 = 512;

#[derive(Debug, Clone)]
pub struct WorkflowScoutOptions {
    pub seeds: Vec<String>,
    pub depth: usize,
    pub candidate_limit: usize,
    pub model: ModelSpec,
    pub reasoning: Option<String>,
    pub service_tier: Option<String>,
    pub policy: RequestPolicy,
    pub rebuild: bool,
    pub supersedes_artifact_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRunConfig {
    pub seeds: Vec<String>,
    pub depth: usize,
    pub candidate_limit: usize,
    pub service_tier: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowScoutReport {
    pub run_id: i64,
    pub status: String,
    /// Gateway-resolved provider/model/api/auth identity for completed calls.
    pub started: Option<crate::llm::StartedInfo>,
    pub artifact_id: Option<i64>,
    pub candidate_count: usize,
    pub decisions: BTreeMap<String, usize>,
    pub usage: Option<Usage>,
    pub billing_path: String,
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowBatchReport {
    pub reports: Vec<WorkflowScoutReport>,
    pub model_calls: usize,
    pub skipped_for_call_budget: usize,
    pub skipped_unscoutable: usize,
    pub duplicate_candidate_sets_skipped: usize,
    pub auto_seed_limit_reached: bool,
    pub skipped_over_budget: Vec<WorkflowBatchSkip>,
    pub skipped_unresolvable: Vec<WorkflowBatchSkip>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowBatchSkip {
    pub subject: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshPlanItem {
    pub artifact_id: i64,
    pub freshness: String,
    pub model: String,
    pub reasoning: Option<String>,
    pub workflow: plan::WorkflowPlan,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshPlanningReport {
    pub plans: Vec<RefreshPlanItem>,
    pub skipped_unresolvable: Vec<WorkflowBatchSkip>,
}

struct PreparedWorkflow {
    candidate_set: WorkflowCandidateSet,
    evidence: EvidencePack,
    request: CompleteRequest,
    spec: RunSpec,
}

#[derive(Default)]
struct PreparationCache {
    models: BTreeMap<String, ModelCapabilities>,
    providers: Option<ProviderSummary>,
}

impl PreparationCache {
    fn model(
        &mut self,
        gateway: &mut dyn LlmGateway,
        spec: &ModelSpec,
    ) -> Result<ModelCapabilities> {
        if let Some(capabilities) = self.models.get(&spec.spec) {
            return Ok(capabilities.clone());
        }
        let (providers, capabilities) = gateway.capabilities(Some(&spec.spec))?;
        self.providers.get_or_insert(providers);
        let Some(capabilities) = capabilities else {
            bail!(
                "model {} is not known to the gateway; run `jscout llm doctor --model {}`",
                spec.spec,
                spec.spec,
            );
        };
        self.models.insert(spec.spec.clone(), capabilities.clone());
        Ok(capabilities)
    }

    fn billing_path(&mut self, gateway: &mut dyn LlmGateway, model: &ModelSpec) -> Result<String> {
        if model.provider == "openai-codex" {
            return Ok("plan".into());
        }
        if self.providers.is_none() {
            self.providers = Some(gateway.capabilities(None)?.0);
        }
        Ok(
            if self
                .providers
                .as_ref()
                .is_some_and(|providers| providers.custom.iter().any(|id| id == &model.provider))
            {
                "custom".into()
            } else {
                "api".into()
            },
        )
    }
}

#[derive(Debug)]
struct ContextBudgetExceeded(String);

impl fmt::Display for ContextBudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ContextBudgetExceeded {}

/// One candidate-closed workflow scouting run for an explicit seed set.
#[cfg(test)]
pub fn scout_workflows(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &WorkflowScoutOptions,
) -> Result<WorkflowScoutReport> {
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;
    let mut plan = plan::workflows(
        root,
        conn,
        &options.seeds,
        options.depth,
        options.candidate_limit,
    )?;
    if plan.items.len() != 1 {
        bail!("single workflow scouting requires one explicit seed group");
    }
    let prepared = prepare_workflow(
        gateway,
        &mut PreparationCache::default(),
        plan.items.remove(0),
        options,
    )?;
    execute_prepared_workflow(root, conn, gateway, options, prepared, true)
}

/// Execute a precomputed explicit or automatic plan. Reusable completed runs
/// are checked before the call budget, so reuse never consumes `--max-calls`.
pub fn scout_workflow_plan(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &WorkflowScoutOptions,
    plan: plan::WorkflowPlan,
) -> Result<WorkflowBatchReport> {
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;
    let skipped_unscoutable = plan.skipped.len();
    let duplicate_candidate_sets_skipped = plan.duplicate_candidate_sets_skipped;
    let auto_seed_limit_reached = plan.auto_seed_limit_reached;
    let automatic = plan.mode == "automatic";
    let mut cache = PreparationCache::default();
    let mut prepared = Vec::new();
    let mut skipped_over_budget = Vec::new();
    for item in plan.items {
        let subject = item.seeds.join(", ");
        match prepare_workflow(gateway, &mut cache, item, options) {
            Ok(workflow) => prepared.push(workflow),
            Err(error) if automatic && error.downcast_ref::<ContextBudgetExceeded>().is_some() => {
                skipped_over_budget.push(WorkflowBatchSkip {
                    subject,
                    reason: error.to_string(),
                });
            }
            Err(error) => return Err(error),
        }
    }
    let mut reports = Vec::new();
    let mut model_calls = 0;
    let mut skipped = 0;
    for prepared in prepared {
        let reusable = !options.rebuild && ledger::reusable_run(conn, &prepared.spec)?.is_some();
        if !reusable && model_calls >= options.policy.max_calls {
            skipped += 1;
            continue;
        }
        let report = execute_prepared_workflow(
            root,
            conn,
            gateway,
            options,
            prepared,
            model_calls < options.policy.max_calls,
        )?;
        if report.status != "reused" {
            model_calls += 1;
        }
        reports.push(report);
    }
    Ok(WorkflowBatchReport {
        reports,
        model_calls,
        skipped_for_call_budget: skipped,
        skipped_unscoutable,
        duplicate_candidate_sets_skipped,
        auto_seed_limit_reached,
        skipped_over_budget,
        skipped_unresolvable: Vec::new(),
    })
}

/// Materialize exact replacement inputs without starting the gateway.
pub fn plan_refresh(
    root: &Path,
    conn: &Connection,
    selection: &refresh::RefreshSelection,
) -> Result<RefreshPlanningReport> {
    let mut plans = Vec::new();
    let mut skipped_unresolvable = Vec::new();
    for target in &selection.targets {
        match plan::workflows(
            root,
            conn,
            &target.config.seeds,
            target.config.depth,
            target.config.candidate_limit,
        ) {
            Ok(workflow) => plans.push(RefreshPlanItem {
                artifact_id: target.artifact_id,
                freshness: target.freshness.clone(),
                model: target.model.spec.clone(),
                reasoning: target.reasoning.clone(),
                workflow,
            }),
            Err(error) => skipped_unresolvable.push(WorkflowBatchSkip {
                subject: format!("artifact {}", target.artifact_id),
                reason: error.to_string(),
            }),
        }
    }
    Ok(RefreshPlanningReport {
        plans,
        skipped_unresolvable,
    })
}

/// Refresh stale/degraded generated workflows under one strict command-level
/// call budget while retaining each run's original model and configuration.
pub fn scout_refresh(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    selection: refresh::RefreshSelection,
    policy: RequestPolicy,
) -> Result<WorkflowBatchReport> {
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;
    let mut prepared = Vec::new();
    let mut skipped_unresolvable = Vec::new();
    let mut skipped_over_budget = Vec::new();
    let mut cache = PreparationCache::default();
    for target in selection.targets {
        let artifact_id = target.artifact_id;
        let workflow_plan = plan::workflows(
            root,
            conn,
            &target.config.seeds,
            target.config.depth,
            target.config.candidate_limit,
        );
        let mut workflow_plan = match workflow_plan {
            Ok(plan) => plan,
            Err(error) => {
                skipped_unresolvable.push(WorkflowBatchSkip {
                    subject: format!("artifact {artifact_id}"),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if workflow_plan.items.len() != 1 {
            skipped_unresolvable.push(WorkflowBatchSkip {
                subject: format!("artifact {artifact_id}"),
                reason: "did not reconstruct one seed group".into(),
            });
            continue;
        }
        let options = WorkflowScoutOptions {
            seeds: target.config.seeds,
            depth: target.config.depth,
            candidate_limit: target.config.candidate_limit,
            model: target.model,
            reasoning: target.reasoning,
            service_tier: target.config.service_tier,
            policy: policy.clone(),
            rebuild: false,
            supersedes_artifact_id: Some(artifact_id),
        };
        match prepare_workflow(gateway, &mut cache, workflow_plan.items.remove(0), &options) {
            Ok(workflow) => prepared.push((options, workflow)),
            Err(error) if error.downcast_ref::<ContextBudgetExceeded>().is_some() => {
                skipped_over_budget.push(WorkflowBatchSkip {
                    subject: format!("artifact {artifact_id}"),
                    reason: error.to_string(),
                });
            }
            Err(error) => return Err(error),
        }
    }

    let mut reports = Vec::new();
    let mut model_calls = 0;
    let mut skipped = 0;
    for (options, prepared) in prepared {
        let reusable = ledger::reusable_run(conn, &prepared.spec)?.is_some();
        if !reusable && model_calls >= policy.max_calls {
            skipped += 1;
            continue;
        }
        let report = execute_prepared_workflow(
            root,
            conn,
            gateway,
            &options,
            prepared,
            model_calls < policy.max_calls,
        )?;
        if report.status != "reused" {
            model_calls += 1;
        }
        reports.push(report);
    }
    Ok(WorkflowBatchReport {
        reports,
        model_calls,
        skipped_for_call_budget: skipped,
        skipped_unscoutable: 0,
        duplicate_candidate_sets_skipped: 0,
        auto_seed_limit_reached: false,
        skipped_over_budget,
        skipped_unresolvable,
    })
}

fn prepare_workflow(
    gateway: &mut dyn LlmGateway,
    cache: &mut PreparationCache,
    item: plan::WorkflowPlanItem,
    options: &WorkflowScoutOptions,
) -> Result<PreparedWorkflow> {
    let candidate_set = item.candidate_set;
    let evidence = item.evidence;
    let mut request = build_request(&candidate_set, &evidence, options)?;
    let capabilities = cache.model(gateway, &options.model)?;
    enforce_context_budget(
        &capabilities,
        &mut request,
        &evidence,
        candidate_set.candidates.len(),
        options,
    )?;

    let input_fingerprint = input_fingerprint(
        &candidate_set,
        &evidence,
        &request,
        options,
        capabilities.base_url.as_deref(),
    );
    let request_hash = blake3::hash(serde_json::to_string(&request)?.as_bytes())
        .to_hex()
        .to_string();
    let config_json = serde_json::to_string(&WorkflowRunConfig {
        seeds: candidate_set.seeds.clone(),
        depth: options.depth,
        candidate_limit: options.candidate_limit,
        service_tier: options.service_tier.clone(),
        base_url: capabilities.base_url.clone(),
    })?;
    let spec = RunSpec {
        scout_kind: "workflow".into(),
        gateway_protocol: PROTOCOL_VERSION,
        provider: options.model.provider.clone(),
        model: options.model.model_id.clone(),
        billing_path: cache.billing_path(gateway, &options.model)?,
        reasoning: options.reasoning.clone(),
        prompt_version: workflow::PROMPT_VERSION.into(),
        source_snapshot: candidate_set.snapshot.clone(),
        input_fingerprint: input_fingerprint.clone(),
        request_hash,
        config_json,
        supersedes_artifact_id: options.supersedes_artifact_id,
    };
    Ok(PreparedWorkflow {
        candidate_set,
        evidence,
        request,
        spec,
    })
}

fn execute_prepared_workflow(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &WorkflowScoutOptions,
    prepared: PreparedWorkflow,
    allow_new_call: bool,
) -> Result<WorkflowScoutReport> {
    let PreparedWorkflow {
        candidate_set,
        evidence,
        request,
        spec,
    } = prepared;
    if !allow_new_call && (options.rebuild || ledger::reusable_run(conn, &spec)?.is_none()) {
        bail!("workflow call budget exhausted before a non-reusable run");
    }
    let input_fingerprint = spec.input_fingerprint.clone();
    let (run_id, supersedes_artifact_id) = match ledger::claim_run(conn, &spec, options.rebuild)? {
        RunClaim::Reused(run_id) => {
            return reuse_report(conn, run_id, &candidate_set, &spec);
        }
        RunClaim::Claimed {
            run_id,
            supersedes_artifact_id,
        } => (run_id, supersedes_artifact_id),
    };

    let outcome = match gateway.complete(&request, options.policy.timeout) {
        Ok(outcome) => outcome,
        Err(error) => {
            let (status, code) = match &error {
                GatewayError::Canceled(_) => (RunOutcome::Canceled, error.code()),
                other => (RunOutcome::Failed, other.code()),
            };
            ledger::finish_run(conn, run_id, status, None, Some(&code))?;
            return Err(anyhow::Error::from(error)).context("gateway completion failed");
        }
    };
    // The ledger keeps the full outcome identity, not just token counts:
    // stop reason and the provider-reported response model expose drift.
    let usage_json = serde_json::to_string(&serde_json::json!({
        "usage": outcome.usage,
        "stop_reason": outcome.stop_reason,
        "response_model": outcome.response_model,
        "base_url": outcome.started.base_url,
    }))?;

    // The gateway's resolved billing path is authoritative; keep the ledger
    // honest if the provisional label differed.
    conn.execute(
        "UPDATE scout_runs SET billing_path=?2 WHERE id=?1",
        rusqlite::params![run_id, outcome.started.billing_path],
    )?;

    let submission: workflow::Submission =
        match serde_json::from_value(outcome.tool_call.arguments.clone()) {
            Ok(submission) if outcome.tool_call.name == workflow::SUBMIT_TOOL_NAME => submission,
            Ok(_) => {
                ledger::finish_run(
                    conn,
                    run_id,
                    RunOutcome::Failed,
                    Some(&usage_json),
                    Some("tool_contract"),
                )?;
                bail!(
                    "model called an unexpected tool `{}`",
                    outcome.tool_call.name
                );
            }
            Err(error) => {
                ledger::finish_run(
                    conn,
                    run_id,
                    RunOutcome::Failed,
                    Some(&usage_json),
                    Some("schema"),
                )?;
                return Err(error).context("submission does not match the output contract");
            }
        };

    let validated = match workflow::validate(&submission, &candidate_set.candidates, &evidence) {
        Ok(validated) => validated,
        Err(error) => {
            ledger::finish_run(
                conn,
                run_id,
                RunOutcome::Failed,
                Some(&usage_json),
                Some("validation"),
            )?;
            return Err(error).context("submission failed candidate-closed validation");
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
        return Ok(report(
            run_id,
            "incomplete",
            None,
            &candidate_set,
            &validated.classifications,
            Some(outcome.usage),
            Some(outcome.started.clone()),
            Some(reason.clone()),
        ));
    }

    // Semantic validation reuses the annotate rules: exact current anchors,
    // on-disk file-hash currency, span bounds, and snapshot currency against
    // the candidate snapshot.
    let annotate_input = workflow::annotate_input(
        &validated,
        candidate_set.snapshot.clone(),
        supersedes_artifact_id,
    )?;
    let validated_artifact = match semantic::validate_annotate_input(root, conn, &annotate_input) {
        Ok(parts) => parts,
        Err(error) => {
            // A snapshot or file change between evidence construction and
            // publication is not a model failure: record incomplete and
            // publish nothing against the old inputs.
            publish_terminal(
                conn,
                run_id,
                RunOutcome::Incomplete,
                &usage_json,
                Some("inputs_changed"),
                &validated.classifications,
            )?;
            return Err(error).context(
                "repository changed between evidence construction and publication; \
                 re-index and re-run",
            );
        }
    };
    let (snapshot, supports) = validated_artifact;

    // Publication: recheck input identity inside the transaction, then commit
    // run completion, classifications, artifact, and supports atomically.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let published = (|| -> Result<i64> {
        let current = structural::current_snapshot(conn)?;
        if current != candidate_set.snapshot {
            bail!("structural snapshot changed during publication");
        }
        for (file, entry) in &evidence.files {
            let indexed: String = conn
                .query_row("SELECT hash FROM files WHERE path=?1", [file], |row| {
                    row.get(0)
                })
                .with_context(|| format!("evidence file `{file}` disappeared from the index"))?;
            if indexed != entry.hash {
                bail!("evidence file `{file}` changed during publication");
            }
        }
        let artifact_id = semantic::persist_validated_artifact(
            conn,
            &annotate_input,
            &snapshot,
            &supports,
            &semantic::ArtifactProvenance {
                model: &options.model.spec,
                prompt_version: workflow::PROMPT_VERSION,
                scout_run_id: Some(run_id),
                input_fingerprint: Some(&input_fingerprint),
            },
        )?;
        ledger::record_classifications(conn, run_id, &validated.classifications)?;
        ledger::finish_run(conn, run_id, RunOutcome::Completed, Some(&usage_json), None)?;
        Ok(artifact_id)
    })();
    let artifact_id = match published {
        Ok(id) => {
            conn.execute_batch("COMMIT")?;
            id
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
            return Err(error).context("publication recheck failed; nothing was published");
        }
    };

    Ok(report(
        run_id,
        "completed",
        Some(artifact_id),
        &candidate_set,
        &validated.classifications,
        Some(outcome.usage),
        Some(outcome.started),
        None,
    ))
}

fn build_request(
    candidate_set: &WorkflowCandidateSet,
    evidence: &EvidencePack,
    options: &WorkflowScoutOptions,
) -> Result<CompleteRequest> {
    let anchors: Vec<String> = candidate_set
        .candidates
        .iter()
        .map(|candidate| candidate.anchor.clone())
        .collect();
    let system = "You are a code-comprehension analyst. You receive a deterministic \
                  candidate set and line-numbered source evidence for one repository \
                  workflow. Classify EVERY candidate exactly once as defining, \
                  supporting, or excluded, then submit through the tool. Rules: \
                  never invent anchors outside the candidate list; cite evidence line \
                  ranges from the numbered source for every included candidate; \
                  defining means part of the minimal cross-file skeleton of the \
                  workflow; supporting means genuinely involved but internal or \
                  peripheral; excluded means unrelated to this workflow. If a \
                  participant you consider essential is missing from the candidate \
                  list, set incomplete_reason instead of classifying around it."
        .to_string();
    let user = format!(
        "Seeds: {}\n\nName and describe the workflow that starts from the seeds, then \
         classify every candidate.\n\n{}",
        candidate_set.seeds.join(", "),
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
            name: workflow::SUBMIT_TOOL_NAME.into(),
            description: "Submit the workflow classification for every candidate".into(),
            parameters: workflow::submit_tool_schema(&anchors),
        },
        timeout_ms: Some(options.policy.timeout.as_millis() as u64),
        max_tokens: None,
        session_id: None,
        provider_options: options.service_tier.as_ref().map(|tier| ProviderOptions {
            service_tier: Some(tier.clone()),
        }),
    })
}

/// The 16 MiB line guard is a corruption check, not the context budget: the
/// pack must fit --context-bytes, and when the gateway reports the model's
/// context window the pack must plausibly fit it too.
fn enforce_context_budget(
    capabilities: &ModelCapabilities,
    request: &mut CompleteRequest,
    evidence: &EvidencePack,
    candidate_count: usize,
    options: &WorkflowScoutOptions,
) -> Result<()> {
    let desired_output = BASE_OUTPUT_TOKENS.saturating_add(
        OUTPUT_TOKENS_PER_CANDIDATE
            .saturating_mul(u64::try_from(candidate_count).unwrap_or(u64::MAX)),
    );
    let output_tokens = capabilities
        .max_tokens
        .map_or(desired_output, |maximum| desired_output.min(maximum));
    request.max_tokens = Some(output_tokens);

    let request_bytes = serde_json::to_string(request)?.len();
    if request_bytes > options.policy.context_bytes {
        return Err(ContextBudgetExceeded(format!(
            "serialized evidence pack is {request_bytes} bytes, over --context-bytes {}; \
             narrow the seeds/depth or raise the budget ({} evidence files)",
            options.policy.context_bytes,
            evidence.files.len(),
        ))
        .into());
    }
    if let Some(window) = capabilities.context_window {
        // pi-ai exposes no common tokenizer. UTF-8 byte length is a
        // conservative upper bound for byte-level provider tokenizers; an
        // average bytes/token divisor can undercount punctuation-heavy code.
        let input_token_ceiling = request_bytes as u64;
        if input_token_ceiling.saturating_add(output_tokens) > window {
            return Err(ContextBudgetExceeded(format!(
                "evidence pack requires at most {input_token_ceiling} input tokens plus \
                 {output_tokens} reserved output tokens, over the {} context window of {}; \
                 narrow the seeds or choose a larger-context model",
                window, options.model.spec,
            ))
            .into());
        }
    }
    Ok(())
}

fn input_fingerprint(
    candidate_set: &WorkflowCandidateSet,
    evidence: &EvidencePack,
    request: &CompleteRequest,
    options: &WorkflowScoutOptions,
    base_url: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-workflow-scout-input-v2\0");
    for part in [
        candidate_set.snapshot.as_str(),
        &candidate_set.seeds.join("\u{1}"),
        &candidate_set.fingerprint,
        &evidence.rendered,
        workflow::PROMPT_VERSION,
        &options.model.spec,
        options.reasoning.as_deref().unwrap_or(""),
        options.service_tier.as_deref().unwrap_or(""),
        base_url.unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
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

/// Terminal transition plus classifications in one transaction; used for
/// incomplete outcomes, which retain the model's decisions but publish no
/// artifact.
fn publish_terminal(
    conn: &Connection,
    run_id: i64,
    outcome: RunOutcome,
    usage_json: &str,
    error_code: Option<&str>,
    classifications: &[ClassificationRow],
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        ledger::record_classifications(conn, run_id, classifications)?;
        ledger::finish_run(conn, run_id, outcome, Some(usage_json), error_code)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn reuse_report(
    conn: &Connection,
    run_id: i64,
    candidate_set: &WorkflowCandidateSet,
    spec: &RunSpec,
) -> Result<WorkflowScoutReport> {
    let artifact_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM semantic_artifacts WHERE scout_run_id=?1",
            [run_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let mut decisions = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT decision, count(*) FROM scout_classifications WHERE run_id=?1 GROUP BY decision",
    )?;
    let rows = statement.query_map([run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    for row in rows {
        let (decision, count) = row?;
        decisions.insert(decision, count);
    }
    Ok(WorkflowScoutReport {
        run_id,
        status: "reused".into(),
        started: None,
        artifact_id,
        candidate_count: candidate_set.candidates.len(),
        decisions,
        usage: None,
        billing_path: spec.billing_path.clone(),
        incomplete_reason: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn report(
    run_id: i64,
    status: &str,
    artifact_id: Option<i64>,
    candidate_set: &WorkflowCandidateSet,
    classifications: &[ClassificationRow],
    usage: Option<Usage>,
    started: Option<crate::llm::StartedInfo>,
    incomplete_reason: Option<String>,
) -> WorkflowScoutReport {
    let mut decisions = BTreeMap::new();
    for row in classifications {
        *decisions.entry(row.decision.clone()).or_insert(0) += 1;
    }
    WorkflowScoutReport {
        run_id,
        status: status.into(),
        artifact_id,
        candidate_count: candidate_set.candidates.len(),
        decisions,
        usage,
        billing_path: started
            .as_ref()
            .map(|started| started.billing_path.clone())
            .unwrap_or_default(),
        started,
        incomplete_reason,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::time::Duration;

    use anyhow::Result;
    use serde_json::json;

    use super::{WorkflowScoutOptions, scout_refresh, scout_workflow_plan, scout_workflows};
    use crate::llm::config::{ModelSpec, RequestPolicy};
    use crate::llm::protocol::{
        CompleteRequest, ModelCapabilities, ProviderSummary, ToolCall, Usage,
    };
    use crate::llm::{CompletionOutcome, GatewayError, LlmGateway, StartedInfo};
    use crate::{indexer, store};

    struct FakeGateway {
        results: VecDeque<Result<CompletionOutcome, GatewayError>>,
        calls: usize,
        capability_calls: usize,
        last_max_tokens: Option<u64>,
        on_complete: Option<Box<dyn FnMut()>>,
    }

    impl FakeGateway {
        fn new(results: Vec<Result<CompletionOutcome, GatewayError>>) -> Self {
            Self {
                results: results.into(),
                calls: 0,
                capability_calls: 0,
                last_max_tokens: None,
                on_complete: None,
            }
        }
    }

    impl LlmGateway for FakeGateway {
        fn capabilities(
            &mut self,
            model: Option<&str>,
        ) -> Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError> {
            self.capability_calls += 1;
            Ok((
                ProviderSummary {
                    builtin: 1,
                    custom: Vec::new(),
                },
                model.map(|_| ModelCapabilities {
                    provider: "faux".into(),
                    model: "faux-model".into(),
                    api: "faux".into(),
                    base_url: Some("https://faux.example.test/v1".into()),
                    context_window: Some(200_000),
                    max_tokens: Some(32_000),
                    reasoning: true,
                    supports_service_tier: false,
                    supports_tools: true,
                }),
            ))
        }

        fn complete(
            &mut self,
            request: &CompleteRequest,
            _timeout: Duration,
        ) -> Result<CompletionOutcome, GatewayError> {
            self.calls += 1;
            self.last_max_tokens = request.max_tokens;
            if let Some(hook) = self.on_complete.as_mut() {
                hook();
            }
            self.results
                .pop_front()
                .expect("unexpected completion call")
        }
    }

    fn outcome(arguments: serde_json::Value) -> CompletionOutcome {
        CompletionOutcome {
            started: StartedInfo {
                provider: "faux".into(),
                model: "faux-model".into(),
                api: "faux".into(),
                base_url: Some("https://faux.example.test/v1".into()),
                billing_path: "api".into(),
                auth_source: "test".into(),
            },
            tool_call: ToolCall {
                name: super::workflow::SUBMIT_TOOL_NAME.into(),
                arguments,
            },
            stop_reason: "toolUse".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                reasoning_tokens: None,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 120,
                cost_total: 0.0,
            },
            response_model: Some("faux-model".into()),
        }
    }

    fn scout_options() -> WorkflowScoutOptions {
        WorkflowScoutOptions {
            seeds: vec!["flow.ts:start".into()],
            depth: 2,
            candidate_limit: crate::semantic::MAX_WORKFLOW_CANDIDATES,
            model: ModelSpec::parse("faux:faux-model").expect("model spec"),
            reasoning: None,
            service_tier: None,
            policy: RequestPolicy::new(30, 1, 240_000).expect("policy"),
            rebuild: false,
            supersedes_artifact_id: None,
        }
    }

    fn fixture(root: &Path) -> Result<rusqlite::Connection> {
        std::fs::write(
            root.join("flow.ts"),
            "export function finish() { return 1; }\n\
             export function start() { return finish(); }\n",
        )?;
        let conn = store::open(root)?;
        indexer::index_repo(root, &conn)?;
        Ok(conn)
    }

    fn candidate_anchors(conn: &rusqlite::Connection, root: &Path) -> Result<Vec<String>> {
        let set = crate::semantic::workflow_candidates(
            root,
            conn,
            &["flow.ts:start".into()],
            &crate::semantic::WorkflowCandidateOptions::default(),
        )?;
        Ok(set.candidates.iter().map(|c| c.anchor.clone()).collect())
    }

    fn full_submission(anchors: &[String]) -> serde_json::Value {
        let candidates: Vec<serde_json::Value> = anchors
            .iter()
            .map(|anchor| {
                if anchor.contains("::start@") {
                    json!({
                        "anchor": anchor,
                        "decision": "defining",
                        "role": "initiates the flow and calls the finisher",
                        "evidence": [{"start_line": 2, "end_line": 2}],
                    })
                } else {
                    json!({
                        "anchor": anchor,
                        "decision": "supporting",
                        "role": "terminal helper invoked by start",
                        "evidence": [{"start_line": 1, "end_line": 1}],
                    })
                }
            })
            .collect();
        json!({
            "name": "start-finish flow",
            "description": "start invokes finish to complete the flow.",
            "candidates": candidates,
            "incomplete_reason": null,
        })
    }

    fn defining_submission(set: &crate::semantic::WorkflowCandidateSet) -> serde_json::Value {
        let candidates = set
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                if index == 0 {
                    json!({
                        "anchor": candidate.anchor,
                        "decision": "defining",
                        "role": "deterministic entry boundary",
                        "evidence": [{
                            "start_line": candidate.evidence_start_line,
                            "end_line": candidate.evidence_end_line,
                        }],
                    })
                } else {
                    json!({
                        "anchor": candidate.anchor,
                        "decision": "excluded",
                        "reason": "outside the minimal boundary",
                    })
                }
            })
            .collect::<Vec<_>>();
        json!({
            "name": "deterministic entry flow",
            "description": "The entry boundary initiates this workflow.",
            "candidates": candidates,
            "incomplete_reason": null,
        })
    }

    #[test]
    fn publishes_a_candidate_closed_workflow_atomically() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        assert_eq!(anchors.len(), 2);

        let mut gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let report = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())?;
        assert_eq!(gateway.last_max_tokens, Some(3_072));
        assert_eq!(report.status, "completed");
        assert_eq!(report.candidate_count, 2);
        assert_eq!(report.decisions["defining"], 1);
        assert_eq!(report.decisions["supporting"], 1);
        let artifact_id = report.artifact_id.expect("published artifact");

        let (model, prompt_version, run_id, fingerprint): (String, String, i64, String) = conn
            .query_row(
                "SELECT model, prompt_version, scout_run_id, input_fingerprint
                 FROM semantic_artifacts WHERE id=?1",
                [artifact_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(model, "faux:faux-model");
        assert_eq!(prompt_version, "workflow-scout/v1");
        assert_eq!(run_id, report.run_id);
        assert!(!fingerprint.is_empty());

        let (status, usage_json, billing): (String, String, String) = conn.query_row(
            "SELECT status, usage_json, billing_path FROM scout_runs WHERE id=?1",
            [report.run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(status, "completed");
        assert!(usage_json.contains("\"total_tokens\":120"));
        assert!(usage_json.contains("\"base_url\":\"https://faux.example.test/v1\""));
        assert_eq!(billing, "api");
        let config: super::WorkflowRunConfig = serde_json::from_str(&conn.query_row(
            "SELECT config_json FROM scout_runs WHERE id=?1",
            [report.run_id],
            |row| row.get::<_, String>(0),
        )?)?;
        assert_eq!(
            config.seeds,
            vec![
                anchors
                    .iter()
                    .find(|anchor| anchor.contains("::start@"))
                    .expect("resolved start anchor")
                    .clone()
            ]
        );
        assert_eq!(config.depth, 2);

        let classifications: i64 = conn.query_row(
            "SELECT count(*) FROM scout_classifications WHERE run_id=?1",
            [report.run_id],
            |row| row.get(0),
        )?;
        assert_eq!(classifications, 2);
        let supports: i64 = conn.query_row(
            "SELECT count(*) FROM semantic_supports WHERE artifact_id=?1",
            [artifact_id],
            |row| row.get(0),
        )?;
        assert!(supports >= 4, "name, description, and participant supports");

        // Reuse: identical inputs return the same artifact without a call.
        let mut idle_gateway = FakeGateway::new(Vec::new());
        let reused = scout_workflows(repo.path(), &conn, &mut idle_gateway, &scout_options())?;
        assert_eq!(reused.status, "reused");
        assert_eq!(reused.artifact_id, Some(artifact_id));
        assert_eq!(idle_gateway.calls, 0);
        Ok(())
    }

    #[test]
    fn automatic_batch_spends_only_for_new_fingerprints() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("index.ts"),
            "export function first() { return 1; }\n\
             export function second() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let mut options = scout_options();
        options.seeds.clear();

        let first_plan = super::plan::workflows(repo.path(), &conn, &[], 2, 31)?;
        assert_eq!(first_plan.items.len(), 2);
        let first_submission = defining_submission(&first_plan.items[0].candidate_set);
        let mut first_gateway = FakeGateway::new(vec![Ok(outcome(first_submission))]);
        let first =
            scout_workflow_plan(repo.path(), &conn, &mut first_gateway, &options, first_plan)?;
        assert_eq!(first.model_calls, 1);
        assert_eq!(first.skipped_for_call_budget, 1);

        let second_plan = super::plan::workflows(repo.path(), &conn, &[], 2, 31)?;
        let second_submission = defining_submission(&second_plan.items[1].candidate_set);
        let mut second_gateway = FakeGateway::new(vec![Ok(outcome(second_submission))]);
        let second = scout_workflow_plan(
            repo.path(),
            &conn,
            &mut second_gateway,
            &options,
            second_plan,
        )?;
        assert_eq!(second.model_calls, 1);
        assert_eq!(second.skipped_for_call_budget, 0);
        assert_eq!(second.reports.len(), 2);
        assert_eq!(
            second
                .reports
                .iter()
                .filter(|report| report.status == "reused")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn automatic_batch_skips_one_oversized_boundary_and_continues() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("index.ts"),
            "export function huge() { return 1; }\n\
             export function tiny() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let mut plan = super::plan::workflows(repo.path(), &conn, &[], 2, 31)?;
        assert_eq!(plan.items.len(), 2);
        plan.items[0]
            .evidence
            .rendered
            .push_str(&"x".repeat(20_000));
        let submission = defining_submission(&plan.items[1].candidate_set);

        let mut options = scout_options();
        options.seeds.clear();
        options.policy = RequestPolicy::new(30, 2, 8_000)?;
        let mut gateway = FakeGateway::new(vec![Ok(outcome(submission))]);
        let batch = scout_workflow_plan(repo.path(), &conn, &mut gateway, &options, plan)?;

        assert_eq!(batch.skipped_over_budget.len(), 1);
        assert_eq!(batch.reports.len(), 1);
        assert_eq!(batch.model_calls, 1);
        assert_eq!(gateway.calls, 1);
        assert_eq!(
            gateway.capability_calls, 1,
            "model capabilities and provider summary are cached for the batch"
        );
        Ok(())
    }

    #[test]
    fn explicit_workflow_keeps_the_hard_context_budget_failure() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let mut options = scout_options();
        options.policy = RequestPolicy::new(30, 1, 1)?;
        let mut gateway = FakeGateway::new(Vec::new());
        let error = scout_workflows(repo.path(), &conn, &mut gateway, &options)
            .expect_err("explicit over-budget input must fail");
        assert!(error.to_string().contains("over --context-bytes"));
        assert_eq!(gateway.calls, 0);
        Ok(())
    }

    #[test]
    fn refresh_replays_recorded_inputs_and_publishes_an_immutable_successor() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        let mut first_gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let first = scout_workflows(repo.path(), &conn, &mut first_gateway, &scout_options())?;
        let first_id = first.artifact_id.expect("first artifact");

        std::fs::write(
            repo.path().join("flow.ts"),
            "export function finish() { return 2; }\n\
             export function start() { return finish(); }\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        let mut selection = super::refresh::select(&conn, &[first_id])?;
        assert_eq!(selection.targets.len(), 1);
        let mut unresolvable = selection.targets[0].clone();
        unresolvable.artifact_id = 999;
        unresolvable.config.seeds = vec!["deleted-workflow-anchor".into()];
        selection.targets.insert(0, unresolvable);
        let refreshed_anchors = candidate_anchors(&conn, repo.path())?;
        let mut gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&refreshed_anchors)))]);
        let batch = scout_refresh(
            repo.path(),
            &conn,
            &mut gateway,
            selection,
            RequestPolicy::new(30, 1, 240_000)?,
        )?;
        assert_eq!(batch.model_calls, 1);
        assert_eq!(batch.skipped_unresolvable.len(), 1);
        let successor = batch.reports[0].artifact_id.expect("refreshed artifact");
        assert_ne!(successor, first_id);
        let supersedes: i64 = conn.query_row(
            "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
            [successor],
            |row| row.get(0),
        )?;
        assert_eq!(supersedes, first_id);
        Ok(())
    }

    #[test]
    fn publishes_multiple_evidence_ranges_as_supports_for_one_participant() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        let mut submission = full_submission(&anchors);
        let start = submission["candidates"]
            .as_array_mut()
            .expect("candidate array")
            .iter_mut()
            .find(|candidate| {
                candidate["anchor"]
                    .as_str()
                    .is_some_and(|anchor| anchor.contains("::start@"))
            })
            .expect("start candidate");
        start["evidence"] = json!([
            {"start_line": 2, "end_line": 2},
            {"start_line": 1, "end_line": 2}
        ]);
        let start_anchor = start["anchor"].as_str().expect("start anchor").to_string();

        let mut gateway = FakeGateway::new(vec![Ok(outcome(submission))]);
        let report = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())?;
        let artifact_id = report.artifact_id.expect("published artifact");
        let body: String = conn.query_row(
            "SELECT body_json FROM semantic_artifacts WHERE id=?1",
            [artifact_id],
            |row| row.get(0),
        )?;
        let body: serde_json::Value = serde_json::from_str(&body)?;
        assert_eq!(
            body["participants"].as_array().expect("participants").len(),
            2
        );
        assert_eq!(
            body["participants"]
                .as_array()
                .expect("participants")
                .iter()
                .filter(|participant| participant["anchor"] == start_anchor)
                .count(),
            1
        );
        let role_supports: i64 = conn.query_row(
            "SELECT count(*) FROM semantic_supports
             WHERE artifact_id=?1 AND anchor_key=?2 AND claim_path LIKE '/participants/%/role'",
            rusqlite::params![artifact_id, start_anchor],
            |row| row.get(0),
        )?;
        assert_eq!(role_supports, 2);
        Ok(())
    }

    #[test]
    fn rebuild_supersedes_the_prior_artifact_in_default_search() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        let mut first_gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let first = scout_workflows(repo.path(), &conn, &mut first_gateway, &scout_options())?;

        let mut rebuild = scout_options();
        rebuild.rebuild = true;
        let mut second_gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let second = scout_workflows(repo.path(), &conn, &mut second_gateway, &rebuild)?;

        let first_id = first.artifact_id.expect("first artifact");
        let second_id = second.artifact_id.expect("successor artifact");
        let supersedes: i64 = conn.query_row(
            "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
            [second_id],
            |row| row.get(0),
        )?;
        assert_eq!(supersedes, first_id);
        let visible = crate::semantic::search(&conn, "start-finish", 10)?;
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, second_id);
        Ok(())
    }

    #[test]
    fn retry_after_failed_rebuild_still_supersedes_the_prior_artifact() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        let mut first_gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let first = scout_workflows(repo.path(), &conn, &mut first_gateway, &scout_options())?;

        let mut rebuild = scout_options();
        rebuild.rebuild = true;
        let mut failed_gateway = FakeGateway::new(vec![Err(GatewayError::Canceled("test".into()))]);
        scout_workflows(repo.path(), &conn, &mut failed_gateway, &rebuild)
            .expect_err("rebuild failure");

        let mut retry_gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let retry = scout_workflows(repo.path(), &conn, &mut retry_gateway, &scout_options())?;
        let first_id = first.artifact_id.expect("first artifact");
        let retry_id = retry.artifact_id.expect("retry artifact");
        let supersedes: i64 = conn.query_row(
            "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
            [retry_id],
            |row| row.get(0),
        )?;
        assert_eq!(supersedes, first_id);
        assert_eq!(crate::semantic::search(&conn, "start-finish", 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn closure_violations_fail_the_run_and_publish_nothing() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;

        // Omit one candidate: candidate closure must reject the submission.
        let partial = json!({
            "name": "partial flow",
            "description": "misses one candidate entirely.",
            "candidates": [{
                "anchor": anchors[0],
                "decision": "defining",
                "role": "only classified candidate",
                "evidence": [{"start_line": 1, "end_line": 1}],
            }],
            "incomplete_reason": null,
        });
        let mut gateway = FakeGateway::new(vec![Ok(outcome(partial))]);
        let error = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())
            .expect_err("closure violation");
        assert!(format!("{error:#}").contains("candidate-closed validation"));

        let (status, code): (String, String) = conn.query_row(
            "SELECT status, error_code FROM scout_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((status.as_str(), code.as_str()), ("failed", "validation"));
        let artifacts: i64 =
            conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
                row.get(0)
            })?;
        assert_eq!(artifacts, 0, "failed runs must publish nothing");
        Ok(())
    }

    #[test]
    fn model_incomplete_records_decisions_without_an_artifact() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        let submission = json!({
            "name": "",
            "description": "",
            "candidates": [{
                "anchor": anchors[0],
                "decision": "excluded",
                "reason": "cannot classify without the missing dispatcher",
            }],
            "incomplete_reason": "the queue dispatcher is outside the candidate set",
        });
        let mut gateway = FakeGateway::new(vec![Ok(outcome(submission))]);
        let report = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())?;
        assert_eq!(report.status, "incomplete");
        assert_eq!(report.artifact_id, None);
        assert!(report.incomplete_reason.is_some());

        let (status, code): (String, String) = conn.query_row(
            "SELECT status, error_code FROM scout_runs WHERE id=?1",
            [report.run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            (status.as_str(), code.as_str()),
            ("incomplete", "model_incomplete")
        );
        let artifacts: i64 =
            conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
                row.get(0)
            })?;
        assert_eq!(artifacts, 0);
        Ok(())
    }

    #[test]
    fn gateway_failures_and_cancellation_are_terminal_ledger_states() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;

        let mut gateway = FakeGateway::new(vec![Err(GatewayError::Canceled("ctrl-c".into()))]);
        let error = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())
            .expect_err("canceled");
        assert!(format!("{error:#}").contains("canceled"));
        let (status, code): (String, String) = conn.query_row(
            "SELECT status, error_code FROM scout_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((status.as_str(), code.as_str()), ("canceled", "canceled"));
        let artifacts: i64 =
            conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
                row.get(0)
            })?;
        assert_eq!(artifacts, 0);
        Ok(())
    }

    #[test]
    fn repository_changes_between_evidence_and_publication_publish_nothing() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;

        let root = repo.path().to_path_buf();
        let db_path = store::db_path(repo.path());
        let mut gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        gateway.on_complete = Some(Box::new(move || {
            // Simulate an index run landing while the model was thinking.
            std::fs::write(
                root.join("flow.ts"),
                "export function finish() { return 2; }\n\
                 export function start() { return finish(); }\n",
            )
            .expect("rewrite fixture");
            let racing = store::open_path(&db_path).expect("open racing connection");
            indexer::index_repo(&root, &racing).expect("racing re-index");
        }));

        let error = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())
            .expect_err("stale inputs");
        assert!(format!("{error:#}").contains("repository changed"));
        let (status, code): (String, String) = conn.query_row(
            "SELECT status, error_code FROM scout_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            (status.as_str(), code.as_str()),
            ("incomplete", "inputs_changed")
        );
        let artifacts: i64 =
            conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
                row.get(0)
            })?;
        assert_eq!(artifacts, 0, "no partial write against changed inputs");
        Ok(())
    }
}
