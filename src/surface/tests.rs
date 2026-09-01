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
    let identities = crate::publication::Identities::read(&conn)?;
    assert_eq!(result.snapshot, identities.code);
    assert_eq!(result.publication_snapshot, identities.publication);
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

    let response = overview_response(
        &conn,
        &OverviewOptions {
            area_limit: 1,
            relation_limit: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(response.snapshot, identities.code);
    assert_eq!(response.publication_snapshot, identities.publication);
    let rendered = serde_json::to_value(&response)?;
    assert!(rendered["response_budget"].get("byte_limit").is_none());
    assert_eq!(
        response.response_budget.rendered_bytes,
        serde_json::to_string(&response)?.len()
    );
    assert_eq!(
        rendered
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| *key == "snapshot")
            .count(),
        1
    );
    let overview = response.overview;
    assert_eq!(overview.areas.len(), 1);
    assert_eq!(overview.areas[0].path, "packages/api");
    assert!(overview.relations.len() <= 2);
    assert_eq!(overview.totals["files"], 2);
    Ok(())
}

#[test]
fn entity_lookup_excludes_occurrences_from_non_structural_formats() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export const visible = process.env.SURFACE_VISIBLE;\n",
    )?;
    fs::write(repo.path().join("poison.rs"), "pub fn poison() {}\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let baseline = entities(
        &conn,
        &EntityLookupOptions {
            query: "SURFACE_VISIBLE".into(),
            ..Default::default()
        },
    )?;
    assert_eq!(baseline.entities.len(), 1);
    let baseline_count = baseline.entities[0].occurrence_count;
    let visible_entity_id: i64 = conn.query_row(
        "SELECT id FROM entities WHERE name='SURFACE_VISIBLE'",
        [],
        |row| row.get(0),
    )?;
    let rust_file_id: i64 = conn.query_row(
        "SELECT id FROM files WHERE path='poison.rs' AND format='rust'",
        [],
        |row| row.get(0),
    )?;
    let rust_chunk_id: i64 = conn.query_row(
        "SELECT id FROM chunks WHERE file_id=?1 ORDER BY id LIMIT 1",
        [rust_file_id],
        |row| row.get(0),
    )?;

    conn.execute(
        "INSERT INTO entity_sites(
           file_id,chunk_id,start,end,line,end_line,plane,entity_type,role,
           identity_kind,identity_name,identity_start,extractor,provenance,confidence
         ) VALUES(?1,?2,0,12,1,1,'general','environment_variable','read',
                  'literal','SURFACE_VISIBLE',0,'fixture','fixture','certain')",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;
    let mixed_site_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO entity_occurrences(
           entity_id,site_id,file_id,chunk_id,start,end,line,end_line,role,
           extractor,provenance,confidence
         ) VALUES(?1,?2,?3,?4,0,12,1,1,'read','fixture','fixture','certain')",
        rusqlite::params![
            visible_entity_id,
            mixed_site_id,
            rust_file_id,
            rust_chunk_id
        ],
    )?;
    conn.execute(
        "INSERT INTO entities(
           entity_key,plane,entity_type,name,identity_anchor
         ) VALUES('fixture:rust:secret','general','rust_fixture',
                  'RUST_SURFACE_SECRET','poison.rs')",
        [],
    )?;
    let rust_entity_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO entity_sites(
           file_id,chunk_id,start,end,line,end_line,plane,entity_type,role,
           identity_kind,identity_name,identity_start,extractor,provenance,confidence
         ) VALUES(?1,?2,0,12,1,1,'general','rust_fixture','declaration',
                  'literal','RUST_SURFACE_SECRET',0,'fixture','fixture','certain')",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;
    let rust_site_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO entity_occurrences(
           entity_id,site_id,file_id,chunk_id,start,end,line,end_line,role,
           extractor,provenance,confidence
         ) VALUES(?1,?2,?3,?4,0,12,1,1,'declaration',
                  'fixture','fixture','certain')",
        rusqlite::params![rust_entity_id, rust_site_id, rust_file_id, rust_chunk_id],
    )?;

    let visible = entities(
        &conn,
        &EntityLookupOptions {
            query: "SURFACE_VISIBLE".into(),
            ..Default::default()
        },
    )?;
    assert_eq!(visible.entities.len(), 1);
    assert_eq!(visible.entities[0].occurrence_count, baseline_count);
    assert!(
        visible.entities[0]
            .occurrences
            .iter()
            .all(|occurrence| occurrence.file == "main.ts")
    );
    let rust_only = entities(
        &conn,
        &EntityLookupOptions {
            query: "RUST_SURFACE_SECRET".into(),
            ..Default::default()
        },
    )?;
    assert!(rust_only.entities.is_empty());
    assert_eq!(rust_only.matched_entities, 0);
    Ok(())
}

#[test]
fn overview_preserves_rust_inventory_but_excludes_rust_structural_rows() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src/app"))?;
    fs::create_dir_all(repo.path().join("native"))?;
    fs::write(
        repo.path().join("src/app/main.ts"),
        "export const visible = process.env.SURFACE_OVERVIEW;\n",
    )?;
    fs::write(
        repo.path().join("native/poison.rs"),
        "pub fn poison() -> usize { 1 }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let options = OverviewOptions {
        area_limit: 100,
        relation_limit: 100,
        reconnaissance_limit: 0,
        ..Default::default()
    };
    let baseline = overview_response(&conn, &options)?.overview;
    assert_eq!(baseline.totals["files"], 2);
    let baseline_native = baseline
        .areas
        .iter()
        .find(|area| area.path == "native")
        .expect("Rust area remains in code inventory");
    assert_eq!(baseline_native.files, 1);
    assert!(baseline_native.chunks > 0);

    let rust_file_id: i64 = conn.query_row(
        "SELECT id FROM files WHERE path='native/poison.rs' AND format='rust'",
        [],
        |row| row.get(0),
    )?;
    let rust_chunk_id: i64 = conn.query_row(
        "SELECT id FROM chunks WHERE file_id=?1 ORDER BY id LIMIT 1",
        [rust_file_id],
        |row| row.get(0),
    )?;
    let typescript_file_id: i64 = conn.query_row(
        "SELECT id FROM files WHERE path='src/app/main.ts' AND format='typescript'",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO symbols(
           file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
         ) VALUES(?1,'RUST_SURFACE_SYMBOL','function',0,12,0,12,'',1,1)",
        [rust_file_id],
    )?;
    conn.execute(
        "INSERT INTO entities(entity_key,plane,entity_type,name,identity_anchor)
         VALUES('fixture:rust:overview','general','rust_overview_fixture',
                'RUST_OVERVIEW_ENTITY','native/poison.rs')",
        [],
    )?;
    let rust_entity_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO entity_sites(
           file_id,chunk_id,start,end,line,end_line,plane,entity_type,role,
           identity_kind,identity_name,identity_start,extractor,provenance,confidence
         ) VALUES(?1,?2,0,12,1,1,'general','rust_overview_fixture','declaration',
                  'literal','RUST_OVERVIEW_ENTITY',0,'fixture','fixture','certain')",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;
    let rust_site_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO entity_occurrences(
           entity_id,site_id,file_id,chunk_id,start,end,line,end_line,role,
           extractor,provenance,confidence
         ) VALUES(?1,?2,?3,?4,0,12,1,1,'declaration',
                  'fixture','fixture','certain')",
        rusqlite::params![rust_entity_id, rust_site_id, rust_file_id, rust_chunk_id],
    )?;
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key,dst_key,kind,confidence,provenance,source_file_id,line
         ) VALUES('fixture:rust:source','fixture:target','surface_rust_poison',
                  'certain','fixture',?1,1)",
        [rust_file_id],
    )?;
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key,dst_key,kind,confidence,provenance,source_file_id,line
         ) VALUES('fixture:ts:source','fixture:target','surface_ts_control',
                  'certain','fixture',?1,1)",
        [typescript_file_id],
    )?;
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key,dst_key,kind,confidence,provenance
         ) VALUES('fixture:global:source','fixture:target',
                  'surface_global_control','certain','fixture')",
        [],
    )?;

    let after = overview_response(&conn, &options)?.overview;
    assert_eq!(after.totals["files"], baseline.totals["files"]);
    assert_eq!(after.totals["chunks"], baseline.totals["chunks"]);
    assert_eq!(after.totals["symbols"], baseline.totals["symbols"]);
    assert_eq!(
        after.totals["entity_occurrences"],
        baseline.totals["entity_occurrences"]
    );
    assert_eq!(
        after.totals["graph_edges"],
        baseline.totals["graph_edges"] + 2
    );
    let after_native = after
        .areas
        .iter()
        .find(|area| area.path == "native")
        .expect("Rust area remains in code inventory");
    assert_eq!(after_native.files, baseline_native.files);
    assert_eq!(after_native.chunks, baseline_native.chunks);
    assert_eq!(after_native.symbols, 0);
    assert_eq!(after_native.entity_occurrences, 0);
    assert!(
        after
            .entity_inventory
            .iter()
            .all(|count| count.kind != "rust_overview_fixture")
    );
    assert!(
        after
            .relations
            .iter()
            .all(|relation| relation.kind != "surface_rust_poison")
    );
    for control in ["surface_ts_control", "surface_global_control"] {
        assert_eq!(
            after
                .relations
                .iter()
                .find(|relation| relation.kind == control)
                .map(|relation| relation.edges),
            Some(1)
        );
    }
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
    fs::write(
        repo.path().join("README.md"),
        "# Documentation\n\nThis stays in the canonical repository inventory.\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let documentation_file_nodes: i64 = conn.query_row(
        "SELECT count(*)
         FROM graph_nodes node
         JOIN files file ON file.id=node.file_id
         WHERE node.node_kind='file' AND file.path='README.md'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        documentation_file_nodes, 0,
        "the code structural projection must exclude Markdown files"
    );
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

    let response = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_limit: 12,
            ..Default::default()
        },
    )?;
    assert_eq!(response.overview.totals["files"], 2);
    assert_eq!(response.overview.files_by_origin["repository"], 2);
    let overlay = response
        .reconnaissance
        .expect("current reconnaissance overlay");
    assert_eq!(overlay.trust, "untrusted_semantic_policy");
    assert_eq!(overlay.status, "current");
    assert!(overlay.refresh_hint.is_none());
    assert_eq!(overlay.roles["runtime"], 1);
    assert_eq!(overlay.effective_file_roles["runtime"], 1);
    assert_eq!(overlay.effective_file_roles["test"], 1);
    assert!(!overlay.effective_file_roles.contains_key("documentation"));
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

    // G28 phase 2: the default overview carries no reconnaissance prose, and
    // when a budget bites, reconnaissance classifications are shed before any
    // structural count.
    let silent = overview_response(&conn, &OverviewOptions::default())?;
    assert!(silent.reconnaissance.is_none());
    assert_eq!(OverviewOptions::default().reconnaissance_limit, 0);
    let full = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_limit: 12,
            ..Default::default()
        },
    )?;
    let full_bytes = serde_json::to_string(&full)?.len();
    let squeezed = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_limit: 12,
            response_byte_limit: full_bytes - 40,
            ..Default::default()
        },
    )?;
    assert!(
        squeezed
            .response_budget
            .omitted_reconnaissance_classifications
            >= 1
    );
    assert_eq!(squeezed.response_budget.omitted_areas, 0);
    assert_eq!(squeezed.response_budget.omitted_relations, 0);
    assert_eq!(squeezed.response_budget.omitted_entity_inventory, 0);

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
    let stale = overview_response(
        &conn,
        &OverviewOptions {
            reconnaissance_limit: 12,
            ..Default::default()
        },
    )?
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
    assert_eq!(
        fresh.response_budget.rendered_bytes,
        serde_json::to_string(&fresh)?.len()
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
