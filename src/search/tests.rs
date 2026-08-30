use std::{
    collections::{HashMap, HashSet},
    fs,
};

use anyhow::Result;

use crate::embed;

use super::{
    DEFAULT_EXPANSION_PATH_LIMIT, DEFAULT_MEMORY_GRAPH_DEPTH, DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
    DEFAULT_RESPONSE_BYTE_LIMIT, ExpansionOptions, ExpansionProjection, Hit,
    MAX_EXHAUSTIVE_PAGE_SIZE, MatchReason, Reranker, ResponseBudget, ResponseBudgetTooSmall,
    RetrievalStatus, SearchExpansion, SearchMode, SearchOptions, SearchResult,
    SearchScopeFileRoles, SearchScopeFormats, apply_repository_policy_penalty,
    apply_response_budget, approximate_name_usage_occurrences, candidate_pool_limits,
    contains_code_identifier, edge_identity, exact_intent_tokens, exhaustive_fts_query,
    exhaustive_warnings, merge_reranked_prefix, prefilter_ranking_by_role, record_vector_ranking,
    reranker_document, resolve_search_limit, search, select_attached_memory,
    select_neighborhood_projection, select_path_projection, tiered_candidates,
};
use crate::config::{EmbeddingSettings, InferenceSettings, RerankerSettings};
use crate::{
    file_role, indexer, origin,
    semantic::{ArtifactRetrievalScore, SemanticArtifact, SemanticSupport},
    store,
    structural::{GraphEdge, GraphNode},
};

#[test]
fn markdown_admission_does_not_change_serialized_code_search_ranking() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export const note = 'shared ranking calibration';\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let options = SearchOptions {
        rerank: false,
        include_memory: false,
        expand: false,
        compact: true,
        ..SearchOptions::default()
    };
    let before = search(&conn, None, "shared ranking calibration", &options)?;
    assert!(!before.hits.is_empty());
    let before_publication = crate::publication::current_publication_snapshot(&conn)?;
    let before_json = serde_json::to_string(&before)?.replace(&before.snapshot, "<snapshot>");

    fs::write(
        repo.path().join("README.md"),
        format!(
            "# Ranking noise\n\n{}\n",
            "shared ranking calibration ".repeat(200)
        ),
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let after = search(&conn, None, "shared ranking calibration", &options)?;
    let after_publication = crate::publication::current_publication_snapshot(&conn)?;
    let after_json = serde_json::to_string(&after)?.replace(&after.snapshot, "<snapshot>");

    assert_eq!(before.snapshot, after.snapshot);
    assert_ne!(before_publication, after_publication);
    assert_eq!(before_json, after_json);
    Ok(())
}

#[test]
fn docs_corpus_named_chunks_cannot_enter_code_exact_lookup() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const codeOnly = 1;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    conn.execute(
        "INSERT INTO files(path,hash,corpus,format,role)
         VALUES('reference.md','docs-file','docs','markdown','documentation')",
        [],
    )?;
    let file_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO chunks(
           file_id,kind,name,start,end,start_line,end_line,hash,content
         ) VALUES(?1,'method','docsOnlyNamedChunk',0,18,1,1,
                  'docs-chunk','docsOnlyNamedChunk')",
        [file_id],
    )?;

    let result = search(
        &conn,
        None,
        "docsOnlyNamedChunk",
        &SearchOptions {
            rerank: false,
            include_memory: false,
            expand: false,
            ..SearchOptions::default()
        },
    )?;
    assert!(result.hits.is_empty());
    assert!(
        crate::query::find_symbols_in_origins(
            &conn,
            "docsOnlyNamedChunk",
            &crate::origin::defaults(),
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn rust_identifier_collisions_do_not_change_ecmascript_exact_candidates() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("definition.ts"),
        "export function collisionNeedle() { return true; }\n",
    )?;
    fs::write(
        repo.path().join("caller.js"),
        "import { collisionNeedle } from './definition';\n\
         export const result = collisionNeedle();\n\
         export const state = { collisionNeedle: result };\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let before = super::exact_intent_candidates(
        &conn,
        "collisionNeedle",
        32,
        &[],
        &origin::defaults(),
        &[],
    )?;
    assert!(!before.definitions[0].is_empty());
    assert!(!before.occurrences[0].is_empty());

    conn.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('000-collision.rs','rust-file','production','repository','code','rust')",
        [],
    )?;
    let rust_file_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO chunks(
           file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(?1,'function','collisionNeedle','','collisionNeedle',0,80,1,3,
                  'rust-chunk','fn collisionNeedle() { collisionNeedle(); }')",
        [rust_file_id],
    )?;
    let rust_chunk_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO chunks_fts(rowid,content,name,symbols,path)
         VALUES(?1,'fn collisionNeedle() { collisionNeedle(); }',
                'collisionNeedle','collisionNeedle','000-collision.rs')",
        [rust_chunk_id],
    )?;
    conn.execute(
        "INSERT INTO symbols(
           file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
         ) VALUES(?1,'collisionNeedle','function',3,18,0,40,'',1,1)",
        [rust_file_id],
    )?;
    conn.execute(
        "INSERT INTO refs(
           file_id,chunk_id,start,line,kind,confidence,target_name,local
         ) VALUES(?1,?2,24,1,'use','certain','collisionNeedle',1)",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;
    conn.execute(
        "INSERT INTO member_calls(file_id,chunk_id,start,line,prop)
         VALUES(?1,?2,25,1,'collisionNeedle')",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;
    conn.execute(
        "INSERT INTO entity_sites(
           file_id,chunk_id,start,end,line,end_line,plane,entity_type,role,
           identity_kind,identity_name,identity_start,target_name,target_start,
           extractor,provenance,confidence
         ) VALUES(?1,?2,26,41,1,1,'general','symbol','use','reference',
                  'collisionNeedle',26,'collisionNeedle',26,
                  'fixture','fixture','certain')",
        rusqlite::params![rust_file_id, rust_chunk_id],
    )?;

    let after = super::exact_intent_candidates(
        &conn,
        "collisionNeedle",
        32,
        &[],
        &origin::defaults(),
        &[],
    )?;
    assert_eq!(after.identifiers, before.identifiers);
    assert_eq!(after.definitions, before.definitions);
    assert_eq!(after.occurrences, before.occurrences);
    assert!(
        !after
            .definitions
            .iter()
            .chain(&after.occurrences)
            .flatten()
            .any(|chunk_id| *chunk_id == rust_chunk_id)
    );
    Ok(())
}

#[test]
fn rust_lexical_hits_do_not_advertise_or_seed_structural_graph_calls() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("native.rs"),
        "pub fn lexical_native_marker() { println!(\"native marker\"); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = search(
        &conn,
        None,
        "lexical native marker",
        &SearchOptions {
            rerank: false,
            include_memory: false,
            expand: true,
            compact: true,
            include_neighborhood_followups: true,
            ..SearchOptions::default()
        },
    )?;
    let hit = result
        .hits
        .iter()
        .find(|hit| hit.file == "native.rs")
        .expect("Rust lexical hit");
    assert!(hit.anchors.is_empty());
    assert!(hit.file_anchor.is_none());
    assert!(!hit.include_neighborhood_followup);
    let expansion = result.expansion.as_ref().expect("requested expansion");
    assert!(expansion.seeds.is_empty());
    assert!(expansion.nodes.is_empty());
    assert!(expansion.edges.is_empty());

    let compact = crate::compact::search_value(&result);
    let rendered_hit = compact["hits"]
        .as_array()
        .and_then(|hits| hits.iter().find(|hit| hit["at"] == "native.rs:1"))
        .expect("compact Rust hit");
    assert!(rendered_hit.get("anchor").is_none());
    let calls = rendered_hit["followups"]["calls"]
        .as_array()
        .expect("file follow-up calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["tool"], "file_outline");
    Ok(())
}

#[test]
fn ranked_format_scope_filters_lexical_and_exact_candidates_before_limits() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("definition.ts"),
        "export function formatScopeNeedle() { return true; }\n",
    )?;
    fs::write(
        repo.path().join("caller.js"),
        "export const result = formatScopeNeedle();\n",
    )?;
    fs::write(
        repo.path().join("native.rs"),
        "pub fn formatScopeNeedle() -> bool { true }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let scoped = |formats: Vec<String>| {
        search(
            &conn,
            None,
            "formatScopeNeedle",
            &SearchOptions {
                formats,
                limit: 20,
                rerank: false,
                ..Default::default()
            },
        )
    };

    let rust = scoped(vec!["rust".into()])?;
    assert!(!rust.hits.is_empty());
    assert!(rust.hits.iter().all(|hit| hit.file == "native.rs"));
    assert!(rust.hits.iter().all(|hit| {
        !matches!(
            hit.match_reason,
            MatchReason::ExactDefinition | MatchReason::ExactOccurrence
        )
    }));
    assert!(rust.hits.iter().all(|hit| {
        hit.anchors.is_empty() && hit.file_anchor.is_none() && !hit.include_neighborhood_followup
    }));

    let typescript = scoped(vec!["typescript".into()])?;
    assert!(!typescript.hits.is_empty());
    assert!(
        typescript
            .hits
            .iter()
            .all(|hit| hit.file == "definition.ts")
    );
    assert!(
        typescript
            .hits
            .iter()
            .any(|hit| hit.match_reason == MatchReason::ExactDefinition)
    );

    let ecmascript = scoped(vec!["typescript".into(), "javascript".into()])?;
    assert!(
        ecmascript
            .hits
            .iter()
            .all(|hit| hit.file.ends_with(".ts") || hit.file.ends_with(".js"))
    );
    assert!(ecmascript.hits.iter().any(|hit| hit.file == "caller.js"));
    assert!(
        ecmascript
            .hits
            .iter()
            .any(|hit| hit.file == "definition.ts")
    );

    let all = scoped(Vec::new())?;
    assert!(all.hits.iter().any(|hit| hit.file == "native.rs"));
    assert!(all.hits.iter().any(|hit| hit.file == "caller.js"));
    assert!(all.hits.iter().any(|hit| hit.file == "definition.ts"));
    Ok(())
}

#[test]
fn format_scope_rejects_unknown_and_document_formats() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const marker = true;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    for format in ["markdown", "unknown"] {
        let error = search(
            &conn,
            None,
            "marker",
            &SearchOptions {
                formats: vec![format.into()],
                rerank: false,
                ..Default::default()
            },
        )
        .expect_err("non-code format must be rejected");
        assert_eq!(
            error.to_string(),
            format!("code format must be one of: javascript, typescript, rust; got `{format}`")
        );
    }
    Ok(())
}

#[test]
fn rust_only_ranked_scope_disables_vector_before_provider_resolution() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("native.rs"),
        "pub fn rustOnlyVectorBypassMarker() {}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let provider = embed::Provider::from_settings(
        &EmbeddingSettings {
            provider: Some("local".into()),
            model: Some("unavailable/test-model".into()),
            revision: Some("test-revision".into()),
            url: None,
            api_key_env: None,
            query_prefix: None,
            batch: 64,
            origins: origin::defaults(),
        },
        &InferenceSettings {
            url: "http://127.0.0.1:9".into(),
            host: "127.0.0.1".into(),
            port: 9,
            project: None,
            uv: "uv".into(),
            allow_remote: false,
            batch_size: 16,
            max_length: 4_096,
            model_cache_root: None,
        },
    )?
    .expect("local provider");

    let result = search(
        &conn,
        Some(&provider),
        "rustOnlyVectorBypassMarker",
        &SearchOptions {
            formats: vec!["rust".into()],
            rerank: false,
            ..Default::default()
        },
    )?;
    assert_eq!(result.retrieval.vector, "disabled");
    assert!(!result.hits.is_empty());
    assert!(result.hits.iter().all(|hit| hit.file == "native.rs"));
    Ok(())
}

#[test]
fn rust_test_module_filename_is_tagged_and_filtered_before_ranking() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src"))?;
    fs::write(
        repo.path().join("src/main.rs"),
        "pub fn shared_rust_role_marker() -> bool { true }\n",
    )?;
    fs::write(
        repo.path().join("src/tests.rs"),
        "pub fn shared_rust_role_marker() -> bool { false }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let role = conn.query_row(
        "SELECT role FROM files WHERE path='src/tests.rs'",
        [],
        |row| row.get::<_, String>(0),
    )?;
    assert_eq!(role, "test");

    let scoped = |role: &str| {
        search(
            &conn,
            None,
            "shared_rust_role_marker",
            &SearchOptions {
                file_roles: vec![role.into()],
                formats: vec!["rust".into()],
                rerank: false,
                ..Default::default()
            },
        )
    };
    let test_only = scoped("test")?;
    assert!(!test_only.hits.is_empty());
    assert!(
        test_only
            .hits
            .iter()
            .all(|hit| hit.file == "src/tests.rs" && hit.file_role == "test")
    );
    let production_only = scoped("production")?;
    assert!(!production_only.hits.is_empty());
    assert!(
        production_only
            .hits
            .iter()
            .all(|hit| hit.file == "src/main.rs" && hit.file_role == "production")
    );
    Ok(())
}

#[test]
fn local_reranker_uses_resolved_inference_endpoint_and_pool_policy() {
    let reranker = Reranker::from_settings(
        &RerankerSettings {
            url: None,
            model: "example/reranker".to_string(),
            revision: Some("revision".to_string()),
            top: 27,
            max_chars: 1234,
        },
        &EmbeddingSettings {
            provider: Some("local".to_string()),
            model: Some("example/embed".to_string()),
            revision: None,
            url: None,
            api_key_env: None,
            query_prefix: None,
            batch: 64,
            origins: origin::defaults(),
        },
        &InferenceSettings {
            url: "http://127.0.0.1:9912/".to_string(),
            host: "127.0.0.1".to_string(),
            port: 9912,
            project: None,
            uv: "uv".to_string(),
            allow_remote: false,
            batch_size: 16,
            max_length: 4096,
            model_cache_root: None,
        },
    )
    .expect("local reranker");
    assert_eq!(reranker.url, "http://127.0.0.1:9912/rerank");
    assert_eq!(reranker.model, "example/reranker");
    assert_eq!(reranker.pool, 27);
    assert_eq!(reranker.max_chars, 1234);
}

fn insert_repository_policy(
    conn: &rusqlite::Connection,
    file_id: i64,
    role: &str,
    suffix: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scout_runs(
           scout_kind,status,gateway_protocol,provider,model,billing_path,
           prompt_version,source_snapshot,input_fingerprint,request_hash,
           config_json,started_at,completed_at
         ) VALUES('repository','completed',1,'test','test','custom','test',
                  'snapshot',?1,?1,'{}','now','now')",
        [format!("search-policy-{suffix}")],
    )?;
    let run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO repository_classifications(
           run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
           explanation,citations_json,evidence_fingerprint,
           classification_fingerprint,source_snapshot,created_at
         ) VALUES(?1,?2,'area','{}',0,?3,'likely','test','[\"E001\"]',
                  ?2,?2,'snapshot','now')",
        rusqlite::params![run_id, format!("area:{suffix}"), role],
    )?;
    let classification_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO repository_file_policy(
           file_id,classification_id,subject_key,scope_role,effective_role,
           source_hash,depth
         ) VALUES(?1,?2,?3,?4,?4,'hash',0)",
        rusqlite::params![file_id, classification_id, format!("area:{suffix}"), role],
    )?;
    Ok(())
}

#[test]
fn vector_retrieval_status_distinguishes_active_disabled_and_degraded() {
    let disabled = RetrievalStatus::vector_disabled();
    assert_eq!(disabled.vector, "disabled");
    assert_eq!(disabled.reranker, "disabled");
    assert!(disabled.vector_action.is_none());

    let mut rankings = Vec::new();
    let active = record_vector_ranking(
        &mut rankings,
        Ok(embed::VectorSearchResult {
            ranking: vec![(7, 0.9)],
            timings: embed::VectorSearchTimings::default(),
        }),
    );
    assert_eq!(active.vector, "active");
    assert_eq!(rankings, vec![vec![(7, 0.9)]]);

    let degraded = record_vector_ranking(
        &mut rankings,
        Err(anyhow::anyhow!("profile is not materialized")),
    );
    assert_eq!(degraded.vector, "degraded");
    assert!(degraded.vector_action.is_some());

    let mut reranked = degraded;
    reranked.reranker_active();
    assert_eq!(reranked.reranker, "active");
    reranked.reranker_degraded();
    assert_eq!(reranked.reranker, "degraded");
}

#[test]
fn vector_candidates_are_overfetched_before_role_filtering() {
    assert_eq!(candidate_pool_limits(8, false), (50, 50));
    assert_eq!(candidate_pool_limits(8, true), (50, 200));
}

#[test]
fn reranking_a_prefix_preserves_every_unreranked_tail_candidate() {
    let fused = (1..=60).map(|id| (id, id as f64)).collect::<Vec<_>>();
    let reranked = (1..=50)
        .rev()
        .map(|id| (id, -(id as f64)))
        .collect::<Vec<_>>();
    let merged = merge_reranked_prefix(&fused, reranked);
    assert_eq!(merged.len(), 60);
    assert_eq!(merged[0].0, 50);
    assert_eq!(merged[49].0, 1);
    assert_eq!(
        merged[50..]
            .iter()
            .map(|(chunk_id, _)| *chunk_id)
            .collect::<Vec<_>>(),
        (51..=60).collect::<Vec<_>>()
    );
}

#[test]
fn exact_identifier_intent_does_not_promote_plain_prose() {
    assert_eq!(exact_intent_tokens("insert"), ["insert"]);
    assert_eq!(exact_intent_tokens("insert()"), ["insert"]);
    assert_eq!(exact_intent_tokens("`insert`"), ["insert"]);
    assert_eq!(
        exact_intent_tokens(
            "find createRouteTypesManifest and NextTypesPlugin in root_layout files"
        ),
        ["createRouteTypesManifest", "NextTypesPlugin", "root_layout"]
    );
    assert!(exact_intent_tokens("development cache behavior").is_empty());
    assert_eq!(
        exact_intent_tokens("CreateRouteTypesManifest"),
        ["CreateRouteTypesManifest"]
    );
    assert!(contains_code_identifier(
        "const state = { collectedRootParams: {} };",
        "collectedRootParams"
    ));
    assert!(contains_code_identifier(
        "state.collectedRootParams = next;",
        "collectedRootParams"
    ));
    assert!(!contains_code_identifier(
        "// collectedRootParams\nconst label = 'collectedRootParams';",
        "collectedRootParams"
    ));
    assert!(!contains_code_identifier(
        "const collectedRootParamsExtra = true;",
        "collectedRootParams"
    ));
}

#[test]
fn exact_tiers_survive_hostile_hybrid_order_and_cover_identifiers() {
    let exact = super::ExactIntentCandidates {
        identifiers: vec!["firstThing".into(), "SecondThing".into()],
        definitions: vec![vec![1, 4], vec![2, 5]],
        occurrences: vec![vec![6, 8], vec![7]],
    };
    let ranked = tiered_candidates(exact, &[(9, 100.0), (7, 90.0), (8, 80.0), (2, -10.0)]);
    assert_eq!(
        ranked
            .iter()
            .map(|candidate| candidate.chunk_id)
            .collect::<Vec<_>>(),
        [1, 2, 4, 5, 8, 7, 6, 9]
    );
    assert!(
        ranked[..4]
            .iter()
            .all(|candidate| candidate.match_reason == MatchReason::ExactDefinition)
    );
    assert!(
        ranked[4..7]
            .iter()
            .all(|candidate| candidate.match_reason == MatchReason::ExactOccurrence)
    );
    assert_eq!(ranked[7].match_reason, MatchReason::Hybrid);
}

#[test]
fn exact_identifier_search_precedes_examples_and_preserves_ambiguity() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("manifest.ts"),
        "export function createRouteTypesManifest() { return true; }\n\
         export function getRootParamsFromLayouts() { return {}; }\n\
         export const collectedRootParams = new Map();\n",
    )?;
    fs::write(
        repo.path().join("plugin-a.ts"),
        "export class NextTypesPlugin { apply() {} }\n",
    )?;
    fs::write(
        repo.path().join("plugin-b.ts"),
        "export class NextTypesPlugin { apply() { return 'second'; } }\n",
    )?;
    fs::write(
        repo.path().join("caller.ts"),
        "import { createRouteTypesManifest } from './manifest';\n\
         export function callManifest() { return createRouteTypesManifest(); }\n",
    )?;
    fs::write(
        repo.path().join("sitecore-example.ts"),
        "export const sitecoreExample = 'createRouteTypesManifest NextTypesPlugin collectedRootParams';\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let multi = search(
        &conn,
        None,
        "createRouteTypesManifest getRootParamsFromLayouts collectedRootParams NextTypesPlugin",
        &SearchOptions {
            limit: 4,
            include_memory: false,
            ..Default::default()
        },
    )?;
    assert_eq!(multi.hits.len(), 4);
    assert!(
        multi
            .hits
            .iter()
            .all(|hit| hit.match_reason == MatchReason::ExactDefinition)
    );
    let covered = multi
        .hits
        .iter()
        .flat_map(|hit| hit.matched_identifiers.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    assert_eq!(
        covered,
        HashSet::from([
            "createRouteTypesManifest",
            "getRootParamsFromLayouts",
            "collectedRootParams",
            "NextTypesPlugin",
        ])
    );
    assert!(
        multi
            .hits
            .iter()
            .all(|hit| hit.file != "sitecore-example.ts")
    );
    let compact = crate::compact::search_value(&multi);
    assert_eq!(compact["default_match"], "hybrid");
    assert_eq!(compact["hits"][0]["match"], "exact_definition");
    assert!(compact["hits"][0]["matched_identifiers"].is_array());

    let ambiguous = search(
        &conn,
        None,
        "NextTypesPlugin",
        &SearchOptions {
            limit: 2,
            include_memory: false,
            ..Default::default()
        },
    )?;
    assert_eq!(ambiguous.hits.len(), 2);
    assert!(ambiguous.hits.iter().all(|hit| {
        hit.match_reason == MatchReason::ExactDefinition
            && hit.name.as_deref() == Some("NextTypesPlugin")
    }));

    let occurrence = search(
        &conn,
        None,
        "createRouteTypesManifest",
        &SearchOptions {
            limit: 3,
            include_memory: false,
            ..Default::default()
        },
    )?;
    assert_eq!(
        occurrence.hits[0].match_reason,
        MatchReason::ExactDefinition
    );
    assert!(occurrence.hits.iter().any(|hit| {
        hit.file == "caller.ts" && hit.match_reason == MatchReason::ExactOccurrence
    }));

    let mixed_intent = search(
        &conn,
        None,
        "locate createRouteTypesManifest behavior",
        &SearchOptions {
            limit: 3,
            include_memory: false,
            ..Default::default()
        },
    )?;
    assert_eq!(
        mixed_intent
            .hits
            .iter()
            .filter(|hit| hit.match_reason == MatchReason::ExactOccurrence)
            .count(),
        1
    );

    fs::write(
        repo.path().join("state.ts"),
        "export const state = { registeredOptions: {} as Record<string, boolean> };\n\
         export function update() { state.registeredOptions['x'] = true; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let property_occurrence = search(
        &conn,
        None,
        "registeredOptions",
        &SearchOptions {
            limit: 3,
            include_memory: false,
            ..Default::default()
        },
    )?;
    assert!(property_occurrence.hits.iter().any(|hit| {
        hit.file == "state.ts"
            && hit.match_reason == MatchReason::ExactOccurrence
            && hit
                .matched_identifiers
                .iter()
                .any(|identifier| identifier == "registeredOptions")
    }));
    Ok(())
}

#[test]
fn exhaustive_search_pages_the_complete_lexical_chunk_set_and_binds_its_cursor() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for (path, name) in [("a.ts", "alpha"), ("b.ts", "beta"), ("c.ts", "gamma")] {
        fs::write(
            repo.path().join(path),
            format!("export function {name}() {{ return pagingNeedle; }}\n"),
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut cursor = None;
    let mut cursor_tokens = HashSet::new();
    let mut chunk_ids = Vec::new();
    let mut files = Vec::new();
    let mut total_chunks = None;
    loop {
        let result = search(
            &conn,
            None,
            "pagingNeedle",
            &SearchOptions {
                mode: SearchMode::Exhaustive {
                    cursor: cursor.clone(),
                },
                limit: 2,
                rerank: false,
                compact: true,
                include_neighborhood_followups: false,
                response_byte_limit: 1_000_000,
                file_origins: vec!["workspace".into(), "repository".into(), "workspace".into()],
                ..Default::default()
            },
        )?;
        let metadata = result.exhaustive.as_ref().expect("exhaustive metadata");
        assert_eq!(metadata.returned, result.hits.len());
        assert_eq!(metadata.effective.page_size, 2);
        assert!(!metadata.effective.vector);
        assert!(!metadata.effective.rerank);
        assert!(!metadata.effective.expand);
        assert!(!metadata.effective.include_memory);
        assert_eq!(metadata.scope.corpus, "indexed_chunks");
        assert_eq!(metadata.scope.file_roles, SearchScopeFileRoles::All);
        assert_eq!(metadata.scope.origins, ["repository", "workspace"]);
        assert_eq!(metadata.scope.formats, SearchScopeFormats::All);
        assert_eq!(metadata.scope.snapshot, result.snapshot);
        if let Some(expected) = total_chunks {
            assert_eq!(metadata.total_chunks, expected);
        } else {
            assert!(metadata.total_chunks > 2);
            total_chunks = Some(metadata.total_chunks);
            let compact = crate::compact::search_value(&result);
            assert_eq!(compact["default_match"], "lexical");
            assert_eq!(compact["scope"]["file_roles"], "all");
            assert_eq!(compact["scope"]["formats"], "all");
            assert_eq!(compact["effective"]["page_size"], 2);
            assert_eq!(compact["total_chunks"], metadata.total_chunks);
            assert_eq!(compact["returned"], metadata.returned);
            assert_eq!(compact["truncated"], metadata.truncated);
            assert!(
                compact["hits"]
                    .as_array()
                    .is_some_and(|hits| { hits.iter().all(|hit| hit.get("match").is_none()) })
            );
        }
        for hit in &result.hits {
            assert!(chunk_ids.iter().all(|chunk_id| chunk_id != &hit.chunk_id));
            chunk_ids.push(hit.chunk_id);
            files.push(hit.file.clone());
        }
        let next = metadata.next_cursor.clone();
        assert_eq!(metadata.truncated, next.is_some());
        let Some(next) = next else {
            break;
        };
        assert!(cursor_tokens.insert(next.clone()));
        assert_ne!(cursor.as_deref(), Some(next.as_str()));
        cursor = Some(next);
    }
    assert_eq!(chunk_ids.len(), total_chunks.expect("first page count"));
    assert_eq!(files, ["a.ts", "b.ts", "c.ts"]);

    let first = search(
        &conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 2,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert_eq!(
        first
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>(),
        ["a.ts", "b.ts"]
    );
    let cursor_chunk_id = first.hits.last().expect("cursor hit").chunk_id;
    let original_snapshot = first.snapshot.clone();
    let first_cursor = first
        .exhaustive
        .as_ref()
        .and_then(|metadata| metadata.next_cursor.clone())
        .expect("continuation cursor");
    let publication_before_docs = crate::publication::Identities::read(&conn)?.publication;
    fs::write(
        repo.path().join("README.md"),
        "# Paging\n\nDocumentation changes must not invalidate code cursors.\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let after_docs = crate::publication::Identities::read(&conn)?;
    assert_eq!(after_docs.code, original_snapshot);
    assert_ne!(after_docs.publication, publication_before_docs);
    let resumed_after_docs = search(
        &conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor.clone()),
            },
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert_eq!(
        resumed_after_docs
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>(),
        ["c.ts"]
    );
    let wrong_query = search(
        &conn,
        None,
        "differentNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor.clone()),
            },
            limit: 1,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )
    .expect_err("cursor must bind the query");
    assert!(wrong_query.to_string().contains("query and scope"));
    let wrong_scope = search(
        &conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor.clone()),
            },
            limit: 1,
            file_roles: vec!["production".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )
    .expect_err("cursor must bind the normalized scope");
    assert!(wrong_scope.to_string().contains("query and scope"));

    // A content-equivalent database can assign the old row ID to a different
    // chunk. Continuation must relocate the logical cursor chunk by stable
    // source identity rather than trusting that disposable ID.
    let rebuilt_repo = tempfile::tempdir()?;
    fs::write(
        rebuilt_repo.path().join("0-temporary.ts"),
        "export const temporary = pagingNeedle;\n",
    )?;
    for (path, name) in [("a.ts", "alpha"), ("b.ts", "beta"), ("c.ts", "gamma")] {
        fs::write(
            rebuilt_repo.path().join(path),
            format!("export function {name}() {{ return pagingNeedle; }}\n"),
        )?;
    }
    let rebuilt_conn = store::open(rebuilt_repo.path())?;
    indexer::index_repo(rebuilt_repo.path(), &rebuilt_conn)?;
    fs::remove_file(rebuilt_repo.path().join("0-temporary.ts"))?;
    indexer::index_repo(rebuilt_repo.path(), &rebuilt_conn)?;
    assert_eq!(
        crate::structural::current_snapshot(&rebuilt_conn)?,
        original_snapshot
    );
    let reassigned_path: String = rebuilt_conn.query_row(
        "SELECT file.path
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE chunk.id=?1",
        [cursor_chunk_id],
        |row| row.get(0),
    )?;
    assert_eq!(reassigned_path, "a.ts", "fixture must reuse the old ID");
    let resumed_after_reassignment = search(
        &rebuilt_conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor.clone()),
            },
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert_eq!(
        resumed_after_reassignment
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>(),
        ["c.ts"]
    );

    // An edit followed by an exact revert restores the snapshot while leaving
    // the old chunk ID absent. The same cursor must still resume after b.ts.
    fs::write(
        repo.path().join("b.ts"),
        "export function beta() { return pagingNeedle + changed; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    fs::write(
        repo.path().join("b.ts"),
        "export function beta() { return pagingNeedle; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(
        crate::structural::current_snapshot(&conn)?,
        original_snapshot
    );
    let old_row_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE id=?1",
        [cursor_chunk_id],
        |row| row.get(0),
    )?;
    assert_eq!(old_row_count, 0, "fixture must retire the old ID");
    let resumed_after_revert = search(
        &conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor.clone()),
            },
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert_eq!(
        resumed_after_revert
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>(),
        ["c.ts"]
    );

    fs::write(
        repo.path().join("d.ts"),
        "export function delta() { return pagingNeedle; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let stale = search(
        &conn,
        None,
        "pagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(first_cursor),
            },
            limit: 1,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )
    .expect_err("cursor must fail against a changed snapshot");
    assert!(stale.to_string().contains("snapshot changed"));
    Ok(())
}

fn exhaustive_budget_fixture() -> Result<(tempfile::TempDir, rusqlite::Connection)> {
    let repo = tempfile::tempdir()?;
    for (path, name) in [
        ("a.ts", "alpha"),
        ("b.ts", "bravo"),
        ("c.ts", "charlie"),
        ("d.ts", "delta"),
    ] {
        let terminal_only = if path == "a.ts" {
            "export const terminalOnlyNeedle = true;\n"
        } else {
            ""
        };
        fs::write(
            repo.path().join(path),
            format!("export function {name}() {{ return strictBudgetNeedle; }}\n{terminal_only}"),
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    Ok((repo, conn))
}

fn exhaustive_budget_search(
    conn: &rusqlite::Connection,
    cursor: Option<String>,
    limit: usize,
    compact: bool,
    response_byte_limit: usize,
) -> Result<SearchResult> {
    exhaustive_budget_search_query(
        conn,
        "strictBudgetNeedle",
        cursor,
        limit,
        compact,
        response_byte_limit,
    )
}

fn exhaustive_budget_search_query(
    conn: &rusqlite::Connection,
    query: &str,
    cursor: Option<String>,
    limit: usize,
    compact: bool,
    response_byte_limit: usize,
) -> Result<SearchResult> {
    search(
        conn,
        None,
        query,
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor },
            limit,
            rerank: false,
            compact,
            response_byte_limit,
            ..Default::default()
        },
    )
}

#[test]
fn exhaustive_search_emits_complete_handoff_only_on_the_first_page() -> Result<()> {
    let (_repo, conn) = exhaustive_budget_fixture()?;

    let first_page = exhaustive_budget_search(&conn, None, 1, true, 1_000_000)?;
    let first_value = crate::compact::search_value(&first_page);
    assert!(first_value["hits"][0]["followups"]["arguments"].is_object());
    let first_cursor = first_page
        .exhaustive
        .as_ref()
        .and_then(|metadata| metadata.next_cursor.clone())
        .expect("first page cursor");
    let second_page = exhaustive_budget_search(&conn, Some(first_cursor), 1, true, 1_000_000)?;
    let second_value = crate::compact::search_value(&second_page);
    assert_eq!(second_page.hits[0].file, "b.ts");
    assert!(second_value["hits"][0]["followups"]["tools"].is_array());
    assert!(
        second_value["hits"][0]["followups"]
            .get("arguments")
            .is_none()
    );
    Ok(())
}

#[test]
fn exhaustive_budget_locator_floor_advances_from_the_last_rendered_hit() -> Result<()> {
    let (_repo, conn) = exhaustive_budget_fixture()?;

    let below_floor = exhaustive_budget_search(&conn, None, 3, true, 1)
        .expect_err("one byte cannot fit an exhaustive locator");
    let floor = *below_floor
        .downcast_ref::<ResponseBudgetTooSmall>()
        .expect("typed exhaustive budget error");
    assert_eq!(floor.byte_limit, 1);
    assert!(floor.minimum_bytes > 1);
    assert_eq!(
        below_floor.to_string(),
        format!(
            "response_budget_too_small: response byte limit 1 cannot fit the minimum exhaustive response; minimum_bytes={}",
            floor.minimum_bytes
        )
    );

    let locator_page = exhaustive_budget_search(&conn, None, 3, true, floor.minimum_bytes)?;
    let locator_metadata = locator_page
        .exhaustive
        .as_ref()
        .expect("exhaustive locator metadata");
    assert_eq!(locator_page.hits.len(), 1);
    assert_eq!(locator_page.hits[0].file, "a.ts");
    assert_eq!(locator_metadata.returned, 1);
    assert!(locator_metadata.truncated);
    let locator_cursor = locator_metadata
        .next_cursor
        .clone()
        .expect("locator continuation cursor");
    assert!(locator_page.response_budget.exhaustive_locator_only);
    assert_eq!(
        locator_page.response_budget.rendered_bytes,
        floor.minimum_bytes
    );
    let locator_value = crate::compact::search_value(&locator_page);
    let locator = locator_value["hits"][0]
        .as_object()
        .expect("compact exhaustive locator");
    assert!(locator.contains_key("at"));
    assert!(locator.contains_key("anchor") || locator.contains_key("anchors"));
    assert!(!locator.contains_key("followups"));

    let continuation = exhaustive_budget_search(&conn, Some(locator_cursor), 3, true, 1_000_000)?;
    assert_eq!(
        continuation
            .hits
            .iter()
            .map(|hit| hit.file.as_str())
            .collect::<Vec<_>>(),
        ["b.ts", "c.ts", "d.ts"]
    );
    assert!(
        crate::compact::search_value(&continuation)["hits"]
            .as_array()
            .expect("continuation hits")
            .iter()
            .all(|hit| hit["followups"].get("arguments").is_none())
    );

    let one_below = exhaustive_budget_search(&conn, None, 3, true, floor.minimum_bytes - 1)
        .expect_err("one byte below the locator floor must fail deterministically");
    let one_below = one_below
        .downcast_ref::<ResponseBudgetTooSmall>()
        .expect("typed boundary error");
    assert_eq!(one_below.minimum_bytes, floor.minimum_bytes);
    Ok(())
}

#[test]
fn exhaustive_budget_floor_handles_terminal_and_diagnostic_envelopes() -> Result<()> {
    let (_repo, conn) = exhaustive_budget_fixture()?;

    // A terminal page can become smaller when every locator is retained:
    // shedding one hit adds both an opaque cursor and omission metadata. The
    // advertised floor must therefore be the smallest rendered candidate
    // seen across the whole shedding pass, not blindly the one-hit size.
    let terminal_error = exhaustive_budget_search(&conn, None, 4, true, 1)
        .expect_err("one byte cannot fit the terminal exhaustive page");
    let terminal_floor = terminal_error
        .downcast_ref::<ResponseBudgetTooSmall>()
        .expect("typed terminal-page budget error")
        .minimum_bytes;
    let terminal_page = exhaustive_budget_search(&conn, None, 4, true, terminal_floor)?;
    assert_eq!(terminal_page.response_budget.rendered_bytes, terminal_floor);
    assert!(terminal_page.response_budget.exhaustive_locator_only);
    let terminal_one_below = exhaustive_budget_search(&conn, None, 4, true, terminal_floor - 1)
        .expect_err("one byte below the terminal-page floor must fail");
    assert_eq!(
        terminal_one_below
            .downcast_ref::<ResponseBudgetTooSmall>()
            .expect("typed terminal boundary error")
            .minimum_bytes,
        terminal_floor
    );

    // Diagnostic JSON serializes `byte_limit` itself. Its reported floor must
    // already include the extra digits introduced when the caller retries at
    // that value.
    let debug_error = exhaustive_budget_search(&conn, None, 3, false, 1)
        .expect_err("one byte cannot fit diagnostic exhaustive JSON");
    let debug_floor = debug_error
        .downcast_ref::<ResponseBudgetTooSmall>()
        .expect("typed diagnostic budget error")
        .minimum_bytes;
    let debug_page = exhaustive_budget_search(&conn, None, 3, false, debug_floor)?;
    assert_eq!(
        serde_json::to_string_pretty(&debug_page)?.len(),
        debug_page.response_budget.rendered_bytes
    );
    assert_eq!(debug_page.response_budget.rendered_bytes, debug_floor);
    let debug_one_below = exhaustive_budget_search(&conn, None, 3, false, debug_floor - 1)
        .expect_err("one byte below the diagnostic floor must fail");
    assert_eq!(
        debug_one_below
            .downcast_ref::<ResponseBudgetTooSmall>()
            .expect("typed diagnostic boundary error")
            .minimum_bytes,
        debug_floor
    );
    Ok(())
}

#[test]
fn exhaustive_budget_floor_includes_initial_and_empty_responses() -> Result<()> {
    let (_repo, conn) = exhaustive_budget_fixture()?;
    for query in ["terminalOnlyNeedle", "absentBudgetNeedle"] {
        for compact in [true, false] {
            let error = exhaustive_budget_search_query(&conn, query, None, 10, compact, 1)
                .expect_err("one byte cannot fit an exhaustive response");
            let minimum = error
                .downcast_ref::<ResponseBudgetTooSmall>()
                .expect("typed exhaustive budget error")
                .minimum_bytes;
            let exact = exhaustive_budget_search_query(&conn, query, None, 10, compact, minimum)?;
            assert_eq!(exact.response_budget.rendered_bytes, minimum);
            if query == "absentBudgetNeedle" {
                assert!(exact.hits.is_empty());
                assert!(!exact.response_budget.truncated);
            } else {
                assert_eq!(exact.hits.len(), 1);
            }
            let one_below =
                exhaustive_budget_search_query(&conn, query, None, 10, compact, minimum - 1)
                    .expect_err("one byte below the exhaustive floor must fail");
            assert_eq!(
                one_below
                    .downcast_ref::<ResponseBudgetTooSmall>()
                    .expect("typed exhaustive boundary error")
                    .minimum_bytes,
                minimum
            );
        }
    }
    Ok(())
}

#[test]
fn exhaustive_search_normalizes_role_and_origin_scope_and_enforces_page_ceiling() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for path in ["production.ts", "test.ts", "dependency.ts"] {
        fs::write(
            repo.path().join(path),
            "export const scopedNeedle = true;\n",
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    conn.execute("UPDATE files SET role='test' WHERE path='test.ts'", [])?;
    conn.execute(
        "UPDATE files SET origin='dependency' WHERE path='dependency.ts'",
        [],
    )?;

    let production = search(
        &conn,
        None,
        "scopedNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            file_roles: vec!["production".into(), "production".into()],
            file_origins: vec!["repository".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let diagnostic = serde_json::to_value(&production)?;
    assert_eq!(diagnostic["total_chunks"], 1);
    assert_eq!(diagnostic["returned"], 1);
    assert_eq!(diagnostic["truncated"], false);
    assert!(diagnostic["next_cursor"].is_null());
    assert_eq!(diagnostic["effective"]["vector"], false);
    assert_eq!(
        diagnostic["scope"]["file_roles"],
        serde_json::json!(["production"])
    );
    let metadata = production.exhaustive.expect("exhaustive metadata");
    assert_eq!(metadata.total_chunks, 1);
    assert_eq!(metadata.returned, 1);
    assert!(!metadata.truncated);
    assert_eq!(
        metadata.scope.file_roles,
        SearchScopeFileRoles::Selected(vec!["production".into()])
    );
    assert_eq!(metadata.scope.origins, ["repository"]);
    assert_eq!(production.hits[0].file, "production.ts");

    let dependency = search(
        &conn,
        None,
        "scopedNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            file_origins: vec!["dependency".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let metadata = dependency.exhaustive.expect("exhaustive metadata");
    assert_eq!(metadata.total_chunks, 1);
    assert_eq!(metadata.scope.origins, ["dependency"]);
    assert_eq!(dependency.hits[0].file, "dependency.ts");

    let too_large = search(
        &conn,
        None,
        "scopedNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: MAX_EXHAUSTIVE_PAGE_SIZE + 1,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )
    .expect_err("exhaustive page size must have a hard ceiling");
    assert!(too_large.to_string().contains("page size"));
    Ok(())
}

#[test]
fn exhaustive_format_scope_filters_counts_echoes_and_binds_cursor() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for path in ["a.ts", "b.ts"] {
        fs::write(
            repo.path().join(path),
            "export const formatPagingNeedle = true;\n",
        )?;
    }
    fs::write(
        repo.path().join("native.rs"),
        "pub const formatPagingNeedle: bool = true;\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let first = search(
        &conn,
        None,
        "formatPagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 1,
            formats: vec!["typescript".into(), "typescript".into()],
            rerank: false,
            compact: true,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let metadata = first.exhaustive.as_ref().expect("exhaustive metadata");
    assert_eq!(metadata.total_chunks, 2);
    assert_eq!(
        metadata.scope.formats,
        SearchScopeFormats::Selected(vec!["typescript".into()])
    );
    assert_eq!(first.hits.len(), 1);
    assert!(first.hits[0].file.ends_with(".ts"));
    let compact = crate::compact::search_value(&first);
    assert_eq!(
        compact["scope"]["formats"],
        serde_json::json!(["typescript"])
    );
    let cursor = metadata.next_cursor.clone().expect("continuation cursor");

    let wrong_scope = search(
        &conn,
        None,
        "formatPagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(cursor.clone()),
            },
            limit: 1,
            formats: vec!["rust".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )
    .expect_err("cursor must bind the normalized format scope");
    assert!(wrong_scope.to_string().contains("query and scope"));

    let continuation = search(
        &conn,
        None,
        "formatPagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive {
                cursor: Some(cursor),
            },
            limit: 1,
            formats: vec!["typescript".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert_eq!(continuation.hits.len(), 1);
    assert!(continuation.hits[0].file.ends_with(".ts"));
    assert_ne!(continuation.hits[0].file, first.hits[0].file);

    let rust = search(
        &conn,
        None,
        "formatPagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            formats: vec!["rust".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let rust_metadata = rust.exhaustive.as_ref().expect("Rust exhaustive metadata");
    assert_eq!(rust_metadata.total_chunks, 1);
    assert_eq!(
        rust_metadata.scope.formats,
        SearchScopeFormats::Selected(vec!["rust".into()])
    );
    assert_eq!(rust.hits[0].file, "native.rs");
    assert!(rust.hits[0].anchors.is_empty());
    assert!(rust.hits[0].file_anchor.is_none());

    let explicit_all = search(
        &conn,
        None,
        "formatPagingNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            formats: vec!["rust".into(), "javascript".into(), "typescript".into()],
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let all_metadata = explicit_all
        .exhaustive
        .as_ref()
        .expect("all-format exhaustive metadata");
    assert_eq!(all_metadata.total_chunks, 3);
    assert_eq!(all_metadata.scope.formats, SearchScopeFormats::All);
    Ok(())
}

#[test]
fn exhaustive_search_clamps_only_an_omitted_configured_limit() {
    let oversized = MAX_EXHAUSTIVE_PAGE_SIZE + 1;
    assert_eq!(
        resolve_search_limit(true, None, oversized),
        MAX_EXHAUSTIVE_PAGE_SIZE
    );
    assert_eq!(resolve_search_limit(true, Some(oversized), 10), oversized);
    assert_eq!(resolve_search_limit(false, None, oversized), oversized);
}

#[test]
fn exhaustive_fts_query_scopes_every_term_to_content() {
    assert_eq!(
        exhaustive_fts_query("alpha beta.gamma"),
        "content:\"alpha\" OR content:\"beta\" OR content:\"gamma\""
    );
}

#[test]
fn broad_or_warning_is_first_page_only_and_requires_distinct_terms_and_threshold() {
    let warnings = exhaustive_warnings("history.cache HISTORY", 200, true);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "broad_or_query");
    assert_eq!(warnings[0].terms, ["history", "cache"]);
    assert_eq!(warnings[0].total_chunks, 200);
    assert_eq!(
        warnings[0].message,
        "Exhaustive search OR-joins FTS terms. Refine or abandon this traversal if that is not the intended evidence set."
    );

    assert!(exhaustive_warnings("history.cache", 199, true).is_empty());
    assert!(exhaustive_warnings("history.history", 200, true).is_empty());
    assert!(exhaustive_warnings("history.cache", 200, false).is_empty());
}

#[test]
fn exhaustive_search_surfaces_broad_or_warning_only_on_the_initial_page() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for index in 0..200 {
        fs::write(
            repo.path().join(format!("subject-{index:03}.ts")),
            "export const history = { cache: true };\n",
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let options = |cursor| SearchOptions {
        mode: SearchMode::Exhaustive { cursor },
        limit: 1,
        rerank: false,
        compact: true,
        response_byte_limit: 1_000_000,
        ..Default::default()
    };

    let first = search(&conn, None, "history.cache", &options(None))?;
    let metadata = first.exhaustive.as_ref().expect("exhaustive metadata");
    assert!(metadata.total_chunks >= 200);
    assert_eq!(metadata.warnings.len(), 1);
    let compact = crate::compact::search_value(&first);
    assert_eq!(compact["warnings"][0]["code"], "broad_or_query");
    assert_eq!(
        compact["warnings"][0]["terms"],
        serde_json::json!(["history", "cache"])
    );
    assert_eq!(
        compact["warnings"][0]["total_chunks"],
        metadata.total_chunks
    );

    let second = search(
        &conn,
        None,
        "history.cache",
        &options(metadata.next_cursor.clone()),
    )?;
    assert!(
        second
            .exhaustive
            .as_ref()
            .expect("continuation metadata")
            .warnings
            .is_empty()
    );
    assert!(
        crate::compact::search_value(&second)
            .get("warnings")
            .is_none()
    );
    Ok(())
}

#[test]
fn exhaustive_search_reports_unique_match_lines_without_rich_decorations() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function links_iteration() {\n  const same = links_iteration + links_iteration;\n  return links_iteration;\n}\n",
    )?;
    fs::write(
        repo.path().join("b.ts"),
        "import { links_iteration } from './a';\nexport function caller() {\n  return links_iteration();\n}\n",
    )?;
    fs::write(
        repo.path().join("pathOnlyNeedle.ts"),
        "export const unrelated = true;\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let exhaustive = search(
        &conn,
        None,
        "LINKS_ITERATION",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: MAX_EXHAUSTIVE_PAGE_SIZE,
            rerank: false,
            compact: true,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let observed_lines = exhaustive
        .hits
        .iter()
        .flat_map(|hit| {
            hit.match_lines
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|line| (hit.file.clone(), *line))
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        observed_lines,
        HashSet::from([
            ("a.ts".into(), 1),
            ("a.ts".into(), 2),
            ("a.ts".into(), 3),
            ("b.ts".into(), 1),
            ("b.ts".into(), 3),
        ])
    );
    assert!(exhaustive.hits.iter().all(|hit| {
        hit.snippet.is_empty()
            && hit.uses.is_empty()
            && hit.used_by.is_empty()
            && hit.match_lines.is_some()
    }));
    let compact = crate::compact::search_value(&exhaustive);
    for hit in compact["hits"].as_array().expect("compact hits") {
        let object = hit.as_object().expect("compact locator hit");
        assert!(object.contains_key("at"));
        assert!(object.contains_key("kind"));
        assert!(object.contains_key("match_lines"));
        assert!(object.contains_key("anchor") || object.contains_key("anchors"));
        assert!(object.keys().all(|key| matches!(
            key.as_str(),
            "at" | "kind" | "match_lines" | "anchor" | "anchors" | "followups"
        )));
    }

    let ranked = search(
        &conn,
        None,
        "links_iteration",
        &SearchOptions {
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let rich_definition = ranked
        .hits
        .iter()
        .find(|hit| hit.file == "a.ts" && hit.name.as_deref() == Some("links_iteration"))
        .expect("ranked definition hit");
    assert!(!rich_definition.snippet.is_empty());
    assert!(!rich_definition.used_by.is_empty());
    assert!(rich_definition.match_lines.is_none());

    let ranked_path_only = search(
        &conn,
        None,
        "pathOnlyNeedle",
        &SearchOptions {
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert!(
        ranked_path_only
            .hits
            .iter()
            .any(|hit| hit.file == "pathOnlyNeedle.ts")
    );

    let path_only = search(
        &conn,
        None,
        "pathOnlyNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    assert!(path_only.hits.is_empty());
    let path_only_metadata = path_only.exhaustive.expect("exhaustive metadata");
    assert_eq!(path_only_metadata.total_chunks, 0);
    assert_eq!(path_only_metadata.returned, 0);
    assert!(!path_only_metadata.truncated);
    assert!(path_only_metadata.next_cursor.is_none());
    Ok(())
}

#[test]
fn exhaustive_match_lines_survive_source_sentinels_and_nul_bytes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("sentinel.ts"),
        "// \u{1e}jscout-match-start\u{1f}\nexport const collisionNeedle = true;\n",
    )?;
    fs::write(
        repo.path().join("nul.ts"),
        "export function wrapper() {\n  // prefix\0suffix\n  return collisionNeedle;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let (canonical_content, searchable_content): (String, String) = conn.query_row(
        "SELECT chunk.content, chunks_fts.content
         FROM chunks_fts
         JOIN chunks chunk ON chunk.id=chunks_fts.rowid
         JOIN files file ON file.id=chunk.file_id
         WHERE file.path='nul.ts'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert!(canonical_content.contains('\0'));
    assert!(!searchable_content.contains('\0'));
    assert_eq!(
        canonical_content
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
        searchable_content
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    );

    let result = search(
        &conn,
        None,
        "collisionNeedle",
        &SearchOptions {
            mode: SearchMode::Exhaustive { cursor: None },
            limit: 10,
            rerank: false,
            response_byte_limit: 1_000_000,
            ..Default::default()
        },
    )?;
    let match_lines = result
        .hits
        .iter()
        .map(|hit| {
            (
                hit.file.as_str(),
                hit.match_lines.as_deref().unwrap_or_default(),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(match_lines["sentinel.ts"], [2]);
    assert_eq!(match_lines["nul.ts"], [3]);
    Ok(())
}

#[test]
fn search_projects_chunks_to_snapshot_scoped_anchors() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function greet(name) { return name; }\n",
    )?;
    fs::write(
        repo.path().join("b.ts"),
        "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = search(
        &conn,
        None,
        "greet",
        &SearchOptions {
            limit: 8,
            ..Default::default()
        },
    )?;
    assert_eq!(result.snapshot.len(), 64);
    let definition = result
        .hits
        .iter()
        .find(|hit| hit.file == "a.ts" && hit.name.as_deref() == Some("greet"))
        .expect("greet definition hit");
    assert_eq!(definition.file_anchor.as_deref(), Some("file:a.ts"));
    assert_eq!(definition.anchors, vec!["sym:a.ts#::greet@1"]);
    assert_eq!(definition.used_by, vec!["greet: 1 sites"]);
    assert!(approximate_name_usage_occurrences(&conn, &result.hits)? > 0);
    assert_eq!(
        result
            .hits
            .iter()
            .filter(|hit| hit.include_followups)
            .count(),
        1
    );
    let compact = crate::compact::search_value(&result);
    let compact_hits = compact["hits"].as_array().expect("compact hits");
    assert_eq!(
        compact_hits
            .iter()
            .filter(|hit| hit["followups"].get("arguments").is_some())
            .count(),
        1
    );
    assert!(compact_hits.iter().all(|hit| {
        hit.get("anchors").is_some()
            || hit["followups"]["tools"].is_array()
            || hit["followups"]["calls"].is_array()
    }));
    assert!(result.expansion.is_none());
    Ok(())
}

#[test]
fn expansion_uses_one_global_node_edge_and_byte_budget() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function greet(name) { return name; }\n",
    )?;
    fs::write(
        repo.path().join("b.ts"),
        "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = search(
        &conn,
        None,
        "greet run",
        &SearchOptions {
            mode: super::SearchMode::Ranked,
            limit: 8,
            expand: true,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            formats: Vec::new(),
            include_memory: true,
            memory_limit: 4,
            memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
            memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            rerank: true,
            reranker: None,
            timing: false,
            compact: true,
            include_neighborhood_followups: true,
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
            expansion: ExpansionOptions {
                projection: ExpansionProjection::Paths,
                depth: 1,
                seed_limit: 3,
                path_limit: DEFAULT_EXPANSION_PATH_LIMIT,
                node_limit: 2,
                edge_limit: 1,
                byte_limit: 1_500,
                min_confidence: "likely".into(),
                file_roles: file_role::DEFAULT_EXPANSION
                    .iter()
                    .map(|role| (*role).to_string())
                    .collect(),
                file_origins: origin::defaults(),
            },
        },
    )?;
    let expansion = result.expansion.expect("expansion context pack");
    assert!(expansion.nodes.len() <= 2);
    assert_eq!(expansion.edges.len(), 1);
    assert!(expansion.edges.iter().all(|edge| {
        expansion.nodes.iter().any(|node| node.key == edge.source)
            && expansion.nodes.iter().any(|node| node.key == edge.target)
    }));
    assert!(expansion.payload_bytes <= 1_500);
    assert!(expansion.truncated);

    let byte_starved = search(
        &conn,
        None,
        "greet",
        &SearchOptions {
            mode: super::SearchMode::Ranked,
            limit: 8,
            expand: true,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            formats: Vec::new(),
            include_memory: true,
            memory_limit: 4,
            memory_graph_depth: DEFAULT_MEMORY_GRAPH_DEPTH,
            memory_graph_node_limit: DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
            rerank: true,
            reranker: None,
            timing: false,
            compact: false,
            include_neighborhood_followups: true,
            response_byte_limit: DEFAULT_RESPONSE_BYTE_LIMIT,
            expansion: ExpansionOptions {
                byte_limit: 1,
                ..Default::default()
            },
        },
    )?
    .expansion
    .expect("expansion context pack");
    assert!(byte_starved.nodes.is_empty());
    assert!(byte_starved.edges.is_empty());
    assert!(byte_starved.payload_bytes <= 1);
    assert!(byte_starved.truncated);
    Ok(())
}

#[test]
fn path_projection_prioritizes_cross_file_continuations_and_keeps_the_full_mode() -> Result<()> {
    let node = |key: &str, file: &str, relevance: f64| GraphNode {
        key: key.into(),
        kind: "symbol".into(),
        display_name: key.into(),
        file: Some(file.into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: Some(1),
        meta: serde_json::json!({}),
        relevance,
    };
    let edge = |source: &str, target: &str, relevance: f64| GraphEdge {
        source: source.into(),
        target: target.into(),
        kind: "call".into(),
        confidence: "certain".into(),
        provenance: "parser".into(),
        file: Some("src/entry.ts".into()),
        line: Some(1),
        detail: serde_json::json!({}),
        relevance,
    };
    let nodes = vec![
        node("root", "src/entry.ts", 1.0),
        node("noise", "src/entry.ts", 0.95),
        node("bridge", "src/entry.ts", 0.9),
        node("effect", "src/effect.ts", 0.8),
    ];
    let edges = vec![
        edge("root", "noise", 0.95),
        edge("root", "bridge", 0.9),
        edge("bridge", "effect", 0.8),
    ];
    let seeds = vec!["root".to_string()];
    let options = ExpansionOptions {
        projection: ExpansionProjection::Paths,
        depth: 2,
        seed_limit: 1,
        path_limit: 1,
        node_limit: 10,
        edge_limit: 10,
        byte_limit: 24_000,
        min_confidence: "likely".into(),
        file_roles: vec!["production".into()],
        file_origins: origin::defaults(),
    };

    let paths = select_path_projection(&seeds, &nodes, &edges, &options, true)?;
    assert_eq!(paths.selected_paths, 1);
    assert_eq!(paths.candidate_paths, 3);
    assert_eq!(
        paths
            .nodes
            .iter()
            .map(|node| node.key.as_str())
            .collect::<HashSet<_>>(),
        HashSet::from(["root", "bridge", "effect"])
    );
    assert_eq!(paths.edges.len(), 2);
    assert!(paths.edges.iter().all(|edge| edge.target != "noise"));

    let neighborhood =
        select_neighborhood_projection(&seeds, &nodes, &edges, &options, true, &paths)?;
    assert_eq!(neighborhood.nodes.len(), 4);
    assert_eq!(neighborhood.edges.len(), 3);
    Ok(())
}

#[test]
fn path_projection_covers_each_seed_before_repeating_one_seed() -> Result<()> {
    let node = |key: &str, file: &str| GraphNode {
        key: key.into(),
        kind: "symbol".into(),
        display_name: key.into(),
        file: Some(file.into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: Some(1),
        meta: serde_json::json!({}),
        relevance: 1.0,
    };
    let edge = |source: &str, target: &str, relevance: f64| GraphEdge {
        source: source.into(),
        target: target.into(),
        kind: "call".into(),
        confidence: "certain".into(),
        provenance: "parser".into(),
        file: Some("src/entry.ts".into()),
        line: Some(1),
        detail: serde_json::json!({}),
        relevance,
    };
    let nodes = vec![
        node("root-a", "src/a.ts"),
        node("root-b", "src/b.ts"),
        node("a-first", "src/a-first.ts"),
        node("a-second", "src/a-second.ts"),
        node("b-first", "src/b-first.ts"),
    ];
    let edges = vec![
        edge("root-a", "a-first", 0.99),
        edge("root-a", "a-second", 0.98),
        edge("root-b", "b-first", 0.5),
    ];
    let seeds = vec!["root-a".to_string(), "root-b".to_string()];
    let options = ExpansionOptions {
        projection: ExpansionProjection::Paths,
        depth: 1,
        seed_limit: 2,
        path_limit: 2,
        node_limit: 4,
        edge_limit: 10,
        byte_limit: 24_000,
        min_confidence: "likely".into(),
        file_roles: vec!["production".into()],
        file_origins: origin::defaults(),
    };
    let selection = select_path_projection(&seeds, &nodes, &edges, &options, true)?;

    let selected = selection
        .nodes
        .iter()
        .map(|node| node.key.as_str())
        .collect::<HashSet<_>>();
    assert!(selected.contains("a-first"));
    assert!(selected.contains("b-first"));
    assert!(!selected.contains("a-second"));
    assert_eq!(selection.selected_paths, 2);
    assert_eq!(selection.candidate_paths, 3);

    // Without reserving the path forest, global edge order fills the last
    // node slot from root-a and omits root-b's weaker continuation. The
    // diagnostic projection must retain every compact path before adding
    // any remaining fan-out.
    let neighborhood =
        select_neighborhood_projection(&seeds, &nodes, &edges, &options, true, &selection)?;
    let neighborhood_nodes = neighborhood
        .nodes
        .iter()
        .map(|node| node.key.as_str())
        .collect::<HashSet<_>>();
    assert!(
        selection
            .nodes
            .iter()
            .all(|node| neighborhood_nodes.contains(node.key.as_str()))
    );
    let neighborhood_edges = neighborhood
        .edges
        .iter()
        .map(edge_identity)
        .collect::<HashSet<_>>();
    assert!(
        selection
            .edges
            .iter()
            .all(|edge| neighborhood_edges.contains(&edge_identity(edge)))
    );
    Ok(())
}

#[test]
fn attached_memory_requires_direct_graph_or_artifact_relation_evidence() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("entry.ts"),
        "import { nearby } from './nearby';\nexport function entry() { return nearby(); }\n",
    )?;
    fs::write(
        repo.path().join("nearby.ts"),
        "export function nearby() { return 1; }\n",
    )?;
    fs::write(
        repo.path().join("unrelated.ts"),
        "export function unrelated() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let anchor = |name: &str| -> Result<String> {
        Ok(conn.query_row(
            "SELECT node_key FROM graph_nodes
             WHERE node_kind='symbol' AND display_name=?1
             ORDER BY node_key LIMIT 1",
            [name],
            |row| row.get(0),
        )?)
    };
    let entry = anchor("entry")?;
    let nearby = anchor("nearby")?;
    let unrelated = anchor("unrelated")?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    for id in [1_i64, 2, 3, 4] {
        conn.execute(
            "INSERT INTO semantic_artifacts(
               id,artifact_type,canonical_name,body_json,model,prompt_version,
               confidence,source_snapshot,created_at,input_fingerprint,artifact_fingerprint
             ) VALUES(?1,'card',?2,'{}','test','test/v1','likely',?3,'now',?2,?2)",
            rusqlite::params![id, format!("artifact-{id}"), snapshot],
        )?;
    }
    conn.execute(
        "INSERT INTO semantic_relations(
           src_artifact_id,dst_artifact_id,relation,claim_path,confidence,dst_fingerprint
         ) VALUES(4,1,'related_to','/related','likely','artifact-1')",
        [],
    )?;

    let support = |anchor: &str, file: &str| SemanticSupport {
        claim_path: "/claim".into(),
        anchor: anchor.into(),
        relationship: "defining-evidence".into(),
        role: None,
        evidence_file: file.into(),
        evidence_start_line: 1,
        evidence_end_line: 1,
        source_hash: "h".repeat(64),
        context_hash: "c".repeat(64),
        confidence: "likely".into(),
        freshness: "fresh".into(),
    };
    let artifact = |id: i64, score: f64, supports: Vec<SemanticSupport>| -> SemanticArtifact {
        SemanticArtifact {
            id,
            supersedes: None,
            artifact_type: if id == 4 { "concept" } else { "card" }.into(),
            name: Some(format!("artifact-{id}")),
            trust: "untrusted-semantic-memory".into(),
            body: serde_json::json!({ "purpose": format!("artifact {id}") }),
            model: "test".into(),
            prompt_version: "test/v1".into(),
            confidence: "likely".into(),
            source_snapshot: snapshot.clone(),
            created_at: "now".into(),
            freshness: "fresh".into(),
            supports,
            retrieval_score: Some(ArtifactRetrievalScore {
                rank_score: score,
                lexical_score: Some(score),
                vector_cosine: None,
            }),
        }
    };
    let hit = Hit {
        chunk_id: 1,
        file: "entry.ts".into(),
        file_role: "production".into(),
        repository_role: None,
        file_origin: "repository".into(),
        kind: "function".into(),
        name: Some("entry".into()),
        start_line: 2,
        end_line: 2,
        score: 1.0,
        match_reason: MatchReason::Hybrid,
        matched_identifiers: Vec::new(),
        match_lines: None,
        snippet: "entry() { return nearby(); }".into(),
        snippet_truncated: false,
        anchors: vec![entry.clone()],
        file_anchor: Some("file:entry.ts".into()),
        uses: Vec::new(),
        used_by: Vec::new(),
        include_followups: true,
        include_neighborhood_followup: true,
    };
    let candidates = vec![
        artifact(3, 1.0, vec![support(&unrelated, "unrelated.ts")]),
        artifact(2, 0.8, vec![support(&nearby, "nearby.ts")]),
        artifact(4, 0.9, Vec::new()),
        artifact(1, 0.1, vec![support(&entry, "entry.ts")]),
    ];
    let (selected, status) =
        select_attached_memory(&conn, candidates, &[hit], 4, 2, 2_000, &origin::defaults())?;
    assert_eq!(
        selected
            .iter()
            .map(|artifact| artifact.id)
            .collect::<Vec<_>>(),
        [1, 2, 4]
    );
    assert_eq!(status.status, "connected");
    assert_eq!(status.connected_candidates, 3);

    let disconnected = artifact(3, 1.0, vec![support(&unrelated, "unrelated.ts")]);
    let (_, status) = select_attached_memory(
        &conn,
        vec![disconnected],
        &[],
        4,
        2,
        2_000,
        &origin::defaults(),
    )?;
    assert_eq!(status.status, "no_connected_memory");
    assert_eq!(status.connected_candidates, 0);
    Ok(())
}

#[test]
fn response_budget_caps_the_complete_rendered_search_envelope() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = format!("export const needle = '{}';\n", "x".repeat(12_000));
    fs::write(repo.path().join("large.ts"), source)?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = search(
        &conn,
        None,
        "needle",
        &SearchOptions {
            limit: 8,
            response_byte_limit: 2_000,
            ..Default::default()
        },
    )?;
    let rendered = serde_json::to_string_pretty(&result)?;
    assert!(rendered.len() <= 2_000);
    assert_eq!(rendered.len(), result.response_budget.rendered_bytes);
    assert!(result.response_budget.unbudgeted_bytes > rendered.len());
    assert!(result.response_budget.truncated);
    assert!(result.response_budget.truncated_snippets > 0);
    serde_json::from_str::<serde_json::Value>(&rendered)?;
    Ok(())
}

#[test]
fn response_budget_removes_low_ranked_subgraphs_not_all_edges() -> Result<()> {
    let node = |key: &str, relevance: f64| GraphNode {
        key: key.into(),
        kind: "symbol".into(),
        display_name: key.into(),
        file: None,
        file_role: None,
        file_origin: None,
        line: None,
        meta: serde_json::json!({}),
        relevance,
    };
    let edge = |target: &str, relevance: f64, padding: usize| GraphEdge {
        source: "root".into(),
        target: target.into(),
        kind: "call".into(),
        confidence: "certain".into(),
        provenance: "test".into(),
        file: None,
        line: None,
        detail: serde_json::json!({ "padding": "x".repeat(padding) }),
        relevance,
    };
    let mut result = SearchResult {
        snapshot: "s".repeat(64),
        requested_formats: Vec::new(),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: Vec::new(),
        semantic_artifacts: Vec::new(),
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 0,
        semantic_selected: 0,
        expansion: Some(SearchExpansion {
            projection: ExpansionProjection::Neighborhood,
            seeds: vec!["root".into()],
            nodes: vec![node("root", 1.0), node("high", 0.8), node("low", 0.1)],
            edges: vec![edge("high", 0.8, 0), edge("low", 0.1, 1_200)],
            candidate_paths: 0,
            selected_paths: 0,
            omitted_paths: 0,
            omitted_nodes: 0,
            omitted_edges: 0,
            selected_path_edges: Vec::new(),
            node_limit: 3,
            edge_limit: 2,
            byte_limit: 10_000,
            file_roles: vec!["production".into(), "unknown".into()],
            file_origins: origin::defaults(),
            payload_bytes: 0,
            truncated: false,
        }),
        response_budget: ResponseBudget {
            byte_limit: 2_200,
            ..Default::default()
        },
    };

    apply_response_budget(&mut result, false)?;
    let expansion = result.expansion.expect("expansion");
    assert!(expansion.nodes.iter().any(|node| node.key == "high"));
    assert!(!expansion.nodes.iter().any(|node| node.key == "low"));
    assert_eq!(expansion.edges.len(), 1);
    assert_eq!(expansion.edges[0].target, "high");
    assert!(result.response_budget.rendered_bytes <= 2_200);
    Ok(())
}

#[test]
fn response_budget_preserves_primary_code_before_memory() -> Result<()> {
    let mut result = SearchResult {
        snapshot: "s".repeat(64),
        requested_formats: Vec::new(),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: vec![Hit {
            chunk_id: 1,
            file: "src/large.ts".into(),
            file_role: "production".into(),
            repository_role: None,
            file_origin: "repository".into(),
            kind: "function".into(),
            name: Some("largeHit".into()),
            start_line: 1,
            end_line: 200,
            score: 1.0,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
            match_lines: None,
            snippet: "x".repeat(8_000),
            snippet_truncated: false,
            anchors: vec!["sym:src/large.ts#::largeHit@1".into()],
            file_anchor: Some("file:src/large.ts".into()),
            uses: vec!["helper (call)".into()],
            used_by: vec!["caller: 1 site".into()],
            include_followups: true,
            include_neighborhood_followup: true,
        }],
        semantic_artifacts: vec![SemanticArtifact {
            id: 1,
            supersedes: None,
            artifact_type: "workflow".into(),
            name: Some("checkout lifecycle".into()),
            trust: "untrusted-semantic-memory".into(),
            body: serde_json::json!({
                "participants": [{
                    "anchor": "sym:src/checkout.ts#::checkout@1",
                    "role": "workflow entry",
                    "scope": "defining"
                }]
            }),
            model: "agent-reported".into(),
            prompt_version: "annotate/v2".into(),
            confidence: "likely".into(),
            source_snapshot: "s".repeat(64),
            created_at: "2026-08-09T00:00:00Z".into(),
            freshness: "fresh".into(),
            supports: Vec::new(),
            retrieval_score: Some(ArtifactRetrievalScore {
                rank_score: 1.0,
                lexical_score: Some(1.0),
                vector_cosine: None,
            }),
        }],
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 1,
        semantic_selected: 1,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 2_000,
            ..Default::default()
        },
    };

    apply_response_budget(&mut result, false)?;
    assert!(result.semantic_artifacts.is_empty());
    assert_eq!(result.response_budget.omitted_semantic_artifacts, 1);
    assert_eq!(result.hits.len(), 1);
    assert!(result.hits[0].snippet_truncated);
    assert!(result.response_budget.truncated);
    assert!(result.response_budget.rendered_bytes <= 2_000);
    Ok(())
}

#[test]
fn compact_budget_sheds_followups_before_primary_hit_identity() -> Result<()> {
    let mut result = SearchResult {
        snapshot: "s".repeat(64),
        requested_formats: Vec::new(),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: vec![Hit {
            chunk_id: 1,
            file: "src/target.ts".into(),
            file_role: "production".into(),
            repository_role: None,
            file_origin: "repository".into(),
            kind: "function".into(),
            name: Some("target".into()),
            start_line: 10,
            end_line: 12,
            score: 1.0,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
            match_lines: None,
            snippet: "export function target() { return 1; }".into(),
            snippet_truncated: false,
            anchors: vec!["sym:src/target.ts#::target@1".into()],
            file_anchor: Some("file:src/target.ts".into()),
            uses: Vec::new(),
            used_by: Vec::new(),
            include_followups: true,
            include_neighborhood_followup: true,
        }],
        semantic_artifacts: Vec::new(),
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 0,
        semantic_selected: 0,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 425,
            ..Default::default()
        },
    };

    apply_response_budget(&mut result, true)?;
    assert_eq!(result.hits.len(), 1);
    assert!(!result.hits[0].include_followups);
    assert_eq!(result.response_budget.omitted_followups, 1);
    let compact = crate::compact::search_value(&result);
    assert!(compact["hits"][0]["followups"]["tools"].is_array());
    assert!(compact["hits"][0]["followups"].get("arguments").is_none());
    assert!(result.response_budget.rendered_bytes <= 425);
    let sections = crate::compact::search_section_bytes(&result)?;
    assert_eq!(
        sections.hits_bytes
            + sections.graph_bytes
            + sections.memory_bytes
            + sections.envelope_bytes,
        sections.total_bytes
    );
    assert_eq!(sections.total_bytes, serde_json::to_vec(&compact)?.len());
    Ok(())
}

#[test]
fn search_caps_rendered_semantic_supports_even_under_a_large_byte_budget() -> Result<()> {
    let supports = (0..20)
        .map(|index| SemanticSupport {
            claim_path: format!("/claims/{index}"),
            anchor: format!("sym:src/workflow.ts#::step{index}@{}", index + 1),
            relationship: "supporting-stage-evidence".into(),
            role: Some(format!("workflow stage {index}")),
            evidence_file: "src/workflow.ts".into(),
            evidence_start_line: index + 1,
            evidence_end_line: index + 1,
            source_hash: "s".repeat(64),
            context_hash: "c".repeat(64),
            confidence: "likely".into(),
            freshness: "fresh".into(),
        })
        .collect();
    let artifact = SemanticArtifact {
        id: 1,
        supersedes: None,
        artifact_type: "workflow".into(),
        name: Some("large workflow".into()),
        trust: "untrusted-semantic-memory".into(),
        body: serde_json::json!({ "purpose": "exercise bounded rendering" }),
        model: "agent-reported".into(),
        prompt_version: "annotate/v2".into(),
        confidence: "likely".into(),
        source_snapshot: "s".repeat(64),
        created_at: "2026-08-13T00:00:00Z".into(),
        freshness: "fresh".into(),
        supports,
        retrieval_score: Some(ArtifactRetrievalScore {
            rank_score: 1.0,
            lexical_score: Some(1.0),
            vector_cosine: None,
        }),
    };
    let mut second_artifact = artifact.clone();
    second_artifact.id = 2;
    second_artifact.name = Some("second workflow".into());
    let mut result = SearchResult {
        snapshot: "s".repeat(64),
        requested_formats: Vec::new(),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: Vec::new(),
        semantic_artifacts: vec![artifact, second_artifact],
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 2,
        semantic_selected: 2,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 100_000,
            ..Default::default()
        },
    };

    apply_response_budget(&mut result, false)?;
    assert_eq!(
        result
            .semantic_artifacts
            .iter()
            .map(|artifact| artifact.supports.len())
            .sum::<usize>(),
        8
    );
    assert_eq!(result.semantic_artifacts[0].supports.len(), 4);
    assert_eq!(result.semantic_artifacts[1].supports.len(), 4);
    assert_eq!(result.response_budget.omitted_semantic_supports, 32);
    assert!(result.response_budget.truncated);
    assert!(result.response_budget.unbudgeted_bytes > result.response_budget.rendered_bytes);
    Ok(())
}

#[test]
fn file_roles_tag_hits_and_filter_search_and_expansion_before_budgets() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src"))?;
    fs::create_dir_all(repo.path().join("tests"))?;
    fs::write(
        repo.path().join("src/service.ts"),
        "export function performRoleFilteredWork() { return 1; }\n",
    )?;
    fs::write(
        repo.path().join("tests/service.test.ts"),
        "import { performRoleFilteredWork } from '../src/service';\nexport function exerciseRoleFilteredWork() { return performRoleFilteredWork(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let production_chunk = conn.query_row(
        "SELECT chunk.id FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='src/service.ts' ORDER BY chunk.id LIMIT 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let (_, reranker_text) =
        reranker_document(&conn, production_chunk, 4_000)?.expect("indexed reranker candidate");
    assert!(reranker_text.contains("path: src/service.ts"));
    assert!(reranker_text.contains("scope: src"));
    assert!(reranker_text.contains("symbol: performRoleFilteredWork"));
    assert!(reranker_text.contains("kind: function"));
    assert!(reranker_text.contains("role: production"));
    assert!(reranker_text.contains("origin: repository"));

    let test_chunk = conn.query_row(
        "SELECT chunk.id FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='tests/service.test.ts' ORDER BY chunk.id LIMIT 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let mut fused_candidates = vec![(test_chunk, 0.9), (production_chunk, 0.8)];
    prefilter_ranking_by_role(&conn, &mut fused_candidates, &["production".into()])?;
    assert_eq!(fused_candidates, vec![(production_chunk, 0.8)]);

    let test_only = search(
        &conn,
        None,
        "performRoleFilteredWork",
        &SearchOptions {
            file_roles: vec!["test".into()],
            ..Default::default()
        },
    )?;
    assert!(!test_only.hits.is_empty());
    assert!(test_only.hits.iter().all(|hit| hit.file_role == "test"));

    let production_expansion = search(
        &conn,
        None,
        "performRoleFilteredWork",
        &SearchOptions {
            expand: true,
            ..Default::default()
        },
    )?
    .expansion
    .expect("expansion context pack");
    assert_eq!(
        production_expansion.file_roles,
        vec!["production".to_string(), "unknown".to_string()]
    );
    assert!(production_expansion.nodes.iter().all(|node| {
        node.file_role
            .as_deref()
            .is_none_or(|role| matches!(role, "production" | "unknown"))
    }));

    let all_roles = search(
        &conn,
        None,
        "performRoleFilteredWork",
        &SearchOptions {
            expand: true,
            expansion: ExpansionOptions {
                file_roles: Vec::new(),
                ..Default::default()
            },
            ..Default::default()
        },
    )?
    .expansion
    .expect("expansion context pack");
    assert!(
        all_roles
            .nodes
            .iter()
            .any(|node| node.file_role.as_deref() == Some("test"))
    );
    Ok(())
}

#[test]
fn fresh_repository_policy_ranks_and_describes_effective_roles() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("docs"))?;
    fs::create_dir_all(repo.path().join("src"))?;
    fs::write(
        repo.path().join("docs/runtime.ts"),
        "export function sharedReconNeedle() { return 'runtime'; }\n",
    )?;
    fs::write(
        repo.path().join("src/tool.ts"),
        "export function sharedReconNeedle() { return 'tooling'; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let (runtime_file, runtime_chunk): (i64, i64) = conn.query_row(
        "SELECT file.id, chunk.id FROM files file JOIN chunks chunk ON chunk.file_id=file.id
         WHERE file.path='docs/runtime.ts' ORDER BY chunk.id LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (tool_file, tool_chunk): (i64, i64) = conn.query_row(
        "SELECT file.id, chunk.id FROM files file JOIN chunks chunk ON chunk.file_id=file.id
         WHERE file.path='src/tool.ts' ORDER BY chunk.id LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    insert_repository_policy(&conn, runtime_file, "runtime", "runtime")?;
    insert_repository_policy(&conn, tool_file, "tooling", "tooling")?;

    let mut ranking = vec![(tool_chunk, 1.0), (runtime_chunk, 0.9)];
    apply_repository_policy_penalty(&conn, &mut ranking)?;
    assert_eq!(ranking[0].0, runtime_chunk);

    let (_, document) =
        reranker_document(&conn, runtime_chunk, 4_000)?.expect("runtime recon candidate");
    assert!(document.contains("role: runtime"));
    assert!(document.contains("deterministic_role: documentation"));

    let result = search(
        &conn,
        None,
        "sharedReconNeedle",
        &SearchOptions {
            limit: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(result.hits[0].file, "docs/runtime.ts");
    assert_eq!(result.hits[0].repository_role.as_deref(), Some("runtime"));
    assert!(result.hits.iter().any(|hit| {
        hit.file == "src/tool.ts" && hit.repository_role.as_deref() == Some("tooling")
    }));
    Ok(())
}
