use std::fs;

use anyhow::Result;
use serde_json::json;

use super::{
    AnnotateInput, AnnotateRequest, SupportInput, WorkflowCandidateOptions, annotate,
    annotate_request_with_provider, search, support_relationship, workflow_candidates,
};
use crate::{indexer, store, structural};

fn support(claim_path: &str, anchor: &str, file: &str) -> SupportInput {
    SupportInput {
        claim_path: claim_path.into(),
        anchor: anchor.into(),
        role: None,
        evidence_file: file.into(),
        evidence_start_line: 1,
        evidence_end_line: 1,
        confidence: "likely".into(),
    }
}

fn publish_workflow(
    root: &std::path::Path,
    conn: &rusqlite::Connection,
    anchor: &str,
    name: &str,
    supersedes: Option<i64>,
) -> Result<super::SemanticArtifact> {
    annotate(
        root,
        conn,
        &AnnotateInput {
            artifact_type: "workflow".into(),
            name: Some(name.into()),
            body: json!({
                "participants": [{
                    "anchor": anchor,
                    "role": "establishes vocabulary",
                    "scope": "defining",
                }],
            }),
            supports: vec![
                support("/name", anchor, "domain.ts"),
                support("/participants/0/role", anchor, "domain.ts"),
            ],
            confidence: "likely".into(),
            snapshot: structural::current_snapshot(conn)?,
            supersedes,
        },
    )
}

fn publish_card(
    root: &std::path::Path,
    conn: &rusqlite::Connection,
    anchor: &str,
    terms: &[&str],
    supersedes: Option<i64>,
) -> Result<super::SemanticArtifact> {
    let mut supports = vec![support("/purpose", anchor, "domain.ts")];
    supports.extend(
        terms
            .iter()
            .enumerate()
            .map(|(index, _)| support(&format!("/domain_terms/{index}"), anchor, "domain.ts")),
    );
    annotate(
        root,
        conn,
        &AnnotateInput {
            artifact_type: "card".into(),
            name: Some(anchor.into()),
            body: json!({
                "purpose": "establishes repository vocabulary",
                "domain_terms": terms,
            }),
            supports,
            confidence: "likely".into(),
            snapshot: structural::current_snapshot(conn)?,
            supersedes,
        },
    )
}

fn artifact_fingerprint(conn: &rusqlite::Connection, id: i64) -> Result<String> {
    Ok(conn.query_row(
        "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
        [id],
        |row| row.get(0),
    )?)
}

fn persist_test_concept(
    root: &std::path::Path,
    conn: &rusqlite::Connection,
    anchor: &str,
    children: &[i64],
    whole_dependencies: bool,
) -> Result<i64> {
    let input = AnnotateInput {
        artifact_type: "concept".into(),
        name: Some("invoice settlement".into()),
        body: json!({
            "definition": "The repository-specific invoice settlement boundary",
            "aliases": ["Invoice Settlement"],
        }),
        supports: vec![
            support("/definition", anchor, "domain.ts"),
            support("/aliases/0", anchor, "domain.ts"),
        ],
        confidence: "likely".into(),
        snapshot: structural::current_snapshot(conn)?,
        supersedes: None,
    };
    let (snapshot, supports) = super::validate_annotate_input(root, conn, &input)?;
    let mut relations = Vec::new();
    for &child in children {
        let fingerprint = artifact_fingerprint(conn, child)?;
        relations.push(super::RelationInput {
            claim_path: "/definition".into(),
            relation: "related_to".into(),
            dst_artifact_id: child,
            dst_fingerprint: fingerprint.clone(),
            confidence: "likely".into(),
        });
        if whole_dependencies {
            relations.push(super::RelationInput {
                claim_path: String::new(),
                relation: "related_to".into(),
                dst_artifact_id: child,
                dst_fingerprint: fingerprint,
                confidence: "likely".into(),
            });
        }
    }
    super::persist_validated_artifact(
        conn,
        &input,
        &snapshot,
        &supports,
        &relations,
        &super::ArtifactProvenance {
            model: "test",
            prompt_version: "concept-scout/v1",
            scout_run_id: None,
            input_fingerprint: None,
        },
    )
}

#[test]
fn concept_semantic_shape_is_closed_normalized_and_supported() -> Result<()> {
    let base = AnnotateInput {
        artifact_type: "concept".into(),
        name: Some("invoice settlement".into()),
        body: json!({
            "definition": "The invoice settlement boundary in this repository",
            "aliases": ["Invoice Settlement", "invoice settlement"],
        }),
        supports: vec![
            support("/definition", "anchor", "domain.ts"),
            support("/aliases/0", "anchor", "domain.ts"),
            support("/aliases/1", "anchor", "domain.ts"),
        ],
        confidence: "likely".into(),
        snapshot: "snapshot".into(),
        supersedes: None,
    };
    super::validate_input(&base)?;

    let noncanonical_name = AnnotateInput {
        name: Some("Invoice  Settlement".into()),
        ..base.clone()
    };
    assert!(
        super::validate_input(&noncanonical_name)
            .unwrap_err()
            .to_string()
            .contains("current concept normalizer")
    );

    let near_alias = AnnotateInput {
        body: json!({
            "definition": "The invoice settlement boundary in this repository",
            "aliases": ["invoice-settlement"],
        }),
        supports: vec![
            support("/definition", "anchor", "domain.ts"),
            support("/aliases/0", "anchor", "domain.ts"),
        ],
        ..base.clone()
    };
    assert!(
        super::validate_input(&near_alias)
            .unwrap_err()
            .to_string()
            .contains("does not normalize")
    );

    let duplicate_display = AnnotateInput {
        body: json!({
            "definition": "The invoice settlement boundary in this repository",
            "aliases": ["Invoice\u{3000}Settlement", "Invoice Settlement"],
        }),
        ..base.clone()
    };
    assert!(
        super::validate_input(&duplicate_display)
            .unwrap_err()
            .to_string()
            .contains("duplicated")
    );

    let extra_field = AnnotateInput {
        body: json!({
            "definition": "The invoice settlement boundary in this repository",
            "aliases": ["Invoice Settlement", "invoice settlement"],
            "model_note": "not part of the persisted concept contract",
        }),
        supports: base
            .supports
            .iter()
            .cloned()
            .chain(std::iter::once(support(
                "/model_note",
                "anchor",
                "domain.ts",
            )))
            .collect(),
        ..base
    };
    assert!(
        super::validate_input(&extra_field)
            .unwrap_err()
            .to_string()
            .contains("exactly `definition` and `aliases`")
    );
    Ok(())
}

#[test]
fn expected_concept_children_use_only_exact_supported_current_vocabulary() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("domain.ts"),
        "export function alpha() { return 1; }\n\
         export function beta() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let alpha = "sym:domain.ts#::alpha@1";
    let beta = "sym:domain.ts#::beta@1";

    let old_workflow = publish_workflow(repo.path(), &conn, alpha, "Invoice Settlement", None)?;
    let card = publish_card(
        repo.path(),
        &conn,
        beta,
        &["invoice settlement", "invoice-settlement"],
        None,
    )?;
    annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("free prose".into()),
            body: json!({"claim": "invoice settlement appears only as prose"}),
            supports: vec![support("/claim", alpha, "domain.ts")],
            confidence: "likely".into(),
            snapshot: structural::current_snapshot(&conn)?,
            supersedes: None,
        },
    )?;

    // A prefix-like support path is not the exact card array-element
    // pointer admitted by deterministic planning.
    conn.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type,canonical_name,body_json,model,prompt_version,confidence,
           source_snapshot,created_at,artifact_fingerprint
         ) VALUES('card',?1,?2,'test','test/v1','likely',?3,
                  '2026-08-12T00:00:00Z','malformed-fingerprint')",
        rusqlite::params![
            alpha,
            json!({
                "purpose": "malformed historical row",
                "domain_terms": ["invoice settlement"],
            })
            .to_string(),
            structural::current_snapshot(&conn)?,
        ],
    )?;
    let malformed_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO semantic_supports(
           artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
           evidence_end_line,source_hash,context_hash,confidence
         ) VALUES(?1,'/domain_terms/0/nested',?2,'domain.ts',1,1,
                  'source','context','likely')",
        rusqlite::params![malformed_id, alpha],
    )?;

    assert_eq!(
        super::expected_concept_child_ids(&conn, "invoice settlement")?,
        [old_workflow.id, card.id].into_iter().collect()
    );
    assert_eq!(
        super::expected_concept_child_ids(&conn, "invoice-settlement")?,
        [card.id].into_iter().collect()
    );
    assert!(super::expected_concept_child_ids(&conn, "appears only as prose")?.is_empty());

    let replacement = publish_workflow(
        repo.path(),
        &conn,
        alpha,
        "Payment Completion",
        Some(old_workflow.id),
    )?;
    assert_eq!(
        super::expected_concept_child_ids(&conn, "invoice settlement")?,
        [card.id].into_iter().collect()
    );
    assert_eq!(
        super::expected_concept_child_ids(&conn, "payment completion")?,
        [replacement.id].into_iter().collect()
    );
    Ok(())
}

#[test]
fn concept_freshness_tracks_exact_whole_child_set_additions_and_removals() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("domain.ts"),
        "export function alpha() { return 1; }\n\
         export function beta() { return 2; }\n\
         export function gamma() { return 3; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let alpha = "sym:domain.ts#::alpha@1";
    let beta = "sym:domain.ts#::beta@1";
    let gamma = "sym:domain.ts#::gamma@1";
    let workflow = publish_workflow(repo.path(), &conn, alpha, "Invoice Settlement", None)?;

    let claim_only = persist_test_concept(repo.path(), &conn, alpha, &[workflow.id], false)?;
    assert!(
        !super::concept_child_set_current(&conn, claim_only, "invoice settlement")?,
        "claim relations do not substitute for whole-input dependencies"
    );

    let first = persist_test_concept(repo.path(), &conn, alpha, &[workflow.id], true)?;
    assert!(super::concept_child_set_current(
        &conn,
        first,
        "invoice settlement"
    )?);
    assert_eq!(
        super::load_artifact(&conn, first)?
            .expect("concept exists")
            .freshness,
        "fresh"
    );

    // Punctuation is preserved by the normalizer, so a near variant does
    // not widen this concept's child set.
    publish_card(repo.path(), &conn, beta, &["invoice-settlement"], None)?;
    assert_eq!(
        super::load_artifact(&conn, first)?
            .expect("concept exists")
            .freshness,
        "fresh"
    );

    let matching = publish_card(repo.path(), &conn, gamma, &["invoice settlement"], None)?;
    assert_eq!(
        super::load_artifact(&conn, first)?
            .expect("concept exists")
            .freshness,
        "stale",
        "a newly added matching child invalidates the old exhaustive set"
    );

    let second =
        persist_test_concept(repo.path(), &conn, alpha, &[workflow.id, matching.id], true)?;
    assert_eq!(
        super::load_artifact(&conn, second)?
            .expect("concept exists")
            .freshness,
        "fresh"
    );

    publish_card(
        repo.path(),
        &conn,
        gamma,
        &["invoice-settlement"],
        Some(matching.id),
    )?;
    assert_eq!(
        super::load_artifact(&conn, second)?
            .expect("concept exists")
            .freshness,
        "stale",
        "removing a matching current child invalidates the stored exhaustive set"
    );
    Ok(())
}

#[test]
fn semantic_search_does_not_hide_matches_older_than_two_hundred_rows() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("a.ts"), "export const a = 1;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    for index in 0..=200 {
        let body = if index == 0 {
            json!({ "claim": "the uniquely ancient needle" })
        } else {
            json!({ "claim": format!("ordinary memory {index}") })
        };
        conn.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type, canonical_name, body_json, model, prompt_version,
               confidence, source_snapshot, created_at, artifact_fingerprint
             ) VALUES('annotation', ?1, ?2, 'test', 'test/v1', 'likely', ?3,
                      strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?4)",
            rusqlite::params![
                format!("memory-{index}"),
                body.to_string(),
                snapshot,
                format!("fingerprint-{index}"),
            ],
        )?;
    }

    let found = search(&conn, "uniquely ancient needle", 4)?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name.as_deref(), Some("memory-0"));
    Ok(())
}

#[test]
fn summaries_degrade_and_stale_with_their_children() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let alpha = "sym:a.ts#::alpha@1";

    let card = annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "card".into(),
            name: Some(alpha.into()),
            body: json!({ "purpose": "starts the alpha flow" }),
            supports: vec![support("/purpose", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        },
    )?;
    let card_fingerprint: String = conn.query_row(
        "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
        [card.id],
        |row| row.get(0),
    )?;

    let summary_input = AnnotateInput {
        artifact_type: "summary".into(),
        name: Some("file:a.ts".into()),
        body: json!({
            "level": "file",
            "scope": "file:a.ts",
            "overview": "hosts the alpha entry point",
        }),
        supports: Vec::new(),
        confidence: "likely".into(),
        snapshot: snapshot.clone(),
        supersedes: None,
    };
    let (current_snapshot, supports) =
        super::validate_annotate_input(repo.path(), &conn, &summary_input)?;
    let parent_id = super::persist_validated_artifact(
        &conn,
        &summary_input,
        &current_snapshot,
        &supports,
        &[
            super::RelationInput {
                claim_path: "/overview".into(),
                relation: "summarizes".into(),
                dst_artifact_id: card.id,
                dst_fingerprint: card_fingerprint.clone(),
                confidence: "likely".into(),
            },
            // Real publication always records the whole-artifact input
            // dependency; the expected-child-set check requires it.
            super::RelationInput {
                claim_path: String::new(),
                relation: "summarizes".into(),
                dst_artifact_id: card.id,
                dst_fingerprint: card_fingerprint,
                confidence: "likely".into(),
            },
        ],
        &super::ArtifactProvenance {
            model: "test",
            prompt_version: "summary-scout/v1",
            scout_run_id: None,
            input_fingerprint: None,
        },
    )?;
    let freshness = |id: i64| -> Result<String> {
        Ok(super::load_artifact(&conn, id)?
            .expect("artifact exists")
            .freshness)
    };
    assert_eq!(freshness(parent_id)?, "fresh");

    // The child's own source drifts: the child is still current with the
    // pinned fingerprint but no longer fresh, so the parent degrades even
    // though its own text and relations are unchanged.
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 2; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_ne!(freshness(card.id)?, "fresh");
    assert_eq!(freshness(parent_id)?, "degraded");

    // The child is superseded: the parent's pinned fingerprint no longer
    // names a current artifact, so the parent is stale outright.
    let successor_snapshot = structural::current_snapshot(&conn)?;
    annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "card".into(),
            name: Some(alpha.into()),
            body: json!({ "purpose": "starts the revised alpha flow" }),
            supports: vec![support("/purpose", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot: successor_snapshot,
            supersedes: Some(card.id),
        },
    )?;
    assert_eq!(freshness(parent_id)?, "stale");
    Ok(())
}

#[test]
fn workflow_round_trips_with_evidence_and_degrades_after_supported_source_changes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    fs::write(
        repo.path().join("b.ts"),
        "export function beta() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let alpha = "sym:a.ts#::alpha@1";
    let beta = "sym:b.ts#::beta@1";
    let input = AnnotateInput {
        artifact_type: "workflow".into(),
        name: Some("handoff workflow".into()),
        body: json!({
            "participants": [
                { "anchor": alpha, "role": "starts handoff", "scope": "defining" },
                { "anchor": beta, "role": "finishes handoff", "scope": "supporting" }
            ]
        }),
        supports: vec![
            support("/name", alpha, "a.ts"),
            support("/participants/0/role", alpha, "a.ts"),
            support("/participants/1/role", beta, "b.ts"),
        ],
        confidence: "likely".into(),
        snapshot,
        supersedes: None,
    };
    let artifact = annotate(repo.path(), &conn, &input)?;
    assert_eq!(artifact.freshness, "fresh");
    assert_eq!(artifact.trust, "untrusted-semantic-memory");
    assert_eq!(artifact.model, "agent-reported");
    assert_eq!(artifact.prompt_version, "annotate/v2");
    assert_eq!(artifact.supports.len(), 3);
    assert_eq!(artifact.supports[0].relationship, "artifact-name-evidence");
    assert!(
        artifact
            .supports
            .iter()
            .any(|support| { support.relationship == "defining-participant-evidence" })
    );
    assert!(
        artifact
            .supports
            .iter()
            .any(|support| { support.relationship == "supporting-participant-evidence" })
    );
    assert_eq!(
        support_relationship(
            "workflow",
            &json!({ "participants": [{ "anchor": alpha, "role": "legacy" }] }),
            "/participants/0/role",
        ),
        "legacy-participant-evidence"
    );
    assert_eq!(search(&conn, "handoff", 4)?.len(), 1);

    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 3; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = search(&conn, "handoff", 4)?;
    assert_eq!(stale[0].freshness, "degraded");
    assert!(
        stale[0]
            .supports
            .iter()
            .any(|support| support.freshness == "source-stale")
    );
    assert!(
        stale[0]
            .supports
            .iter()
            .any(|support| support.freshness == "fresh")
    );
    Ok(())
}

#[test]
fn workflow_requires_unique_scoped_participants_and_one_defining_boundary() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let alpha = "sym:a.ts#::alpha@1";
    let snapshot = structural::current_snapshot(&conn)?;
    let make_input = |participants| AnnotateInput {
        artifact_type: "workflow".into(),
        name: Some("scoped workflow".into()),
        body: json!({ "participants": participants }),
        supports: vec![
            support("/name", alpha, "a.ts"),
            support("/participants/0/role", alpha, "a.ts"),
        ],
        confidence: "likely".into(),
        snapshot: snapshot.clone(),
        supersedes: None,
    };

    let missing_scope = make_input(json!([
        { "anchor": alpha, "role": "entry" }
    ]));
    assert!(
        annotate(repo.path(), &conn, &missing_scope)
            .unwrap_err()
            .to_string()
            .contains("requires scope")
    );

    let only_supporting = make_input(json!([
        { "anchor": alpha, "role": "helper", "scope": "supporting" }
    ]));
    assert!(
        annotate(repo.path(), &conn, &only_supporting)
            .unwrap_err()
            .to_string()
            .contains("at least one defining participant")
    );

    let duplicate = AnnotateInput {
        body: json!({
            "participants": [
                { "anchor": alpha, "role": "entry", "scope": "defining" },
                { "anchor": alpha, "role": "helper", "scope": "supporting" }
            ]
        }),
        supports: vec![
            support("/name", alpha, "a.ts"),
            support("/participants/0/role", alpha, "a.ts"),
            support("/participants/1/role", alpha, "a.ts"),
        ],
        ..make_input(json!([]))
    };
    assert!(
        annotate(repo.path(), &conn, &duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicated")
    );
    Ok(())
}

#[test]
fn workflow_candidates_expand_ranked_production_symbols_and_fingerprint_the_set() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("entry.ts"),
        "import { middle } from './middle';\nexport function entry() { return middle(); }\n",
    )?;
    fs::write(
        repo.path().join("middle.ts"),
        "import { leaf } from './leaf';\nexport function middle() { return leaf(); }\n",
    )?;
    fs::write(
        repo.path().join("leaf.ts"),
        "export function leaf() { return 1; }\n",
    )?;
    fs::write(
        repo.path().join("entry.test.ts"),
        "export function testHelper() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let entry = "sym:entry.ts#::entry@1".to_string();
    let leaf = "sym:leaf.ts#::leaf@1".to_string();
    let options = WorkflowCandidateOptions::default();
    let result = workflow_candidates(repo.path(), &conn, &[entry.clone(), leaf.clone()], &options)?;
    assert_eq!(result.candidates.len(), 3);
    assert!(!result.traversal_truncated);
    assert!(!result.candidate_truncated);
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| !candidate.file.contains("test"))
    );
    assert!(result.candidates.iter().any(|candidate| {
        candidate.display_name == "middle" && candidate.evidence_end_line >= 2
    }));

    let reversed = workflow_candidates(repo.path(), &conn, &[leaf, entry], &options)?;
    assert_eq!(result.fingerprint, reversed.fingerprint);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| &candidate.anchor)
            .collect::<Vec<_>>(),
        reversed
            .candidates
            .iter()
            .map(|candidate| &candidate.anchor)
            .collect::<Vec<_>>(),
    );

    let limited = workflow_candidates(
        repo.path(),
        &conn,
        &["entry".into()],
        &WorkflowCandidateOptions {
            candidate_limit: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(limited.candidates.len(), 2);
    assert!(limited.candidate_truncated);

    let file_seed = workflow_candidates(repo.path(), &conn, &["entry.ts".into()], &options)
        .expect_err("file seeds must not silently choose an operation");
    assert!(
        file_seed
            .to_string()
            .contains("workflow seed must resolve to a symbol anchor")
    );
    Ok(())
}

#[test]
fn workflow_candidates_keep_seed_symbols_under_singular_doc_directories() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src/doc"))?;
    fs::write(
        repo.path().join("src/doc/job.ts"),
        "export function dispatchJob() { return handleJob(); }\n\
         export function handleJob() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let role: String = conn.query_row(
        "SELECT role FROM files WHERE path='src/doc/job.ts'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(role, "production");

    let seed = "sym:src/doc/job.ts#::dispatchJob@1";
    let candidates = workflow_candidates(
        repo.path(),
        &conn,
        &[seed.into()],
        &WorkflowCandidateOptions::default(),
    )?;
    let candidate = candidates
        .candidates
        .iter()
        .find(|candidate| candidate.anchor == seed)
        .expect("the production seed must remain in its own candidate set");
    assert!(candidate.seed);
    assert_eq!(candidate.file, "src/doc/job.ts");
    Ok(())
}

#[test]
fn docs_only_publication_preserves_semantic_code_guards() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let publication = crate::publication::current_publication_snapshot(&conn)?;
    let alpha = "sym:a.ts#::alpha@1";

    fs::write(
        repo.path().join("README.md"),
        "# Alpha\n\nAlpha is the workflow entry.\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);
    assert_ne!(
        crate::publication::current_publication_snapshot(&conn)?,
        publication
    );

    let candidates = workflow_candidates(
        repo.path(),
        &conn,
        &[alpha.into()],
        &WorkflowCandidateOptions {
            expected_snapshot: Some(snapshot.clone()),
            ..Default::default()
        },
    )?;
    assert_eq!(candidates.snapshot, snapshot);
    assert_eq!(
        candidates.publication_snapshot,
        crate::publication::Identities::read(&conn)?.publication
    );
    assert!(
        candidates
            .candidates
            .iter()
            .any(|candidate| candidate.anchor == alpha)
    );

    let artifact = annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("alpha behavior".into()),
            body: json!({ "claim": "alpha is the workflow entry" }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        },
    )?;
    assert_eq!(
        artifact.source_snapshot,
        crate::publication::durable_code_source(&snapshot)
    );

    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 2; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = workflow_candidates(
        repo.path(),
        &conn,
        &[alpha.into()],
        &WorkflowCandidateOptions {
            expected_snapshot: Some(snapshot),
            ..Default::default()
        },
    )
    .expect_err("a code edit must invalidate the old semantic guard");
    assert!(stale.to_string().contains("snapshot is stale"));
    Ok(())
}

#[test]
fn annotation_publication_echoes_the_identity_validated_by_its_write() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let identities = crate::publication::Identities::read(&conn)?;
    let anchor = "sym:a.ts#::alpha@1";

    let publication = annotate_request_with_provider(
        repo.path(),
        &conn,
        None,
        AnnotateRequest::Annotation {
            name: Some("alpha behavior".into()),
            body: json!({ "claim": "alpha is stable" }),
            supports: vec![support("/claim", anchor, "a.ts")],
            confidence: "likely".into(),
            snapshot: identities.code.clone(),
            supersedes: None,
        },
    )?;

    assert_eq!(publication.snapshot, identities.code);
    assert_eq!(publication.publication_snapshot, identities.publication);
    assert_eq!(
        publication.artifact.source_snapshot,
        crate::publication::durable_code_source(&publication.snapshot)
    );
    let rendered = serde_json::to_value(&publication)?;
    assert_eq!(rendered["snapshot"], publication.snapshot);
    assert_eq!(
        rendered["publication_snapshot"],
        publication.publication_snapshot
    );
    Ok(())
}

#[test]
fn annotate_rejects_untrusted_confidence_bad_spans_and_stale_snapshots() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let alpha = "sym:a.ts#::alpha@1";
    let base = AnnotateInput {
        artifact_type: "annotation".into(),
        name: Some("claim".into()),
        body: json!({ "claim": "alpha participates in a workflow" }),
        supports: vec![support("/claim", alpha, "a.ts")],
        confidence: "certain".into(),
        snapshot: structural::current_snapshot(&conn)?,
        supersedes: None,
    };
    assert!(
        annotate(repo.path(), &conn, &base)
            .unwrap_err()
            .to_string()
            .contains("likely")
    );

    let bad_span = AnnotateInput {
        confidence: "likely".into(),
        supports: vec![SupportInput {
            evidence_end_line: 99,
            ..support("/claim", alpha, "a.ts")
        }],
        ..base.clone()
    };
    assert!(
        annotate(repo.path(), &conn, &bad_span)
            .unwrap_err()
            .to_string()
            .contains("line count")
    );

    let publication_snapshot = AnnotateInput {
        confidence: "likely".into(),
        snapshot: crate::publication::current_publication_snapshot(&conn)?,
        supports: vec![support("/claim", alpha, "a.ts")],
        ..bad_span.clone()
    };
    assert_eq!(
        annotate(repo.path(), &conn, &publication_snapshot)
            .unwrap_err()
            .to_string(),
        "annotation snapshot is the current `publication_snapshot`; pass the code digest from the response's `snapshot` field"
    );

    let stale_snapshot = AnnotateInput {
        snapshot: "0".repeat(64),
        supports: vec![support("/claim", alpha, "a.ts")],
        ..bad_span
    };
    assert!(
        annotate(repo.path(), &conn, &stale_snapshot)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM semantic_artifacts", [], |row| {
        row.get(0)
    })?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn superseding_annotation_hides_prior_record_from_default_search() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let alpha = "sym:a.ts#::alpha@1";
    let first = annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("alpha behavior".into()),
            body: json!({ "claim": "alpha returns one" }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        },
    )?;
    let second = annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("alpha behavior".into()),
            body: json!({ "claim": "alpha is the handoff entry" }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot,
            supersedes: Some(first.id),
        },
    )?;
    let results = search(&conn, "alpha", 4)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, second.id);
    assert_eq!(results[0].supersedes, Some(first.id));
    Ok(())
}

#[test]
fn every_semantic_leaf_claim_requires_evidence_support() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function alpha() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let alpha = "sym:a.ts#::alpha@1";
    let result = annotate(
        repo.path(),
        &conn,
        &AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("alpha behavior".into()),
            body: json!({
                "claim": "alpha returns one",
                "unsupported_detail": "alpha also starts the handoff"
            }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "likely".into(),
            snapshot: structural::current_snapshot(&conn)?,
            supersedes: None,
        },
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("/unsupported_detail")
    );
    Ok(())
}
