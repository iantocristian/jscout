use anyhow::Result;

use super::{
    NewClassification, SubjectSelector, build_scope_state, file_policy_by_path, path_in_scope,
    persist_classification, reconcile_file_policy,
};
use crate::scouting::ledger::{RunClaim, RunOutcome, RunSpec};
use crate::{scouting::ledger, store};

fn run(conn: &rusqlite::Connection, fingerprint: &str) -> Result<i64> {
    let spec = RunSpec {
        scout_kind: "repository".into(),
        gateway_protocol: 1,
        provider: "openai-codex".into(),
        model: "gpt-5.6-terra".into(),
        billing_path: "plan".into(),
        reasoning: None,
        prompt_version: "repository-recon/v2".into(),
        source_snapshot: "snapshot".into(),
        input_fingerprint: fingerprint.into(),
        request_hash: "request".into(),
        config_json: "{}".into(),
        supersedes_artifact_id: None,
    };
    let RunClaim::Claimed { run_id, .. } = ledger::claim_run(conn, &spec, false)? else {
        panic!("new run");
    };
    Ok(run_id)
}

fn classify(
    conn: &rusqlite::Connection,
    state: &super::SubjectState,
    parent_subject_key: Option<&str>,
    depth: usize,
    role: &str,
    confidence: &str,
    fingerprint: &str,
) -> Result<i64> {
    let run_id = run(conn, fingerprint)?;
    let classification_id = persist_classification(
        conn,
        &NewClassification {
            run_id,
            subject_key: &state.subject_key,
            subject_kind: "area",
            selector: &state.selector,
            parent_subject_key,
            depth,
            role,
            confidence,
            explanation: "test classification",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &state.evidence_fingerprint,
            classification_fingerprint: fingerprint,
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(conn, run_id, RunOutcome::Completed, None, None)?;
    Ok(classification_id)
}

#[test]
fn scope_matching_is_literal_and_root_direct_is_bounded() {
    assert!(path_in_scope("src/a.ts", "src", false));
    assert!(!path_in_scope("src2/a.ts", "src", false));
    assert!(path_in_scope("root.ts", ".", true));
    assert!(!path_in_scope("src/a.ts", ".", true));
}

#[test]
fn documentation_rows_do_not_change_code_scope_evidence() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let selector = SubjectSelector::RepositoryArea {
        scope: ".".into(),
        direct_only: false,
    };
    let before = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:.".into(),
        selector.clone(),
    )?;

    conn.execute_batch(
        "INSERT INTO files(id,path,hash,role,origin)
           VALUES(100,'README.md','docs','documentation','repository');
         INSERT INTO chunks(
           id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(100,100,'markdown_section',NULL,'','',0,8,1,1,'doc-chunk','# Guide');
         INSERT INTO doc_chunk_meta(
           chunk_id,title,breadcrumb,nearest_heading,ordinal,
           embedding_identity,front_matter_state
         ) VALUES(100,'Guide','Guide','Guide',0,'identity','absent');",
    )?;
    let after = build_scope_state(repo.path(), &conn, "area:repository:.".into(), selector)?;

    assert_eq!(after.members, before.members);
    assert_eq!(after.evidence_fingerprint, before.evidence_fingerprint);
    Ok(())
}

#[test]
fn policy_reuses_exact_subject_evidence_and_stales_only_changed_scope() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("docs"))?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    std::fs::write(
        repo.path().join("docs/read.ts"),
        "export const guide = 1;\n",
    )?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;

    let docs_selector = SubjectSelector::RepositoryArea {
        scope: "docs".into(),
        direct_only: false,
    };
    let docs = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:docs".into(),
        docs_selector.clone(),
    )?;
    let run_id = run(&conn, "classification-1")?;
    persist_classification(
        &conn,
        &NewClassification {
            run_id,
            subject_key: &docs.subject_key,
            subject_kind: "area",
            selector: &docs_selector,
            parent_subject_key: None,
            depth: 0,
            role: "documentation",
            confidence: "likely",
            explanation: "guide source",
            citations_json: "[\"aggregate:file-kinds\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &docs.evidence_fingerprint,
            classification_fingerprint: "classification-1",
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(&conn, run_id, RunOutcome::Completed, None, None)?;
    assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
    assert_eq!(
        file_policy_by_path(&conn, "docs/read.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );

    // A real reindex after an edit outside docs leaves the exact docs
    // evidence fingerprint current.
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 2;\n")?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
    assert!(file_policy_by_path(&conn, "docs/read.ts")?.is_some());

    // A real membership change inside docs makes the classification
    // neutral without deleting its immutable history.
    std::fs::write(
        repo.path().join("docs/new.ts"),
        "export const anotherGuide = 1;\n",
    )?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 0);
    assert!(file_policy_by_path(&conn, "docs/read.ts")?.is_none());

    // Returning to the original branch content reactivates the old exact
    // classification without another model call.
    std::fs::remove_file(repo.path().join("docs/new.ts"))?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
    assert_eq!(
        file_policy_by_path(&conn, "docs/read.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );

    // A newer neutral answer for the same exact evidence must shadow the
    // older actionable answer. Filtering for `likely` before choosing the
    // newest row would incorrectly reactivate stale policy here.
    let neutral_run_id = run(&conn, "classification-neutral")?;
    persist_classification(
        &conn,
        &NewClassification {
            run_id: neutral_run_id,
            subject_key: &docs.subject_key,
            subject_kind: "area",
            selector: &docs_selector,
            parent_subject_key: None,
            depth: 0,
            role: "mixed",
            confidence: "possible",
            explanation: "evidence is mixed",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &docs.evidence_fingerprint,
            classification_fingerprint: "classification-neutral",
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(&conn, neutral_run_id, RunOutcome::Completed, None, None)?;
    assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 0);
    assert!(file_policy_by_path(&conn, "docs/read.ts")?.is_none());
    Ok(())
}

#[test]
fn scope_freshness_tracks_membership_and_only_bounded_representative_content() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    for index in 0..10 {
        std::fs::write(
            repo.path().join(format!("src/{index:02}.ts")),
            format!("export const value{index} = {index};\n"),
        )?;
    }
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let selector = SubjectSelector::RepositoryArea {
        scope: "src".into(),
        direct_only: false,
    };
    let initial = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:src".into(),
        selector.clone(),
    )?;

    // With ten members and an eight-file spread, 04.ts is not in the
    // bounded evidence sample and therefore must not create recurring
    // model cost for evidence the model never saw.
    std::fs::write(
        repo.path().join("src/04.ts"),
        "export const value4 = 400;\n",
    )?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let unselected_edit = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:src".into(),
        selector.clone(),
    )?;
    assert_eq!(
        unselected_edit.evidence_fingerprint,
        initial.evidence_fingerprint
    );

    std::fs::write(
        repo.path().join("src/05.ts"),
        "export const value5 = 500;\n",
    )?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let selected_edit =
        build_scope_state(repo.path(), &conn, "area:repository:src".into(), selector)?;
    assert_ne!(
        selected_edit.evidence_fingerprint,
        initial.evidence_fingerprint
    );
    Ok(())
}

#[test]
fn index_publishes_l1_and_clears_policy_when_optional_reconciliation_fails() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let state = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:src".into(),
        SubjectSelector::RepositoryArea {
            scope: "src".into(),
            direct_only: false,
        },
    )?;
    let classification_id = classify(&conn, &state, None, 0, "runtime", "likely", "valid-policy")?;
    reconcile_file_policy(repo.path(), &conn)?;
    assert!(file_policy_by_path(&conn, "src/run.ts")?.is_some());

    // Corrupt durable policy metadata stands in for any optional-plane
    // read/reconciliation failure. The subsequent structural index still
    // succeeds and removes the projection instead of serving old policy.
    conn.execute(
        "UPDATE repository_classifications SET selector_json='{' WHERE id=?1",
        [classification_id],
    )?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 2;\n")?;
    let outcome = crate::indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(outcome.rejected, 0);
    assert!(file_policy_by_path(&conn, "src/run.ts")?.is_none());
    Ok(())
}

#[test]
fn definite_parent_suppresses_old_descendants_until_parent_is_mixed_again() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("mixed/docs"))?;
    std::fs::write(
        repo.path().join("mixed/runtime.ts"),
        "export const runtime = 1;\n",
    )?;
    std::fs::write(
        repo.path().join("mixed/docs/guide.ts"),
        "export const guide = 1;\n",
    )?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let parent = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:mixed".into(),
        SubjectSelector::RepositoryArea {
            scope: "mixed".into(),
            direct_only: false,
        },
    )?;
    let child = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:mixed/docs".into(),
        SubjectSelector::RepositoryArea {
            scope: "mixed/docs".into(),
            direct_only: false,
        },
    )?;
    classify(&conn, &parent, None, 0, "mixed", "likely", "parent-mixed")?;
    classify(
        &conn,
        &child,
        Some(&parent.subject_key),
        1,
        "documentation",
        "likely",
        "child-docs",
    )?;
    reconcile_file_policy(repo.path(), &conn)?;
    assert_eq!(
        file_policy_by_path(&conn, "mixed/docs/guide.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );

    classify(
        &conn,
        &parent,
        None,
        0,
        "runtime",
        "likely",
        "parent-runtime",
    )?;
    reconcile_file_policy(repo.path(), &conn)?;
    assert_eq!(
        file_policy_by_path(&conn, "mixed/docs/guide.ts")?
            .unwrap()
            .effective_role,
        "runtime"
    );

    classify(
        &conn,
        &parent,
        None,
        0,
        "mixed",
        "likely",
        "parent-mixed-again",
    )?;
    reconcile_file_policy(repo.path(), &conn)?;
    assert_eq!(
        file_policy_by_path(&conn, "mixed/docs/guide.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );
    Ok(())
}

#[test]
fn fresh_scope_policy_overrides_ambiguous_doc_path_heuristics_for_workflows() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("doc"))?;
    std::fs::create_dir_all(repo.path().join("docs"))?;
    std::fs::write(
        repo.path().join("doc/guide.ts"),
        "export function renderGuide() { return 'guide'; }\n",
    )?;
    std::fs::write(
        repo.path().join("docs/runtime.ts"),
        "export function finish() { return 1; }\n\
         export function start() { return finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let deterministic: String = conn.query_row(
        "SELECT role FROM files WHERE path='docs/runtime.ts'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(deterministic, "documentation");

    let selector = SubjectSelector::RepositoryArea {
        scope: "docs".into(),
        direct_only: false,
    };
    let state = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:docs".into(),
        selector.clone(),
    )?;
    let run_id = run(&conn, "runtime-docs")?;
    persist_classification(
        &conn,
        &NewClassification {
            run_id,
            subject_key: &state.subject_key,
            subject_kind: "area",
            selector: &selector,
            parent_subject_key: None,
            depth: 0,
            role: "runtime",
            confidence: "likely",
            explanation: "runtime document-domain implementation",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &state.evidence_fingerprint,
            classification_fingerprint: "runtime-docs",
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(&conn, run_id, RunOutcome::Completed, None, None)?;

    let doc_selector = SubjectSelector::RepositoryArea {
        scope: "doc".into(),
        direct_only: false,
    };
    let doc_state = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:doc".into(),
        doc_selector.clone(),
    )?;
    let doc_run_id = run(&conn, "documentation-doc")?;
    persist_classification(
        &conn,
        &NewClassification {
            run_id: doc_run_id,
            subject_key: &doc_state.subject_key,
            subject_kind: "area",
            selector: &doc_selector,
            parent_subject_key: None,
            depth: 0,
            role: "documentation",
            confidence: "likely",
            explanation: "reader-facing guide implementation",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &doc_state.evidence_fingerprint,
            classification_fingerprint: "documentation-doc",
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(&conn, doc_run_id, RunOutcome::Completed, None, None)?;
    reconcile_file_policy(repo.path(), &conn)?;
    assert_eq!(
        file_policy_by_path(&conn, "docs/runtime.ts")?
            .unwrap()
            .effective_role,
        "runtime"
    );
    assert_eq!(
        file_policy_by_path(&conn, "doc/guide.ts")?
            .unwrap()
            .effective_role,
        "documentation"
    );

    let candidates = crate::semantic::workflow_candidates(
        repo.path(),
        &conn,
        &["docs/runtime.ts:start".into()],
        &crate::semantic::WorkflowCandidateOptions::default(),
    )?;
    assert!(
        candidates
            .candidates
            .iter()
            .any(|candidate| candidate.display_name == "start")
    );
    Ok(())
}

#[test]
fn runtime_scope_preserves_test_fixture_and_generated_artifact_roles() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("src/fixtures"))?;
    std::fs::create_dir_all(repo.path().join("src/generated"))?;
    std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
    std::fs::write(
        repo.path().join("src/run.test.ts"),
        "test('run', () => 1);\n",
    )?;
    std::fs::write(
        repo.path().join("src/fixtures/data.ts"),
        "export const fixture = 1;\n",
    )?;
    std::fs::write(
        repo.path().join("src/generated/schema.ts"),
        "// @generated\nexport const schema = 1;\n",
    )?;
    let conn = store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let selector = SubjectSelector::RepositoryArea {
        scope: "src".into(),
        direct_only: false,
    };
    let state = build_scope_state(
        repo.path(),
        &conn,
        "area:repository:src".into(),
        selector.clone(),
    )?;
    let run_id = run(&conn, "runtime-protected-artifacts")?;
    persist_classification(
        &conn,
        &NewClassification {
            run_id,
            subject_key: &state.subject_key,
            subject_kind: "area",
            selector: &selector,
            parent_subject_key: None,
            depth: 0,
            role: "runtime",
            confidence: "likely",
            explanation: "runtime scope with protected artifacts",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &state.evidence_fingerprint,
            classification_fingerprint: "runtime-protected-artifacts",
            source_snapshot: "snapshot",
        },
    )?;
    ledger::finish_run(&conn, run_id, RunOutcome::Completed, None, None)?;
    reconcile_file_policy(repo.path(), &conn)?;

    let cases = [
        ("src/run.ts", "runtime"),
        ("src/run.test.ts", "test"),
        ("src/fixtures/data.ts", "fixture"),
        ("src/generated/schema.ts", "generated"),
    ];
    for (path, expected) in cases {
        let policy = file_policy_by_path(&conn, path)?.expect("active scope policy");
        assert_eq!(policy.scope_role, "runtime");
        assert_eq!(policy.effective_role, expected);
    }
    let conflicts: i64 = conn.query_row(
        "SELECT conflict_files FROM repository_current_classifications
         WHERE subject_key='area:repository:src'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(conflicts, 3);
    Ok(())
}
