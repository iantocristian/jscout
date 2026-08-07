//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0).
//! Exposes the index to agents: semantic_search, who_uses, definition,
//! file_outline, events.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{embed, query, search, store};

pub fn serve(root: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    let conn = store::open(&root)?;
    let provider = embed::Provider::from_env();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(&mut out, &rpc_error(Value::Null, -32700, &format!("parse error: {e}")))?;
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        // Notifications get no response.
        if id.is_null() && method.starts_with("notifications/") {
            continue;
        }
        let response = match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("2025-06-18");
                rpc_ok(
                    id,
                    json!({
                        "protocolVersion": requested,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "jscout", "version": env!("CARGO_PKG_VERSION") }
                    }),
                )
            }
            "ping" => rpc_ok(id, json!({})),
            "tools/list" => rpc_ok(id, json!({ "tools": tool_defs() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(&conn, provider.as_ref(), name, &args) {
                    Ok(text) => rpc_ok(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    ),
                    Err(e) => rpc_ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": format!("error: {e}") }],
                            "isError": true
                        }),
                    ),
                }
            }
            _ => rpc_error(id, -32601, &format!("method not found: {method}")),
        };
        write_msg(&mut out, &response)?;
    }
    Ok(())
}

fn write_msg(out: &mut impl Write, msg: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, msg)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "semantic_search",
            "description": "Hybrid (BM25 + embedding) search over the indexed codebase. Returns ranked code chunks with call-graph context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language and/or identifiers" },
                    "limit": { "type": "integer", "default": 8 }
                },
                "required": ["query"]
            }
        },
        {
            "name": "who_uses",
            "description": "All usage sites of a symbol (function, class, component, method), grouped by confidence: certain (resolved imports/renders/calls), possible (name-match member calls).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "NAME or path-substring:NAME, e.g. 'getUser' or 'services/user:getUser'" }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "definition",
            "description": "Definition site(s) and source of a symbol.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "NAME or path-substring:NAME" }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "file_outline",
            "description": "Structural outline of one file: chunks (functions, classes, components) with line ranges and the symbols they use.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Repo-relative path (or unique suffix)" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "events",
            "description": "String-keyed event wiring: emit sites and listener sites, matched by event name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Optional event name filter" }
                }
            }
        }
    ])
}

fn call_tool(
    conn: &Connection,
    provider: Option<&embed::Provider>,
    name: &str,
    args: &Value,
) -> Result<String> {
    match name {
        "semantic_search" => {
            let q = args["query"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(8) as usize;
            let hits = search::search(conn, provider, q, limit)?;
            Ok(serde_json::to_string_pretty(&hits)?)
        }
        "who_uses" => {
            let spec = args["symbol"].as_str().unwrap_or("");
            let graph = query::ModuleGraph::load(conn)?;
            let targets = query::find_symbols(conn, spec)?;
            let mut results = Vec::new();
            for t in &targets {
                let usages = query::who_uses(conn, &graph, t.file_id, &t.name)?;
                results.push(json!({ "target": t, "usages": usages }));
            }
            Ok(serde_json::to_string_pretty(&results)?)
        }
        "definition" => {
            let spec = args["symbol"].as_str().unwrap_or("");
            let targets = query::find_symbols(conn, spec)?;
            let mut results = Vec::new();
            for t in targets.iter().take(5) {
                let source: Option<String> = conn
                    .query_row(
                        "SELECT c.content FROM chunks c JOIN files f ON c.file_id = f.id
                         WHERE f.id = ?1 AND (c.name = ?2 OR c.symbols LIKE '%' || ?2 || '%')
                         ORDER BY c.name = ?2 DESC LIMIT 1",
                        rusqlite::params![t.file_id, t.name],
                        |r| r.get(0),
                    )
                    .ok();
                results.push(json!({ "target": t, "source": source }));
            }
            Ok(serde_json::to_string_pretty(&results)?)
        }
        "file_outline" => {
            let path = args["path"].as_str().unwrap_or("");
            let mut stmt = conn.prepare(
                "SELECT f.path, c.kind, c.name, c.scope_chain, c.start_line, c.end_line, c.id
                 FROM chunks c JOIN files f ON c.file_id = f.id
                 WHERE f.path = ?1 OR f.path LIKE '%' || ?1
                 ORDER BY f.path, c.start",
            )?;
            let rows = stmt.query_map([path], |r| {
                Ok(json!({
                    "file": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "name": r.get::<_, Option<String>>(2)?,
                    "scope": r.get::<_, String>(3)?,
                    "lines": [r.get::<_, i64>(4)?, r.get::<_, i64>(5)?],
                }))
            })?;
            let outline: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
            Ok(serde_json::to_string_pretty(&outline)?)
        }
        "events" => {
            let filter = args["name"].as_str();
            let sites = query::events(conn, filter)?;
            Ok(serde_json::to_string_pretty(&sites)?)
        }
        _ => anyhow::bail!("unknown tool: {name}"),
    }
}
