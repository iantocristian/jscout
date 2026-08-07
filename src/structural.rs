use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::query::ModuleGraph;

pub const PROJECTION_VERSION: &str = "1";

#[derive(Debug, Clone)]
struct SymbolNode {
    id: i64,
    file_id: i64,
    path: String,
    name: String,
    scope: String,
    decl_start: i64,
    decl_end: i64,
    line: i64,
    key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub key: String,
    pub kind: String,
    pub display_name: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub confidence: String,
    pub provenance: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub detail: Value,
}

#[derive(Debug, Clone)]
pub struct NeighborhoodOptions {
    pub expected_snapshot: Option<String>,
    pub depth: usize,
    pub direction: String,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub min_confidence: String,
    pub kinds: Vec<String>,
}

impl Default for NeighborhoodOptions {
    fn default() -> Self {
        Self {
            expected_snapshot: None,
            depth: 1,
            direction: "both".into(),
            node_limit: 50,
            edge_limit: 200,
            min_confidence: "likely".into(),
            kinds: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Neighborhood {
    pub snapshot: String,
    pub requested_anchor: String,
    pub resolved_anchor: String,
    pub anchor_status: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
}

pub fn current_snapshot(conn: &Connection) -> Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |r| {
        r.get(0)
    })
    .context("no structural snapshot; run `jscout index <root>` first")
}

pub fn compute_snapshot(conn: &Connection) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-structural-snapshot\0");
    hasher.update(PROJECTION_VERSION.as_bytes());
    let mut stmt = conn.prepare("SELECT path, hash FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (path, hash) = row?;
        hasher.update(b"\0");
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Rebuild the disposable structural graph from canonical extraction tables.
pub fn rebuild_projection(conn: &Connection, snapshot: &str) -> Result<()> {
    let files = load_files(conn)?;
    let graph = ModuleGraph::load(conn)?;
    let symbols = load_symbols(conn, &files)?;
    let mut root_symbol: HashMap<(i64, String), Vec<&SymbolNode>> = HashMap::new();
    for symbol in &symbols {
        if symbol.scope.is_empty() {
            root_symbol
                .entry((symbol.file_id, symbol.name.clone()))
                .or_default()
                .push(symbol);
        }
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;

        let mut insert_node = conn.prepare_cached(
            "INSERT INTO graph_nodes(
               node_key, node_kind, native_table, native_id, display_name, file_id, line, meta_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        for (file_id, path) in &files {
            insert_node.execute(params![
                file_key(path),
                "file",
                "files",
                file_id,
                path,
                file_id,
                1_i64,
                json!({ "path": path }).to_string(),
            ])?;
        }
        for symbol in &symbols {
            insert_node.execute(params![
                symbol.key,
                "symbol",
                "symbols",
                symbol.id,
                symbol.name,
                symbol.file_id,
                symbol.line,
                json!({
                    "path": symbol.path,
                    "scope": symbol.scope,
                    "declaration": [symbol.decl_start, symbol.decl_end]
                })
                .to_string(),
            ])?;
        }

        let mut insert_edge = conn.prepare_cached(
            "INSERT INTO resolved_edges(
               src_key, dst_key, kind, confidence, provenance,
               source_file_id, source_ref_id, line, detail_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;

        project_module_edges(conn, &files, &mut insert_node, &mut insert_edge)?;
        project_references(
            conn,
            &files,
            &graph,
            &symbols,
            &root_symbol,
            &mut insert_edge,
        )?;
        project_events(conn, &files, &mut insert_node, &mut insert_edge)?;

        conn.execute(
            "INSERT INTO meta(key, value) VALUES('snapshot', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [snapshot],
        )?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('projection_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [PROJECTION_VERSION],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn load_files(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut files = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, path FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, path) = row?;
        files.insert(id, path);
    }
    Ok(files)
}

fn load_symbols(conn: &Connection, files: &HashMap<i64, String>) -> Result<Vec<SymbolNode>> {
    let mut raw = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, file_id, name, scope_chain, decl_start, decl_end, line
         FROM symbols ORDER BY file_id, scope_chain, name, decl_start, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        raw.push(row?);
    }
    raw.sort_by(|a, b| {
        let a_path = files.get(&a.1).map(String::as_str).unwrap_or("");
        let b_path = files.get(&b.1).map(String::as_str).unwrap_or("");
        (a_path, &a.3, &a.2, a.4, a.0).cmp(&(b_path, &b.3, &b.2, b.4, b.0))
    });

    let mut ordinals: HashMap<(i64, String, String), usize> = HashMap::new();
    let mut symbols = Vec::with_capacity(raw.len());
    for (id, file_id, name, scope, decl_start, decl_end, line) in raw {
        let path = files
            .get(&file_id)
            .cloned()
            .with_context(|| format!("symbol {id} refers to missing file {file_id}"))?;
        let ordinal = ordinals
            .entry((file_id, scope.clone(), name.clone()))
            .and_modify(|n| *n += 1)
            .or_insert(1);
        let key = symbol_key(&path, &scope, &name, *ordinal);
        symbols.push(SymbolNode {
            id,
            file_id,
            path,
            name,
            scope,
            decl_start,
            decl_end,
            line,
            key,
        });
    }
    Ok(symbols)
}

fn project_module_edges(
    conn: &Connection,
    files: &HashMap<i64, String>,
    insert_node: &mut rusqlite::CachedStatement<'_>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut packages = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT from_file, request, to_file, package FROM module_edges
         ORDER BY from_file, request",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (from_id, request, to_id, package) = row?;
        let Some(from_path) = files.get(&from_id) else {
            continue;
        };
        let destination = if let Some(to_id) = to_id {
            let Some(to_path) = files.get(&to_id) else {
                continue;
            };
            file_key(to_path)
        } else if let Some(package) = package {
            if packages.insert(package.clone()) {
                insert_node.execute(params![
                    package_key(&package),
                    "package",
                    Option::<String>::None,
                    Option::<i64>::None,
                    package,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    json!({ "package": package }).to_string(),
                ])?;
            }
            package_key(&package)
        } else {
            continue;
        };
        insert_edge.execute(params![
            file_key(from_path),
            destination,
            "import",
            "certain",
            "resolver",
            from_id,
            Option::<i64>::None,
            Option::<i64>::None,
            json!({ "request": request }).to_string(),
        ])?;
    }
    Ok(())
}

fn project_references(
    conn: &Connection,
    files: &HashMap<i64, String>,
    graph: &ModuleGraph,
    symbols: &[SymbolNode],
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut symbols_by_file: HashMap<i64, Vec<&SymbolNode>> = HashMap::new();
    for symbol in symbols {
        symbols_by_file
            .entry(symbol.file_id)
            .or_default()
            .push(symbol);
    }
    let mut package_for: HashMap<(i64, String), String> = HashMap::new();
    let mut package_stmt = conn.prepare(
        "SELECT from_file, request, package FROM module_edges WHERE package IS NOT NULL",
    )?;
    let package_rows = package_stmt.query_map([], |r| {
        Ok((
            (r.get::<_, i64>(0)?, r.get::<_, String>(1)?),
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in package_rows {
        let (key, package) = row?;
        package_for.insert(key, package);
    }

    let mut stmt = conn.prepare(
        "SELECT id, file_id, start, line, kind, confidence,
                target_request, target_name, local, detail
         FROM refs ORDER BY file_id, start, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, i64>(8)? != 0,
            r.get::<_, Option<String>>(9)?,
        ))
    })?;
    for row in rows {
        let (id, file_id, start, line, kind, confidence, request, name, local, detail) = row?;
        let Some(path) = files.get(&file_id) else {
            continue;
        };
        let source = owner_at(symbols_by_file.get(&file_id), start)
            .map(|s| s.key.clone())
            .unwrap_or_else(|| file_key(path));

        let target = if local {
            unique_symbol(root_symbol.get(&(file_id, name.clone())))
        } else if let Some(request) = request.as_deref() {
            match graph.edge(file_id, request) {
                Some(target_file) if name == "*" => {
                    graph.paths.get(&target_file).map(|path| file_key(path))
                }
                Some(target_file) => {
                    let resolved = graph
                        .resolve_export(target_file, &name)
                        .or_else(|| Some((target_file, name.clone())));
                    resolved.and_then(|(resolved_file, resolved_name)| {
                        if resolved_name == "*" {
                            graph.paths.get(&resolved_file).map(|path| file_key(path))
                        } else {
                            unique_symbol(root_symbol.get(&(resolved_file, resolved_name)))
                        }
                    })
                }
                None => package_for
                    .get(&(file_id, request.to_string()))
                    .map(|package| package_key(package)),
            }
        } else {
            None
        };
        let Some(target) = target else { continue };
        insert_edge.execute(params![
            source,
            target,
            kind,
            confidence,
            "semantic+resolver",
            file_id,
            id,
            line,
            json!({ "request": request, "targetName": name, "detail": detail }).to_string(),
        ])?;
    }
    Ok(())
}

fn project_events(
    conn: &Connection,
    files: &HashMap<i64, String>,
    insert_node: &mut rusqlite::CachedStatement<'_>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut hubs = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT e.rowid, e.file_id, e.line, e.role, e.name, e.method
         FROM events e ORDER BY e.name, e.file_id, e.line, e.rowid",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    for row in rows {
        let (event_id, file_id, line, role, name, method) = row?;
        let Some(path) = files.get(&file_id) else {
            continue;
        };
        let hub = event_key(&name);
        if hubs.insert(hub.clone()) {
            insert_node.execute(params![
                hub,
                "event_hub",
                Option::<String>::None,
                Option::<i64>::None,
                name,
                Option::<i64>::None,
                Option::<i64>::None,
                json!({ "event": name, "namespace": "unknown" }).to_string(),
            ])?;
        }
        let site = format!("event-site:{path}:{event_id}");
        insert_node.execute(params![
            site,
            "event_site",
            "events",
            event_id,
            format!("{role} {name}"),
            file_id,
            line,
            json!({ "event": name, "role": role, "method": method }).to_string(),
        ])?;
        insert_edge.execute(params![
            file_key(path),
            site,
            "contains_event",
            "certain",
            "syntax",
            file_id,
            Option::<i64>::None,
            line,
            "{}",
        ])?;
        let (source, target, edge_kind) = if role == "emit" {
            (site.clone(), hub.clone(), "emits")
        } else {
            (hub.clone(), site.clone(), "listens")
        };
        insert_edge.execute(params![
            source,
            target,
            edge_kind,
            "possible",
            "string-event",
            file_id,
            Option::<i64>::None,
            line,
            json!({ "namespace": "unknown", "method": method }).to_string(),
        ])?;
    }
    Ok(())
}

fn owner_at<'a>(symbols: Option<&Vec<&'a SymbolNode>>, offset: i64) -> Option<&'a SymbolNode> {
    symbols?
        .iter()
        .copied()
        .filter(|s| s.decl_start <= offset && offset < s.decl_end)
        .min_by_key(|s| s.decl_end - s.decl_start)
}

fn unique_symbol(symbols: Option<&Vec<&SymbolNode>>) -> Option<String> {
    let symbols = symbols?;
    (symbols.len() == 1).then(|| symbols[0].key.clone())
}

pub fn neighborhood(
    conn: &Connection,
    anchor: &str,
    options: &NeighborhoodOptions,
) -> Result<Neighborhood> {
    if !matches!(options.direction.as_str(), "in" | "out" | "both") {
        bail!("direction must be one of: in, out, both");
    }
    if confidence_rank(&options.min_confidence).is_none() {
        bail!("min confidence must be one of: certain, likely, possible");
    }
    if options.node_limit == 0 || options.edge_limit == 0 {
        bail!("node and edge limits must be greater than zero");
    }
    let snapshot = current_snapshot(conn)?;
    let (resolved_anchor, anchor_status) = resolve_anchor(
        conn,
        anchor,
        options.expected_snapshot.as_deref(),
        &snapshot,
    )?;
    let allowed_kinds: HashSet<&str> = options.kinds.iter().map(String::as_str).collect();
    let min_rank = confidence_rank(&options.min_confidence).unwrap();
    let mut discovered = HashSet::from([resolved_anchor.clone()]);
    let mut queue = VecDeque::from([(resolved_anchor.clone(), 0_usize)]);
    let mut edges_by_id: HashMap<i64, GraphEdge> = HashMap::new();
    let mut truncated = false;

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= options.depth {
            continue;
        }
        let directions: &[&str] = match options.direction.as_str() {
            "out" => &["out"],
            "in" => &["in"],
            _ => &["out", "in"],
        };
        for direction in directions {
            let sql = if *direction == "out" {
                "SELECT e.id, e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                        f.path, e.line, e.detail_json
                 FROM resolved_edges e LEFT JOIN files f ON e.source_file_id = f.id
                 WHERE e.src_key = ?1 ORDER BY e.kind, e.dst_key, e.id"
            } else {
                "SELECT e.id, e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                        f.path, e.line, e.detail_json
                 FROM resolved_edges e LEFT JOIN files f ON e.source_file_id = f.id
                 WHERE e.dst_key = ?1 ORDER BY e.kind, e.src_key, e.id"
            };
            let mut stmt = conn.prepare_cached(sql)?;
            let rows = stmt.query_map([&node], |r| {
                let detail: String = r.get(8)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    GraphEdge {
                        source: r.get(1)?,
                        target: r.get(2)?,
                        kind: r.get(3)?,
                        confidence: r.get(4)?,
                        provenance: r.get(5)?,
                        file: r.get(6)?,
                        line: r.get(7)?,
                        detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
                    },
                ))
            })?;
            for row in rows {
                let (edge_id, edge) = row?;
                if confidence_rank(&edge.confidence).unwrap_or(0) < min_rank
                    || (!allowed_kinds.is_empty() && !allowed_kinds.contains(edge.kind.as_str()))
                {
                    continue;
                }
                let other = if *direction == "out" {
                    &edge.target
                } else {
                    &edge.source
                };
                if !discovered.contains(other) && discovered.len() >= options.node_limit {
                    truncated = true;
                    continue;
                }
                if edges_by_id.len() >= options.edge_limit && !edges_by_id.contains_key(&edge_id) {
                    truncated = true;
                    continue;
                }
                if discovered.insert(other.clone()) {
                    queue.push_back((other.clone(), depth + 1));
                }
                edges_by_id.insert(edge_id, edge);
            }
        }
    }

    let mut nodes = Vec::with_capacity(discovered.len());
    for key in discovered {
        nodes.push(load_node(conn, &key)?.with_context(|| format!("missing graph node {key}"))?);
    }
    nodes.sort_by(|a, b| a.key.cmp(&b.key));
    let mut edges: Vec<GraphEdge> = edges_by_id.into_values().collect();
    edges.sort_by(|a, b| (&a.source, &a.kind, &a.target).cmp(&(&b.source, &b.kind, &b.target)));
    Ok(Neighborhood {
        snapshot,
        requested_anchor: anchor.to_string(),
        resolved_anchor,
        anchor_status,
        nodes,
        edges,
        truncated,
    })
}

fn resolve_anchor(
    conn: &Connection,
    anchor: &str,
    expected_snapshot: Option<&str>,
    current_snapshot: &str,
) -> Result<(String, String)> {
    let stale = expected_snapshot.is_some_and(|snapshot| snapshot != current_snapshot);
    if anchor.starts_with("sym:") && stale {
        let (path, scope, name, _) = parse_symbol_key(anchor)
            .with_context(|| format!("cannot re-resolve malformed stale anchor `{anchor}`"))?;
        let candidates = symbol_candidates(conn, Some(&path), Some(&scope), &name)?;
        return unique_anchor(anchor, candidates, "re-resolved");
    }
    if graph_node_exists(conn, anchor)? {
        return Ok((
            anchor.to_string(),
            if stale { "re-resolved" } else { "exact" }.into(),
        ));
    }

    if let Some(path) = anchor.strip_prefix("file:") {
        let candidates = file_candidates(conn, path)?;
        return unique_anchor(
            anchor,
            candidates,
            if stale { "re-resolved" } else { "resolved" },
        );
    }
    if !anchor.contains(':') {
        let files = file_candidates(conn, anchor)?;
        if files.len() == 1 {
            return Ok((files[0].clone(), "resolved".into()));
        }
    }
    let (path, name) = anchor
        .rsplit_once(':')
        .map_or((None, anchor), |(path, name)| (Some(path), name));
    let candidates = symbol_candidates(conn, path, None, name)?;
    unique_anchor(
        anchor,
        candidates,
        if stale { "re-resolved" } else { "resolved" },
    )
}

fn unique_anchor(anchor: &str, candidates: Vec<String>, status: &str) -> Result<(String, String)> {
    match candidates.as_slice() {
        [candidate] => Ok((candidate.clone(), status.to_string())),
        [] => bail!("anchor `{anchor}` was not found in the current snapshot"),
        _ => bail!(
            "anchor `{anchor}` is ambiguous in the current snapshot; candidates: {}",
            candidates.join(", ")
        ),
    }
}

fn graph_node_exists(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM graph_nodes WHERE node_key=?1)",
        [key],
        |r| r.get::<_, i64>(0),
    )? != 0)
}

fn symbol_candidates(
    conn: &Connection,
    path: Option<&str>,
    scope: Option<&str>,
    name: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT g.node_key, f.path, s.scope_chain
         FROM graph_nodes g
         JOIN symbols s ON g.native_table='symbols' AND g.native_id=s.id
         JOIN files f ON s.file_id=f.id
         WHERE s.name=?1 ORDER BY f.path, s.scope_chain, s.decl_start, g.node_key",
    )?;
    let rows = stmt.query_map([name], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (key, candidate_path, candidate_scope) = row?;
        if path.is_none_or(|filter| candidate_path == filter || candidate_path.contains(filter))
            && scope.is_none_or(|filter| candidate_scope == filter)
        {
            candidates.push(key);
        }
    }
    Ok(candidates)
}

fn file_candidates(conn: &Connection, path: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT node_key FROM graph_nodes
         WHERE node_kind='file' AND (display_name=?1 OR display_name LIKE '%' || ?1)
         ORDER BY display_name",
    )?;
    let rows = stmt.query_map([path], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn load_node(conn: &Connection, key: &str) -> Result<Option<GraphNode>> {
    let mut stmt = conn.prepare(
        "SELECT g.node_key, g.node_kind, g.display_name, f.path, g.line, g.meta_json
         FROM graph_nodes g LEFT JOIN files f ON g.file_id=f.id WHERE g.node_key=?1",
    )?;
    let node = stmt
        .query_row([key], |r| {
            let meta: String = r.get(5)?;
            Ok(GraphNode {
                key: r.get(0)?,
                kind: r.get(1)?,
                display_name: r.get(2)?,
                file: r.get(3)?,
                line: r.get(4)?,
                meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
            })
        })
        .optional()?;
    Ok(node)
}

fn confidence_rank(confidence: &str) -> Option<u8> {
    match confidence {
        "possible" => Some(0),
        "likely" => Some(1),
        "certain" => Some(2),
        _ => None,
    }
}

fn file_key(path: &str) -> String {
    format!("file:{path}")
}

fn package_key(package: &str) -> String {
    format!("pkg:{package}")
}

fn event_key(name: &str) -> String {
    format!("event:unknown:{name}")
}

fn symbol_key(path: &str, scope: &str, name: &str, ordinal: usize) -> String {
    format!("sym:{path}#{scope}::{name}@{ordinal}")
}

fn parse_symbol_key(key: &str) -> Result<(String, String, String, usize)> {
    let body = key.strip_prefix("sym:").context("missing sym: prefix")?;
    let (identity, ordinal) = body.rsplit_once('@').context("missing ordinal")?;
    let ordinal = ordinal.parse().context("invalid ordinal")?;
    let (path_scope, name) = identity.rsplit_once("::").context("missing symbol name")?;
    let (path, scope) = path_scope.rsplit_once('#').context("missing scope")?;
    Ok((path.into(), scope.into(), name.into(), ordinal))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;

    use super::{NeighborhoodOptions, neighborhood};
    use crate::{indexer, store};

    fn write(root: &Path, path: &str, source: &str) -> Result<()> {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, source)?;
        Ok(())
    }

    #[test]
    fn projects_resolved_calls_and_returns_snapshot() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(
            repo.path(),
            "a.ts",
            "export function greet(name) { return name; }\n",
        )?;
        write(
            repo.path(),
            "b.ts",
            "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let result = neighborhood(
            &conn,
            "a.ts:greet",
            &NeighborhoodOptions {
                depth: 2,
                ..Default::default()
            },
        )?;
        assert_eq!(result.snapshot.len(), 64);
        assert!(result.resolved_anchor.contains("a.ts#::greet@1"));
        assert!(result.edges.iter().any(|edge| {
            edge.kind == "call"
                && edge.source.contains("b.ts#::run@1")
                && edge.target.contains("a.ts#::greet@1")
                && edge.confidence == "certain"
        }));
        Ok(())
    }

    #[test]
    fn scopes_same_named_methods_by_class() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(
            repo.path(),
            "models.ts",
            "class Alpha { ping() {} }\nclass Beta { ping() {} }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let keys: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT node_key FROM graph_nodes
                 WHERE node_kind='symbol' AND display_name='ping' ORDER BY node_key",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|key| key.contains("#Alpha::ping@1")));
        assert!(keys.iter().any(|key| key.contains("#Beta::ping@1")));
        assert!(neighborhood(&conn, "ping", &NeighborhoodOptions::default()).is_err());
        Ok(())
    }

    #[test]
    fn rebuild_reroutes_barrel_reexports() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(repo.path(), "a.ts", "export function target() {}\n")?;
        write(repo.path(), "b.ts", "export function target() {}\n")?;
        write(repo.path(), "barrel.ts", "export { target } from './a';\n")?;
        write(
            repo.path(),
            "use.ts",
            "import { target } from './barrel';\nexport function run() { target(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let first = neighborhood(
            &conn,
            "use.ts:run",
            &NeighborhoodOptions {
                direction: "out".into(),
                ..Default::default()
            },
        )?;
        assert!(
            first
                .edges
                .iter()
                .any(|edge| edge.target.contains("a.ts#::target@1"))
        );

        write(repo.path(), "barrel.ts", "export { target } from './b';\n")?;
        indexer::index_repo(repo.path(), &conn)?;
        let second = neighborhood(
            &conn,
            "use.ts:run",
            &NeighborhoodOptions {
                direction: "out".into(),
                ..Default::default()
            },
        )?;
        assert!(
            second
                .edges
                .iter()
                .any(|edge| edge.target.contains("b.ts#::target@1"))
        );
        assert!(
            !second
                .edges
                .iter()
                .any(|edge| edge.target.contains("a.ts#::target@1"))
        );
        Ok(())
    }

    #[test]
    fn stale_symbol_anchor_is_explicitly_reresolved() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(repo.path(), "mod.ts", "export function target() {}\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let first = neighborhood(&conn, "mod.ts:target", &NeighborhoodOptions::default())?;

        write(
            repo.path(),
            "mod.ts",
            "// moved\n\nexport function target() {}\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        let second = neighborhood(
            &conn,
            &first.resolved_anchor,
            &NeighborhoodOptions {
                expected_snapshot: Some(first.snapshot.clone()),
                ..Default::default()
            },
        )?;
        assert_ne!(first.snapshot, second.snapshot);
        assert_eq!(second.anchor_status, "re-resolved");
        Ok(())
    }

    #[test]
    fn events_use_hubs_instead_of_direct_emit_listener_edges() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(repo.path(), "emit.ts", "bus.emit('ready');\n")?;
        write(repo.path(), "listen.ts", "bus.on('ready', start);\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let result = neighborhood(
            &conn,
            "event:unknown:ready",
            &NeighborhoodOptions {
                direction: "both".into(),
                min_confidence: "possible".into(),
                ..Default::default()
            },
        )?;
        assert!(result.edges.iter().any(|edge| edge.kind == "emits"));
        assert!(result.edges.iter().any(|edge| edge.kind == "listens"));
        assert!(!result.edges.iter().any(|edge| {
            edge.source.starts_with("event-site:") && edge.target.starts_with("event-site:")
        }));
        Ok(())
    }
}
