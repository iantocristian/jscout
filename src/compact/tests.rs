use serde_json::json;

use super::{
    compact_hit, compact_receiver_types, definition_string, graph_value, neighborhood_value,
    prune_unreferenced_nodes, render_neighborhood, search_string, semantic_preview,
    settle_neighborhood_bytes, who_uses_string,
};
use crate::{
    origin,
    query::{SymbolTarget, Usage},
    scout::RenderedSource,
    search::{Hit, MatchReason, ResponseBudget, RetrievalStatus, SearchExpansion, SearchResult},
    semantic::{ArtifactRetrievalScore, SemanticArtifact, SemanticSupport},
    structural::{GraphEdge, GraphNode, Neighborhood},
};

fn node(key: &str, line: i64) -> GraphNode {
    GraphNode {
        key: key.into(),
        kind: "symbol".into(),
        display_name: key.rsplit("::").next().unwrap_or(key).into(),
        file: Some("src/workflow.ts".into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: Some(line),
        meta: serde_json::json!({ "diagnostic_padding": "x".repeat(800) }),
        relevance: 1.0,
    }
}

fn edge(source: &str, target: &str, line: i64) -> GraphEdge {
    GraphEdge {
        source: source.into(),
        target: target.into(),
        kind: "member_call".into(),
        confidence: "likely".into(),
        provenance: "typescript-checker".into(),
        file: Some("src/workflow.ts".into()),
        line: Some(line),
        detail: serde_json::json!({
            "receiverTypes": ["QueueService"],
            "projects": (0..20).map(|index| format!("tsconfig-{index}.json")).collect::<Vec<_>>(),
            "occurrenceSpecific": true,
        }),
        relevance: 1.0,
    }
}

#[test]
fn compact_graph_reuses_source_locators_already_carried_by_anchors() {
    let file = GraphNode {
        key: "file:src/workflow.ts".into(),
        kind: "file".into(),
        display_name: "src/workflow.ts".into(),
        file: Some("src/workflow.ts".into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: None,
        meta: json!({}),
        relevance: 1.0,
    };
    let hub = GraphNode {
        key: "event:ready".into(),
        kind: "event".into(),
        display_name: "ready".into(),
        file: Some("src/workflow.ts".into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: Some(4),
        meta: json!({}),
        relevance: 1.0,
    };
    let symbol = GraphNode {
        key: "sym:src/workflow.ts#::run@1".into(),
        kind: "symbol".into(),
        display_name: "run".into(),
        file: Some("src/workflow.ts".into()),
        file_role: Some("production".into()),
        file_origin: Some("repository".into()),
        line: Some(7),
        meta: json!({}),
        relevance: 1.0,
    };

    let graph = graph_value(&[file, hub, symbol], &[], &[]);
    assert!(graph["nodes"]["n1"].get("at").is_none());
    assert_eq!(graph["nodes"]["n2"]["at"], "src/workflow.ts:4");
    assert!(graph["nodes"]["n3"].get("at").is_none());
    assert_eq!(graph["nodes"]["n3"]["line"], 7);
}

fn symbol_target() -> SymbolTarget {
    SymbolTarget {
        file: "src/adapter.ts".into(),
        file_origin: "workspace".into(),
        file_id: 41,
        name: "Adapter".into(),
        kind: "class".into(),
        line: 8,
        exported: true,
    }
}

fn response_identity() -> crate::publication::ResponseIdentity {
    crate::publication::ResponseIdentity {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
    }
}

#[test]
fn compact_search_keeps_localization_and_relation_evidence() -> anyhow::Result<()> {
    let root = "sym:src/workflow.ts#::start@1";
    let target = "sym:src/workflow.ts#::finish@20";
    let result = SearchResult {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: vec![Hit {
            chunk_id: 41,
            file: "src/workflow.ts".into(),
            file_role: "production".into(),
            repository_role: None,
            file_origin: "repository".into(),
            kind: "method".into(),
            name: Some("start".into()),
            start_line: 1,
            end_line: 8,
            score: 0.98,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
            match_lines: None,
            snippet: "start() { return this.queue.finish(); }".into(),
            snippet_truncated: false,
            anchors: vec![root.into()],
            file_anchor: Some("file:src/workflow.ts".into()),
            uses: Vec::new(),
            used_by: Vec::new(),
        }],
        semantic_artifacts: Vec::new(),
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 0,
        semantic_selected: 0,
        expansion: Some(SearchExpansion {
            projection: crate::search::ExpansionProjection::Paths,
            seeds: vec![root.into()],
            nodes: vec![node(root, 1), node(target, 20)],
            edges: vec![edge(root, target, 3)],
            candidate_paths: 1,
            selected_paths: 1,
            omitted_paths: 0,
            omitted_nodes: 0,
            omitted_edges: 0,
            selected_path_edges: vec![vec![(
                root.into(),
                target.into(),
                "member_call".into(),
                Some("src/workflow.ts".into()),
                Some(3),
            )]],
            node_limit: 40,
            edge_limit: 120,
            file_roles: vec!["production".into(), "unknown".into()],
            file_origins: origin::defaults(),
            payload_bytes: 0,
            truncated: false,
        }),
        response_budget: ResponseBudget {
            byte_limit: 24_000,
            ..Default::default()
        },
    };

    let compact = search_string(&result)?;
    let diagnostic = serde_json::to_string_pretty(&result)?;
    let diagnostic_value = serde_json::to_value(&result)?;
    assert!(
        diagnostic_value["response_budget"]
            .get("byte_limit")
            .is_none()
    );
    assert!(diagnostic_value["expansion"].get("byte_limit").is_none());
    assert!(compact.len() * 2 < diagnostic.len());
    let value: serde_json::Value = serde_json::from_str(&compact)?;
    assert!(value.get("retrieval").is_none());
    assert_eq!(value["hits"][0]["at"], "src/workflow.ts:1-8");
    assert_eq!(value["hits"][0]["symbol"], "start");
    assert_eq!(value["hits"][0]["anchor"], root);
    assert_eq!(value["snapshot"], "s".repeat(64));
    assert_eq!(value["publication_snapshot"], "p".repeat(64));
    assert!(value["hits"][0].get("followups").is_none());
    assert!(value["hits"][0].get("chunk_id").is_none());
    assert_eq!(value["graph"]["projection"], "paths");
    assert!(value["graph"]["nodes"]["n1"].get("at").is_none());
    assert!(value["graph"]["nodes"]["n2"].get("at").is_none());
    assert_eq!(value["graph"]["edges"][0][3], "likely");
    assert_eq!(value["graph"]["edges"][0][4], "typescript-checker");
    assert_eq!(
        value["graph"]["edges"][0][6]["receiver_types"][0],
        "QueueService"
    );
    let sections = super::search_section_bytes(&result)?;
    assert_eq!(sections.total_bytes, compact.len());
    assert_eq!(
        sections.hits_bytes
            + sections.graph_bytes
            + sections.memory_bytes
            + sections.envelope_bytes,
        sections.total_bytes
    );
    Ok(())
}

#[test]
fn compact_receiver_types_keep_bounded_heads_and_mark_truncation() {
    let receiver_types = vec![
        json!(format!("Errors<{}>", "Nested<".repeat(200))),
        json!("QueueService"),
        json!("Third"),
        json!("Fourth"),
        json!("Fifth"),
    ];
    let compact = compact_receiver_types(&receiver_types);
    assert_eq!(compact["receiver_types"][0], "Errors");
    assert_eq!(compact["receiver_types"][1], "QueueService");
    assert_eq!(compact["receiver_types_truncated"], true);
    assert!(serde_json::to_string(&compact).unwrap().len() < 160);
}

#[test]
fn compact_search_emits_anchors_without_followup_scaffolding() {
    let hit = Hit {
        chunk_id: 1,
        file: "src/overlap.ts".into(),
        file_role: "production".into(),
        repository_role: None,
        file_origin: "repository".into(),
        kind: "module".into(),
        name: None,
        start_line: 1,
        end_line: 20,
        score: 1.0,
        match_reason: MatchReason::Hybrid,
        matched_identifiers: Vec::new(),
        match_lines: None,
        snippet: "const first = 1; const second = 2;".into(),
        snippet_truncated: false,
        anchors: vec![
            "sym:src/overlap.ts#::first@1".into(),
            "sym:src/overlap.ts#::second@1".into(),
        ],
        file_anchor: Some("file:src/overlap.ts".into()),
        uses: Vec::new(),
        used_by: Vec::new(),
    };
    let value = compact_hit(&hit, MatchReason::Hybrid);
    assert_eq!(value["anchors"].as_array().map(Vec::len), Some(2));
    assert!(value.get("followups").is_none());
    assert!(value.get("followup_candidates").is_none());
}

#[test]
fn ordinary_eight_hit_search_fits_under_four_kibibytes() -> anyhow::Result<()> {
    let hits = (0..8)
        .map(|index| Hit {
            chunk_id: index,
            file: format!("src/services/service-{index}.ts"),
            file_role: "production".into(),
            repository_role: None,
            file_origin: "repository".into(),
            kind: "function".into(),
            name: Some(format!("handleStep{index}")),
            start_line: 10 + index,
            end_line: 14 + index,
            score: 1.0 / (index + 1) as f64,
            match_reason: MatchReason::Hybrid,
            matched_identifiers: Vec::new(),
            match_lines: None,
            snippet: format!("export function handleStep{index}() {{ return next(); }}"),
            snippet_truncated: false,
            anchors: vec![format!(
                "sym:src/services/service-{index}.ts#::handleStep{index}@{}",
                10 + index
            )],
            file_anchor: Some(format!("file:src/services/service-{index}.ts")),
            uses: vec!["next (call)".into()],
            used_by: Vec::new(),
        })
        .collect();
    let result = SearchResult {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits,
        semantic_artifacts: Vec::new(),
        semantic_retrieval: None,
        semantic_attachment: None,
        semantic_candidates: 0,
        semantic_selected: 0,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 24_000,
            rendered_bytes: 0,
            unbudgeted_bytes: 0,
            ..Default::default()
        },
    };
    assert!(search_string(&result)?.len() < 4_000);
    Ok(())
}

#[test]
fn search_memory_is_a_small_actionable_preview() -> anyhow::Result<()> {
    let result = SearchResult {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: Vec::new(),
        semantic_artifacts: vec![SemanticArtifact {
            id: 7,
            supersedes: None,
            artifact_type: "card".into(),
            name: Some("sym:src/cache.ts#::resolveRoute@1".into()),
            trust: "untrusted-semantic-memory".into(),
            body: serde_json::json!({
                "purpose": "Resolves a cached route while preserving rewrite state.",
                "invariants": ["The rewrite marker survives fallback."]
            }),
            model: "test".into(),
            prompt_version: "card/v1".into(),
            confidence: "likely".into(),
            source_snapshot: "s".repeat(64),
            created_at: "2026-08-16T00:00:00Z".into(),
            freshness: "fresh".into(),
            supports: vec![SemanticSupport {
                claim_path: "/purpose".into(),
                anchor: "sym:src/cache.ts#::resolveRoute@1".into(),
                relationship: "defining-evidence".into(),
                role: None,
                evidence_file: "src/cache.ts".into(),
                evidence_start_line: 10,
                evidence_end_line: 20,
                source_hash: "h".repeat(64),
                context_hash: "c".repeat(64),
                confidence: "likely".into(),
                freshness: "fresh".into(),
            }],
            retrieval_score: Some(ArtifactRetrievalScore {
                rank_score: 0.75,
                lexical_score: Some(0.5),
                vector_cosine: Some(0.8),
            }),
        }],
        semantic_retrieval: Some(crate::semantic::ArtifactRetrievalStatus {
            lexical: "active",
            vector: "disabled",
            corpus_artifacts: 8,
            vector_action: None,
            vector_timings: None,
        }),
        semantic_attachment: Some(crate::search::MemoryAttachmentStatus {
            status: "connected",
            connected_candidates: 1,
            graph_depth: 2,
            graph_nodes: 1,
            graph_truncated: false,
        }),
        semantic_candidates: 8,
        semantic_selected: 1,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 24_000,
            ..Default::default()
        },
    };

    let value: serde_json::Value = serde_json::from_str(&search_string(&result)?)?;
    let memory = &value["semantic_memory"];
    assert!(memory.get("candidate_pool").is_none());
    assert!(memory.get("selected").is_none());
    assert!(memory.get("returned").is_none());
    assert!(memory.get("retrieval").is_none());
    assert!(memory.get("attachment").is_none());
    assert_eq!(memory["next_tool"], "semantic_memory");
    assert_eq!(
        memory["artifacts"][0]["summary"],
        "Resolves a cached route while preserving rewrite state."
    );
    assert!(memory["artifacts"][0].get("body").is_none());
    assert!(memory["artifacts"][0].get("supports").is_none());
    assert!(memory["artifacts"][0].get("score").is_none());
    assert!(search_string(&result)?.len() < 1_000);
    Ok(())
}

#[test]
fn annotation_claim_is_visible_in_memory_preview() {
    assert_eq!(
        semantic_preview(&serde_json::json!({
            "claim": "The route cache preserves the previous tree during refresh."
        })),
        Some("The route cache preserves the previous tree during refresh.".into())
    );
}

#[test]
fn degraded_vector_status_is_visible_without_query_candidates() -> anyhow::Result<()> {
    let result = SearchResult {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: Vec::new(),
        semantic_artifacts: Vec::new(),
        semantic_retrieval: Some(crate::semantic::ArtifactRetrievalStatus {
            lexical: "active",
            vector: "degraded",
            corpus_artifacts: 20,
            vector_action: Some("start or repair the configured embedding service, then retry"),
            vector_timings: None,
        }),
        semantic_attachment: Some(crate::search::MemoryAttachmentStatus {
            status: "no_connected_memory",
            connected_candidates: 0,
            graph_depth: 2,
            graph_nodes: 0,
            graph_truncated: false,
        }),
        semantic_candidates: 0,
        semantic_selected: 0,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 24_000,
            ..Default::default()
        },
    };
    let value: serde_json::Value = serde_json::from_str(&search_string(&result)?)?;
    assert!(value["semantic_memory"].get("candidate_pool").is_none());
    assert!(value["semantic_memory"].get("returned").is_none());
    assert_eq!(value["semantic_memory"]["retrieval"]["vector"], "degraded");
    assert_eq!(
        value["semantic_memory"]["attachment"]["status"],
        "no_connected_memory"
    );
    Ok(())
}

#[test]
fn omitted_memory_keeps_the_follow_up_envelope() -> anyhow::Result<()> {
    let result = SearchResult {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        exhaustive: None,
        retrieval: RetrievalStatus::vector_disabled(),
        hits: Vec::new(),
        semantic_artifacts: Vec::new(),
        semantic_retrieval: Some(crate::semantic::ArtifactRetrievalStatus {
            lexical: "active",
            vector: "degraded",
            corpus_artifacts: 20,
            vector_action: Some("run jscout embed <root> --semantic-only"),
            vector_timings: None,
        }),
        semantic_attachment: Some(crate::search::MemoryAttachmentStatus {
            status: "connected",
            connected_candidates: 3,
            graph_depth: 2,
            graph_nodes: 10,
            graph_truncated: false,
        }),
        semantic_candidates: 12,
        semantic_selected: 3,
        expansion: None,
        response_budget: ResponseBudget {
            byte_limit: 512,
            omitted_semantic_artifacts: 3,
            truncated: true,
            ..Default::default()
        },
    };
    let value: serde_json::Value = serde_json::from_str(&search_string(&result)?)?;
    assert!(value["semantic_memory"].get("candidate_pool").is_none());
    assert!(value["semantic_memory"].get("selected").is_none());
    assert!(value["semantic_memory"].get("returned").is_none());
    assert_eq!(value["semantic_memory"]["omitted"]["artifacts"], 3);
    assert_eq!(value["semantic_memory"]["next_tool"], "semantic_memory");
    Ok(())
}

#[test]
fn ten_checker_edges_fit_under_eight_kibibytes() -> anyhow::Result<()> {
    let root = "sym:src/workflow.ts#::step0@1";
    let mut nodes = vec![node(root, 1)];
    let mut edges = Vec::new();
    for index in 1..=10 {
        let target = format!("sym:src/workflow.ts#::step{index}@{}", index + 1);
        nodes.push(node(&target, index + 1));
        edges.push(edge(root, &target, index + 1));
    }
    let neighborhood = Neighborhood {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        requested_anchor: root.into(),
        resolved_anchor: root.into(),
        anchor_status: "current".into(),
        nodes,
        edges,
        truncated: false,
    };
    let diagnostic = serde_json::to_string_pretty(&neighborhood)?;
    let compact = render_neighborhood(&neighborhood, 8_000)?;
    assert!(diagnostic.len() > 8_000);
    assert!(compact.len() < 8_000);
    let value: serde_json::Value = serde_json::from_str(&compact)?;
    assert_eq!(value["graph"]["edges"].as_array().map(Vec::len), Some(10));
    Ok(())
}

fn render_neighborhood_linearly(
    neighborhood: &Neighborhood,
    byte_limit: usize,
) -> anyhow::Result<String> {
    if byte_limit == 0 {
        anyhow::bail!("response byte limit must be greater than zero");
    }
    let mut nodes = neighborhood.nodes.clone();
    let mut edges = neighborhood.edges.clone();
    let original_nodes = nodes.len();
    let original_edges = edges.len();
    let mut rendered_bytes = 0;
    let mut unbudgeted_bytes = 0;
    for _ in 0..8 {
        let rendered = settle_neighborhood_bytes(
            neighborhood,
            &nodes,
            &edges,
            unbudgeted_bytes,
            original_nodes,
            original_edges,
            &mut rendered_bytes,
        )?;
        if rendered == unbudgeted_bytes {
            break;
        }
        unbudgeted_bytes = rendered;
    }

    loop {
        // Each candidate has its own byte-count fixed point. Reusing a wider
        // prior count can select the conservative side of a decimal-boundary
        // fixed point and incorrectly reject an exactly fitting prefix.
        rendered_bytes = 0;
        let rendered = settle_neighborhood_bytes(
            neighborhood,
            &nodes,
            &edges,
            unbudgeted_bytes,
            original_nodes,
            original_edges,
            &mut rendered_bytes,
        )?;
        if rendered <= byte_limit {
            return Ok(serde_json::to_string(&neighborhood_value(
                neighborhood,
                &nodes,
                &edges,
                unbudgeted_bytes,
                rendered_bytes,
                original_nodes,
                original_edges,
            ))?);
        }
        if edges.pop().is_some() {
            prune_unreferenced_nodes(&mut nodes, &edges, &neighborhood.resolved_anchor);
            continue;
        }
        if let Some(index) = nodes
            .iter()
            .rposition(|node| node.key != neighborhood.resolved_anchor)
        {
            nodes.remove(index);
            continue;
        }
        anyhow::bail!(
            "response byte limit {byte_limit} is below the minimum compact neighborhood envelope ({rendered} bytes)"
        );
    }
}

fn assert_budget_renderers_match(
    neighborhood: &Neighborhood,
    byte_limits: impl IntoIterator<Item = usize>,
) -> anyhow::Result<()> {
    for byte_limit in byte_limits {
        let expected = render_neighborhood_linearly(neighborhood, byte_limit)
            .map_err(|error| error.to_string());
        let actual =
            render_neighborhood(neighborhood, byte_limit).map_err(|error| error.to_string());
        assert_eq!(actual, expected, "response differs at {byte_limit} bytes");
        if let Ok(rendered) = actual {
            assert!(rendered.len() <= byte_limit);
            let value: serde_json::Value = serde_json::from_str(&rendered)?;
            assert_eq!(
                value["response"]["rendered_bytes"].as_u64(),
                Some(rendered.len() as u64)
            );
            assert_eq!(value["snapshot"], neighborhood.snapshot);
            assert_eq!(
                value["publication_snapshot"],
                neighborhood.publication_snapshot
            );
            assert!(value["response"].get("byte_limit").is_none());
        }
    }
    Ok(())
}

#[test]
fn neighborhood_prefix_search_matches_linear_budget_shedding() -> anyhow::Result<()> {
    let root = "sym:src/workflow.ts#::step0@1";
    let mut nodes = vec![node(root, 1)];
    let mut edges = Vec::new();
    for index in 1..=130 {
        let target = format!("sym:src/workflow.ts#::step{index}@{}", index + 1);
        nodes.push(node(&target, index + 1));
        edges.push(edge(root, &target, index + 1));
    }
    let neighborhood = Neighborhood {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        requested_anchor: root.into(),
        resolved_anchor: root.into(),
        anchor_status: "current".into(),
        nodes,
        edges,
        truncated: false,
    };
    let full = render_neighborhood_linearly(&neighborhood, usize::MAX)?.len();
    let mut limits = vec![
        1,
        255,
        256,
        511,
        512,
        999,
        1_000,
        1_999,
        2_000,
        9_999,
        10_000,
        23_999,
        24_000,
        full.saturating_sub(1),
        full,
        full + 1,
    ];
    limits.extend((300..full).step_by((full / 24).max(1)));
    limits.sort_unstable();
    limits.dedup();
    assert_budget_renderers_match(&neighborhood, limits)
}

#[test]
fn neighborhood_prefix_search_matches_linear_isolated_node_shedding() -> anyhow::Result<()> {
    let root = "sym:src/workflow.ts#::root@1";
    let mut nodes = vec![node(root, 1)];
    for index in 1..=25 {
        nodes.push(node(
            &format!("sym:src/workflow.ts#::isolated{index}@{}", index + 1),
            index + 1,
        ));
    }
    let neighborhood = Neighborhood {
        snapshot: "s".repeat(64),
        publication_snapshot: "p".repeat(64),
        requested_anchor: root.into(),
        resolved_anchor: root.into(),
        anchor_status: "current".into(),
        nodes,
        edges: Vec::new(),
        truncated: false,
    };
    assert_budget_renderers_match(
        &neighborhood,
        [1, 255, 256, 511, 512, 999, 1_000, 2_000, 4_000, 8_000],
    )
}

#[test]
fn compact_who_uses_groups_sites_without_losing_candidate_evidence() -> anyhow::Result<()> {
    let target = symbol_target();
    let results = vec![(
        &target,
        vec![
            Usage {
                file: "src/caller.ts".into(),
                file_origin: "workspace".into(),
                line: 12,
                kind: "call".into(),
                confidence: "certain".into(),
                detail: None,
                chunk_name: Some("run".into()),
            },
            Usage {
                file: "src/dynamic.ts".into(),
                file_origin: "repository".into(),
                line: 30,
                kind: "call".into(),
                confidence: "possible".into(),
                detail: Some("candidate.Adapter()".into()),
                chunk_name: None,
            },
        ],
    )];

    let identity = response_identity();
    let rendered = who_uses_string(&results, &identity, None, 4_000)?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;

    assert!(rendered.len() < 1_000);
    assert_eq!(value["targets"][0]["target"]["at"], "src/adapter.ts:8");
    assert!(value["targets"][0]["target"].get("file_id").is_none());
    assert_eq!(
        value["targets"][0]["usages"]["certain"]["src/caller.ts"][0],
        serde_json::json!([12, "call", "run"])
    );
    assert_eq!(
        value["targets"][0]["usages"]["possible"]["src/dynamic.ts"][0],
        serde_json::json!([30, "call", null, "candidate.Adapter()"])
    );
    assert_eq!(value["response"]["matched_usages"], 2);
    assert_eq!(value["response"]["returned_usages"], 2);
    assert_eq!(value["snapshot"], identity.snapshot);
    assert_eq!(value["publication_snapshot"], identity.publication_snapshot);
    Ok(())
}

#[test]
fn compact_who_uses_truncates_to_the_complete_response_budget() -> anyhow::Result<()> {
    let target = symbol_target();
    let usages = (0..100)
        .map(|line| Usage {
            file: format!("src/caller-{line:03}.ts"),
            file_origin: "workspace".into(),
            line,
            kind: "call".into(),
            confidence: if line < 50 { "certain" } else { "possible" }.into(),
            detail: Some(format!("receiver-{line}.Adapter()")),
            chunk_name: Some(format!("caller{line}")),
        })
        .collect();
    let results = vec![(&target, usages)];

    let identity = response_identity();
    let unbudgeted = who_uses_string(&results, &identity, None, usize::MAX)?.len();
    let rendered = who_uses_string(&results, &identity, None, 1_200)?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;

    assert!(rendered.len() <= 1_200);
    assert_eq!(value["response"]["rendered_bytes"], rendered.len());
    assert_eq!(value["response"]["unbudgeted_bytes"], unbudgeted);
    assert_eq!(value["response"]["truncated"], true);
    assert!(value["response"]["returned_usages"].as_u64().unwrap() < 100);
    assert!(value["targets"][0]["usages"].get("certain").is_some());
    assert!(value["targets"][0]["usages"].get("possible").is_none());
    Ok(())
}

#[test]
fn compact_definition_serializes_source_once_and_obeys_the_whole_budget() -> anyhow::Result<()> {
    let target = symbol_target();
    let source = RenderedSource {
        representation: "full",
        text: format!("UNIQUE_SOURCE_MARKER\n{}", "const value = 1;\n".repeat(200)),
        original_bytes: 3_500,
        rendered_bytes: 3_500,
        elisions: Vec::new(),
        budget_truncated: false,
    };

    let identity = response_identity();
    let results = [(target, Some(source))];
    let unbudgeted = definition_string(&results, 1, &identity, None, usize::MAX)?.len();
    let rendered = definition_string(&results, 1, &identity, None, 1_000)?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;

    assert!(rendered.len() <= 1_000);
    assert_eq!(rendered.matches("UNIQUE_SOURCE_MARKER").count(), 1);
    assert!(value["definitions"][0]["source_meta"].get("text").is_none());
    assert!(value["definitions"][0]["target"].get("file_id").is_none());
    assert_eq!(
        value["definitions"][0]["source_meta"]["budget_truncated"],
        true
    );
    assert_eq!(value["response"]["rendered_bytes"], rendered.len());
    assert_eq!(value["response"]["unbudgeted_bytes"], unbudgeted);
    Ok(())
}
