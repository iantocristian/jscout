use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;

use super::{
    AppliedResultTransport, McpClientInfo, ResultTransportPolicy, ToolAccess, ToolProfile,
    allowed_tool_defs, call_documentation_tool, call_tool, call_tool_with_allowlist,
    definition_source_metrics, duration_ms, ensure_effective_surface, exhaustive_telemetry_metrics,
    expansion_role_metrics, initialize_result, log_request, plan_tool_call, render_bounded_items,
    render_bounded_object_arrays, render_tool_result, search_options_from_args,
    semantic_artifact_metrics, server_instructions, settle_unbudgeted_response,
    settle_value_rendered_bytes, sum_durations, telemetry_snapshot, tool_access, tool_defs,
    tool_registered, validate_tool_names,
};
use crate::{config, embed, indexer, scout::SourceView, search, store, structural};

#[test]
fn initialize_exposes_effective_documentation_freshness_defaults() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join(".jscout.toml"),
        "version = 1\n\n[docs.search]\nfreshness = true\nmax_rank_movement = 3\n",
    )?;
    let runtime = config::RuntimeConfig::load(Some(repo.path()), None)?;
    let result = initialize_result(
        "2025-06-18",
        "test-binary",
        &repo.path().join("index.db"),
        ToolProfile::Structural,
        ResultTransportPolicy::Text,
        &McpClientInfo::default(),
        &runtime,
        &[],
    );
    let defaults = &result["serverInfo"]["documentationRetrievalDefaults"];
    assert_eq!(defaults["freshness"], json!(true));
    assert_eq!(defaults["maxRankMovement"], json!(3));
    Ok(())
}

fn capture_code_read_surfaces(
    root: &Path,
    conn: &Connection,
) -> Result<(
    String,
    BTreeMap<&'static str, String>,
    BTreeSet<&'static str>,
)> {
    let identities = crate::publication::Identities::read(conn)?;
    let snapshot = identities.code.clone();
    let finish_anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='finish'
         ORDER BY node_key LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let start_anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='start'
         ORDER BY node_key LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let mut surfaces = BTreeMap::new();
    let mut covered_tools = BTreeSet::new();
    let mut capture = |label: &'static str,
                       tool: &'static str,
                       arguments: serde_json::Value,
                       expected_fragment: &str|
     -> Result<()> {
        let rendered = call_tool(
            root,
            conn,
            None,
            ToolProfile::Structural,
            SourceView::Full,
            tool,
            &arguments,
        )?;
        assert!(
            rendered.contains(expected_fragment),
            "MCP differential probe `{label}` did not exercise `{expected_fragment}`: {rendered}"
        );
        let mut value: serde_json::Value = serde_json::from_str(&rendered)?;
        assert_eq!(
            value.get("snapshot"),
            Some(&json!(snapshot)),
            "MCP code surface `{label}` returned the wrong plane identity: {rendered}"
        );
        assert_eq!(
            value.get("publication_snapshot"),
            Some(&json!(identities.publication)),
            "MCP code surface `{label}` returned the wrong publication identity: {rendered}"
        );
        assert_eq!(
            count_json_key(&value, "snapshot"),
            1,
            "MCP code surface `{label}` repeated snapshot: {rendered}"
        );
        assert_eq!(
            count_json_key(&value, "publication_snapshot"),
            1,
            "MCP code surface `{label}` repeated publication_snapshot: {rendered}"
        );
        value["snapshot"] = json!("<code-digest>");
        value["publication_snapshot"] = json!("<publication-snapshot>");
        covered_tools.insert(tool);
        surfaces.insert(label, serde_json::to_string(&value)?);
        Ok(())
    };

    capture(
        "search/ranked",
        "semantic_search",
        json!({
            "query": "finish API_KEY",
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "expand": false,
            "limit": 20,
            "response_bytes": 100_000
        }),
        "finish",
    )?;
    capture(
        "search/exhaustive",
        "semantic_search",
        json!({
            "query": "finish API_KEY",
            "exhaustive": true,
            "limit": 200,
            "response_bytes": 100_000
        }),
        "finish",
    )?;
    capture(
        "search/expanded",
        "semantic_search",
        json!({
            "query": "finish",
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "expand": true,
            "expand_mode": "paths",
            "limit": 20,
            "response_bytes": 100_000
        }),
        "finish",
    )?;
    let exact = json!({
        "anchor": finish_anchor,
        "snapshot": snapshot,
        "origins": ["repository"],
        "response_bytes": 100_000
    });
    capture("definition", "definition", exact.clone(), "finish")?;
    capture("who_uses", "who_uses", exact.clone(), "start")?;
    capture("neighborhood", "neighborhood", exact, "finish")?;
    capture(
        "file_outline",
        "file_outline",
        json!({ "path": "src/service.ts", "response_bytes": 100_000 }),
        "finish",
    )?;
    capture(
        "events",
        "events",
        json!({ "name": "ready", "response_bytes": 100_000 }),
        "ready",
    )?;
    capture(
        "calls",
        "calls",
        json!({
            "method": "insert",
            "args": ["merge=replace"],
            "response_bytes": 100_000
        }),
        "insert",
    )?;
    capture(
        "entities",
        "entities",
        json!({ "query": "API_KEY", "response_bytes": 100_000 }),
        "API_KEY",
    )?;
    capture(
        "paths",
        "paths",
        json!({
            "from": start_anchor,
            "to": finish_anchor,
            "snapshot": snapshot,
            "direction": "out",
            "response_bytes": 100_000
        }),
        "finish",
    )?;
    capture(
        "repository_overview",
        "repository_overview",
        json!({
            "include_semantic": false,
            "reconnaissance_limit": 0,
            "response_bytes": 100_000
        }),
        "\"files\":2",
    )?;
    capture(
        "semantic_memory/discovery",
        "semantic_memory",
        json!({
            "query": "differential memory",
            "vector": false,
            "response_bytes": 100_000
        }),
        "annotation",
    )?;
    capture(
        "semantic_memory/body",
        "semantic_memory",
        json!({
            "artifact": 1,
            "view": "body",
            "response_bytes": 100_000
        }),
        "differential memory",
    )?;

    Ok((snapshot, surfaces, covered_tools))
}

fn seed_code_surface_memory(root: &Path, conn: &Connection) -> Result<()> {
    let snapshot = structural::current_snapshot(conn)?;
    let rendered = call_tool(
        root,
        conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "annotate",
        &json!({
            "type": "annotation",
            "body": { "claim": "differential memory" },
            "supports": [{
                "claim_path": "/claim",
                "anchor": "sym:src/service.ts#::finish@1",
                "evidence_file": "src/service.ts",
                "evidence_start_line": 1,
                "evidence_end_line": 1,
                "confidence": "likely"
            }],
            "confidence": "likely",
            "snapshot": snapshot
        }),
    )?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    assert_eq!(value["freshness"], "fresh");
    Ok(())
}

fn count_json_key(value: &serde_json::Value, key: &str) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.contains_key(key))
                + object
                    .values()
                    .map(|value| count_json_key(value, key))
                    .sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            values.iter().map(|value| count_json_key(value, key)).sum()
        }
        _ => 0,
    }
}

fn assert_code_read_surfaces_equal(
    expected: &BTreeMap<&'static str, String>,
    actual: &BTreeMap<&'static str, String>,
    arm: &str,
) {
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        actual.keys().collect::<Vec<_>>()
    );
    for (surface, expected) in expected {
        assert_eq!(
            actual.get(surface),
            Some(expected),
            "Markdown changed normalized MCP code surface `{surface}` in {arm}"
        );
    }
}

#[test]
fn documentation_search_tool_is_separate_and_works_without_a_provider() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("README.md"),
        "# Deployment\n\nUse the blue release channel.\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::refresh_repo_with_options(repo.path(), &conn, &indexer::IndexOptions::default())?;

    let defaults = config::DocsSettings {
        enabled: true,
        include: vec!["**/*.md".into()],
        exclude: Vec::new(),
        search: config::DocsSearchSettings {
            vector: false,
            rerank: false,
            freshness: false,
            max_rank_movement: 2,
            limit: 10,
            response_bytes: 24_000,
        },
    };
    let rendered = call_documentation_tool(
        repo.path(),
        &conn,
        None,
        &defaults,
        &json!({ "query": "blue release" }),
    )?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    let identities = crate::publication::Identities::read(&conn)?;
    assert_eq!(value["snapshot"], identities.documentation);
    assert_eq!(value["publication_snapshot"], identities.publication);
    assert_eq!(count_json_key(&value, "snapshot"), 1);
    assert_eq!(count_json_key(&value, "publication_snapshot"), 1);
    assert_eq!(value["hits"][0]["path"], "README.md");
    assert_eq!(value["hits"][0]["heading"], "Deployment");
    assert_eq!(value["hits"][0]["source_state"], "current");
    assert_eq!(value["hits"][0]["freshness_basis"], "unknown");
    assert!(value["hits"][0]["freshness_value"].is_null());
    assert_eq!(value["hits"][0]["base_rank"], 1);
    assert_eq!(value["hits"][0]["movement"], 0);
    assert_eq!(value["retrieval"]["vector"], "disabled");
    assert_eq!(value["retrieval"]["freshness"], "disabled");
    assert_eq!(value["retrieval"]["max_rank_movement"], 2);
    for private in ["snapshot", "file_hash", "source_start", "source_end"] {
        assert!(
            value["hits"][0].get(private).is_none(),
            "documentation hit repeated private field `{private}`: {value}"
        );
    }
    assert!(
        tool_defs(ToolProfile::Baseline, true)
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "documentation_search")
    );
    // Documentation routing lives in the skill; the instructions only point
    // at the skill file.
    let instructions = server_instructions(ToolProfile::Structural);
    assert!(instructions.contains(".agents/skills/jscout/SKILL.md"));
    assert!(!instructions.contains("documentation_search"));
    let outline = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "file_outline",
        &json!({ "path": "README.md" }),
    )?;
    assert!(!outline.contains("README.md"));
    assert!(!outline.contains("markdown_section"));

    fs::write(
        repo.path().join("README.md"),
        "# Deployment\n\nUse the red release channel.\n",
    )?;
    let stale = call_documentation_tool(
        repo.path(),
        &conn,
        None,
        &defaults,
        &json!({ "query": "blue release" }),
    )?;
    let stale: serde_json::Value = serde_json::from_str(&stale)?;
    assert_eq!(stale["hits"][0]["source_state"], "source_mismatch");
    assert_eq!(stale["hits"][0]["source_detail"], "hash_mismatch");
    assert_eq!(stale["hits"][0]["content"], "Use the blue release channel.");
    Ok(())
}

#[test]
fn documentation_search_applies_configured_freshness_at_the_mcp_boundary() -> Result<()> {
    if Command::new("git").arg("--version").output().is_err() {
        return Ok(());
    }
    let repo = tempfile::tempdir()?;
    let git = |args: &[&str]| -> Result<()> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&["init", "--quiet"])?;
    git(&["config", "user.name", "Documentation Test"])?;
    git(&["config", "user.email", "docs@example.invalid"])?;
    fs::write(
        repo.path().join("a-obsolete.md"),
        "# Guidance\n\nShared header recommendation.\n",
    )?;
    git(&["add", "a-obsolete.md"])?;
    let old_commit = Command::new("git")
        .args(["commit", "--quiet", "-m", "obsolete"])
        .current_dir(repo.path())
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00+00:00")
        .output()?;
    anyhow::ensure!(old_commit.status.success(), "initial Git commit failed");
    fs::write(
        repo.path().join("z-current.md"),
        "# Guidance\n\nShared header recommendation.\n",
    )?;
    git(&["add", "z-current.md"])?;
    let new_commit = Command::new("git")
        .args(["commit", "--quiet", "-m", "current"])
        .current_dir(repo.path())
        .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00+00:00")
        .output()?;
    anyhow::ensure!(new_commit.status.success(), "second Git commit failed");

    let conn = store::open(repo.path())?;
    indexer::refresh_repo_with_options(
        repo.path(),
        &conn,
        &indexer::IndexOptions {
            docs_freshness: true,
            ..indexer::IndexOptions::default()
        },
    )?;
    let defaults = config::DocsSettings {
        enabled: true,
        include: vec!["**/*.md".into()],
        exclude: Vec::new(),
        search: config::DocsSearchSettings {
            vector: false,
            rerank: false,
            freshness: true,
            max_rank_movement: 1,
            limit: 10,
            response_bytes: 24_000,
        },
    };
    let rendered = call_documentation_tool(
        repo.path(),
        &conn,
        None,
        &defaults,
        &json!({ "query": "shared header recommendation" }),
    )?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    assert_eq!(value["retrieval"]["freshness"], "active");
    assert_eq!(value["retrieval"]["max_rank_movement"], 1);
    assert_eq!(value["hits"][0]["path"], "z-current.md");
    assert_eq!(value["hits"][0]["freshness_basis"], "git");
    assert_eq!(value["hits"][0]["base_rank"], 2);
    assert_eq!(value["hits"][0]["movement"], 1);
    assert_eq!(value["hits"][1]["path"], "a-obsolete.md");
    assert_eq!(value["hits"][1]["base_rank"], 1);
    assert_eq!(value["hits"][1]["movement"], -1);
    Ok(())
}

#[test]
fn disabled_documentation_is_absent_and_rejected_at_the_mcp_boundary() -> Result<()> {
    for profile in [ToolProfile::Baseline, ToolProfile::Structural] {
        let tools = tool_defs(profile, false);
        assert!(
            tools
                .as_array()
                .expect("tool definitions")
                .iter()
                .all(|tool| tool["name"] != "documentation_search")
        );
        let instructions = server_instructions(profile);
        assert!(!instructions.contains("documentation_search"));
        assert!(instructions.contains("plane digest as snapshot"));
        assert!(instructions.contains("publication_snapshot"));
    }

    let repo = tempfile::tempdir()?;
    let conn = Connection::open_in_memory()?;
    let error = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "documentation_search",
        &json!({ "query": "stale guidance" }),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unavailable without documentation retrieval configuration")
    );
    Ok(())
}

#[test]
fn documentation_admission_is_inert_across_mcp_code_read_surfaces() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    fs::create_dir(repo.path().join("src"))?;
    fs::write(
        repo.path().join("src/service.ts"),
        "export function finish() { return process.env.API_KEY; }\n\
         export function start(db: any, bus: any) {\n\
           bus.emit('ready');\n\
           bus.on('ready', finish);\n\
           db.items.insert({ merge: 'replace' });\n\
           return finish();\n\
         }\n",
    )?;
    fs::write(
        repo.path().join("src/main.ts"),
        "import { start } from './service';\n\
         export function boot(db: any, bus: any) { return start(db, bus); }\n",
    )?;

    let incremental = store::open_path(&state.path().join("incremental.db"))?;
    indexer::index_repo(repo.path(), &incremental)?;
    seed_code_surface_memory(repo.path(), &incremental)?;
    let (code_snapshot, code_surfaces, covered_tools) =
        capture_code_read_surfaces(repo.path(), &incremental)?;
    let code_publication = crate::publication::current_publication_snapshot(&incremental)?;

    fs::write(
        repo.path().join("README.md"),
        format!(
            "# finish API_KEY ready insert\n\n{}\n",
            "`finish` calls `start`; `process.env.API_KEY`; \
             `bus.emit('ready')`; `db.items.insert({ merge: 'replace' })`.\n\n"
                .repeat(100)
        ),
    )?;
    fs::write(
        repo.path().join("guide.mdx"),
        "# start finish API_KEY ready insert\n\n\
         import { start } from './src/service';\n\n\
         export const Guidance = () => <code>finish API_KEY ready insert</code>;\n",
    )?;
    indexer::index_repo(repo.path(), &incremental)?;
    let (incremental_snapshot, incremental_surfaces, _) =
        capture_code_read_surfaces(repo.path(), &incremental)?;
    let documentation_publication = crate::publication::current_publication_snapshot(&incremental)?;
    assert_eq!(code_snapshot, incremental_snapshot);
    assert_ne!(code_publication, documentation_publication);
    assert_code_read_surfaces_equal(
        &code_surfaces,
        &incremental_surfaces,
        "incremental docs admission",
    );

    let fresh = store::open_path(&state.path().join("fresh.db"))?;
    indexer::index_repo(repo.path(), &fresh)?;
    seed_code_surface_memory(repo.path(), &fresh)?;
    let (fresh_snapshot, fresh_surfaces, _) = capture_code_read_surfaces(repo.path(), &fresh)?;
    assert_eq!(incremental_snapshot, fresh_snapshot);
    assert_code_read_surfaces_equal(&code_surfaces, &fresh_surfaces, "fresh docs index");

    let structural_tools = tool_defs(ToolProfile::Structural, true);
    let defined_tools = structural_tools
        .as_array()
        .expect("structural tool definitions")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<BTreeSet<_>>();
    let deliberately_excluded = BTreeSet::from(["annotate", "documentation_search"]);
    let accounted_tools = covered_tools
        .union(&deliberately_excluded)
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        accounted_tools, defined_tools,
        "every structural MCP tool must be probed or deliberately excluded"
    );
    Ok(())
}

#[test]
fn poisoned_rust_facts_do_not_cross_structural_mcp_boundaries() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir(repo.path().join("src"))?;
    fs::write(
        repo.path().join("src/service.ts"),
        "export function sharedNeedle(db: any, bus: any) {\n\
           bus.emit('shared-event');\n\
           db.items.sharedCall({ marker: true });\n\
           return sharedNeedle(db, bus);\n\
         }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let anchor: String = conn.query_row(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='symbol' AND display_name='sharedNeedle'",
        [],
        |row| row.get(0),
    )?;
    let typescript_file: i64 = conn.query_row(
        "SELECT id FROM files WHERE path='src/service.ts'",
        [],
        |row| row.get(0),
    )?;

    let rust_source = "db.items.sharedCall({ marker: true });\n";
    fs::write(repo.path().join("poison.rs"), rust_source)?;
    conn.execute(
        "INSERT INTO files(path,hash,corpus,format,role,origin)
         VALUES('poison.rs',?1,'code','rust','production','repository')",
        [blake3::hash(rust_source.as_bytes()).to_hex().to_string()],
    )?;
    let rust_file = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO chunks(
           file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(?1,'method','sharedNeedle','','sharedNeedle',0,39,1,1,
                  'rust-poison-chunk',?2)",
        rusqlite::params![rust_file, rust_source],
    )?;
    let rust_chunk = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO symbols(
           file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
         ) VALUES(?1,'sharedNeedle','function',0,12,0,39,'',1,1)",
        [rust_file],
    )?;
    conn.execute(
        "INSERT INTO refs(
           file_id,chunk_id,start,line,kind,confidence,target_request,target_name,local
         ) VALUES(?1,?2,0,91,'call','certain','./src/service','sharedNeedle',0)",
        rusqlite::params![rust_file, rust_chunk],
    )?;
    conn.execute(
        "INSERT INTO module_edges(from_file,request,to_file,resolution)
         VALUES(?1,'./src/service',?2,'resolver')",
        rusqlite::params![rust_file, typescript_file],
    )?;
    conn.execute(
        "INSERT INTO events(file_id,chunk_id,line,role,name,method)
         VALUES(?1,?2,92,'listen','shared-event','on')",
        rusqlite::params![rust_file, rust_chunk],
    )?;
    conn.execute(
        "INSERT INTO member_calls(file_id,chunk_id,start,end,line,prop,object)
         VALUES(?1,?2,0,39,93,'sharedCall','items')",
        rusqlite::params![rust_file, rust_chunk],
    )?;

    for (tool, arguments) in [
        (
            "definition",
            json!({
                "symbol": "sharedNeedle",
                "origins": ["repository"],
                "response_bytes": 100_000
            }),
        ),
        (
            "who_uses",
            json!({
                "anchor": anchor,
                "snapshot": snapshot,
                "origins": ["repository"],
                "response_bytes": 100_000
            }),
        ),
        (
            "events",
            json!({
                "name": "shared-event",
                "origins": ["repository"],
                "response_bytes": 100_000
            }),
        ),
        (
            "calls",
            json!({
                "method": "sharedCall",
                "origins": ["repository"],
                "response_bytes": 100_000
            }),
        ),
    ] {
        let rendered = call_tool(
            repo.path(),
            &conn,
            None,
            ToolProfile::Structural,
            SourceView::Full,
            tool,
            &arguments,
        )?;
        assert!(
            rendered.contains("src/service.ts"),
            "{tool} did not exercise the eligible TypeScript control: {rendered}"
        );
        assert!(
            !rendered.contains("poison.rs"),
            "{tool} leaked a checker-ineligible Rust fact: {rendered}"
        );
    }
    Ok(())
}

#[test]
fn rust_semantic_search_expand_stays_lexical_at_mcp_boundary() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("native.rs"),
        "pub fn lexical_native_marker() { println!(\"native marker\"); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rendered = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "lexical native marker",
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "expand": true,
            "expand_mode": "paths",
            "limit": 10,
            "response_bytes": 100_000
        }),
    )?;
    let response: serde_json::Value = serde_json::from_str(&rendered)?;
    let hit = response["hits"]
        .as_array()
        .and_then(|hits| hits.iter().find(|hit| hit["at"] == "native.rs:1"))
        .unwrap_or_else(|| panic!("missing compact Rust hit: {rendered}"));

    assert!(hit.get("anchor").is_none());
    assert!(hit.get("followups").is_none());

    assert_eq!(response["graph"]["projection"], "paths");
    assert_eq!(response["graph"]["seeds"], json!([]));
    assert_eq!(response["graph"]["nodes"], json!({}));
    assert_eq!(response["graph"]["edges"], json!([]));

    let tools = tool_defs(ToolProfile::Structural, true);
    let outline_tool = tools
        .as_array()
        .expect("tool definitions")
        .iter()
        .find(|tool| tool["name"] == "file_outline")
        .expect("file_outline definition");
    assert!(
        outline_tool["description"].as_str().is_some_and(
            |description| description.contains("span-only line ranges with null names")
        )
    );
    let outline = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "file_outline",
        &json!({ "path": "native.rs" }),
    )?;
    let outline: serde_json::Value = serde_json::from_str(&outline)?;
    let ranges = outline["outline"].as_array().expect("Rust outline ranges");
    assert!(!ranges.is_empty());
    assert!(ranges.iter().all(|range| range["name"].is_null()));
    assert!(ranges.iter().all(|range| range["lines"].is_array()));
    Ok(())
}

#[test]
fn semantic_search_formats_scope_is_enforced_and_echoed_at_mcp_boundary() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("web.ts"),
        "export const crossFormatMcpMarker = true;\n",
    )?;
    fs::write(
        repo.path().join("native.rs"),
        "pub const crossFormatMcpMarker: bool = true;\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rendered = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "crossFormatMcpMarker",
            "formats": ["rust"],
            "exhaustive": true,
            "limit": 10,
            "response_bytes": 100_000
        }),
    )?;
    let response: serde_json::Value = serde_json::from_str(&rendered)?;
    assert_eq!(response["scope"]["formats"], json!(["rust"]));
    let hits = response["hits"].as_array().expect("compact hits");
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| {
        hit["at"]
            .as_str()
            .is_some_and(|location| location.starts_with("native.rs:"))
    }));

    let error = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Baseline,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "crossFormatMcpMarker",
            "formats": ["markdown"],
            "vector": false,
            "rerank": false
        }),
    )
    .expect_err("documentation formats must not enter code search");
    assert!(error.to_string().contains("code format must be one of"));

    for (formats, expected) in [
        (json!("rust"), "`formats` must be an array of strings"),
        (json!([]), "`formats` must contain at least one value"),
        (json!([null]), "`formats[0]` must be a string"),
        (json!(["rust", 1]), "`formats[1]` must be a string"),
    ] {
        let error = call_tool(
            repo.path(),
            &conn,
            None,
            ToolProfile::Baseline,
            SourceView::Full,
            "semantic_search",
            &json!({
                "query": "crossFormatMcpMarker",
                "formats": formats,
                "vector": false,
                "rerank": false
            }),
        )
        .expect_err("malformed formats must fail closed");
        assert_eq!(error.to_string(), expected);
    }
    Ok(())
}

#[test]
fn scoped_search_anchors_preserve_formats_through_definition_and_who_uses() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("target.ts"),
        "export function scopedFormatTarget() { return 'SCOPED_FORMAT_TARGET'; }\n",
    )?;
    fs::write(
        repo.path().join("ts_caller.ts"),
        "import { scopedFormatTarget } from './target';\nexport function callFromTs() { return scopedFormatTarget(); }\n",
    )?;
    fs::write(
        repo.path().join("js_caller.js"),
        "import { scopedFormatTarget } from './target';\nexport function callFromJs() { return scopedFormatTarget(); }\n",
    )?;
    fs::write(
        repo.path().join("same_name.js"),
        "export function scopedFormatTarget() { return 'JAVASCRIPT_SAME_NAME'; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rendered = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "semantic_search",
        &json!({
            "query": "SCOPED_FORMAT_TARGET",
            "formats": ["typescript"],
            "vector": false,
            "rerank": false,
            "include_memory": false,
            "limit": 3
        }),
    )?;
    let search: serde_json::Value = serde_json::from_str(&rendered)?;
    let hit = search["hits"]
        .as_array()
        .and_then(|hits| {
            hits.iter().find(|hit| {
                hit["at"]
                    .as_str()
                    .is_some_and(|location| location.starts_with("target.ts:"))
            })
        })
        .unwrap_or_else(|| panic!("TypeScript target hit: {search}"));
    assert_eq!(hit["used_by"], json!(["scopedFormatTarget: 1 sites"]));
    assert!(hit.get("followups").is_none());
    let definition_arguments = json!({
        "anchor": hit["anchor"],
        "snapshot": search["snapshot"],
        "formats": ["typescript"]
    });
    let who_uses_arguments = definition_arguments.clone();
    let neighborhood_arguments = json!({
        "anchor": hit["anchor"],
        "snapshot": search["snapshot"]
    });

    let definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &definition_arguments,
    )?;
    let definition: serde_json::Value = serde_json::from_str(&definition)?;
    assert_eq!(definition["definitions"][0]["target"]["at"], "target.ts:1");

    let usages = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "who_uses",
        &who_uses_arguments,
    )?;
    assert!(usages.contains("ts_caller.ts"));
    assert!(!usages.contains("js_caller.js"));

    let neighborhood = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "neighborhood",
        &neighborhood_arguments,
    )?;
    let neighborhood: serde_json::Value = serde_json::from_str(&neighborhood)?;
    assert_eq!(neighborhood["anchor"], hit["anchor"]);

    let fuzzy_scoped = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &json!({ "symbol": "scopedFormatTarget", "formats": ["typescript"] }),
    )?;
    let fuzzy_scoped: serde_json::Value = serde_json::from_str(&fuzzy_scoped)?;
    assert_eq!(fuzzy_scoped["response"]["matched_targets"], 1);
    assert_eq!(
        fuzzy_scoped["definitions"][0]["target"]["at"],
        "target.ts:1"
    );

    let fuzzy_unscoped = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &json!({ "symbol": "scopedFormatTarget" }),
    )?;
    let fuzzy_unscoped: serde_json::Value = serde_json::from_str(&fuzzy_unscoped)?;
    assert_eq!(fuzzy_unscoped["response"]["matched_targets"], 2);
    Ok(())
}

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
    assert!(options.formats.is_empty());
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
        limit: search::MAX_EXHAUSTIVE_PAGE_SIZE + 11,
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
    assert_eq!(options.limit, search::MAX_EXHAUSTIVE_PAGE_SIZE);
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

    let (_, explicit_oversized) = search_options_from_args(
        ToolProfile::Structural,
        &json!({
            "query": "dispatch",
            "exhaustive": true,
            "limit": search::MAX_EXHAUSTIVE_PAGE_SIZE + 1
        }),
        &defaults,
    )?;
    assert_eq!(
        explicit_oversized.limit,
        search::MAX_EXHAUSTIVE_PAGE_SIZE + 1
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
fn baseline_ranked_search_forces_unavailable_configured_stages_off() -> Result<()> {
    let defaults = config::SearchSettings {
        vector: false,
        attach_memory: true,
        expansion: config::ExpansionSettings {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let (vector, options) = search_options_from_args(
        ToolProfile::Baseline,
        &json!({ "query": "dispatch", "vector": true }),
        &defaults,
    )?;
    assert!(vector);
    assert!(!options.expand);
    assert!(!options.include_memory);

    let (_, explicit_disable) = search_options_from_args(
        ToolProfile::Baseline,
        &json!({ "query": "dispatch", "expand": false }),
        &defaults,
    )?;
    assert!(!explicit_disable.expand);

    let explicit_enable = search_options_from_args(
        ToolProfile::Baseline,
        &json!({ "query": "dispatch", "expand": true }),
        &defaults,
    )
    .expect_err("baseline must still reject an explicitly enabled structural stage");
    assert!(explicit_enable.to_string().contains("unavailable"));

    let (_, structural) = search_options_from_args(
        ToolProfile::Structural,
        &json!({ "query": "dispatch" }),
        &defaults,
    )?;
    assert!(structural.expand);
    assert!(structural.include_memory);
    Ok(())
}

#[test]
fn profile_instructions_are_identity_pointer_and_mechanical_contracts() {
    let baseline = server_instructions(ToolProfile::Baseline);
    let structural = server_instructions(ToolProfile::Structural);

    for instructions in [&baseline, &structural] {
        for marker in [
            "jscout agent-guide --tier",
            "plane digest as snapshot",
            "canonical indexed publication identity as publication_snapshot",
            "next_cursor",
            "truncated=false",
            "response_budget_too_small",
            "minimum_bytes=N",
            "response_bytes=N",
        ] {
            assert!(
                instructions.contains(marker),
                "missing server-instruction contract: {marker}"
            );
        }
        // G28: routing rules, documentation routing included, live in the
        // skill; the instructions point at the skill file instead.
        assert!(instructions.contains(".agents/skills/jscout/SKILL.md"));
        for routing in [
            "Investigation loop",
            "Inquiry loop",
            "localize first",
            "broad_or_query",
            "repository_overview",
            "semantic_memory",
            "documentation_search",
            "convention",
        ] {
            assert!(
                !instructions.contains(routing),
                "server instructions restate a skill routing rule: {routing}"
            );
        }
        assert!(
            instructions.len() < 1_000,
            "server instructions grew past identity, pointer, and contracts: {} bytes",
            instructions.len()
        );
    }
    assert!(baseline.contains("--tier core"));
    assert!(structural.contains("--tier full"));

    for profile in [ToolProfile::Baseline, ToolProfile::Structural] {
        let tools = tool_defs(profile, true);
        let search = tools
            .as_array()
            .expect("tool definitions")
            .iter()
            .find(|tool| tool["name"] == "semantic_search")
            .expect("search definition");
        let origins = &search["inputSchema"]["properties"]["origins"];
        assert!(origins.get("default").is_none());
        let formats = &search["inputSchema"]["properties"]["formats"];
        assert!(formats.get("default").is_none());
        assert_eq!(
            formats["items"]["enum"],
            json!(["javascript", "typescript", "rust"])
        );
        assert_eq!(formats["minItems"], 1);
        assert!(
            formats["description"]
                .as_str()
                .is_some_and(|description| description.contains("all registered code formats"))
        );
        for tool_name in ["definition", "who_uses"] {
            let tool = tools
                .as_array()
                .expect("tool definitions")
                .iter()
                .find(|tool| tool["name"] == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} definition"));
            let formats = &tool["inputSchema"]["properties"]["formats"];
            assert!(formats.get("default").is_none());
            assert_eq!(formats["minItems"], 1);
        }
        for tool in tools.as_array().expect("tool definitions") {
            let description = tool["description"].as_str().expect("description");
            assert!(
                description.len() < 200,
                "{} description is not a one-liner: {} bytes",
                tool["name"],
                description.len()
            );
            assert!(
                !description.contains("repository configuration"),
                "{} restates configuration prose",
                tool["name"]
            );
        }
    }
}

#[test]
fn default_surface_costs_under_a_third_of_the_pre_g28_structural_surface() {
    // The pre-G28 structural profile with documentation cost 26,828 bytes of
    // tool definitions plus instructions; G28 accepts the default (core)
    // surface only under a third of that.
    const PRE_G28_STRUCTURAL_SURFACE: usize = 26_828;
    let surface = |profile: ToolProfile| -> usize {
        serde_json::to_string(&tool_defs(profile, true))
            .expect("serialize tool definitions")
            .len()
            + server_instructions(profile).len()
    };
    let core = surface(ToolProfile::Baseline);
    assert!(
        core * 3 < PRE_G28_STRUCTURAL_SURFACE,
        "core surface is {core} bytes; the G28 gate is under {}",
        PRE_G28_STRUCTURAL_SURFACE / 3
    );
    let full = surface(ToolProfile::Structural);
    println!("surface bytes: core={core} full={full}");
    // The full profile keeps every schema; its gate is only "no regrowth".
    assert!(
        full < 20_000,
        "full surface is {full} bytes; the schema-only full profile should stay under 20 KB"
    );
}

#[test]
fn trimmed_tools_are_refused_at_the_boundary_before_any_access_decision() -> Result<()> {
    let allow = ["definition".to_string()];
    assert!(tool_registered(&allow, "definition"));
    assert!(!tool_registered(&allow, "annotate"));
    assert!(tool_registered(&[], "annotate"));
    // The write-capable tool is refused by registration alone; an in-memory
    // connection with no schema proves no tool logic (or writer) was reached.
    let conn = Connection::open_in_memory().expect("in-memory database");
    for (profile, name, args) in [
        (
            ToolProfile::Structural,
            "annotate",
            json!({ "type": "workflow" }),
        ),
        (
            ToolProfile::Structural,
            "semantic_memory",
            json!({ "query": "x" }),
        ),
        (
            ToolProfile::Baseline,
            "file_outline",
            json!({ "path": "x.ts" }),
        ),
    ] {
        let error = call_tool_with_allowlist(
            Path::new("."),
            &conn,
            None,
            profile,
            SourceView::Full,
            &allow,
            name,
            &args,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(&format!("tool `{name}` is not enabled by [mcp].tools")),
            "{name}: {error}"
        );
    }
    // The production preflight is the same plan: trimmed tools are refused
    // before any access decision, registered ones classify normally.
    let error = plan_tool_call(&allow, ToolProfile::Structural, "annotate")
        .unwrap_err()
        .to_string();
    assert!(error.contains("tool `annotate` is not enabled by [mcp].tools"));
    assert_eq!(
        plan_tool_call(&[], ToolProfile::Structural, "annotate")?,
        ToolAccess::Write
    );
    assert_eq!(
        plan_tool_call(&allow, ToolProfile::Baseline, "definition")?,
        ToolAccess::Read
    );
    Ok(())
}

#[test]
fn allowlists_that_register_nothing_are_refused_at_server_start() {
    let only_annotate = ["annotate".to_string()];
    let error = ensure_effective_surface(ToolProfile::Baseline, true, &only_annotate)
        .unwrap_err()
        .to_string();
    assert!(error.contains("registers no tool under profile"), "{error}");
    ensure_effective_surface(ToolProfile::Structural, true, &only_annotate)
        .expect("annotate is registered by the full profile");
    let only_docs = ["documentation_search".to_string()];
    let error = ensure_effective_surface(ToolProfile::Baseline, false, &only_docs)
        .unwrap_err()
        .to_string();
    assert!(error.contains("with documentation disabled"), "{error}");
    ensure_effective_surface(ToolProfile::Baseline, true, &only_docs)
        .expect("documentation_search is registered when documentation is enabled");
    ensure_effective_surface(ToolProfile::Baseline, false, &[])
        .expect("an omitted allowlist registers the whole profile");
}

#[test]
fn tool_allowlist_narrows_registration_and_rejects_calls() -> Result<()> {
    let allow = ["definition".to_string(), "who_uses".to_string()];
    let names = |value: serde_json::Value| -> Vec<String> {
        value
            .as_array()
            .expect("tool definitions")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_string())
            .collect()
    };
    // Registration order follows the definition order, not the allowlist.
    assert_eq!(
        names(allowed_tool_defs(ToolProfile::Structural, true, &allow)),
        ["who_uses", "definition"]
    );
    // The allowlist only narrows: a structural-only tool stays absent in baseline.
    let structural_only = ["neighborhood".to_string(), "definition".to_string()];
    assert_eq!(
        names(allowed_tool_defs(
            ToolProfile::Baseline,
            true,
            &structural_only
        )),
        ["definition"]
    );
    assert!(
        allowed_tool_defs(ToolProfile::Structural, true, &[])
            .as_array()
            .unwrap()
            .len()
            == 13
    );
    assert!(
        validate_tool_names(&["definitions".to_string()])
            .unwrap_err()
            .to_string()
            .contains("unknown tool `definitions`")
    );
    Ok(())
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
    let structural = tool_defs(ToolProfile::Structural, true);
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
    let structural = tool_defs(ToolProfile::Structural, true);
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
    let structural = tool_defs(ToolProfile::Structural, true);
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
    let baseline = tool_defs(ToolProfile::Baseline, true);
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

    let structural = tool_defs(ToolProfile::Structural, true);
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
    assert!(debug.is_object());
    assert!(debug["definitions"][0]["source"].is_string());
    assert!(debug["definitions"][0]["source_meta"].get("text").is_none());
    assert!(
        debug["definitions"][0]["source_meta"]
            .get("byte_limit")
            .is_none()
    );
    assert_eq!(count_json_key(&debug, "snapshot"), 1);
    assert_eq!(count_json_key(&debug, "publication_snapshot"), 1);
    Ok(())
}

#[test]
fn search_anchors_round_trip_exact_same_named_methods() -> Result<()> {
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
    assert!(hit.get("followups").is_none());
    let class_arguments = json!({
        "anchor": hit["anchor"],
        "snapshot": search["snapshot"]
    });

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
    assert!(baseline_search["hits"][0].get("followups").is_none());

    let definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &class_arguments,
    )?;
    let definition_value: serde_json::Value = serde_json::from_str(&definition)?;
    assert!(definition_value.get("resolution").is_none());
    assert_eq!(definition_value["snapshot"], search["snapshot"]);
    assert_eq!(
        definition_value["publication_snapshot"],
        search["publication_snapshot"]
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

    let re_resolved_definition = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &json!({
            "anchor": second_class_anchor,
            "snapshot": search["publication_snapshot"]
        }),
    )?;
    let re_resolved_value: serde_json::Value = serde_json::from_str(&re_resolved_definition)?;
    assert_eq!(
        re_resolved_value["resolution"]["requested_anchor"],
        second_class_anchor
    );
    assert_eq!(
        re_resolved_value["resolution"]["resolved_anchor"],
        second_class_anchor
    );
    assert_eq!(
        re_resolved_value["resolution"]["anchor_status"],
        "re-resolved"
    );
    assert_eq!(
        re_resolved_value["response"]["rendered_bytes"],
        re_resolved_definition.len()
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
    assert!(method_definition.get("resolution").is_none());
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
    assert!(usages.get("resolution").is_none());
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

    let debug_re_resolved = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "who_uses",
        &json!({
            "anchor": second_anchor,
            "snapshot": search["publication_snapshot"],
            "origins": ["repository"],
            "debug": true
        }),
    )?;
    let debug_re_resolved: serde_json::Value = serde_json::from_str(&debug_re_resolved)?;
    assert_eq!(
        debug_re_resolved["resolution"]["requested_anchor"],
        second_anchor
    );
    assert_eq!(
        debug_re_resolved["resolution"]["resolved_anchor"],
        second_anchor
    );
    assert_eq!(
        debug_re_resolved["resolution"]["anchor_status"],
        "re-resolved"
    );

    let neighborhood = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "neighborhood",
        &class_arguments,
    )?;
    let neighborhood: serde_json::Value = serde_json::from_str(&neighborhood)?;
    assert_eq!(neighborhood["anchor"], second_class_anchor);
    assert!(neighborhood.get("anchor_status").is_none());

    let re_resolved_neighborhood = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "neighborhood",
        &json!({
            "anchor": second_class_anchor,
            "snapshot": search["publication_snapshot"]
        }),
    )?;
    let re_resolved_neighborhood: serde_json::Value =
        serde_json::from_str(&re_resolved_neighborhood)?;
    assert_eq!(
        re_resolved_neighborhood["requested_anchor"],
        second_class_anchor
    );
    assert_eq!(re_resolved_neighborhood["anchor_status"], "re-resolved");

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

    conn.execute(
        "INSERT INTO symbols(
           file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
         )
         SELECT symbol.file_id,symbol.name,symbol.kind,symbol.start,symbol.end,
                symbol.decl_start,symbol.decl_end,symbol.scope_chain,symbol.line,
                symbol.exported
         FROM symbols symbol JOIN graph_nodes node ON node.native_id=symbol.id
         WHERE node.native_table='symbols' AND node.node_key=?1",
        [&second_anchor],
    )?;
    let overload_id = conn.last_insert_rowid();
    let (anchor_prefix, _) = second_anchor
        .rsplit_once('@')
        .expect("canonical symbol anchor has an ordinal");
    let overload_anchor = format!("{anchor_prefix}@2");
    conn.execute(
        "INSERT INTO graph_nodes(
           node_key,node_kind,native_table,native_id,display_name,file_id,line,meta_json
         )
         SELECT ?1,'symbol','symbols',symbol.id,symbol.name,symbol.file_id,
                symbol.line,'{}'
         FROM symbols symbol WHERE symbol.id=?2",
        rusqlite::params![overload_anchor, overload_id],
    )?;

    call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &method_arguments,
    )?;
    let stale_overload = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "definition",
        &json!({
            "anchor": second_anchor,
            "snapshot": search["publication_snapshot"],
            "origins": ["repository"]
        }),
    )
    .unwrap_err()
    .to_string();
    assert!(stale_overload.contains("stale anchor"));
    assert!(stale_overload.contains("ambiguous after re-resolution"));
    assert!(stale_overload.contains("current response's `snapshot`"));
    Ok(())
}

#[test]
fn search_anchors_preserve_cross_origin_definitions_and_callers() -> Result<()> {
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
        assert_eq!(search["hits"][0]["symbol"], query);
        assert!(search["hits"][0].get("followups").is_none());
        let arguments = json!({
            "anchor": search["hits"][0]["anchor"],
            "snapshot": search["snapshot"]
        });
        assert!(arguments.get("origins").is_none());
        let usages = call_tool(
            repo.path(),
            &conn,
            None,
            ToolProfile::Structural,
            SourceView::Full,
            "who_uses",
            &arguments,
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
    assert_eq!(dependency_search["hits"][0]["symbol"], "dependencyTarget");
    assert!(dependency_search["hits"][0].get("followups").is_none());
    let arguments = json!({
        "anchor": dependency_search["hits"][0]["anchor"],
        "snapshot": dependency_search["snapshot"],
        "origins": ["repository", "workspace", "dependency"]
    });
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
        &arguments,
    )?;
    assert!(definition.contains("dependency-target"));
    let usages = call_tool(
        repo.path(),
        &conn,
        None,
        ToolProfile::Structural,
        SourceView::Full,
        "who_uses",
        &arguments,
    )?;
    assert!(usages.contains("dependency-caller.ts"));
    Ok(())
}

#[test]
fn bounded_item_envelope_accounts_for_json_overhead() -> Result<()> {
    let items = (0..100)
        .map(|index| json!({ "index": index, "text": "x".repeat(80) }))
        .collect();
    let rendered = render_bounded_items(
        "outline",
        items,
        &crate::publication::ResponseIdentity {
            snapshot: "code".into(),
            publication_snapshot: "publication".into(),
        },
        1_500,
    )?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    assert!(rendered.len() <= 1_500);
    assert_eq!(value["response_budget"]["rendered_bytes"], rendered.len());
    assert_eq!(value["response_budget"]["truncated"], true);
    assert_eq!(value["snapshot"], "code");
    assert_eq!(value["publication_snapshot"], "publication");
    assert!(value["response_budget"].get("byte_limit").is_none());
    assert!(value["outline"].as_array().unwrap().len() < 100);

    let complete = render_bounded_items(
        "outline",
        vec![json!({ "index": 1, "text": "short" })],
        &crate::publication::ResponseIdentity {
            snapshot: "code".into(),
            publication_snapshot: "publication".into(),
        },
        usize::MAX,
    )?;
    let complete_value: serde_json::Value = serde_json::from_str(&complete)?;
    assert_eq!(
        complete_value["response_budget"]["rendered_bytes"],
        complete.len()
    );
    assert_eq!(
        complete_value["response_budget"]["unbudgeted_bytes"],
        complete.len()
    );
    Ok(())
}

fn linearly_shed_bounded_object_array_states(
    mut response: serde_json::Value,
    fields: &[&str],
) -> Result<Vec<String>> {
    let original_items: usize = fields
        .iter()
        .map(|field| response[*field].as_array().map_or(0, Vec::len))
        .sum();
    response["response_budget"] = json!({
        "rendered_bytes": 0,
        "unbudgeted_bytes": 0,
        "truncated": false,
        "omitted_items": 0,
    });
    settle_unbudgeted_response(&mut response)?;
    let mut states = Vec::with_capacity(original_items + 1);
    loop {
        settle_value_rendered_bytes(&mut response)?;
        states.push(serde_json::to_string(&response)?);
        let removed = fields.iter().any(|field| {
            response[*field]
                .as_array_mut()
                .is_some_and(|items| items.pop().is_some())
        });
        if !removed {
            break;
        }
        response["response_budget"]["truncated"] = json!(true);
        if response.get("truncated").is_some() {
            response["truncated"] = json!(true);
        }
        let remaining: usize = fields
            .iter()
            .map(|field| response[*field].as_array().map_or(0, Vec::len))
            .sum();
        response["response_budget"]["omitted_items"] = json!(original_items - remaining);
    }
    Ok(states)
}

#[test]
fn bounded_array_prefix_search_matches_linear_shedding() -> Result<()> {
    let response = json!({
        "snapshot": "code",
        "publication_snapshot": "publication",
        "truncated": false,
        "first": (0..73).map(|index| match index % 3 {
            0 => json!(index),
            1 => json!("a".repeat(index % 7)),
            _ => json!({ "index": index, "text": "b".repeat(index % 5) }),
        }).collect::<Vec<_>>(),
        "second": (0..39).map(|index| json!({
            "index": index,
            "text": "c".repeat(index % 11),
        })).collect::<Vec<_>>(),
    });
    let full = render_bounded_object_arrays(response.clone(), &["first", "second"], usize::MAX)?;
    let linear_states =
        linearly_shed_bounded_object_array_states(response.clone(), &["first", "second"])?;
    assert_eq!(linear_states.first(), Some(&full));
    let minimum = linear_states
        .last()
        .expect("linear shedding always records the empty envelope")
        .len();
    for limit in 1..=full.len() + 1 {
        let expected = linear_states
            .iter()
            .find(|rendered| rendered.len() <= limit)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "response byte limit {limit} is below the minimum response envelope ({minimum} bytes)"
                )
            });
        let actual = render_bounded_object_arrays(response.clone(), &["first", "second"], limit);
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "limit={limit}"),
            (Err(expected), Err(actual)) => {
                assert_eq!(actual.to_string(), expected.to_string(), "limit={limit}");
            }
            (expected, actual) => {
                panic!("budget result differed at {limit}: expected={expected:?} actual={actual:?}")
            }
        }
    }
    Ok(())
}

#[test]
fn tool_access_classifies_only_structural_annotate_as_a_write() {
    assert_eq!(
        tool_access(ToolProfile::Structural, "annotate"),
        ToolAccess::Write
    );
    assert_eq!(
        tool_access(ToolProfile::Structural, "search"),
        ToolAccess::Read
    );
    assert_eq!(
        tool_access(ToolProfile::Baseline, "annotate"),
        ToolAccess::Read
    );
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
fn failed_tool_telemetry_does_not_guess_a_snapshot() {
    let failed = Err(anyhow::anyhow!("retrieval failed"));
    let successful = Ok(json!({ "snapshot": "observed" }).to_string());

    assert_eq!(telemetry_snapshot(&failed), None);
    assert_eq!(telemetry_snapshot(&successful).as_deref(), Some("observed"));
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
fn telemetry_records_exhaustive_counts_and_warnings_without_misclassifying_ranked_results() {
    let warning = json!({
        "code": "broad_or_query",
        "terms": ["history", "cache"],
        "total_chunks": 1496,
        "message": "refine"
    });
    let metrics = exhaustive_telemetry_metrics(Some(&json!({
        "total_chunks": 1496,
        "returned": 200,
        "truncated": true,
        "warnings": [warning.clone()]
    })));
    assert_eq!(metrics.total_chunks, Some(1496));
    assert_eq!(metrics.returned, Some(200));
    assert_eq!(metrics.truncated, Some(true));
    assert_eq!(metrics.warnings, Some(vec![warning]));

    let no_warning = exhaustive_telemetry_metrics(Some(&json!({
        "total_chunks": 4,
        "returned": 4,
        "truncated": false
    })));
    assert_eq!(no_warning.warnings, Some(Vec::new()));

    assert_eq!(
        exhaustive_telemetry_metrics(Some(&json!({ "hits": [] }))),
        Default::default()
    );
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
