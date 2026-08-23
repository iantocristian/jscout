use std::fs::{self, OpenOptions};
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;

use super::{
    AppliedResultTransport, McpClientInfo, ResultTransportPolicy, ToolProfile, call_tool,
    definition_source_metrics, duration_ms, expansion_role_metrics, log_request,
    render_bounded_items, render_tool_result, search_options_from_args, semantic_artifact_metrics,
    server_instructions, sum_durations, tool_defs,
};
use crate::{config, embed, indexer, scout::SourceView, search, store, structural};

#[test]
fn omitted_search_arguments_use_repository_defaults_and_explicit_values_win() -> Result<()> {
    let defaults = config::SearchSettings {
        vector: false,
        rerank: false,
        attach_memory: false,
        limit: 3,
        response_bytes: 9_000,
        expansion: config::ExpansionSettings {
            enabled: true,
            nodes: 17,
            ..Default::default()
        },
        ..Default::default()
    };

    let (vector, options) = search_options_from_args(
        ToolProfile::Structural,
        &json!({ "query": "dispatch" }),
        &defaults,
    )?;
    assert!(!vector);
    assert!(!options.rerank);
    assert!(!options.include_memory);
    assert_eq!(options.limit, 3);
    assert_eq!(options.response_byte_limit, 9_000);
    assert!(options.expand);
    assert_eq!(
        options.expansion.projection,
        search::ExpansionProjection::Paths
    );
    assert_eq!(options.expansion.path_limit, 8);
    assert_eq!(options.expansion.node_limit, 17);

    let (vector, options) = search_options_from_args(
        ToolProfile::Structural,
        &json!({
            "query": "dispatch",
            "vector": true,
            "rerank": true,
            "include_memory": true,
            "limit": 8,
            "expand": false,
            "expand_mode": "neighborhood",
            "expand_paths": 2
        }),
        &defaults,
    )?;
    assert!(vector);
    assert!(options.rerank);
    assert!(options.include_memory);
    assert_eq!(options.limit, 8);
    assert!(!options.expand);
    assert_eq!(
        options.expansion.projection,
        search::ExpansionProjection::Neighborhood
    );
    assert_eq!(options.expansion.path_limit, 2);
    Ok(())
}

#[test]
fn exhaustive_search_overrides_configured_stages_and_rejects_only_explicit_enables() -> Result<()> {
    let defaults = config::SearchSettings {
        vector: true,
        rerank: true,
        attach_memory: true,
        limit: 7,
        expansion: config::ExpansionSettings {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let (vector, options) = search_options_from_args(
        ToolProfile::Structural,
        &json!({ "query": "dispatch", "exhaustive": true }),
        &defaults,
    )?;
    assert!(!vector);
    assert_eq!(
        options.mode,
        search::SearchMode::Exhaustive { cursor: None }
    );
    assert_eq!(options.limit, 7);
    assert!(!options.rerank);
    assert!(!options.include_memory);
    assert!(!options.expand);

    let (_, continued) = search_options_from_args(
        ToolProfile::Structural,
        &json!({
            "query": "dispatch",
            "exhaustive": true,
            "cursor": "opaque",
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "expand": false
        }),
        &defaults,
    )?;
    assert_eq!(
        continued.mode,
        search::SearchMode::Exhaustive {
            cursor: Some("opaque".into())
        }
    );

    for field in ["vector", "rerank", "include_memory", "expand"] {
        let error = search_options_from_args(
            ToolProfile::Structural,
            &json!({ "query": "dispatch", "exhaustive": true, (field): true }),
            &defaults,
        )
        .expect_err("explicitly enabled conflicting stage must fail");
        assert!(error.to_string().contains(field));
    }
    let cursor_without_mode = search_options_from_args(
        ToolProfile::Structural,
        &json!({ "query": "dispatch", "cursor": "opaque" }),
        &defaults,
    )
    .expect_err("cursor without exhaustive mode must fail");
    assert!(
        cursor_without_mode
            .to_string()
            .contains("requires exhaustive")
    );
    Ok(())
}

#[test]
fn profile_instructions_explain_when_to_use_structural_traversal() {
    let baseline = server_instructions(ToolProfile::Baseline);
    let structural = server_instructions(ToolProfile::Structural);
    assert!(baseline.contains("semantic_search"));
    assert!(baseline.contains("calls for exact member-method"));
    assert!(baseline.contains("repository alone does not mean the whole repository"));
    assert!(!baseline.contains("neighborhood"));
    assert!(structural.contains("neighborhood"));
    assert!(structural.contains("repository_overview"));
    assert!(structural.contains("semantic_memory"));
    assert!(structural.contains("entities"));
    assert!(structural.contains("paths"));
    assert!(structural.contains("calls for exact member-method"));
    assert!(structural.contains("opt-in evidence-connected preview"));
    assert!(structural.contains("Split multi-clause tasks"));
    assert!(structural.contains("repository byte budget"));
    assert!(structural.contains("follow-up search"));
    assert!(structural.contains("Verify decisive claims in source"));
    assert!(structural.contains("direct participants field"));
    assert!(structural.contains("as defining"));
    assert!(structural.contains("as supporting"));
    assert!(structural.contains("repository alone does not mean the whole repository"));

    let tools = tool_defs(ToolProfile::Structural);
    let search = tools
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "semantic_search")
        .expect("search definition");
    let origins = &search["inputSchema"]["properties"]["origins"];
    assert!(origins.get("default").is_none());
    assert!(
        origins["description"]
            .as_str()
            .is_some_and(|description| description.contains("repository configuration"))
    );
    assert!(
        origins["description"]
            .as_str()
            .is_some_and(|description| description.contains("not the whole repository"))
    );
}

#[test]
fn auto_structured_transport_is_scoped_to_verified_codex_versions() -> Result<()> {
    let codex = McpClientInfo {
        name: Some("codex-mcp-client".to_string()),
        version: Some("0.147.0".to_string()),
    };
    let future_codex = McpClientInfo {
        version: Some("0.148.0-dev.1".to_string()),
        ..codex.clone()
    };
    let old_codex = McpClientInfo {
        version: Some("0.146.9".to_string()),
        ..codex.clone()
    };
    let unknown = McpClientInfo {
        name: Some("claude-code".to_string()),
        version: Some("2.1.220".to_string()),
    };

    assert_eq!(
        ResultTransportPolicy::parse("auto")?.resolve(&codex),
        AppliedResultTransport::Structured
    );
    assert_eq!(
        ResultTransportPolicy::Auto.resolve(&future_codex),
        AppliedResultTransport::Structured
    );
    assert_eq!(
        ResultTransportPolicy::Auto.resolve(&old_codex),
        AppliedResultTransport::Text
    );
    assert_eq!(
        ResultTransportPolicy::Auto.resolve(&unknown),
        AppliedResultTransport::Text
    );
    assert!(ResultTransportPolicy::parse("both").is_err());
    Ok(())
}

#[test]
fn structured_transport_keeps_equal_json_text_fallback_without_double_encoding() {
    let result = Ok(r#"{"snapshot":"abc","hits":[{"anchor":"sym:a"}]}"#.to_string());
    let (wire, metrics) = render_tool_result(
        &result,
        ResultTransportPolicy::Structured,
        &McpClientInfo::default(),
    );

    assert_eq!(metrics.applied, AppliedResultTransport::Structured);
    assert_eq!(
        wire["structuredContent"],
        serde_json::from_str::<serde_json::Value>(result.as_ref().unwrap()).unwrap()
    );
    assert_eq!(
        wire["content"][0]["text"].as_str(),
        Some(result.as_ref().unwrap().as_str())
    );
    assert!(wire["structuredContent"].is_object());
    assert!(!metrics.structured_parse_failed);
    assert!(metrics.structured_content_bytes.is_some());
    assert!(metrics.tool_result_wire_bytes > metrics.fallback_text_bytes);
}

#[test]
fn invalid_json_and_errors_fail_back_to_text_only() {
    let invalid = Ok("plain output".to_string());
    let (invalid_wire, invalid_metrics) = render_tool_result(
        &invalid,
        ResultTransportPolicy::Structured,
        &McpClientInfo::default(),
    );
    assert_eq!(invalid_metrics.applied, AppliedResultTransport::Text);
    assert!(invalid_metrics.structured_parse_failed);
    assert!(invalid_wire.get("structuredContent").is_none());

    let failure: anyhow::Result<String> = Err(anyhow::anyhow!("broken result"));
    let (error_wire, error_metrics) = render_tool_result(
        &failure,
        ResultTransportPolicy::Structured,
        &McpClientInfo::default(),
    );
    assert_eq!(error_metrics.applied, AppliedResultTransport::Text);
    assert_eq!(error_wire["isError"], true);
    assert!(error_wire.get("structuredContent").is_none());
}

#[test]
fn request_log_records_order_and_exact_tool_arguments() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("requests.jsonl");
    let mut file = Some(OpenOptions::new().create(true).append(true).open(&path)?);
    log_request(
        &mut file,
        ToolProfile::Structural,
        7,
        "tools/call",
        &json!({
            "name": "semantic_search",
            "arguments": { "query": "live request headers", "expand": true }
        }),
    );
    drop(file);

    let row: serde_json::Value = serde_json::from_str(fs::read_to_string(path)?.trim())?;
    assert_eq!(row["sequence"], 7);
    assert_eq!(row["method"], "tools/call");
    assert_eq!(row["tool"], "semantic_search");
    assert_eq!(row["arguments"]["query"], "live request headers");
    assert_eq!(row["arguments"]["expand"], true);
    Ok(())
}

#[test]
fn paths_schema_caps_graph_scope() {
    let structural = tool_defs(ToolProfile::Structural);
    let paths = structural
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "paths")
        .expect("paths definition");
    let properties = &paths["inputSchema"]["properties"];
    assert_eq!(properties["node_limit"]["maximum"], 200);
    assert_eq!(properties["edge_limit"]["maximum"], 800);
}

#[test]
fn neighborhood_schema_has_a_whole_response_budget() {
    let structural = tool_defs(ToolProfile::Structural);
    let neighborhood = structural
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "neighborhood")
        .expect("neighborhood definition");
    assert_eq!(
        neighborhood["inputSchema"]["properties"]["response_bytes"]["default"],
        24_000
    );
    assert_eq!(
        neighborhood["inputSchema"]["properties"]["debug"]["default"],
        false
    );
    let search = structural
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "semantic_search")
        .expect("search definition");
    assert_eq!(
        search["inputSchema"]["properties"]["debug"]["default"],
        false
    );
    assert!(
        search["inputSchema"]["properties"]["limit"]
            .get("default")
            .is_none()
    );
    assert_eq!(
        search["inputSchema"]["properties"]["exhaustive"]["default"],
        false
    );
    assert!(search["inputSchema"]["properties"].get("cursor").is_some());
    assert_eq!(
        search["inputSchema"]["properties"]["memory_depth"]["maximum"],
        crate::search::MAX_MEMORY_GRAPH_DEPTH
    );
    assert_eq!(
        search["inputSchema"]["properties"]["memory_nodes"]["maximum"],
        crate::search::MAX_MEMORY_GRAPH_NODE_LIMIT
    );
    let definition = structural
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "definition")
        .unwrap();
    assert!(
        definition["inputSchema"]["properties"]
            .get("anchor")
            .is_some()
    );
    assert!(
        definition["inputSchema"]["properties"]
            .get("snapshot")
            .is_some()
    );
    let overview = structural
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "repository_overview")
        .unwrap();
    assert!(
        overview["inputSchema"]["properties"]
            .get("reconnaissance_subject")
            .is_some()
    );
    let events = structural
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "events")
        .expect("events definition");
    assert_eq!(
        events["inputSchema"]["properties"]["response_bytes"]["default"],
        24_000
    );
}

#[test]
fn annotate_schema_exposes_workflow_participant_scope() {
    let structural = tool_defs(ToolProfile::Structural);
    let annotate = structural
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "annotate")
        .expect("annotate definition");
    let participant = &annotate["inputSchema"]["properties"]["participants"]["items"];
    assert_eq!(
        participant["properties"]["scope"]["enum"],
        json!(["defining", "supporting"])
    );
    assert!(
        participant["required"]
            .as_array()
            .expect("participant required fields")
            .iter()
            .any(|field| field == "scope")
    );
}

#[test]
fn annotate_parse_error_returns_the_complete_workflow_shape() {
    let conn = Connection::open_in_memory().expect("memory database");
    let error = call_tool(
        Path::new("."),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "annotate",
        &json!({}),
    )
    .expect_err("empty annotate request must fail");
    let message = error.to_string();
    assert!(message.contains("one complete object"));
    assert!(message.contains("participants"));
    assert!(message.contains("do not send body/supports"));
}

#[test]
fn baseline_profile_removes_structural_tools_and_expansion_controls() {
    let baseline = tool_defs(ToolProfile::Baseline);
    let tools = baseline.as_array().expect("tool definitions");
    assert!(!tools.iter().any(|tool| tool["name"] == "neighborhood"));
    assert!(!tools.iter().any(|tool| tool["name"] == "annotate"));
    assert!(!tools.iter().any(|tool| tool["name"] == "entities"));
    assert!(!tools.iter().any(|tool| tool["name"] == "paths"));
    assert!(!tools.iter().any(|tool| tool["name"] == "semantic_memory"));
    assert!(
        !tools
            .iter()
            .any(|tool| tool["name"] == "repository_overview")
    );
    let search = tools
        .iter()
        .find(|tool| tool["name"] == "semantic_search")
        .expect("semantic_search definition");
    assert!(search["inputSchema"]["properties"].get("expand").is_none());
    assert!(
        search["inputSchema"]["properties"]
            .get("exhaustive")
            .is_some()
    );
    assert!(search["inputSchema"]["properties"].get("cursor").is_some());
    assert!(
        search["inputSchema"]["properties"]
            .get("include_memory")
            .is_none()
    );
    assert!(
        search["inputSchema"]["properties"]
            .get("response_bytes")
            .is_some()
    );
    let definition = tools
        .iter()
        .find(|tool| tool["name"] == "definition")
        .expect("definition tool");
    assert!(
        definition["inputSchema"]["properties"]
            .get("view")
            .is_some()
    );
    assert!(
        definition["inputSchema"]["properties"]
            .get("source_bytes")
            .is_some()
    );
    assert!(
        definition["inputSchema"]["properties"]
            .get("response_bytes")
            .is_some()
    );
    assert!(
        definition["inputSchema"]["properties"]
            .get("debug")
            .is_some()
    );
    let who_uses = tools
        .iter()
        .find(|tool| tool["name"] == "who_uses")
        .expect("who_uses tool");
    assert!(
        who_uses["inputSchema"]["properties"]
            .get("response_bytes")
            .is_some()
    );
    let calls = tools
        .iter()
        .find(|tool| tool["name"] == "calls")
        .expect("calls tool");
    assert!(
        calls["inputSchema"]["properties"]
            .get("response_bytes")
            .is_some()
    );

    let structural = tool_defs(ToolProfile::Structural);
    let tools = structural.as_array().expect("tool definitions");
    assert!(tools.iter().any(|tool| tool["name"] == "neighborhood"));
    assert!(tools.iter().any(|tool| tool["name"] == "annotate"));
    assert!(tools.iter().any(|tool| tool["name"] == "entities"));
    assert!(tools.iter().any(|tool| tool["name"] == "paths"));
    assert!(tools.iter().any(|tool| tool["name"] == "semantic_memory"));
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "repository_overview")
    );
    let search = tools
        .iter()
        .find(|tool| tool["name"] == "semantic_search")
        .expect("semantic_search definition");
    assert!(search["inputSchema"]["properties"].get("expand").is_some());
    let memory = tools
        .iter()
        .find(|tool| tool["name"] == "semantic_memory")
        .expect("semantic_memory definition");
    assert!(
        memory["inputSchema"]["properties"]["vector"]
            .get("default")
            .is_none()
    );
    assert!(memory["inputSchema"]["properties"].get("file").is_some());
    assert_eq!(
        memory["inputSchema"]["properties"]["view"]["enum"],
        json!(["compact", "body", "full"])
    );
    assert_eq!(
        memory["inputSchema"]["properties"]["source_limit"]["default"],
        1
    );
    assert!(
        memory["inputSchema"]["properties"]
            .get("reconnaissance_subject")
            .is_some()
    );
}

#[test]
fn baseline_profile_rejects_structural_calls_even_if_client_bypasses_schema() {
    let conn = Connection::open_in_memory().expect("in-memory database");
    let expanded = call_tool(
        Path::new("."),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "semantic_search",
        &json!({ "query": "x", "expand": true }),
    );
    assert!(
        expanded
            .unwrap_err()
            .to_string()
            .contains("baseline MCP profile")
    );
    let neighborhood = call_tool(
        Path::new("."),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "neighborhood",
        &json!({ "anchor": "file:x.ts" }),
    );
    assert!(
        neighborhood
            .unwrap_err()
            .to_string()
            .contains("baseline MCP profile")
    );
    let entities = call_tool(
        Path::new("."),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "entities",
        &json!({}),
    );
    assert!(
        entities
            .unwrap_err()
            .to_string()
            .contains("baseline MCP profile")
    );
    let annotate = call_tool(
        Path::new("."),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "annotate",
        &json!({}),
    );
    assert!(
        annotate
            .unwrap_err()
            .to_string()
            .contains("baseline MCP profile")
    );
}

#[test]
fn agent_surfaces_are_wired_and_response_bounded() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("flow.ts"),
        "export function finish() { return process.env.API_KEY; }\n\
         export function start() { return finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let entities = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "entities",
        &json!({ "query": "API_KEY", "response_bytes": 4000 }),
    )?;
    let entities: serde_json::Value = serde_json::from_str(&entities)?;
    assert_eq!(entities["entities"][0]["name"], "API_KEY");
    assert!(
        entities["response_budget"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            <= 4000
    );

    let paths = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "paths",
        &json!({
            "from": "flow.ts:start",
            "to": "flow.ts:finish",
            "direction": "out",
            "kinds": ["call"],
            "node_limit": 100_000,
            "edge_limit": 100_000,
            "response_bytes": 4000
        }),
    )?;
    let paths: serde_json::Value = serde_json::from_str(&paths)?;
    assert_eq!(paths["paths"][0]["steps"][0]["edge"]["kind"], "call");
    assert!(paths["response_budget"]["rendered_bytes"].as_u64().unwrap() <= 4000);

    let overview = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "repository_overview",
        &json!({ "area_limit": 5, "relation_limit": 5, "response_bytes": 4000 }),
    )?;
    let overview: serde_json::Value = serde_json::from_str(&overview)?;
    assert_eq!(overview["totals"]["files"], 1);
    assert!(
        overview["response_budget"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            <= 4000
    );

    let events = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "events",
        &json!({ "response_bytes": 1_000 }),
    )?;
    let events: serde_json::Value = serde_json::from_str(&events)?;
    assert!(events["events"].is_array());
    assert!(
        events["response_budget"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            <= 1_000
    );
    Ok(())
}

#[test]
fn calls_response_obeys_complete_byte_budget() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = (0..40)
        .map(|index| {
            format!(
                "export function run{index}(db: any) {{ return db.items.insert({{ merge: 'replace' }}); }}\n"
            )
        })
        .collect::<String>();
    fs::write(repo.path().join("calls.ts"), source)?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rendered = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "calls",
        &json!({
            "method": "insert",
            "args": ["merge=replace"],
            "response_bytes": 1_500
        }),
    )?;
    let result: serde_json::Value = serde_json::from_str(&rendered)?;
    assert!(rendered.len() <= 1_500);
    assert_eq!(result["response_budget"]["rendered_bytes"], rendered.len());
    assert_eq!(result["response_budget"]["truncated"], true);
    assert!(result["matches"].as_array().unwrap().len() < 40);
    Ok(())
}

#[test]
fn annotate_writes_fresh_workflow_memory_retrieved_only_by_structural_profile() -> Result<()> {
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
    let workflow = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "annotate",
        &json!({
            "type": "workflow",
            "name": "handoff workflow",
            "participants": [
                { "anchor": alpha, "role": "starts handoff", "scope": "defining", "evidence_file": "a.ts", "evidence_start_line": 1, "evidence_end_line": 1, "confidence": "likely" },
                { "anchor": beta, "role": "finishes handoff", "scope": "supporting", "evidence_file": "b.ts", "evidence_start_line": 1, "evidence_end_line": 1, "confidence": "likely" }
            ],
            "confidence": "likely",
            "snapshot": snapshot
        }),
    )?;
    let workflow: serde_json::Value = serde_json::from_str(&workflow)?;
    assert_eq!(workflow["freshness"], "fresh");
    let workflow_id = workflow["id"].as_i64().expect("published workflow id");

    let memory_discovery = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_memory",
        &json!({ "query": "handoff workflow", "vector": false }),
    )?;
    let memory_discovery: serde_json::Value = serde_json::from_str(&memory_discovery)?;
    assert_eq!(
        memory_discovery["artifact_handles"][0]["followup"]["arguments"]["view"],
        "body"
    );
    assert!(memory_discovery.get("candidate_artifacts").is_none());
    assert!(
        memory_discovery["artifact_handles"][0]
            .get("retrieval_score")
            .is_none()
    );

    let compact_memory = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_memory",
        &json!({ "artifact": workflow_id, "vector": false }),
    )?;
    let compact_memory: serde_json::Value = serde_json::from_str(&compact_memory)?;
    assert_eq!(compact_memory["view"], "compact");
    assert_eq!(
        compact_memory["semantic_artifacts"][0]["primary_claim"],
        "starts handoff"
    );
    assert_eq!(
        compact_memory["semantic_artifacts"][0]["defining_participants"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert!(
        compact_memory["semantic_artifacts"][0]
            .get("body")
            .is_none()
    );
    assert!(
        compact_memory["response_budget"]["omitted"]["supports"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );

    let body_memory = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_memory",
        &memory_discovery["artifact_handles"][0]["followup"]["arguments"],
    )?;
    let body_memory: serde_json::Value = serde_json::from_str(&body_memory)?;
    assert_eq!(body_memory["view"], "body");
    assert_eq!(
        body_memory["semantic_artifacts"][0]["body"]["participants"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        body_memory["semantic_artifacts"][0]["evidence"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert!(
        body_memory["semantic_artifacts"][0]["evidence"][0]
            .get("source_hash")
            .is_none()
    );

    let full_memory = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_memory",
        &json!({ "artifact": workflow_id, "view": "full", "vector": false }),
    )?;
    let full_memory: serde_json::Value = serde_json::from_str(&full_memory)?;
    assert!(full_memory["semantic_artifacts"][0]["model"].is_string());
    assert!(full_memory["semantic_artifacts"][0]["supports"][0]["source_hash"].is_string());

    let structural_search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({ "query": "alpha handoff", "include_memory": true }),
    )?;
    let structural_search: serde_json::Value = serde_json::from_str(&structural_search)?;
    assert_eq!(
        structural_search["semantic_memory"]["artifacts"][0]["name"],
        "handoff workflow"
    );
    assert!(
        structural_search["semantic_memory"]
            .get("retrieval")
            .is_none()
    );
    assert!(
        structural_search["semantic_memory"]
            .get("candidate_pool")
            .is_none()
    );
    assert!(
        structural_search["semantic_memory"]
            .get("selected")
            .is_none()
    );
    assert_eq!(
        structural_search["semantic_memory"]["next_tool"],
        "semantic_memory"
    );
    assert!(
        structural_search["semantic_memory"]["artifacts"][0]
            .get("body")
            .is_none()
    );

    let diagnostic_search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({ "query": "alpha handoff", "include_memory": true, "debug": true }),
    )?;
    let diagnostic_search: serde_json::Value = serde_json::from_str(&diagnostic_search)?;
    assert_eq!(diagnostic_search["semantic_candidates"], 1);
    assert_eq!(diagnostic_search["semantic_selected"], 1);
    assert_eq!(
        diagnostic_search["semantic_retrieval"]["vector"],
        "disabled"
    );
    assert!(
        diagnostic_search["response_budget"]["transport_sections"]["total_bytes"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        diagnostic_search["semantic_artifacts"][0]["name"],
        "handoff workflow"
    );

    let baseline_search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "semantic_search",
        &json!({ "query": "handoff", "include_memory": true }),
    )?;
    let baseline_search: serde_json::Value = serde_json::from_str(&baseline_search)?;
    assert!(baseline_search.get("semantic_artifacts").is_none());
    Ok(())
}

#[test]
fn definition_renders_configured_source_view_with_a_shared_byte_ceiling() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("a.ts"),
        "export function run(value: number): number {\n  const localPlumbingWithALongName = value + value + value + value;\n  if (value < 0) throw new Error('negative');\n  return value;\n}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let elided = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Elided,
        "definition",
        &json!({ "symbol": "run", "source_bytes": 512 }),
    )?;
    let elided: serde_json::Value = serde_json::from_str(&elided)?;
    assert_eq!(
        elided["definitions"][0]["source_meta"]["representation"],
        "elided"
    );
    assert!(
        elided["definitions"][0]["source_meta"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            <= 512
    );
    assert!(
        !elided["definitions"][0]["source"]
            .as_str()
            .unwrap()
            .contains("localPlumbingWithALongName")
    );

    let full = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Elided,
        "definition",
        &json!({ "symbol": "run", "view": "full", "source_bytes": 512 }),
    )?;
    let full: serde_json::Value = serde_json::from_str(&full)?;
    assert_eq!(
        full["definitions"][0]["source_meta"]["representation"],
        "full"
    );
    assert!(
        full["definitions"][0]["source_meta"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            <= 512
    );
    assert!(
        full["definitions"][0]["source"]
            .as_str()
            .unwrap()
            .contains("localPlumbingWithALongName")
    );

    let debug = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "definition",
        &json!({ "symbol": "run", "source_bytes": 512, "debug": true }),
    )?;
    let debug: serde_json::Value = serde_json::from_str(&debug)?;
    assert!(debug.is_array());
    assert_eq!(debug[0]["source"], debug[0]["source_meta"]["text"]);
    Ok(())
}

#[test]
fn search_followups_round_trip_exact_same_named_methods() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("methods.ts"),
        "class First {\n  run() { return 'FIRST_MARKER'; }\n}\n\nclass Second {\n  run() { return 'SECOND_MARKER'; }\n}\n\nexport function invokeSecond(value: Second) { return value.run(); }\nexport function invokeUnknown(value: any) { return value.run(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let second_anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='run' AND node_key LIKE '%#Second::run@%'
         LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let second_class_anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='Second' LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let caller_anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='invokeSecond' LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let file_id: i64 =
        conn.query_row("SELECT id FROM files WHERE path='methods.ts'", [], |row| {
            row.get(0)
        })?;
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key,dst_key,kind,confidence,provenance,source_file_id,line,detail_json
         ) VALUES(?1,?2,'call','likely','test',?3,9,
                  '{\"request\":\"./methods\",\"targetName\":\"run\",\"detail\":null,\"candidateCount\":1}')",
        rusqlite::params![caller_anchor, second_anchor, file_id],
    )?;

    let search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "SECOND_MARKER",
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "limit": 3
        }),
    )?;
    let search: serde_json::Value = serde_json::from_str(&search)?;
    let hit = search["hits"]
        .as_array()
        .and_then(|hits| hits.iter().find(|hit| hit["anchor"] == second_class_anchor))
        .unwrap_or_else(|| panic!("search hit for exact second class: {search}"));
    let followups = &hit["followups"];
    assert_eq!(
        followups["tools"],
        json!(["definition", "who_uses", "neighborhood"])
    );
    assert_eq!(followups["arguments"]["anchor"], second_class_anchor);
    assert_eq!(followups["arguments"]["snapshot"], search["snapshot"]);

    let baseline_search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "SECOND_MARKER",
            "vector": false,
            "rerank": false,
            "limit": 3
        }),
    )?;
    let baseline_search: serde_json::Value = serde_json::from_str(&baseline_search)?;
    assert_eq!(
        baseline_search["hits"][0]["followups"]["tools"],
        json!(["definition", "who_uses"])
    );

    let definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &followups["arguments"],
    )?;
    let definition_value: serde_json::Value = serde_json::from_str(&definition)?;
    assert_eq!(
        definition_value["resolution"]["resolved_anchor"],
        second_class_anchor
    );
    assert!(
        definition_value["definitions"][0]["source"]
            .as_str()
            .is_some_and(|source| source.contains("SECOND_MARKER"))
    );
    assert!(
        !definition_value["definitions"][0]["source"]
            .as_str()
            .is_some_and(|source| source.contains("FIRST_MARKER"))
    );
    assert_eq!(
        definition_value["response"]["rendered_bytes"],
        definition.len()
    );

    let method_arguments = json!({
        "anchor": second_anchor,
        "snapshot": search["snapshot"],
        "origins": ["repository"]
    });
    let method_definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &method_arguments,
    )?;
    let method_definition: serde_json::Value = serde_json::from_str(&method_definition)?;
    assert_eq!(
        method_definition["resolution"]["resolved_anchor"],
        second_anchor
    );
    assert!(
        method_definition["definitions"][0]["source"]
            .as_str()
            .is_some_and(|source| source.contains("SECOND_MARKER"))
    );

    let usages_rendered = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "who_uses",
        &method_arguments,
    )?;
    let usages: serde_json::Value = serde_json::from_str(&usages_rendered)?;
    assert_eq!(usages["resolution"]["resolved_anchor"], second_anchor);
    assert_eq!(usages["response"]["matched_usages"], 2);
    assert_eq!(
        usages["targets"][0]["usages"]["likely"]["methods.ts"][0],
        json!([9, "call", "invokeSecond"])
    );
    assert_eq!(
        usages["targets"][0]["usages"]["possible"]["methods.ts"][0],
        json!([10, "call", "invokeUnknown", "value.run()"])
    );
    assert!(!usages_rendered.contains("targetName"));
    assert!(!usages_rendered.contains("candidateCount"));

    let neighborhood = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "neighborhood",
        &followups["arguments"],
    )?;
    let neighborhood: serde_json::Value = serde_json::from_str(&neighborhood)?;
    assert_eq!(neighborhood["anchor"], second_class_anchor);

    let fuzzy_snapshot = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &json!({ "symbol": "run", "snapshot": search["snapshot"] }),
    );
    assert!(
        fuzzy_snapshot
            .unwrap_err()
            .to_string()
            .contains("only valid with exact")
    );
    Ok(())
}

#[test]
fn search_followups_preserve_cross_origin_definitions_and_callers() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for (file, source) in [
        (
            "workspace-target.ts",
            "export function workspaceTarget() { return 'workspace-target'; }\n",
        ),
        (
            "repo-caller.ts",
            "import { workspaceTarget } from './workspace-target';\nexport function callWorkspace() { return workspaceTarget(); }\n",
        ),
        (
            "repo-target.ts",
            "export function repoTarget() { return 'repo-target'; }\n",
        ),
        (
            "workspace-caller.ts",
            "import { repoTarget } from './repo-target';\nexport function callRepo() { return repoTarget(); }\n",
        ),
        (
            "dependency-target.ts",
            "export function dependencyTarget() { return 'dependency-target'; }\n",
        ),
        (
            "dependency-caller.ts",
            "import { dependencyTarget } from './dependency-target';\nexport function callDependency() { return dependencyTarget(); }\n",
        ),
    ] {
        fs::write(repo.path().join(file), source)?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    conn.execute(
        "UPDATE files SET origin='workspace' WHERE path IN ('workspace-target.ts','workspace-caller.ts')",
        [],
    )?;
    conn.execute(
        "UPDATE files SET origin='dependency' WHERE path='dependency-target.ts'",
        [],
    )?;

    for (query, caller) in [
        ("workspaceTarget", "repo-caller.ts"),
        ("repoTarget", "workspace-caller.ts"),
    ] {
        let search = call_tool(
            repo.path(),
            &conn,
            None,
            ToolProfile::Structural,
            SourceView::Full,
            "semantic_search",
            &json!({
                "query": query,
                "vector": false,
                "rerank": false,
                "include_memory": false,
                "limit": 3
            }),
        )?;
        let search: serde_json::Value = serde_json::from_str(&search)?;
        let arguments = &search["hits"][0]["followups"]["arguments"];
        assert_eq!(search["hits"][0]["symbol"], query);
        assert!(arguments.get("origins").is_none());
        let usages = call_tool(
            repo.path(),
            &conn,
            None,
            ToolProfile::Structural,
            SourceView::Full,
            "who_uses",
            arguments,
        )?;
        assert!(usages.contains(caller), "missing {caller} in {usages}");
    }

    let dependency_search = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "dependencyTarget",
            "origins": ["repository", "workspace", "dependency"],
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "limit": 3
        }),
    )?;
    let dependency_search: serde_json::Value = serde_json::from_str(&dependency_search)?;
    let arguments = &dependency_search["hits"][0]["followups"]["arguments"];
    assert_eq!(dependency_search["hits"][0]["symbol"], "dependencyTarget");
    assert_eq!(
        arguments["origins"],
        json!(["repository", "workspace", "dependency"])
    );
    let definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        arguments,
    )?;
    assert!(definition.contains("dependency-target"));
    let usages = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "who_uses",
        arguments,
    )?;
    assert!(usages.contains("dependency-caller.ts"));
    Ok(())
}

#[test]
fn bounded_item_envelope_accounts_for_json_overhead() -> Result<()> {
    let items = (0..100)
        .map(|index| json!({ "index": index, "text": "x".repeat(80) }))
        .collect();
    let rendered = render_bounded_items("outline", items, 1_500)?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    assert!(rendered.len() <= 1_500);
    assert_eq!(value["response_budget"]["rendered_bytes"], rendered.len());
    assert_eq!(value["response_budget"]["truncated"], true);
    assert!(value["outline"].as_array().unwrap().len() < 100);
    Ok(())
}

#[test]
fn telemetry_counts_expansion_file_roles_without_recording_payloads() {
    let metrics = expansion_role_metrics(
        &json!({
            "expansion": {
                "nodes": [
                    { "file_role": "production" },
                    { "file_role": "test" },
                    { "file_role": null }
                ]
            }
        })
        .to_string(),
    );
    assert_eq!(metrics.nodes, 3);
    assert_eq!(metrics.file_nodes, 2);
    assert_eq!(metrics.role_counts["production"], 1);
    assert_eq!(metrics.test_fixture_generated, 1);

    let compact = expansion_role_metrics(
        &json!({
            "graph": {
                "nodes": {
                    "n1": { "at": "src/a.ts:1" },
                    "n2": { "at": "tests/a.test.ts:1", "role": "test" },
                    "n3": { "kind": "event", "name": "ready" }
                }
            }
        })
        .to_string(),
    );
    assert_eq!(compact.nodes, 3);
    assert_eq!(compact.file_nodes, 2);
    assert_eq!(compact.role_counts["production"], 1);
    assert_eq!(compact.test_fixture_generated, 1);
}

#[test]
fn telemetry_reads_compact_definition_source_metrics() {
    let metrics = definition_source_metrics(
        &json!({
            "definitions": [{
                "target": { "at": "src/a.ts:1", "symbol": "run", "kind": "function" },
                "source": "run() {}",
                "source_meta": {
                    "representation": "full",
                    "original_bytes": 20,
                    "rendered_bytes": 8,
                    "budget_truncated": true
                }
            }]
        })
        .to_string(),
    );

    assert_eq!(metrics, (1, 8, 20, 1));
}

#[test]
fn telemetry_counts_semantic_artifact_freshness() {
    let metrics = semantic_artifact_metrics(
        &json!({
            "semantic_artifacts": [
                { "freshness": "fresh" },
                { "freshness": "degraded" },
                { "freshness": "stale" }
            ]
        })
        .to_string(),
    );
    assert_eq!(metrics.returned, 3);
    assert_eq!(metrics.fresh, 1);
    assert_eq!(metrics.degraded, 1);
    assert_eq!(metrics.stale, 1);

    let handles = semantic_artifact_metrics(
        &json!({
            "artifact_handles": [
                { "freshness": "fresh" },
                { "freshness": "stale" }
            ],
            "semantic_artifacts": []
        })
        .to_string(),
    );
    assert_eq!(handles.returned, 2);
    assert_eq!(handles.fresh, 1);
    assert_eq!(handles.stale, 1);

    let compact = semantic_artifact_metrics(
        &json!({
            "semantic_memory": {
                "artifacts": [
                    { "freshness": "fresh" },
                    { "freshness": "degraded" }
                ]
            }
        })
        .to_string(),
    );
    assert_eq!(compact.returned, 2);
    assert_eq!(compact.fresh, 1);
    assert_eq!(compact.degraded, 1);

    let overlay = semantic_artifact_metrics(
        &json!({
            "semantic_overlay": {
                "artifacts": [
                    { "freshness": "fresh" },
                    { "freshness": "fresh" }
                ]
            }
        })
        .to_string(),
    );
    assert_eq!(overlay.returned, 2);
    assert_eq!(overlay.fresh, 2);
}

#[test]
fn retrieval_stage_timings_are_aggregated_for_telemetry() {
    let code = embed::VectorSearchTimings {
        embedding_query: std::time::Duration::from_millis(7),
        vector_index: std::time::Duration::from_millis(11),
    };
    let semantic = embed::VectorSearchTimings {
        embedding_query: std::time::Duration::from_millis(3),
        vector_index: std::time::Duration::from_millis(5),
    };
    assert_eq!(
        duration_ms(sum_durations([
            Some(code.embedding_query),
            Some(semantic.embedding_query)
        ])),
        Some(10.0)
    );
    assert_eq!(
        duration_ms(sum_durations([
            Some(code.vector_index),
            Some(semantic.vector_index)
        ])),
        Some(16.0)
    );
}
