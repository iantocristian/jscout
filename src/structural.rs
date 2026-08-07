use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{query::ModuleGraph, store};

pub const PROJECTION_VERSION: &str = "2";

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

#[derive(Debug, Default)]
struct ProjectedTargets {
    keys: Vec<String>,
    ambiguous: bool,
}

impl ProjectedTargets {
    fn exact(key: String) -> Self {
        Self {
            keys: vec![key],
            ambiguous: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub key: String,
    pub kind: String,
    pub display_name: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub meta: Value,
    /// Query-local path relevance; not part of persistent graph identity.
    pub relevance: f64,
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
    /// Query-local path relevance; not part of the canonical edge.
    pub relevance: f64,
}

#[derive(Debug)]
struct RankedStep {
    edge_id: i64,
    edge: GraphEdge,
    other: String,
    depth: usize,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    score: f64,
}

impl PartialEq for RankedStep {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits()
            && self.depth == other.depth
            && self.edge_id == other.edge_id
            && self.other == other.other
    }
}

impl Eq for RankedStep {}

impl PartialOrd for RankedStep {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedStep {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.depth.cmp(&self.depth))
            .then_with(|| other.other.cmp(&self.other))
            .then_with(|| other.edge_id.cmp(&self.edge_id))
    }
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

        let timing = std::env::var_os("JSCOUT_TIMING").is_some();
        let stage_started = Instant::now();
        project_module_edges(conn, &files, &mut insert_node, &mut insert_edge)?;
        if timing {
            eprintln!("timing project-modules={:?}", stage_started.elapsed());
        }
        let stage_started = Instant::now();
        project_references(
            conn,
            &files,
            &graph,
            &symbols,
            &root_symbol,
            &mut insert_edge,
        )?;
        if timing {
            eprintln!("timing project-references={:?}", stage_started.elapsed());
        }
        let stage_started = Instant::now();
        project_member_calls(
            conn,
            &files,
            &symbols,
            &mut insert_node,
            &mut insert_edge,
        )?;
        if timing {
            eprintln!("timing project-member-calls={:?}", stage_started.elapsed());
        }
        let stage_started = Instant::now();
        project_events(conn, &files, &mut insert_node, &mut insert_edge)?;
        if timing {
            eprintln!("timing project-events={:?}", stage_started.elapsed());
        }

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

        let targets = if local {
            projected_symbols(root_symbol.get(&(file_id, name.clone())))
        } else if let Some(request) = request.as_deref() {
            match graph.edge(file_id, request) {
                Some(target_file) if name == "*" => graph
                    .paths
                    .get(&target_file)
                    .map(|path| ProjectedTargets::exact(file_key(path)))
                    .unwrap_or_default(),
                Some(target_file) => {
                    let resolved = graph
                        .resolve_export(target_file, &name)
                        .or_else(|| Some((target_file, name.clone())));
                    resolved.map_or_else(ProjectedTargets::default, |(resolved_file, resolved_name)| {
                        if resolved_name == "*" {
                            graph
                                .paths
                                .get(&resolved_file)
                                .map(|path| ProjectedTargets::exact(file_key(path)))
                                .unwrap_or_default()
                        } else {
                            projected_symbols(root_symbol.get(&(resolved_file, resolved_name)))
                        }
                    })
                }
                None => package_for
                    .get(&(file_id, request.to_string()))
                    .map(|package| ProjectedTargets::exact(package_key(package)))
                    .unwrap_or_default(),
            }
        } else {
            ProjectedTargets::default()
        };
        if targets.keys.is_empty() {
            continue;
        }
        let projected_confidence = if targets.ambiguous {
            "possible"
        } else {
            confidence.as_str()
        };
        let provenance = if targets.ambiguous {
            "semantic+resolver-candidate"
        } else {
            "semantic+resolver"
        };
        let edge_detail = json!({
            "request": &request,
            "targetName": &name,
            "detail": &detail,
            "ambiguousTarget": targets.ambiguous,
            "candidateCount": targets.keys.len(),
            "candidates": targets.ambiguous.then_some(&targets.keys),
        })
        .to_string();
        for target in &targets.keys {
            insert_edge.execute(params![
                source,
                target,
                kind,
                projected_confidence,
                provenance,
                file_id,
                id,
                line,
                edge_detail,
            ])?;
        }
    }
    Ok(())
}

fn project_member_calls(
    conn: &Connection,
    files: &HashMap<i64, String>,
    symbols: &[SymbolNode],
    insert_node: &mut rusqlite::CachedStatement<'_>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut symbols_by_file: HashMap<i64, Vec<&SymbolNode>> = HashMap::new();
    let mut candidates_by_name: HashMap<String, Vec<&SymbolNode>> = HashMap::new();
    for symbol in symbols {
        symbols_by_file
            .entry(symbol.file_id)
            .or_default()
            .push(symbol);
        candidates_by_name
            .entry(symbol.name.clone())
            .or_default()
            .push(symbol);
    }

    let mut hubs = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT m.rowid, m.file_id, m.start, m.line, m.prop, m.object
         FROM member_calls m
         ORDER BY m.prop, m.file_id, m.start, m.rowid",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in rows {
        let (member_call_id, file_id, start, line, property, object) = row?;
        let Some(path) = files.get(&file_id) else {
            continue;
        };
        let candidates: &[&SymbolNode] = candidates_by_name
            .get(&property)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if candidates.is_empty() {
            continue;
        }
        let hub = member_key(&property);
        if hubs.insert(hub.clone()) {
            insert_node.execute(params![
                hub,
                "member_hub",
                Option::<String>::None,
                Option::<i64>::None,
                property,
                Option::<i64>::None,
                Option::<i64>::None,
                json!({
                    "property": property,
                    "receiver": "unknown",
                    "candidateCount": candidates.len()
                })
                .to_string(),
            ])?;
            for candidate in candidates {
                insert_edge.execute(params![
                    hub,
                    candidate.key,
                    "member_candidate",
                    "possible",
                    "member-name-match",
                    Option::<i64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    json!({
                        "property": property,
                        "candidateCount": candidates.len()
                    })
                    .to_string(),
                ])?;
            }
        }

        let source = owner_at(symbols_by_file.get(&file_id), start)
            .map(|symbol| symbol.key.clone())
            .unwrap_or_else(|| file_key(path));
        insert_edge.execute(params![
            source,
            hub,
            "member_call",
            "possible",
            "member-name-match",
            file_id,
            Option::<i64>::None,
            line,
            json!({
                "memberCallId": member_call_id,
                "object": object,
                "property": property,
                "candidateCount": candidates.len()
            })
            .to_string(),
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

fn projected_symbols(symbols: Option<&Vec<&SymbolNode>>) -> ProjectedTargets {
    let Some(symbols) = symbols else {
        return ProjectedTargets::default();
    };
    ProjectedTargets {
        keys: symbols.iter().map(|symbol| symbol.key.clone()).collect(),
        ambiguous: symbols.len() > 1,
    }
}

pub fn neighborhood(
    conn: &Connection,
    anchor: &str,
    options: &NeighborhoodOptions,
) -> Result<Neighborhood> {
    store::with_read_snapshot(conn, "jscout_neighborhood", || {
        neighborhood_in_snapshot(conn, anchor, options)
    })
}

fn neighborhood_in_snapshot(
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
    let mut node_relevance = HashMap::from([(resolved_anchor.clone(), 1.0_f64)]);
    let mut expanded = HashSet::from([resolved_anchor.clone()]);
    let mut frontier = BinaryHeap::new();
    let mut edges_by_id: HashMap<i64, GraphEdge> = HashMap::new();
    let mut degree_cache = HashMap::new();
    let mut truncated = false;

    if options.depth > 0 {
        let root_degree = graph_degree(conn, &resolved_anchor)?;
        degree_cache.insert(resolved_anchor.clone(), root_degree);
        enqueue_ranked_steps(
            conn,
            &resolved_anchor,
            0,
            hub_damping(root_degree),
            1.0,
            1.0,
            options,
            min_rank,
            &allowed_kinds,
            &mut degree_cache,
            &mut frontier,
        )?;
    }

    while let Some(step) = frontier.pop() {
        if edges_by_id.contains_key(&step.edge_id) {
            continue;
        }
        if edges_by_id.len() >= options.edge_limit {
            truncated = true;
            break;
        }
        let new_node = !discovered.contains(&step.other);
        if new_node && discovered.len() >= options.node_limit {
            truncated = true;
            continue;
        }
        if new_node {
            discovered.insert(step.other.clone());
            node_relevance.insert(step.other.clone(), step.score);
        }
        edges_by_id.insert(step.edge_id, step.edge);

        if step.depth < options.depth && expanded.insert(step.other.clone()) {
            enqueue_ranked_steps(
                conn,
                &step.other,
                step.depth,
                step.confidence_floor,
                step.relation_floor,
                step.hub_floor,
                options,
                min_rank,
                &allowed_kinds,
                &mut degree_cache,
                &mut frontier,
            )?;
        }
    }

    let mut nodes = Vec::with_capacity(discovered.len());
    for key in discovered {
        let mut node =
            load_node(conn, &key)?.with_context(|| format!("missing graph node {key}"))?;
        node.relevance = *node_relevance.get(&key).unwrap_or(&0.0);
        nodes.push(node);
    }
    nodes.sort_by(|a, b| {
        b.relevance.total_cmp(&a.relevance).then_with(|| a.key.cmp(&b.key))
    });
    let mut edges: Vec<GraphEdge> = edges_by_id.into_values().collect();
    edges.sort_by(|a, b| {
        b.relevance.total_cmp(&a.relevance).then_with(|| {
            (&a.source, &a.kind, &a.target).cmp(&(&b.source, &b.kind, &b.target))
        })
    });
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

#[allow(clippy::too_many_arguments)]
fn enqueue_ranked_steps(
    conn: &Connection,
    node: &str,
    depth: usize,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    options: &NeighborhoodOptions,
    min_rank: u8,
    allowed_kinds: &HashSet<&str>,
    degree_cache: &mut HashMap<String, usize>,
    frontier: &mut BinaryHeap<RankedStep>,
) -> Result<()> {
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
             WHERE e.src_key = ?1"
        } else {
            "SELECT e.id, e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                    f.path, e.line, e.detail_json
             FROM resolved_edges e LEFT JOIN files f ON e.source_file_id = f.id
             WHERE e.dst_key = ?1"
        };
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([node], |r| {
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
                    relevance: 0.0,
                },
            ))
        })?;
        for row in rows {
            let (edge_id, mut edge) = row?;
            if confidence_rank(&edge.confidence).unwrap_or(0) < min_rank
                || (!allowed_kinds.is_empty() && !allowed_kinds.contains(edge.kind.as_str()))
            {
                continue;
            }
            let other = if *direction == "out" {
                edge.target.clone()
            } else {
                edge.source.clone()
            };
            let degree = match degree_cache.get(&other) {
                Some(degree) => *degree,
                None => {
                    let degree = graph_degree(conn, &other)?;
                    degree_cache.insert(other.clone(), degree);
                    degree
                }
            };
            let confidence_floor = confidence_floor.min(confidence_weight(&edge.confidence));
            let relation_floor = relation_floor.min(relation_weight(&edge.kind));
            let hub_floor = hub_floor.min(hub_damping(degree));
            let next_depth = depth + 1;
            let score = round_score(
                confidence_floor
                    * relation_floor
                    * distance_decay(next_depth)
                    * hub_floor,
            );
            edge.relevance = score;
            frontier.push(RankedStep {
                edge_id,
                edge,
                other,
                depth: next_depth,
                confidence_floor,
                relation_floor,
                hub_floor,
                score,
            });
        }
    }
    Ok(())
}

fn graph_degree(conn: &Connection, node: &str) -> Result<usize> {
    let degree: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resolved_edges WHERE src_key=?1 OR dst_key=?1",
        [node],
        |row| row.get(0),
    )?;
    Ok(degree.max(0) as usize)
}

fn confidence_weight(confidence: &str) -> f64 {
    match confidence {
        "certain" => 1.0,
        "likely" => 0.6,
        "possible" => 0.3,
        _ => 0.0,
    }
}

fn relation_weight(kind: &str) -> f64 {
    match kind {
        "call" | "render" | "extend" => 1.0,
        "member_call" | "member_candidate" => 0.9,
        "import" | "reexport" => 0.75,
        "emits" | "listens" => 0.7,
        "contains_event" => 0.6,
        _ => 0.5,
    }
}

fn distance_decay(depth: usize) -> f64 {
    0.75_f64.powi(depth.saturating_sub(1) as i32)
}

fn hub_damping(degree: usize) -> f64 {
    1.0 / (degree as f64 + 2.0).log2()
}

fn round_score(score: f64) -> f64 {
    (score * 1_000_000.0).round() / 1_000_000.0
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
                relevance: 0.0,
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

fn member_key(name: &str) -> String {
    format!("member:unknown:{name}")
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

    use super::{NeighborhoodOptions, compute_snapshot, neighborhood, rebuild_projection};
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
    fn truncation_keeps_higher_ranked_candidates_not_sql_order() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(repo.path(), "root.ts", "export const root = 1;\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        for (key, name) in [("candidate:a-low", "a-low"), ("candidate:z-high", "z-high")] {
            conn.execute(
                "INSERT INTO graph_nodes(node_key, node_kind, display_name, meta_json)
                 VALUES(?1, 'candidate', ?2, '{}')",
                rusqlite::params![key, name],
            )?;
        }
        conn.execute(
            "INSERT INTO resolved_edges(
               src_key, dst_key, kind, confidence, provenance, detail_json
             ) VALUES('file:root.ts', 'candidate:a-low', 'call', 'possible', 'test', '{}')",
            [],
        )?;
        conn.execute(
            "INSERT INTO resolved_edges(
               src_key, dst_key, kind, confidence, provenance, detail_json
             ) VALUES('file:root.ts', 'candidate:z-high', 'import', 'certain', 'test', '{}')",
            [],
        )?;

        let result = neighborhood(
            &conn,
            "file:root.ts",
            &NeighborhoodOptions {
                depth: 1,
                direction: "out".into(),
                node_limit: 2,
                edge_limit: 2,
                min_confidence: "possible".into(),
                kinds: Vec::new(),
                expected_snapshot: None,
            },
        )?;
        assert!(result.truncated);
        assert!(result.nodes.iter().any(|node| node.key == "candidate:z-high"));
        assert!(!result.nodes.iter().any(|node| node.key == "candidate:a-low"));
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].kind, "import");
        assert!(result.edges[0].relevance > 0.0);
        Ok(())
    }

    #[test]
    fn ambiguous_root_reference_projects_every_candidate_as_possible() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(
            repo.path(),
            "ambiguous.js",
            "function target() {}\nfunction run() { target(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let file_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path='ambiguous.js'",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO symbols(
               file_id, name, kind, start, end, decl_start, decl_end,
               scope_chain, line, exported
             ) VALUES(?1, 'target', 'function', 0, 20, 0, 20, '', 1, 0)",
            [file_id],
        )?;
        let snapshot = compute_snapshot(&conn)?;
        rebuild_projection(&conn, &snapshot)?;

        let result = neighborhood(
            &conn,
            "ambiguous.js:run",
            &NeighborhoodOptions {
                direction: "out".into(),
                min_confidence: "possible".into(),
                ..Default::default()
            },
        )?;
        let candidates: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.kind == "call" && edge.target.contains("::target@"))
            .collect();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|edge| edge.confidence == "possible"));
        assert!(candidates.iter().all(|edge| {
            edge.detail["ambiguousTarget"] == true && edge.detail["candidateCount"] == 2
        }));
        Ok(())
    }

    #[test]
    fn possible_member_calls_traverse_through_candidate_hubs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(
            repo.path(),
            "service.ts",
            "class Service { load() {} }\nfunction run(client) { client.load(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let default_result = neighborhood(
            &conn,
            "service.ts:load",
            &NeighborhoodOptions {
                depth: 2,
                direction: "both".into(),
                ..Default::default()
            },
        )?;
        assert!(!default_result.edges.iter().any(|edge| {
            edge.kind == "member_call" || edge.kind == "member_candidate"
        }));

        let result = neighborhood(
            &conn,
            "service.ts:load",
            &NeighborhoodOptions {
                depth: 2,
                direction: "both".into(),
                min_confidence: "possible".into(),
                ..Default::default()
            },
        )?;
        assert!(result.edges.iter().any(|edge| {
            edge.kind == "member_candidate"
                && edge.source == "member:unknown:load"
                && edge.target.contains("#Service::load@1")
        }));
        assert!(result.edges.iter().any(|edge| {
            edge.kind == "member_call"
                && edge.source.contains("#::run@1")
                && edge.target == "member:unknown:load"
                && edge.confidence == "possible"
        }));
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
