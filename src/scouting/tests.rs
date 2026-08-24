use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use super::{
    CardScoutOptions, ConceptScoutOptions, ScoutReport, SummaryScoutOptions, WorkflowScoutOptions,
    scout_card_plan, scout_concept_plan, scout_refresh, scout_workflow_plan, scout_workflows,
};
use crate::llm::config::{ModelSpec, RequestPolicy};
use crate::llm::protocol::{CompleteRequest, ModelCapabilities, ProviderSummary, ToolCall, Usage};
use crate::llm::{CompletionOutcome, CompletionTask, GatewayError, LlmGateway, StartedInfo};
use crate::{indexer, store};

struct FakeGateway {
    results: VecDeque<Result<CompletionOutcome, GatewayError>>,
    calls: usize,
    capability_calls: usize,
    batch_sizes: Vec<usize>,
    batch_result_count: Option<usize>,
    last_max_tokens: Option<u64>,
    on_complete: Option<Box<dyn FnMut()>>,
}

impl FakeGateway {
    fn new(results: Vec<Result<CompletionOutcome, GatewayError>>) -> Self {
        Self {
            results: results.into(),
            calls: 0,
            capability_calls: 0,
            batch_sizes: Vec::new(),
            batch_result_count: None,
            last_max_tokens: None,
            on_complete: None,
        }
    }

    fn with_batch_result_count(mut self, count: usize) -> Self {
        self.batch_result_count = Some(count);
        self
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
                billing_path: Some("api".into()),
                auth_configured: true,
                auth_type: Some("api_key".into()),
                auth_source: Some("test".into()),
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

    fn complete_batch(
        &mut self,
        tasks: &[CompletionTask<'_>],
    ) -> Vec<Result<CompletionOutcome, GatewayError>> {
        self.batch_sizes.push(tasks.len());
        let mut outcomes = tasks
            .iter()
            .map(|task| self.complete(task.request, task.timeout))
            .collect::<Vec<_>>();
        let Some(result_count) = self.batch_result_count else {
            return outcomes;
        };
        outcomes.truncate(result_count);
        while outcomes.len() < result_count {
            outcomes.push(
                self.results
                    .pop_front()
                    .expect("missing configured extra batch result"),
            );
        }
        outcomes
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
        attempts: 1,
        response_model: Some("faux-model".into()),
    }
}

fn repository_outcome(role: &str) -> CompletionOutcome {
    let mut result = outcome(json!({
        "role": role,
        "confidence": "likely",
        "explanation": format!("the evidence classifies this scope as {role}"),
        "evidence": ["E001"],
    }));
    result.tool_call.name = super::repository::SUBMIT_TOOL_NAME.into();
    result
}

#[test]
fn repository_scout_subdivides_mixed_scopes_and_reuses_every_exact_run() -> Result<()> {
    use super::repository::{
        CurrentClassification, EvidenceItem, RepositoryEvidencePack, RepositoryPlan,
        RepositoryPlanItem, RepositoryScoutOptions,
    };
    use crate::recon::{self, SubjectSelector};

    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("mixed/docs"))?;
    std::fs::write(
        repo.path().join("mixed/runtime.ts"),
        "export const run = 1;\n",
    )?;
    std::fs::write(
        repo.path().join("mixed/docs/guide.ts"),
        "export const guide = 1;\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let selector = SubjectSelector::RepositoryArea {
        scope: "mixed".into(),
        direct_only: false,
    };
    let state = recon::build_scope_state(
        repo.path(),
        &conn,
        "area:repository:mixed".into(),
        selector.clone(),
    )?;
    let evidence = RepositoryEvidencePack {
        algorithm: recon::EVIDENCE_ALGORITHM,
        subject_key: state.subject_key.clone(),
        subject_kind: "area".into(),
        member_count: state.members.len(),
        language_counts: Default::default(),
        chunk_kind_counts: Default::default(),
        surface_counts: BTreeMap::from([("handwritten".into(), 2)]),
        items: vec![EvidenceItem {
            id: "E001".into(),
            kind: "aggregate".into(),
            source: None,
            start_line: None,
            end_line: None,
            content: "runtime and documentation files coexist".into(),
        }],
        rendered: "deterministic mixed-scope evidence".into(),
    };
    let make_plan = || RepositoryPlan {
        snapshot: snapshot.clone(),
        max_subjects: 3,
        max_depth: 1,
        subject_limit_reached: false,
        configured_projects: 0,
        configuration_problems: Vec::new(),
        items: vec![RepositoryPlanItem {
            subject_key: state.subject_key.clone(),
            subject_kind: "area".into(),
            display_name: "mixed".into(),
            parent_subject_key: None,
            depth: 0,
            selector: selector.clone(),
            evidence_fingerprint: state.evidence_fingerprint.clone(),
            member_count: state.members.len(),
            evidence: evidence.clone(),
            current_classification: None::<CurrentClassification>,
            downstream_policy: "neutral inclusion".into(),
            potential_children: vec![
                "area:repository:mixed:direct".into(),
                "area:repository:mixed/docs".into(),
            ],
            state: state.clone(),
        }],
        omitted_subjects: Vec::new(),
    };
    let options = RepositoryScoutOptions {
        model: ModelSpec::parse("faux:faux-model")?,
        reasoning: None,
        service_tier: None,
        policy: RequestPolicy::new(30, 3, 240_000)?,
        rebuild: false,
        max_subjects: 3,
        max_depth: 1,
    };

    let mut first = FakeGateway::new(vec![
        Ok(repository_outcome("mixed")),
        Ok(repository_outcome("documentation")),
        Ok(repository_outcome("runtime")),
    ]);
    let report = super::repository::execute(repo.path(), &conn, &mut first, &options, make_plan())?;
    assert_eq!(report.model_calls, 3);
    assert_eq!(report.reports.len(), 3);
    assert_eq!(first.calls, 3);
    assert_eq!(
        report
            .reports
            .iter()
            .map(|report| report.subject.as_str())
            .collect::<Vec<_>>(),
        vec![
            "area:repository:mixed",
            "area:repository:mixed/docs",
            "area:repository:mixed:direct",
        ],
    );
    assert_eq!(
        recon::file_policy_by_path(&conn, "mixed/runtime.ts")?
            .unwrap()
            .effective_role,
        "runtime"
    );
    assert_eq!(
        recon::file_policy_by_path(&conn, "mixed/docs/guide.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );
    let cited: String = conn.query_row(
        "SELECT cited_evidence_json FROM repository_classifications
         WHERE subject_key='area:repository:mixed/docs' ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let cited: serde_json::Value = serde_json::from_str(&cited)?;
    assert_eq!(cited[0]["id"], "E001");
    assert!(
        cited[0]["content"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    let mut dry_gateway = FakeGateway::new(Vec::new());
    let dry = super::repository::dry_run_report(&conn, &mut dry_gateway, &make_plan(), &options)?;
    assert_eq!(dry["calls_planned"], json!(0));
    assert_eq!(dry["reusable_items"], json!(1));
    assert_eq!(dry["plan"]["items"][0]["reusable"], json!(true));
    assert_eq!(dry["plan"]["items"][0]["would_call"], json!(false));
    assert_eq!(dry_gateway.calls, 0);

    let mut rebuild_options = options.clone();
    rebuild_options.rebuild = true;
    let mut rebuild_gateway = FakeGateway::new(Vec::new());
    let rebuild = super::repository::dry_run_report(
        &conn,
        &mut rebuild_gateway,
        &make_plan(),
        &rebuild_options,
    )?;
    assert_eq!(rebuild["calls_planned"], json!(1));
    assert_eq!(rebuild["reusable_items"], json!(0));
    assert_eq!(rebuild["plan"]["items"][0]["reusable"], json!(false));
    assert_eq!(rebuild["plan"]["items"][0]["would_call"], json!(true));
    assert_eq!(rebuild_gateway.calls, 0);

    let mut reused = FakeGateway::new(Vec::new());
    let report =
        super::repository::execute(repo.path(), &conn, &mut reused, &options, make_plan())?;
    assert_eq!(report.model_calls, 0);
    assert_eq!(report.reports.len(), 3);
    assert!(
        report
            .reports
            .iter()
            .all(|report| report.status == "reused")
    );
    assert_eq!(reused.calls, 0);
    Ok(())
}

#[test]
fn repository_subject_drift_is_incomplete_and_does_not_abort_later_subjects() -> Result<()> {
    use super::repository::{
        CurrentClassification, EvidenceItem, RepositoryEvidencePack, RepositoryPlan,
        RepositoryPlanItem, RepositoryScoutOptions,
    };
    use crate::recon::{self, SubjectSelector};

    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("docs"))?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    std::fs::write(
        repo.path().join("docs/guide.ts"),
        "export const guide = 1;\n",
    )?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let item = |scope: &str| -> Result<RepositoryPlanItem> {
        let subject_key = format!("area:repository:{scope}");
        let selector = SubjectSelector::RepositoryArea {
            scope: scope.into(),
            direct_only: false,
        };
        let state =
            recon::build_scope_state(repo.path(), &conn, subject_key.clone(), selector.clone())?;
        Ok(RepositoryPlanItem {
            subject_key: subject_key.clone(),
            subject_kind: "area".into(),
            display_name: scope.into(),
            parent_subject_key: None,
            depth: 0,
            selector,
            evidence_fingerprint: state.evidence_fingerprint.clone(),
            member_count: state.members.len(),
            evidence: RepositoryEvidencePack {
                algorithm: recon::EVIDENCE_ALGORITHM,
                subject_key,
                subject_kind: "area".into(),
                member_count: state.members.len(),
                language_counts: Default::default(),
                chunk_kind_counts: Default::default(),
                surface_counts: BTreeMap::from([("handwritten".into(), 2)]),
                items: vec![EvidenceItem {
                    id: "E001".into(),
                    kind: "aggregate".into(),
                    source: None,
                    start_line: None,
                    end_line: None,
                    content: format!("bounded evidence for {scope}"),
                }],
                rendered: format!("bounded evidence for {scope}"),
            },
            current_classification: None::<CurrentClassification>,
            downstream_policy: "neutral inclusion".into(),
            potential_children: Vec::new(),
            state,
        })
    };
    let plan = RepositoryPlan {
        snapshot,
        max_subjects: 2,
        max_depth: 0,
        subject_limit_reached: false,
        configured_projects: 0,
        configuration_problems: Vec::new(),
        items: vec![item("docs")?, item("src")?],
        omitted_subjects: Vec::new(),
    };
    let options = RepositoryScoutOptions {
        model: ModelSpec::parse("faux:faux-model")?,
        reasoning: None,
        service_tier: None,
        policy: RequestPolicy::new(30, 2, 240_000)?,
        rebuild: false,
        max_subjects: 2,
        max_depth: 0,
    };
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let mut completions = 0;
    let mut gateway = FakeGateway::new(vec![
        Ok(repository_outcome("documentation")),
        Ok(repository_outcome("runtime")),
    ]);
    gateway.on_complete = Some(Box::new(move || {
        completions += 1;
        if completions == 1 {
            std::fs::write(root.join("docs/guide.ts"), "export const guide = 2;\n")
                .expect("change first subject");
            let racing = store::open_path(&db_path).expect("open racing connection");
            indexer::index_repo(&root, &racing).expect("publish racing index");
        }
    }));

    let report = super::repository::execute(repo.path(), &conn, &mut gateway, &options, plan)?;
    assert_eq!(report.model_calls, 2);
    assert_eq!(report.reports[0].status, "incomplete");
    assert_eq!(report.reports[1].status, "completed");
    assert!(recon::file_policy_by_path(&conn, "docs/guide.ts")?.is_none());
    assert_eq!(
        recon::file_policy_by_path(&conn, "src/run.ts")?
            .unwrap()
            .effective_role,
        "runtime"
    );
    Ok(())
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
    let first = scout_workflow_plan(repo.path(), &conn, &mut first_gateway, &options, first_plan)?;
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
    let super::refresh::RefreshConfig::Workflow(config) = &mut unresolvable.config else {
        panic!("expected a workflow replay configuration");
    };
    config.seeds = vec!["deleted-workflow-anchor".into()];
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
    let report = scout_workflows(repo.path(), &conn, &mut gateway, &scout_options())?;
    assert_eq!(
        report.status, "failed",
        "subject-local failure, not an abort"
    );
    assert!(
        report
            .failure
            .as_deref()
            .unwrap_or("")
            .contains("candidate-closed validation")
    );

    let (status, code): (String, String) = conn.query_row(
        "SELECT status, error_code FROM scout_runs ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((status.as_str(), code.as_str()), ("failed", "validation"));
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
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
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
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
    let error =
        scout_workflows(repo.path(), &conn, &mut gateway, &scout_options()).expect_err("canceled");
    assert!(format!("{error:#}").contains("canceled"));
    let (status, code): (String, String) = conn.query_row(
        "SELECT status, error_code FROM scout_runs ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((status.as_str(), code.as_str()), ("canceled", "canceled"));
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
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
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(artifacts, 0, "no partial write against changed inputs");
    Ok(())
}

#[test]
fn dry_run_report_marks_budget_and_call_slots() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("index.ts"),
        "export function first() { return shared(); }\n\
         export function second() { return 2; }\n\
         function shared() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let plan = super::plan::workflows(repo.path(), &conn, &[], 2, 31)?;
    assert!(
        plan.items.len() >= 2,
        "fixture must yield two distinct boundaries, got {}",
        plan.items.len()
    );
    assert_eq!(plan.auto_seeds_discovered, Some(2));

    let mut options = scout_options();
    options.seeds = Vec::new();
    options.policy = RequestPolicy::new(30, 1, 240_000).expect("policy");
    let report = super::dry_run_report(&plan, &options)?;
    assert_eq!(report["dry_run"], serde_json::json!(true));
    assert_eq!(report["calls_planned"], serde_json::json!(1));
    assert_eq!(report["over_context_bytes_items"], serde_json::json!(0));
    let items = report["plan"]["items"].as_array().expect("annotated items");
    assert_eq!(items[0]["would_call"], serde_json::json!(true));
    assert_eq!(items[1]["would_call"], serde_json::json!(false));
    assert!(items[0]["request_bytes"].as_u64().expect("bytes") > 0);
    assert_eq!(items[0]["over_context_bytes"], serde_json::json!(false));

    // A budget below any request size refuses every item and plans no
    // calls, exactly as execution would.
    options.policy = RequestPolicy::new(30, 1, 64).expect("tiny policy");
    let refused = super::dry_run_report(&plan, &options)?;
    assert_eq!(refused["calls_planned"], serde_json::json!(0));
    assert_eq!(
        refused["over_context_bytes_items"],
        serde_json::json!(plan.items.len())
    );
    let refused_items = refused["plan"]["items"]
        .as_array()
        .expect("annotated items");
    assert!(
        refused_items
            .iter()
            .all(|item| item["over_context_bytes"] == serde_json::json!(true)
                && item["would_call"] == serde_json::json!(false))
    );
    Ok(())
}

fn card_outcome(arguments: serde_json::Value) -> CompletionOutcome {
    let mut outcome = outcome(arguments);
    outcome.tool_call.name = super::card::SUBMIT_TOOL_NAME.into();
    outcome
}

fn card_options() -> CardScoutOptions {
    CardScoutOptions {
        anchors: vec!["flow.ts:start".into()],
        files: Vec::new(),
        reconnaissance_subjects: Vec::new(),
        model: ModelSpec::parse("faux:faux-model").expect("model spec"),
        reasoning: None,
        service_tier: None,
        policy: RequestPolicy::new(30, 1, 240_000).expect("policy"),
        rebuild: false,
        supersedes_artifact_id: None,
    }
}

fn card_submission() -> serde_json::Value {
    json!({
        "purpose": {
            "text": "entry point that completes the flow through the finisher",
            "evidence": [{"start_line": 2, "end_line": 2}],
        },
        "side_effects": [{
            "text": "delegates to the terminal finisher",
            "evidence": [{"start_line": 2, "end_line": 2}],
        }],
        "incomplete_reason": null,
    })
}

fn concept_options(term: &str) -> ConceptScoutOptions {
    ConceptScoutOptions {
        terms: vec![term.into()],
        model: ModelSpec::parse("faux:faux-model").expect("model spec"),
        reasoning: None,
        service_tier: None,
        policy: RequestPolicy::new(30, 1, 240_000).expect("policy"),
        rebuild: false,
        supersedes_artifact_id: None,
    }
}

fn concept_outcome(arguments: serde_json::Value) -> CompletionOutcome {
    let mut outcome = outcome(arguments);
    outcome.tool_call.name = super::concept::SUBMIT_TOOL_NAME.into();
    outcome
}

fn concept_submission(aliases: &[String], source_count: usize) -> serde_json::Value {
    let alias_claims = aliases
        .iter()
        .enumerate()
        .map(|(index, alias)| {
            json!({
                "text": alias,
                "sources": [format!("S{}", index.min(source_count.saturating_sub(1)) + 1)],
            })
        })
        .collect::<Vec<_>>();
    let candidates = (0..source_count)
        .map(|source_index| {
            let reference = format!("S{}", source_index + 1);
            let mut claims = vec!["/definition".to_string()];
            for (alias_index, _) in aliases.iter().enumerate() {
                if alias_index.min(source_count.saturating_sub(1)) == source_index {
                    claims.push(format!("/aliases/{alias_index}"));
                }
            }
            json!({
                "source": reference,
                "decision": "included",
                "claims": claims,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "definition": {
            "text": "Repository vocabulary describing how invoice settlement is completed.",
            "sources": (1..=source_count).map(|index| format!("S{index}")).collect::<Vec<_>>(),
        },
        "aliases": alias_claims,
        "candidates": candidates,
        "incomplete_reason": null,
    })
}

#[test]
fn remote_timeout_fails_one_subject_and_the_batch_continues() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let conn = fixture(dir.path())?;
    let mut options = card_options();
    options.anchors = vec!["flow.ts:start".into(), "flow.ts:finish".into()];
    options.policy = RequestPolicy::new(30, 2, 240_000)
        .expect("policy")
        .with_max_concurrency(2)?;
    let plan = super::plan::cards(dir.path(), &conn, &options.anchors)?;
    assert_eq!(plan.items.len(), 2);
    let mut gateway = FakeGateway::new(vec![
        Err(GatewayError::Remote(crate::llm::protocol::RemoteError {
            code: "timeout".into(),
            message: "completion exceeded 300000 ms".into(),
            retryable: true,
            capacity: false,
        })),
        Ok(card_outcome(card_submission())),
    ]);
    let database = store::db_path(dir.path());
    gateway.on_complete = Some(Box::new(move || {
        let inspection = store::open_path(&database).expect("open scouting ledger");
        let claimed: i64 = inspection
            .query_row(
                "SELECT count(*) FROM scout_runs WHERE status='running'",
                [],
                |row| row.get(0),
            )
            .expect("count claimed runs");
        assert_eq!(
            claimed, 2,
            "every concurrent call is claimed before dispatch"
        );
    }));
    let batch = scout_card_plan(dir.path(), &conn, &mut gateway, &options, plan)?;
    assert_eq!(gateway.batch_sizes, vec![2]);
    assert_eq!(batch.reports.len(), 2);
    let failed = batch
        .reports
        .iter()
        .find(|report| report.status == "failed")
        .expect("timed-out subject records a failed report");
    assert!(
        failed
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("gateway timeout")),
        "{failed:?}"
    );
    assert!(
        failed.subject.contains("flow.ts"),
        "the slow subject must be identifiable: {}",
        failed.subject
    );
    assert!(
        batch
            .reports
            .iter()
            .any(|report| report.status == "completed"),
        "the batch continues past a remote timeout"
    );
    Ok(())
}

#[test]
fn later_claim_conflict_releases_an_earlier_staged_run() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let conn = fixture(dir.path())?;
    let mut options = card_options();
    options.anchors = vec!["flow.ts:start".into(), "flow.ts:finish".into()];
    options.policy = RequestPolicy::new(30, 2, 240_000)?.with_max_concurrency(2)?;
    let plan = super::plan::cards(dir.path(), &conn, &options.anchors)?;
    assert_eq!(plan.items.len(), 2);

    let mut preparation_gateway = FakeGateway::new(Vec::new());
    let mut cache = super::PreparationCache::default();
    let first = super::prepare_card(
        &mut preparation_gateway,
        &mut cache,
        plan.items[0].clone(),
        &options,
    )?;
    let blocked = super::prepare_card(
        &mut preparation_gateway,
        &mut cache,
        plan.items[1].clone(),
        &options,
    )?;
    let first_fingerprint = first.spec.input_fingerprint;
    let blocked_fingerprint = blocked.spec.input_fingerprint.clone();
    let super::ledger::RunClaim::Claimed {
        run_id: blocked_run,
        ..
    } = super::ledger::claim_run(&conn, &blocked.spec, false)?
    else {
        panic!("expected the conflicting input to be claimed");
    };

    let mut gateway = FakeGateway::new(Vec::new());
    let error = scout_card_plan(dir.path(), &conn, &mut gateway, &options, plan)
        .expect_err("the later in-flight input must reject the wave");
    assert!(
        error.to_string().contains("already in progress"),
        "{error:#}"
    );
    assert_eq!(gateway.calls, 0, "no staged request may dispatch");

    let (status, error_code): (String, Option<String>) = conn.query_row(
        "SELECT status, error_code FROM scout_runs
         WHERE input_fingerprint=?1 ORDER BY id DESC LIMIT 1",
        [&first_fingerprint],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("wave_aborted"));
    let blocked_status: String = conn.query_row(
        "SELECT status FROM scout_runs WHERE id=?1",
        [blocked_run],
        |row| row.get(0),
    )?;
    assert_eq!(blocked_status, "running");
    assert_ne!(first_fingerprint, blocked_fingerprint);
    Ok(())
}

#[test]
fn malformed_batch_cardinality_terminalizes_every_claim() -> Result<()> {
    for result_count in [1, 3] {
        let dir = tempfile::tempdir()?;
        let conn = fixture(dir.path())?;
        let mut options = card_options();
        options.anchors = vec!["flow.ts:start".into(), "flow.ts:finish".into()];
        options.policy = RequestPolicy::new(30, 2, 240_000)?.with_max_concurrency(2)?;
        let plan = super::plan::cards(dir.path(), &conn, &options.anchors)?;
        assert_eq!(plan.items.len(), 2);

        let mut gateway = FakeGateway::new(vec![
            Ok(card_outcome(card_submission())),
            Ok(card_outcome(card_submission())),
            Ok(card_outcome(card_submission())),
        ])
        .with_batch_result_count(result_count);
        let error = scout_card_plan(dir.path(), &conn, &mut gateway, &options, plan)
            .expect_err("malformed batch cardinality must fail the wave");
        assert_eq!(gateway.batch_sizes, vec![2]);
        assert_eq!(gateway.calls, 2, "every expected task must be dispatched");
        assert!(
            error.to_string().contains(&format!(
                "gateway returned {result_count} outcomes for 2 card requests"
            )),
            "{error:#}"
        );

        let running: i64 = conn.query_row(
            "SELECT count(*) FROM scout_runs WHERE status='running'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(running, 0, "malformed results must not strand a claim");
        if result_count == 1 {
            let protocol_failures: i64 = conn.query_row(
                "SELECT count(*) FROM scout_runs
                 WHERE status='failed' AND error_code='protocol'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                protocol_failures, 1,
                "the missing outcome must terminalize its claimed run"
            );
        }
    }
    Ok(())
}

#[test]
fn local_frame_timeout_remains_batch_fatal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let conn = fixture(dir.path())?;
    let options = card_options();
    let plan = super::plan::cards(dir.path(), &conn, &options.anchors)?;
    let mut gateway = FakeGateway::new(vec![Err(GatewayError::Timeout(
        std::time::Duration::from_secs(30),
    ))]);
    let error = scout_card_plan(dir.path(), &conn, &mut gateway, &options, plan)
        .expect_err("a poisoned connection must abort the batch");
    assert!(error.to_string().contains("gateway completion failed"));
    Ok(())
}

fn scout_one_card(
    root: &Path,
    conn: &rusqlite::Connection,
    gateway: &mut dyn LlmGateway,
    options: &CardScoutOptions,
) -> Result<ScoutReport> {
    let plan = super::plan::cards(root, conn, &options.anchors)?;
    assert_eq!(plan.items.len(), 1, "one explicit anchor is one card run");
    let batch = scout_card_plan(root, conn, gateway, options, plan)?;
    Ok(batch.reports.into_iter().next().expect("one card report"))
}

#[test]
fn concept_scout_publishes_exact_links_reuses_and_drills_to_source() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut card = card_submission();
    card["domain_terms"] = json!([{
        "term": "Invoice Settlement",
        "evidence": [{"start_line": 2, "end_line": 2}],
    }]);
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card))]);
    let card_report = scout_one_card(repo.path(), &conn, &mut card_gateway, &card_options())?;
    let card_id = card_report.artifact_id.expect("card artifact");

    let options = concept_options("invoice settlement");
    let plan = super::plan::concepts(&conn, &options.terms)?;
    assert_eq!(plan.items.len(), 1);
    let aliases = plan.items[0].aliases.clone();
    let child_count = plan.items[0].child_count;
    let mut gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &aliases,
        child_count,
    )))]);
    let batch = scout_concept_plan(repo.path(), &conn, &mut gateway, &options, plan)?;
    assert_eq!(batch.reports.len(), 1);
    assert_eq!(batch.reports[0].status, "completed");
    assert_eq!(batch.reports[0].kind, "concept");
    let concept_id = batch.reports[0].artifact_id.expect("concept artifact");
    let relation_count: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_relations
         WHERE src_artifact_id=?1 AND dst_artifact_id=?2 AND relation='related_to'",
        rusqlite::params![concept_id, card_id],
        |row| row.get(0),
    )?;
    assert!(
        relation_count >= 2,
        "claim link plus whole-input dependency"
    );

    let artifact = crate::semantic::load_artifact(&conn, concept_id)?.expect("concept");
    assert_eq!(artifact.freshness, "fresh");
    assert_eq!(artifact.name.as_deref(), Some("invoice settlement"));

    let second_plan = super::plan::concepts(&conn, &options.terms)?;
    let mut no_call = FakeGateway::new(Vec::new());
    let second = scout_concept_plan(repo.path(), &conn, &mut no_call, &options, second_plan)?;
    assert_eq!(second.reports[0].status, "reused");
    assert_eq!(no_call.calls, 0);

    let queried = crate::semantic_query::query(
        repo.path(),
        &conn,
        None,
        &crate::semantic_query::QueryOptions {
            artifact_id: Some(concept_id),
            artifact_types: vec!["concept".into()],
            include_source: true,
            ..Default::default()
        },
    )?;
    assert_eq!(queried.semantic_artifacts[0].id, concept_id);
    assert!(
        queried
            .source_evidence
            .iter()
            .any(|evidence| evidence.file == "flow.ts" && evidence.source.is_some()),
        "concept evidence must drill to exact source"
    );
    Ok(())
}

#[test]
fn concept_publication_rechecks_new_matching_children() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let start = crate::structural::resolve_current_anchor(&conn, "flow.ts:start")?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let first = crate::semantic::annotate(
        repo.path(),
        &conn,
        &crate::semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(start.clone()),
            body: json!({"purpose":"starts settlement", "domain_terms":["Invoice Settlement"]}),
            supports: vec![
                crate::semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: start.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                },
                crate::semantic::SupportInput {
                    claim_path: "/domain_terms/0".into(),
                    anchor: start.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                },
            ],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        },
    )?;
    let options = concept_options("invoice settlement");
    let plan = super::plan::concepts(&conn, &options.terms)?;
    let aliases = plan.items[0].aliases.clone();
    let child_count = plan.items[0].child_count;
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let mut gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &aliases,
        child_count,
    )))]);
    gateway.on_complete = Some(Box::new(move || {
        let racing = store::open_path(&db_path).expect("racing connection");
        let finish = crate::structural::resolve_current_anchor(&racing, "flow.ts:finish")
            .expect("finish anchor");
        crate::semantic::annotate(
            &root,
            &racing,
            &crate::semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(finish.clone()),
                body: json!({"purpose":"finishes settlement", "domain_terms":["invoice settlement"]}),
                supports: vec![
                    crate::semantic::SupportInput { claim_path:"/purpose".into(), anchor:finish.clone(), role:None, evidence_file:"flow.ts".into(), evidence_start_line:1, evidence_end_line:1, confidence:"likely".into() },
                    crate::semantic::SupportInput { claim_path:"/domain_terms/0".into(), anchor:finish, role:None, evidence_file:"flow.ts".into(), evidence_start_line:1, evidence_end_line:1, confidence:"likely".into() },
                ],
                confidence:"likely".into(), snapshot: snapshot.clone(), supersedes:None,
            },
        ).expect("racing card");
    }));
    let error = scout_concept_plan(repo.path(), &conn, &mut gateway, &options, plan)
        .expect_err("new exact vocabulary child must refuse publication");
    assert!(format!("{error:#}").contains("vocabulary child set changed"));
    let concepts: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='concept'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(concepts, 0);
    let (status, code): (String, String) = conn.query_row(
        "SELECT status,error_code FROM scout_runs WHERE scout_kind='concept' ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("incomplete", "publication_recheck")
    );
    assert!(first.id > 0);
    Ok(())
}

#[test]
fn concept_publication_rechecks_a_concurrent_same_identity_concept() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut card = card_submission();
    card["domain_terms"] = json!([{
        "term": "Invoice Settlement",
        "evidence": [{"start_line": 2, "end_line": 2}],
    }]);
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card))]);
    scout_one_card(repo.path(), &conn, &mut card_gateway, &card_options())?;

    let options = concept_options("invoice settlement");
    let plan = super::plan::concepts(&conn, &options.terms)?;
    let aliases = plan.items[0].aliases.clone();
    let child_count = plan.items[0].child_count;
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let start = crate::structural::resolve_current_anchor(&conn, "flow.ts:start")?;
    let mut gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &aliases,
        child_count,
    )))]);
    gateway.on_complete = Some(Box::new(move || {
        let racing = store::open_path(&db_path).expect("racing connection");
        crate::semantic::annotate(
            &root,
            &racing,
            &crate::semantic::AnnotateInput {
                artifact_type: "concept".into(),
                name: Some("invoice settlement".into()),
                body: json!({
                    "definition": "A concurrent definition.",
                    "aliases": ["Invoice Settlement"],
                }),
                supports: vec![
                    crate::semantic::SupportInput {
                        claim_path: "/definition".into(),
                        anchor: start.clone(),
                        role: None,
                        evidence_file: "flow.ts".into(),
                        evidence_start_line: 2,
                        evidence_end_line: 2,
                        confidence: "likely".into(),
                    },
                    crate::semantic::SupportInput {
                        claim_path: "/aliases/0".into(),
                        anchor: start.clone(),
                        role: None,
                        evidence_file: "flow.ts".into(),
                        evidence_start_line: 2,
                        evidence_end_line: 2,
                        confidence: "likely".into(),
                    },
                ],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )
        .expect("racing concept");
    }));

    let error = scout_concept_plan(repo.path(), &conn, &mut gateway, &options, plan)
        .expect_err("a concurrent same-identity concept must refuse publication");
    assert!(format!("{error:#}").contains("lineage changed during publication"));
    let concepts: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='concept'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(concepts, 1, "only the racing concept may remain");
    let (status, code): (String, String) = conn.query_row(
        "SELECT status,error_code FROM scout_runs WHERE scout_kind='concept' ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("incomplete", "publication_recheck")
    );
    Ok(())
}

#[test]
fn concept_refresh_replays_the_normalized_group_and_publishes_a_successor() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut card = card_submission();
    card["domain_terms"] = json!([{
        "term": "Invoice Settlement",
        "evidence": [{"start_line": 2, "end_line": 2}],
    }]);
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card))]);
    let card_report = scout_one_card(repo.path(), &conn, &mut card_gateway, &card_options())?;
    let card_id = card_report.artifact_id.expect("card artifact");

    let options = concept_options("invoice settlement");
    let plan = super::plan::concepts(&conn, &options.terms)?;
    let aliases = plan.items[0].aliases.clone();
    let child_count = plan.items[0].child_count;
    let mut concept_gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &aliases,
        child_count,
    )))]);
    let first = scout_concept_plan(repo.path(), &conn, &mut concept_gateway, &options, plan)?;
    let concept_id = first.reports[0].artifact_id.expect("concept artifact");

    // Replace the model card with a current agent-authored card carrying
    // the same vocabulary. The concept's pinned child is now stale while
    // the normalized vocabulary group remains resolvable.
    let start = crate::structural::resolve_current_anchor(&conn, "flow.ts:start")?;
    crate::semantic::annotate(
        repo.path(),
        &conn,
        &crate::semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(start.clone()),
            body: json!({
                "purpose": "revised settlement entry point",
                "domain_terms": ["Invoice Settlement"],
            }),
            supports: vec![
                crate::semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: start.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                },
                crate::semantic::SupportInput {
                    claim_path: "/domain_terms/0".into(),
                    anchor: start,
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                },
            ],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(&conn)?,
            supersedes: Some(card_id),
        },
    )?;

    let selection = super::refresh::select(&conn, &[])?;
    assert_eq!(selection.targets.len(), 1);
    assert_eq!(selection.targets[0].artifact_id, concept_id);
    assert_eq!(selection.targets[0].config.kind(), "concept");
    assert_eq!(selection.targets[0].freshness, "stale");

    let refresh_plan = super::plan::concepts(&conn, &["invoice settlement".into()])?;
    let refreshed_aliases = refresh_plan.items[0].aliases.clone();
    let refreshed_children = refresh_plan.items[0].child_count;
    let mut refresh_gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &refreshed_aliases,
        refreshed_children,
    )))]);
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut refresh_gateway,
        selection,
        RequestPolicy::new(30, 1, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 1);
    assert_eq!(batch.reports[0].kind, "concept");
    let successor = batch.reports[0]
        .artifact_id
        .expect("refreshed concept successor");
    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [successor],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(concept_id));
    assert_eq!(
        crate::semantic::load_artifact(&conn, successor)?
            .expect("successor")
            .freshness,
        "fresh"
    );
    Ok(())
}

#[test]
fn concept_refresh_reports_non_fresh_unchanged_children_instead_of_reusing() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut card = card_submission();
    card["domain_terms"] = json!([{
        "term": "Invoice Settlement",
        "evidence": [{"start_line": 2, "end_line": 2}],
    }]);
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card))]);
    let card_report = scout_one_card(repo.path(), &conn, &mut card_gateway, &card_options())?;
    let card_id = card_report.artifact_id.expect("card artifact");

    let options = concept_options("invoice settlement");
    let plan = super::plan::concepts(&conn, &options.terms)?;
    let aliases = plan.items[0].aliases.clone();
    let child_count = plan.items[0].child_count;
    let mut concept_gateway = FakeGateway::new(vec![Ok(concept_outcome(concept_submission(
        &aliases,
        child_count,
    )))]);
    let first = scout_concept_plan(repo.path(), &conn, &mut concept_gateway, &options, plan)?;
    let concept_id = first.reports[0].artifact_id.expect("concept artifact");

    // Re-index a source change without replacing the semantic child. The
    // concept input fingerprint is unchanged, but both child and concept
    // are now non-fresh. Reusing the old run would falsely report success.
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 2; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_ne!(
        crate::semantic::load_artifact(&conn, card_id)?
            .expect("card")
            .freshness,
        "fresh"
    );

    let selection = super::refresh::select(&conn, &[concept_id])?;
    let mut no_call = FakeGateway::new(Vec::new());
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut no_call,
        selection,
        RequestPolicy::new(30, 1, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 0);
    assert!(batch.reports.is_empty());
    assert_eq!(batch.skipped_unresolvable.len(), 1);
    assert!(
        batch.skipped_unresolvable[0]
            .reason
            .contains("refresh those children first")
    );
    assert!(
        crate::semantic::load_artifact(&conn, concept_id)?
            .expect("concept")
            .freshness
            != "fresh"
    );
    Ok(())
}

#[test]
fn publishes_a_card_with_claim_level_supports_and_reuses_identical_inputs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let report = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(report.status, "completed");
    assert_eq!(report.kind, "card");
    assert_eq!(report.candidate_count, 1);
    assert!(report.subject.contains("::start@"));
    let artifact_id = report.artifact_id.expect("published card");

    let (artifact_type, name, prompt_version, confidence): (String, String, String, String) = conn
        .query_row(
            "SELECT artifact_type, canonical_name, prompt_version, confidence
             FROM semantic_artifacts WHERE id=?1",
            [artifact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(artifact_type, "card");
    assert_eq!(name, report.subject);
    assert_eq!(prompt_version, "card-scout/v1");
    assert_eq!(confidence, "likely");

    let supports: Vec<(String, String, String)> = {
        let mut statement = conn.prepare(
            "SELECT claim_path, anchor_key, confidence FROM semantic_supports
             WHERE artifact_id=?1 ORDER BY claim_path",
        )?;
        let rows = statement.query_map([artifact_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    assert_eq!(
        supports.len(),
        2,
        "one support per claim per evidence range"
    );
    assert_eq!(supports[0].0, "/purpose");
    assert_eq!(supports[1].0, "/side_effects/0");
    assert!(
        supports
            .iter()
            .all(|support| support.1 == report.subject && support.2 == "likely"),
        "every card claim is anchored on its subject at likely confidence"
    );

    let (scout_kind, status, config_json): (String, String, String) = conn.query_row(
        "SELECT scout_kind, status, config_json FROM scout_runs WHERE id=?1",
        [report.run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        (scout_kind.as_str(), status.as_str()),
        ("card", "completed")
    );
    let config: super::CardRunConfig = serde_json::from_str(&config_json)?;
    assert_eq!(config.anchor, report.subject);

    // Reuse: identical inputs return the same artifact without a call.
    let mut idle_gateway = FakeGateway::new(Vec::new());
    let reused = scout_one_card(repo.path(), &conn, &mut idle_gateway, &card_options())?;
    assert_eq!(reused.status, "reused");
    assert_eq!(reused.artifact_id, Some(artifact_id));
    assert_eq!(idle_gateway.calls, 0);
    Ok(())
}

#[test]
fn unsupported_and_out_of_range_card_claims_publish_nothing() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;

    let mut unsupported = card_submission();
    unsupported["invariants"] = json!([{"text": "callers hold the lock", "evidence": []}]);
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(unsupported))]);
    let report = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(
        report.status, "failed",
        "subject-local failure, not an abort"
    );
    assert!(
        report
            .failure
            .as_deref()
            .unwrap_or("")
            .contains("claim-level card validation")
    );

    let mut out_of_range = card_submission();
    out_of_range["purpose"]["evidence"] = json!([{"start_line": 1, "end_line": 99}]);
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(out_of_range))]);
    let report = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(report.status, "failed");
    assert!(
        report
            .failure
            .as_deref()
            .unwrap_or("")
            .contains("outside `flow.ts`")
    );

    let failed: i64 = conn.query_row(
        "SELECT count(*) FROM scout_runs WHERE scout_kind='card' AND status='failed'
         AND error_code='validation'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(failed, 2);
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(artifacts, 0, "failed card runs publish nothing");
    Ok(())
}

#[test]
fn card_incomplete_records_the_run_without_an_artifact() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let submission = json!({
        "purpose": null,
        "incomplete_reason": "the caller supplies the settlement policy",
    });
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(submission))]);
    let report = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
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
    let classifications: i64 = conn.query_row(
        "SELECT count(*) FROM scout_classifications WHERE run_id=?1 AND decision='excluded'",
        [report.run_id],
        |row| row.get(0),
    )?;
    assert_eq!(classifications, 1);
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(artifacts, 0);
    Ok(())
}

#[test]
fn unrelated_repository_change_reuses_completed_card_runs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let first = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(first.status, "completed");

    // A new file changes the snapshot but not the subject's evidence:
    // the completed run must be reused, not respent.
    std::fs::write(
        repo.path().join("unrelated.ts"),
        "export const noise = 1;\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut idle_gateway = FakeGateway::new(Vec::new());
    let second = scout_one_card(repo.path(), &conn, &mut idle_gateway, &card_options())?;
    assert_eq!(second.status, "reused");
    assert_eq!(second.artifact_id, first.artifact_id);
    assert_eq!(idle_gateway.calls, 0);
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(artifacts, 1, "no duplicate current card");
    Ok(())
}

#[test]
fn subject_change_supersedes_the_previous_card_instead_of_duplicating() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let first = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    let first_artifact = first.artifact_id.expect("published card");

    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 2; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let second = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(second.status, "completed");
    let second_artifact = second.artifact_id.expect("published successor");

    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [second_artifact],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(first_artifact));
    let current: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts artifact
         WHERE artifact.artifact_type='card'
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           )",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(current, 1, "exactly one current card per subject");
    Ok(())
}

#[test]
fn reverting_the_subject_republishes_over_the_stale_successor() -> Result<()> {
    let original = "export function finish() { return 1; }\n\
                    export function start() { return finish(); }\n";
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;

    // Input A -> card A.
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let card_a = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    let artifact_a = card_a.artifact_id.expect("card A");
    let run_a = card_a.run_id;

    // Input B -> card B supersedes card A.
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 2; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let card_b = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    let artifact_b = card_b.artifact_id.expect("card B");
    let run_a_status: String = conn.query_row(
        "SELECT status FROM scout_runs WHERE id=?1",
        [run_a],
        |row| row.get(0),
    )?;
    assert_eq!(
        run_a_status, "superseded",
        "superseding card A must retire its generating run"
    );

    // Input back to A: run A must NOT satisfy reuse; the third card must
    // supersede stale card B and become the sole current artifact.
    std::fs::write(repo.path().join("flow.ts"), original)?;
    indexer::index_repo(repo.path(), &conn)?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let card_c = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    assert_eq!(card_c.status, "completed", "reverted input must republish");
    let artifact_c = card_c.artifact_id.expect("card C");
    assert_ne!(artifact_c, artifact_a);
    assert_ne!(artifact_c, artifact_b);

    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [artifact_c],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(artifact_b));
    let current: Vec<i64> = {
        let mut statement = conn.prepare(
            "SELECT artifact.id FROM semantic_artifacts artifact
             WHERE artifact.artifact_type='card'
               AND NOT EXISTS(
                 SELECT 1 FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id
               )",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    assert_eq!(current, vec![artifact_c], "card C is the sole current card");
    Ok(())
}

#[test]
fn a_failed_subject_does_not_abort_the_card_batch() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut options = card_options();
    options.anchors = vec!["flow.ts:finish".into(), "flow.ts:start".into()];
    options.policy = RequestPolicy::new(30, 2, 240_000).expect("policy");

    // First subject: arguments that cannot deserialize (schema failure).
    // Second subject: a valid card. The batch must record the first as a
    // failed, budget-consuming run and still publish the second.
    let mut gateway = FakeGateway::new(vec![
        Ok(card_outcome(
            json!({ "purpose": 42, "incomplete_reason": null }),
        )),
        Ok(card_outcome(card_submission())),
    ]);
    let plan = super::plan::cards(repo.path(), &conn, &options.anchors)?;
    assert_eq!(plan.items.len(), 2);
    let batch = scout_card_plan(repo.path(), &conn, &mut gateway, &options, plan)?;

    assert_eq!(batch.reports.len(), 2);
    assert_eq!(batch.reports[0].status, "failed");
    assert!(
        batch.reports[0]
            .failure
            .as_deref()
            .unwrap_or("")
            .contains("output contract")
    );
    assert_eq!(batch.reports[1].status, "completed");
    assert!(batch.reports[1].artifact_id.is_some());
    assert_eq!(batch.model_calls, 2, "a failed call still spends budget");
    let failed_runs: i64 = conn.query_row(
        "SELECT count(*) FROM scout_runs WHERE status='failed' AND error_code='schema'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(failed_runs, 1);
    Ok(())
}

#[test]
fn card_publication_loses_the_snapshot_race_without_a_partial_write() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    gateway.on_complete = Some(Box::new(move || {
        std::fs::write(
            root.join("flow.ts"),
            "export function finish() { return 2; }\n\
             export function start() { return finish(); }\n",
        )
        .expect("rewrite fixture");
        let racing = store::open_path(&db_path).expect("open racing connection");
        indexer::index_repo(&root, &racing).expect("racing re-index");
    }));

    let error = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())
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
    let artifacts: i64 = conn.query_row("SELECT count(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(artifacts, 0, "no partial write against changed inputs");
    Ok(())
}

#[test]
fn rebuilt_and_refreshed_cards_become_immutable_successors() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let first = scout_one_card(repo.path(), &conn, &mut gateway, &card_options())?;
    let first_id = first.artifact_id.expect("first card");

    let mut rebuild = card_options();
    rebuild.rebuild = true;
    let mut rebuild_gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let rebuilt = scout_one_card(repo.path(), &conn, &mut rebuild_gateway, &rebuild)?;
    let rebuilt_id = rebuilt.artifact_id.expect("rebuilt card");
    let supersedes: i64 = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [rebuilt_id],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, first_id);

    // Source drift stales the card; refresh replays its recorded anchor
    // and model into another immutable successor.
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 3; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let selection = super::refresh::select(&conn, &[])?;
    assert_eq!(selection.targets.len(), 1);
    assert_eq!(selection.targets[0].artifact_id, rebuilt_id);
    assert_eq!(selection.targets[0].config.kind(), "card");
    let mut refresh_gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut refresh_gateway,
        selection,
        RequestPolicy::new(30, 1, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 1);
    let successor = batch.reports[0].artifact_id.expect("refreshed card");
    let supersedes: i64 = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [successor],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, rebuilt_id);
    assert_eq!(batch.reports[0].kind, "card");
    Ok(())
}

#[test]
fn card_dry_run_plans_without_calls_ledger_rows_or_byte_drift() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let mut options = card_options();
    options.anchors = Vec::new();
    options.policy = RequestPolicy::new(30, 1, 240_000)?;

    let plan = super::plan::cards(repo.path(), &conn, &options.anchors)?;
    assert_eq!(plan.mode, "automatic");
    assert_eq!(
        plan.items.len(),
        2,
        "both exported symbols are card subjects"
    );
    let first = super::card_dry_run_report(&plan, &options)?;
    let repeat_plan = super::plan::cards(repo.path(), &conn, &options.anchors)?;
    let second = super::card_dry_run_report(&repeat_plan, &options)?;
    assert_eq!(
        serde_json::to_string(&first)?,
        serde_json::to_string(&second)?,
        "dry-run output must be byte-deterministic"
    );
    assert_eq!(first["dry_run"], json!(true));
    assert_eq!(first["calls_planned"], json!(1));
    assert_eq!(first["over_context_bytes_items"], json!(0));
    let items = first["plan"]["items"].as_array().expect("annotated items");
    assert_eq!(items[0]["would_call"], json!(true));
    assert_eq!(items[1]["would_call"], json!(false));
    assert!(items[0]["request_bytes"].as_u64().expect("bytes") > 0);

    options.policy = RequestPolicy::new(30, 2, 64)?;
    let refused = super::card_dry_run_report(&plan, &options)?;
    assert_eq!(refused["calls_planned"], json!(0));
    assert_eq!(refused["over_context_bytes_items"], json!(plan.items.len()));

    let runs: i64 = conn.query_row("SELECT count(*) FROM scout_runs", [], |row| row.get(0))?;
    assert_eq!(runs, 0, "a dry run writes no ledger rows");
    Ok(())
}

fn summary_outcome(arguments: serde_json::Value) -> CompletionOutcome {
    let mut outcome = outcome(arguments);
    outcome.tool_call.name = super::summary::SUBMIT_TOOL_NAME.into();
    outcome
}

fn summary_options(level: Option<&str>, max_calls: usize) -> SummaryScoutOptions {
    SummaryScoutOptions {
        level: level.map(str::to_string),
        scopes: Vec::new(),
        model: ModelSpec::parse("faux:faux-model").expect("model spec"),
        reasoning: None,
        service_tier: None,
        policy: RequestPolicy::new(30, max_calls, 240_000).expect("policy"),
        rebuild: false,
        supersedes_artifact_id: None,
    }
}

fn summary_submission(overview: &str, children: &[&str]) -> serde_json::Value {
    json!({
        "overview": { "text": overview, "children": children },
        "key_points": [],
        "incomplete_reason": null,
    })
}

fn artifact_fingerprint(conn: &rusqlite::Connection, artifact_id: i64) -> Result<String> {
    Ok(conn.query_row(
        "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
        [artifact_id],
        |row| row.get(0),
    )?)
}

/// (`claim_path`, relation, `dst_artifact_id`, `dst_fingerprint`, confidence).
type RelationRow = (String, String, i64, String, String);

/// Every child relation of one artifact, in a deterministic order.
fn relations_of(conn: &rusqlite::Connection, artifact_id: i64) -> Result<Vec<RelationRow>> {
    let mut statement = conn.prepare(
        "SELECT claim_path, relation, dst_artifact_id, dst_fingerprint, confidence
         FROM semantic_relations WHERE src_artifact_id=?1
         ORDER BY claim_path, dst_artifact_id",
    )?;
    let rows = statement.query_map([artifact_id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn current_summaries(conn: &rusqlite::Connection) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT artifact.id FROM semantic_artifacts artifact
         WHERE artifact.artifact_type='summary'
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           )
         ORDER BY artifact.id",
    )?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// One completed card over the shared `fixture` repo, used as the child
/// artifact every summary test summarizes.
fn seed_card(root: &Path, conn: &rusqlite::Connection) -> Result<ScoutReport> {
    let mut gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let report = scout_one_card(root, conn, &mut gateway, &card_options())?;
    assert_eq!(report.status, "completed", "seed card must publish");
    Ok(report)
}

/// Both exported symbols of the shared `fixture` repo as cards, so one
/// file scope plans two children. Returned in artifact-id order, which is
/// the order the file-level planner assigns `C1`, `C2`.
fn seed_two_cards(root: &Path, conn: &rusqlite::Connection) -> Result<Vec<i64>> {
    let mut options = card_options();
    options.anchors = vec!["flow.ts:finish".into(), "flow.ts:start".into()];
    options.policy = RequestPolicy::new(30, 2, 240_000)?;
    let mut finisher = card_submission();
    finisher["purpose"]["text"] = json!("terminal helper that completes the flow");
    finisher["purpose"]["evidence"] = json!([{"start_line": 1, "end_line": 1}]);
    finisher["side_effects"] = json!([]);
    let mut gateway = FakeGateway::new(vec![
        Ok(card_outcome(finisher)),
        Ok(card_outcome(card_submission())),
    ]);
    let plan = super::plan::cards(root, conn, &options.anchors)?;
    assert_eq!(plan.items.len(), 2);
    let batch = scout_card_plan(root, conn, &mut gateway, &options, plan)?;
    assert!(
        batch
            .reports
            .iter()
            .all(|report| report.status == "completed"),
        "both seed cards must publish"
    );
    let mut ids: Vec<i64> = batch
        .reports
        .iter()
        .map(|report| report.artifact_id.expect("seeded card"))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids.len(), 2);
    Ok(ids)
}

#[test]
fn summary_tool_contract_failure_is_subject_local() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    seed_card(repo.path(), &conn)?;
    let mut gateway = FakeGateway::new(vec![Err(GatewayError::Remote(
        crate::llm::protocol::RemoteError {
            code: "tool_contract".into(),
            message: "model returned no submit_scope_summary call".into(),
            retryable: true,
            capacity: false,
        },
    ))]);

    let batch = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;

    assert_eq!(batch.model_calls, 1);
    assert_eq!(batch.reports.len(), 1);
    assert_eq!(batch.reports[0].status, "failed");
    assert!(
        batch.reports[0]
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("tool_contract"))
    );
    assert_eq!(
        conn.query_row(
            "SELECT status FROM scout_runs WHERE id=?1",
            [batch.reports[0].run_id],
            |row| row.get::<_, String>(0),
        )?,
        "failed"
    );
    Ok(())
}

#[test]
fn uncited_children_still_gate_publication_and_freshness() -> Result<()> {
    // (a) An uncited child is still an input dependency: the model saw it
    // and chose what to keep, so its later supersession stales the summary
    // exactly like a cited child would.
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let cards = seed_two_cards(repo.path(), &conn)?;
    let (cited, uncited) = (cards[0], cards[1]);

    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the terminal helper that completes the settlement flow",
        &["C1"],
    )))]);
    let batch = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(batch.reports[0].status, "completed");
    assert_eq!(batch.reports[0].candidate_count, 2, "two planned children");
    let summary_id = batch.reports[0].artifact_id.expect("published summary");

    let relations = relations_of(&conn, summary_id)?;
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.0 == "/overview")
            .map(|relation| relation.2)
            .collect::<Vec<_>>(),
        vec![cited],
        "only the cited child carries a claim relation"
    );
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.0.is_empty())
            .map(|relation| relation.2)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([cited, uncited]),
        "every planned child is a whole-artifact input dependency"
    );

    let freshness = |id: i64| -> Result<String> {
        Ok(crate::semantic::load_artifact(&conn, id)?
            .expect("artifact exists")
            .freshness)
    };
    assert_eq!(freshness(summary_id)?, "fresh");

    let uncited_subject: String = conn.query_row(
        "SELECT canonical_name FROM semantic_artifacts WHERE id=?1",
        [uncited],
        |row| row.get(0),
    )?;
    crate::semantic::annotate(
        repo.path(),
        &conn,
        &crate::semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(uncited_subject.clone()),
            body: json!({ "purpose": "revised entry point for the settlement flow" }),
            supports: vec![crate::semantic::SupportInput {
                claim_path: "/purpose".into(),
                anchor: uncited_subject,
                role: None,
                evidence_file: "flow.ts".into(),
                evidence_start_line: 2,
                evidence_end_line: 2,
                confidence: "likely".into(),
            }],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(&conn)?,
            supersedes: Some(uncited),
        },
    )?;
    assert_eq!(
        freshness(summary_id)?,
        "stale",
        "superseding a child the summary never cited still stales it"
    );

    // (b) The same dependency blocks publication: a child superseded
    // mid-flight refuses the write whole, cited or not.
    let race_repo = tempfile::tempdir()?;
    let race_conn = fixture(race_repo.path())?;
    let race_cards = seed_two_cards(race_repo.path(), &race_conn)?;
    let race_uncited = race_cards[1];
    let race_subject: String = race_conn.query_row(
        "SELECT canonical_name FROM semantic_artifacts WHERE id=?1",
        [race_uncited],
        |row| row.get(0),
    )?;
    let root = race_repo.path().to_path_buf();
    let db_path = store::db_path(race_repo.path());
    let mut race_gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the terminal helper that completes the settlement flow",
        &["C1"],
    )))]);
    race_gateway.on_complete = Some(Box::new(move || {
        let racing = store::open_path(&db_path).expect("open racing connection");
        let snapshot =
            crate::structural::current_snapshot(&racing).expect("racing current snapshot");
        crate::semantic::annotate(
            &root,
            &racing,
            &crate::semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(race_subject.clone()),
                body: json!({ "purpose": "revised entry point for the settlement flow" }),
                supports: vec![crate::semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: race_subject.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot,
                supersedes: Some(race_uncited),
            },
        )
        .expect("racing successor card");
    }));

    let error = super::scout_summaries(
        race_repo.path(),
        &race_conn,
        &mut race_gateway,
        &summary_options(Some("file"), 1),
    )
    .expect_err("an uncited child stopped being current");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("publication recheck failed")
            && rendered.contains(&format!("child artifact {race_uncited} changed")),
        "unexpected failure: {rendered}"
    );
    let (status, code): (String, String) = race_conn.query_row(
        "SELECT status, error_code FROM scout_runs WHERE scout_kind='summary'
         ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("incomplete", "publication_recheck")
    );
    let summaries: i64 = race_conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='summary'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(summaries, 0, "an uncited child race publishes nothing");
    Ok(())
}

#[test]
fn hierarchy_gates_refuse_parents_over_missing_lower_scopes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;
    let package = repo.path().join("packages/app");
    std::fs::create_dir_all(package.join("src"))?;
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"@fixture/app\",\"version\":\"1.0.0\"}\n",
    )?;
    std::fs::write(
        package.join("src/alpha.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    std::fs::write(
        package.join("src/beta.ts"),
        "export function beta() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    // Both files bear cards, so both are file-level summary subjects.
    let mut cards = card_options();
    cards.anchors = vec![
        "packages/app/src/alpha.ts:alpha".into(),
        "packages/app/src/beta.ts:beta".into(),
    ];
    cards.policy = RequestPolicy::new(30, 2, 240_000)?;
    let mut single_line = card_submission();
    single_line["purpose"]["evidence"] = json!([{"start_line": 1, "end_line": 1}]);
    single_line["side_effects"] = json!([]);
    let mut card_gateway = FakeGateway::new(vec![
        Ok(card_outcome(single_line.clone())),
        Ok(card_outcome(single_line)),
    ]);
    let card_plan = super::plan::cards(repo.path(), &conn, &cards.anchors)?;
    let card_batch = scout_card_plan(repo.path(), &conn, &mut card_gateway, &cards, card_plan)?;
    assert!(
        card_batch
            .reports
            .iter()
            .all(|report| report.status == "completed")
    );

    // Only the first file gets a summary: the second exhausts the budget.
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "declares the alpha entry point of the app package",
        &["C1"],
    )))]);
    let first = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(first.reports.len(), 1);
    assert_eq!(first.reports[0].subject, "file:packages/app/src/alpha.ts");
    assert_eq!(first.skipped_for_call_budget, 1, "beta.ts is unsummarized");

    // The module cannot plan over the hole, and says which file is missing.
    let gated = super::plan::summaries(repo.path(), &conn, "module", &[])?;
    assert!(gated.items.is_empty(), "no module scope over a hole");
    assert_eq!(gated.skipped.len(), 1);
    assert_eq!(gated.skipped[0].scope, "module:@fixture/app");
    assert!(
        gated.skipped[0].reason.contains("packages/app/src/beta.ts")
            && gated.skipped[0].reason.contains("no current file summary"),
        "unexpected gate reason: {}",
        gated.skipped[0].reason
    );

    // Asking for the gated scope explicitly is a hard error, not a skip.
    let explicit = super::plan::summaries(
        repo.path(),
        &conn,
        "module",
        &["module:@fixture/app".to_string()],
    )
    .expect_err("an explicit gated scope must fail");
    let rendered = format!("{explicit:#}");
    assert!(
        rendered.contains("is not ready") && rendered.contains("packages/app/src/beta.ts"),
        "unexpected explicit failure: {rendered}"
    );

    // The repository is gated too: the module it owns has no summary yet.
    let repo_gated = super::plan::summaries(repo.path(), &conn, "repository", &[])?;
    assert!(repo_gated.items.is_empty());
    assert_eq!(repo_gated.skipped.len(), 1);
    assert_eq!(repo_gated.skipped[0].scope, "repo");
    assert!(
        repo_gated.skipped[0].reason.contains("@fixture/app")
            && repo_gated.skipped[0]
                .reason
                .contains("no current module summary"),
        "unexpected repository gate reason: {}",
        repo_gated.skipped[0].reason
    );

    // Summarize the second file; alpha's completed run is reused, so the
    // one budgeted call goes to beta and the module gate lifts.
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "declares the beta entry point of the app package",
        &["C1"],
    )))]);
    let second = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(second.reports.len(), 2);
    assert_eq!(second.reports[0].status, "reused");
    assert_eq!(second.reports[1].status, "completed");
    assert_eq!(second.reports[1].subject, "file:packages/app/src/beta.ts");

    let planned = super::plan::summaries(repo.path(), &conn, "module", &[])?;
    assert!(planned.skipped.is_empty(), "the gate lifts");
    assert_eq!(planned.items.len(), 1);
    assert_eq!(planned.items[0].scope, "module:@fixture/app");
    assert_eq!(
        planned.items[0].child_count, 2,
        "both file summaries are children"
    );
    Ok(())
}

#[test]
fn added_children_stale_summaries_and_close_gates() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;
    let package = repo.path().join("packages/app");
    std::fs::create_dir_all(package.join("src"))?;
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"@fixture/app\",\"version\":\"1.0.0\"}\n",
    )?;
    std::fs::write(
        package.join("src/flow.ts"),
        "export function finish() { return 1; }\n\
         export function start() { return finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let file = "packages/app/src/flow.ts";

    // Card A, then a file summary that covers exactly it.
    let mut cards = card_options();
    cards.anchors = vec![format!("{file}:start")];
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let card_a = scout_one_card(repo.path(), &conn, &mut card_gateway, &cards)?;
    let card_a_id = card_a.artifact_id.expect("card A");

    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the package entry point that delegates to its finisher",
        &["C1"],
    )))]);
    let published = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(published.reports[0].status, "completed");
    let summary_id = published.reports[0].artifact_id.expect("file summary");

    let freshness = |id: i64| -> Result<String> {
        Ok(crate::semantic::load_artifact(&conn, id)?
            .expect("artifact exists")
            .freshness)
    };
    assert_eq!(freshness(summary_id)?, "fresh");
    let before = super::plan::summaries(repo.path(), &conn, "module", &[])?;
    assert_eq!(
        before.items.len(),
        1,
        "the module plans before the addition"
    );
    assert!(before.skipped.is_empty());

    // Card B lands on the same file. Card A is untouched, the source is
    // untouched, and the snapshot does not move: only the scope's child
    // set grew.
    let finish_anchor =
        crate::structural::resolve_current_anchor(&conn, &format!("{file}:finish"))?;
    let card_b = crate::semantic::annotate(
        repo.path(),
        &conn,
        &crate::semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(finish_anchor.clone()),
            body: json!({ "purpose": "terminal helper that completes the flow" }),
            supports: vec![crate::semantic::SupportInput {
                claim_path: "/purpose".into(),
                anchor: finish_anchor,
                role: None,
                evidence_file: file.into(),
                evidence_start_line: 1,
                evidence_end_line: 1,
                confidence: "likely".into(),
            }],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(&conn)?,
            supersedes: None,
        },
    )?;
    let card_b_id = card_b.id;

    // (a) The summary no longer covers its scope, so it is stale even
    // though every dependency it stored is intact.
    assert_eq!(freshness(card_a_id)?, "fresh", "card A is untouched");
    assert_eq!(
        freshness(summary_id)?,
        "stale",
        "a child added after publication stales the summary"
    );

    // (b) The module refuses to build on the now-incomplete file summary.
    let gated = super::plan::summaries(repo.path(), &conn, "module", &[])?;
    assert!(gated.items.is_empty(), "no module over stale coverage");
    assert_eq!(gated.skipped.len(), 1);
    assert_eq!(gated.skipped[0].scope, "module:@fixture/app");
    assert!(
        gated.skipped[0].reason.contains(file)
            && gated.skipped[0]
                .reason
                .contains("no longer covers its current child set"),
        "unexpected gate reason: {}",
        gated.skipped[0].reason
    );

    // (c) Refresh selects it and republishes over the full child set.
    let selection = super::refresh::select(&conn, &[])?;
    assert_eq!(selection.targets.len(), 1, "only the summary is non-fresh");
    assert_eq!(selection.targets[0].artifact_id, summary_id);
    assert_eq!(selection.targets[0].config.kind(), "summary");

    let mut refresh_gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the package entry point and the terminal helper it delegates to",
        &["C1", "C2"],
    )))]);
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut refresh_gateway,
        selection,
        RequestPolicy::new(30, 1, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 1);
    assert_eq!(batch.reports.len(), 1);
    assert_eq!(batch.reports[0].status, "completed");
    assert_eq!(batch.reports[0].candidate_count, 2, "both children planned");
    let successor = batch.reports[0].artifact_id.expect("refreshed summary");
    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [successor],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(summary_id));
    assert_eq!(current_summaries(&conn)?, vec![successor]);

    let relations = relations_of(&conn, successor)?;
    let both = std::collections::BTreeSet::from([card_a_id, card_b_id]);
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.0 == "/overview")
            .map(|relation| relation.2)
            .collect::<std::collections::BTreeSet<_>>(),
        both,
        "both children are cited"
    );
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.0.is_empty())
            .map(|relation| relation.2)
            .collect::<std::collections::BTreeSet<_>>(),
        both,
        "both children are input dependencies"
    );
    assert_eq!(relations.len(), 4, "claim plus input-dep row per child");
    assert_eq!(
        freshness(successor)?,
        "fresh",
        "the successor covers the current child set"
    );

    // (d) With coverage restored the module gate opens again.
    let reopened = super::plan::summaries(repo.path(), &conn, "module", &[])?;
    assert!(reopened.skipped.is_empty(), "the gate opens");
    assert_eq!(reopened.items.len(), 1);
    assert_eq!(reopened.items[0].scope, "module:@fixture/app");
    assert_eq!(reopened.items[0].child_count, 1);
    Ok(())
}

#[test]
fn mixed_refresh_ends_with_every_current_artifact_fresh() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let card = seed_card(repo.path(), &conn)?;
    let card_id = card.artifact_id.expect("seeded card");
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    let published = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    let summary_id = published.reports[0].artifact_id.expect("published summary");

    // One source change stales the card and, through it, the summary.
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 2; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let freshness = |id: i64| -> Result<String> {
        Ok(crate::semantic::load_artifact(&conn, id)?
            .expect("artifact exists")
            .freshness)
    };
    assert_ne!(freshness(card_id)?, "fresh");
    assert_ne!(freshness(summary_id)?, "fresh");

    let selection = super::refresh::select(&conn, &[])?;
    assert_eq!(selection.targets.len(), 2, "card and summary both selected");

    // Dependency order guarantees the card successor publishes first, so
    // the summary re-plans against it rather than reusing its own run.
    let mut refresh_gateway = FakeGateway::new(vec![
        Ok(card_outcome(card_submission())),
        Ok(summary_outcome(summary_submission(
            "hosts the revised settlement entry point and its terminal helper",
            &["C1"],
        ))),
    ]);
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut refresh_gateway,
        selection,
        RequestPolicy::new(30, 2, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 2);
    assert_eq!(batch.reports.len(), 2);
    assert_eq!(batch.reports[0].kind, "card", "children refresh first");
    assert_eq!(batch.reports[1].kind, "summary");
    assert!(
        batch
            .reports
            .iter()
            .all(|report| report.status == "completed"),
        "no target may reuse a stale run"
    );

    let successor = batch.reports[1].artifact_id.expect("refreshed summary");
    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [successor],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(summary_id));
    assert_eq!(current_summaries(&conn)?, vec![successor]);

    // The acceptance condition: refresh converges: nothing current is left
    // stale or degraded.
    let current: Vec<(i64, String)> = {
        let mut statement = conn.prepare(
            "SELECT artifact.id, artifact.artifact_type FROM semantic_artifacts artifact
             WHERE NOT EXISTS(
               SELECT 1 FROM semantic_artifacts successor
               WHERE successor.supersedes_artifact_id=artifact.id
             )
             ORDER BY artifact.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    assert_eq!(current.len(), 2, "one current card and one current summary");
    for (id, artifact_type) in current {
        assert_eq!(
            freshness(id)?,
            "fresh",
            "current {artifact_type} {id} is still not fresh after refresh"
        );
    }
    Ok(())
}

#[test]
fn publishes_a_file_summary_with_pinned_child_relations() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let card = seed_card(repo.path(), &conn)?;
    let card_id = card.artifact_id.expect("seeded card");
    let card_fingerprint = artifact_fingerprint(&conn, card_id)?;

    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    let batch = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(batch.reports.len(), 1, "one file scope has one child");
    assert_eq!(batch.model_calls, 1);
    let report = &batch.reports[0];
    assert_eq!(report.status, "completed");
    assert_eq!(report.kind, "summary");
    assert_eq!(report.subject, "file:flow.ts");
    assert_eq!(report.candidate_count, 1, "one cited child");
    let summary_id = report.artifact_id.expect("published summary");

    let (artifact_type, name, prompt_version, confidence): (String, String, String, String) = conn
        .query_row(
            "SELECT artifact_type, canonical_name, prompt_version, confidence
             FROM semantic_artifacts WHERE id=?1",
            [summary_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(artifact_type, "summary");
    assert_eq!(name, "file:flow.ts");
    assert_eq!(prompt_version, "summary-scout/v1");
    assert_eq!(confidence, "likely", "generated claims never exceed likely");

    let relations = relations_of(&conn, summary_id)?;
    assert_eq!(
        relations.len(),
        2,
        "one claim citation plus one whole-artifact input dependency"
    );
    let claim = relations
        .iter()
        .find(|relation| relation.0 == "/overview")
        .expect("claim relation");
    assert_eq!(
        (
            claim.1.as_str(),
            claim.2,
            claim.3.as_str(),
            claim.4.as_str()
        ),
        ("summarizes", card_id, card_fingerprint.as_str(), "likely"),
        "the citation is pinned to the child's artifact fingerprint"
    );
    let input_dependency = relations
        .iter()
        .find(|relation| relation.0.is_empty())
        .expect("every planned child is an input dependency");
    assert_eq!(
        (input_dependency.2, input_dependency.3.as_str()),
        (card_id, card_fingerprint.as_str())
    );

    // A summary's evidence is its children, not source spans.
    let supports: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_supports WHERE artifact_id=?1",
        [summary_id],
        |row| row.get(0),
    )?;
    assert_eq!(supports, 0);

    let config: super::SummaryRunConfig = serde_json::from_str(&conn.query_row(
        "SELECT config_json FROM scout_runs WHERE id=?1",
        [report.run_id],
        |row| row.get::<_, String>(0),
    )?)?;
    assert_eq!(config.level, "file");
    assert_eq!(config.scope, "file:flow.ts");
    Ok(())
}

#[test]
fn staged_run_builds_module_summaries_from_file_summaries_it_just_published() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;
    let package = repo.path().join("packages/app");
    std::fs::create_dir_all(package.join("src"))?;
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"@fixture/app\",\"version\":\"1.0.0\"}\n",
    )?;
    std::fs::write(
        package.join("src/flow.ts"),
        "export function finish() { return 1; }\n\
         export function start() { return finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut card_options = card_options();
    card_options.anchors = vec!["packages/app/src/flow.ts:start".into()];
    let mut card_gateway = FakeGateway::new(vec![Ok(card_outcome(card_submission()))]);
    let card = scout_one_card(repo.path(), &conn, &mut card_gateway, &card_options)?;
    assert_eq!(card.status, "completed");
    let card_id = card.artifact_id.expect("seeded card");

    // Two calls: the file summary, then the module summary planned from
    // the file summary this same invocation just published. The repository
    // level is planned too but has no budget left, so it never calls.
    let mut gateway = FakeGateway::new(vec![
        Ok(summary_outcome(summary_submission(
            "hosts the package entry point that delegates to its finisher",
            &["C1"],
        ))),
        Ok(summary_outcome(summary_submission(
            "the app package exposes a single settlement entry point",
            &["C1"],
        ))),
    ]);
    let batch =
        super::scout_summaries(repo.path(), &conn, &mut gateway, &summary_options(None, 2))?;
    assert_eq!(batch.model_calls, 2);
    assert_eq!(
        batch.reports.len(),
        2,
        "file then module; repository has no budget left"
    );
    assert_eq!(batch.skipped_for_call_budget, 1, "the repository scope");
    assert!(
        batch
            .reports
            .iter()
            .all(|report| report.status == "completed")
    );
    assert_eq!(batch.reports[0].subject, "file:packages/app/src/flow.ts");
    assert_eq!(batch.reports[1].subject, "module:@fixture/app");

    let file_summary = batch.reports[0].artifact_id.expect("file summary");
    let module_summary = batch.reports[1].artifact_id.expect("module summary");
    assert_eq!(
        relations_of(&conn, file_summary)?
            .iter()
            .map(|relation| relation.2)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([card_id])
    );
    let module_relations = relations_of(&conn, module_summary)?;
    assert_eq!(
        module_relations.len(),
        2,
        "claim citation plus input dependency"
    );
    assert!(
        module_relations
            .iter()
            .all(|relation| relation.2 == file_summary
                && relation.3 == artifact_fingerprint(&conn, file_summary).expect("fingerprint")),
        "the module summary cites the file summary published in this invocation"
    );
    Ok(())
}

#[test]
fn summary_reuse_survives_unrelated_changes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    seed_card(repo.path(), &conn)?;
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    let first = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    let summary_id = first.reports[0].artifact_id.expect("published summary");

    // A new file moves the structural snapshot but leaves this scope's
    // children untouched: the summary fingerprint is snapshot-free, so the
    // completed run is reused instead of respent.
    std::fs::write(
        repo.path().join("unrelated.ts"),
        "export const noise = 1;\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut idle_gateway = FakeGateway::new(Vec::new());
    let second = super::scout_summaries(
        repo.path(),
        &conn,
        &mut idle_gateway,
        &summary_options(Some("file"), 1),
    )?;
    assert_eq!(second.reports.len(), 1);
    assert_eq!(second.reports[0].status, "reused");
    assert_eq!(second.reports[0].artifact_id, Some(summary_id));
    assert_eq!(second.model_calls, 0);
    assert_eq!(idle_gateway.calls, 0);
    assert_eq!(
        current_summaries(&conn)?,
        vec![summary_id],
        "reuse yields the one current summary, never a duplicate"
    );
    Ok(())
}

#[test]
fn summary_publication_loses_the_child_race_without_a_partial_write() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let card = seed_card(repo.path(), &conn)?;
    let card_id = card.artifact_id.expect("seeded card");
    let subject = card.subject.clone();

    // Mid-flight the child card is superseded by an agent annotation on a
    // second connection. The repository itself does not change, so the
    // snapshot gate passes and the child-currency recheck is what refuses.
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    gateway.on_complete = Some(Box::new(move || {
        let racing = store::open_path(&db_path).expect("open racing connection");
        let snapshot =
            crate::structural::current_snapshot(&racing).expect("racing current snapshot");
        crate::semantic::annotate(
            &root,
            &racing,
            &crate::semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(subject.clone()),
                body: json!({ "purpose": "revised entry point for the settlement flow" }),
                supports: vec![crate::semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: subject.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 2,
                    evidence_end_line: 2,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot,
                supersedes: Some(card_id),
            },
        )
        .expect("racing successor card");
    }));

    let error = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )
    .expect_err("the cited child stopped being current");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("publication recheck failed")
            && rendered.contains(&format!("child artifact {card_id} changed")),
        "unexpected failure: {rendered}"
    );

    let (status, code): (String, String) = conn.query_row(
        "SELECT status, error_code FROM scout_runs WHERE scout_kind='summary'
         ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("incomplete", "publication_recheck")
    );
    let summaries: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='summary'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(summaries, 0, "no partial write against a moved child");
    let relations: i64 = conn.query_row("SELECT count(*) FROM semantic_relations", [], |row| {
        row.get(0)
    })?;
    assert_eq!(relations, 0);
    Ok(())
}

#[test]
fn summary_publication_refuses_a_child_added_mid_flight() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    seed_card(repo.path(), &conn)?;

    // Planning sees only the start card. While the model call is in
    // flight, an agent adds a second current card to the same file. No
    // source or structural snapshot changes, and the original child is
    // untouched, so only a complete child-set recheck can catch it.
    let finish_anchor = crate::structural::resolve_current_anchor(&conn, "flow.ts:finish")?;
    let root = repo.path().to_path_buf();
    let db_path = store::db_path(repo.path());
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    gateway.on_complete = Some(Box::new(move || {
        let racing = store::open_path(&db_path).expect("open racing connection");
        let snapshot =
            crate::structural::current_snapshot(&racing).expect("racing current snapshot");
        crate::semantic::annotate(
            &root,
            &racing,
            &crate::semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(finish_anchor.clone()),
                body: json!({ "purpose": "terminal helper that completes the flow" }),
                supports: vec![crate::semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: finish_anchor.clone(),
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 1,
                    evidence_end_line: 1,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot,
                supersedes: None,
            },
        )
        .expect("racing additional card");
    }));

    let error = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )
    .expect_err("the scope gained a child during completion");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("publication recheck failed")
            && rendered
                .contains("summary child set changed during publication (planned 1, current 2)"),
        "unexpected failure: {rendered}"
    );

    let (status, code): (String, String) = conn.query_row(
        "SELECT status, error_code FROM scout_runs WHERE scout_kind='summary'
         ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("incomplete", "publication_recheck")
    );
    let summaries: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='summary'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(summaries, 0, "no partial summary write");
    let cards: i64 = conn.query_row(
        "SELECT count(*) FROM semantic_artifacts WHERE artifact_type='card'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(cards, 2, "the racing child committed independently");
    Ok(())
}

#[test]
fn summary_refresh_replaces_a_child_stale_summary() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = fixture(repo.path())?;
    let card = seed_card(repo.path(), &conn)?;
    let card_id = card.artifact_id.expect("seeded card");
    let mut gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the settlement entry point and the terminal helper it calls",
        &["C1"],
    )))]);
    let first = super::scout_summaries(
        repo.path(),
        &conn,
        &mut gateway,
        &summary_options(Some("file"), 1),
    )?;
    let summary_id = first.reports[0].artifact_id.expect("published summary");
    let summary_run = first.reports[0].run_id;

    // The child drifts and is superseded: the summary's pinned fingerprint
    // no longer names a current artifact, so it stales without its own
    // text or supports changing.
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 2; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let successor_card = crate::semantic::annotate(
        repo.path(),
        &conn,
        &crate::semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(card.subject.clone()),
            body: json!({ "purpose": "revised entry point for the settlement flow" }),
            supports: vec![crate::semantic::SupportInput {
                claim_path: "/purpose".into(),
                anchor: card.subject.clone(),
                role: None,
                evidence_file: "flow.ts".into(),
                evidence_start_line: 2,
                evidence_end_line: 2,
                confidence: "likely".into(),
            }],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(&conn)?,
            supersedes: Some(card_id),
        },
    )?;

    let selection = super::refresh::select(&conn, &[])?;
    assert_eq!(
        selection.targets.len(),
        1,
        "only the summary is refreshable"
    );
    assert_eq!(selection.targets[0].artifact_id, summary_id);
    assert_eq!(selection.targets[0].config.kind(), "summary");
    assert_eq!(selection.targets[0].freshness, "stale");

    let mut refresh_gateway = FakeGateway::new(vec![Ok(summary_outcome(summary_submission(
        "hosts the revised settlement entry point and its terminal helper",
        &["C1"],
    )))]);
    let batch = scout_refresh(
        repo.path(),
        &conn,
        &mut refresh_gateway,
        selection,
        RequestPolicy::new(30, 1, 240_000)?,
    )?;
    assert_eq!(batch.model_calls, 1);
    assert_eq!(batch.reports.len(), 1);
    assert_eq!(batch.reports[0].status, "completed");
    assert_eq!(batch.reports[0].kind, "summary");
    let successor = batch.reports[0].artifact_id.expect("refreshed summary");

    let supersedes: Option<i64> = conn.query_row(
        "SELECT supersedes_artifact_id FROM semantic_artifacts WHERE id=?1",
        [successor],
        |row| row.get(0),
    )?;
    assert_eq!(supersedes, Some(summary_id));
    assert_eq!(
        current_summaries(&conn)?,
        vec![successor],
        "the successor is the sole current summary for the scope"
    );
    let retired: String = conn.query_row(
        "SELECT status FROM scout_runs WHERE id=?1",
        [summary_run],
        |row| row.get(0),
    )?;
    assert_eq!(
        retired, "superseded",
        "superseding a summary retires its generating run"
    );

    // The replacement is grounded on the child that is current NOW.
    let relations = relations_of(&conn, successor)?;
    assert_eq!(relations.len(), 2, "claim citation plus input dependency");
    assert!(
        relations
            .iter()
            .all(|relation| relation.2 == successor_card.id
                && relation.3
                    == artifact_fingerprint(&conn, successor_card.id).expect("fingerprint"))
    );
    Ok(())
}
