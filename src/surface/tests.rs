use std::fs;

use anyhow::Result;

use serde_json::json;

use super::{EntityLookupOptions, OverviewOptions, entities, overview_response, repository_area};
use crate::{indexer, recon, semantic, store, structural};

#[test]
fn entity_lookup_filters_evidence_and_overview_is_bounded() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("packages/api/src"))?;
    fs::write(
        repo.path().join("packages/api/src/main.ts"),
        "export function run() { return process.env.API_KEY + process.env.OTHER_KEY; }\n",
    )?;
    fs::create_dir_all(repo.path().join("packages/api/test"))?;
    fs::write(
        repo.path().join("packages/api/test/main.test.ts"),
        "test('env', () => process.env.API_KEY);\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = entities(
        &conn,
        &EntityLookupOptions {
            query: "API_KEY".into(),
            planes: vec!["general".into()],
            ..Default::default()
        },
    )?;
    assert_eq!(result.entities.len(), 1);
    assert_eq!(result.entities[0].occurrence_count, 1);
    assert_eq!(result.entities[0].occurrences[0].file_role, "production");

    let bounded = entities(
        &conn,
        &EntityLookupOptions {
            planes: vec!["general".into()],
            limit: 1,
            ..Default::default()
        },
    )?;
    assert_eq!(bounded.entities.len(), 1);
    assert_eq!(bounded.matched_entities, 2);
    assert!(bounded.truncated);

    let overview = overview_response(
        &conn,
        &OverviewOptions {
            area_limit: 1,
            relation_limit: 2,
            ..Default::default()
        },
    )?
    .overview;
    assert_eq!(overview.areas.len(), 1);
    assert_eq!(overview.areas[0].path, "packages/api");
    assert!(overview.relations.len() <= 2);
    assert_eq!(overview.totals["files"], 2);
    Ok(())
}

#[test]
fn dependency_areas_preserve_the_package_instance_prefix() {
    assert_eq!(
        repository_area("dependency:lodash@4.17.21#abc123/lodash.js"),
        "dependency:lodash@4.17.21#abc123"
    );
    assert_eq!(
        repository_area("dependency:@scope/pkg@2.0.0#def456/src/index.ts"),
        "dependency:@scope/pkg@2.0.0#def456"
    );
}

#[test]
fn overview_surfaces_current_cited_reconnaissance_and_effective_roles() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("docs"))?;
    fs::write(
        repo.path().join("docs/runtime.ts"),
        "export function renderDocument() { return 1; }\n",
    )?;
    fs::write(
        repo.path().join("docs/runtime.test.ts"),
        "test('render', () => renderDocument());\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let selector = recon::SubjectSelector::RepositoryArea {
        scope: "docs".into(),
        direct_only: false,
    };
    let state = recon::build_scope_state(
        repo.path(),
        &conn,
        "area:repository:docs".into(),
        selector.clone(),
    )?;
    let snapshot = structural::current_snapshot(&conn)?;
    conn.execute(
        "INSERT INTO scout_runs(
           scout_kind,status,gateway_protocol,provider,model,billing_path,
           prompt_version,source_snapshot,input_fingerprint,request_hash,
           config_json,started_at,completed_at
         ) VALUES('repository','completed',1,'openai-codex','gpt-5.6-terra',
                  'plan','repository-recon/v2',?1,'overview-recon',
                  'overview-recon','{}','now','now')",
        [&snapshot],
    )?;
    let run_id = conn.last_insert_rowid();
    let cited = json!([{
        "id": "E001",
        "kind": "outline",
        "source": "docs/runtime.ts",
        "start_line": 1,
        "end_line": 1,
        "content": "exported function `renderDocument`"
    }]);
    recon::persist_classification(
        &conn,
        &recon::NewClassification {
            run_id,
            subject_key: &state.subject_key,
            subject_kind: "area",
            selector: &selector,
            parent_subject_key: None,
            depth: 0,
            role: "runtime",
            confidence: "likely",
            explanation: "document-domain runtime implementation",
            citations_json: "[\"E001\"]",
            cited_evidence_json: &cited.to_string(),
            evidence_fingerprint: &state.evidence_fingerprint,
            classification_fingerprint: "overview-recon",
            source_snapshot: &snapshot,
        },
    )?;
    recon::reconcile_file_policy(repo.path(), &conn)?;

    let response = overview_response(&conn, &OverviewOptions::default())?;
    let overlay = response
        .reconnaissance
        .expect("current reconnaissance overlay");
    assert_eq!(overlay.trust, "untrusted_semantic_policy");
    assert_eq!(overlay.status, "current");
    assert!(overlay.refresh_hint.is_none());
    assert_eq!(overlay.roles["runtime"], 1);
    assert_eq!(overlay.effective_file_roles["runtime"], 1);
    assert_eq!(overlay.effective_file_roles["test"], 1);
    assert_eq!(overlay.classifications[0].conflict_files, 1);
    assert_eq!(overlay.classifications[0].citation_count, 1);
    assert_eq!(
        overlay.classifications[0].reason,
        "document-domain runtime implementation"
    );
    assert!(overlay.classifications[0].detail.is_none());

    let detailed = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_subject: Some(state.subject_key.clone()),
            reconnaissance_detail: true,
            ..Default::default()
        },
    )?
    .reconnaissance
    .expect("scoped reconnaissance detail");
    assert_eq!(detailed.matched, 1);
    assert_eq!(detailed.returned, 1);
    let detail = detailed.classifications[0]
        .detail
        .as_ref()
        .expect("explicit detail");
    assert_eq!(detail.citation_ids, ["E001"]);
    assert_eq!(
        detail.cited_evidence[0].source.as_deref(),
        Some("docs/runtime.ts")
    );
    let scoped_compact = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_subject: Some(state.subject_key.clone()),
            ..Default::default()
        },
    )?;
    let bounded_detail = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_subject: Some(state.subject_key.clone()),
            reconnaissance_detail: true,
            response_byte_limit: scoped_compact.response_budget.rendered_bytes + 32,
            ..Default::default()
        },
    )?;
    assert!(
        bounded_detail
            .reconnaissance
            .as_ref()
            .unwrap()
            .classifications[0]
            .detail
            .is_none()
    );
    assert_eq!(
        bounded_detail
            .response_budget
            .omitted_reconnaissance_details,
        1
    );
    assert!(
        overview_response(
            &conn,
            &OverviewOptions {
                reconnaissance_detail: true,
                ..Default::default()
            },
        )
        .is_err()
    );

    let without_reconnaissance = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_limit: 0,
            ..Default::default()
        },
    )?;
    assert!(without_reconnaissance.reconnaissance.is_none());

    fs::write(
        repo.path().join("docs/added-after-scout.ts"),
        "export const later = 1;\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = overview_response(&conn, &OverviewOptions::default())?
        .reconnaissance
        .expect("historical reconnaissance status");
    assert_eq!(stale.status, "no_current_classifications");
    assert_eq!(stale.matched, 0);
    assert!(stale.classifications.is_empty());
    assert!(
        stale
            .refresh_hint
            .expect("refresh hint")
            .contains("jscout scout repository")
    );
    Ok(())
}

#[test]
fn semantic_overview_includes_only_current_fresh_memory() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("flow.ts"),
        "export function start() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = structural::resolve_current_anchor(&conn, "flow.ts:start")?;
    let snapshot = structural::current_snapshot(&conn)?;
    semantic::annotate(
        repo.path(),
        &conn,
        &semantic::AnnotateInput {
            artifact_type: "card".into(),
            name: Some(anchor.clone()),
            body: json!({ "purpose": "starts the flow ".repeat(256) }),
            supports: vec![semantic::SupportInput {
                claim_path: "/purpose".into(),
                anchor,
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
    )?;
    conn.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type, canonical_name, body_json, model, prompt_version,
           confidence, source_snapshot, created_at, artifact_fingerprint
         ) VALUES('summary','module:excluded',?1,'test','summary-scout/v1',
                  'likely',?2,'now','excluded-summary')",
        rusqlite::params![
            json!({
                "level": "module",
                "scope": "module:excluded",
                "overview": "must not be freshness-loaded by a card-only overlay",
            })
            .to_string(),
            structural::current_snapshot(&conn)?,
        ],
    )?;
    conn.execute(
        "UPDATE meta SET value='/definitely/missing/jscout/root' WHERE key='root'",
        [],
    )?;

    let options = OverviewOptions {
        include_semantic: true,
        semantic_types: vec!["card".into()],
        ..Default::default()
    };
    let fresh = overview_response(&conn, &options)?;
    let deterministic_areas = fresh.overview.areas.len();
    let overlay = fresh.semantic_overlay.as_ref().expect("overlay requested");
    assert_eq!(overlay.returned, 1);
    assert_eq!(overlay.excluded_non_fresh, 0);
    assert_eq!(overlay.artifacts[0].freshness, "fresh");
    assert_eq!(
        fresh.response_budget.unbudgeted_bytes,
        fresh.response_budget.rendered_bytes
    );

    let deterministic = overview_response(
        &conn,
        &OverviewOptions {
            include_semantic: false,
            ..options.clone()
        },
    )?;
    let bounded_limit = deterministic.response_budget.rendered_bytes + 512;
    assert!(fresh.response_budget.rendered_bytes > bounded_limit);

    let bounded = overview_response(
        &conn,
        &OverviewOptions {
            response_byte_limit: bounded_limit,
            ..options.clone()
        },
    )?;
    assert!(bounded.response_budget.truncated);
    assert_eq!(bounded.response_budget.omitted_semantic_artifacts, 1);
    assert!(
        bounded
            .semantic_overlay
            .as_ref()
            .is_some_and(|overlay| overlay.artifacts.is_empty())
    );
    assert_eq!(bounded.overview.areas.len(), deterministic_areas);
    assert!(bounded.response_budget.rendered_bytes <= bounded_limit);

    fs::write(
        repo.path().join("flow.ts"),
        "export function start() { return 2; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let drifted = overview_response(&conn, &options)?;
    let overlay = drifted.semantic_overlay.expect("overlay requested");
    assert!(overlay.artifacts.is_empty());
    assert_eq!(overlay.excluded_non_fresh, 1);
    Ok(())
}
