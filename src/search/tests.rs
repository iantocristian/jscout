use std::{collections::HashSet, fs};

use anyhow::Result;

use crate::embed;

use super::{
    DEFAULT_EXPANSION_PATH_LIMIT, DEFAULT_MEMORY_GRAPH_DEPTH, DEFAULT_MEMORY_GRAPH_NODE_LIMIT,
    DEFAULT_RESPONSE_BYTE_LIMIT, ExpansionOptions, ExpansionProjection, Hit, MatchReason, Reranker,
    ResponseBudget, RetrievalStatus, SearchExpansion, SearchOptions, SearchResult,
    apply_repository_policy_penalty, apply_response_budget, approximate_name_usage_occurrences,
    candidate_pool_limits, contains_code_identifier, edge_identity, exact_intent_tokens,
    merge_reranked_prefix, prefilter_ranking_by_role, record_vector_ranking, reranker_document,
    search, select_attached_memory, select_neighborhood_projection, select_path_projection,
    tiered_candidates,
};
use crate::config::{EmbeddingSettings, InferenceSettings, RerankerSettings};
use crate::{
    file_role, indexer, origin,
    semantic::{ArtifactRetrievalScore, SemanticArtifact, SemanticSupport},
    store,
    structural::{GraphEdge, GraphNode},
};

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
    assert_eq!(definition.file_anchor, "file:a.ts");
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
            limit: 8,
            expand: true,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
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
            limit: 8,
            expand: true,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
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
        snippet: "entry() { return nearby(); }".into(),
        snippet_truncated: false,
        anchors: vec![entry.clone()],
        file_anchor: "file:entry.ts".into(),
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
            snippet: "x".repeat(8_000),
            snippet_truncated: false,
            anchors: vec!["sym:src/large.ts#::largeHit@1".into()],
            file_anchor: "file:src/large.ts".into(),
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
            snippet: "export function target() { return 1; }".into(),
            snippet_truncated: false,
            anchors: vec!["sym:src/target.ts#::target@1".into()],
            file_anchor: "file:src/target.ts".into(),
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
