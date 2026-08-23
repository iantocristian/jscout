use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{file_role, origin, query::ModuleGraph, store};

pub const PROJECTION_VERSION: &str = "12";
const WORKFLOW_HUB_DEGREE_LIMIT: usize = 12;

#[derive(Debug, Clone)]
struct SymbolNode {
    id: i64,
    file_id: i64,
    path: String,
    name: String,
    kind: String,
    scope: String,
    start: i64,
    decl_start: i64,
    decl_end: i64,
    line: i64,
    key: String,
}

#[derive(Debug)]
struct EntitySiteNode {
    id: i64,
    file_id: i64,
    chunk_id: Option<i64>,
    start: i64,
    end: i64,
    line: i64,
    end_line: i64,
    plane: String,
    entity_type: String,
    role: String,
    identity_kind: String,
    identity_name: String,
    identity_start: i64,
    target_name: Option<String>,
    target_start: Option<i64>,
    extractor: String,
    provenance: String,
    confidence: String,
    detail: Value,
}

#[derive(Debug, Clone)]
struct ContractDefinition {
    site_id: i64,
    file_id: i64,
    start: i64,
    end: i64,
    line: i64,
    entity_type: String,
    name: String,
    key: String,
}

#[derive(Debug, Default)]
struct ContractCatalog {
    by_site: HashMap<i64, ContractDefinition>,
    by_local: HashMap<(i64, String), Vec<ContractDefinition>>,
    by_file: HashMap<i64, Vec<ContractDefinition>>,
    imports: HashMap<(i64, String), Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
struct ContractTarget {
    key: String,
    entity_type: String,
    name: String,
    identity_anchor: Option<String>,
    inferred: bool,
    file_id: Option<i64>,
    line: Option<i64>,
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
    /// Deterministic role of the backing file; absent for package/event hubs.
    pub file_role: Option<String>,
    /// Corpus origin of the backing file; absent for package/event hubs.
    pub file_origin: Option<String>,
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
    detail_key: String,
    edge: GraphEdge,
    other: String,
    depth: usize,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    role_floor: f64,
    score: f64,
}

#[derive(Debug)]
struct OrderedGraphEdge {
    edge_id: i64,
    detail_key: String,
    edge: GraphEdge,
}

fn graph_edge_content_cmp(
    left: &GraphEdge,
    left_detail_key: &str,
    right: &GraphEdge,
    right_detail_key: &str,
) -> Ordering {
    left.source
        .cmp(&right.source)
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.target.cmp(&right.target))
        .then_with(|| left.confidence.cmp(&right.confidence))
        .then_with(|| left.provenance.cmp(&right.provenance))
        .then_with(|| left.file.cmp(&right.file))
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left_detail_key.cmp(right_detail_key))
}

impl PartialEq for RankedStep {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits()
            && self.depth == other.depth
            && self.edge_id == other.edge_id
            && self.other == other.other
            && graph_edge_content_cmp(&self.edge, &self.detail_key, &other.edge, &other.detail_key)
                == Ordering::Equal
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
            .then_with(|| {
                graph_edge_content_cmp(&other.edge, &other.detail_key, &self.edge, &self.detail_key)
            })
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
    /// Empty means every role. Hubs without a backing file always remain eligible.
    pub file_roles: Vec<String>,
    /// Eligible backing-file origins. Hubs remain eligible; dependencies require opt-in.
    pub file_origins: Vec<String>,
    /// Apply deterministic non-production penalties before traversal budgets.
    pub penalize_file_roles: bool,
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
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            penalize_file_roles: false,
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

#[derive(Debug, Serialize)]
pub struct WorkflowNeighborhood {
    pub nodes: Vec<GraphNode>,
    pub traversed_edges: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
struct WorkflowLogicalStep {
    other: GraphNode,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    runtime_boundary: bool,
    terminal: bool,
}

#[derive(Debug, Clone)]
struct WorkflowRankedNode {
    key: String,
    depth: usize,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    crossed_runtime: bool,
    score: f64,
}

impl PartialEq for WorkflowRankedNode {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits()
            && self.depth == other.depth
            && self.key == other.key
    }
}

impl Eq for WorkflowRankedNode {}

impl PartialOrd for WorkflowRankedNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorkflowRankedNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.depth.cmp(&self.depth))
            .then_with(|| other.key.cmp(&self.key))
    }
}

pub const MAX_PATH_NODE_LIMIT: usize = 200;
pub const MAX_PATH_EDGE_LIMIT: usize = 800;
pub const MAX_PATH_SEARCH_STATES: usize = 50_000;

#[derive(Debug, Clone)]
pub struct PathOptions {
    pub expected_snapshot: Option<String>,
    pub max_depth: usize,
    pub path_limit: usize,
    pub node_limit: usize,
    pub edge_limit: usize,
    pub direction: String,
    pub min_confidence: String,
    pub kinds: Vec<String>,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
}

impl Default for PathOptions {
    fn default() -> Self {
        Self {
            expected_snapshot: None,
            max_depth: 4,
            path_limit: 8,
            node_limit: MAX_PATH_NODE_LIMIT,
            edge_limit: MAX_PATH_EDGE_LIMIT,
            direction: "both".into(),
            min_confidence: "likely".into(),
            kinds: Vec::new(),
            file_roles: file_role::DEFAULT_EXPANSION
                .iter()
                .map(|role| (*role).to_string())
                .collect(),
            file_origins: origin::defaults(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PathStep {
    pub from: String,
    pub to: String,
    /// True when the path traverses the underlying directed edge target-to-source.
    pub reversed: bool,
    pub edge: GraphEdge,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphPath {
    pub score: f64,
    pub nodes: Vec<GraphNode>,
    pub steps: Vec<PathStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathSearch {
    pub snapshot: String,
    pub requested_from: String,
    pub requested_to: String,
    pub resolved_from: String,
    pub resolved_to: String,
    pub from_status: String,
    pub to_status: String,
    pub paths: Vec<GraphPath>,
    pub searched_nodes: usize,
    pub searched_edges: usize,
    pub searched_states: usize,
    pub truncated: bool,
}

pub fn current_snapshot(conn: &Connection) -> Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |r| {
        r.get(0)
    })
    .context("no structural snapshot; run `jscout index <root>` first")
}

#[cfg(test)]
pub fn compute_snapshot(conn: &Connection) -> Result<String> {
    let resolution_hash = compute_resolution_hash(conn)?;
    compute_snapshot_with_resolution(conn, &resolution_hash)
}

/// Deterministic digest of the module-resolution outcome. Resolution reads
/// unindexed inputs such as tsconfigs, manifests, and `node_modules`, so this
/// digest is part of the public structural snapshot as well as the no-op
/// projection identity.
pub(crate) fn compute_resolution_hash(conn: &Connection) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-resolution-hash-v2\0");
    let mut stmt = conn.prepare(
        "SELECT source.path, edge.request, COALESCE(target.path, ''),
                COALESCE(edge.package, ''), COALESCE(edge.resolution, ''),
                COALESCE(package.canonical_root, ''), edge.type_only
         FROM module_edges edge
         JOIN files source ON source.id=edge.from_file
         LEFT JOIN files target ON target.id=edge.to_file
         LEFT JOIN package_instances package ON package.id=edge.package_instance_id
         ORDER BY source.path, edge.request, COALESCE(target.path, ''),
                  COALESCE(edge.package, ''), COALESCE(edge.resolution, ''),
                  COALESCE(package.canonical_root, ''), edge.type_only",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (source, request, target, package, resolution, package_root, type_only) = row?;
        for value in [
            source.as_str(),
            request.as_str(),
            target.as_str(),
            package.as_str(),
            resolution.as_str(),
            package_root.as_str(),
        ] {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update(type_only.to_le_bytes().as_slice());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) fn compute_snapshot_with_resolution(
    conn: &Connection,
    resolution_hash: &str,
) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-structural-snapshot-v2\0");
    hasher.update(PROJECTION_VERSION.as_bytes());
    let mut stmt = conn.prepare(
        "SELECT f.path, f.hash, f.role, f.origin, COALESCE(f.package_path, ''),
                COALESCE(p.origin, ''), COALESCE(p.name, ''),
                COALESCE(p.version, ''), COALESCE(p.locator, ''),
                COALESCE(p.manifest_hash, ''), COALESCE(p.status, '')
         FROM files f
         LEFT JOIN package_instances p ON p.id = f.package_instance_id
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok([
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, String>(10)?,
        ])
    })?;
    for row in rows {
        for value in row? {
            hasher.update(b"\0");
            hasher.update(value.as_bytes());
        }
    }
    hasher.update(b"\0module-resolution\0");
    hasher.update(resolution_hash.as_bytes());
    Ok(hasher.finalize().to_hex().to_string())
}

/// Rebuild the disposable structural graph from canonical extraction tables.
pub fn rebuild_projection(conn: &Connection, snapshot: &str) -> Result<()> {
    rebuild_projection_with_timing(conn, snapshot, false)
}

pub fn rebuild_projection_with_timing(
    conn: &Connection,
    snapshot: &str,
    timing: bool,
) -> Result<()> {
    let files = load_files(conn)?;
    let graph = ModuleGraph::load_with_contracts(conn)?;
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
        conn.execute("DELETE FROM entities", [])?;

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
        project_entities(
            conn,
            &files,
            &graph,
            &symbols,
            &root_symbol,
            &mut insert_node,
            &mut insert_edge,
        )?;
        if timing {
            eprintln!("timing project-entities={:?}", stage_started.elapsed());
        }
        let stage_started = Instant::now();
        project_member_calls(conn, &files, &symbols, &mut insert_node, &mut insert_edge)?;
        if timing {
            eprintln!("timing project-member-calls={:?}", stage_started.elapsed());
        }
        let stage_started = Instant::now();
        receiver_flow::project_receiver_value_flows(
            conn,
            &files,
            &graph,
            &symbols,
            &root_symbol,
            &mut insert_edge,
        )?;
        if timing {
            eprintln!(
                "timing project-receiver-value-flows={:?}",
                stage_started.elapsed()
            );
        }
        let stage_started = Instant::now();
        project_checker_enrichments(conn, &files, &symbols, snapshot, &mut insert_edge)?;
        if timing {
            eprintln!(
                "timing project-checker-enrichments={:?}",
                stage_started.elapsed()
            );
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

/// Remove the optional checker batch and its projected edges without changing
/// the deterministic structural snapshot. Watch uses this before an explicit
/// enrichment cycle so config-only events fail closed even when module
/// resolution produces the same snapshot hash.
#[cfg(test)]
pub(crate) fn clear_checker_plane(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        conn.execute("DELETE FROM checker_enrichment_batches", [])?;
        conn.execute("DELETE FROM resolved_edges WHERE provenance='checker'", [])?;
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
        "SELECT id, file_id, name, kind, scope_chain, start, decl_start, decl_end, line
         FROM symbols ORDER BY file_id, scope_chain, name, decl_start, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)?,
        ))
    })?;
    for row in rows {
        raw.push(row?);
    }
    raw.sort_by(|a, b| {
        let a_path = files.get(&a.1).map_or("", String::as_str);
        let b_path = files.get(&b.1).map_or("", String::as_str);
        (a_path, &a.4, &a.2, a.6, a.0).cmp(&(b_path, &b.4, &b.2, b.6, b.0))
    });

    let mut ordinals: HashMap<(i64, String, String), usize> = HashMap::new();
    let mut symbols = Vec::with_capacity(raw.len());
    for (id, file_id, name, kind, scope, start, decl_start, decl_end, line) in raw {
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
            kind,
            scope,
            start,
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
    let mut package_hubs = HashSet::new();
    let mut contained_modules = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT edge.from_file, edge.request, edge.to_file, edge.package, edge.resolution,
                edge.package_instance_id, package.origin, package.name, package.version,
                package.locator, package.status, source.package_instance_id, edge.type_only
         FROM module_edges edge
         JOIN files source ON source.id=edge.from_file
         LEFT JOIN package_instances package ON package.id=edge.package_instance_id
         ORDER BY edge.from_file, edge.request",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, Option<i64>>(11)?,
            r.get::<_, i64>(12)? != 0,
        ))
    })?;
    for row in rows {
        let (
            from_id,
            request,
            to_id,
            package,
            resolution,
            package_instance_id,
            package_origin,
            instance_name,
            instance_version,
            instance_locator,
            instance_status,
            source_package_instance_id,
            type_only,
        ) = row?;
        let Some(from_path) = files.get(&from_id) else {
            continue;
        };
        let dependency_instance = if package_origin.as_deref() == Some("dependency") {
            match (
                package_instance_id,
                instance_name.as_deref(),
                instance_locator.as_deref(),
            ) {
                (Some(id), Some(name), Some(locator)) => Some((
                    id,
                    name,
                    package_instance_key(name, instance_version.as_deref(), locator),
                )),
                _ => None,
            }
        } else {
            None
        };
        let crosses_dependency_boundary = dependency_instance
            .as_ref()
            .is_some_and(|(id, _, _)| source_package_instance_id != Some(*id));

        if crosses_dependency_boundary
            && let Some((instance_id, name, hub)) = dependency_instance.as_ref()
        {
            if package_hubs.insert(hub.clone()) {
                insert_node.execute(params![
                    hub,
                    "package",
                    "package_instances",
                    instance_id,
                    format!(
                        "{}@{}",
                        name,
                        instance_version.as_deref().unwrap_or("unknown")
                    ),
                    Option::<i64>::None,
                    Option::<i64>::None,
                    json!({
                        "origin": "dependency",
                        "package": name,
                        "version": instance_version,
                        "locator": instance_locator,
                        "status": instance_status,
                    })
                    .to_string(),
                ])?;
            }
            if let Some(to_id) = to_id
                && let Some(to_path) = files.get(&to_id)
                && contained_modules.insert((hub.clone(), to_id))
            {
                insert_edge.execute(params![
                    hub,
                    file_key(to_path),
                    "contains_module",
                    "certain",
                    "dependency-index",
                    Option::<i64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    json!({ "packageInstanceId": instance_id }).to_string(),
                ])?;
            }
        }
        let destination = if let Some(to_id) = to_id {
            let Some(to_path) = files.get(&to_id) else {
                continue;
            };
            file_key(to_path)
        } else if let Some((_, _, hub)) = dependency_instance.as_ref() {
            hub.clone()
        } else if let Some(package) = package {
            let hub = package_key(&package);
            if package_hubs.insert(hub.clone()) {
                insert_node.execute(params![
                    hub,
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
        // Heuristic workspace mappings (mirrored dist layouts, source-name
        // search) are honest leads, not proven links — never project them as
        // certain.
        let (confidence, provenance) = match (type_only, resolution.as_deref()) {
            (true, Some("workspace-inferred")) => ("likely", "type-workspace-inferred"),
            (true, Some("workspace")) => ("certain", "type-workspace"),
            (true, _) => ("certain", "type-resolver"),
            (false, Some("workspace-inferred")) => ("likely", "workspace-inferred"),
            (false, Some("workspace")) => ("certain", "workspace"),
            (false, _) => ("certain", "resolver"),
        };
        insert_edge.execute(params![
            file_key(from_path),
            destination,
            if type_only { "imports_types" } else { "import" },
            confidence,
            provenance,
            from_id,
            Option::<i64>::None,
            Option::<i64>::None,
            json!({ "request": request }).to_string(),
        ])?;
        if crosses_dependency_boundary
            && to_id.is_some()
            && let Some((instance_id, _, hub)) = dependency_instance.as_ref()
        {
            insert_edge.execute(params![
                file_key(from_path),
                hub,
                if type_only {
                    "imports_package_types"
                } else {
                    "imports_package"
                },
                confidence,
                provenance,
                from_id,
                Option::<i64>::None,
                Option::<i64>::None,
                json!({ "request": request, "packageInstanceId": instance_id }).to_string(),
            ])?;
        }
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
        "SELECT edge.from_file, edge.request, edge.package, package.origin,
                package.name, package.version, package.locator
         FROM module_edges edge
         LEFT JOIN package_instances package ON package.id=edge.package_instance_id
         WHERE edge.package IS NOT NULL",
    )?;
    let package_rows = package_stmt.query_map([], |r| {
        let package = r.get::<_, String>(2)?;
        let target = if r.get::<_, Option<String>>(3)?.as_deref() == Some("dependency") {
            match (
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ) {
                (Some(name), version, Some(locator)) => {
                    package_instance_key(&name, version.as_deref(), &locator)
                }
                _ => package_key(&package),
            }
        } else {
            package_key(&package)
        };
        Ok(((r.get::<_, i64>(0)?, r.get::<_, String>(1)?), target))
    })?;
    for row in package_rows {
        let (key, package) = row?;
        package_for.insert(key, package);
    }

    let mut stmt = conn.prepare(
        "SELECT reference.id, reference.file_id, reference.start, reference.line,
                reference.kind, reference.confidence, reference.target_request,
                reference.target_name, reference.local, reference.detail,
                member_call.rowid
         FROM refs reference
         LEFT JOIN member_calls member_call
           ON member_call.file_id=reference.file_id
          AND member_call.receiver_start=reference.start
          AND member_call.prop=reference.target_name
          AND reference.local=0
          AND reference.detail LIKE 'via namespace %'
         ORDER BY reference.file_id, reference.start, reference.id",
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
            r.get::<_, Option<i64>>(10)?,
        ))
    })?;
    for row in rows {
        let (
            id,
            file_id,
            start,
            line,
            kind,
            confidence,
            request,
            name,
            local,
            detail,
            member_call_id,
        ) = row?;
        let Some(path) = files.get(&file_id) else {
            continue;
        };
        let source = owner_at(symbols_by_file.get(&file_id), start)
            .map_or_else(|| file_key(path), |s| s.key.clone());

        // References that reach their target across a heuristically resolved
        // module edge (workspace-inferred) must not project as certain.
        let mut via_inferred = false;
        let targets = if local {
            projected_symbols(root_symbol.get(&(file_id, name.clone())))
        } else if let Some(request) = request.as_deref() {
            match graph.edge(file_id, request) {
                Some(target_file) if name == "*" => {
                    via_inferred = graph.edge_inferred(file_id, request);
                    graph
                        .paths
                        .get(&target_file)
                        .map(|path| ProjectedTargets::exact(file_key(path)))
                        .unwrap_or_default()
                }
                Some(target_file) => {
                    via_inferred = graph.edge_inferred(file_id, request);
                    let resolved = graph
                        .resolve_export_traced(target_file, &name)
                        .map(|(resolved_file, resolved_name, chain_inferred)| {
                            via_inferred |= chain_inferred;
                            (resolved_file, resolved_name)
                        })
                        .or_else(|| Some((target_file, name.clone())));
                    resolved.map_or_else(
                        ProjectedTargets::default,
                        |(resolved_file, resolved_name)| {
                            if resolved_name == "*" {
                                graph
                                    .paths
                                    .get(&resolved_file)
                                    .map(|path| ProjectedTargets::exact(file_key(path)))
                                    .unwrap_or_default()
                            } else {
                                projected_symbols(root_symbol.get(&(resolved_file, resolved_name)))
                            }
                        },
                    )
                }
                None => package_for
                    .get(&(file_id, request.to_string()))
                    .map(|package| ProjectedTargets::exact(package.clone()))
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
        } else if via_inferred && confidence == "certain" {
            "likely"
        } else {
            confidence.as_str()
        };
        let provenance = if targets.ambiguous {
            "semantic+resolver-candidate"
        } else if via_inferred {
            "semantic+resolver-inferred"
        } else {
            "semantic+resolver"
        };
        let mut edge_detail = json!({
            "request": &request,
            "targetName": &name,
            "detail": &detail,
            "ambiguousTarget": targets.ambiguous,
            "candidateCount": targets.keys.len(),
            "candidates": targets.ambiguous.then_some(&targets.keys),
        });
        // Namespace imports are the one member-call shape whose property
        // target the deterministic reference resolver already identifies
        // exactly. Carry the occurrence identity so checker planning can
        // avoid asking TypeScript the same question again.
        if let Some(member_call_id) = member_call_id {
            edge_detail["memberCallId"] = member_call_id.into();
        }
        let edge_detail = edge_detail.to_string();
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

fn project_entities(
    conn: &Connection,
    files: &HashMap<i64, String>,
    graph: &ModuleGraph,
    symbols: &[SymbolNode],
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    insert_node: &mut rusqlite::CachedStatement<'_>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut symbols_by_file: HashMap<i64, Vec<&SymbolNode>> = HashMap::new();
    for symbol in symbols {
        symbols_by_file
            .entry(symbol.file_id)
            .or_default()
            .push(symbol);
    }
    let sites = load_entity_sites(conn)?;
    let contract_catalog = ContractCatalog::build(conn, &sites, files)?;
    let mut inserted_nodes = HashSet::new();
    // Occurrence tables retain every evidence site. The disposable traversal
    // graph keeps one navigational edge per relationship so repeated writes in
    // one symbol do not inflate degree and ranking.
    let mut projected_edges: HashSet<(String, String, String)> = HashSet::new();
    let mut insert_entity = conn.prepare_cached(
        "INSERT INTO entities(
           entity_key, plane, entity_type, name, identity_anchor, meta_json
         ) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(entity_key) DO UPDATE SET
           plane=excluded.plane,
           entity_type=excluded.entity_type,
           name=excluded.name,
           identity_anchor=excluded.identity_anchor,
           meta_json=excluded.meta_json",
    )?;
    let mut insert_occurrence = conn.prepare_cached(
        "INSERT INTO entity_occurrences(
           entity_id, site_id, file_id, chunk_id, start, end, line, end_line,
           role, extractor, provenance, confidence, detail_json
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
    )?;
    let mut insert_entity_edge = conn.prepare_cached(
        "INSERT INTO entity_edges(
           occurrence_id, target_key, kind, confidence, provenance, detail_json
         ) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;

    for site in sites {
        let Some(path) = files.get(&site.file_id) else {
            continue;
        };
        if site.plane == "contract" {
            project_contract_site(
                conn,
                &site,
                path,
                graph,
                root_symbol,
                &symbols_by_file,
                &contract_catalog,
                &mut inserted_nodes,
                &mut insert_entity,
                &mut insert_occurrence,
                &mut insert_entity_edge,
                insert_node,
                insert_edge,
            )?;
            continue;
        }
        let identity = resolve_entity_identity(conn, &site, path, graph, root_symbol)?;
        let occurrence_confidence = lower_confidence(
            &site.confidence,
            if identity.ambiguous {
                "possible"
            } else {
                &site.confidence
            },
        );
        let entity_key = match site.identity_kind.as_str() {
            "literal" => entity_key(&site.entity_type, &site.identity_name),
            _ => reference_entity_key(&site.entity_type, &identity.keys, path, &site.identity_name),
        };
        let identity_anchor = (identity.keys.len() == 1).then(|| identity.keys[0].clone());
        let entity_meta = json!({
            "plane": site.plane,
            "type": site.entity_type,
            "identityKind": site.identity_kind,
            "identityAnchor": identity_anchor,
            "identityCandidates": identity.ambiguous.then_some(&identity.keys),
        });
        insert_entity.execute(params![
            entity_key,
            site.plane,
            site.entity_type,
            site.identity_name,
            identity_anchor,
            entity_meta.to_string(),
        ])?;
        let entity_id: i64 = conn.query_row(
            "SELECT id FROM entities WHERE entity_key=?1",
            [&entity_key],
            |row| row.get(0),
        )?;
        if inserted_nodes.insert(entity_key.clone()) {
            insert_node.execute(params![
                entity_key,
                "entity",
                "entities",
                entity_id,
                site.identity_name,
                Option::<i64>::None,
                Option::<i64>::None,
                entity_meta.to_string(),
            ])?;
        }

        let occurrence_detail = json!({
            "site": site.detail,
            "identityKind": site.identity_kind,
            "identityName": site.identity_name,
            "identityAnchor": identity_anchor,
            "identityCandidates": identity.ambiguous.then_some(&identity.keys),
        });
        insert_occurrence.execute(params![
            entity_id,
            site.id,
            site.file_id,
            site.chunk_id,
            site.start,
            site.end,
            site.line,
            site.end_line,
            site.role,
            site.extractor,
            site.provenance,
            occurrence_confidence,
            occurrence_detail.to_string(),
        ])?;
        let occurrence_id = conn.last_insert_rowid();
        let source = site_source_symbol(symbols_by_file.get(&site.file_id), &site)
            .cloned()
            .unwrap_or_else(|| file_key(path));
        let graph_detail = json!({
            "entityOccurrenceId": occurrence_id,
            "entitySiteId": site.id,
            "extractor": site.extractor,
            "role": site.role,
            "evidence": {
                "start": site.start,
                "end": site.end,
                "endLine": site.end_line,
            },
            "detail": site.detail,
        });

        match site.role.as_str() {
            "dispatch_site" | "lifecycle_producer" | "job_producer" | "injection_site"
            | "graphql_operation" | "environment_read" | "config_read" | "database_read"
            | "database_write" | "database_acquire" | "feature_flag_check"
            | "external_host_call" => {
                let kind = match site.role.as_str() {
                    "dispatch_site" => "dispatches",
                    "lifecycle_producer" => "produces_lifecycle",
                    "job_producer" => "produces_job",
                    "injection_site" => "injects",
                    "graphql_operation" => "invokes_graphql",
                    "environment_read" => "reads_env",
                    "config_read" => "reads_config",
                    "database_read" => "reads_resource",
                    "database_write" => "writes_resource",
                    "database_acquire" => "acquires_resource",
                    "feature_flag_check" => "checks_flag",
                    _ => "calls_host",
                };
                insert_entity_edge.execute(params![
                    occurrence_id,
                    entity_key,
                    kind,
                    occurrence_confidence,
                    site.provenance,
                    graph_detail.to_string(),
                ])?;
                if projected_edges.insert((source.clone(), entity_key.clone(), kind.to_string())) {
                    insert_edge.execute(params![
                        source,
                        entity_key,
                        kind,
                        occurrence_confidence,
                        site.provenance,
                        site.file_id,
                        Option::<i64>::None,
                        site.line,
                        graph_detail.to_string(),
                    ])?;
                }
                if matches!(site.role.as_str(), "lifecycle_producer" | "job_producer") {
                    project_entity_callers(
                        conn,
                        &source,
                        &entity_key,
                        occurrence_id,
                        &occurrence_confidence,
                        if site.role == "lifecycle_producer" {
                            "produces_lifecycle_via"
                        } else {
                            "produces_job_via"
                        },
                        insert_edge,
                        &mut projected_edges,
                    )?;
                }
            }
            "registered_handler" | "lifecycle_listener" | "job_handler" | "provider"
            | "route_handler" | "graphql_handler" => {
                let mut targets = resolve_entity_target(conn, &site, graph, root_symbol)?;
                let target_fallback = targets.keys.is_empty();
                if target_fallback
                    && matches!(site.role.as_str(), "route_handler" | "graphql_handler")
                    && !matches!(
                        site.extractor.as_str(),
                        "http-route-decorator" | "graphql-operation-decorator"
                    )
                {
                    // Inline or otherwise unresolved API-call handlers have no
                    // honest symbol target. Preserve the entity occurrence but
                    // do not fabricate a handler edge to the containing file or
                    // the next declaration.
                    continue;
                }
                if target_fallback {
                    targets = ProjectedTargets::exact(source.clone());
                }
                let edge_provenance = if target_fallback {
                    if site.role == "provider" {
                        "provider-site-fallback"
                    } else {
                        "registration-site-fallback"
                    }
                } else {
                    &site.provenance
                };
                let mut edge_detail = graph_detail.clone();
                if target_fallback {
                    edge_detail["targetResolution"] = json!("site-fallback");
                }
                let edge_confidence = lower_confidence(
                    &occurrence_confidence,
                    if targets.ambiguous {
                        "possible"
                    } else {
                        &occurrence_confidence
                    },
                );
                let kind = match site.role.as_str() {
                    "registered_handler" => "registered_handler",
                    "lifecycle_listener" => "lifecycle_listener",
                    "job_handler" => "job_handler",
                    "provider" => "provides",
                    "route_handler" => "handles_route",
                    _ => "handles_graphql",
                };
                for target in &targets.keys {
                    insert_entity_edge.execute(params![
                        occurrence_id,
                        target,
                        kind,
                        edge_confidence,
                        edge_provenance,
                        edge_detail.to_string(),
                    ])?;
                    if projected_edges.insert((
                        entity_key.clone(),
                        target.clone(),
                        kind.to_string(),
                    )) {
                        insert_edge.execute(params![
                            entity_key,
                            target,
                            kind,
                            edge_confidence,
                            edge_provenance,
                            site.file_id,
                            Option::<i64>::None,
                            site.line,
                            edge_detail.to_string(),
                        ])?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

impl ContractCatalog {
    fn build(
        conn: &Connection,
        sites: &[EntitySiteNode],
        files: &HashMap<i64, String>,
    ) -> Result<Self> {
        let mut catalog = Self::default();
        for site in sites {
            if site.plane != "contract" || site.role != "contract_declaration" {
                continue;
            }
            let Some(path) = files.get(&site.file_id) else {
                continue;
            };
            let definition = ContractDefinition {
                site_id: site.id,
                file_id: site.file_id,
                start: site.start,
                end: site.end,
                line: site.line,
                entity_type: site.entity_type.clone(),
                name: site.identity_name.clone(),
                key: contract_definition_key(path, &site.entity_type, &site.identity_name),
            };
            catalog.by_site.insert(site.id, definition.clone());
            catalog
                .by_local
                .entry((site.file_id, site.identity_name.clone()))
                .or_default()
                .push(definition.clone());
            catalog
                .by_file
                .entry(site.file_id)
                .or_default()
                .push(definition);
        }
        let mut stmt = conn.prepare(
            "SELECT file_id, local_name, imported_name, request
             FROM contract_imports ORDER BY file_id, local_name, request, imported_name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (file_id, local, imported, request) = row?;
            catalog
                .imports
                .entry((file_id, local))
                .or_default()
                .push((imported, request));
        }
        Ok(catalog)
    }

    fn local(&self, file_id: i64, name: &str) -> Vec<ContractTarget> {
        self.by_local
            .get(&(file_id, name.to_string()))
            .into_iter()
            .flatten()
            .map(contract_target_from_definition)
            .collect()
    }

    fn enclosing(&self, file_id: i64, offset: i64, excluded_site: i64) -> Option<&str> {
        self.by_file
            .get(&file_id)?
            .iter()
            .filter(|definition| {
                definition.site_id != excluded_site
                    && definition.start <= offset
                    && offset < definition.end
            })
            .min_by_key(|definition| definition.end - definition.start)
            .map(|definition| definition.key.as_str())
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "projection borrows caller-owned indexes, dedupe state, and five prepared statements"
)]
fn project_contract_site(
    conn: &Connection,
    site: &EntitySiteNode,
    path: &str,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_file: &HashMap<i64, Vec<&SymbolNode>>,
    catalog: &ContractCatalog,
    inserted_nodes: &mut HashSet<String>,
    insert_entity: &mut rusqlite::CachedStatement<'_>,
    insert_occurrence: &mut rusqlite::CachedStatement<'_>,
    insert_entity_edge: &mut rusqlite::CachedStatement<'_>,
    insert_node: &mut rusqlite::CachedStatement<'_>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let mut targets = resolve_contract_targets(site, graph, root_symbol, catalog);
    targets.sort_by(|left, right| left.key.cmp(&right.key));
    targets.dedup_by(|left, right| left.key == right.key);
    let ambiguous = targets.len() > 1;
    let inferred = targets.iter().any(|target| target.inferred);
    let confidence = lower_confidence(
        &site.confidence,
        if ambiguous {
            "possible"
        } else if inferred {
            "likely"
        } else {
            &site.confidence
        },
    );
    let backing_file = if targets.len() == 1 {
        targets.first()
    } else {
        None
    };
    let backing_file_id = backing_file.and_then(|target| target.file_id);
    let backing_line = backing_file.and_then(|target| target.line);
    let (entity_key, entity_type, display_name, identity_anchor) = match targets.as_slice() {
        [target] => (
            target.key.clone(),
            target.entity_type.clone(),
            target.name.clone(),
            target.identity_anchor.clone(),
        ),
        [] => (
            contract_reference_key(&site.entity_type, &[], path, &site.identity_name),
            site.entity_type.clone(),
            site.identity_name.clone(),
            None,
        ),
        candidates => (
            contract_reference_key(
                &site.entity_type,
                &candidates
                    .iter()
                    .map(|target| target.key.clone())
                    .collect::<Vec<_>>(),
                path,
                &site.identity_name,
            ),
            site.entity_type.clone(),
            site.identity_name.clone(),
            None,
        ),
    };
    let candidate_keys: Vec<&str> = targets.iter().map(|target| target.key.as_str()).collect();
    let entity_meta = json!({
        "plane": "contract",
        "type": entity_type,
        "identityKind": site.identity_kind,
        "identityAnchor": identity_anchor,
        "identityCandidates": ambiguous.then_some(&candidate_keys),
        "documentary": true,
    });
    insert_entity.execute(params![
        entity_key,
        "contract",
        entity_type,
        display_name,
        identity_anchor,
        entity_meta.to_string(),
    ])?;
    let entity_id: i64 = conn.query_row(
        "SELECT id FROM entities WHERE entity_key=?1",
        [&entity_key],
        |row| row.get(0),
    )?;
    if inserted_nodes.insert(entity_key.clone()) {
        insert_node.execute(params![
            entity_key,
            "contract",
            "entities",
            entity_id,
            display_name,
            backing_file_id,
            backing_line,
            entity_meta.to_string(),
        ])?;
    }

    let occurrence_detail = json!({
        "site": site.detail,
        "identityName": site.identity_name,
        "identityAnchor": identity_anchor,
        "identityCandidates": ambiguous.then_some(&candidate_keys),
        "documentary": true,
    });
    insert_occurrence.execute(params![
        entity_id,
        site.id,
        site.file_id,
        site.chunk_id,
        site.start,
        site.end,
        site.line,
        site.end_line,
        site.role,
        site.extractor,
        site.provenance,
        confidence,
        occurrence_detail.to_string(),
    ])?;
    let occurrence_id = conn.last_insert_rowid();
    let source = if site.role == "contract_declaration" {
        file_key(path)
    } else if site.role == "contract_reference" {
        catalog
            .enclosing(site.file_id, site.start, site.id)
            .map(str::to_string)
            .or_else(|| site_source_symbol(symbols_by_file.get(&site.file_id), site).cloned())
            .unwrap_or_else(|| file_key(path))
    } else {
        site_source_symbol(symbols_by_file.get(&site.file_id), site)
            .cloned()
            .unwrap_or_else(|| file_key(path))
    };
    let kind = match site.role.as_str() {
        "contract_declaration" => "declares_contract",
        "parameter_contract" => "accepts_contract",
        "return_contract" => "returns_contract",
        "decorator_use" => "decorated_by",
        _ => "references_contract",
    };
    let graph_detail = json!({
        "entityOccurrenceId": occurrence_id,
        "entitySiteId": site.id,
        "extractor": site.extractor,
        "role": site.role,
        "documentary": true,
        "evidence": {
            "start": site.start,
            "end": site.end,
            "endLine": site.end_line,
        },
        "detail": site.detail,
    });
    insert_entity_edge.execute(params![
        occurrence_id,
        entity_key,
        kind,
        confidence,
        site.provenance,
        graph_detail.to_string(),
    ])?;
    if source != entity_key {
        insert_edge.execute(params![
            source,
            entity_key,
            kind,
            confidence,
            site.provenance,
            site.file_id,
            Option::<i64>::None,
            site.line,
            graph_detail.to_string(),
        ])?;
    }
    Ok(())
}

fn site_source_symbol<'a>(
    symbols: Option<&Vec<&'a SymbolNode>>,
    site: &EntitySiteNode,
) -> Option<&'a String> {
    if let Some(owner) = owner_at(symbols, site.start) {
        return Some(&owner.key);
    }
    if !matches!(
        site.extractor.as_str(),
        "decorator-contract" | "http-route-decorator" | "graphql-operation-decorator"
    ) {
        return None;
    }
    // Decorators precede their class/method declaration, so they fall outside
    // the declaration span used by `owner_at`. Bound the forward association
    // to 512 bytes to avoid attaching a distant symbol; large decorator
    // payloads intentionally degrade to the containing file as their source.
    symbols?
        .iter()
        .copied()
        .filter(|symbol| symbol.decl_start >= site.start && symbol.decl_start - site.start <= 512)
        .min_by_key(|symbol| symbol.decl_start - site.start)
        .map(|symbol| &symbol.key)
}

fn resolve_contract_targets(
    site: &EntitySiteNode,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    catalog: &ContractCatalog,
) -> Vec<ContractTarget> {
    if site.role == "contract_declaration" {
        return catalog
            .by_site
            .get(&site.id)
            .map(contract_target_from_definition)
            .into_iter()
            .collect();
    }
    let (import_local, qualified_name) = site
        .identity_name
        .split_once('.')
        .map_or((site.identity_name.as_str(), None), |(local, name)| {
            (local, Some(name))
        });
    let imports = catalog
        .imports
        .get(&(site.file_id, import_local.to_string()))
        .cloned()
        .unwrap_or_default();
    let mut targets = Vec::new();
    for (imported, request) in imports {
        let target_name = if imported == "*" {
            qualified_name.unwrap_or("*")
        } else {
            imported.as_str()
        };
        let Some(target_file) = graph.edge(site.file_id, &request) else {
            let external = is_external_module_request(&request);
            targets.push(ContractTarget {
                key: if external {
                    contract_external_key(&site.entity_type, &request, target_name)
                } else {
                    contract_unresolved_key(&site.entity_type, &request, target_name)
                },
                entity_type: site.entity_type.clone(),
                name: target_name.to_string(),
                identity_anchor: external.then(|| format!("pkg:{request}#{target_name}")),
                inferred: false,
                file_id: None,
                line: None,
            });
            continue;
        };
        let (resolved_file, resolved_name, inferred) = graph
            .resolve_contract_export_traced(target_file, target_name)
            .unwrap_or((target_file, target_name.to_string(), false));
        let mut resolved = catalog.local(resolved_file, &resolved_name);
        if resolved.is_empty() {
            resolved.extend(contract_symbol_targets(
                root_symbol.get(&(resolved_file, resolved_name.clone())),
                &resolved_name,
                if site.entity_type == "decorator" {
                    "decorator"
                } else {
                    "class"
                },
            ));
        }
        for target in &mut resolved {
            target.inferred |= inferred || graph.edge_inferred(site.file_id, &request);
        }
        targets.extend(resolved);
    }
    if targets.is_empty() {
        targets.extend(catalog.local(site.file_id, &site.identity_name));
    }
    if targets.is_empty() {
        targets.extend(contract_symbol_targets(
            root_symbol.get(&(site.file_id, site.identity_name.clone())),
            &site.identity_name,
            if site.entity_type == "decorator" {
                "decorator"
            } else {
                "class"
            },
        ));
    }
    targets
}

fn contract_target_from_definition(definition: &ContractDefinition) -> ContractTarget {
    ContractTarget {
        key: definition.key.clone(),
        entity_type: definition.entity_type.clone(),
        name: definition.name.clone(),
        identity_anchor: Some(definition.key.clone()),
        inferred: false,
        file_id: Some(definition.file_id),
        line: Some(definition.line),
    }
}

fn contract_symbol_targets(
    symbols: Option<&Vec<&SymbolNode>>,
    name: &str,
    entity_type: &str,
) -> Vec<ContractTarget> {
    symbols
        .into_iter()
        .flatten()
        .map(|symbol| contract_target_from_anchor(entity_type, name, symbol))
        .collect()
}

fn contract_target_from_anchor(
    entity_type: &str,
    name: &str,
    symbol: &SymbolNode,
) -> ContractTarget {
    let digest = blake3::hash(symbol.key.as_bytes()).to_hex();
    ContractTarget {
        key: format!("contract:{entity_type}:ref-{}", &digest[..16]),
        entity_type: entity_type.to_string(),
        name: name.to_string(),
        identity_anchor: Some(symbol.key.clone()),
        inferred: false,
        file_id: Some(symbol.file_id),
        line: Some(symbol.line),
    }
}

fn load_entity_sites(conn: &Connection) -> Result<Vec<EntitySiteNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, chunk_id, start, end, line, end_line, plane,
                entity_type, role, identity_kind, identity_name, identity_start,
                target_name, target_start, extractor, provenance, confidence,
                detail_json
         FROM entity_sites ORDER BY entity_type, identity_name, file_id, start, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let detail: String = row.get(18)?;
        Ok(EntitySiteNode {
            id: row.get(0)?,
            file_id: row.get(1)?,
            chunk_id: row.get(2)?,
            start: row.get(3)?,
            end: row.get(4)?,
            line: row.get(5)?,
            end_line: row.get(6)?,
            plane: row.get(7)?,
            entity_type: row.get(8)?,
            role: row.get(9)?,
            identity_kind: row.get(10)?,
            identity_name: row.get(11)?,
            identity_start: row.get(12)?,
            target_name: row.get(13)?,
            target_start: row.get(14)?,
            extractor: row.get(15)?,
            provenance: row.get(16)?,
            confidence: row.get(17)?,
            detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn resolve_entity_identity(
    conn: &Connection,
    site: &EntitySiteNode,
    path: &str,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
) -> Result<ProjectedTargets> {
    if site.identity_kind == "literal" {
        return Ok(ProjectedTargets::default());
    }
    let targets = resolve_reference_at(
        conn,
        graph,
        root_symbol,
        site.file_id,
        site.identity_start,
        &site.identity_name,
    )?;
    if targets.keys.is_empty() {
        Ok(ProjectedTargets::exact(format!(
            "file:{path}#unresolved::{}",
            site.identity_name
        )))
    } else {
        Ok(targets)
    }
}

fn resolve_entity_target(
    conn: &Connection,
    site: &EntitySiteNode,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
) -> Result<ProjectedTargets> {
    let (Some(name), Some(start)) = (site.target_name.as_deref(), site.target_start) else {
        return Ok(ProjectedTargets::default());
    };
    resolve_reference_at(conn, graph, root_symbol, site.file_id, start, name)
}

fn resolve_reference_at(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    file_id: i64,
    start: i64,
    fallback_name: &str,
) -> Result<ProjectedTargets> {
    let reference = conn
        .query_row(
            "SELECT target_request, target_name, local
             FROM refs WHERE file_id=?1 AND start=?2
             ORDER BY id LIMIT 1",
            params![file_id, start],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?;
    let Some((request, name, local)) = reference else {
        return Ok(projected_symbols(
            root_symbol.get(&(file_id, fallback_name.to_string())),
        ));
    };
    if local {
        return Ok(projected_symbols(root_symbol.get(&(file_id, name))));
    }
    let Some(request) = request else {
        return Ok(ProjectedTargets::default());
    };
    let Some(target_file) = graph.edge(file_id, &request) else {
        return Ok(ProjectedTargets::default());
    };
    let (resolved_file, resolved_name) = graph
        .resolve_export(target_file, &name)
        .unwrap_or((target_file, name));
    Ok(projected_symbols(
        root_symbol.get(&(resolved_file, resolved_name)),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "caller projection keeps occurrence provenance and shared edge dedupe state explicit"
)]
fn project_entity_callers(
    conn: &Connection,
    producer: &str,
    entity_key: &str,
    occurrence_id: i64,
    occurrence_confidence: &str,
    via_kind: &str,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
    projected_edges: &mut HashSet<(String, String, String)>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT src_key, confidence, source_file_id, line
         FROM resolved_edges
         WHERE dst_key=?1 AND kind='call'
         ORDER BY src_key, id",
    )?;
    let rows = stmt.query_map([producer], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    for row in rows {
        let (caller, call_confidence, source_file_id, line) = row?;
        let confidence = lower_confidence(occurrence_confidence, &call_confidence);
        if !projected_edges.insert((caller.clone(), entity_key.to_string(), via_kind.to_string())) {
            continue;
        }
        insert_edge.execute(params![
            caller,
            entity_key,
            via_kind,
            confidence,
            "entity-boundary-collapse",
            source_file_id,
            Option::<i64>::None,
            line,
            json!({
                "entityOccurrenceId": occurrence_id,
                "producer": producer,
            })
            .to_string(),
        ])?;
    }
    Ok(())
}

fn lower_confidence(left: &str, right: &str) -> String {
    let rank = |confidence: &str| match confidence {
        "certain" => 2,
        "likely" => 1,
        _ => 0,
    };
    if rank(left) <= rank(right) {
        left
    } else {
        right
    }
    .to_string()
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
        let candidates: &[&SymbolNode] =
            candidates_by_name.get(&property).map_or(&[], Vec::as_slice);
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
            .map_or_else(|| file_key(path), |symbol| symbol.key.clone());
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

#[derive(Debug)]
struct CheckerProjection {
    source: String,
    target: String,
    file_id: i64,
    line: i64,
    member_call_id: i64,
    call_start: i64,
    call_end: i64,
    receiver_start: i64,
    receiver_end: i64,
    property_start: i64,
    property_end: i64,
    confidence: String,
    projects: BTreeSet<String>,
    unknown_projects: BTreeSet<String>,
    failed_projects: BTreeSet<String>,
    receiver_types: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct CheckerOccurrenceCoverage {
    unknown_projects: BTreeSet<String>,
    failed_projects: BTreeSet<String>,
}

fn checker_occurrence_coverage(
    conn: &Connection,
    snapshot: &str,
) -> Result<HashMap<i64, CheckerOccurrenceCoverage>> {
    let mut statement = conn.prepare(
        "SELECT project.member_call_id, project.project_id, project.status,
                run.status
         FROM checker_occurrence_projects project
         JOIN checker_enrichment_batches batch
           ON batch.id=project.batch_id AND batch.active=1
          AND batch.source_snapshot=?1
         JOIN checker_project_runs run
           ON run.batch_id=project.batch_id AND run.project_id=project.project_id
         JOIN files source
           ON source.path=project.source_file AND source.hash=project.source_hash
         JOIN member_calls call
           ON call.rowid=project.member_call_id
          AND call.file_id=source.id
          AND call.start=project.call_start AND call.end=project.call_end
          AND call.receiver_start=project.receiver_start
          AND call.receiver_end=project.receiver_end
          AND call.property_start=project.property_start
          AND call.property_end=project.property_end
         ORDER BY project.member_call_id, project.project_id",
    )?;
    let rows = statement.query_map([snapshot], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut coverage = HashMap::new();
    for row in rows {
        let (member_call_id, project, status, run_status) = row?;
        let entry = coverage
            .entry(member_call_id)
            .or_insert_with(CheckerOccurrenceCoverage::default);
        if run_status == "failed" || status == "failed" {
            entry.failed_projects.insert(project);
        } else if status == "unknown" {
            entry.unknown_projects.insert(project);
        }
    }
    Ok(coverage)
}

/// Recreate the targeted `checker` edges of the active batch, fact by fact.
///
/// A checker batch belongs to exactly one structural snapshot. Within that
/// snapshot, source occurrence and target fingerprints are still checked
/// defensively. A different snapshot never projects: manual indexing drops
/// it, while watch may keep it hidden only long enough to construct a newly
/// rebound batch for the current snapshot.
fn project_checker_enrichments(
    conn: &Connection,
    files: &HashMap<i64, String>,
    symbols: &[SymbolNode],
    snapshot: &str,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let coverage = checker_occurrence_coverage(conn, snapshot)?;
    // `resolved_edges` can be much larger than the checker fact set. Scan it
    // once instead of running an unindexed correlated lookup for every fact.
    let value_flow_resolved = conn
        .prepare(
            "SELECT source_ref_id FROM resolved_edges
             WHERE provenance='receiver-value-flow' AND confidence='likely'
               AND source_ref_id IS NOT NULL",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    let mut symbols_by_file: HashMap<i64, Vec<&SymbolNode>> = HashMap::new();
    for symbol in symbols {
        symbols_by_file
            .entry(symbol.file_id)
            .or_default()
            .push(symbol);
    }
    let mut statement = conn.prepare(
        "SELECT enrichment.member_call_id, call.rowid, source.id, source.path,
                call.line, enrichment.call_start, enrichment.call_end,
                enrichment.receiver_start, enrichment.receiver_end,
                enrichment.property_start, enrichment.property_end,
                enrichment.project_id, enrichment.receiver_type,
                enrichment.target_anchor, enrichment.target_fingerprint,
                enrichment.confidence,
                target_file.hash, target_symbol.decl_start, target_symbol.decl_end
         FROM checker_enrichments enrichment
         JOIN checker_enrichment_batches batch
           ON batch.id=enrichment.batch_id AND batch.active=1
          AND batch.source_snapshot=?1
         JOIN checker_project_runs run
           ON run.batch_id=enrichment.batch_id
          AND run.project_id=enrichment.project_id
          AND run.status IN ('completed','partial')
         JOIN files source
           ON source.path=enrichment.source_file AND source.hash=enrichment.source_hash
         JOIN member_calls call
           ON call.rowid=enrichment.member_call_id
          AND call.file_id=source.id
          AND call.start=enrichment.call_start AND call.end=enrichment.call_end
          AND call.receiver_start=enrichment.receiver_start
          AND call.receiver_end=enrichment.receiver_end
          AND call.property_start=enrichment.property_start
          AND call.property_end=enrichment.property_end
         JOIN graph_nodes target ON target.node_key=enrichment.target_anchor
         JOIN symbols target_symbol
           ON target.native_table='symbols' AND target.native_id=target_symbol.id
         JOIN files target_file ON target_file.id=target_symbol.file_id
         ORDER BY enrichment.member_call_id, enrichment.target_anchor, enrichment.project_id",
    )?;
    let rows = statement.query_map([snapshot], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, String>(16)?,
            row.get::<_, i64>(17)?,
            row.get::<_, i64>(18)?,
        ))
    })?;
    let mut projected: BTreeMap<(i64, String), CheckerProjection> = BTreeMap::new();
    for row in rows {
        let (
            enrichment_member_call_id,
            member_call_id,
            file_id,
            path,
            line,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
            project,
            receiver_type,
            target,
            fingerprint,
            confidence,
            target_hash,
            target_start,
            target_end,
        ) = row?;
        if value_flow_resolved.contains(&member_call_id) {
            continue;
        }
        let Some(occurrence_coverage) = coverage.get(&enrichment_member_call_id) else {
            continue;
        };
        if crate::checker::target_fingerprint(&target, &target_hash, target_start, target_end)
            != fingerprint
        {
            continue;
        }
        let Some(current_path) = files.get(&file_id) else {
            continue;
        };
        if current_path != &path {
            continue;
        }
        let source = owner_at(symbols_by_file.get(&file_id), call_start)
            .map_or_else(|| file_key(&path), |symbol| symbol.key.clone());
        let projection = projected
            .entry((member_call_id, target.clone()))
            .or_insert_with(|| CheckerProjection {
                source,
                target,
                file_id,
                line,
                member_call_id,
                call_start,
                call_end,
                receiver_start,
                receiver_end,
                property_start,
                property_end,
                confidence: confidence.clone(),
                projects: BTreeSet::new(),
                unknown_projects: occurrence_coverage.unknown_projects.clone(),
                failed_projects: occurrence_coverage.failed_projects.clone(),
                receiver_types: BTreeSet::new(),
            });
        projection.projects.insert(project);
        if let Some(receiver_type) = receiver_type {
            projection.receiver_types.insert(receiver_type);
        }
        if confidence == "possible" {
            projection.confidence = "possible".into();
        }
        if !projection.failed_projects.is_empty() {
            projection.confidence = "possible".into();
        }
    }
    let candidate_counts = projected.keys().fold(
        BTreeMap::<i64, usize>::new(),
        |mut counts, (occurrence, _)| {
            *counts.entry(*occurrence).or_default() += 1;
            counts
        },
    );
    for projection in projected.into_values() {
        let candidate_count = candidate_counts
            .get(&projection.member_call_id)
            .copied()
            .unwrap_or(1);
        insert_edge.execute(params![
            projection.source,
            projection.target,
            "member_call",
            projection.confidence,
            "checker",
            projection.file_id,
            projection.member_call_id,
            projection.line,
            json!({
                "memberCallId": projection.member_call_id,
                "call": [projection.call_start, projection.call_end],
                "receiver": [projection.receiver_start, projection.receiver_end],
                "property": [projection.property_start, projection.property_end],
                "projects": projection.projects,
                "unknownProjects": projection.unknown_projects,
                "failedProjects": projection.failed_projects,
                "receiverTypes": projection.receiver_types,
                "candidateCount": candidate_count,
                "occurrenceSpecific": true
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
    file_role::validate_all(&options.file_roles)?;
    origin::validate_all(&options.file_origins)?;
    if options.node_limit == 0 || options.edge_limit == 0 {
        bail!("node and edge limits must be greater than zero");
    }
    let snapshot = current_snapshot(conn)?;
    let allowed_file_origins: HashSet<&str> =
        options.file_origins.iter().map(String::as_str).collect();
    let (resolved_anchor, anchor_status) = resolve_anchor(
        conn,
        anchor,
        options.expected_snapshot.as_deref(),
        &snapshot,
        &allowed_file_origins,
    )?;
    let allowed_kinds: HashSet<&str> = options.kinds.iter().map(String::as_str).collect();
    let allowed_file_roles: HashSet<&str> = options.file_roles.iter().map(String::as_str).collect();
    let min_rank = confidence_rank(&options.min_confidence).unwrap();
    let mut discovered = HashSet::from([resolved_anchor.clone()]);
    let mut node_relevance = HashMap::from([(resolved_anchor.clone(), 1.0_f64)]);
    let mut expanded = HashSet::from([resolved_anchor.clone()]);
    let mut frontier = BinaryHeap::new();
    let mut edges_by_id: HashMap<i64, (String, GraphEdge)> = HashMap::new();
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
            1.0,
            options,
            min_rank,
            &allowed_kinds,
            &allowed_file_roles,
            &allowed_file_origins,
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
        edges_by_id.insert(step.edge_id, (step.detail_key, step.edge));

        if step.depth < options.depth && expanded.insert(step.other.clone()) {
            enqueue_ranked_steps(
                conn,
                &step.other,
                step.depth,
                step.confidence_floor,
                step.relation_floor,
                step.hub_floor,
                step.role_floor,
                options,
                min_rank,
                &allowed_kinds,
                &allowed_file_roles,
                &allowed_file_origins,
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
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut edges = edges_by_id
        .into_iter()
        .map(|(edge_id, (detail_key, edge))| OrderedGraphEdge {
            edge_id,
            detail_key,
            edge,
        })
        .collect::<Vec<_>>();
    edges.sort_by(|a, b| {
        b.edge
            .relevance
            .total_cmp(&a.edge.relevance)
            .then_with(|| graph_edge_content_cmp(&a.edge, &a.detail_key, &b.edge, &b.detail_key))
            .then_with(|| a.edge_id.cmp(&b.edge_id))
    });
    let edges = edges.into_iter().map(|entry| entry.edge).collect();
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

/// Traverse the code/runtime plane in logical workflow hops. Direct code edges
/// cost one hop. A complementary producer/entity/consumer handoff also costs
/// one hop even though it is represented by two physical graph edges.
pub fn workflow_neighborhood(
    conn: &Connection,
    anchor: &str,
    depth: usize,
    node_limit: usize,
    edge_limit: usize,
    file_origins: &[String],
) -> Result<WorkflowNeighborhood> {
    if depth == 0 || depth > 3 {
        bail!("workflow traversal depth must be between 1 and 3");
    }
    if node_limit == 0 || edge_limit == 0 {
        bail!("workflow traversal node and edge limits must be greater than zero");
    }
    origin::validate_all(file_origins)?;
    let allowed_origins: HashSet<&str> = file_origins.iter().map(String::as_str).collect();
    let root = load_node(conn, anchor)?
        .with_context(|| format!("missing workflow seed graph node {anchor}"))?;
    if root.kind != "symbol" {
        bail!("workflow seed must resolve to a symbol anchor");
    }

    let mut nodes = HashMap::from([(root.key.clone(), root)]);
    let mut relevance = HashMap::from([(anchor.to_string(), 1.0_f64)]);
    // The same node may be rediscovered at another depth through a stronger
    // runtime-crossing route. Keep depth in the expansion identity so that
    // route can propagate its score, while suppressing weaker repeats at the
    // same depth.
    let mut expanded: HashMap<(String, usize), f64> = HashMap::new();
    let mut direct_degree_cache = HashMap::new();
    let mut entity_degree_cache = HashMap::new();
    let mut frontier = BinaryHeap::from([WorkflowRankedNode {
        key: anchor.to_string(),
        depth: 0,
        confidence_floor: 1.0,
        relation_floor: 1.0,
        hub_floor: 1.0,
        crossed_runtime: false,
        score: 1.0,
    }]);
    let mut traversed_edges = 0;
    let mut truncated = false;

    'walk: while let Some(state) = frontier.pop() {
        let state_key = (state.key.clone(), state.depth);
        if expanded
            .get(&state_key)
            .is_some_and(|score| *score >= state.score)
        {
            continue;
        }
        expanded.insert(state_key, state.score);
        if state.depth >= depth {
            continue;
        }
        let steps = workflow_logical_steps(
            conn,
            &state.key,
            &allowed_origins,
            &mut direct_degree_cache,
            &mut entity_degree_cache,
        )?;
        for step in steps {
            if traversed_edges >= edge_limit {
                truncated = true;
                break 'walk;
            }
            traversed_edges += 1;
            let new_node = !nodes.contains_key(&step.other.key);
            if new_node && nodes.len() >= node_limit {
                truncated = true;
                continue;
            }
            let next_depth = state.depth + 1;
            let confidence_floor = state.confidence_floor.min(step.confidence_floor);
            let relation_floor = state.relation_floor.min(step.relation_floor);
            let hub_floor = state.hub_floor.min(step.hub_floor);
            let crossed_runtime = state.crossed_runtime || step.runtime_boundary;
            let score = round_score(
                confidence_floor * relation_floor * distance_decay(next_depth) * hub_floor
                    + if crossed_runtime { 1.0 } else { 0.0 },
            );
            relevance
                .entry(step.other.key.clone())
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
            nodes
                .entry(step.other.key.clone())
                .or_insert(step.other.clone());
            if !step.terminal {
                frontier.push(WorkflowRankedNode {
                    key: step.other.key,
                    depth: next_depth,
                    confidence_floor,
                    relation_floor,
                    hub_floor,
                    crossed_runtime,
                    score,
                });
            }
        }
    }

    let mut nodes = nodes
        .into_values()
        .map(|mut node| {
            node.relevance = relevance.get(&node.key).copied().unwrap_or(0.0);
            node
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(WorkflowNeighborhood {
        nodes,
        traversed_edges,
        truncated,
    })
}

fn workflow_logical_steps(
    conn: &Connection,
    node: &str,
    allowed_origins: &HashSet<&str>,
    direct_degree_cache: &mut HashMap<String, usize>,
    entity_degree_cache: &mut HashMap<String, usize>,
) -> Result<Vec<WorkflowLogicalStep>> {
    let mut stmt = conn.prepare_cached(
        "SELECT src_key, dst_key, kind, confidence
         FROM resolved_edges
         WHERE src_key=?1 OR dst_key=?1",
    )?;
    let incident = stmt
        .query_map([node], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_target: HashMap<String, WorkflowLogicalStep> = HashMap::new();
    for (source, target, kind, confidence) in incident {
        if confidence_rank(&confidence).unwrap_or(0) < confidence_rank("likely").unwrap() {
            continue;
        }
        let other_key = if source == node { &target } else { &source };
        let Some(other) = load_node(conn, other_key)? else {
            continue;
        };
        if workflow_direct_kind(&kind) && workflow_symbol_allowed(&other, allowed_origins) {
            let step = WorkflowLogicalStep {
                // Symbol degree includes documentary and file-projection edges
                // outside this workflow plane, so it is not a valid hub signal.
                hub_floor: 1.0,
                confidence_floor: confidence_weight(&confidence),
                relation_floor: relation_weight(&kind),
                terminal: cached_workflow_direct_degree(
                    conn,
                    &other.key,
                    allowed_origins,
                    direct_degree_cache,
                )? > WORKFLOW_HUB_DEGREE_LIMIT,
                other,
                runtime_boundary: false,
            };
            retain_stronger_workflow_step(&mut by_target, step);
            continue;
        }
        if let Some(family) = workflow_general_association_kind(&kind)
            && other.kind == "entity"
            && other.meta.get("plane").and_then(Value::as_str) == Some("general")
        {
            let entity_degree = cached_graph_degree(conn, &other.key, entity_degree_cache)?;
            collect_general_workflow_steps(
                conn,
                node,
                family,
                &kind,
                &confidence,
                &other,
                entity_degree,
                allowed_origins,
                &mut by_target,
            )?;
            continue;
        }
        let Some((family, side)) = workflow_runtime_boundary_kind(&kind) else {
            continue;
        };
        if other.kind != "entity"
            || other.meta.get("plane").and_then(Value::as_str) != Some("runtime")
        {
            continue;
        }
        // A high-degree DI token is commonly infrastructure wiring. Walking
        // from its provider side to every injection site would spend the
        // candidate budget on an inverse-usage fan-out. Keep the useful
        // consumer -> provider resolution, but suppress that reverse bridge.
        if family == "di"
            && !side
            && cached_graph_degree(conn, &other.key, entity_degree_cache)?
                > WORKFLOW_HUB_DEGREE_LIMIT
        {
            continue;
        }
        let mut entity_stmt = conn.prepare_cached(
            "SELECT src_key, dst_key, kind, confidence
             FROM resolved_edges
             WHERE src_key=?1 OR dst_key=?1",
        )?;
        let entity_edges = entity_stmt
            .query_map([&other.key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (peer_source, peer_target, peer_kind, peer_confidence) in entity_edges {
            let Some((peer_family, peer_side)) = workflow_runtime_boundary_kind(&peer_kind) else {
                continue;
            };
            if family != peer_family
                || side == peer_side
                || confidence_rank(&peer_confidence).unwrap_or(0)
                    < confidence_rank("likely").unwrap()
            {
                continue;
            }
            let peer_key = if peer_source == other.key {
                &peer_target
            } else {
                &peer_source
            };
            if peer_key == node {
                continue;
            }
            let Some(peer) = load_node(conn, peer_key)? else {
                continue;
            };
            if !workflow_symbol_allowed(&peer, allowed_origins) {
                continue;
            }
            let step = WorkflowLogicalStep {
                // A complementary runtime handoff is the primary workflow
                // signal. Confidence still gates eligibility and remains on
                // the underlying edges; candidate relevance prioritizes the
                // handoff ahead of ordinary depth-two helper fan-out.
                hub_floor: 1.0,
                confidence_floor: 1.0,
                relation_floor: 1.0,
                other: peer,
                runtime_boundary: true,
                terminal: false,
            };
            retain_stronger_workflow_step(&mut by_target, step);
        }
    }
    let mut steps = by_target.into_values().collect::<Vec<_>>();
    steps.sort_by(|left, right| {
        workflow_step_strength(right)
            .total_cmp(&workflow_step_strength(left))
            .then_with(|| left.other.key.cmp(&right.other.key))
    });
    Ok(steps)
}

#[expect(
    clippy::too_many_arguments,
    reason = "association expansion carries edge ranking inputs into a shared step accumulator"
)]
fn collect_general_workflow_steps(
    conn: &Connection,
    node: &str,
    family: &str,
    kind: &str,
    confidence: &str,
    entity: &GraphNode,
    entity_degree: usize,
    allowed_origins: &HashSet<&str>,
    steps: &mut HashMap<String, WorkflowLogicalStep>,
) -> Result<()> {
    // Shared configuration/data identities are associative clues, not hard
    // handoffs. High-degree identities are repository-wide hubs and do not
    // belong in a bounded workflow candidate set.
    if entity_degree > WORKFLOW_HUB_DEGREE_LIMIT {
        return Ok(());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT src_key, dst_key, kind, confidence
         FROM resolved_edges
         WHERE src_key=?1 OR dst_key=?1",
    )?;
    let peers = stmt
        .query_map([&entity.key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (source, target, peer_kind, peer_confidence) in peers {
        if workflow_general_association_kind(&peer_kind) != Some(family)
            || confidence_rank(&peer_confidence).unwrap_or(0) < confidence_rank("likely").unwrap()
        {
            continue;
        }
        let peer_key = if source == entity.key {
            &target
        } else {
            &source
        };
        if peer_key == node {
            continue;
        }
        let Some(peer) = load_node(conn, peer_key)? else {
            continue;
        };
        if !workflow_symbol_allowed(&peer, allowed_origins) {
            continue;
        }
        retain_stronger_workflow_step(
            steps,
            WorkflowLogicalStep {
                hub_floor: hub_damping(entity_degree),
                confidence_floor: confidence_weight(confidence)
                    .min(confidence_weight(&peer_confidence)),
                relation_floor: relation_weight(kind).min(relation_weight(&peer_kind)),
                other: peer,
                runtime_boundary: false,
                terminal: true,
            },
        );
    }
    Ok(())
}

fn retain_stronger_workflow_step(
    steps: &mut HashMap<String, WorkflowLogicalStep>,
    candidate: WorkflowLogicalStep,
) {
    match steps.get(&candidate.other.key) {
        Some(existing)
            if workflow_step_strength(existing) >= workflow_step_strength(&candidate) => {}
        _ => {
            steps.insert(candidate.other.key.clone(), candidate);
        }
    }
}

fn workflow_step_strength(step: &WorkflowLogicalStep) -> f64 {
    step.confidence_floor * step.relation_floor * step.hub_floor
        + if step.runtime_boundary { 1.0 } else { 0.0 }
}

fn workflow_symbol_allowed(node: &GraphNode, allowed_origins: &HashSet<&str>) -> bool {
    node.kind == "symbol"
        && node.file_role.as_deref() == Some("production")
        && node
            .file_origin
            .as_deref()
            .is_none_or(|value| allowed_origins.contains(value))
}

fn workflow_direct_degree(
    conn: &Connection,
    node: &str,
    allowed_origins: &HashSet<&str>,
) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "SELECT src_key, dst_key, kind, confidence
         FROM resolved_edges
         WHERE src_key=?1 OR dst_key=?1",
    )?;
    let incident = stmt.query_map([node], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut neighbors = HashSet::new();
    for row in incident {
        let (source, target, kind, confidence) = row?;
        if !workflow_direct_kind(&kind)
            || confidence_rank(&confidence).unwrap_or(0) < confidence_rank("likely").unwrap()
        {
            continue;
        }
        let other_key = if source == node { target } else { source };
        if let Some(other) = load_node(conn, &other_key)?
            && workflow_symbol_allowed(&other, allowed_origins)
        {
            neighbors.insert(other_key);
        }
    }
    Ok(neighbors.len())
}

fn cached_workflow_direct_degree(
    conn: &Connection,
    node: &str,
    allowed_origins: &HashSet<&str>,
    cache: &mut HashMap<String, usize>,
) -> Result<usize> {
    if let Some(degree) = cache.get(node) {
        return Ok(*degree);
    }
    let degree = workflow_direct_degree(conn, node, allowed_origins)?;
    cache.insert(node.to_string(), degree);
    Ok(degree)
}

fn cached_graph_degree(
    conn: &Connection,
    node: &str,
    cache: &mut HashMap<String, usize>,
) -> Result<usize> {
    if let Some(degree) = cache.get(node) {
        return Ok(*degree);
    }
    let degree = graph_degree(conn, node)?;
    cache.insert(node.to_string(), degree);
    Ok(degree)
}

fn workflow_direct_kind(kind: &str) -> bool {
    matches!(kind, "call" | "render" | "extend" | "member_call")
}

fn workflow_general_association_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "handles_graphql" | "invokes_graphql" => Some("graphql"),
        "reads_resource" | "writes_resource" | "acquires_resource" => Some("resource"),
        "reads_env" => Some("environment"),
        "reads_config" => Some("configuration"),
        "checks_flag" => Some("feature_flag"),
        "calls_host" => Some("external_host"),
        _ => None,
    }
}

/// Returns (boundary family, producer side). Complementary sides in the same
/// family form one logical workflow transition through the runtime entity.
fn workflow_runtime_boundary_kind(kind: &str) -> Option<(&'static str, bool)> {
    match kind {
        "dispatches" => Some(("registry", true)),
        "registered_handler" => Some(("registry", false)),
        "produces_lifecycle" | "produces_lifecycle_via" => Some(("lifecycle", true)),
        "lifecycle_listener" => Some(("lifecycle", false)),
        "produces_job" | "produces_job_via" => Some(("job", true)),
        "job_handler" => Some(("job", false)),
        "injects" => Some(("di", true)),
        "provides" => Some(("di", false)),
        _ => None,
    }
}

pub fn paths(conn: &Connection, from: &str, to: &str, options: &PathOptions) -> Result<PathSearch> {
    if options.max_depth == 0 || options.max_depth > 8 {
        bail!("path depth must be between 1 and 8");
    }
    if options.path_limit == 0 || options.path_limit > 50 {
        bail!("path limit must be between 1 and 50");
    }
    if options.node_limit == 0 || options.node_limit > MAX_PATH_NODE_LIMIT {
        bail!("path node limit must be between 1 and {MAX_PATH_NODE_LIMIT}");
    }
    if options.edge_limit == 0 || options.edge_limit > MAX_PATH_EDGE_LIMIT {
        bail!("path edge limit must be between 1 and {MAX_PATH_EDGE_LIMIT}");
    }
    origin::validate_all(&options.file_origins)?;
    file_role::validate_all(&options.file_roles)?;
    let allowed_origins: HashSet<&str> = options.file_origins.iter().map(String::as_str).collect();
    let snapshot = current_snapshot(conn)?;
    let (resolved_to, to_status) = resolve_anchor(
        conn,
        to,
        options.expected_snapshot.as_deref(),
        &snapshot,
        &allowed_origins,
    )?;
    let neighborhood = neighborhood(
        conn,
        from,
        &NeighborhoodOptions {
            expected_snapshot: options.expected_snapshot.clone(),
            depth: options.max_depth,
            direction: options.direction.clone(),
            node_limit: options.node_limit,
            edge_limit: options.edge_limit,
            min_confidence: options.min_confidence.clone(),
            kinds: options.kinds.clone(),
            file_roles: options.file_roles.clone(),
            file_origins: options.file_origins.clone(),
            penalize_file_roles: true,
        },
    )?;
    let mut node_by_key: HashMap<String, GraphNode> = neighborhood
        .nodes
        .iter()
        .cloned()
        .map(|node| (node.key.clone(), node))
        .collect();
    if !node_by_key.contains_key(&resolved_to)
        && let Some(node) = load_node(conn, &resolved_to)?
    {
        node_by_key.insert(resolved_to.clone(), node);
    }
    let mut adjacency: HashMap<String, Vec<(String, GraphEdge, bool)>> = HashMap::new();
    for edge in &neighborhood.edges {
        if options.direction != "in" {
            adjacency.entry(edge.source.clone()).or_default().push((
                edge.target.clone(),
                edge.clone(),
                false,
            ));
        }
        if options.direction != "out" {
            adjacency.entry(edge.target.clone()).or_default().push((
                edge.source.clone(),
                edge.clone(),
                true,
            ));
        }
    }
    for edges in adjacency.values_mut() {
        edges.sort_by(
            |(left_node, left, left_reversed), (right_node, right, right_reversed)| {
                right
                    .relevance
                    .total_cmp(&left.relevance)
                    .then_with(|| left_node.cmp(right_node))
                    .then_with(|| left.kind.cmp(&right.kind))
                    .then_with(|| left_reversed.cmp(right_reversed))
            },
        );
    }

    #[derive(Clone)]
    struct Candidate {
        nodes: Vec<String>,
        steps: Vec<PathStep>,
        score: f64,
    }

    let candidate_limit = options
        .path_limit
        .saturating_mul(32)
        .max(options.path_limit);
    let mut stack = vec![Candidate {
        nodes: vec![neighborhood.resolved_anchor.clone()],
        steps: Vec::new(),
        score: 1.0,
    }];
    let mut candidates = Vec::new();
    let mut enumeration_truncated = false;
    let mut searched_states = 0;
    while let Some(candidate) = stack.pop() {
        if searched_states >= MAX_PATH_SEARCH_STATES {
            enumeration_truncated = true;
            break;
        }
        searched_states += 1;
        let current = candidate.nodes.last().expect("path has a node");
        if current == &resolved_to {
            candidates.push(candidate);
            if candidates.len() >= candidate_limit {
                enumeration_truncated = true;
                break;
            }
            continue;
        }
        if candidate.steps.len() >= options.max_depth {
            continue;
        }
        let Some(edges) = adjacency.get(current) else {
            continue;
        };
        for (next, edge, reversed) in edges.iter().rev() {
            if candidate.nodes.contains(next) {
                continue;
            }
            let mut next_candidate = candidate.clone();
            next_candidate.nodes.push(next.clone());
            next_candidate.steps.push(PathStep {
                from: current.clone(),
                to: next.clone(),
                reversed: *reversed,
                edge: edge.clone(),
            });
            next_candidate.score = round_score(candidate.score.min(edge.relevance));
            stack.push(next_candidate);
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.steps.len().cmp(&right.steps.len()))
            .then_with(|| left.nodes.cmp(&right.nodes))
    });
    let matched_paths = candidates.len();
    let paths = candidates
        .into_iter()
        .take(options.path_limit)
        .map(|candidate| {
            let nodes = candidate
                .nodes
                .iter()
                .filter_map(|key| node_by_key.get(key).cloned())
                .collect();
            GraphPath {
                score: candidate.score,
                nodes,
                steps: candidate.steps,
            }
        })
        .collect();
    Ok(PathSearch {
        snapshot: neighborhood.snapshot,
        requested_from: from.to_string(),
        requested_to: to.to_string(),
        resolved_from: neighborhood.resolved_anchor,
        resolved_to,
        from_status: neighborhood.anchor_status,
        to_status,
        paths,
        searched_nodes: neighborhood.nodes.len(),
        searched_edges: neighborhood.edges.len(),
        searched_states,
        truncated: neighborhood.truncated
            || enumeration_truncated
            || matched_paths > options.path_limit,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "ranked traversal threads four score floors, filters, degree cache, and frontier"
)]
fn enqueue_ranked_steps(
    conn: &Connection,
    node: &str,
    depth: usize,
    confidence_floor: f64,
    relation_floor: f64,
    hub_floor: f64,
    role_floor: f64,
    options: &NeighborhoodOptions,
    min_rank: u8,
    allowed_kinds: &HashSet<&str>,
    allowed_file_roles: &HashSet<&str>,
    allowed_file_origins: &HashSet<&str>,
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
                    f.path, e.line, e.detail_json, other_file.role, other_file.origin
             FROM resolved_edges e
             LEFT JOIN files f ON e.source_file_id = f.id
             LEFT JOIN graph_nodes other ON other.node_key=e.dst_key
             LEFT JOIN files other_file ON other.file_id=other_file.id
             WHERE e.src_key = ?1"
        } else {
            "SELECT e.id, e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                    f.path, e.line, e.detail_json, other_file.role, other_file.origin
             FROM resolved_edges e
             LEFT JOIN files f ON e.source_file_id = f.id
             LEFT JOIN graph_nodes other ON other.node_key=e.src_key
             LEFT JOIN files other_file ON other.file_id=other_file.id
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
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        for row in rows {
            let (edge_id, mut edge, other_file_role, other_file_origin) = row?;
            if confidence_rank(&edge.confidence).unwrap_or(0) < min_rank
                || (!allowed_kinds.is_empty() && !allowed_kinds.contains(edge.kind.as_str()))
                || (!allowed_file_roles.is_empty()
                    && other_file_role
                        .as_deref()
                        .is_some_and(|role| !allowed_file_roles.contains(role)))
                || other_file_origin
                    .as_deref()
                    .is_some_and(|origin| !allowed_file_origins.contains(origin))
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
            let role_floor = if options.penalize_file_roles {
                role_floor.min(file_role::penalty(other_file_role.as_deref()))
            } else {
                role_floor
            };
            let next_depth = depth + 1;
            let score = round_score(
                confidence_floor
                    * relation_floor
                    * distance_decay(next_depth)
                    * hub_floor
                    * role_floor,
            );
            edge.relevance = score;
            let detail_key = edge.detail.to_string();
            frontier.push(RankedStep {
                edge_id,
                detail_key,
                edge,
                other,
                depth: next_depth,
                confidence_floor,
                relation_floor,
                hub_floor,
                role_floor,
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
        "call"
        | "render"
        | "extend"
        | "dispatches"
        | "registered_handler"
        | "produces_lifecycle"
        | "produces_lifecycle_via"
        | "lifecycle_listener"
        | "produces_job"
        | "produces_job_via"
        | "job_handler"
        | "injects"
        | "provides"
        | "handles_route"
        | "handles_graphql" => 1.0,
        "invokes_graphql" | "reads_resource" | "writes_resource" => 0.9,
        "acquires_resource" | "reads_env" | "reads_config" | "checks_flag" | "calls_host" => 0.8,
        "member_call" | "member_candidate" => 0.9,
        "import" | "reexport" => 0.75,
        "imports_types" | "imports_package_types" => 0.6,
        "decorated_by" => 0.7,
        "accepts_contract" | "returns_contract" | "references_contract" => 0.65,
        "declares_contract" => 0.55,
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

/// Resolve a user-facing anchor against the current structural snapshot for a
/// durable semantic support. Ambiguity remains an error; stored supports use
/// the returned exact node key. This write path deliberately considers every
/// indexed origin, unlike retrieval defaults, so a shorthand cannot silently
/// bind to first-party code when a dependency introduces the same symbol.
pub fn resolve_current_anchor(conn: &Connection, anchor: &str) -> Result<String> {
    let all = origin::ALL.iter().copied().collect();
    let snapshot = current_snapshot(conn)?;
    resolve_anchor(conn, anchor, None, &snapshot, &all).map(|(resolved, _)| resolved)
}

pub fn resolve_current_anchor_in_origins(
    conn: &Connection,
    anchor: &str,
    file_origins: &[String],
) -> Result<String> {
    origin::validate_all(file_origins)?;
    let allowed = file_origins.iter().map(String::as_str).collect();
    let snapshot = current_snapshot(conn)?;
    resolve_anchor(conn, anchor, None, &snapshot, &allowed).map(|(resolved, _)| resolved)
}

/// Resolve an exact or user-facing anchor against an explicitly expected
/// structural snapshot and origin boundary. Read surfaces use this to consume
/// copy-safe anchors returned by an earlier query without routing them through
/// a lossy `path:name` symbol lookup. A stale symbol anchor is re-resolved by
/// path, scope, and name using the same fail-closed ambiguity policy as
/// neighborhood traversal.
pub fn resolve_anchor_in_origins(
    conn: &Connection,
    anchor: &str,
    expected_snapshot: Option<&str>,
    file_origins: &[String],
) -> Result<(String, String)> {
    origin::validate_all(file_origins)?;
    let allowed = file_origins.iter().map(String::as_str).collect();
    let snapshot = current_snapshot(conn)?;
    resolve_anchor(conn, anchor, expected_snapshot, &snapshot, &allowed)
}

fn resolve_anchor(
    conn: &Connection,
    anchor: &str,
    expected_snapshot: Option<&str>,
    current_snapshot: &str,
    allowed_origins: &HashSet<&str>,
) -> Result<(String, String)> {
    let stale = expected_snapshot.is_some_and(|snapshot| snapshot != current_snapshot);
    if anchor.starts_with("sym:") && stale {
        let (path, scope, name, _) = parse_symbol_key(anchor)
            .with_context(|| format!("cannot re-resolve malformed stale anchor `{anchor}`"))?;
        let candidates = allowed_candidates(
            conn,
            symbol_candidates(conn, Some(&path), Some(&scope), &name)?,
            allowed_origins,
        )?;
        return unique_anchor(anchor, candidates, "re-resolved");
    }
    if graph_node_exists(conn, anchor)? {
        if !node_origin_allowed(conn, anchor, allowed_origins)? {
            bail!("anchor `{anchor}` is outside the requested file origins");
        }
        return Ok((
            anchor.to_string(),
            if stale { "re-resolved" } else { "exact" }.into(),
        ));
    }

    if let Some(path) = anchor.strip_prefix("file:") {
        let candidates = allowed_candidates(conn, file_candidates(conn, path)?, allowed_origins)?;
        return unique_anchor(
            anchor,
            candidates,
            if stale { "re-resolved" } else { "resolved" },
        );
    }
    if !anchor.contains(':') {
        let files = allowed_candidates(conn, file_candidates(conn, anchor)?, allowed_origins)?;
        if files.len() == 1 {
            return Ok((files[0].clone(), "resolved".into()));
        }
    }
    let (path, name) = anchor
        .rsplit_once(':')
        .map_or((None, anchor), |(path, name)| (Some(path), name));
    let candidates = allowed_candidates(
        conn,
        symbol_candidates(conn, path, None, name)?,
        allowed_origins,
    )?;
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

fn node_origin_allowed(
    conn: &Connection,
    key: &str,
    allowed_origins: &HashSet<&str>,
) -> Result<bool> {
    let origin = conn
        .query_row(
            "SELECT file.origin
             FROM graph_nodes node
             JOIN files file ON file.id=node.file_id
             WHERE node.node_key=?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(origin.is_none_or(|origin| allowed_origins.contains(origin.as_str())))
}

fn allowed_candidates(
    conn: &Connection,
    candidates: Vec<String>,
    allowed_origins: &HashSet<&str>,
) -> Result<Vec<String>> {
    candidates
        .into_iter()
        .filter_map(
            |candidate| match node_origin_allowed(conn, &candidate, allowed_origins) {
                Ok(true) => Some(Ok(candidate)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
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
        "SELECT g.node_key, g.node_kind, g.display_name, f.path, f.role, f.origin,
                g.line, g.meta_json
         FROM graph_nodes g LEFT JOIN files f ON g.file_id=f.id WHERE g.node_key=?1",
    )?;
    let node = stmt
        .query_row([key], |r| {
            let meta: String = r.get(7)?;
            Ok(GraphNode {
                key: r.get(0)?,
                kind: r.get(1)?,
                display_name: r.get(2)?,
                file: r.get(3)?,
                file_role: r.get(4)?,
                file_origin: r.get(5)?,
                line: r.get(6)?,
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

fn package_instance_key(name: &str, version: Option<&str>, locator: &str) -> String {
    let identity = format!("{name}\0{}\0{locator}", version.unwrap_or("unknown"));
    let digest = blake3::hash(identity.as_bytes()).to_hex();
    format!(
        "pkg:{name}@{}#{}",
        version.unwrap_or("unknown"),
        &digest[..8]
    )
}

fn event_key(name: &str) -> String {
    format!("event:unknown:{name}")
}

fn member_key(name: &str) -> String {
    format!("member:unknown:{name}")
}

fn entity_key(entity_type: &str, name: &str) -> String {
    format!("entity:{entity_type}:{}", encode_key_component(name))
}

fn contract_definition_key(path: &str, entity_type: &str, name: &str) -> String {
    format!(
        "contract:{entity_type}:{}#{}",
        encode_key_component(path),
        encode_key_component(name)
    )
}

fn contract_reference_key(
    entity_type: &str,
    targets: &[String],
    path: &str,
    fallback_name: &str,
) -> String {
    let identity = if targets.is_empty() {
        format!("{path}\0{fallback_name}")
    } else {
        targets.join("\0")
    };
    let digest = blake3::hash(identity.as_bytes()).to_hex();
    format!("contract:{entity_type}:ref-{}", &digest[..16])
}

fn contract_external_key(entity_type: &str, request: &str, name: &str) -> String {
    format!(
        "contract:{entity_type}:external:{}#{}",
        encode_key_component(request),
        encode_key_component(name)
    )
}

fn contract_unresolved_key(entity_type: &str, request: &str, name: &str) -> String {
    format!(
        "contract:{entity_type}:unresolved:{}#{}",
        encode_key_component(request),
        encode_key_component(name)
    )
}

fn is_external_module_request(request: &str) -> bool {
    !request.starts_with('.') && !request.starts_with('/')
}

fn reference_entity_key(
    entity_type: &str,
    targets: &[String],
    path: &str,
    fallback_name: &str,
) -> String {
    let identity = if targets.is_empty() {
        format!("{path}\0{fallback_name}")
    } else {
        targets.join("\0")
    };
    let digest = blake3::hash(identity.as_bytes()).to_hex();
    format!("entity:{entity_type}:ref-{}", &digest[..16])
}

fn encode_key_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b'@') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
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

mod receiver_flow;

#[cfg(test)]
mod tests;
