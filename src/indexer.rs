use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{Connection, params};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::dependency::{self, DependencyLimits};
use crate::graph::{self, FileGraph};
use crate::package_exports::RESOLVE_CONDITIONS;
use crate::{file_role, parse, store, walk};

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    pub dependencies: Vec<String>,
    pub dependency_limits: DependencyLimits,
}

pub struct IndexOutcome {
    pub indexed: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub failures: Vec<IndexFailure>,
    pub chunks: usize,
    pub refs: usize,
    pub dependency_packages: usize,
    pub dependency_files: usize,
    pub dependency_bytes: u64,
    pub dependency_skipped: usize,
    pub dependency_skipped_bytes: u64,
    pub dependency_plans: Vec<String>,
    /// False when the structural projection was provably identical (same
    /// snapshot, projection version, and module resolution) and was kept.
    pub projection_rebuilt: bool,
    /// True when a forced re-extraction (cleared file hashes, e.g. after a
    /// schema migration) truncated the extraction tables wholesale instead of
    /// replacing files one at a time.
    pub extraction_reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFailure {
    pub path: String,
    pub stage: &'static str,
    pub error: String,
}

impl IndexOutcome {
    fn record_failure(
        &mut self,
        path: impl Into<String>,
        stage: &'static str,
        error: impl std::fmt::Display,
    ) {
        self.failed += 1;
        self.failures.push(IndexFailure {
            path: path.into(),
            stage,
            error: error.to_string(),
        });
    }
}

pub fn report_failures(outcome: &IndexOutcome) {
    if outcome.failures.is_empty() {
        return;
    }
    eprintln!("index failures ({}):", outcome.failures.len());
    for failure in &outcome.failures {
        let error = failure.error.replace('\n', "\n      ");
        eprintln!("  [{}] {}: {error}", failure.stage, failure.path);
    }
}

struct FileData {
    chunks: Vec<Chunk>,
    graph: FileGraph,
    lines: LineIndex,
}

struct FileIdentity<'a> {
    path: &'a str,
    hash: &'a str,
    role: &'a str,
    origin: &'a str,
    package_instance_id: Option<i64>,
    package_path: Option<&'a str>,
}

pub(crate) fn resolver_options(
    alias: oxc_resolver::Alias,
    tsconfig: Option<TsconfigDiscovery>,
) -> ResolveOptions {
    ResolveOptions {
        // Workspace package names -> in-repo source, so monorepo cross-package
        // imports resolve to indexed files instead of missing/dist targets.
        alias,
        extensions: vec![
            ".ts".into(),
            ".tsx".into(),
            ".mts".into(),
            ".cts".into(),
            ".js".into(),
            ".jsx".into(),
            ".mjs".into(),
            ".cjs".into(),
            ".json".into(),
        ],
        // TS convention: `./x.js` in source may mean `./x.ts` on disk.
        extension_alias: vec![
            (
                ".js".into(),
                vec![".ts".into(), ".tsx".into(), ".js".into(), ".jsx".into()],
            ),
            (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
            (".cjs".into(), vec![".cts".into(), ".cjs".into()]),
        ],
        condition_names: RESOLVE_CONDITIONS
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        main_fields: vec!["module".into(), "main".into()],
        tsconfig,
        ..ResolveOptions::default()
    }
}

/// Index (or re-index) a repository. Files whose content hash is unchanged
/// are skipped; changed files are fully replaced.
#[cfg(test)]
pub fn index_repo(root: &Path, conn: &Connection) -> Result<IndexOutcome> {
    index_repo_with_options(root, conn, &IndexOptions::default())
}

pub fn index_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(root, conn, options, true)
}

/// The pre-reset code path: always replace files one at a time, even when
/// every hash is cleared. Kept only so tests can prove the wholesale reset
/// produces the same database.
#[cfg(test)]
pub(crate) fn index_repo_without_extraction_reset(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(root, conn, options, false)
}

fn index_repo_impl(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    allow_extraction_reset: bool,
) -> Result<IndexOutcome> {
    let root = root.canonicalize()?;
    ensure_extraction_version(conn)?;
    let files = walk::source_files(&root);
    let mut outcome = IndexOutcome {
        indexed: 0,
        unchanged: 0,
        failed: 0,
        failures: Vec::new(),
        chunks: 0,
        refs: 0,
        dependency_packages: 0,
        dependency_files: 0,
        dependency_bytes: 0,
        dependency_skipped: 0,
        dependency_skipped_bytes: 0,
        dependency_plans: Vec::new(),
        projection_rebuilt: true,
        extraction_reset: false,
    };

    let mut existing: HashMap<String, (i64, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT path, id, hash, role FROM files WHERE origin!='dependency'")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ),
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    // Migrations force re-extraction by clearing file hashes, and the
    // first-party loop commits atomically, so a real database sits at ~100%
    // or ~0% cleared; the half-way threshold only guards hand-edited state.
    // At that scale, per-file replacement is pathological: every
    // `store::delete_file` cascades through the large evidence tables and the
    // FTS index while they are still fully populated. Truncate everything
    // once and let the loop below insert like a fresh index instead.
    let cleared = existing
        .values()
        .filter(|(_, hash, _)| hash.is_empty())
        .count();
    let extraction_reset =
        allow_extraction_reset && !existing.is_empty() && cleared * 2 >= existing.len();

    let mut seen: std::collections::HashSet<String> = Default::default();
    conn.execute_batch("BEGIN")?;
    if extraction_reset {
        store::reset_extraction_state(conn)?;
        existing.clear();
        outcome.extraction_reset = true;
    }
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        seen.insert(rel.clone());
        let source = match std::fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                outcome.record_failure(rel, "read", error);
                continue;
            }
        };
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let role = file_role::classify(Path::new(&rel), &source);
        if let Some((id, old_hash, old_role)) = existing.get(&rel)
            && *old_hash == hash
        {
            if old_role != role {
                conn.execute("UPDATE files SET role=?1 WHERE id=?2", params![role, id])?;
            }
            outcome.unchanged += 1;
            continue;
        }
        if std::env::var_os("JSCOUT_DEBUG").is_some() {
            eprintln!("extracting {rel}");
        }
        match extract_file(file, &rel, &source) {
            Ok(data) => {
                if let Some((old_id, _, _)) = existing.get(&rel) {
                    store::delete_file(conn, *old_id)?;
                }
                let identity = FileIdentity {
                    path: &rel,
                    hash: &hash,
                    role,
                    origin: "repository",
                    package_instance_id: None,
                    package_path: None,
                };
                let (nchunks, nrefs) = insert_file(conn, &identity, &data)?;
                outcome.indexed += 1;
                outcome.chunks += nchunks;
                outcome.refs += nrefs;
            }
            Err(e) => {
                outcome.record_failure(rel, "extract", e);
            }
        }
    }
    // Remove files that disappeared from disk.
    for (path, (id, _, _)) in &existing {
        if !seen.contains(path) {
            store::delete_file(conn, *id)?;
        }
    }

    // Remember the published projection identity before invalidating it: if
    // this run reproduces the exact same snapshot and module resolution, the
    // existing projection rows are provably identical and can be republished
    // without a rebuild. A wholesale reset wiped those rows, so nothing can
    // be republished no matter what identity was left behind.
    let previous = if outcome.extraction_reset {
        ProjectionIdentity {
            snapshot: None,
            projection_version: None,
            resolution_hash: None,
        }
    } else {
        ProjectionIdentity::read(conn)?
    };

    // Commit canonical rows and snapshot invalidation atomically. Every
    // following dependency/resolution step can fail, so the previous graph
    // must stop being public before control enters that phase.
    conn.execute(
        "DELETE FROM meta WHERE key IN ('snapshot', 'projection_version', 'resolution_hash')",
        [],
    )?;
    conn.execute_batch("COMMIT")?;

    let workspace = crate::workspace::WorkspaceMap::build(&root);
    let discovered = dependency::discover(&root, conn, &options.dependencies, &workspace)?;
    let plans = dependency::plan_packages(&discovered, options.dependency_limits)?;
    let instances = dependency::synchronize_instances(&root, conn, &workspace, &plans)?;
    index_dependency_files(conn, &plans, &instances, &mut outcome)?;
    if outcome.indexed > 0 {
        crate::embed::materialize_cached_embeddings(conn)?;
    }

    resolve_module_edges(&root, conn)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('root', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [root.to_string_lossy()],
    )?;
    let resolution = crate::structural::compute_resolution_hash(conn)?;
    let snapshot = crate::structural::compute_snapshot_with_resolution(conn, &resolution)?;
    let current = ProjectionIdentity {
        snapshot: Some(snapshot.clone()),
        projection_version: Some(crate::structural::PROJECTION_VERSION.to_string()),
        resolution_hash: Some(resolution.clone()),
    };
    let projection_started = std::time::Instant::now();
    if previous == current && crate::structural::checker_projection_reusable(conn, &snapshot)? {
        // The projection is a pure function of the canonical tables: the
        // snapshot covers every extracted row (file content identity) and the
        // resolution hash covers module edges, whose inputs (tsconfigs,
        // manifests, node_modules layout) live outside indexed content. An
        // active checker batch adds its own exact input manifest to this
        // reuse gate. Identical inputs under the same projection version
        // republish the existing rows instead of rebuilding them.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = current.publish(conn);
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        outcome.projection_rebuilt = false;
        if std::env::var_os("JSCOUT_TIMING").is_some() {
            eprintln!("timing structural-projection=skipped (unchanged)");
        }
        return Ok(outcome);
    }
    crate::structural::rebuild_projection(conn, &snapshot)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('resolution_hash', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&resolution],
    )?;
    if std::env::var_os("JSCOUT_TIMING").is_some() {
        eprintln!(
            "timing structural-projection={:?}",
            projection_started.elapsed()
        );
    }
    Ok(outcome)
}

/// The three meta values that must all match for the previous projection to
/// be provably identical to what a rebuild would produce.
#[derive(PartialEq, Eq)]
struct ProjectionIdentity {
    snapshot: Option<String>,
    projection_version: Option<String>,
    resolution_hash: Option<String>,
}

impl ProjectionIdentity {
    fn read(conn: &Connection) -> Result<Self> {
        let read = |key: &str| -> Result<Option<String>> {
            Ok(conn
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .ok())
        };
        Ok(Self {
            snapshot: read("snapshot")?,
            projection_version: read("projection_version")?,
            resolution_hash: read("resolution_hash")?,
        })
    }

    fn publish(&self, conn: &Connection) -> Result<()> {
        for (key, value) in [
            ("snapshot", &self.snapshot),
            ("projection_version", &self.projection_version),
            ("resolution_hash", &self.resolution_hash),
        ] {
            let value = value
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("projection identity is missing {key}"))?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        Ok(())
    }
}

fn ensure_extraction_version(conn: &Connection) -> Result<()> {
    let current = conn
        .query_row(
            "SELECT value FROM meta WHERE key='extraction_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    if current.as_deref() == Some(crate::entity::EXTRACTION_VERSION) {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        conn.execute("UPDATE files SET hash=''", [])?;
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute(
            "DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')",
            [],
        )?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('extraction_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [crate::entity::EXTRACTION_VERSION],
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

fn extract_file(abs: &Path, rel: &str, source: &str) -> Result<FileData> {
    parse::with_parsed(source, abs, |ret, semantic| {
        let chunker = Chunker::new(Path::new(rel), source, ret);
        let chunks = chunker.chunk_program(&ret.program, &ret.program.comments);
        let graph = graph::extract(ret, semantic);
        FileData {
            chunks,
            graph,
            lines: LineIndex::new(source),
        }
    })
}

fn insert_file(
    conn: &Connection,
    identity: &FileIdentity<'_>,
    data: &FileData,
) -> Result<(usize, usize)> {
    conn.execute(
        "INSERT INTO files(
           path, hash, role, origin, package_instance_id, package_path
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            identity.path,
            identity.hash,
            identity.role,
            identity.origin,
            identity.package_instance_id,
            identity.package_path,
        ],
    )?;
    let file_id = conn.last_insert_rowid();

    // Chunk spans for assigning refs to chunks (sorted by construction).
    let mut chunk_ids: Vec<(u32, u32, i64)> = Vec::with_capacity(data.chunks.len());
    {
        let mut ins_chunk = conn.prepare_cached(
            "INSERT INTO chunks(file_id, kind, name, scope_chain, symbols, start, end, start_line, end_line, hash, content)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut ins_fts = conn.prepare_cached(
            "INSERT INTO chunks_fts(rowid, content, name, symbols, path) VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for c in &data.chunks {
            let kind = serde_json::to_value(c.kind)?
                .as_str()
                .unwrap_or("module")
                .to_string();
            ins_chunk.execute(params![
                file_id,
                kind,
                c.name,
                c.scope_chain.join("."),
                c.symbols.join(" "),
                c.start,
                c.end,
                c.start_line,
                c.end_line,
                c.hash,
                c.content,
            ])?;
            let chunk_id = conn.last_insert_rowid();
            chunk_ids.push((c.start, c.end, chunk_id));
            ins_fts.execute(params![
                chunk_id,
                c.content,
                c.name.as_deref().unwrap_or(""),
                c.symbols.join(" "),
                identity.path,
            ])?;
        }
    }
    let chunk_for = |offset: u32| -> Option<i64> {
        chunk_ids
            .iter()
            .find(|(s, e, _)| *s <= offset && offset < *e)
            .map(|(_, _, id)| *id)
    };

    let mut ins_sym = conn.prepare_cached(
        "INSERT INTO symbols(
           file_id, name, kind, start, end, decl_start, decl_end, scope_chain, line, exported
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for s in &data.graph.symbols {
        ins_sym.execute(params![
            file_id,
            s.name,
            s.kind,
            s.start,
            s.end,
            s.decl_start,
            s.decl_end,
            s.scope_chain,
            data.lines.line(s.decl_start),
            s.exported as i64,
        ])?;
    }

    let mut ins_imp = conn.prepare_cached(
        "INSERT INTO imports(file_id, local_name, imported_name, request) VALUES(?1,?2,?3,?4)",
    )?;
    for i in &data.graph.imports {
        ins_imp.execute(params![file_id, i.local, i.imported, i.request])?;
    }

    let mut ins_exp = conn.prepare_cached(
        "INSERT INTO exports(file_id, export_name, local_name, from_request, from_name) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for e in &data.graph.exports {
        ins_exp.execute(params![
            file_id,
            e.export_name,
            e.local_name,
            e.from_request,
            e.from_name
        ])?;
    }

    let mut ins_contract_imp = conn.prepare_cached(
        "INSERT INTO contract_imports(file_id, local_name, imported_name, request)
         VALUES(?1,?2,?3,?4)",
    )?;
    for import in &data.graph.contract_imports {
        ins_contract_imp.execute(params![
            file_id,
            import.local,
            import.imported,
            import.request,
        ])?;
    }

    let mut ins_contract_exp = conn.prepare_cached(
        "INSERT INTO contract_exports(
           file_id, export_name, local_name, from_request, from_name
         ) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for export in &data.graph.contract_exports {
        ins_contract_exp.execute(params![
            file_id,
            export.export_name,
            export.local_name,
            export.from_request,
            export.from_name,
        ])?;
    }

    let mut ins_event = conn.prepare_cached(
        "INSERT INTO events(file_id, chunk_id, line, role, name, method) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for e in &data.graph.events {
        ins_event.execute(params![
            file_id,
            chunk_for(e.span_start),
            data.lines.line(e.span_start),
            e.role,
            e.name,
            e.method,
        ])?;
    }

    let mut ins_mc = conn.prepare_cached(
        "INSERT INTO member_calls(
           file_id, chunk_id, start, end, line, end_line, prop, object, receiver,
           receiver_start, receiver_end, property_start, property_end
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
    )?;
    for m in &data.graph.member_calls {
        ins_mc.execute(params![
            file_id,
            chunk_for(m.span_start),
            m.span_start,
            m.span_end,
            data.lines.line(m.span_start),
            data.lines.line(m.span_end.saturating_sub(1)),
            m.prop,
            m.object,
            m.receiver,
            m.receiver_start,
            m.receiver_end,
            m.property_start,
            m.property_end,
        ])?;
    }

    let mut ins_entity_site = conn.prepare_cached(
        "INSERT INTO entity_sites(
           file_id, chunk_id, start, end, line, end_line, plane, entity_type,
           role, identity_kind, identity_name, identity_start, target_name,
           target_start, extractor, provenance, confidence, detail_json
         ) VALUES(
           ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18
         )",
    )?;
    for site in &data.graph.entity_sites {
        ins_entity_site.execute(params![
            file_id,
            chunk_for(site.span_start),
            site.span_start,
            site.span_end,
            data.lines.line(site.span_start),
            data.lines.line(site.span_end.saturating_sub(1)),
            site.plane,
            site.entity_type,
            site.role,
            site.identity_kind,
            site.identity_name,
            site.identity_start,
            site.target_name,
            site.target_start,
            site.extractor,
            site.provenance,
            site.confidence,
            site.detail.to_string(),
        ])?;
    }

    let mut ins_ref = conn.prepare_cached(
        "INSERT INTO refs(
           file_id, chunk_id, start, line, kind, confidence,
           target_request, target_name, local, detail
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for r in &data.graph.refs {
        ins_ref.execute(params![
            file_id,
            chunk_for(r.span_start),
            r.span_start,
            data.lines.line(r.span_start),
            r.kind,
            r.confidence,
            r.target_request,
            r.target_name,
            r.local as i64,
            r.detail,
        ])?;
    }
    Ok((data.chunks.len(), data.graph.refs.len()))
}

fn index_dependency_files(
    conn: &Connection,
    plans: &[dependency::PackagePlan],
    instances: &std::collections::BTreeMap<PathBuf, i64>,
    outcome: &mut IndexOutcome,
) -> Result<()> {
    let existing: HashMap<String, (i64, String, String, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, id, hash, role, package_instance_id, package_path
             FROM files WHERE origin='dependency'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ),
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut seen = std::collections::HashSet::new();
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        for plan in plans {
            outcome.dependency_packages += 1;
            outcome.dependency_skipped += plan.skipped_files;
            outcome.dependency_skipped_bytes += plan.skipped_bytes;
            outcome.dependency_plans.push(format!(
                "{}@{}: {} ({})",
                plan.package.name,
                plan.package.version.as_deref().unwrap_or("unknown"),
                plan.source_basis,
                plan.status,
            ));
            let package_id = *instances.get(&plan.package.canonical_root).ok_or_else(|| {
                anyhow::anyhow!("dependency package instance was not synchronized")
            })?;
            for file in &plan.files {
                let display = dependency_display_path(&plan.package, &file.package_path);
                let source = match std::fs::read_to_string(&file.source_path) {
                    Ok(source) => source,
                    Err(error) => {
                        // Preserve the last successfully indexed row on a
                        // transient read failure; a later successful cycle can
                        // replace it without first losing known-good data.
                        seen.insert(display.clone());
                        outcome.record_failure(
                            display,
                            "read",
                            format!("{}: {error}", file.source_path.display()),
                        );
                        continue;
                    }
                };
                if dependency::should_skip_minified(&file.source_path, &source, file.forced_entry) {
                    // Policy exclusions are intentionally not seen: cleanup
                    // below removes a row that no longer belongs in the corpus.
                    outcome.dependency_skipped += 1;
                    continue;
                }
                seen.insert(display.clone());
                outcome.dependency_bytes += file.bytes;
                let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
                let role = file_role::classify(Path::new(&file.package_path), &source);
                if let Some((id, old_hash, old_role, old_package, old_package_path)) =
                    existing.get(&display)
                    && *old_hash == hash
                    && *old_package == package_id
                    && *old_package_path == file.package_path
                {
                    if old_role != role {
                        conn.execute("UPDATE files SET role=?1 WHERE id=?2", params![role, id])?;
                    }
                    outcome.unchanged += 1;
                    outcome.dependency_files += 1;
                    continue;
                }
                match extract_file(&file.source_path, &display, &source) {
                    Ok(data) => {
                        if let Some((old_id, _, _, _, _)) = existing.get(&display) {
                            store::delete_file(conn, *old_id)?;
                        }
                        let identity = FileIdentity {
                            path: &display,
                            hash: &hash,
                            role,
                            origin: "dependency",
                            package_instance_id: Some(package_id),
                            package_path: Some(&file.package_path),
                        };
                        let (chunks, refs) = insert_file(conn, &identity, &data)?;
                        outcome.indexed += 1;
                        outcome.dependency_files += 1;
                        outcome.chunks += chunks;
                        outcome.refs += refs;
                    }
                    Err(error) => {
                        outcome.record_failure(display, "extract", error);
                    }
                }
            }
        }
        for (path, (id, _, _, _, _)) in &existing {
            if !seen.contains(path) {
                store::delete_file(conn, *id)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn dependency_display_path(package: &dependency::DiscoveredPackage, package_path: &str) -> String {
    let version = package.version.as_deref().unwrap_or("unknown");
    let locator = blake3::hash(package.canonical_root.to_string_lossy().as_bytes()).to_hex();
    format!(
        "dependency:{}@{}#{}/{package_path}",
        package.name,
        version,
        &locator[..8]
    )
}

/// Resolve every (file, request) pair to an in-repo file or external package.
pub fn resolve_module_edges(root: &Path, conn: &Connection) -> Result<()> {
    let workspace = crate::workspace::WorkspaceMap::build(root);
    let resolver = Resolver::new(resolver_options(
        workspace.aliases.clone(),
        Some(TsconfigDiscovery::Auto),
    ));
    // A tsconfig that fails to load fails every resolution under it — e.g.
    // `extends: "@scope/tsconfig-pkg/..."` in a monorepo without node_modules
    // installed. Retry such failures without tsconfig discovery so a broken
    // tsconfig degrades resolution instead of dropping the whole file's edges.
    let no_tsconfig = Resolver::new(resolver_options(workspace.aliases.clone(), None));
    // Third-party source must follow the package installation graph. Applying
    // workspace aliases inside it can redirect a dependency's own imports to
    // an unrelated first-party package with the same name.
    let dependency_resolver = Resolver::new(resolver_options(Vec::new(), None));
    let (file_ids, importer_paths) = {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, f.origin, f.package_path, f.package_instance_id,
                    p.canonical_root
             FROM files f
             LEFT JOIN package_instances p ON p.id=f.package_instance_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut by_path = HashMap::new();
        let mut by_id = HashMap::new();
        for row in rows {
            let (id, path, origin, package_path, package_instance, package_root) = row?;
            let physical =
                if origin == "dependency" {
                    PathBuf::from(package_root.ok_or_else(|| {
                        anyhow::anyhow!("dependency file {path} has no package root")
                    })?)
                    .join(package_path.ok_or_else(|| {
                        anyhow::anyhow!("dependency file {path} has no package path")
                    })?)
                } else {
                    root.join(path)
                };
            let physical = physical.canonicalize().unwrap_or(physical);
            by_path.insert(physical.clone(), (id, package_instance));
            by_id.insert(id, (physical, origin == "dependency"));
        }
        (by_path, by_id)
    };

    let mut package_roots: Vec<(PathBuf, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT canonical_root, id FROM package_instances WHERE origin='dependency'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                row.get::<_, i64>(1)?,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    package_roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));

    let pairs: Vec<(i64, String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT f.id, requests.request, max(requests.is_runtime) = 0
             FROM files f
             JOIN (
               SELECT file_id, request, 1 AS is_runtime FROM imports
               UNION ALL
               SELECT file_id, from_request, 1 FROM exports WHERE from_request IS NOT NULL
               UNION ALL
               SELECT file_id, target_request, 1 FROM refs WHERE target_request IS NOT NULL
               UNION ALL
               SELECT file_id, request, 0 FROM contract_imports
               UNION ALL
               SELECT file_id, from_request, 0 FROM contract_exports
                 WHERE from_request IS NOT NULL
             ) requests ON requests.file_id = f.id
             GROUP BY f.id, requests.request",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM module_edges", [])?;
    let mut ins = conn.prepare_cached(
        "INSERT INTO module_edges(
           from_file, request, to_file, package, resolution, package_instance_id, type_only
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    type Resolved = (
        Option<i64>,
        Option<String>,
        Option<&'static str>,
        Option<i64>,
    );
    let mut cache: HashMap<(PathBuf, String), Resolved> = HashMap::new();
    for (file_id, request, type_only) in pairs {
        let (importer, dependency_importer) = importer_paths
            .get(&file_id)
            .ok_or_else(|| anyhow::anyhow!("indexed importer {file_id} has no physical path"))?
            .clone();
        let key = (importer.clone(), request.clone());
        let (to_file, package, resolution, package_instance) = cache
            .entry(key)
            .or_insert_with(|| {
                match if dependency_importer {
                    dependency_resolver.resolve_file(&importer, &request)
                } else {
                    resolver
                        .resolve_file(&importer, &request)
                        .or_else(|_| no_tsconfig.resolve_file(&importer, &request))
                } {
                    Ok(resolution) => {
                        let path = resolution.path().to_path_buf();
                        let path = path.canonicalize().unwrap_or(path);
                        match file_ids.get(&path) {
                            Some((id, package_instance)) => (
                                Some(*id),
                                None,
                                Some(workspace.classify(&request)),
                                *package_instance,
                            ),
                            None if external_package_name(&request).is_none() => {
                                // Resolved to a real but un-indexable file
                                // (styles, assets, JSON): keep the edge as
                                // evidence without inventing a package.
                                (None, None, Some("unresolved"), None)
                            }
                            None => {
                                let package = external_package_name(&request)
                                    .expect("guarded external package request");
                                let package_instance = package_roots
                                    .iter()
                                    .find(|(root, _)| path.starts_with(root))
                                    .map(|(_, id)| *id);
                                (None, Some(package), None, package_instance)
                            }
                        }
                    }
                    Err(_) if external_package_name(&request).is_none() => {
                        (None, None, Some("unresolved"), None)
                    }
                    Err(_) => (None, external_package_name(&request), None, None),
                }
            })
            .clone();
        ins.execute(params![
            file_id,
            request,
            to_file,
            package,
            resolution,
            package_instance,
            type_only,
        ])?;
    }
    drop(ins);
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Return the package boundary for a syntactically valid bare package
/// specifier. Relative/absolute paths, package-import aliases (`#name`),
/// bundler aliases (`~/`, `@/`), URLs, and Windows paths are unresolved
/// evidence rather than invented `pkg:` identities.
fn external_package_name(request: &str) -> Option<String> {
    if let Some((scheme, _)) = request.split_once(':') {
        return matches!(scheme, "node" | "bun").then(|| package_name(request));
    }
    if request.is_empty()
        || request.starts_with(['.', '/', '~', '#'])
        || request.contains(['\\', '%', '?'])
    {
        return None;
    }
    let mut parts = request.split('/');
    let first = parts.next()?;
    if let Some(scope) = first.strip_prefix('@') {
        let name = parts.next()?;
        if scope.is_empty() || !valid_package_segment(scope) || !valid_package_segment(name) {
            return None;
        }
        Some(format!("@{scope}/{name}"))
    } else {
        valid_package_segment(first).then(|| first.to_string())
    }
}

fn valid_package_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '\\' | '%' | ':' | '#' | '?' | '@')
        })
}

/// "@scope/pkg/sub/path" -> "@scope/pkg"; "./x" stays as-is (unresolved relative).
pub(crate) fn package_name(request: &str) -> String {
    if request.starts_with('.') || request.starts_with('/') {
        return request.to_string();
    }
    let mut parts = request.splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(scope), Some(name)) if scope.starts_with('@') => format!("{scope}/{name}"),
        (Some(name), _) => name.to_string(),
        _ => request.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::{
        IndexOptions, index_repo, index_repo_with_options, index_repo_without_extraction_reset,
    };
    use crate::{origin, query, search, store, structural};

    #[test]
    fn reports_the_file_and_stage_for_read_failures() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("bad.ts"), [0xff, 0xfe])?;
        let conn = store::open(repo.path())?;

        let outcome = index_repo(repo.path(), &conn)?;

        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].path, "bad.ts");
        assert_eq!(outcome.failures[0].stage, "read");
        assert!(!outcome.failures[0].error.is_empty());
        Ok(())
    }

    #[test]
    fn extraction_version_change_forces_unchanged_files_through_extraction() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "export function run() { return process.env.API_KEY; }\n",
        )?;
        let conn = store::open(repo.path())?;
        let first = index_repo(repo.path(), &conn)?;
        assert_eq!(first.indexed, 1);
        let second = index_repo(repo.path(), &conn)?;
        assert_eq!((second.indexed, second.unchanged), (0, 1));

        conn.execute(
            "UPDATE meta SET value='legacy' WHERE key='extraction_version'",
            [],
        )?;
        let third = index_repo(repo.path(), &conn)?;
        assert_eq!((third.indexed, third.unchanged), (1, 0));
        let environment_occurrences: i64 = conn.query_row(
            "SELECT count(*) FROM entity_occurrences occurrence
             JOIN entities entity ON entity.id=occurrence.entity_id
             WHERE entity.plane='general' AND entity.entity_type='environment_variable'
               AND entity.name='API_KEY'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(environment_occurrences, 1);
        Ok(())
    }

    #[test]
    fn unresolved_non_package_imports_carry_no_package_identity() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("style.module.scss"),
            ".a { color: red; }\n",
        )?;
        fs::write(
            repo.path().join("main.ts"),
            "import styles from './style.module.scss';\n\
             import Tree from './Tree.vue';\n\
             import cover from '~/assets/cover.png';\n\
             import app from '@/components/app';\n\
             import internal from '#internal/widget';\n\
             import icon from 'C:\\\\assets\\\\icon.svg';\n\
             import missing from 'not-installed-pkg';\n\
             import scoped from '@scope/not-installed/subpath';\n\
             export const view = () => [styles, Tree, cover, app, internal, icon, missing, scoped];\n",
        )?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;

        let edge = |request: &str| -> Result<(Option<i64>, Option<String>, Option<String>)> {
            Ok(conn.query_row(
                "SELECT edge.to_file, edge.package, edge.resolution
                 FROM module_edges edge
                 JOIN files source ON source.id=edge.from_file
                 WHERE source.path='main.ts' AND edge.request=?1",
                [request],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        };
        // Resolved to a real but un-indexable asset, and not resolvable at
        // all: both keep the edge as evidence with no package identity.
        assert_eq!(
            edge("./style.module.scss")?,
            (None, None, Some("unresolved".into()))
        );
        assert_eq!(edge("./Tree.vue")?, (None, None, Some("unresolved".into())));
        // Bundler aliases, package-import aliases, and Windows paths are not
        // installable package identities.
        assert_eq!(
            edge("~/assets/cover.png")?,
            (None, None, Some("unresolved".into()))
        );
        assert_eq!(
            edge("@/components/app")?,
            (None, None, Some("unresolved".into()))
        );
        assert_eq!(
            edge("#internal/widget")?,
            (None, None, Some("unresolved".into()))
        );
        assert_eq!(
            edge(r"C:\assets\icon.svg")?,
            (None, None, Some("unresolved".into()))
        );
        // Bare specifiers stay classified as external packages.
        assert_eq!(
            edge("not-installed-pkg")?,
            (None, Some("not-installed-pkg".into()), None)
        );
        assert_eq!(
            edge("@scope/not-installed/subpath")?,
            (None, Some("@scope/not-installed".into()), None)
        );

        let bogus_packages: i64 = conn.query_row(
            "SELECT count(*) FROM graph_nodes
             WHERE node_key LIKE 'pkg:.%' OR node_key LIKE 'pkg:/%'
                OR node_key LIKE 'pkg:~%' OR node_key LIKE 'pkg:@/%'
                OR node_key LIKE 'pkg:#%' OR node_key LIKE 'pkg:C:%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            bogus_packages, 0,
            "relative requests must not mint pkg: nodes"
        );
        let package_hub: i64 = conn.query_row(
            "SELECT count(*) FROM graph_nodes WHERE node_key='pkg:not-installed-pkg'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(package_hub, 1);
        let unresolved_projected: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges
             WHERE kind IN ('import','imports_types','imports_package','imports_package_types')
               AND (detail_json LIKE '%Tree.vue%' OR detail_json LIKE '%scss%')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(unresolved_projected, 0);
        let dangling: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges edge
             LEFT JOIN graph_nodes node ON node.node_key=edge.dst_key
             WHERE node.node_key IS NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(dangling, 0);
        Ok(())
    }

    #[test]
    fn noop_reindex_republishes_projection_without_rebuild() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("lib.ts"), "export const lib = 1;\n")?;
        fs::write(
            repo.path().join("main.ts"),
            "import { lib } from 'lib';\nexport const main = () => lib;\n",
        )?;
        let conn = store::open(repo.path())?;
        let meta_snapshot = || -> Result<String> {
            Ok(
                conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                    row.get(0)
                })?,
            )
        };
        let edge_count = || -> Result<i64> {
            Ok(conn.query_row("SELECT count(*) FROM resolved_edges", [], |row| row.get(0))?)
        };

        let first = index_repo(repo.path(), &conn)?;
        assert!(first.projection_rebuilt);
        let original_snapshot = meta_snapshot()?;
        let original_edges = edge_count()?;
        let original_neighborhood = structural::neighborhood(
            &conn,
            "main.ts:main",
            &structural::NeighborhoodOptions::default(),
        )?;

        let second = index_repo(repo.path(), &conn)?;
        assert!(!second.projection_rebuilt, "no-op must keep the projection");
        assert_eq!(meta_snapshot()?, original_snapshot);
        assert_eq!(edge_count()?, original_edges);

        // Resolution inputs live outside indexed content: a new tsconfig
        // remaps 'lib' onto ./lib.ts without changing any indexed file. The
        // graph and its public snapshot must both change.
        fs::write(
            repo.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {"lib": ["./lib.ts"]}}}"#,
        )?;
        let third = index_repo(repo.path(), &conn)?;
        assert!(
            third.projection_rebuilt,
            "resolution change without content change must rebuild"
        );
        assert_ne!(meta_snapshot()?, original_snapshot);
        let updated_neighborhood = structural::neighborhood(
            &conn,
            &original_neighborhood.resolved_anchor,
            &structural::NeighborhoodOptions {
                expected_snapshot: Some(original_neighborhood.snapshot),
                ..Default::default()
            },
        )?;
        assert_eq!(updated_neighborhood.anchor_status, "re-resolved");
        let target: Option<String> = conn.query_row(
            "SELECT target.path FROM module_edges edge
             JOIN files source ON source.id=edge.from_file
             LEFT JOIN files target ON target.id=edge.to_file
             WHERE source.path='main.ts' AND edge.request='lib'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(target, Some("lib.ts".into()));
        let fourth = index_repo(repo.path(), &conn)?;
        assert!(!fourth.projection_rebuilt);

        fs::write(repo.path().join("lib.ts"), "export const lib = 2;\n")?;
        let fifth = index_repo(repo.path(), &conn)?;
        assert!(fifth.projection_rebuilt, "content change must rebuild");
        assert_ne!(meta_snapshot()?, original_snapshot);

        fs::remove_file(repo.path().join("main.ts"))?;
        let sixth = index_repo(repo.path(), &conn)?;
        assert!(sixth.projection_rebuilt, "deletion must rebuild");
        let seventh = index_repo(repo.path(), &conn)?;
        assert!(!seventh.projection_rebuilt);
        Ok(())
    }

    #[test]
    fn resolves_paths_from_the_importers_nearest_tsconfig() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let app = repo.path().join("packages/app");
        fs::create_dir_all(app.join("src"))?;
        fs::write(
            app.join("tsconfig.json"),
            r#"{
                "compilerOptions": { "paths": { "src/*": ["./src/*"] } },
                "include": ["src/**/*.ts"]
            }"#,
        )?;
        fs::write(
            app.join("src/main.ts"),
            "import { helper } from 'src/helper';\nexport const main = () => helper();\n",
        )?;
        fs::write(
            app.join("src/helper.ts"),
            "export const helper = () => 1;\n",
        )?;

        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let resolved: (Option<String>, Option<String>) = conn.query_row(
            "SELECT target.path, edge.package
             FROM module_edges edge
             JOIN files source ON source.id=edge.from_file
             LEFT JOIN files target ON target.id=edge.to_file
             WHERE source.path='packages/app/src/main.ts' AND edge.request='src/helper'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(resolved, (Some("packages/app/src/helper.ts".into()), None));
        Ok(())
    }

    #[test]
    fn resolves_workspace_package_imports_to_internal_files() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )?;

        // Library package: main points at untracked dist output, but the
        // module field names the source entry directly (manifest truth).
        let lib = repo.path().join("packages/lib");
        fs::create_dir_all(lib.join("src/utils"))?;
        fs::write(
            lib.join("package.json"),
            r#"{"name": "@acme/lib", "main": "dist/index.js", "module": "src/index.ts"}"#,
        )?;
        fs::write(
            lib.join("src/index.ts"),
            "export const greet = () => 'hi';\n",
        )?;
        fs::write(
            lib.join("src/utils/format.ts"),
            "export const fmt = (s: string) => s;\n",
        )?;

        // Subpath-only package (no "." export, no root entry) whose wildcard
        // export re-roots the tree: src/scrub.ts is a decoy the generic src/
        // prefix would pick; the exported file is src/inner/scrub.ts.
        let tools = repo.path().join("packages/tools");
        fs::create_dir_all(tools.join("src/inner"))?;
        fs::write(
            tools.join("package.json"),
            r#"{"name": "@acme/tools", "exports": {"./*": "./dist/inner/*.js"}}"#,
        )?;
        fs::write(tools.join("src/scrub.ts"), "export const decoy = 1;\n")?;
        fs::write(
            tools.join("src/inner/scrub.ts"),
            "export const scrub = (s: string) => s;\n",
        )?;

        let app = repo.path().join("packages/app");
        fs::create_dir_all(app.join("src"))?;
        fs::write(app.join("package.json"), r#"{"name": "@acme/app"}"#)?;
        fs::write(
            app.join("src/main.ts"),
            "import { greet } from '@acme/lib';\n\
             import { fmt } from '@acme/lib/utils/format';\n\
             import { fmt as distFmt } from '@acme/lib/dist/utils/format';\n\
             import { scrub } from '@acme/tools/scrub';\n\
             import { readFile } from 'node:fs';\n\
             import lodash from 'lodash';\n\
             export const main = () => scrub(fmt(greet())) + distFmt('');\n",
        )?;

        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        type Edge = (Option<String>, Option<String>, Option<String>);
        let edge = |request: &str| -> Result<Edge> {
            Ok(conn.query_row(
                "SELECT target.path, edge.package, edge.resolution
                 FROM module_edges edge
                 JOIN files source ON source.id=edge.from_file
                 LEFT JOIN files target ON target.id=edge.to_file
                 WHERE source.path='packages/app/src/main.ts' AND edge.request=?1",
                [request],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        };
        // Bare package import -> the manifest-named source entry: certain.
        assert_eq!(
            edge("@acme/lib")?,
            (
                Some("packages/lib/src/index.ts".into()),
                None,
                Some("workspace".into())
            )
        );
        // Subpath through the src/ layout heuristic: internal but inferred.
        assert_eq!(
            edge("@acme/lib/utils/format")?,
            (
                Some("packages/lib/src/utils/format.ts".into()),
                None,
                Some("workspace-inferred".into())
            )
        );
        // Wildcard export translation beats the generic src/ prefix (which
        // would have picked the decoy src/scrub.ts).
        assert_eq!(
            edge("@acme/tools/scrub")?,
            (
                Some("packages/tools/src/inner/scrub.ts".into()),
                None,
                Some("workspace-inferred".into())
            )
        );
        // Imports naming build output land on the mirrored source tree.
        assert_eq!(
            edge("@acme/lib/dist/utils/format")?,
            (
                Some("packages/lib/src/utils/format.ts".into()),
                None,
                Some("workspace-inferred".into())
            )
        );
        // Non-workspace imports keep their external package classification.
        assert_eq!(edge("lodash")?, (None, Some("lodash".into()), None));
        assert_eq!(edge("node:fs")?, (None, Some("node:fs".into()), None));

        // The structural projection downgrades heuristic mappings: the
        // manifest-backed import stays certain, inferred ones cap at likely —
        // including references that cross an inferred edge.
        let projected = |detail: &str| -> Result<(String, String)> {
            Ok(conn.query_row(
                "SELECT confidence, provenance FROM resolved_edges
                 WHERE kind='import' AND detail_json=?1",
                [detail],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        };
        assert_eq!(
            projected(r#"{"request":"@acme/lib"}"#)?,
            ("certain".into(), "workspace".into())
        );
        assert_eq!(
            projected(r#"{"request":"@acme/lib/utils/format"}"#)?,
            ("likely".into(), "workspace-inferred".into())
        );
        let fmt_call: (String, String) = conn.query_row(
            "SELECT confidence, provenance FROM resolved_edges
             WHERE kind='call' AND detail_json LIKE '%\"targetName\":\"fmt\"%'
               AND provenance LIKE 'semantic+%' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            fmt_call,
            ("likely".into(), "semantic+resolver-inferred".into())
        );
        let greet_call: (String, String) = conn.query_row(
            "SELECT confidence, provenance FROM resolved_edges
             WHERE kind='call' AND detail_json LIKE '%\"targetName\":\"greet\"%'
               AND provenance LIKE 'semantic+%' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(greet_call, ("certain".into(), "semantic+resolver".into()));
        Ok(())
    }

    #[test]
    fn unloadable_tsconfig_degrades_to_plain_resolution() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )?;

        let lib = repo.path().join("packages/lib");
        fs::create_dir_all(lib.join("src"))?;
        fs::write(lib.join("package.json"), r#"{"name": "@acme/lib"}"#)?;
        fs::write(lib.join("src/index.ts"), "export const lib = 1;\n")?;

        // The n8n shape: tsconfig extends a workspace package by bare name,
        // which cannot resolve without node_modules installed.
        let app = repo.path().join("packages/app");
        fs::create_dir_all(app.join("src"))?;
        fs::write(app.join("package.json"), r#"{"name": "@acme/app"}"#)?;
        fs::write(
            app.join("tsconfig.json"),
            r#"{"extends": "@acme/tsconfig/base.json", "include": ["src"]}"#,
        )?;
        fs::write(
            app.join("src/helper.ts"),
            "export const helper = () => 1;\n",
        )?;
        fs::write(
            app.join("src/main.ts"),
            "import { helper } from './helper';\n\
             import { lib } from '@acme/lib';\n\
             export const main = () => helper() + lib;\n",
        )?;

        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let edge = |request: &str| -> Result<(Option<String>, Option<String>)> {
            Ok(conn.query_row(
                "SELECT target.path, edge.package
                 FROM module_edges edge
                 JOIN files source ON source.id=edge.from_file
                 LEFT JOIN files target ON target.id=edge.to_file
                 WHERE source.path='packages/app/src/main.ts' AND edge.request=?1",
                [request],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        };
        assert_eq!(
            edge("./helper")?,
            (Some("packages/app/src/helper.ts".into()), None)
        );
        assert_eq!(
            edge("@acme/lib")?,
            (Some("packages/lib/src/index.ts".into()), None)
        );
        Ok(())
    }

    #[test]
    fn workspace_ownership_uses_literal_path_prefixes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages:\n  - packages/my_pkg\n",
        )?;
        let workspace = repo.path().join("packages/my_pkg");
        fs::create_dir_all(workspace.join("src"))?;
        fs::write(
            workspace.join("package.json"),
            r#"{"name":"my-pkg","version":"1.0.0"}"#,
        )?;
        fs::write(workspace.join("src/index.ts"), "export const owned = 1;\n")?;
        let sibling = repo.path().join("packages/my1pkg/src");
        fs::create_dir_all(&sibling)?;
        fs::write(sibling.join("index.ts"), "export const sibling = 1;\n")?;

        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let origins: (String, String) = conn.query_row(
            "SELECT
               (SELECT origin FROM files WHERE path='packages/my_pkg/src/index.ts'),
               (SELECT origin FROM files WHERE path='packages/my1pkg/src/index.ts')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(origins, ("workspace".into(), "repository".into()));
        Ok(())
    }

    #[test]
    fn indexes_scoped_dependency_selected_by_exact_name() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "import { scoped } from '@scope/pkg';\nexport const result = scoped();\n",
        )?;
        let dependency = repo.path().join("node_modules/@scope/pkg");
        fs::create_dir_all(&dependency)?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"@scope/pkg","version":"2.0.0","main":"index.js"}"#,
        )?;
        fs::write(
            dependency.join("index.js"),
            "export const scoped = () => 2;\n",
        )?;

        let conn = store::open(repo.path())?;
        index_repo_with_options(
            repo.path(),
            &conn,
            &IndexOptions {
                dependencies: vec!["@scope/pkg".into()],
                ..Default::default()
            },
        )?;
        let resolved: (String, String, String) = conn.query_row(
            "SELECT package.name, package.version, target.package_path
             FROM module_edges edge
             JOIN package_instances package ON package.id=edge.package_instance_id
             JOIN files target ON target.id=edge.to_file
             JOIN files source ON source.id=edge.from_file
             WHERE source.path='main.ts' AND edge.request='@scope/pkg'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            resolved,
            ("@scope/pkg".into(), "2.0.0".into(), "index.js".into())
        );
        Ok(())
    }

    #[test]
    fn indexes_only_selected_dependency_files_and_removes_them_when_omitted() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "import { publicApi } from 'selected-dep';\n\
             export const internal = () => 'first-party';\n\
             export const result = publicApi();\n",
        )?;

        let dependency = repo.path().join("node_modules/selected-dep");
        fs::create_dir_all(dependency.join("dist"))?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"selected-dep","version":"1.2.3","main":"dist/index.js"}"#,
        )?;
        fs::write(
            dependency.join("dist/index.js"),
            "export { internal as publicApi } from './internal.js';\n",
        )?;
        fs::write(
            dependency.join("dist/internal.js"),
            "export const internal = () => 42;\n\
             export const dependencyOnlyMarker = true;\n",
        )?;

        let ignored = repo.path().join("node_modules/ignored-dep");
        fs::create_dir_all(&ignored)?;
        fs::write(
            ignored.join("package.json"),
            r#"{"name":"ignored-dep","version":"9.9.9","main":"index.js"}"#,
        )?;
        fs::write(ignored.join("index.js"), "export const ignored = true;\n")?;

        let conn = store::open(repo.path())?;
        let selected = vec!["selected-dep".to_string()];
        let first = index_repo_with_options(
            repo.path(),
            &conn,
            &IndexOptions {
                dependencies: selected.clone(),
                ..Default::default()
            },
        )?;
        assert_eq!(first.dependency_packages, 1);
        assert_eq!(first.dependency_files, 2);
        assert!(first.dependency_bytes > 0);

        let package: (String, String, String) = conn.query_row(
            "SELECT origin, name, version FROM package_instances WHERE name='selected-dep'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            package,
            ("dependency".into(), "selected-dep".into(), "1.2.3".into())
        );
        let dependency_files: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT origin, package_path FROM files
                 WHERE origin='dependency' ORDER BY package_path",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        assert_eq!(
            dependency_files,
            vec![
                ("dependency".into(), "dist/index.js".into()),
                ("dependency".into(), "dist/internal.js".into()),
            ]
        );
        let edge: (String, i64) = conn.query_row(
            "SELECT target.package_path, edge.package_instance_id
             FROM module_edges edge
             JOIN files source ON source.id=edge.from_file
             JOIN files target ON target.id=edge.to_file
             WHERE source.path='main.ts' AND edge.request='selected-dep'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(edge.0, "dist/index.js");
        let package_hub: (String, String) = conn.query_row(
            "SELECT node_key, meta_json FROM graph_nodes
             WHERE native_table='package_instances' AND native_id=?1",
            [edge.1],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(package_hub.0.starts_with("pkg:selected-dep@1.2.3#"));
        assert!(package_hub.1.contains(r#""origin":"dependency""#));
        let boundary_edges: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT kind, dst_key FROM resolved_edges
                 WHERE (kind='imports_package' AND src_key='file:main.ts')
                    OR (kind='contains_module' AND src_key=?1)
                 ORDER BY kind, dst_key",
            )?;
            let rows = stmt.query_map([&package_hub.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        assert_eq!(
            boundary_edges,
            vec![
                (
                    "contains_module".into(),
                    format!(
                        "file:{}",
                        conn.query_row(
                            "SELECT path FROM files WHERE package_instance_id=?1
                             AND package_path='dist/index.js'",
                            [edge.1],
                            |row| row.get::<_, String>(0),
                        )?
                    ),
                ),
                ("imports_package".into(), package_hub.0.clone()),
            ]
        );

        let default_search = search::search(
            &conn,
            None,
            "dependencyOnlyMarker",
            &search::SearchOptions::default(),
        )?;
        assert!(default_search.hits.is_empty());
        let dependency_search = search::search(
            &conn,
            None,
            "dependencyOnlyMarker",
            &search::SearchOptions {
                file_origins: vec!["dependency".into()],
                ..Default::default()
            },
        )?;
        assert!(!dependency_search.hits.is_empty());
        assert!(
            dependency_search
                .hits
                .iter()
                .all(|hit| hit.file_origin == "dependency")
        );
        assert!(
            query::find_symbols_in_origins(&conn, "dependencyOnlyMarker", &origin::defaults(),)?
                .is_empty()
        );
        let dependency_definitions =
            query::find_symbols_in_origins(&conn, "dependencyOnlyMarker", &["dependency".into()])?;
        assert_eq!(dependency_definitions.len(), 1);
        assert_eq!(dependency_definitions[0].file_origin, "dependency");
        let first_party_anchor =
            structural::resolve_current_anchor_in_origins(&conn, "internal", &origin::defaults())?;
        assert!(first_party_anchor.starts_with("sym:main.ts#::internal@"));
        assert!(structural::resolve_current_anchor(&conn, "internal").is_err());

        let default_boundary = structural::neighborhood(
            &conn,
            "file:main.ts",
            &structural::NeighborhoodOptions {
                direction: "out".into(),
                ..Default::default()
            },
        )?;
        assert!(
            default_boundary
                .nodes
                .iter()
                .any(|node| node.key == package_hub.0)
        );
        assert!(
            default_boundary
                .nodes
                .iter()
                .all(|node| node.file_origin.as_deref() != Some("dependency"))
        );
        let dependency_boundary = structural::neighborhood(
            &conn,
            "file:main.ts",
            &structural::NeighborhoodOptions {
                direction: "out".into(),
                file_origins: vec!["repository".into(), "dependency".into()],
                ..Default::default()
            },
        )?;
        assert!(
            dependency_boundary
                .nodes
                .iter()
                .any(|node| node.file_origin.as_deref() == Some("dependency"))
        );

        let second = index_repo_with_options(
            repo.path(),
            &conn,
            &IndexOptions {
                dependencies: selected,
                ..Default::default()
            },
        )?;
        assert_eq!(second.dependency_files, 2);
        assert_eq!(second.indexed, 0);
        assert_eq!(second.unchanged, 3);

        index_repo(repo.path(), &conn)?;
        let remaining_dependencies: i64 = conn.query_row(
            "SELECT count(*) FROM files WHERE origin='dependency'",
            [],
            |row| row.get(0),
        )?;
        let remaining_instances: i64 = conn.query_row(
            "SELECT count(*) FROM package_instances WHERE origin='dependency'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!((remaining_dependencies, remaining_instances), (0, 0));
        let chunk_counts: (i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM chunks),
               (SELECT count(*) FROM chunks_fts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(chunk_counts.0, chunk_counts.1);
        let orphan_match: i64 = conn.query_row(
            "SELECT count(*) FROM chunks_fts
             WHERE chunks_fts MATCH 'dependencyOnlyMarker'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(orphan_match, 0);
        let fallback: (Option<i64>, Option<String>) = conn.query_row(
            "SELECT edge.to_file, edge.package FROM module_edges edge
             JOIN files source ON source.id=edge.from_file
             WHERE source.path='main.ts' AND edge.request='selected-dep'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(fallback, (None, Some("selected-dep".into())));
        Ok(())
    }

    /// Render one query with every rowid replaced by content identity, in a
    /// total order over all output columns, so two databases built by
    /// different code paths can be compared byte-for-byte.
    fn dump_section(conn: &rusqlite::Connection, sql: &str) -> Result<String> {
        let columns = conn.prepare(sql)?.column_count();
        let order: Vec<String> = (1..=columns).map(|index| index.to_string()).collect();
        let wrapped = format!("SELECT * FROM ({sql}) ORDER BY {}", order.join(","));
        let mut stmt = conn.prepare(&wrapped)?;
        let mut rows = stmt.query([])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            for index in 0..columns {
                use rusqlite::types::ValueRef;
                match row.get_ref(index)? {
                    ValueRef::Null => out.push_str("<null>"),
                    ValueRef::Integer(value) => out.push_str(&value.to_string()),
                    ValueRef::Real(value) => out.push_str(&value.to_string()),
                    ValueRef::Text(value) => out.push_str(&String::from_utf8_lossy(value)),
                    ValueRef::Blob(value) => out.push_str(&format!("<blob:{}>", value.len())),
                }
                out.push('\x1f');
            }
            out.push('\n');
        }
        Ok(out)
    }

    /// Every canonical and projected table, keyed by paths/keys/spans instead
    /// of rowids. `snapshot` and `resolution_hash` are excluded: the
    /// resolution hash digests file ids, which every full re-index reassigns
    /// — on the per-file path and the wholesale-reset path alike.
    fn canonical_dump(conn: &rusqlite::Connection) -> Result<Vec<(&'static str, String)>> {
        const SECTIONS: &[(&str, &str)] = &[
            (
                "counts",
                "SELECT (SELECT count(*) FROM chunks), (SELECT count(*) FROM chunks_fts)",
            ),
            (
                "files",
                "SELECT f.path, f.hash, f.role, f.origin, f.package_path,
                        p.origin, p.name, p.version, p.canonical_root, p.locator,
                        p.manifest_hash, p.status
                 FROM files f LEFT JOIN package_instances p ON p.id=f.package_instance_id",
            ),
            (
                "chunks",
                "SELECT f.path, c.kind, c.name, c.scope_chain, c.symbols, c.start, c.end,
                        c.start_line, c.end_line, c.hash, c.content
                 FROM chunks c JOIN files f ON f.id=c.file_id",
            ),
            (
                "chunks_fts",
                "SELECT f.path, c.start, fts.content, fts.name, fts.symbols, fts.path
                 FROM chunks_fts fts
                 JOIN chunks c ON c.id=fts.rowid
                 JOIN files f ON f.id=c.file_id",
            ),
            (
                "symbols",
                "SELECT f.path, s.name, s.kind, s.start, s.end, s.decl_start, s.decl_end,
                        s.scope_chain, s.line, s.exported
                 FROM symbols s JOIN files f ON f.id=s.file_id",
            ),
            (
                "imports",
                "SELECT f.path, i.local_name, i.imported_name, i.request
                 FROM imports i JOIN files f ON f.id=i.file_id",
            ),
            (
                "exports",
                "SELECT f.path, e.export_name, e.local_name, e.from_request, e.from_name
                 FROM exports e JOIN files f ON f.id=e.file_id",
            ),
            (
                "contract_imports",
                "SELECT f.path, i.local_name, i.imported_name, i.request
                 FROM contract_imports i JOIN files f ON f.id=i.file_id",
            ),
            (
                "contract_exports",
                "SELECT f.path, e.export_name, e.local_name, e.from_request, e.from_name
                 FROM contract_exports e JOIN files f ON f.id=e.file_id",
            ),
            (
                "module_edges",
                "SELECT src.path, e.request, dst.path, e.package, e.resolution,
                        p.name, p.version, p.canonical_root, e.type_only
                 FROM module_edges e
                 JOIN files src ON src.id=e.from_file
                 LEFT JOIN files dst ON dst.id=e.to_file
                 LEFT JOIN package_instances p ON p.id=e.package_instance_id",
            ),
            (
                "refs",
                "SELECT f.path, c.start, r.start, r.line, r.kind, r.confidence,
                        r.target_request, r.target_name, r.local, r.detail
                 FROM refs r
                 JOIN files f ON f.id=r.file_id
                 LEFT JOIN chunks c ON c.id=r.chunk_id",
            ),
            (
                "events",
                "SELECT f.path, c.start, e.line, e.role, e.name, e.method
                 FROM events e
                 JOIN files f ON f.id=e.file_id
                 LEFT JOIN chunks c ON c.id=e.chunk_id",
            ),
            (
                "member_calls",
                "SELECT f.path, c.start, m.start, m.end, m.line, m.end_line,
                        m.prop, m.object, m.receiver
                 FROM member_calls m
                 JOIN files f ON f.id=m.file_id
                 LEFT JOIN chunks c ON c.id=m.chunk_id",
            ),
            (
                "entity_sites",
                "SELECT f.path, c.start, s.start, s.end, s.line, s.end_line, s.plane,
                        s.entity_type, s.role, s.identity_kind, s.identity_name,
                        s.identity_start, s.target_name, s.target_start, s.extractor,
                        s.provenance, s.confidence, s.detail_json
                 FROM entity_sites s
                 JOIN files f ON f.id=s.file_id
                 LEFT JOIN chunks c ON c.id=s.chunk_id",
            ),
            (
                "entities",
                "SELECT entity_key, plane, entity_type, name, identity_anchor, meta_json
                 FROM entities",
            ),
            (
                "entity_occurrences",
                "SELECT en.entity_key, f.path, site.start, o.start, o.end, o.line,
                        o.end_line, o.role, o.extractor, o.provenance, o.confidence,
                        o.detail_json
                 FROM entity_occurrences o
                 JOIN entities en ON en.id=o.entity_id
                 JOIN entity_sites site ON site.id=o.site_id
                 JOIN files f ON f.id=o.file_id",
            ),
            (
                // detail_json embeds occurrence/site rowid pointers, which
                // every full re-index reassigns; the join already pins the
                // same identity by content.
                "entity_edges",
                "SELECT en.entity_key, o.start, e.target_key, e.kind, e.confidence,
                        e.provenance,
                        json_remove(e.detail_json, '$.entityOccurrenceId', '$.entitySiteId')
                 FROM entity_edges e
                 JOIN entity_occurrences o ON o.id=e.occurrence_id
                 JOIN entities en ON en.id=o.entity_id",
            ),
            (
                "graph_nodes",
                "SELECT n.node_key, n.node_kind, n.native_table, n.display_name,
                        f.path, n.line, n.meta_json
                 FROM graph_nodes n LEFT JOIN files f ON f.id=n.file_id",
            ),
            (
                "resolved_edges",
                "SELECT e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                        f.path, e.line,
                        json_remove(e.detail_json, '$.entityOccurrenceId', '$.entitySiteId')
                 FROM resolved_edges e LEFT JOIN files f ON f.id=e.source_file_id",
            ),
            ("scout_runs", "SELECT * FROM scout_runs"),
            (
                "scout_classifications",
                "SELECT * FROM scout_classifications",
            ),
            ("semantic_artifacts", "SELECT * FROM semantic_artifacts"),
            ("semantic_supports", "SELECT * FROM semantic_supports"),
            ("semantic_relations", "SELECT * FROM semantic_relations"),
            (
                "embeddings",
                "SELECT e.chunk_hash, p.provider, p.model, p.config_fingerprint, p.dimensions
                 FROM embeddings e JOIN embedding_profiles p ON p.id=e.profile_id",
            ),
            (
                "meta",
                "SELECT key, value FROM meta WHERE key NOT IN ('snapshot', 'resolution_hash')",
            ),
        ];
        SECTIONS
            .iter()
            .map(|(name, sql)| Ok((*name, dump_section(conn, sql)?)))
            .collect()
    }

    #[test]
    fn forced_reextraction_reset_matches_per_file_replacement() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )?;
        let lib = repo.path().join("packages/lib");
        fs::create_dir_all(lib.join("src"))?;
        fs::write(
            lib.join("package.json"),
            r#"{"name": "@acme/lib", "module": "src/index.ts"}"#,
        )?;
        fs::write(
            lib.join("src/index.ts"),
            "export const greet = (name: string) => `hi ${name}`;\n\
             export interface Shape { id: string }\n",
        )?;
        fs::write(
            repo.path().join("helper.ts"),
            "export const helper = (value: string) => value.trim();\n",
        )?;
        fs::write(
            repo.path().join("main.ts"),
            "import { greet } from '@acme/lib';\n\
             import type { Shape } from '@acme/lib';\n\
             import { EventEmitter } from 'node:events';\n\
             import { helper } from './helper';\n\
             import { inner } from 'selected-dep';\n\
             import missing from 'not-installed-pkg';\n\
             const emitter = new EventEmitter();\n\
             emitter.on('ready', () => greet('x'));\n\
             emitter.emit('ready');\n\
             export function main(shape: Shape) {\n\
               const key = process.env.API_KEY;\n\
               return helper(greet(key ?? shape.id)) + inner() + missing;\n\
             }\n\
             export const spans = emitter.listeners(\n\
               'ready',\n\
             );\n",
        )?;
        let dependency = repo.path().join("node_modules/selected-dep");
        fs::create_dir_all(&dependency)?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"selected-dep","version":"1.2.3","main":"index.js"}"#,
        )?;
        fs::write(
            dependency.join("index.js"),
            "export { inner } from './inner.js';\n",
        )?;
        fs::write(
            dependency.join("inner.js"),
            "export const inner = () => 42;\n",
        )?;

        let databases = tempfile::tempdir()?;
        let per_file = store::open_path(&databases.path().join("per-file.db"))?;
        let reset = store::open_path(&databases.path().join("reset.db"))?;
        let options = IndexOptions {
            dependencies: vec!["selected-dep".into()],
            ..Default::default()
        };
        for conn in [&per_file, &reset] {
            let outcome = index_repo_with_options(repo.path(), conn, &options)?;
            assert!(!outcome.extraction_reset, "initial index must not reset");
            // Semantic memory that must survive a forced re-extraction on
            // both paths: a completed scout run, its classification, and an
            // artifact with one support.
            conn.execute_batch(
                "INSERT INTO scout_runs(
                   id, scout_kind, status, gateway_protocol, provider, model,
                   billing_path, prompt_version, source_snapshot,
                   input_fingerprint, request_hash, started_at, completed_at
                 ) VALUES(7, 'workflow', 'completed', 1, 'test', 'test-model',
                          'api', 'v1', 'snap', 'fp', 'req',
                          '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z');
                 INSERT INTO scout_classifications(
                   run_id, anchor_key, decision, role, evidence_json
                 ) VALUES(7, 'sym:main.ts#::main@10', 'defining', 'entry', '{}');
                 INSERT INTO semantic_artifacts(
                   id, artifact_type, canonical_name, body_json, model,
                   prompt_version, confidence, source_snapshot, created_at,
                   scout_run_id, input_fingerprint, artifact_fingerprint
                 ) VALUES(3, 'workflow', 'checkout', '{}', 'test-model', 'v1',
                          'likely', 'snap', '2026-01-01T00:01:00Z', 7, 'fp', 'af');
                 INSERT INTO semantic_supports(
                   artifact_id, claim_path, anchor_key, role, evidence_file,
                   evidence_start_line, evidence_end_line, source_hash,
                   context_hash, confidence
                 ) VALUES(3, '$.steps[0]', 'sym:main.ts#::main@10', 'entry',
                          'main.ts', 10, 13, 'sh', 'ch', 'likely');",
            )?;
            // The v15-style forced re-extraction: clear every hash and
            // invalidate the disposable projection and its public identity.
            conn.execute("UPDATE files SET hash = ''", [])?;
            conn.execute("DELETE FROM resolved_edges", [])?;
            conn.execute("DELETE FROM graph_nodes", [])?;
            conn.execute(
                "DELETE FROM meta
                 WHERE key IN ('snapshot', 'projection_version', 'resolution_hash')",
                [],
            )?;
        }

        let slow = index_repo_without_extraction_reset(repo.path(), &per_file, &options)?;
        assert!(!slow.extraction_reset);
        let fast = index_repo_with_options(repo.path(), &reset, &options)?;
        assert!(fast.extraction_reset, "cleared hashes must take the reset");
        assert_eq!(
            (fast.indexed, fast.unchanged, fast.failed),
            (slow.indexed, slow.unchanged, slow.failed)
        );

        for ((section, slow_rows), (_, fast_rows)) in canonical_dump(&per_file)?
            .iter()
            .zip(canonical_dump(&reset)?)
        {
            assert_eq!(
                slow_rows, &fast_rows,
                "section `{section}` diverged between per-file and reset paths"
            );
        }

        // Equality alone cannot prove survival; pin the preserved rows and a
        // live FTS index on the reset path explicitly.
        let (runs, artifacts, supports, classifications): (i64, i64, i64, i64) = reset.query_row(
            "SELECT (SELECT count(*) FROM scout_runs),
                    (SELECT count(*) FROM semantic_artifacts),
                    (SELECT count(*) FROM semantic_supports),
                    (SELECT count(*) FROM scout_classifications)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!((runs, artifacts, supports, classifications), (1, 1, 1, 1));
        let greet_hits: i64 = reset.query_row(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'greet'",
            [],
            |row| row.get(0),
        )?;
        assert!(greet_hits > 0, "rebuilt FTS index must serve matches");
        Ok(())
    }

    #[test]
    fn extraction_reset_triggers_only_at_majority_cleared() -> Result<()> {
        let repo = tempfile::tempdir()?;
        for name in ["one", "two", "three"] {
            fs::write(
                repo.path().join(format!("{name}.ts")),
                format!("export const {name} = 1;\n"),
            )?;
        }
        let conn = store::open(repo.path())?;
        let first = index_repo(repo.path(), &conn)?;
        assert!(!first.extraction_reset);

        let second = index_repo(repo.path(), &conn)?;
        assert!(!second.extraction_reset, "no-op run must stay incremental");
        assert_eq!((second.indexed, second.unchanged), (0, 3));

        conn.execute("UPDATE files SET hash='' WHERE path='one.ts'", [])?;
        let minority = index_repo(repo.path(), &conn)?;
        assert!(
            !minority.extraction_reset,
            "one cleared hash out of three must replace per file"
        );
        assert_eq!((minority.indexed, minority.unchanged), (1, 2));

        conn.execute(
            "UPDATE files SET hash='' WHERE path IN ('one.ts', 'two.ts')",
            [],
        )?;
        let majority = index_repo(repo.path(), &conn)?;
        assert!(majority.extraction_reset, "majority cleared must reset");
        assert_eq!((majority.indexed, majority.unchanged), (3, 0));
        Ok(())
    }

    #[test]
    fn dependency_failure_invalidates_snapshot_after_first_party_commit() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let main = repo.path().join("main.ts");
        fs::write(
            &main,
            "import value from 'selected-dep';\nexport const before = value;\n",
        )?;
        let dependency = repo.path().join("node_modules/selected-dep");
        fs::create_dir_all(&dependency)?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"selected-dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        fs::write(dependency.join("index.js"), "export default 1;\n")?;

        let conn = store::open(repo.path())?;
        let options = IndexOptions {
            dependencies: vec!["selected-dep".into()],
            ..Default::default()
        };
        index_repo_with_options(repo.path(), &conn, &options)?;
        let old_snapshot: String =
            conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                row.get(0)
            })?;

        let changed = "import value from 'selected-dep';\nexport const after = value + 1;\n";
        fs::write(&main, changed)?;
        fs::remove_dir_all(&dependency)?;
        let error = index_repo_with_options(repo.path(), &conn, &options)
            .err()
            .expect("missing selected dependency must fail the run");
        assert!(error.to_string().contains("not installed or resolvable"));

        let committed_hash: String =
            conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            committed_hash,
            blake3::hash(changed.as_bytes()).to_hex().to_string()
        );
        let public_snapshot_rows: i64 = conn.query_row(
            "SELECT count(*) FROM meta
             WHERE key IN ('snapshot', 'projection_version')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            public_snapshot_rows, 0,
            "stale snapshot remained public: {old_snapshot}"
        );
        Ok(())
    }
}
