use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone)]
struct ExportEntry {
    export_name: String,
    local_name: Option<String>,
    from_request: Option<String>,
    from_name: Option<String>,
}

/// In-memory view of the module graph for export-chain resolution. Each edge
/// remembers whether its resolution was heuristic (workspace-inferred) so
/// consumers can downgrade confidence for paths that cross such edges.
pub struct ModuleGraph {
    exports: HashMap<i64, Vec<ExportEntry>>,
    contract_exports: HashMap<i64, Vec<ExportEntry>>,
    edges: HashMap<(i64, String), (Option<i64>, bool)>,
    pub paths: HashMap<i64, String>,
}

impl ModuleGraph {
    pub fn load(conn: &Connection) -> Result<Self> {
        Self::load_inner(conn, false)
    }

    /// Load the documentary export plane for structural contract resolution.
    /// Runtime-only consumers such as `who_uses` use [`Self::load`] and avoid
    /// scanning contract exports they cannot consume.
    pub(crate) fn load_with_contracts(conn: &Connection) -> Result<Self> {
        Self::load_inner(conn, true)
    }

    fn load_inner(conn: &Connection, include_contracts: bool) -> Result<Self> {
        let resolver_formats =
            crate::formats::eligible_ids_json(crate::formats::Capability::Resolver);
        let mut exports: HashMap<i64, Vec<ExportEntry>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT export.file_id, export.export_name, export.local_name,
                    export.from_request, export.from_name
             FROM exports export
             JOIN code_files file ON file.id=export.file_id
             WHERE file.format IN (SELECT value FROM json_each(?1))",
        )?;
        let rows = stmt.query_map([&resolver_formats], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                ExportEntry {
                    export_name: r.get(1)?,
                    local_name: r.get(2)?,
                    from_request: r.get(3)?,
                    from_name: r.get(4)?,
                },
            ))
        })?;
        for row in rows {
            let (id, e) = row?;
            exports.entry(id).or_default().push(e);
        }

        let mut contract_exports: HashMap<i64, Vec<ExportEntry>> = HashMap::new();
        if include_contracts {
            let mut stmt = conn.prepare(
                "SELECT export.file_id, export.export_name, export.local_name,
                        export.from_request, export.from_name
                 FROM contract_exports export
                 JOIN code_files file ON file.id=export.file_id
                 WHERE file.format IN (SELECT value FROM json_each(?1))",
            )?;
            let rows = stmt.query_map([&resolver_formats], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ExportEntry {
                        export_name: r.get(1)?,
                        local_name: r.get(2)?,
                        from_request: r.get(3)?,
                        from_name: r.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (id, entry) = row?;
                contract_exports.entry(id).or_default().push(entry);
            }
        }

        let mut edges: HashMap<(i64, String), (Option<i64>, bool)> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT edge.from_file, edge.request, edge.to_file, edge.resolution
             FROM module_edges edge
             JOIN code_files importer ON importer.id=edge.from_file
             LEFT JOIN code_files target ON target.id=edge.to_file
             WHERE importer.format IN (SELECT value FROM json_each(?1))
               AND (edge.to_file IS NULL
                    OR target.format IN (SELECT value FROM json_each(?1)))",
        )?;
        let rows = stmt.query_map([&resolver_formats], |r| {
            Ok((
                (r.get::<_, i64>(0)?, r.get::<_, String>(1)?),
                (
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?.as_deref() == Some("workspace-inferred"),
                ),
            ))
        })?;
        for row in rows {
            let (k, v) = row?;
            edges.insert(k, v);
        }

        let mut paths = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT id, path FROM code_files
             WHERE format IN (SELECT value FROM json_each(?1))",
        )?;
        let rows = stmt.query_map([&resolver_formats], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            paths.insert(id, path);
        }
        Ok(Self {
            exports,
            contract_exports,
            edges,
            paths,
        })
    }

    pub fn edge(&self, file: i64, request: &str) -> Option<i64> {
        self.edges
            .get(&(file, request.to_string()))
            .and_then(|(target, _)| *target)
    }

    /// Whether the module edge for (file, request) was resolved through a
    /// heuristic workspace mapping rather than direct resolution.
    pub fn edge_inferred(&self, file: i64, request: &str) -> bool {
        self.edges
            .get(&(file, request.to_string()))
            .is_some_and(|(_, inferred)| *inferred)
    }

    /// Follow export chains (aliases, re-exports, barrels, stars) from
    /// (file, `export_name`) to the defining (file, `local_name`).
    pub fn resolve_export(&self, file: i64, name: &str) -> Option<(i64, String)> {
        self.resolve_export_traced(file, name)
            .map(|(f, n, _)| (f, n))
    }

    /// Like [`Self::resolve_export`], additionally reporting whether any hop
    /// on the successful chain crossed a heuristically resolved module edge.
    pub fn resolve_export_traced(&self, file: i64, name: &str) -> Option<(i64, String, bool)> {
        let mut inferred = false;
        let (file, name) = self.resolve_export_inner(
            &self.exports,
            file,
            name,
            &mut HashSet::new(),
            &mut inferred,
        )?;
        Some((file, name, inferred))
    }

    /// Resolve one runtime export without accepting heuristic module edges or
    /// choosing an arbitrary `export *` branch. This stricter form is used by
    /// projections that suppress a later checker pass and therefore need a
    /// closed binding, not merely the graph's best structural candidate.
    pub(crate) fn resolve_export_exact(&self, file: i64, name: &str) -> Option<(i64, String)> {
        match self.resolve_export_exact_inner(file, name, &mut HashSet::new()) {
            ExactExportResolution::Candidates(candidates) if candidates.len() == 1 => {
                candidates.into_iter().next()
            }
            ExactExportResolution::Missing
            | ExactExportResolution::Unsafe
            | ExactExportResolution::Candidates(_) => None,
        }
    }

    fn resolve_export_exact_inner(
        &self,
        file: i64,
        name: &str,
        visited: &mut HashSet<(i64, String)>,
    ) -> ExactExportResolution {
        if !visited.insert((file, name.to_string())) {
            return ExactExportResolution::Unsafe;
        }
        let Some(entries) = self.exports.get(&file) else {
            return ExactExportResolution::Missing;
        };

        let exact = entries
            .iter()
            .filter(|entry| entry.export_name == name)
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            let mut candidates = BTreeSet::new();
            for entry in exact {
                let resolved = match (&entry.local_name, &entry.from_request, &entry.from_name) {
                    (Some(local), _, _) => {
                        ExactExportResolution::Candidates(BTreeSet::from([(file, local.clone())]))
                    }
                    (None, None, _) => ExactExportResolution::Candidates(BTreeSet::from([(
                        file,
                        "default".to_string(),
                    )])),
                    (None, Some(request), Some(from)) => {
                        if self.edge_inferred(file, request) {
                            return ExactExportResolution::Unsafe;
                        }
                        let Some(target) = self.edge(file, request) else {
                            return ExactExportResolution::Unsafe;
                        };
                        if from == "*" {
                            ExactExportResolution::Candidates(BTreeSet::from([(
                                target,
                                "*".to_string(),
                            )]))
                        } else {
                            let mut branch = visited.clone();
                            self.resolve_export_exact_inner(target, from, &mut branch)
                        }
                    }
                    _ => ExactExportResolution::Unsafe,
                };
                let ExactExportResolution::Candidates(resolved) = resolved else {
                    return ExactExportResolution::Unsafe;
                };
                candidates.extend(resolved);
            }
            return ExactExportResolution::Candidates(candidates);
        }

        // ECMAScript `export * from` never re-exports the target module's
        // default binding. Only an explicit `export { default } from` may do
        // that, and it would have appeared in the exact entries above.
        if name == "default" {
            return ExactExportResolution::Missing;
        }

        let mut candidates = BTreeSet::new();
        for entry in entries.iter().filter(|entry| entry.export_name == "*") {
            let Some(request) = entry.from_request.as_deref() else {
                return ExactExportResolution::Unsafe;
            };
            if self.edge_inferred(file, request) {
                return ExactExportResolution::Unsafe;
            }
            let Some(target) = self.edge(file, request) else {
                return ExactExportResolution::Unsafe;
            };
            let mut branch = visited.clone();
            match self.resolve_export_exact_inner(target, name, &mut branch) {
                ExactExportResolution::Candidates(resolved) => candidates.extend(resolved),
                ExactExportResolution::Missing => {}
                ExactExportResolution::Unsafe => return ExactExportResolution::Unsafe,
            }
        }
        if candidates.is_empty() {
            ExactExportResolution::Missing
        } else {
            ExactExportResolution::Candidates(candidates)
        }
    }

    /// Resolve a documentary/type export chain without allowing type-only
    /// bindings to influence runtime reference projection.
    pub fn resolve_contract_export_traced(
        &self,
        file: i64,
        name: &str,
    ) -> Option<(i64, String, bool)> {
        let mut inferred = false;
        let (file, name) = self.resolve_export_inner(
            &self.contract_exports,
            file,
            name,
            &mut HashSet::new(),
            &mut inferred,
        )?;
        Some((file, name, inferred))
    }

    fn resolve_export_inner(
        &self,
        exports: &HashMap<i64, Vec<ExportEntry>>,
        file: i64,
        name: &str,
        visited: &mut HashSet<(i64, String)>,
        inferred: &mut bool,
    ) -> Option<(i64, String)> {
        if !visited.insert((file, name.to_string())) {
            return None; // cycle
        }
        let entries = exports.get(&file)?;
        // Exact export first.
        for e in entries {
            if e.export_name != name {
                continue;
            }
            match (&e.local_name, &e.from_request, &e.from_name) {
                (Some(local), _, _) => return Some((file, local.clone())),
                (None, None, _) => return Some((file, "default".to_string())),
                (None, Some(req), Some(from)) => {
                    let target = self.edge(file, req)?;
                    if self.edge_inferred(file, req) {
                        *inferred = true;
                    }
                    if from == "*" {
                        // `export * as ns from` — the namespace itself.
                        return Some((target, "*".to_string()));
                    }
                    return self.resolve_export_inner(exports, target, from, visited, inferred);
                }
                _ => return None,
            }
        }
        // Star re-exports: try each source module. A failing branch must not
        // taint the flag, so restore it before moving to the next source.
        for e in entries {
            if e.export_name == "*"
                && let Some(request) = e.from_request.as_deref()
                && let Some(target) = self.edge(file, request)
            {
                let before = *inferred;
                if self.edge_inferred(file, request) {
                    *inferred = true;
                }
                if let Some(hit) =
                    self.resolve_export_inner(exports, target, name, visited, inferred)
                {
                    return Some(hit);
                }
                *inferred = before;
            }
        }
        None
    }
}

enum ExactExportResolution {
    Missing,
    Unsafe,
    Candidates(BTreeSet<(i64, String)>),
}

#[derive(Debug, serde::Serialize)]
pub struct Usage {
    pub file: String,
    pub file_origin: String,
    pub line: i64,
    pub kind: String,
    pub confidence: String,
    pub detail: Option<String>,
    pub chunk_name: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SymbolTarget {
    pub file: String,
    pub file_origin: String,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub line: i64,
    pub exported: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct SymbolAnchorResolution {
    pub requested_anchor: String,
    pub resolved_anchor: String,
    pub anchor_status: String,
}

impl SymbolAnchorResolution {
    pub(crate) fn is_exact_current(&self) -> bool {
        self.requested_anchor == self.resolved_anchor && self.anchor_status == "exact"
    }
}

/// Resolve one name-mode CLI target back to its canonical structural anchor.
/// A declaration-line match wins; otherwise the target is exact only when its
/// file and name identify one symbol node. Ambiguity deliberately falls back
/// to the legacy name-based usage query.
pub fn unique_anchor_for_symbol_target(
    conn: &Connection,
    target: &SymbolTarget,
) -> Result<Option<String>> {
    let candidates = conn
        .prepare(
            "SELECT node.node_key, symbol.line
             FROM graph_nodes node
             JOIN symbols symbol
               ON node.native_table='symbols' AND node.native_id=symbol.id
             WHERE symbol.file_id=?1 AND symbol.name=?2
             ORDER BY symbol.line, symbol.decl_start, node.node_key",
        )?
        .query_map(rusqlite::params![target.file_id, target.name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let line_matches = candidates
        .iter()
        .filter(|(_, line)| *line == target.line)
        .map(|(anchor, _)| anchor)
        .collect::<Vec<_>>();
    if let [anchor] = line_matches.as_slice() {
        return Ok(Some((*anchor).clone()));
    }
    Ok(match candidates.as_slice() {
        [(anchor, _)] => Some(anchor.clone()),
        _ => None,
    })
}

/// Resolve a snapshot-scoped structural symbol anchor to the exact canonical
/// symbol row consumed by definition and usage queries. This deliberately does
/// not accept file anchors or fall back to fuzzy name matching.
pub fn find_symbol_by_anchor_in_scope(
    conn: &Connection,
    anchor: &str,
    expected_snapshot: Option<&str>,
    file_origins: &[String],
    requested_formats: &[String],
) -> Result<(SymbolTarget, SymbolAnchorResolution)> {
    let eligible_formats = crate::formats::eligible_ids_in_scope_json(
        crate::formats::Capability::ExactDefinition,
        requested_formats,
    );
    let (resolved_anchor, anchor_status) = crate::structural::resolve_anchor_in_origins(
        conn,
        anchor,
        expected_snapshot,
        file_origins,
    )?;
    let target = conn
        .query_row(
            "SELECT file.path, file.origin, file.id, symbol.name, symbol.kind,
                    symbol.line, symbol.exported
             FROM graph_nodes node
             JOIN symbols symbol
               ON node.native_table='symbols' AND node.native_id=symbol.id
             JOIN files file ON file.id=symbol.file_id
             WHERE node.node_key=?1 AND node.node_kind='symbol'
               AND file.format IN (SELECT value FROM json_each(?2))",
            rusqlite::params![&resolved_anchor, eligible_formats],
            |row| {
                Ok(SymbolTarget {
                    file: row.get(0)?,
                    file_origin: row.get(1)?,
                    file_id: row.get(2)?,
                    name: row.get(3)?,
                    kind: row.get(4)?,
                    line: row.get(5)?,
                    exported: row.get::<_, i64>(6)? != 0,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("anchor `{resolved_anchor}` is not a symbol node"))?;
    if !file_origins
        .iter()
        .any(|origin| origin == &target.file_origin)
    {
        bail!("anchor `{resolved_anchor}` is outside the requested file origins");
    }
    Ok((
        target,
        SymbolAnchorResolution {
            requested_anchor: anchor.to_string(),
            resolved_anchor,
            anchor_status,
        },
    ))
}

/// Exact usage lookup for a canonical symbol anchor. Precise projected edges
/// are returned directly. Unresolved member calls are included only when the
/// graph projects the requested symbol as one of that member hub's candidates;
/// those sites remain explicitly `possible`.
pub fn who_uses_anchor_in_origins(
    conn: &Connection,
    anchor: &str,
    file_origins: &[String],
) -> Result<Vec<Usage>> {
    who_uses_anchor_in_scope(conn, anchor, file_origins, &[])
}

pub fn who_uses_anchor_in_scope(
    conn: &Connection,
    anchor: &str,
    file_origins: &[String],
    requested_formats: &[String],
) -> Result<Vec<Usage>> {
    crate::origin::validate_all(file_origins)?;
    let origins_json = serde_json::to_string(file_origins)?;
    let occurrence_formats = crate::formats::eligible_ids_in_scope_json(
        crate::formats::Capability::Structural,
        requested_formats,
    );
    let mut statement = conn.prepare(
        "SELECT file.path,file.origin,COALESCE(edge.line,source.line),
                edge.kind,edge.confidence,
                CASE WHEN json_type(edge.detail_json,'$.detail')='text'
                     THEN json_extract(edge.detail_json,'$.detail') END,
                source.display_name
         FROM resolved_edges edge
         JOIN graph_nodes source ON source.node_key=edge.src_key
         JOIN files file ON file.id=COALESCE(edge.source_file_id,source.file_id)
         WHERE edge.dst_key=?1
           AND file.origin IN (SELECT value FROM json_each(?2))
           AND file.format IN (SELECT value FROM json_each(?3))
           AND COALESCE(edge.line,source.line) IS NOT NULL
         ORDER BY
           CASE edge.confidence WHEN 'certain' THEN 0 WHEN 'likely' THEN 1 ELSE 2 END,
           file.path,COALESCE(edge.line,source.line),edge.kind,edge.id",
    )?;
    let rows = statement.query_map(
        rusqlite::params![anchor, &origins_json, &occurrence_formats],
        |row| {
            Ok(Usage {
                file: row.get(0)?,
                file_origin: row.get(1)?,
                line: row.get(2)?,
                kind: row.get(3)?,
                confidence: row.get(4)?,
                detail: row.get(5)?,
                chunk_name: row.get(6)?,
            })
        },
    )?;
    let mut usages = rows.collect::<std::result::Result<Vec<_>, _>>()?;

    // Deterministic extraction represents an unresolved member call as two
    // edges: caller -> member hub -> candidate symbol. Attribute the candidate
    // to the caller edge so the usage keeps its real file and line. Once any
    // occurrence-specific edge closes that call at certain/likely, suppress
    // every generic hub candidate instead of offering resolved sites as
    // possible callers of other same-named symbols.
    let mut seen_sites = usages
        .iter()
        .map(|usage| (usage.file.clone(), usage.line))
        .collect::<HashSet<_>>();
    let mut statement = conn.prepare(
        "SELECT file.path,file.origin,call.line,'call','possible',
                CASE WHEN json_type(call.detail_json,'$.object')='text'
                     THEN json_extract(call.detail_json,'$.object') || '.' ||
                          json_extract(call.detail_json,'$.property') || '()' END,
                source.display_name
         FROM resolved_edges candidate
         JOIN resolved_edges call
           ON call.dst_key=candidate.src_key AND call.kind='member_call'
         JOIN graph_nodes source ON source.node_key=call.src_key
         JOIN files file ON file.id=COALESCE(call.source_file_id,source.file_id)
         WHERE candidate.dst_key=?1 AND candidate.kind='member_candidate'
           AND file.origin IN (SELECT value FROM json_each(?2))
           AND file.format IN (SELECT value FROM json_each(?3))
           AND call.line IS NOT NULL
           AND NOT EXISTS(
             SELECT 1 FROM resolved_edges resolved
             WHERE resolved.src_key=call.src_key
               AND resolved.confidence IN ('certain','likely')
               AND json_type(resolved.detail_json,'$.memberCallId')='integer'
               AND json_extract(resolved.detail_json,'$.memberCallId')=
                   json_extract(call.detail_json,'$.memberCallId')
           )
         ORDER BY file.path,call.line,call.id,candidate.id",
    )?;
    let rows = statement.query_map(
        rusqlite::params![anchor, &origins_json, &occurrence_formats],
        |row| {
            Ok(Usage {
                file: row.get(0)?,
                file_origin: row.get(1)?,
                line: row.get(2)?,
                kind: row.get(3)?,
                confidence: row.get(4)?,
                detail: row.get(5)?,
                chunk_name: row.get(6)?,
            })
        },
    )?;
    for row in rows {
        let usage = row?;
        if seen_sites.insert((usage.file.clone(), usage.line)) {
            usages.push(usage);
        }
    }
    Ok(usages)
}

/// Find symbols matching "Name" or "path-substring:Name" within an origin allowlist.
pub fn find_symbols_in_origins(
    conn: &Connection,
    spec: &str,
    file_origins: &[String],
) -> Result<Vec<SymbolTarget>> {
    find_symbols_in_scope(conn, spec, file_origins, &[])
}

pub fn find_symbols_in_scope(
    conn: &Connection,
    spec: &str,
    file_origins: &[String],
    requested_formats: &[String],
) -> Result<Vec<SymbolTarget>> {
    crate::origin::validate_all(file_origins)?;
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let eligible_formats = crate::formats::eligible_ids_in_scope_json(
        crate::formats::Capability::ExactDefinition,
        requested_formats,
    );
    let (path_filter, name) = match spec.rsplit_once(':') {
        Some((p, n)) => (Some(p.to_string()), n.to_string()),
        None => (None, spec.to_string()),
    };
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, f.id, s.name, s.kind, s.line, s.exported
         FROM symbols s JOIN code_files f ON s.file_id = f.id
         WHERE s.name = ?1
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
           AND f.format IN (SELECT value FROM json_each(?5))
         ORDER BY s.exported DESC, f.path, s.line, s.kind",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![&name, repository, workspace, dependency, &eligible_formats],
        |r| {
            Ok(SymbolTarget {
                file: r.get(0)?,
                file_origin: r.get(1)?,
                file_id: r.get(2)?,
                name: r.get(3)?,
                kind: r.get(4)?,
                line: r.get(5)?,
                exported: r.get::<_, i64>(6)? != 0,
            })
        },
    )?;
    for row in rows {
        let t = row?;
        if path_filter.as_deref().is_none_or(|p| t.file.contains(p)) {
            out.push(t);
        }
    }
    // Class methods aren't root symbols; they exist as method chunks.
    if out.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT f.path, f.origin, f.id, c.name, c.start_line, c.scope_chain
             FROM code_chunks c JOIN code_files f ON c.file_id = f.id
             WHERE c.name = ?1 AND c.kind = 'method'
               AND ((?2 AND f.origin='repository')
                 OR (?3 AND f.origin='workspace')
                 OR (?4 AND f.origin='dependency'))
               AND f.format IN (SELECT value FROM json_each(?5))
             ORDER BY f.path, c.start_line, c.scope_chain",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![&name, repository, workspace, dependency, &eligible_formats],
            |r| {
                Ok((
                    SymbolTarget {
                        file: r.get(0)?,
                        file_origin: r.get(1)?,
                        file_id: r.get(2)?,
                        name: name.clone(),
                        kind: "method".into(),
                        line: r.get(4)?,
                        exported: false,
                    },
                    r.get::<_, String>(5)?,
                ))
            },
        )?;
        for row in rows {
            let (mut t, scope) = row?;
            if !scope.is_empty() {
                t.kind = format!("method of {scope}");
            }
            if path_filter.as_deref().is_none_or(|p| t.file.contains(p)) {
                out.push(t);
            }
        }
    }
    Ok(out)
}

/// All usages of the symbol `name` defined in `file_id`, filtered by origin.
pub fn who_uses_in_origins(
    conn: &Connection,
    graph: &ModuleGraph,
    file_id: i64,
    name: &str,
    file_origins: &[String],
) -> Result<Vec<Usage>> {
    who_uses_in_scope(conn, graph, file_id, name, file_origins, &[])
}

pub fn who_uses_in_scope(
    conn: &Connection,
    graph: &ModuleGraph,
    file_id: i64,
    name: &str,
    file_origins: &[String],
    requested_formats: &[String],
) -> Result<Vec<Usage>> {
    crate::origin::validate_all(file_origins)?;
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let occurrence_formats = crate::formats::eligible_ids_in_scope_json(
        crate::formats::Capability::Structural,
        requested_formats,
    );
    let mut usages = Vec::new();

    // Same-file references.
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, r.line, r.kind, r.confidence, r.detail, c.name
         FROM refs r JOIN files f ON r.file_id = f.id
         LEFT JOIN chunks c ON r.chunk_id = c.id
         WHERE r.local = 1 AND r.file_id = ?1 AND r.target_name = ?2
           AND ((?3 AND f.origin='repository')
             OR (?4 AND f.origin='workspace')
             OR (?5 AND f.origin='dependency'))
           AND f.format IN (SELECT value FROM json_each(?6))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            file_id,
            name,
            repository,
            workspace,
            dependency,
            &occurrence_formats
        ],
        row_to_usage,
    )?;
    usages.extend(rows.filter_map(|r| r.ok()));

    // Cross-file: refs through imports whose export chain lands on (file_id, name).
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, r.line, r.kind, r.confidence, r.detail, c.name,
                r.file_id, r.target_request, r.target_name
         FROM refs r JOIN files f ON r.file_id = f.id
         LEFT JOIN chunks c ON r.chunk_id = c.id
         WHERE r.local = 0 AND r.target_request IS NOT NULL
           AND ((?1 AND f.origin='repository')
             OR (?2 AND f.origin='workspace')
             OR (?3 AND f.origin='dependency'))
           AND f.format IN (SELECT value FROM json_each(?4))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![repository, workspace, dependency, &occurrence_formats],
        |r| {
            Ok((
                row_to_usage(r)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
            ))
        },
    )?;
    for row in rows {
        let (usage, ref_file, request, target_name) = row?;
        if target_name == "*" {
            continue; // bare namespace use; not symbol-specific
        }
        let Some(target_file) = graph.edge(ref_file, &request) else {
            continue;
        };
        let resolved = if target_file == file_id && target_name == name {
            // direct hit even without export rows (defensive)
            Some((file_id, name.to_string()))
        } else {
            graph.resolve_export(target_file, &target_name)
        };
        if resolved == Some((file_id, name.to_string())) {
            usages.push(usage);
        }
    }

    // Tier 3: member-call name matches (`x.name(...)`), minus sites already
    // matched precisely above.
    let seen: HashSet<(String, i64)> = usages.iter().map(|u| (u.file.clone(), u.line)).collect();
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, m.line, m.object, c.name
         FROM member_calls m JOIN files f ON m.file_id = f.id
         LEFT JOIN chunks c ON m.chunk_id = c.id
         WHERE m.prop = ?1
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
           AND f.format IN (SELECT value FROM json_each(?5))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![name, repository, workspace, dependency, &occurrence_formats],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        },
    )?;
    for row in rows {
        let (file, file_origin, line, object, chunk_name) = row?;
        if seen.contains(&(file.clone(), line)) {
            continue;
        }
        usages.push(Usage {
            file,
            file_origin,
            line,
            kind: "call".into(),
            confidence: "possible".into(),
            detail: object.map(|o| format!("{o}.{name}()")),
            chunk_name,
        });
    }
    Ok(usages)
}

#[derive(Debug, serde::Serialize)]
pub struct EventSite {
    pub file: String,
    pub file_origin: String,
    pub line: i64,
    pub role: String,
    pub name: String,
    pub method: String,
    pub chunk_name: Option<String>,
}

/// Event wiring filtered by event name and file origin.
pub fn events_in_origins(
    conn: &Connection,
    filter: Option<&str>,
    file_origins: &[String],
) -> Result<Vec<EventSite>> {
    crate::origin::validate_all(file_origins)?;
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::Structural);
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, e.line, e.role, e.name, e.method, c.name
         FROM events e JOIN files f ON e.file_id = f.id
         LEFT JOIN chunks c ON e.chunk_id = c.id
         WHERE (?1 IS NULL OR e.name = ?1)
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
           AND f.format IN (SELECT value FROM json_each(?5))
         ORDER BY e.name, e.role DESC, f.path, e.line",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![filter, repository, workspace, dependency, eligible_formats],
        |r| {
            Ok(EventSite {
                file: r.get(0)?,
                file_origin: r.get(1)?,
                line: r.get(2)?,
                role: r.get(3)?,
                name: r.get(4)?,
                method: r.get(5)?,
                chunk_name: r.get(6)?,
            })
        },
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn row_to_usage(r: &rusqlite::Row<'_>) -> rusqlite::Result<Usage> {
    Ok(Usage {
        file: r.get(0)?,
        file_origin: r.get(1)?,
        line: r.get(2)?,
        kind: r.get(3)?,
        confidence: r.get(4)?,
        detail: r.get(5)?,
        chunk_name: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn anchor_resolution_does_not_repeat_response_identity() -> Result<()> {
        let value = serde_json::to_value(SymbolAnchorResolution {
            requested_anchor: "sym:old.ts#::run@1".into(),
            resolved_anchor: "sym:new.ts#::run@1".into(),
            anchor_status: "moved".into(),
        })?;
        assert!(value.get("snapshot").is_none());
        assert_eq!(value["anchor_status"], "moved");
        Ok(())
    }

    #[test]
    fn poisoned_rust_facts_stay_out_of_exact_and_event_queries() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let conn = crate::store::open(directory.path())?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('snapshot','registry-boundary')",
            [],
        )?;
        conn.execute(
            "INSERT INTO files(path,hash,corpus,format,role,origin)
             VALUES('main.ts','typescript','code','typescript','production','repository')",
            [],
        )?;
        let typescript_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks(
               file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'function','sharedNeedle','','sharedNeedle',0,80,1,4,
                      'typescript-chunk','export function sharedNeedle() {}')",
            [typescript_file],
        )?;
        let typescript_chunk = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols(
               file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
             ) VALUES(?1,'sharedNeedle','function',16,28,0,40,'',1,1)",
            [typescript_file],
        )?;
        conn.execute(
            "INSERT INTO refs(
               file_id,chunk_id,start,line,kind,confidence,target_name,local
             ) VALUES(?1,?2,45,2,'call','certain','sharedNeedle',1)",
            rusqlite::params![typescript_file, typescript_chunk],
        )?;
        conn.execute(
            "INSERT INTO member_calls(file_id,chunk_id,start,end,line,prop,object)
             VALUES(?1,?2,50,70,3,'sharedNeedle','service')",
            rusqlite::params![typescript_file, typescript_chunk],
        )?;
        conn.execute(
            "INSERT INTO events(file_id,chunk_id,line,role,name,method)
             VALUES(?1,?2,4,'emit','shared-event','emit')",
            rusqlite::params![typescript_file, typescript_chunk],
        )?;

        conn.execute(
            "INSERT INTO files(path,hash,corpus,format,role,origin)
             VALUES('poison.rs','rust','code','rust','production','repository')",
            [],
        )?;
        let rust_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks(
               file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'method','rustOnlyMethod','','sharedNeedle rustOnlyMethod',0,80,1,4,
                      'rust-chunk','fn sharedNeedle() {}')",
            [rust_file],
        )?;
        let rust_chunk = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols(
               file_id,name,kind,start,end,decl_start,decl_end,scope_chain,line,exported
             ) VALUES(?1,'sharedNeedle','function',3,15,0,40,'',1,1)",
            [rust_file],
        )?;
        conn.execute(
            "INSERT INTO refs(
               file_id,chunk_id,start,line,kind,confidence,target_request,target_name,local
             ) VALUES(?1,?2,20,99,'call','certain','./main','sharedNeedle',0)",
            rusqlite::params![rust_file, rust_chunk],
        )?;
        conn.execute(
            "INSERT INTO module_edges(from_file,request,to_file,resolution)
             VALUES(?1,'./main',?2,'resolver')",
            rusqlite::params![rust_file, typescript_file],
        )?;
        conn.execute(
            "INSERT INTO member_calls(file_id,chunk_id,start,end,line,prop,object)
             VALUES(?1,?2,30,50,100,'sharedNeedle','poison')",
            rusqlite::params![rust_file, rust_chunk],
        )?;
        conn.execute(
            "INSERT INTO events(file_id,chunk_id,line,role,name,method)
             VALUES(?1,?2,101,'listen','shared-event','on')",
            rusqlite::params![rust_file, rust_chunk],
        )?;

        let definitions =
            find_symbols_in_origins(&conn, "sharedNeedle", &crate::origin::defaults())?;
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].file, "main.ts");
        assert!(
            find_symbols_in_origins(&conn, "rustOnlyMethod", &crate::origin::defaults(),)?
                .is_empty()
        );

        let graph = ModuleGraph::load(&conn)?;
        assert!(!graph.paths.contains_key(&rust_file));
        assert!(graph.edge(rust_file, "./main").is_none());
        let usages = who_uses_in_origins(
            &conn,
            &graph,
            typescript_file,
            "sharedNeedle",
            &crate::origin::defaults(),
        )?;
        assert_eq!(usages.len(), 2);
        assert!(usages.iter().all(|usage| usage.file == "main.ts"));

        let events = events_in_origins(&conn, Some("shared-event"), &crate::origin::defaults())?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].file, "main.ts");
        Ok(())
    }
}
