//! Semantic scouting: generative runs over deterministic candidates.
//!
//! Every model call is recorded in the run ledger before it can publish
//! anything; failed, incomplete, and canceled runs stay attributable and
//! never create semantic artifacts. The model cannot add anchors: candidate
//! expansion is a Rust change, not model improvisation.

pub mod evidence;
pub mod ledger;
pub mod workflow;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::llm::config::{ModelSpec, RequestPolicy};
use crate::llm::protocol::{
    ChatMessage, CompleteRequest, PROTOCOL_VERSION, ProviderOptions, SubmitTool, Usage,
};
use crate::llm::{GatewayError, LlmGateway};
use crate::semantic::{self, WorkflowCandidateOptions, WorkflowCandidateSet};
use crate::{store, structural};

use evidence::EvidencePack;
use ledger::{ClassificationRow, RunClaim, RunOutcome, RunSpec};

const ORPHAN_SWEEP_MINUTES: i64 = 24 * 60;
/// Rough bytes-per-token floor used only to reject packs that cannot
/// possibly fit the selected model's reported context window.
const MIN_BYTES_PER_TOKEN: usize = 3;

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

/// One candidate-closed workflow scouting run for an explicit seed set.
pub fn scout_workflows(
    root: &Path,
    conn: &Connection,
    gateway: &mut dyn LlmGateway,
    options: &WorkflowScoutOptions,
) -> Result<WorkflowScoutReport> {
    // This command issues exactly one completion; the budget still has to
    // admit it so `--max-calls 0` cannot silently no-op.
    if options.policy.max_calls < 1 {
        bail!("--max-calls must admit at least one request for workflow scouting");
    }
    ledger::sweep_orphaned_runs(conn, ORPHAN_SWEEP_MINUTES)?;

    // One bounded read snapshot for candidates and evidence; released before
    // any network wait so no database snapshot spans model latency.
    let (candidate_set, evidence) = store::with_read_snapshot(conn, "jscout_scout", || {
        let candidate_set = semantic::workflow_candidates(
            root,
            conn,
            &options.seeds,
            &WorkflowCandidateOptions {
                expected_snapshot: None,
                depth: options.depth,
                candidate_limit: options.candidate_limit,
            },
        )?;
        if candidate_set.traversal_truncated || candidate_set.candidate_truncated {
            bail!(
                "the deterministic candidate set is truncated (traversal: {}, candidates: {}); \
                 narrow the seeds or depth, or raise the supported deterministic limit — \
                 the model is never asked to interpret a partial boundary",
                candidate_set.traversal_truncated,
                candidate_set.candidate_truncated,
            );
        }
        let evidence = evidence::build(root, conn, &candidate_set.candidates)?;
        Ok((candidate_set, evidence))
    })?;

    let request = build_request(&candidate_set, &evidence, options)?;
    enforce_context_budget(gateway, &request, &evidence, options)?;

    let input_fingerprint = input_fingerprint(&candidate_set, &evidence, &request, options);
    let request_hash = blake3::hash(serde_json::to_string(&request)?.as_bytes())
        .to_hex()
        .to_string();
    let spec = RunSpec {
        scout_kind: "workflow".into(),
        gateway_protocol: PROTOCOL_VERSION,
        provider: options.model.provider.clone(),
        model: options.model.model_id.clone(),
        billing_path: provisional_billing_path(gateway, &options.model)?,
        reasoning: options.reasoning.clone(),
        prompt_version: workflow::PROMPT_VERSION.into(),
        source_snapshot: candidate_set.snapshot.clone(),
        input_fingerprint: input_fingerprint.clone(),
        request_hash,
    };

    let run_id = match ledger::claim_run(conn, &spec, options.rebuild)? {
        RunClaim::Reused(run_id) => {
            return reuse_report(conn, run_id, &candidate_set, &spec);
        }
        RunClaim::Claimed(run_id) => run_id,
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
    let annotate_input = semantic::workflow_request(
        validated.name.clone(),
        Some(validated.description.clone()),
        validated.participants.clone(),
        "likely".into(),
        candidate_set.snapshot.clone(),
        None,
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
    gateway: &mut dyn LlmGateway,
    request: &CompleteRequest,
    evidence: &EvidencePack,
    options: &WorkflowScoutOptions,
) -> Result<()> {
    let request_bytes = serde_json::to_string(request)?.len();
    if request_bytes > options.policy.context_bytes {
        bail!(
            "serialized evidence pack is {request_bytes} bytes, over --context-bytes {}; \
             narrow the seeds/depth or raise the budget ({} evidence files)",
            options.policy.context_bytes,
            evidence.files.len(),
        );
    }
    let (_, capabilities) = gateway.capabilities(Some(&options.model.spec))?;
    let Some(capabilities) = capabilities else {
        bail!(
            "model {} is not known to the gateway; run `jscout llm doctor --model {}`",
            options.model.spec,
            options.model.spec,
        );
    };
    if let Some(window) = capabilities.context_window {
        let estimated_tokens = (request_bytes / MIN_BYTES_PER_TOKEN) as u64;
        if estimated_tokens >= window {
            bail!(
                "evidence pack (~{estimated_tokens} tokens) cannot fit the {} context window \
                 of {}; narrow the seeds or choose a larger-context model",
                window,
                options.model.spec,
            );
        }
    }
    Ok(())
}

fn provisional_billing_path(gateway: &mut dyn LlmGateway, model: &ModelSpec) -> Result<String> {
    if model.provider == "openai-codex" {
        return Ok("plan".into());
    }
    let (providers, _) = gateway.capabilities(None)?;
    Ok(if providers.custom.iter().any(|id| id == &model.provider) {
        "custom".into()
    } else {
        "api".into()
    })
}

fn input_fingerprint(
    candidate_set: &WorkflowCandidateSet,
    evidence: &EvidencePack,
    request: &CompleteRequest,
    options: &WorkflowScoutOptions,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-workflow-scout-input-v1\0");
    for part in [
        candidate_set.snapshot.as_str(),
        &candidate_set.seeds.join("\u{1}"),
        &candidate_set.fingerprint,
        &evidence.rendered,
        workflow::PROMPT_VERSION,
        &options.model.spec,
        options.reasoning.as_deref().unwrap_or(""),
        options.service_tier.as_deref().unwrap_or(""),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(&PROTOCOL_VERSION.to_le_bytes());
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

    use super::{WorkflowScoutOptions, scout_workflows};
    use crate::llm::config::{ModelSpec, RequestPolicy};
    use crate::llm::protocol::{
        CompleteRequest, ModelCapabilities, ProviderSummary, ToolCall, Usage,
    };
    use crate::llm::{CompletionOutcome, GatewayError, LlmGateway, StartedInfo};
    use crate::{indexer, store};

    struct FakeGateway {
        results: VecDeque<Result<CompletionOutcome, GatewayError>>,
        calls: usize,
        on_complete: Option<Box<dyn FnMut()>>,
    }

    impl FakeGateway {
        fn new(results: Vec<Result<CompletionOutcome, GatewayError>>) -> Self {
            Self {
                results: results.into(),
                calls: 0,
                on_complete: None,
            }
        }
    }

    impl LlmGateway for FakeGateway {
        fn capabilities(
            &mut self,
            model: Option<&str>,
        ) -> Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError> {
            Ok((
                ProviderSummary {
                    builtin: 1,
                    custom: Vec::new(),
                },
                model.map(|_| ModelCapabilities {
                    provider: "faux".into(),
                    model: "faux-model".into(),
                    api: "faux".into(),
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
            _request: &CompleteRequest,
            _timeout: Duration,
        ) -> Result<CompletionOutcome, GatewayError> {
            self.calls += 1;
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

    #[test]
    fn publishes_a_candidate_closed_workflow_atomically() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = fixture(repo.path())?;
        let anchors = candidate_anchors(&conn, repo.path())?;
        assert_eq!(anchors.len(), 2);

        let mut gateway = FakeGateway::new(vec![Ok(outcome(full_submission(&anchors)))]);
        let report = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())?;
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
        assert_eq!(billing, "api");

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
