//! Compact, self-describing JSON for agent-facing retrieval surfaces.
//!
//! Canonical graph/search structs retain full diagnostic provenance. This
//! module projects them into a transport shape that keeps anchors, source
//! locations, relation identity, confidence, and provenance while removing
//! byte offsets, repeated defaults, occurrence IDs, and empty fields.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::{origin, query, scout, search, structural};

pub(crate) fn search_string(result: &search::SearchResult) -> Result<String> {
    Ok(serde_json::to_string(&search_value(result))?)
}

pub(crate) fn search_rendered_bytes(result: &search::SearchResult) -> Result<usize> {
    Ok(serde_json::to_string(&search_value(result))?.len())
}

pub(crate) fn search_section_bytes(
    result: &search::SearchResult,
) -> Result<search::SearchSectionBytes> {
    let value = search_value(result);
    let total_bytes = serde_json::to_vec(&value)?.len();
    let hits_bytes = serde_json::to_vec(&value["hits"])?.len();
    let graph_bytes = value
        .get("graph")
        .map(serde_json::to_vec)
        .transpose()?
        .map_or(0, |bytes| bytes.len());
    let memory_bytes = value
        .get("semantic_memory")
        .map(serde_json::to_vec)
        .transpose()?
        .map_or(0, |bytes| bytes.len());
    let envelope_bytes = total_bytes
        .saturating_sub(hits_bytes)
        .saturating_sub(graph_bytes)
        .saturating_sub(memory_bytes);
    Ok(search::SearchSectionBytes {
        hits_bytes,
        graph_bytes,
        memory_bytes,
        envelope_bytes,
        total_bytes,
    })
}

pub(crate) fn search_value(result: &search::SearchResult) -> Value {
    let default_match = if result.exhaustive.is_some() {
        search::MatchReason::Lexical
    } else {
        search::MatchReason::Hybrid
    };
    let hits = result
        .hits
        .iter()
        .map(|hit| {
            if result.exhaustive.is_some() {
                compact_exhaustive_hit(
                    hit,
                    &result.snapshot,
                    result.response_budget.exhaustive_locator_only,
                )
            } else {
                compact_hit(hit, &result.snapshot, default_match)
            }
        })
        .collect::<Vec<_>>();
    let mut response = Map::new();
    response.insert("snapshot".into(), json!(result.snapshot));
    if let Some(exhaustive) = &result.exhaustive {
        response.insert("effective".into(), json!(exhaustive.effective));
        response.insert("scope".into(), json!(exhaustive.scope));
        response.insert("total_chunks".into(), json!(exhaustive.total_chunks));
        response.insert("returned".into(), json!(exhaustive.returned));
        response.insert("truncated".into(), json!(exhaustive.truncated));
        response.insert(
            "next_cursor".into(),
            json!(exhaustive.next_cursor.as_deref()),
        );
        if !exhaustive.warnings.is_empty() {
            response.insert("warnings".into(), json!(exhaustive.warnings));
        }
    }
    if search_retrieval_is_actionable(&result.retrieval) {
        response.insert("retrieval".into(), json!(result.retrieval));
    }
    response.insert(
        "default_match".into(),
        json!(match default_match {
            search::MatchReason::Lexical => "lexical",
            _ => "hybrid",
        }),
    );
    response.insert("hits".into(), Value::Array(hits));

    if let Some(retrieval) = result.semantic_retrieval.as_ref().filter(|retrieval| {
        retrieval.corpus_artifacts > 0
            || semantic_retrieval_is_actionable(retrieval)
            || result
                .semantic_attachment
                .as_ref()
                .is_some_and(|attachment| attachment.status != "connected")
    }) {
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
        let mut memory = Map::new();
        memory.insert("trust".into(), json!("untrusted"));
        if semantic_retrieval_is_actionable(retrieval) {
            memory.insert("retrieval".into(), json!(retrieval));
        }
        if let Some(attachment) = result
            .semantic_attachment
            .as_ref()
            .filter(|attachment| attachment.status != "connected" || attachment.graph_truncated)
        {
            memory.insert("attachment".into(), memory_attachment_value(attachment));
        }
        let omitted = result.semantic_selected.saturating_sub(artifacts.len());
        if omitted > 0 {
            memory.insert("omitted".into(), json!({ "artifacts": omitted }));
        }
        memory.insert("next_tool".into(), json!("semantic_memory"));
        memory.insert("artifacts".into(), Value::Array(artifacts));
        response.insert("semantic_memory".into(), Value::Object(memory));
    }

    if let Some(expansion) = &result.expansion {
        let mut graph = graph_value(&expansion.nodes, &expansion.edges, &expansion.seeds);
        graph["projection"] = json!(expansion.projection.as_str());
        if expansion.truncated {
            graph["truncated"] = json!(true);
            let mut omitted = Map::new();
            for (name, count) in [
                ("paths", expansion.omitted_paths),
                ("nodes", expansion.omitted_nodes),
                ("edges", expansion.omitted_edges),
            ] {
                if count > 0 {
                    omitted.insert(name.into(), json!(count));
                }
            }
            if !omitted.is_empty() {
                graph["omitted"] = Value::Object(omitted);
            }
        }
        response.insert("graph".into(), graph);
    }
    if result.response_budget.truncated {
        response.insert(
            "response".into(),
            search_budget_value(&result.response_budget),
        );
    }
    Value::Object(response)
}

fn search_retrieval_is_actionable(retrieval: &search::RetrievalStatus) -> bool {
    retrieval.lexical != "active"
        || matches!(retrieval.vector, "degraded" | "failed")
        || matches!(retrieval.reranker, "degraded" | "failed")
        || retrieval.vector_action.is_some()
}

fn semantic_retrieval_is_actionable(retrieval: &crate::semantic::ArtifactRetrievalStatus) -> bool {
    retrieval.lexical != "active"
        || matches!(retrieval.vector, "degraded" | "failed")
        || retrieval.vector_action.is_some()
}

fn memory_attachment_value(attachment: &search::MemoryAttachmentStatus) -> Value {
    let mut value = Map::new();
    value.insert("status".into(), json!(attachment.status));
    if attachment.graph_truncated {
        value.insert("connected".into(), json!(attachment.connected_candidates));
        let mut graph = Map::new();
        graph.insert("depth".into(), json!(attachment.graph_depth));
        graph.insert("nodes".into(), json!(attachment.graph_nodes));
        graph.insert("truncated".into(), json!(true));
        value.insert("graph".into(), Value::Object(graph));
    }
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

fn compact_hit(hit: &search::Hit, snapshot: &str, default_match: search::MatchReason) -> Value {
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
    if hit.match_reason != default_match {
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
    if hit.anchors.len() <= 1 {
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

fn compact_exhaustive_hit(hit: &search::Hit, snapshot: &str, locator_only: bool) -> Value {
    let mut value = Map::new();
    value.insert(
        "at".into(),
        json!(source_at(
            Some(&hit.file),
            Some(hit.start_line),
            Some(hit.end_line)
        )),
    );
    value.insert("kind".into(), json!(hit.kind));
    value.insert(
        "match_lines".into(),
        json!(hit.match_lines.as_deref().unwrap_or_default()),
    );
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
    if !locator_only && hit.anchors.len() <= 1 {
        let anchor = hit.anchors.first().unwrap_or(&hit.file_anchor);
        value.insert("followups".into(), compact_followups(hit, anchor, snapshot));
    }
    Value::Object(value)
}

fn compact_followups(hit: &search::Hit, anchor: &str, snapshot: &str) -> Value {
    let origins = (hit.file_origin == "dependency").then(|| origin::ALL.to_vec());
    if anchor.starts_with("sym:") {
        let tools = if hit.include_neighborhood_followup {
            vec!["definition", "who_uses", "neighborhood"]
        } else {
            vec!["definition", "who_uses"]
        };
        let mut arguments = Map::new();
        arguments.insert("anchor".into(), json!(anchor));
        arguments.insert("snapshot".into(), json!(snapshot));
        if let Some(origins) = origins {
            arguments.insert("origins".into(), json!(origins));
        }
        let mut followups = Map::new();
        followups.insert("tools".into(), json!(tools));
        if hit.include_followups {
            followups.insert("arguments".into(), Value::Object(arguments));
        }
        Value::Object(followups)
    } else {
        let tools = if hit.include_neighborhood_followup {
            vec!["file_outline", "neighborhood"]
        } else {
            vec!["file_outline"]
        };
        if !hit.include_followups {
            return json!({ "tools": tools });
        }
        let mut outline_arguments = Map::new();
        outline_arguments.insert("path".into(), json!(hit.file));
        if let Some(origins) = &origins {
            outline_arguments.insert("origins".into(), json!(origins));
        }
        let mut calls = vec![json!({
            "tool": "file_outline",
            "arguments": outline_arguments,
        })];
        if hit.include_neighborhood_followup {
            let mut neighborhood_arguments = Map::new();
            neighborhood_arguments.insert("anchor".into(), json!(anchor));
            neighborhood_arguments.insert("snapshot".into(), json!(snapshot));
            if let Some(origins) = &origins {
                neighborhood_arguments.insert("origins".into(), json!(origins));
            }
            calls.push(json!({
                "tool": "neighborhood",
                "arguments": neighborhood_arguments,
            }));
        }
        json!({ "calls": calls })
    }
}

fn search_budget_value(budget: &search::ResponseBudget) -> Value {
    let mut value = Map::new();
    value.insert("rendered_bytes".into(), json!(budget.rendered_bytes));
    if budget.truncated {
        value.insert("truncated".into(), json!(true));
        let mut omitted = Map::new();
        for (name, count) in [
            ("hits", budget.omitted_hits),
            ("memory", budget.omitted_semantic_artifacts),
            ("supports", budget.omitted_semantic_supports),
            ("nodes", budget.omitted_nodes),
            ("edges", budget.omitted_edges),
            ("followup_arguments", budget.omitted_followups),
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

#[expect(
    clippy::too_many_arguments,
    reason = "response builder keeps result selection and complete byte-budget accounting explicit"
)]
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
                tuple.push(compact_receiver_types(receiver_types));
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

fn compact_receiver_types(receiver_types: &[Value]) -> Value {
    const TYPE_LIMIT: usize = 4;
    const DISPLAY_BYTE_LIMIT: usize = 120;

    let mut displays = Vec::new();
    let mut truncated = receiver_types.len() > TYPE_LIMIT;
    for value in receiver_types.iter().take(TYPE_LIMIT) {
        let Some(receiver_type) = value.as_str() else {
            truncated = true;
            continue;
        };
        let mut display = receiver_type.trim().to_string();
        if let Some(generic) = display.find('<') {
            display.truncate(generic);
            display = display.trim().to_string();
            truncated = true;
        }
        if display.len() > DISPLAY_BYTE_LIMIT {
            display = truncate_utf8(&display, DISPLAY_BYTE_LIMIT).to_string();
            truncated = true;
        }
        if !display.is_empty() && !displays.contains(&display) {
            displays.push(display);
        }
    }
    let mut detail = Map::new();
    detail.insert("receiver_types".into(), json!(displays));
    if truncated {
        detail.insert("receiver_types_truncated".into(), json!(true));
    }
    Value::Object(detail)
}

pub(crate) fn render_neighborhood(
    neighborhood: &structural::Neighborhood,
    byte_limit: usize,
) -> Result<String> {
    if byte_limit == 0 {
        anyhow::bail!("response byte limit must be greater than zero");
    }
    let original_nodes = neighborhood.nodes.len();
    let original_edges = neighborhood.edges.len();
    let mut rendered_bytes = 0;
    let mut unbudgeted_bytes = 0;
    for _ in 0..8 {
        let rendered = settle_neighborhood_bytes(
            neighborhood,
            &neighborhood.nodes,
            &neighborhood.edges,
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

    if rendered_bytes <= byte_limit {
        let rendered = serde_json::to_string(&neighborhood_value(
            neighborhood,
            &neighborhood.nodes,
            &neighborhood.edges,
            byte_limit,
            unbudgeted_bytes,
            rendered_bytes,
            original_nodes,
            original_edges,
        ))?;
        debug_assert_eq!(rendered.len(), rendered_bytes);
        return Ok(rendered);
    }

    if neighborhood.edges.is_empty() {
        let non_anchor_nodes = neighborhood
            .nodes
            .iter()
            .filter(|node| node.key != neighborhood.resolved_anchor)
            .count();
        if non_anchor_nodes > 0 {
            let retained = largest_fitting_prefix(non_anchor_nodes, byte_limit, |count| {
                let nodes = neighborhood_prefix_isolated_nodes(neighborhood, count);
                settled_neighborhood_size(
                    neighborhood,
                    &nodes,
                    &[],
                    byte_limit,
                    unbudgeted_bytes,
                    original_nodes,
                    original_edges,
                )
            })?;
            if let Some(retained) = retained {
                let nodes = neighborhood_prefix_isolated_nodes(neighborhood, retained);
                return render_neighborhood_selection(
                    neighborhood,
                    &nodes,
                    &[],
                    byte_limit,
                    unbudgeted_bytes,
                    original_nodes,
                    original_edges,
                );
            }
        }
    } else {
        let retained = largest_fitting_prefix(neighborhood.edges.len(), byte_limit, |count| {
            let nodes = neighborhood_prefix_nodes(neighborhood, count);
            settled_neighborhood_size(
                neighborhood,
                &nodes,
                &neighborhood.edges[..count],
                byte_limit,
                unbudgeted_bytes,
                original_nodes,
                original_edges,
            )
        })?;
        if let Some(retained) = retained {
            let nodes = neighborhood_prefix_nodes(neighborhood, retained);
            return render_neighborhood_selection(
                neighborhood,
                &nodes,
                &neighborhood.edges[..retained],
                byte_limit,
                unbudgeted_bytes,
                original_nodes,
                original_edges,
            );
        }
    }

    let nodes = neighborhood_prefix_isolated_nodes(neighborhood, 0);
    let rendered = settled_neighborhood_size(
        neighborhood,
        &nodes,
        &[],
        byte_limit,
        unbudgeted_bytes,
        original_nodes,
        original_edges,
    )?;
    anyhow::bail!(
        "response byte limit {byte_limit} is below the minimum compact neighborhood envelope ({rendered} bytes)"
    );
}

fn largest_fitting_prefix(
    failing_count: usize,
    byte_limit: usize,
    mut rendered_size: impl FnMut(usize) -> Result<usize>,
) -> Result<Option<usize>> {
    if rendered_size(0)? > byte_limit {
        return Ok(None);
    }
    let mut fitting = 0;
    let mut failing = failing_count;
    while fitting + 1 < failing {
        let candidate = fitting + (failing - fitting) / 2;
        if rendered_size(candidate)? <= byte_limit {
            fitting = candidate;
        } else {
            failing = candidate;
        }
    }

    // Fixed-point metadata can change width at decimal boundaries. Recheck
    // the selected boundary rather than relying on the binary-search probe.
    while fitting > 0 && rendered_size(fitting)? > byte_limit {
        fitting -= 1;
    }
    while fitting + 1 < failing_count && rendered_size(fitting + 1)? <= byte_limit {
        fitting += 1;
    }
    Ok(Some(fitting))
}

fn neighborhood_prefix_nodes(
    neighborhood: &structural::Neighborhood,
    edge_count: usize,
) -> Vec<structural::GraphNode> {
    let mut nodes = neighborhood.nodes.clone();
    if edge_count < neighborhood.edges.len() {
        prune_unreferenced_nodes(
            &mut nodes,
            &neighborhood.edges[..edge_count],
            &neighborhood.resolved_anchor,
        );
    }
    nodes
}

fn neighborhood_prefix_isolated_nodes(
    neighborhood: &structural::Neighborhood,
    retained_non_anchor: usize,
) -> Vec<structural::GraphNode> {
    let mut retained = 0;
    neighborhood
        .nodes
        .iter()
        .filter(|node| {
            if node.key == neighborhood.resolved_anchor {
                true
            } else if retained < retained_non_anchor {
                retained += 1;
                true
            } else {
                false
            }
        })
        .cloned()
        .collect()
}

fn settled_neighborhood_size(
    neighborhood: &structural::Neighborhood,
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    byte_limit: usize,
    unbudgeted_bytes: usize,
    original_nodes: usize,
    original_edges: usize,
) -> Result<usize> {
    let mut rendered_bytes = 0;
    settle_neighborhood_bytes(
        neighborhood,
        nodes,
        edges,
        byte_limit,
        unbudgeted_bytes,
        original_nodes,
        original_edges,
        &mut rendered_bytes,
    )
}

fn render_neighborhood_selection(
    neighborhood: &structural::Neighborhood,
    nodes: &[structural::GraphNode],
    edges: &[structural::GraphEdge],
    byte_limit: usize,
    unbudgeted_bytes: usize,
    original_nodes: usize,
    original_edges: usize,
) -> Result<String> {
    let mut rendered_bytes = 0;
    settle_neighborhood_bytes(
        neighborhood,
        nodes,
        edges,
        byte_limit,
        unbudgeted_bytes,
        original_nodes,
        original_edges,
        &mut rendered_bytes,
    )?;
    let rendered = serde_json::to_string(&neighborhood_value(
        neighborhood,
        nodes,
        edges,
        byte_limit,
        unbudgeted_bytes,
        rendered_bytes,
        original_nodes,
        original_edges,
    ))?;
    debug_assert_eq!(rendered.len(), rendered_bytes);
    Ok(rendered)
}

#[expect(
    clippy::too_many_arguments,
    reason = "fixed-point serialization needs the selected graph and current and original byte counts"
)]
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

#[expect(
    clippy::too_many_arguments,
    reason = "response envelope keeps the selected graph and complete byte-budget metadata explicit"
)]
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
mod tests;
