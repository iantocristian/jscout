//! Compact, self-describing JSON for agent-facing retrieval surfaces.
//!
//! Canonical graph/search structs retain full diagnostic provenance. This
//! module projects them into a transport shape that keeps anchors, source
//! locations, relation identity, confidence, and provenance while removing
//! byte offsets, repeated defaults, occurrence IDs, and empty fields.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::{query, scout, search, structural};

pub(crate) fn search_string(result: &search::SearchResult) -> Result<String> {
    Ok(serde_json::to_string(&search_value(result))?)
}

pub(crate) fn search_rendered_bytes(result: &search::SearchResult) -> Result<usize> {
    Ok(serde_json::to_string(&search_value(result))?.len())
}

pub(crate) fn search_value(result: &search::SearchResult) -> Value {
    let hits = result
        .hits
        .iter()
        .map(|hit| compact_hit(hit, &result.snapshot))
        .collect::<Vec<_>>();
    let mut response = Map::new();
    response.insert("snapshot".into(), json!(result.snapshot));
    response.insert("retrieval".into(), json!(result.retrieval));
    response.insert("default_match".into(), json!("hybrid"));
    response.insert("hits".into(), Value::Array(hits));

    if result
        .semantic_retrieval
        .as_ref()
        .is_some_and(|retrieval| retrieval.corpus_artifacts > 0)
    {
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
                if let Some(summary) = semantic_preview(&artifact.body) {
                    value.insert("summary".into(), json!(summary));
                }
                value.insert("confidence".into(), json!(artifact.confidence));
                value.insert("freshness".into(), json!(artifact.freshness));
                if let Some(score) = &artifact.retrieval_score {
                    value.insert("score".into(), json!(score));
                }
                if let Some(support) = artifact.supports.first() {
                    value.insert(
                        "evidence".into(),
                        json!({
                            "anchor": support.anchor,
                            "at": source_at(
                                Some(&support.evidence_file),
                                Some(support.evidence_start_line),
                                Some(support.evidence_end_line),
                            ),
                        }),
                    );
                }
                Value::Object(value)
            })
            .collect::<Vec<_>>();
        response.insert(
            "semantic_memory".into(),
            json!({
                "trust": "untrusted",
                "retrieval": result.semantic_retrieval,
                "attachment": result.semantic_attachment.as_ref().map(memory_attachment_value),
                "candidate_pool": result.semantic_candidates,
                "selected": result.semantic_selected,
                "returned": artifacts.len(),
                "budget_omitted": result.semantic_selected.saturating_sub(artifacts.len()),
                "next_tool": "semantic_memory",
                "detail": "preview; pool/scores are uncalibrated; use semantic_memory for bodies/evidence",
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

fn memory_attachment_value(attachment: &search::MemoryAttachmentStatus) -> Value {
    let mut value = Map::new();
    value.insert("status".into(), json!(attachment.status));
    value.insert("connected".into(), json!(attachment.connected_candidates));
    let mut graph = Map::new();
    graph.insert("depth".into(), json!(attachment.graph_depth));
    graph.insert("nodes".into(), json!(attachment.graph_nodes));
    if attachment.graph_truncated {
        graph.insert("truncated".into(), json!(true));
    }
    value.insert("graph".into(), Value::Object(graph));
    Value::Object(value)
}

fn semantic_preview(body: &Value) -> Option<String> {
    let object = body.as_object()?;
    let value = ["claim", "purpose", "overview", "description", "definition"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))?;
    let mut preview = value.trim().to_string();
    const MAX_PREVIEW_BYTES: usize = 360;
    if preview.len() > MAX_PREVIEW_BYTES {
        let mut cut = MAX_PREVIEW_BYTES;
        while !preview.is_char_boundary(cut) {
            cut -= 1;
        }
        preview.truncate(cut);
        preview.push('…');
    }
    (!preview.is_empty()).then_some(preview)
}

fn compact_hit(hit: &search::Hit, snapshot: &str) -> Value {
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
    if hit.match_reason != search::MatchReason::Hybrid {
        value.insert("match".into(), json!(hit.match_reason));
    }
    if !hit.matched_identifiers.is_empty() {
        value.insert("matched_identifiers".into(), json!(hit.matched_identifiers));
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
        _ => {
            value.insert("anchor".into(), json!(hit.file_anchor));
        }
    }
    if hit.include_followups && hit.anchors.len() <= 1 {
        let anchor = hit.anchors.first().unwrap_or(&hit.file_anchor);
        value.insert("followups".into(), compact_followups(hit, anchor, snapshot));
    }
    if !hit.uses.is_empty() {
        value.insert("uses".into(), json!(hit.uses));
    }
    if !hit.used_by.is_empty() {
        value.insert("used_by".into(), json!(hit.used_by));
    }
    let effective_role = hit.repository_role.as_deref().unwrap_or(&hit.file_role);
    if effective_role != "production" {
        value.insert("role".into(), json!(effective_role));
    }
    if hit.file_origin == "dependency" {
        value.insert("origin".into(), json!(hit.file_origin));
    }
    Value::Object(value)
}

fn compact_followups(hit: &search::Hit, anchor: &str, snapshot: &str) -> Value {
    let origins = [&hit.file_origin];
    if anchor.starts_with("sym:") {
        let tools = if hit.include_neighborhood_followup {
            vec!["definition", "who_uses", "neighborhood"]
        } else {
            vec!["definition", "who_uses"]
        };
        json!({
            "tools": tools,
            "arguments": {
                "anchor": anchor,
                "snapshot": snapshot,
                "origins": origins,
            }
        })
    } else {
        let mut calls = vec![json!({
            "tool": "file_outline",
            "arguments": {
                "path": hit.file,
                "origins": origins,
            }
        })];
        if hit.include_neighborhood_followup {
            calls.push(json!({
                "tool": "neighborhood",
                "arguments": {
                    "anchor": anchor,
                    "snapshot": snapshot,
                    "origins": origins,
                }
            }));
        }
        json!({ "calls": calls })
    }
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
            ("followups", budget.omitted_followups),
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

pub(crate) fn who_uses_string(
    results: &[(&query::SymbolTarget, Vec<query::Usage>)],
    byte_limit: usize,
) -> Result<String> {
    if byte_limit < 256 {
        anyhow::bail!("response byte limit must be at least 256 bytes");
    }
    let mut sites = results
        .iter()
        .enumerate()
        .flat_map(|(target_index, (_, usages))| {
            usages.iter().map(move |usage| (target_index, usage))
        })
        .collect::<Vec<_>>();
    sites.sort_by(|(left_target, left), (right_target, right)| {
        confidence_rank(&left.confidence)
            .cmp(&confidence_rank(&right.confidence))
            .then_with(|| left_target.cmp(right_target))
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.chunk_name.cmp(&right.chunk_name))
            .then_with(|| left.detail.cmp(&right.detail))
    });

    let matched_targets = results.len();
    let matched_usages = sites.len();
    let (full, full_bytes) = settle_who_uses(
        results,
        matched_targets,
        &sites,
        byte_limit,
        matched_targets,
        matched_usages,
        None,
    )?;
    if full_bytes <= byte_limit {
        return Ok(serde_json::to_string(&full)?);
    }

    let mut returned_targets = matched_targets;
    loop {
        let (_, base_bytes) = settle_who_uses(
            results,
            returned_targets,
            &[],
            byte_limit,
            matched_targets,
            matched_usages,
            Some(full_bytes),
        )?;
        if base_bytes <= byte_limit {
            break;
        }
        if returned_targets == 0 {
            anyhow::bail!(
                "response byte limit {byte_limit} is below the minimum compact who_uses envelope ({base_bytes} bytes)"
            );
        }
        returned_targets -= 1;
    }

    let eligible = sites
        .into_iter()
        .filter(|(target_index, _)| *target_index < returned_targets)
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = eligible.len() + 1;
    while low < high {
        let middle = low + (high - low) / 2;
        let (_, rendered) = settle_who_uses(
            results,
            returned_targets,
            &eligible[..middle],
            byte_limit,
            matched_targets,
            matched_usages,
            Some(full_bytes),
        )?;
        if rendered <= byte_limit {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let retained = low.saturating_sub(1);
    let (value, rendered) = settle_who_uses(
        results,
        returned_targets,
        &eligible[..retained],
        byte_limit,
        matched_targets,
        matched_usages,
        Some(full_bytes),
    )?;
    if rendered > byte_limit {
        anyhow::bail!("failed to fit compact who_uses response into {byte_limit} bytes");
    }
    Ok(serde_json::to_string(&value)?)
}

#[allow(clippy::too_many_arguments)]
fn settle_who_uses(
    results: &[(&query::SymbolTarget, Vec<query::Usage>)],
    returned_targets: usize,
    sites: &[(usize, &query::Usage)],
    byte_limit: usize,
    matched_targets: usize,
    matched_usages: usize,
    unbudgeted_bytes: Option<usize>,
) -> Result<(Value, usize)> {
    let mut rendered_bytes = 0usize;
    for _ in 0..8 {
        let value = who_uses_value(
            results,
            returned_targets,
            sites,
            byte_limit,
            rendered_bytes,
            matched_targets,
            matched_usages,
            unbudgeted_bytes,
        );
        let rendered = serde_json::to_string(&value)?.len();
        if rendered == rendered_bytes {
            return Ok((value, rendered));
        }
        rendered_bytes = rendered;
    }
    let value = who_uses_value(
        results,
        returned_targets,
        sites,
        byte_limit,
        rendered_bytes,
        matched_targets,
        matched_usages,
        unbudgeted_bytes,
    );
    let rendered = serde_json::to_string(&value)?.len();
    Ok((value, rendered))
}

#[allow(clippy::too_many_arguments)]
fn who_uses_value(
    results: &[(&query::SymbolTarget, Vec<query::Usage>)],
    returned_targets: usize,
    sites: &[(usize, &query::Usage)],
    byte_limit: usize,
    rendered_bytes: usize,
    matched_targets: usize,
    matched_usages: usize,
    unbudgeted_bytes: Option<usize>,
) -> Value {
    let mut groups = (0..returned_targets)
        .map(|_| BTreeMap::<&'static str, BTreeMap<String, Vec<Value>>>::new())
        .collect::<Vec<_>>();
    let mut dependency_files = (0..returned_targets)
        .map(|_| BTreeSet::<String>::new())
        .collect::<Vec<_>>();
    for (target_index, usage) in sites {
        let confidence = normalized_confidence(&usage.confidence);
        let mut site = vec![json!(usage.line), json!(usage.kind)];
        match (usage.chunk_name.as_deref(), usage.detail.as_deref()) {
            (Some(chunk), Some(detail)) => {
                site.push(json!(chunk));
                site.push(json!(detail));
            }
            (Some(chunk), None) => site.push(json!(chunk)),
            (None, Some(detail)) => {
                site.push(Value::Null);
                site.push(json!(detail));
            }
            (None, None) => {}
        }
        groups[*target_index]
            .entry(confidence)
            .or_default()
            .entry(usage.file.clone())
            .or_default()
            .push(Value::Array(site));
        if usage.file_origin == "dependency" {
            dependency_files[*target_index].insert(usage.file.clone());
        }
    }

    let targets = results
        .iter()
        .take(returned_targets)
        .enumerate()
        .map(|(index, (target, _))| {
            let mut value = Map::new();
            value.insert("target".into(), compact_target(target));
            if !groups[index].is_empty() {
                value.insert("usages".into(), json!(groups[index]));
            }
            if !dependency_files[index].is_empty() {
                value.insert("dependency_files".into(), json!(dependency_files[index]));
            }
            Value::Object(value)
        })
        .collect::<Vec<_>>();

    let returned_usages = sites.len();
    let omitted_targets = matched_targets.saturating_sub(returned_targets);
    let omitted_usages = matched_usages.saturating_sub(returned_usages);
    let truncated = omitted_targets > 0 || omitted_usages > 0;
    let mut response = Map::new();
    response.insert("byte_limit".into(), json!(byte_limit));
    response.insert("rendered_bytes".into(), json!(rendered_bytes));
    response.insert("matched_targets".into(), json!(matched_targets));
    response.insert("returned_targets".into(), json!(returned_targets));
    response.insert("matched_usages".into(), json!(matched_usages));
    response.insert("returned_usages".into(), json!(returned_usages));
    if truncated {
        response.insert("truncated".into(), json!(true));
        if let Some(unbudgeted_bytes) = unbudgeted_bytes {
            response.insert("unbudgeted_bytes".into(), json!(unbudgeted_bytes));
        }
        let mut omitted = Map::new();
        if omitted_targets > 0 {
            omitted.insert("targets".into(), json!(omitted_targets));
        }
        if omitted_usages > 0 {
            omitted.insert("usages".into(), json!(omitted_usages));
        }
        response.insert("omitted".into(), Value::Object(omitted));
    }
    json!({
        "usage_fields": ["line", "kind", "enclosing?", "detail?"],
        "targets": targets,
        "response": response,
    })
}

pub(crate) fn definition_string(
    results: &[(query::SymbolTarget, Option<scout::RenderedSource>)],
    matched_targets: usize,
    byte_limit: usize,
) -> Result<String> {
    if byte_limit < 256 {
        anyhow::bail!("response byte limit must be at least 256 bytes");
    }
    let mut definitions = results
        .iter()
        .map(|(target, source)| compact_definition(target, source.as_ref()))
        .collect::<Vec<_>>();
    let mut omitted_source_bytes = 0usize;

    loop {
        let (value, rendered) = settle_definitions(
            &definitions,
            matched_targets,
            byte_limit,
            omitted_source_bytes,
        )?;
        if rendered <= byte_limit {
            return Ok(serde_json::to_string(&value)?);
        }
        if definitions.len() > 1 {
            definitions.pop();
            continue;
        }
        let Some(definition) = definitions.first_mut() else {
            anyhow::bail!(
                "response byte limit {byte_limit} is below the minimum compact definition envelope ({rendered} bytes)"
            );
        };
        let Some(source) = definition.get("source").and_then(Value::as_str) else {
            anyhow::bail!(
                "response byte limit {byte_limit} is below the minimum compact definition envelope ({rendered} bytes)"
            );
        };
        if source.is_empty() {
            anyhow::bail!(
                "response byte limit {byte_limit} is below the minimum compact definition envelope ({rendered} bytes)"
            );
        }
        let excess = rendered.saturating_sub(byte_limit).max(1);
        let keep = source.len().saturating_sub(excess);
        let shortened = truncate_utf8(source, keep).to_string();
        omitted_source_bytes += source.len().saturating_sub(shortened.len());
        definition["source"] = json!(shortened);
        definition["source_meta"]["rendered_bytes"] =
            json!(definition["source"].as_str().map_or(0, str::len));
        definition["source_meta"]["budget_truncated"] = json!(true);
    }
}

fn settle_definitions(
    definitions: &[Value],
    matched_targets: usize,
    byte_limit: usize,
    omitted_source_bytes: usize,
) -> Result<(Value, usize)> {
    let mut rendered_bytes = 0usize;
    for _ in 0..8 {
        let value = definitions_value(
            definitions,
            matched_targets,
            byte_limit,
            rendered_bytes,
            omitted_source_bytes,
        );
        let rendered = serde_json::to_string(&value)?.len();
        if rendered == rendered_bytes {
            return Ok((value, rendered));
        }
        rendered_bytes = rendered;
    }
    let value = definitions_value(
        definitions,
        matched_targets,
        byte_limit,
        rendered_bytes,
        omitted_source_bytes,
    );
    let rendered = serde_json::to_string(&value)?.len();
    Ok((value, rendered))
}

fn definitions_value(
    definitions: &[Value],
    matched_targets: usize,
    byte_limit: usize,
    rendered_bytes: usize,
    omitted_source_bytes: usize,
) -> Value {
    let returned_targets = definitions.len();
    let omitted_targets = matched_targets.saturating_sub(returned_targets);
    let truncated = omitted_targets > 0 || omitted_source_bytes > 0;
    let mut response = Map::new();
    response.insert("byte_limit".into(), json!(byte_limit));
    response.insert("rendered_bytes".into(), json!(rendered_bytes));
    response.insert("matched_targets".into(), json!(matched_targets));
    response.insert("returned_targets".into(), json!(returned_targets));
    if truncated {
        response.insert("truncated".into(), json!(true));
        let mut omitted = Map::new();
        if omitted_targets > 0 {
            omitted.insert("targets".into(), json!(omitted_targets));
        }
        if omitted_source_bytes > 0 {
            omitted.insert("source_bytes".into(), json!(omitted_source_bytes));
        }
        response.insert("omitted".into(), Value::Object(omitted));
    }
    json!({ "definitions": definitions, "response": response })
}

fn compact_definition(
    target: &query::SymbolTarget,
    source: Option<&scout::RenderedSource>,
) -> Value {
    let mut value = Map::new();
    value.insert("target".into(), compact_target(target));
    if let Some(source) = source {
        value.insert("source".into(), json!(source.text));
        let mut meta = Map::new();
        meta.insert("representation".into(), json!(source.representation));
        meta.insert("original_bytes".into(), json!(source.original_bytes));
        meta.insert("rendered_bytes".into(), json!(source.rendered_bytes));
        if !source.elisions.is_empty() {
            meta.insert("elisions".into(), json!(source.elisions));
        }
        if source.budget_truncated {
            meta.insert("budget_truncated".into(), json!(true));
        }
        value.insert("source_meta".into(), Value::Object(meta));
    }
    Value::Object(value)
}

fn compact_target(target: &query::SymbolTarget) -> Value {
    let mut value = Map::new();
    value.insert(
        "at".into(),
        json!(format!("{}:{}", target.file, target.line)),
    );
    value.insert("symbol".into(), json!(target.name));
    value.insert("kind".into(), json!(target.kind));
    if target.exported {
        value.insert("exported".into(), json!(true));
    }
    if target.file_origin == "dependency" {
        value.insert("origin".into(), json!("dependency"));
    }
    Value::Object(value)
}

fn confidence_rank(confidence: &str) -> usize {
    match confidence {
        "certain" => 0,
        "likely" => 1,
        "possible" => 2,
        _ => 3,
    }
}

fn normalized_confidence(confidence: &str) -> &'static str {
    match confidence {
        "certain" => "certain",
        "likely" => "likely",
        "possible" => "possible",
        _ => "unknown",
    }
}

fn truncate_utf8(text: &str, maximum: usize) -> &str {
    let mut end = maximum.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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
    use serde_json::json;

    use super::{
        compact_hit, definition_string, render_neighborhood, search_string, semantic_preview,
        who_uses_string,
    };
    use crate::{
        origin,
        query::{SymbolTarget, Usage},
        scout::RenderedSource,
        search::{
            Hit, MatchReason, ResponseBudget, RetrievalStatus, SearchExpansion, SearchResult,
        },
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
                repository_role: None,
                file_origin: "repository".into(),
                kind: "method".into(),
                name: Some("start".into()),
                start_line: 1,
                end_line: 8,
                score: 0.98,
                match_reason: MatchReason::Hybrid,
                matched_identifiers: Vec::new(),
                snippet: "start() { return this.queue.finish(); }".into(),
                snippet_truncated: false,
                anchors: vec![root.into()],
                file_anchor: "file:src/workflow.ts".into(),
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
        assert_eq!(
            value["hits"][0]["followups"]["tools"],
            json!(["definition", "who_uses", "neighborhood"])
        );
        assert_eq!(value["hits"][0]["followups"]["arguments"]["anchor"], root);
        assert_eq!(
            value["hits"][0]["followups"]["arguments"]["snapshot"],
            "s".repeat(64)
        );
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
    fn file_only_search_hits_offer_only_file_compatible_followups() {
        let hit = Hit {
            chunk_id: 1,
            file: "src/config.ts".into(),
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
            snippet: "export const config = {};".into(),
            snippet_truncated: false,
            anchors: Vec::new(),
            file_anchor: "file:src/config.ts".into(),
            uses: Vec::new(),
            used_by: Vec::new(),
            include_followups: true,
            include_neighborhood_followup: true,
        };
        let value = compact_hit(&hit, "snapshot");
        assert_eq!(value["anchor"], "file:src/config.ts");
        assert_eq!(value["followups"]["calls"][0]["tool"], "file_outline");
        assert_eq!(value["followups"]["calls"][1]["tool"], "neighborhood");
        assert!(
            value["followups"]["calls"]
                .as_array()
                .unwrap()
                .iter()
                .all(|call| call["tool"] != "definition" && call["tool"] != "who_uses")
        );
    }

    #[test]
    fn ambiguous_search_hits_do_not_emit_copy_unsafe_followups() {
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
            snippet: "const first = 1; const second = 2;".into(),
            snippet_truncated: false,
            anchors: vec![
                "sym:src/overlap.ts#::first@1".into(),
                "sym:src/overlap.ts#::second@1".into(),
            ],
            file_anchor: "file:src/overlap.ts".into(),
            uses: Vec::new(),
            used_by: Vec::new(),
            include_followups: true,
            include_neighborhood_followup: true,
        };
        let value = compact_hit(&hit, "snapshot");
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
                snippet: format!("export function handleStep{index}() {{ return next(); }}"),
                snippet_truncated: false,
                anchors: vec![format!(
                    "sym:src/services/service-{index}.ts#::handleStep{index}@{}",
                    10 + index
                )],
                file_anchor: format!("file:src/services/service-{index}.ts"),
                uses: vec!["next (call)".into()],
                used_by: Vec::new(),
                include_followups: true,
                include_neighborhood_followup: true,
            })
            .collect();
        let result = SearchResult {
            snapshot: "s".repeat(64),
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
        assert_eq!(memory["candidate_pool"], 8);
        assert_eq!(memory["selected"], 1);
        assert_eq!(memory["returned"], 1);
        assert_eq!(memory["attachment"]["status"], "connected");
        assert_eq!(memory["next_tool"], "semantic_memory");
        assert_eq!(
            memory["artifacts"][0]["summary"],
            "Resolves a cached route while preserving rewrite state."
        );
        assert!(memory["artifacts"][0].get("body").is_none());
        assert!(memory["artifacts"][0].get("supports").is_none());
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
        assert_eq!(value["semantic_memory"]["candidate_pool"], 0);
        assert_eq!(value["semantic_memory"]["returned"], 0);
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
        assert_eq!(value["semantic_memory"]["candidate_pool"], 12);
        assert_eq!(value["semantic_memory"]["selected"], 3);
        assert_eq!(value["semantic_memory"]["returned"], 0);
        assert_eq!(value["semantic_memory"]["budget_omitted"], 3);
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

        let rendered = who_uses_string(&results, 4_000)?;
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

        let rendered = who_uses_string(&results, 1_200)?;
        let value: serde_json::Value = serde_json::from_str(&rendered)?;

        assert!(rendered.len() <= 1_200);
        assert_eq!(value["response"]["rendered_bytes"], rendered.len());
        assert_eq!(value["response"]["truncated"], true);
        assert!(value["response"]["returned_usages"].as_u64().unwrap() < 100);
        assert!(value["targets"][0]["usages"].get("certain").is_some());
        assert!(value["targets"][0]["usages"].get("possible").is_none());
        Ok(())
    }

    #[test]
    fn compact_definition_serializes_source_once_and_obeys_the_whole_budget() -> anyhow::Result<()>
    {
        let target = symbol_target();
        let source = RenderedSource {
            representation: "full",
            text: format!("UNIQUE_SOURCE_MARKER\n{}", "const value = 1;\n".repeat(200)),
            byte_limit: 8_000,
            original_bytes: 3_500,
            rendered_bytes: 3_500,
            compression_ratio: 1.0,
            elisions: Vec::new(),
            budget_truncated: false,
        };

        let rendered = definition_string(&[(target, Some(source))], 1, 1_000)?;
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
        Ok(())
    }
}
