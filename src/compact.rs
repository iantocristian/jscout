//! Compact, self-describing JSON for agent-facing retrieval surfaces.
//!
//! Canonical graph/search structs retain full diagnostic provenance. This
//! module projects them into a transport shape that keeps anchors, source
//! locations, relation identity, confidence, and provenance while removing
//! byte offsets, repeated defaults, occurrence IDs, and empty fields.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::{search, structural};

pub(crate) fn search_string(result: &search::SearchResult) -> Result<String> {
    Ok(serde_json::to_string(&search_value(result))?)
}

pub(crate) fn search_rendered_bytes(result: &search::SearchResult) -> Result<usize> {
    Ok(serde_json::to_string(&search_value(result))?.len())
}

pub(crate) fn search_value(result: &search::SearchResult) -> Value {
    let hits = result.hits.iter().map(compact_hit).collect::<Vec<_>>();
    let mut response = Map::new();
    response.insert("snapshot".into(), json!(result.snapshot));
    response.insert("retrieval".into(), json!(result.retrieval));
    response.insert("hits".into(), Value::Array(hits));

    if !result.semantic_artifacts.is_empty() {
        let artifacts = result
            .semantic_artifacts
            .iter()
            .map(|artifact| {
                let mut value = Map::new();
                value.insert("id".into(), json!(artifact.id));
                value.insert("type".into(), json!(artifact.artifact_type));
                if let Some(name) = &artifact.name {
                    value.insert("name".into(), json!(name));
                }
                value.insert("body".into(), artifact.body.clone());
                value.insert("confidence".into(), json!(artifact.confidence));
                value.insert("freshness".into(), json!(artifact.freshness));
                if !artifact.supports.is_empty() {
                    value.insert(
                        "supports".into(),
                        Value::Array(
                            artifact
                                .supports
                                .iter()
                                .map(|support| {
                                    json!([
                                        support.claim_path,
                                        support.relationship,
                                        support.anchor,
                                        source_at(
                                            Some(&support.evidence_file),
                                            Some(support.evidence_start_line),
                                            Some(support.evidence_end_line),
                                        ),
                                        support.role,
                                        support.confidence,
                                        support.freshness,
                                    ])
                                })
                                .collect(),
                        ),
                    );
                }
                Value::Object(value)
            })
            .collect::<Vec<_>>();
        response.insert(
            "semantic_memory".into(),
            json!({
                "trust": "untrusted",
                "support_fields": [
                    "claim", "relationship", "anchor", "at", "role",
                    "confidence", "freshness"
                ],
                "artifacts": artifacts,
            }),
        );
    }

    if let Some(expansion) = &result.expansion {
        let mut graph = graph_value(&expansion.nodes, &expansion.edges, &expansion.seeds);
        if expansion.truncated {
            graph["truncated"] = json!(true);
        }
        response.insert("graph".into(), graph);
    }
    response.insert(
        "response".into(),
        search_budget_value(&result.response_budget),
    );
    Value::Object(response)
}

fn compact_hit(hit: &search::Hit) -> Value {
    let mut value = Map::new();
    value.insert(
        "at".into(),
        json!(source_at(
            Some(&hit.file),
            Some(hit.start_line),
            Some(hit.end_line)
        )),
    );
    if let Some(name) = &hit.name {
        value.insert("symbol".into(), json!(name));
    }
    value.insert("kind".into(), json!(hit.kind));
    value.insert("snippet".into(), json!(hit.snippet));
    if hit.snippet_truncated {
        value.insert("snippet_truncated".into(), json!(true));
    }
    match hit.anchors.as_slice() {
        [anchor] => {
            value.insert("anchor".into(), json!(anchor));
        }
        anchors if !anchors.is_empty() => {
            value.insert("anchors".into(), json!(anchors));
        }
        _ => {}
    }
    if !hit.uses.is_empty() {
        value.insert("uses".into(), json!(hit.uses));
    }
    if !hit.used_by.is_empty() {
        value.insert("used_by".into(), json!(hit.used_by));
    }
    if hit.file_role != "production" {
        value.insert("role".into(), json!(hit.file_role));
    }
    if hit.file_origin == "dependency" {
        value.insert("origin".into(), json!(hit.file_origin));
    }
    Value::Object(value)
}

fn search_budget_value(budget: &search::ResponseBudget) -> Value {
    let mut value = Map::new();
    value.insert("byte_limit".into(), json!(budget.byte_limit));
    value.insert("rendered_bytes".into(), json!(budget.rendered_bytes));
    if budget.truncated {
        value.insert("truncated".into(), json!(true));
        value.insert("unbudgeted_bytes".into(), json!(budget.unbudgeted_bytes));
        let mut omitted = Map::new();
        for (name, count) in [
            ("hits", budget.omitted_hits),
            ("memory", budget.omitted_semantic_artifacts),
            ("supports", budget.omitted_semantic_supports),
            ("nodes", budget.omitted_nodes),
            ("edges", budget.omitted_edges),
            ("snippets", budget.truncated_snippets),
        ] {
            if count > 0 {
                omitted.insert(name.into(), json!(count));
            }
        }
        if !omitted.is_empty() {
            value.insert("omitted".into(), Value::Object(omitted));
        }
    }
    Value::Object(value)
}

pub(crate) fn expansion_payload_bytes(
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    seeds: &[String],
) -> Result<usize> {
    let graph = graph_value(nodes, edges, seeds);
    let node_values = graph["nodes"]
        .as_object()
        .into_iter()
        .flat_map(|nodes| nodes.iter())
        .map(|(id, value)| serde_json::to_vec(&(id, value)).map(|bytes| bytes.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let edge_values = graph["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|edge| serde_json::to_vec(edge).map(|bytes| bytes.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(node_values.iter().sum::<usize>()
        + edge_values.iter().sum::<usize>()
        + node_values.len().saturating_sub(1)
        + edge_values.len().saturating_sub(1))
}

fn graph_value(
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    seeds: &[String],
) -> Value {
    let ids = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.key.clone(), format!("n{}", index + 1)))
        .collect::<HashMap<_, _>>();
    let mut node_values = Map::new();
    for node in nodes {
        let id = ids.get(&node.key).expect("node id");
        let mut value = Map::new();
        value.insert("anchor".into(), json!(node.key));
        value.insert("name".into(), json!(node.display_name));
        if node.kind != "symbol" {
            value.insert("kind".into(), json!(node.kind));
        }
        if let Some(at) = source_at_option(node.file.as_deref(), node.line, node.line) {
            value.insert("at".into(), json!(at));
        }
        if node
            .file_role
            .as_deref()
            .is_some_and(|role| role != "production")
        {
            value.insert("role".into(), json!(node.file_role.as_deref()));
        }
        if node.file_origin.as_deref() == Some("dependency") {
            value.insert("origin".into(), json!("dependency"));
        }
        node_values.insert(id.clone(), Value::Object(value));
    }

    let edge_values = edges
        .iter()
        .filter_map(|edge| {
            let source = ids.get(&edge.source)?;
            let target = ids.get(&edge.target)?;
            let mut tuple = vec![
                json!(source),
                json!(edge.kind),
                json!(target),
                json!(edge.confidence),
                json!(edge.provenance),
                source_at_option(edge.file.as_deref(), edge.line, edge.line)
                    .map_or(Value::Null, Value::String),
            ];
            if let Some(receiver_types) = edge
                .detail
                .get("receiverTypes")
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty())
            {
                tuple.push(json!({ "receiver_types": receiver_types }));
            }
            Some(Value::Array(tuple))
        })
        .collect::<Vec<_>>();
    let local_seeds = seeds
        .iter()
        .map(|seed| ids.get(seed).cloned().unwrap_or_else(|| seed.clone()))
        .collect::<Vec<_>>();
    json!({
        "seeds": local_seeds,
        "nodes": node_values,
        "edge_fields": ["source", "kind", "target", "confidence", "provenance", "at", "detail?"],
        "edges": edge_values,
    })
}

pub(crate) fn render_neighborhood(
    neighborhood: &structural::Neighborhood,
    byte_limit: usize,
) -> Result<String> {
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
            byte_limit,
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
        let rendered = settle_neighborhood_bytes(
            neighborhood,
            &nodes,
            &edges,
            byte_limit,
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
                byte_limit,
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

#[allow(clippy::too_many_arguments)]
fn settle_neighborhood_bytes(
    neighborhood: &structural::Neighborhood,
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    byte_limit: usize,
    unbudgeted_bytes: usize,
    original_nodes: usize,
    original_edges: usize,
    rendered_bytes: &mut usize,
) -> Result<usize> {
    for _ in 0..8 {
        let rendered = serde_json::to_string(&neighborhood_value(
            neighborhood,
            nodes,
            edges,
            byte_limit,
            unbudgeted_bytes,
            *rendered_bytes,
            original_nodes,
            original_edges,
        ))?
        .len();
        if *rendered_bytes == rendered {
            return Ok(rendered);
        }
        *rendered_bytes = rendered;
    }
    Ok(serde_json::to_string(&neighborhood_value(
        neighborhood,
        nodes,
        edges,
        byte_limit,
        unbudgeted_bytes,
        *rendered_bytes,
        original_nodes,
        original_edges,
    ))?
    .len())
}

#[allow(clippy::too_many_arguments)]
fn neighborhood_value(
    neighborhood: &structural::Neighborhood,
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    byte_limit: usize,
    unbudgeted_bytes: usize,
    rendered_bytes: usize,
    original_nodes: usize,
    original_edges: usize,
) -> Value {
    let mut response = Map::new();
    response.insert("snapshot".into(), json!(neighborhood.snapshot));
    response.insert("anchor".into(), json!(neighborhood.resolved_anchor));
    if neighborhood.requested_anchor != neighborhood.resolved_anchor {
        response.insert(
            "requested_anchor".into(),
            json!(neighborhood.requested_anchor),
        );
        response.insert("anchor_status".into(), json!(neighborhood.anchor_status));
    }
    response.insert(
        "graph".into(),
        graph_value(
            nodes,
            edges,
            std::slice::from_ref(&neighborhood.resolved_anchor),
        ),
    );
    let omitted_nodes = original_nodes.saturating_sub(nodes.len());
    let omitted_edges = original_edges.saturating_sub(edges.len());
    let truncated = neighborhood.truncated || omitted_nodes > 0 || omitted_edges > 0;
    let mut budget = Map::new();
    budget.insert("byte_limit".into(), json!(byte_limit));
    budget.insert("rendered_bytes".into(), json!(rendered_bytes));
    if truncated {
        budget.insert("truncated".into(), json!(true));
        budget.insert("unbudgeted_bytes".into(), json!(unbudgeted_bytes));
        let mut omitted = Map::new();
        if omitted_nodes > 0 {
            omitted.insert("nodes".into(), json!(omitted_nodes));
        }
        if omitted_edges > 0 {
            omitted.insert("edges".into(), json!(omitted_edges));
        }
        if !omitted.is_empty() {
            budget.insert("omitted".into(), Value::Object(omitted));
        }
    }
    response.insert("response".into(), Value::Object(budget));
    Value::Object(response)
}

fn prune_unreferenced_nodes(
    nodes: &mut Vec<structural::GraphNode>,
    edges: &[structural::GraphEdge],
    root: &str,
) {
    let referenced = edges
        .iter()
        .flat_map(|edge| [edge.source.as_str(), edge.target.as_str()])
        .collect::<HashSet<_>>();
    nodes.retain(|node| node.key == root || referenced.contains(node.key.as_str()));
}

fn source_at(file: Option<&str>, start: Option<i64>, end: Option<i64>) -> String {
    source_at_option(file, start, end).unwrap_or_default()
}

fn source_at_option(file: Option<&str>, start: Option<i64>, end: Option<i64>) -> Option<String> {
    let file = file?;
    match (start, end) {
        (Some(start), Some(end)) if end != start => Some(format!("{file}:{start}-{end}")),
        (Some(line), _) => Some(format!("{file}:{line}")),
        _ => Some(file.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{render_neighborhood, search_string};
    use crate::{
        origin,
        search::{Hit, ResponseBudget, RetrievalStatus, SearchExpansion, SearchResult},
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
    fn compact_search_keeps_localization_and_relation_evidence() -> anyhow::Result<()> {
        let root = "sym:src/workflow.ts#::start@1";
        let target = "sym:src/workflow.ts#::finish@20";
        let result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits: vec![Hit {
                chunk_id: 41,
                file: "src/workflow.ts".into(),
                file_role: "production".into(),
                file_origin: "repository".into(),
                kind: "method".into(),
                name: Some("start".into()),
                start_line: 1,
                end_line: 8,
                score: 0.98,
                snippet: "start() { return this.queue.finish(); }".into(),
                snippet_truncated: false,
                anchors: vec![root.into()],
                file_anchor: "file:src/workflow.ts".into(),
                uses: Vec::new(),
                used_by: Vec::new(),
            }],
            semantic_artifacts: Vec::new(),
            expansion: Some(SearchExpansion {
                seeds: vec![root.into()],
                nodes: vec![node(root, 1), node(target, 20)],
                edges: vec![edge(root, target, 3)],
                node_limit: 40,
                edge_limit: 120,
                byte_limit: 24_000,
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
        assert!(compact.len() * 2 < diagnostic.len());
        let value: serde_json::Value = serde_json::from_str(&compact)?;
        assert_eq!(value["retrieval"]["lexical"], "active");
        assert_eq!(value["retrieval"]["vector"], "disabled");
        assert_eq!(value["retrieval"]["reranker"], "disabled");
        assert_eq!(value["hits"][0]["at"], "src/workflow.ts:1-8");
        assert_eq!(value["hits"][0]["symbol"], "start");
        assert_eq!(value["hits"][0]["anchor"], root);
        assert!(value["hits"][0].get("chunk_id").is_none());
        assert_eq!(value["graph"]["edges"][0][3], "likely");
        assert_eq!(value["graph"]["edges"][0][4], "typescript-checker");
        assert_eq!(
            value["graph"]["edges"][0][6]["receiver_types"][0],
            "QueueService"
        );
        Ok(())
    }

    #[test]
    fn ordinary_eight_hit_search_fits_under_four_kibibytes() -> anyhow::Result<()> {
        let hits = (0..8)
            .map(|index| Hit {
                chunk_id: index,
                file: format!("src/services/service-{index}.ts"),
                file_role: "production".into(),
                file_origin: "repository".into(),
                kind: "function".into(),
                name: Some(format!("handleStep{index}")),
                start_line: 10 + index,
                end_line: 14 + index,
                score: 1.0 / (index + 1) as f64,
                snippet: format!("export function handleStep{index}() {{ return next(); }}"),
                snippet_truncated: false,
                anchors: vec![format!(
                    "sym:src/services/service-{index}.ts#::handleStep{index}@{}",
                    10 + index
                )],
                file_anchor: format!("file:src/services/service-{index}.ts"),
                uses: vec!["next (call)".into()],
                used_by: Vec::new(),
            })
            .collect();
        let result = SearchResult {
            snapshot: "s".repeat(64),
            retrieval: RetrievalStatus::vector_disabled(),
            hits,
            semantic_artifacts: Vec::new(),
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
}
