use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

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
        let mut exports: HashMap<i64, Vec<ExportEntry>> = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT file_id, export_name, local_name, from_request, from_name FROM exports")?;
        let rows = stmt.query_map([], |r| {
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
        let mut stmt = conn.prepare(
            "SELECT file_id, export_name, local_name, from_request, from_name
             FROM contract_exports",
        )?;
        let rows = stmt.query_map([], |r| {
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

        let mut edges: HashMap<(i64, String), (Option<i64>, bool)> = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT from_file, request, to_file, resolution FROM module_edges")?;
        let rows = stmt.query_map([], |r| {
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
        let mut stmt = conn.prepare("SELECT id, path FROM files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, path) = row?;
            paths.insert(id, path);
        }
        Ok(Self { exports, contract_exports, edges, paths })
    }

    pub fn edge(&self, file: i64, request: &str) -> Option<i64> {
        self.edges.get(&(file, request.to_string())).and_then(|(target, _)| *target)
    }

    /// Whether the module edge for (file, request) was resolved through a
    /// heuristic workspace mapping rather than direct resolution.
    pub fn edge_inferred(&self, file: i64, request: &str) -> bool {
        self.edges
            .get(&(file, request.to_string()))
            .is_some_and(|(_, inferred)| *inferred)
    }

    /// Follow export chains (aliases, re-exports, barrels, stars) from
    /// (file, export_name) to the defining (file, local_name).
    pub fn resolve_export(&self, file: i64, name: &str) -> Option<(i64, String)> {
        self.resolve_export_traced(file, name).map(|(f, n, _)| (f, n))
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

/// Find symbols matching "Name" or "path-substring:Name" within an origin allowlist.
pub fn find_symbols_in_origins(
    conn: &Connection,
    spec: &str,
    file_origins: &[String],
) -> Result<Vec<SymbolTarget>> {
    crate::origin::validate_all(file_origins)?;
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let (path_filter, name) = match spec.rsplit_once(':') {
        Some((p, n)) => (Some(p.to_string()), n.to_string()),
        None => (None, spec.to_string()),
    };
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, f.id, s.name, s.kind, s.line, s.exported
         FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
         ORDER BY s.exported DESC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![&name, repository, workspace, dependency],
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
    })?;
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
             FROM chunks c JOIN files f ON c.file_id = f.id
             WHERE c.name = ?1 AND c.kind = 'method'
               AND ((?2 AND f.origin='repository')
                 OR (?3 AND f.origin='workspace')
                 OR (?4 AND f.origin='dependency'))",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![&name, repository, workspace, dependency],
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
        })?;
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
    crate::origin::validate_all(file_origins)?;
    let repository = file_origins.iter().any(|origin| origin == "repository");
    let workspace = file_origins.iter().any(|origin| origin == "workspace");
    let dependency = file_origins.iter().any(|origin| origin == "dependency");
    let mut usages = Vec::new();

    // Same-file references.
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, r.line, r.kind, r.confidence, r.detail, c.name
         FROM refs r JOIN files f ON r.file_id = f.id
         LEFT JOIN chunks c ON r.chunk_id = c.id
         WHERE r.local = 1 AND r.file_id = ?1 AND r.target_name = ?2
           AND ((?3 AND f.origin='repository')
             OR (?4 AND f.origin='workspace')
             OR (?5 AND f.origin='dependency'))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![file_id, name, repository, workspace, dependency],
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
             OR (?3 AND f.origin='dependency'))",
    )?;
    let rows = stmt.query_map(rusqlite::params![repository, workspace, dependency], |r| {
        Ok((
            row_to_usage(r)?,
            r.get::<_, i64>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
        ))
    })?;
    for row in rows {
        let (usage, ref_file, request, target_name) = row?;
        if target_name == "*" {
            continue; // bare namespace use; not symbol-specific
        }
        let Some(target_file) = graph.edge(ref_file, &request) else { continue };
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
    let seen: HashSet<(String, i64)> =
        usages.iter().map(|u| (u.file.clone(), u.line)).collect();
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, m.line, m.object, c.name
         FROM member_calls m JOIN files f ON m.file_id = f.id
         LEFT JOIN chunks c ON m.chunk_id = c.id
         WHERE m.prop = ?1
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![name, repository, workspace, dependency],
        |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
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
    let mut stmt = conn.prepare(
        "SELECT f.path, f.origin, e.line, e.role, e.name, e.method, c.name
         FROM events e JOIN files f ON e.file_id = f.id
         LEFT JOIN chunks c ON e.chunk_id = c.id
         WHERE (?1 IS NULL OR e.name = ?1)
           AND ((?2 AND f.origin='repository')
             OR (?3 AND f.origin='workspace')
             OR (?4 AND f.origin='dependency'))
         ORDER BY e.name, e.role DESC, f.path, e.line",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![filter, repository, workspace, dependency],
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
    })?;
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
