use anyhow::Result;
use serde_json::json;

use super::{
    ArtifactViewMode, MAX_CONCEPT_TAG_LIMIT, QueryOptions, concept_tags, line_excerpt, query,
};
use crate::{indexer, semantic, store};

fn concept_fixture(
    repo: &std::path::Path,
    conn: &rusqlite::Connection,
    name: &str,
    anchor: &str,
    start_line: i64,
    end_line: i64,
    supersedes: Option<i64>,
) -> Result<semantic::SemanticArtifact> {
    semantic::annotate(
        repo,
        conn,
        &semantic::AnnotateInput {
            artifact_type: "concept".into(),
            name: Some(name.into()),
            body: json!({
                "definition": format!("Repository meaning of {name}"),
                "aliases": [name],
            }),
            supports: vec![
                semantic::SupportInput {
                    claim_path: "/definition".into(),
                    anchor: anchor.into(),
                    role: None,
                    evidence_file: "concept.ts".into(),
                    evidence_start_line: start_line,
                    evidence_end_line: end_line,
                    confidence: "likely".into(),
                },
                semantic::SupportInput {
                    claim_path: "/aliases/0".into(),
                    anchor: anchor.into(),
                    role: None,
                    evidence_file: "concept.ts".into(),
                    evidence_start_line: start_line,
                    evidence_end_line: end_line,
                    confidence: "likely".into(),
                },
            ],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(conn)?,
            supersedes,
        },
    )
}

fn replace_concept_chunks(conn: &rusqlite::Connection, spans: &[(i64, i64)]) -> Result<()> {
    let file_id: i64 =
        conn.query_row("SELECT id FROM files WHERE path='concept.ts'", [], |row| {
            row.get(0)
        })?;
    conn.execute("DELETE FROM chunks WHERE file_id=?1", [file_id])?;
    for (index, &(start_line, end_line)) in spans.iter().enumerate() {
        conn.execute(
            "INSERT INTO chunks(
               file_id, kind, name, scope_chain, symbols, start, end,
               start_line, end_line, hash, content
             ) VALUES(?1,'function',?2,'','',0,1,?3,?4,?5,'x')",
            rusqlite::params![
                file_id,
                format!("chunk_{index}"),
                start_line,
                end_line,
                format!("hash-{index}"),
            ],
        )?;
    }
    Ok(())
}

#[test]
fn semantic_query_filters_relates_and_drills_to_exact_source() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return 1; }\n\
         export function start() { return finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let start = crate::structural::resolve_current_anchor(&conn, "flow.ts:start")?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let card = semantic::annotate(
        repo.path(),
        &conn,
        &semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(start.clone()),
            body: json!({ "purpose": "starts the settlement flow" }),
            supports: vec![semantic::SupportInput {
                claim_path: "/purpose".into(),
                anchor: start,
                role: None,
                evidence_file: "flow.ts".into(),
                evidence_start_line: 2,
                evidence_end_line: 2,
                confidence: "likely".into(),
            }],
            confidence: "likely".into(),
            snapshot,
            supersedes: None,
        },
    )?;

    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "settlement".into(),
            artifact_types: vec!["card".into()],
            ..Default::default()
        },
    )?;
    assert_eq!(result.candidate_artifacts, 1);
    assert_eq!(result.mode, "discovery");
    assert_eq!(result.artifact_handles[0].id, card.id);
    assert!(result.artifact_handles[0].current);
    assert_eq!(result.artifact_handles[0].freshness, "fresh");
    assert!(result.semantic_artifacts.is_empty());
    assert!(result.source_evidence.is_empty());
    let detail = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(card.id),
            include_source: true,
            ..Default::default()
        },
    )?;
    assert_eq!(detail.mode, "artifact_detail");
    assert_eq!(detail.semantic_artifacts[0].id, card.id);
    assert_eq!(detail.source_evidence[0].source_status, "current-source");
    assert!(
        detail.source_evidence[0]
            .source
            .as_deref()
            .is_some_and(|source| source.contains("function start"))
    );
    let compact = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(card.id),
            supports_per_artifact: 1,
            artifact_view: ArtifactViewMode::Compact,
            debug: false,
            ..Default::default()
        },
    )?;
    let compact = serde_json::to_value(&compact)?;
    assert_eq!(compact["view"], "compact");
    assert_eq!(
        compact["semantic_artifacts"][0]["primary_claim"],
        "starts the settlement flow"
    );
    assert!(compact.get("retrieval").is_none());
    assert!(compact["semantic_artifacts"][0].get("body").is_none());
    assert!(compact["semantic_artifacts"][0].get("model").is_none());
    assert!(compact["semantic_artifacts"][0].get("supports").is_none());

    let body = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(card.id),
            supports_per_artifact: 1,
            artifact_view: ArtifactViewMode::Body,
            debug: false,
            ..Default::default()
        },
    )?;
    let body = serde_json::to_value(&body)?;
    assert_eq!(body["view"], "body");
    assert_eq!(
        body["semantic_artifacts"][0]["body"]["purpose"],
        "starts the settlement flow"
    );
    assert_eq!(
        body["semantic_artifacts"][0]["evidence"][0]["file"],
        "flow.ts"
    );
    assert!(body["semantic_artifacts"][0].get("model").is_none());
    assert!(
        body["semantic_artifacts"][0]["evidence"][0]
            .get("source_hash")
            .is_none()
    );

    let full = serde_json::to_value(&detail)?;
    assert_eq!(
        full["semantic_artifacts"][0]["body"]["purpose"],
        "starts the settlement flow"
    );
    assert!(full["semantic_artifacts"][0]["model"].is_string());
    assert!(full["semantic_artifacts"][0]["supports"][0]["source_hash"].is_string());
    let no_source = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(card.id),
            include_source: true,
            source_limit: 0,
            ..Default::default()
        },
    )?;
    assert_eq!(no_source.mode, "artifact_detail");
    assert_eq!(no_source.semantic_artifacts[0].id, card.id);
    assert!(no_source.source_evidence.is_empty());
    assert!(result.response_budget.rendered_bytes <= 24_000);
    assert_eq!(
        result.response_budget.unbudgeted_bytes,
        result.response_budget.rendered_bytes
    );

    std::fs::write(
        repo.path().join("flow.ts"),
        "export function moved() { return 0; }\n\
         export function finish() { return 1; }\n\
         export function start() { return finish(); }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(card.id),
            include_source: true,
            ..Default::default()
        },
    )?;
    assert_eq!(stale.source_evidence[0].support_freshness, "source-stale");
    assert_eq!(stale.source_evidence[0].source_status, "source-stale");
    assert!(stale.source_evidence[0].source.is_none());
    Ok(())
}

#[test]
fn localized_memory_never_backfills_unsupported_semantic_analogs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("target.ts"),
        "export function targetRootLayout() { return 1; }\n",
    )?;
    std::fs::write(
        repo.path().join("cms.ts"),
        "export function cmsExample() { return 2; }\n",
    )?;
    std::fs::write(
        repo.path().join("blind.ts"),
        "export function blindSpot() { return 3; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let target = crate::structural::resolve_current_anchor(&conn, "target.ts:targetRootLayout")?;
    let cms = crate::structural::resolve_current_anchor(&conn, "cms.ts:cmsExample")?;
    let blind = crate::structural::resolve_current_anchor(&conn, "blind.ts:blindSpot")?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let publish = |_name: &str, body: serde_json::Value, anchor: &str, file: &str| -> Result<i64> {
        Ok(semantic::annotate(
            repo.path(),
            &conn,
            &semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(anchor.into()),
                body,
                supports: vec![semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: anchor.into(),
                    role: None,
                    evidence_file: file.into(),
                    evidence_start_line: 1,
                    evidence_end_line: 1,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )?
        .id)
    };
    let target_card = publish(
        "target card",
        json!({"purpose": "builds route parameters"}),
        &target,
        "target.ts",
    )?;
    publish(
        "CMS root layout example",
        json!({"purpose": "createRouteTypesManifest root layout parameter generation"}),
        &cms,
        "cms.ts",
    )?;

    let anchored = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "createRouteTypesManifest root layout parameter generation".into(),
            anchor: Some(target.clone()),
            ..Default::default()
        },
    )?;
    assert_eq!(anchored.status, "results");
    assert_eq!(anchored.candidate_artifacts, 1);
    assert_eq!(anchored.artifact_handles[0].id, target_card);
    assert_eq!(
        anchored.artifact_handles[0].selection_reason,
        "exact_anchor_support"
    );
    assert_eq!(
        anchored.artifact_handles[0].followup.arguments.artifact,
        target_card
    );
    let rendered = serde_json::to_value(&anchored)?;
    assert!(rendered["artifact_handles"][0].get("body").is_none());
    assert_eq!(rendered["semantic_artifacts"], json!([]));

    let tiered = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            anchor: Some(target.clone()),
            file: Some("cms.ts".into()),
            limit: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(tiered.artifact_handles.len(), 2);
    assert_eq!(tiered.artifact_handles[0].id, target_card);
    assert_eq!(
        tiered.artifact_handles[0].selection_reason,
        "exact_anchor_support"
    );
    assert_eq!(
        tiered.artifact_handles[1].selection_reason,
        "exact_file_support"
    );

    let detail = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(target_card),
            ..Default::default()
        },
    )?;
    assert_eq!(
        detail.semantic_artifacts[0].body["purpose"],
        "builds route parameters"
    );
    assert!(detail.artifact_handles.is_empty());

    let file_scoped = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "CMS root layout".into(),
            file: Some("target.ts".into()),
            ..Default::default()
        },
    )?;
    assert_eq!(file_scoped.artifact_handles[0].id, target_card);
    assert_eq!(
        file_scoped.artifact_handles[0].selection_reason,
        "exact_file_support"
    );

    let unsupported = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "root layout".into(),
            anchor: Some(blind),
            ..Default::default()
        },
    )?;
    assert_eq!(unsupported.status, "no_supported_memory");
    assert_eq!(unsupported.candidate_artifacts, 0);
    assert!(unsupported.artifact_handles.is_empty());
    assert!(unsupported.semantic_artifacts.is_empty());
    Ok(())
}

#[test]
fn summary_drilldown_follows_only_claim_citations_and_reports_depth_caps() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("flow.ts"),
        "export function cited() { return 1; }\n\
         export function uncited() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let cited_anchor = crate::structural::resolve_current_anchor(&conn, "flow.ts:cited")?;
    let uncited_anchor = crate::structural::resolve_current_anchor(&conn, "flow.ts:uncited")?;
    let create_card = |name: String, line: i64, purpose: &str| -> Result<_> {
        semantic::annotate(
            repo.path(),
            &conn,
            &semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(name.clone()),
                body: json!({ "purpose": purpose }),
                supports: vec![semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: name,
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: line,
                    evidence_end_line: line,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )
    };
    let cited = create_card(cited_anchor, 1, "cited child")?;
    let uncited = create_card(uncited_anchor, 2, "uncited child")?;
    let fingerprint = |id| -> Result<String> {
        Ok(conn.query_row(
            "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
            [id],
            |row| row.get(0),
        )?)
    };
    let summary = semantic::AnnotateInput {
        artifact_type: "summary".into(),
        name: Some("file:flow.ts".into()),
        body: json!({
            "level": "file",
            "scope": "file:flow.ts",
            "overview": "uses only the cited child",
            "key_points": ["the same child supports a second claim"],
        }),
        supports: Vec::new(),
        confidence: "likely".into(),
        snapshot: snapshot.clone(),
        supersedes: None,
    };
    let (current_snapshot, supports) =
        semantic::validate_annotate_input(repo.path(), &conn, &summary)?;
    let summary_id = semantic::persist_validated_artifact(
        &conn,
        &summary,
        &current_snapshot,
        &supports,
        &[
            semantic::RelationInput {
                claim_path: "/overview".into(),
                relation: "summarizes".into(),
                dst_artifact_id: cited.id,
                dst_fingerprint: fingerprint(cited.id)?,
                confidence: "likely".into(),
            },
            semantic::RelationInput {
                claim_path: "/key_points/0".into(),
                relation: "summarizes".into(),
                dst_artifact_id: cited.id,
                dst_fingerprint: fingerprint(cited.id)?,
                confidence: "likely".into(),
            },
            semantic::RelationInput {
                claim_path: String::new(),
                relation: "summarizes".into(),
                dst_artifact_id: cited.id,
                dst_fingerprint: fingerprint(cited.id)?,
                confidence: "likely".into(),
            },
            semantic::RelationInput {
                claim_path: String::new(),
                relation: "summarizes".into(),
                dst_artifact_id: uncited.id,
                dst_fingerprint: fingerprint(uncited.id)?,
                confidence: "likely".into(),
            },
        ],
        &semantic::ArtifactProvenance {
            model: "test",
            prompt_version: "summary-scout/v1",
            scout_run_id: None,
            input_fingerprint: None,
        },
    )?;
    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(summary_id),
            include_source: true,
            ..Default::default()
        },
    )?;
    assert_eq!(result.source_evidence.len(), 2);
    assert!(
        result
            .source_evidence
            .iter()
            .all(|evidence| evidence.evidence_artifact_id == cited.id)
    );
    let claim_paths = result
        .source_evidence
        .iter()
        .map(|evidence| evidence.via_relations[0].claim_path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        claim_paths,
        std::collections::BTreeSet::from(["/key_points/0", "/overview"])
    );

    let middle_fingerprint = "middle-fingerprint";
    conn.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type, canonical_name, body_json, model, prompt_version,
           confidence, source_snapshot, created_at, artifact_fingerprint
         ) VALUES('annotation','middle','{\"claim\":\"middle\"}','test','test/v1',
                  'likely',?1,'now',?2)",
        rusqlite::params![snapshot, middle_fingerprint],
    )?;
    let middle_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO semantic_relations VALUES(?1,?2,'related_to','/claim','likely',?3)",
        rusqlite::params![middle_id, cited.id, fingerprint(cited.id)?],
    )?;
    let root_fingerprint = "root-fingerprint";
    conn.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type, canonical_name, body_json, model, prompt_version,
           confidence, source_snapshot, created_at, artifact_fingerprint
         ) VALUES('annotation','root','{\"claim\":\"root\"}','test','test/v1',
                  'likely',?1,'now',?2)",
        rusqlite::params![snapshot, root_fingerprint],
    )?;
    let root_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO semantic_relations VALUES(?1,?2,'related_to','/claim','likely',?3)",
        rusqlite::params![root_id, middle_id, middle_fingerprint],
    )?;
    let capped = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(root_id),
            include_source: true,
            evidence_relation_depth: 1,
            ..Default::default()
        },
    )?;
    assert!(capped.response_budget.relation_depth_truncated);
    assert_eq!(capped.response_budget.omitted_relation_branches, 1);
    assert!(capped.source_evidence.is_empty());
    Ok(())
}

#[test]
fn line_excerpt_is_inclusive_and_rejects_invalid_spans() {
    assert_eq!(line_excerpt("a\nb\nc\n", 2, 3).as_deref(), Some("b\nc\n"));
    assert!(line_excerpt("a\n", 0, 1).is_none());
    assert!(line_excerpt("a\n", 2, 2).is_none());
}

#[test]
fn concept_tags_use_inclusive_overlap_dedupe_and_file_fallback() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  const a = 1;\n  const b = 2;\n  const c = 3;\n  return a + b + c;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let overlapping = concept_fixture(repo.path(), &conn, "boundary concept", &anchor, 2, 4, None)?;
    replace_concept_chunks(&conn, &[(1, 2), (4, 5)])?;

    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(overlapping.id),
            ..Default::default()
        },
    )?;
    assert_eq!(result.matched_concept_tags, 3);
    assert_eq!(result.concept_tags.len(), 3);
    assert_eq!(result.concept_tags[0].level, "file");
    assert!(result.concept_tags[0].chunk_id.is_none());
    assert_eq!(result.concept_tags[1].chunk_lines, Some([1, 2]));
    assert_eq!(result.concept_tags[2].chunk_lines, Some([4, 5]));
    assert!(
        result
            .concept_tags
            .iter()
            .all(|tag| tag.concept_artifact_id == overlapping.id)
    );

    let file_only = concept_fixture(repo.path(), &conn, "gap concept", &anchor, 3, 3, None)?;
    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(file_only.id),
            ..Default::default()
        },
    )?;
    assert_eq!(result.matched_concept_tags, 1);
    assert_eq!(result.concept_tags.len(), 1);
    assert_eq!(result.concept_tags[0].level, "file");
    assert!(result.concept_tags[0].chunk_lines.is_none());
    Ok(())
}

#[test]
fn relation_backed_concept_tags_follow_child_semantic_supports() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  const a = 1;\n  return a;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let child = semantic::annotate(
        repo.path(),
        &conn,
        &semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(anchor.clone()),
            body: json!({
                "purpose": "establishes the relation-backed concept meaning",
                "domain_terms": ["relation concept"],
            }),
            supports: vec![
                semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor: anchor.clone(),
                    role: None,
                    evidence_file: "concept.ts".into(),
                    evidence_start_line: 1,
                    evidence_end_line: 3,
                    confidence: "likely".into(),
                },
                semantic::SupportInput {
                    claim_path: "/domain_terms/0".into(),
                    anchor: anchor.clone(),
                    role: None,
                    evidence_file: "concept.ts".into(),
                    evidence_start_line: 1,
                    evidence_end_line: 1,
                    confidence: "likely".into(),
                },
            ],
            confidence: "likely".into(),
            snapshot: crate::structural::current_snapshot(&conn)?,
            supersedes: None,
        },
    )?;
    let fingerprint: String = conn.query_row(
        "SELECT artifact_fingerprint FROM semantic_artifacts WHERE id=?1",
        [child.id],
        |row| row.get(0),
    )?;
    let concept_input = semantic::AnnotateInput {
        artifact_type: "concept".into(),
        name: Some("relation concept".into()),
        body: json!({
            "definition": "Repository meaning established by the child card",
            "aliases": ["relation concept"],
        }),
        supports: Vec::new(),
        confidence: "likely".into(),
        snapshot: crate::structural::current_snapshot(&conn)?,
        supersedes: None,
    };
    let (snapshot, supports) =
        semantic::validate_annotate_input(repo.path(), &conn, &concept_input)?;
    let relations = vec![
        semantic::RelationInput {
            claim_path: "/definition".into(),
            relation: "related_to".into(),
            dst_artifact_id: child.id,
            dst_fingerprint: fingerprint.clone(),
            confidence: "likely".into(),
        },
        semantic::RelationInput {
            claim_path: "/aliases/0".into(),
            relation: "related_to".into(),
            dst_artifact_id: child.id,
            dst_fingerprint: fingerprint.clone(),
            confidence: "likely".into(),
        },
        semantic::RelationInput {
            claim_path: String::new(),
            relation: "related_to".into(),
            dst_artifact_id: child.id,
            dst_fingerprint: fingerprint,
            confidence: "likely".into(),
        },
    ];
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let concept_id = semantic::persist_validated_artifact(
        &conn,
        &concept_input,
        &snapshot,
        &supports,
        &relations,
        &semantic::ArtifactProvenance {
            model: "test",
            prompt_version: "concept-scout/v1",
            scout_run_id: None,
            input_fingerprint: None,
        },
    )?;
    conn.execute_batch("COMMIT")?;

    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(concept_id),
            ..Default::default()
        },
    )?;
    assert!(result.semantic_artifacts[0].supports.is_empty());
    assert!(
        result
            .concept_tags
            .iter()
            .any(|tag| tag.file == "concept.ts"),
        "tags must traverse concept -> child -> exact source supports"
    );
    Ok(())
}

#[test]
fn concept_query_uses_the_versioned_unicode_normalizer() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let concept = concept_fixture(
        repo.path(),
        &conn,
        "invoice settlement",
        &anchor,
        1,
        1,
        None,
    )?;
    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "ＩＮＶＯＩＣＥ　ＳＥＴＴＬＥＭＥＮＴ".into(),
            artifact_types: vec!["concept".into()],
            ..Default::default()
        },
    )?;
    assert_eq!(result.artifact_handles.len(), 1);
    assert_eq!(result.artifact_handles[0].id, concept.id);
    assert_eq!(
        result.artifact_handles[0]
            .retrieval_score
            .as_ref()
            .expect("retrieval score")
            .rank_score,
        1.0
    );
    Ok(())
}

#[test]
fn exact_short_or_punctuation_concept_query_bypasses_token_length_filter() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let expected = concept_fixture(repo.path(), &conn, "c++", &anchor, 1, 1, None)?;
    concept_fixture(
        repo.path(),
        &conn,
        "newer unrelated concept",
        &anchor,
        1,
        1,
        None,
    )?;

    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "C++".into(),
            artifact_types: vec!["concept".into()],
            limit: 1,
            ..Default::default()
        },
    )?;
    assert_eq!(result.candidate_artifacts, 1);
    assert_eq!(result.artifact_handles[0].id, expected.id);
    assert_eq!(
        result.artifact_handles[0]
            .retrieval_score
            .as_ref()
            .expect("retrieval score")
            .rank_score,
        1.0
    );
    Ok(())
}

#[test]
fn concept_tags_apply_file_origin_policy_before_matching() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  return 1;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let artifact = concept_fixture(repo.path(), &conn, "origin concept", &anchor, 1, 2, None)?;
    conn.execute(
        "UPDATE files SET origin='dependency' WHERE path='concept.ts'",
        [],
    )?;

    let excluded = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(artifact.id),
            ..Default::default()
        },
    )?;
    assert_eq!(excluded.matched_concept_tags, 0);
    assert!(excluded.concept_tags.is_empty());
    assert_eq!(excluded.response_budget.omitted_concept_tags, 0);

    let included = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(artifact.id),
            file_origins: vec!["dependency".into()],
            ..Default::default()
        },
    )?;
    assert!(included.matched_concept_tags > 0);
    assert_eq!(included.matched_concept_tags, included.concept_tags.len());
    assert!(
        included
            .concept_tags
            .iter()
            .all(|tag| tag.file == "concept.ts")
    );
    Ok(())
}

#[test]
fn concept_tags_exclude_historical_degraded_and_stale_concepts() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  return 1;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let historical = concept_fixture(repo.path(), &conn, "history concept", &anchor, 1, 2, None)?;
    let current = concept_fixture(
        repo.path(),
        &conn,
        "history concept",
        &anchor,
        1,
        2,
        Some(historical.id),
    )?;

    let result = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            query: "history concept".into(),
            artifact_types: vec!["concept".into()],
            include_superseded: true,
            ..Default::default()
        },
    )?;
    assert_eq!(result.candidate_artifacts, 2);
    assert_eq!(result.artifact_handles.len(), 2);
    assert!(result.concept_tags.is_empty());
    let detail = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(current.id),
            ..Default::default()
        },
    )?;
    assert!(!detail.concept_tags.is_empty());
    assert!(
        detail
            .concept_tags
            .iter()
            .all(|tag| tag.concept_artifact_id == current.id)
    );

    let mut degraded = semantic::load_artifact(&conn, current.id)?.expect("current concept");
    degraded.freshness = "degraded".into();
    assert!(
        concept_tags(
            &conn,
            &[degraded],
            &std::collections::HashMap::from([(current.id, true)]),
            &crate::origin::defaults(),
        )?
        .is_empty()
    );

    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  return 2;\n}\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(current.id),
            ..Default::default()
        },
    )?;
    assert_eq!(stale.semantic_artifacts[0].freshness, "stale");
    assert_eq!(stale.matched_concept_tags, 0);
    assert!(stale.concept_tags.is_empty());
    Ok(())
}

#[test]
fn concept_tag_limits_and_response_budget_account_for_whole_tags() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("concept.ts"),
        "export function subject() {\n  const a = 1;\n  const b = 2;\n  const c = 3;\n  return a + b + c;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = crate::structural::resolve_current_anchor(&conn, "concept.ts:subject")?;
    let artifact = concept_fixture(repo.path(), &conn, "budget concept", &anchor, 2, 4, None)?;
    replace_concept_chunks(&conn, &[(1, 2), (4, 5)])?;

    let limited = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(artifact.id),
            concept_tag_limit: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(limited.matched_concept_tags, 3);
    assert_eq!(limited.concept_tags.len(), 2);
    assert_eq!(limited.response_budget.omitted_concept_tags, 1);

    let full = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(artifact.id),
            include_source: true,
            ..Default::default()
        },
    )?;
    let budgeted = query(
        repo.path(),
        &conn,
        None,
        &QueryOptions {
            artifact_id: Some(artifact.id),
            include_source: true,
            response_byte_limit: full.response_budget.rendered_bytes - 64,
            ..Default::default()
        },
    )?;
    assert!(budgeted.concept_tags.len() < full.concept_tags.len());
    assert_eq!(
        budgeted.response_budget.omitted_concept_tags,
        budgeted.matched_concept_tags - budgeted.concept_tags.len()
    );
    assert_eq!(
        budgeted.semantic_artifacts.len(),
        full.semantic_artifacts.len()
    );
    assert_eq!(budgeted.source_evidence.len(), full.source_evidence.len());
    assert!(
        budgeted
            .source_evidence
            .iter()
            .all(|evidence| evidence.source.is_some())
    );
    assert!(budgeted.response_budget.rendered_bytes <= full.response_budget.rendered_bytes - 64);

    assert!(
        query(
            repo.path(),
            &conn,
            None,
            &QueryOptions {
                artifact_id: Some(artifact.id),
                concept_tag_limit: MAX_CONCEPT_TAG_LIMIT + 1,
                ..Default::default()
            },
        )
        .is_err()
    );
    Ok(())
}
