use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{Connection, params};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::dependency::{self, DependencyLimits};
use crate::graph::{self, FileGraph};
use crate::package_exports::RESOLVE_CONDITIONS;
use crate::{file_role, io_policy, parse, store, walk};

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    pub dependencies: Vec<String>,
    pub dependency_limits: DependencyLimits,
}

pub struct IndexOutcome {
    pub indexed: usize,
    pub unchanged: usize,
    /// Previously indexed first-party files omitted from the resulting
    /// snapshot because they disappeared or had a non-retryable
    /// read/extraction failure.
    pub removed: usize,
    /// Inputs or repository boundaries omitted because they had a
    /// non-retryable traversal, manifest, read, or extraction failure. These
    /// are visible corpus exclusions, not phase failures.
    pub rejected: usize,
    pub rejections: Vec<IndexRejection>,
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
    /// True when the disposable snapshot tables were truncated wholesale,
    /// either for an explicit fixed-snapshot refresh or a forced extractor
    /// re-extraction, instead of replacing files one at a time.
    pub extraction_reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRejection {
    pub path: String,
    pub stage: &'static str,
    pub error: String,
}

fn retryable_read_failure(path: &str, error: std::io::Error) -> anyhow::Error {
    anyhow::Error::new(error).context(format!("retryable read failure for `{path}`"))
}

fn read_source(path: &Path) -> std::io::Result<String> {
    #[cfg(test)]
    if let Some(error) = TEST_READ_FAILURES.with(|failures| failures.borrow_mut().remove(path)) {
        return Err(error);
    }
    std::fs::read_to_string(path)
}

#[cfg(test)]
thread_local! {
    static TEST_READ_FAILURES: std::cell::RefCell<HashMap<PathBuf, std::io::Error>> =
        std::cell::RefCell::new(HashMap::new());
}

#[cfg(test)]
fn inject_read_failure(path: PathBuf, error: std::io::Error) {
    TEST_READ_FAILURES.with(|failures| {
        failures.borrow_mut().insert(path, error);
    });
}

impl IndexOutcome {
    fn record_rejection(
        &mut self,
        path: impl Into<String>,
        stage: &'static str,
        error: impl std::fmt::Display,
    ) {
        self.rejected += 1;
        self.rejections.push(IndexRejection {
            path: path.into(),
            stage,
            error: error.to_string(),
        });
    }
}

pub fn report_rejections(outcome: &IndexOutcome) {
    if outcome.rejections.is_empty() {
        return;
    }
    eprintln!("index inputs rejected ({}):", outcome.rejections.len());
    for rejection in &outcome.rejections {
        let error = rejection.error.replace('\n', "\n      ");
        eprintln!("  [{}] {}: {error}", rejection.stage, rejection.path);
    }
}

struct FileData {
    chunks: Vec<Chunk>,
    graph: FileGraph,
    lines: LineIndex,
}

struct PreparedDependencyFile {
    package_root: PathBuf,
    display: String,
    source_path: PathBuf,
    package_path: String,
    source: String,
    hash: String,
    role: &'static str,
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

/// Incrementally index a repository for differential implementation tests.
#[cfg(test)]
pub fn index_repo(root: &Path, conn: &Connection) -> Result<IndexOutcome> {
    index_repo_with_options(root, conn, &IndexOptions::default())
}

/// Test convenience wrapper for the production incremental refresh.
#[cfg(test)]
pub fn index_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    incremental_refresh_repo_with_options(root, conn, options)
}

/// Refresh the published snapshot by retaining unchanged first-party rows and
/// replacing changed or missing files. This is a watch latency optimization:
/// it still scans and hashes the complete current source tree, re-evaluates
/// dependency ownership and module resolution, and publishes the same snapshot
/// contract as a full refresh.
pub fn incremental_refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(root, conn, options, true, IndexMode::Incremental)
}

/// Rebuild every repository-derived row for the current checkout while
/// preserving content-addressed embeddings and semantic memory.
pub fn refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(root, conn, options, true, IndexMode::FullRefresh)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    Incremental,
    FullRefresh,
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
    index_repo_impl(root, conn, options, false, IndexMode::Incremental)
}

fn index_repo_impl(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    allow_extraction_reset: bool,
    mode: IndexMode,
) -> Result<IndexOutcome> {
    let root = root.canonicalize()?;
    let inventory = walk::source_inventory(&root)?;
    let workspace_discovery = crate::workspace::WorkspaceMap::discover(&root, &inventory.files)?;
    let workspace = workspace_discovery.map;
    let mut outcome = IndexOutcome {
        indexed: 0,
        unchanged: 0,
        removed: 0,
        rejected: 0,
        rejections: Vec::new(),
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
    for rejection in inventory.rejections {
        outcome.record_rejection(
            display_repository_path(&root, &rejection.path),
            rejection.stage,
            rejection.error,
        );
    }
    for rejection in workspace_discovery.rejections {
        outcome.record_rejection(
            display_repository_path(&root, &rejection.path),
            rejection.stage,
            rejection.error,
        );
    }

    // Source extraction and every selected-dependency read happen before the
    // publication boundary. A retryable corpus failure therefore rolls this
    // transaction back and leaves the previously published snapshot intact.
    conn.execute_batch("BEGIN")?;
    let preparation = (|| -> Result<(_, _, _)> {
        ensure_extraction_version(conn)?;
        let stored: HashMap<String, (i64, String, String)> = {
            let mut stmt =
                conn.prepare("SELECT path, id, hash, role FROM files WHERE origin!='dependency'")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ),
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let previous_paths = stored
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut existing = if mode == IndexMode::FullRefresh {
            HashMap::new()
        } else {
            stored
        };

        // Extractor-version changes force re-extraction by clearing file
        // hashes. At that scale, per-file replacement is pathological, so
        // truncate the disposable plane once and insert like a fresh index.
        let cleared = existing
            .values()
            .filter(|(_, hash, _)| hash.is_empty())
            .count();
        let extraction_reset = mode == IndexMode::FullRefresh
            || (allow_extraction_reset && !existing.is_empty() && cleared * 2 >= existing.len());
        if extraction_reset {
            if mode == IndexMode::FullRefresh {
                store::reset_snapshot_state(conn)?;
            } else {
                store::reset_extraction_state(conn)?;
            }
            existing.clear();
            outcome.extraction_reset = true;
        }

        let mut seen = std::collections::HashSet::new();
        let mut published = std::collections::HashSet::new();
        for file in &inventory.files {
            let rel = display_repository_path(&root, file);
            let source = match read_source(file) {
                Ok(source) => {
                    seen.insert(rel.clone());
                    source
                }
                Err(error) if io_policy::is_inventory_race(&error) => continue,
                Err(error) if io_policy::is_retryable(&error) => {
                    return Err(retryable_read_failure(&rel, error));
                }
                Err(error) => {
                    seen.insert(rel.clone());
                    if let Some((old_id, _, _)) = existing.get(&rel) {
                        store::delete_file(conn, *old_id)?;
                    }
                    outcome.record_rejection(rel, "read", error);
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
                published.insert(rel);
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
                    let (chunks, refs) = insert_file(conn, &identity, &data)?;
                    outcome.indexed += 1;
                    outcome.chunks += chunks;
                    outcome.refs += refs;
                    published.insert(rel);
                }
                Err(error) => {
                    if let Some((old_id, _, _)) = existing.get(&rel) {
                        store::delete_file(conn, *old_id)?;
                    }
                    outcome.record_rejection(rel, "extract", error);
                }
            }
        }
        for (path, (id, _, _)) in &existing {
            if !seen.contains(path) {
                store::delete_file(conn, *id)?;
            }
        }
        outcome.removed = previous_paths.difference(&published).count();

        let previous = if outcome.extraction_reset {
            ProjectionIdentity {
                snapshot: None,
                projection_version: None,
                resolution_hash: None,
            }
        } else {
            ProjectionIdentity::read(conn)?
        };

        // Dependency discovery sees the just-extracted, uncommitted importer
        // rows. Reading and parsing the selected corpus here closes the gap
        // where one transient dependency file previously invalidated the old
        // publication before failing.
        let discovered = dependency::discover(&root, conn, &options.dependencies, &workspace)?;
        let plans = dependency::plan_packages(&discovered, options.dependency_limits)?;
        let prepared = prepare_dependency_files(&plans, &mut outcome)?;

        conn.execute(
            "DELETE FROM meta WHERE key IN ('snapshot', 'projection_version', 'resolution_hash')",
            [],
        )?;
        Ok((previous, plans, prepared))
    })();
    let (previous, plans, prepared) = match preparation {
        Ok(preparation) => preparation,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    };
    if let Err(error) = conn.execute_batch("COMMIT") {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(error.into());
    }

    let instances = dependency::synchronize_instances(&root, conn, &workspace, &plans)?;
    index_dependency_files(conn, &prepared, &instances, &mut outcome)?;
    if outcome.indexed > 0 {
        crate::embed::materialize_cached_embeddings(conn)?;
    }

    resolve_module_edges(&root, conn, &workspace)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('root', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [root.to_string_lossy()],
    )?;
    let resolution = crate::structural::compute_resolution_hash(conn)?;
    let snapshot = crate::structural::compute_snapshot_with_resolution(conn, &resolution)?;
    // An exact-snapshot checker batch is still valid and expensive to
    // reproduce. Old-snapshot batches are removed before either refresh mode
    // can publish the new snapshot.
    store::retain_checker_batches_for_snapshot(conn, &snapshot)?;
    let current = ProjectionIdentity {
        snapshot: Some(snapshot.clone()),
        projection_version: Some(crate::structural::PROJECTION_VERSION.to_string()),
        resolution_hash: Some(resolution.clone()),
    };
    let projection_started = std::time::Instant::now();
    if previous == current {
        // The projection is a pure function of the canonical tables: the
        // snapshot covers every extracted row (file content identity) and the
        // resolution hash covers module edges, whose inputs (tsconfigs,
        // manifests, node_modules layout) live outside indexed content.
        // Checker publication rebuilds the projection immediately and its
        // batch is accepted only for this exact snapshot. Identical inputs
        // under the same projection version can therefore republish the
        // existing rows.
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
        crate::recon::reconcile_file_policy_after_index(&root, conn);
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
    crate::recon::reconcile_file_policy_after_index(&root, conn);
    Ok(outcome)
}

fn display_repository_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
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
    // The caller owns the refresh transaction. Keeping this invalidation
    // inside it ensures a later source/dependency acquisition failure restores
    // the previously published extractor version and snapshot together.
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
           receiver_start, receiver_end, property_start, property_end,
           receiver_unbound
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
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
            m.receiver_unbound,
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

fn prepare_dependency_files(
    plans: &[dependency::PackagePlan],
    outcome: &mut IndexOutcome,
) -> Result<Vec<PreparedDependencyFile>> {
    let mut prepared = Vec::new();
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
        for file in &plan.files {
            let display = dependency_display_path(&plan.package, &file.package_path);
            let source = match read_source(&file.source_path) {
                Ok(source) => source,
                Err(error) if io_policy::is_inventory_race(&error) => continue,
                Err(error) if io_policy::is_retryable(&error) => {
                    let path = format!("{display} ({})", file.source_path.display());
                    return Err(retryable_read_failure(&path, error));
                }
                Err(error) => {
                    outcome.record_rejection(
                        display,
                        "read",
                        format!("{}: {error}", file.source_path.display()),
                    );
                    continue;
                }
            };
            if dependency::should_skip_minified(&file.source_path, &source, file.forced_entry) {
                outcome.dependency_skipped += 1;
                continue;
            }
            outcome.dependency_bytes += file.bytes;
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let role = file_role::classify(Path::new(&file.package_path), &source);
            prepared.push(PreparedDependencyFile {
                package_root: plan.package.canonical_root.clone(),
                display,
                source_path: file.source_path.clone(),
                package_path: file.package_path.clone(),
                source,
                hash,
                role,
            });
        }
    }
    Ok(prepared)
}

fn index_dependency_files(
    conn: &Connection,
    prepared: &[PreparedDependencyFile],
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
        for file in prepared {
            let package_id = *instances.get(&file.package_root).ok_or_else(|| {
                anyhow::anyhow!("dependency package instance was not synchronized")
            })?;
            seen.insert(file.display.clone());
            if let Some((id, old_hash, old_role, old_package, old_package_path)) =
                existing.get(&file.display)
                && *old_hash == file.hash
                && *old_package == package_id
                && *old_package_path == file.package_path
            {
                if old_role != file.role {
                    conn.execute(
                        "UPDATE files SET role=?1 WHERE id=?2",
                        params![file.role, id],
                    )?;
                }
                outcome.unchanged += 1;
                outcome.dependency_files += 1;
                continue;
            }
            if let Some((old_id, _, _, _, _)) = existing.get(&file.display) {
                store::delete_file(conn, *old_id)?;
            }
            match extract_file(&file.source_path, &file.display, &file.source) {
                Ok(data) => {
                    let identity = FileIdentity {
                        path: &file.display,
                        hash: &file.hash,
                        role: file.role,
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
                Err(error) => outcome.record_rejection(file.display.clone(), "extract", error),
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
pub fn resolve_module_edges(
    root: &Path,
    conn: &Connection,
    workspace: &crate::workspace::WorkspaceMap,
) -> Result<()> {
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
    use std::io::ErrorKind;

    use anyhow::Result;

    use super::{
        IndexOptions, incremental_refresh_repo_with_options, index_repo, index_repo_with_options,
        index_repo_without_extraction_reset, inject_read_failure, refresh_repo_with_options,
    };
    use crate::{embed, origin, query, search, semantic, store, structural};

    #[test]
    fn reports_the_file_and_stage_for_rejected_reads() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("bad.ts"), [0xff, 0xfe])?;
        let conn = store::open(repo.path())?;

        let outcome = index_repo(repo.path(), &conn)?;

        assert_eq!(outcome.rejected, 1);
        assert_eq!(outcome.rejections.len(), 1);
        assert_eq!(outcome.rejections[0].path, "bad.ts");
        assert_eq!(outcome.rejections[0].stage, "read");
        assert!(!outcome.rejections[0].error.is_empty());
        assert!(!structural::current_snapshot(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    fn file_disappearance_after_inventory_is_a_removal_not_a_retry() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let retained = repo.path().join("retained.ts");
        let vanished = repo.path().join("vanished.ts");
        fs::write(&retained, "export const retained = 1;\n")?;
        fs::write(&vanished, "export const vanished = 1;\n")?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;

        inject_read_failure(
            vanished.canonicalize()?,
            std::io::Error::from(ErrorKind::NotFound),
        );
        let outcome = index_repo(repo.path(), &conn)?;

        assert_eq!(outcome.rejected, 0);
        assert_eq!(outcome.removed, 1);
        let paths = conn
            .prepare("SELECT path FROM files ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(paths, vec!["retained.ts"]);
        Ok(())
    }

    #[test]
    fn retryable_source_read_preserves_the_published_snapshot() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = repo.path().join("main.ts");
        let before = "export const before = 1;\n";
        fs::write(&source, before)?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let old_snapshot = structural::current_snapshot(&conn)?;

        fs::write(&source, "export const after = 2;\n")?;
        #[cfg(unix)]
        let transient_error = std::io::Error::from_raw_os_error(libc::EMFILE);
        #[cfg(not(unix))]
        let transient_error = std::io::Error::from(ErrorKind::Interrupted);
        inject_read_failure(source.canonicalize()?, transient_error);
        let error = index_repo(repo.path(), &conn)
            .err()
            .expect("retryable source read must abort preparation");

        assert!(error.to_string().contains("retryable read failure"));
        let retained_hash: String =
            conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            retained_hash,
            blake3::hash(before.as_bytes()).to_hex().to_string()
        );
        assert_eq!(structural::current_snapshot(&conn)?, old_snapshot);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn previously_indexed_unreadable_subtree_reports_removal_magnitude() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir()?;
        let locked = repo.path().join("locked");
        fs::create_dir_all(&locked)?;
        fs::write(repo.path().join("good.ts"), "export const good = 1;\n")?;
        fs::write(locked.join("first.ts"), "export const first = 1;\n")?;
        fs::write(locked.join("second.ts"), "export const second = 2;\n")?;
        let conn = store::open(repo.path())?;
        let initial = index_repo(repo.path(), &conn)?;
        assert_eq!(
            (initial.indexed, initial.removed, initial.rejected),
            (3, 0, 0)
        );

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;
        let result = index_repo(repo.path(), &conn);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700))?;
        let outcome = result?;

        assert_eq!(outcome.indexed, 0);
        assert_eq!(outcome.unchanged, 1);
        assert_eq!(outcome.removed, 2);
        assert_eq!(outcome.rejected, 1);
        assert_eq!(outcome.rejections[0].path, "locked");
        assert_eq!(outcome.rejections[0].stage, "walk");
        let paths = conn
            .prepare("SELECT path FROM files ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(paths, vec!["good.ts"]);
        assert!(!structural::current_snapshot(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    fn workspace_boundary_rejections_remain_visible_while_sources_index() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        )?;
        fs::create_dir_all(repo.path().join("packages/broken/src"))?;
        fs::write(
            repo.path().join("packages/broken/package.json"),
            "{ not-json",
        )?;
        fs::write(
            repo.path().join("packages/broken/src/index.ts"),
            "export const indexed = 1;\n",
        )?;
        let conn = store::open(repo.path())?;

        let outcome = index_repo(repo.path(), &conn)?;

        assert_eq!(outcome.indexed, 1);
        assert!(outcome.rejections.iter().any(|rejection| {
            rejection.path == "packages/broken/package.json"
                && rejection.stage == "workspace-manifest"
        }));
        Ok(())
    }

    #[test]
    fn full_refresh_preserves_source_less_workspace_identities() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join(".git"))?;
        fs::write(
            repo.path().join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        )?;
        fs::write(repo.path().join(".gitignore"), "packages/ignored/src/\n")?;
        fs::create_dir_all(repo.path().join("packages/dist-only/dist"))?;
        fs::write(
            repo.path().join("packages/dist-only/package.json"),
            r#"{"name":"dist-only","main":"dist/index.js"}"#,
        )?;
        fs::write(
            repo.path().join("packages/dist-only/dist/index.js"),
            "module.exports = 1;\n",
        )?;
        fs::create_dir_all(repo.path().join("packages/ignored/src"))?;
        fs::write(
            repo.path().join("packages/ignored/package.json"),
            r#"{"name":"ignored-source","main":"src/index.ts"}"#,
        )?;
        fs::write(
            repo.path().join("packages/ignored/src/index.ts"),
            "export const ignored = true;\n",
        )?;
        fs::write(repo.path().join("main.ts"), "export const main = true;\n")?;
        let conn = store::open(repo.path())?;

        refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

        let workspace_names = conn
            .prepare(
                "SELECT name FROM package_instances
                 WHERE origin='workspace' ORDER BY name",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(workspace_names, vec!["dist-only", "ignored-source"]);
        Ok(())
    }

    #[test]
    fn indexes_js_files_containing_jsx() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("page.js"),
            "export default function Page() { return <main>Hello</main>; }\n",
        )?;
        let conn = store::open(repo.path())?;

        let outcome = index_repo(repo.path(), &conn)?;

        assert_eq!((outcome.indexed, outcome.rejected), (1, 0));
        let chunks: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))?;
        assert!(chunks > 0);
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
    fn non_retryable_dependency_rejections_remove_stale_rows() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "import value from 'selected-dep';\nexport const result = value;\n",
        )?;
        let dependency = repo.path().join("node_modules/selected-dep");
        fs::create_dir_all(&dependency)?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"selected-dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        let entry = dependency.join("index.js");
        fs::write(
            &entry,
            "export const dependencyStaleMarker = true;\nexport default 1;\n",
        )?;

        let conn = store::open(repo.path())?;
        let options = IndexOptions {
            dependencies: vec!["selected-dep".into()],
            ..Default::default()
        };
        index_repo_with_options(repo.path(), &conn, &options)?;

        fs::write(&entry, [0xff, 0xfe])?;
        let unreadable = index_repo_with_options(repo.path(), &conn, &options)?;
        assert_eq!(unreadable.rejected, 1);
        assert_eq!(unreadable.rejections[0].stage, "read");
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM files WHERE origin='dependency'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'dependencyStaleMarker'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );

        fs::write(&entry, "export default 2;\n")?;
        index_repo_with_options(repo.path(), &conn, &options)?;
        fs::write(&entry, "export default function Broken() { return <main>")?;
        let unparseable = index_repo_with_options(repo.path(), &conn, &options)?;
        assert_eq!(unparseable.rejected, 1);
        assert_eq!(unparseable.rejections[0].stage, "extract");
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM files WHERE origin='dependency'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        assert!(!structural::current_snapshot(&conn)?.is_empty());
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
    /// of rowids, including the stable published snapshot identity.
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
            ("meta", "SELECT key, value FROM meta"),
        ];
        SECTIONS
            .iter()
            .map(|(name, sql)| Ok((*name, dump_section(conn, sql)?)))
            .collect()
    }

    #[test]
    fn full_refresh_rebuilds_snapshot_and_preserves_expensive_planes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "export function run() { return 'stable'; }\n",
        )?;
        let conn = store::open(repo.path())?;
        let first = index_repo(repo.path(), &conn)?;
        assert_eq!((first.indexed, first.unchanged), (1, 0));

        let snapshot = structural::current_snapshot(&conn)?;
        let (chunk_hash, source_hash): (String, String) = conn.query_row(
            "SELECT chunk.hash, file.hash
             FROM chunks chunk JOIN files file ON file.id=chunk.file_id
             WHERE file.path='main.ts' ORDER BY chunk.id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let anchor = "file:main.ts";
        let context_hash = semantic::context_hash(&conn, anchor)?;

        // Durable cache and memory rows deliberately coexist with disposable
        // rows in one database.
        conn.execute_batch(
            "INSERT INTO embedding_profiles(
               id, provider, model, config_fingerprint, dimensions, config_json
             ) VALUES(1, 'test', 'tiny', 'test-profile', 2, '{}');
             INSERT INTO checker_enrichment_batches(
               id, source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(1, 'old', '5.9.3', 'test', 'checker-fp', 1,
                      '2026-01-01T00:00:00Z', 1);
             INSERT INTO package_instances(
               id, origin, name, canonical_root, locator, manifest_hash
             ) VALUES(99, 'dependency', 'obsolete', '/obsolete/package',
                      'obsolete@1', 'manifest');
             INSERT INTO scout_runs(
               id, scout_kind, status, gateway_protocol, provider, model,
               billing_path, prompt_version, source_snapshot,
               input_fingerprint, request_hash, started_at, completed_at
             ) VALUES(7, 'workflow', 'completed', 1, 'test', 'test-model',
                      'api', 'v1', 'old', 'memory-fp', 'request',
                      '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z');
             INSERT INTO scout_classifications(
               run_id, anchor_key, decision, role, evidence_json
             ) VALUES(7, 'file:main.ts', 'defining', 'entry', '{}');
             INSERT INTO semantic_artifacts(
               id, artifact_type, canonical_name, body_json, model,
               prompt_version, confidence, source_snapshot, created_at,
               scout_run_id, input_fingerprint, artifact_fingerprint
             ) VALUES(3, 'annotation', 'stable behavior', '{}', 'test-model',
                      'v1', 'likely', 'old', '2026-01-01T00:01:00Z', 7,
                      'memory-fp', 'artifact-fp');",
        )?;
        conn.execute(
            "UPDATE checker_enrichment_batches SET source_snapshot=?1 WHERE id=1",
            [&snapshot],
        )?;
        conn.execute(
            "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES(?1, 1, ?2)",
            rusqlite::params![chunk_hash, vec![0_u8; 8]],
        )?;
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id, claim_path, anchor_key, role, evidence_file,
               evidence_start_line, evidence_end_line, source_hash,
               context_hash, confidence
             ) VALUES(3, '$', ?1, 'evidence', 'main.ts', 1, 1, ?2, ?3, 'likely')",
            rusqlite::params![anchor, source_hash, context_hash],
        )?;
        embed::materialize_cached_embeddings(&conn)?;
        assert_eq!(
            semantic::load_artifact(&conn, 3)?.unwrap().freshness,
            "fresh"
        );

        let refreshed = refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
        assert!(refreshed.extraction_reset);
        assert_eq!(
            (refreshed.indexed, refreshed.unchanged, refreshed.rejected),
            (1, 0, 0)
        );
        assert_eq!(structural::current_snapshot(&conn)?, snapshot);

        let counts: (i64, i64, i64, i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM embedding_profiles),
               (SELECT count(*) FROM embeddings),
               (SELECT count(*) FROM embedding_index_entries),
               (SELECT count(*) FROM semantic_artifacts),
               (SELECT count(*) FROM checker_enrichment_batches),
               (SELECT count(*) FROM package_instances WHERE id=99)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        assert_eq!(counts, (1, 1, 1, 1, 1, 0));
        assert_eq!(
            semantic::load_artifact(&conn, 3)?.unwrap().freshness,
            "fresh"
        );

        fs::write(
            repo.path().join("main.ts"),
            "export function run() { return 'changed'; }\n",
        )?;
        refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM checker_enrichment_batches",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            0,
            "a checker batch must not survive a different structural snapshot"
        );
        Ok(())
    }

    #[test]
    fn changed_incremental_refresh_retires_old_checker_batches() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1, '5.9.3', 'test', 'checker-fp', 1,
                      '2026-01-01T00:00:00Z', 1)",
            [&snapshot],
        )?;

        fs::write(repo.path().join("main.ts"), "export const value = 2;\n")?;
        incremental_refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM checker_enrichment_batches",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );
        Ok(())
    }

    #[test]
    fn incremental_read_failure_removes_the_stale_file_row() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = repo.path().join("main.ts");
        fs::write(&source, "export const value = 1;\n")?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;

        fs::write(&source, [0xff, 0xfe])?;
        let outcome =
            incremental_refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

        assert_eq!(outcome.rejected, 1);
        assert_eq!(outcome.removed, 1);
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM files WHERE path='main.ts'",
                [],
                |row| { row.get::<_, i64>(0) }
            )?,
            0
        );
        Ok(())
    }

    #[test]
    fn incremental_and_full_refresh_publish_the_same_structural_identity() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("main.ts"),
            "import { stable } from './stable';\n\
             import { edited } from './edited';\n\
             import { removed } from './removed';\n\
             import { renamed } from './old-name';\n\
             export const total = stable + edited + removed + renamed;\n",
        )?;
        fs::write(repo.path().join("stable.ts"), "export const stable = 1;\n")?;
        fs::write(repo.path().join("edited.ts"), "export const edited = 2;\n")?;
        fs::write(
            repo.path().join("removed.ts"),
            "export const removed = 3;\n",
        )?;
        fs::write(
            repo.path().join("old-name.ts"),
            "export const renamed = 4;\n",
        )?;

        let incremental = store::open_path(&repo.path().join("incremental.db"))?;
        let full = store::open_path(&repo.path().join("full.db"))?;
        refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
        refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;

        fs::write(repo.path().join("edited.ts"), "export const edited = 20;\n")?;
        fs::remove_file(repo.path().join("removed.ts"))?;
        fs::rename(
            repo.path().join("old-name.ts"),
            repo.path().join("renamed.ts"),
        )?;
        fs::write(repo.path().join("added.ts"), "export const added = 5;\n")?;
        fs::write(
            repo.path().join("main.ts"),
            "import { stable } from './stable';\n\
             import { edited } from './edited';\n\
             import { added } from './added';\n\
             import { renamed } from './renamed';\n\
             export const total = stable + edited + added + renamed;\n",
        )?;

        let incremental_outcome = incremental_refresh_repo_with_options(
            repo.path(),
            &incremental,
            &IndexOptions::default(),
        )?;
        let full_outcome = refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;
        assert_eq!(
            (
                incremental_outcome.indexed,
                incremental_outcome.unchanged,
                incremental_outcome.removed,
            ),
            (4, 1, 2)
        );
        assert_eq!((full_outcome.indexed, full_outcome.unchanged), (5, 0));

        let incremental_resolution = structural::compute_resolution_hash(&incremental)?;
        let full_resolution = structural::compute_resolution_hash(&full)?;
        let incremental_snapshot = structural::current_snapshot(&incremental)?;
        let full_snapshot = structural::current_snapshot(&full)?;
        assert_eq!(incremental_resolution, full_resolution);
        assert_eq!(incremental_snapshot, full_snapshot);
        assert_eq!(canonical_dump(&incremental)?, canonical_dump(&full)?);

        incremental.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1, '5.9.3', 'test', 'checker-fp', 1,
                      '2026-01-01T00:00:00Z', 1)",
            [&incremental_snapshot],
        )?;
        refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
        assert_eq!(structural::current_snapshot(&incremental)?, full_snapshot);
        assert_eq!(
            incremental.query_row(
                "SELECT count(*) FROM checker_enrichment_batches",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            1,
            "an unchanged full reconciliation must retain the exact checker batch"
        );
        Ok(())
    }

    #[test]
    fn identical_full_refresh_reprojects_the_exact_checker_batch() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("service.ts"),
            "export class Service { load() {} }\n\
             export function run(service: Service) { service.load(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let (
            member_call_id,
            source_file_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
        ) = conn.query_row(
            "SELECT call.rowid, file.id, file.hash, call.start, call.end,
                    call.receiver_start, call.receiver_end,
                    call.property_start, call.property_end
             FROM member_calls call JOIN files file ON file.id=call.file_id
             WHERE call.prop='load'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )?;
        let (target, target_hash, target_start, target_end): (String, String, i64, i64) = conn
            .query_row(
                "SELECT node.node_key, file.hash, symbol.decl_start, symbol.decl_end
                 FROM graph_nodes node
                 JOIN symbols symbol
                   ON node.native_table='symbols' AND node.native_id=symbol.id
                 JOIN files file ON file.id=symbol.file_id
                 WHERE node.display_name='load'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let target_fingerprint =
            crate::checker::target_fingerprint(&target, &target_hash, target_start, target_end);
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1,'5.9.3','test','inputs',1,datetime('now'),1)",
            [&snapshot],
        )?;
        let batch_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO checker_project_runs(
               batch_id, project_id, status, selected_occurrences,
               completed_occurrences, checker_input_fingerprint, updated_at
             ) VALUES(?1,'tsconfig.json','completed',1,1,'inputs',datetime('now'))",
            [batch_id],
        )?;
        conn.execute(
            "INSERT INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,'service.ts',?4,?5,?6,?7,?8,?9,?10,
               'tsconfig.json','Service',?11,?12,'likely','checker','inputs'
             )",
            rusqlite::params![
                batch_id,
                member_call_id,
                source_file_id,
                source_hash,
                call_start,
                call_end,
                receiver_start,
                receiver_end,
                property_start,
                property_end,
                target,
                target_fingerprint,
            ],
        )?;
        conn.execute(
            "INSERT INTO checker_occurrence_projects(
               batch_id, member_call_id, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,'tsconfig.json','inputs','resolved')",
            rusqlite::params![batch_id, member_call_id],
        )?;
        structural::rebuild_projection(&conn, &snapshot)?;

        refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
        let counts: (i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM checker_enrichment_batches),
               (SELECT count(*) FROM resolved_edges
                  WHERE provenance='checker' AND dst_key=?1)",
            [&target],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(counts, (1, 1));
        Ok(())
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
            (fast.indexed, fast.unchanged, fast.rejected),
            (slow.indexed, slow.unchanged, slow.rejected)
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
    fn dependency_failure_rolls_back_before_snapshot_invalidation() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let main = repo.path().join("main.ts");
        let before = "import value from 'selected-dep';\nexport const before = value;\n";
        fs::write(&main, before)?;
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

        let retained_hash: String =
            conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            retained_hash,
            blake3::hash(before.as_bytes()).to_hex().to_string(),
            "failed dependency preparation committed first-party changes"
        );
        let retained_snapshot: String =
            conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            retained_snapshot, old_snapshot,
            "failed dependency preparation removed the published snapshot"
        );
        Ok(())
    }

    #[test]
    fn retryable_dependency_read_preserves_the_published_snapshot() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let main = repo.path().join("main.ts");
        let before = "import value from 'selected-dep';\nexport const before = value;\n";
        fs::write(&main, before)?;
        let dependency = repo.path().join("node_modules/selected-dep");
        fs::create_dir_all(&dependency)?;
        fs::write(
            dependency.join("package.json"),
            r#"{"name":"selected-dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        let entry = dependency.join("index.js");
        fs::write(&entry, "export default 1;\n")?;

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

        fs::write(
            &main,
            "import value from 'selected-dep';\nexport const after = value + 1;\n",
        )?;
        inject_read_failure(
            entry.canonicalize()?,
            std::io::Error::from(ErrorKind::Interrupted),
        );
        let error = index_repo_with_options(repo.path(), &conn, &options)
            .err()
            .expect("retryable dependency read must fail preparation");
        assert!(error.to_string().contains("retryable read failure"));

        let retained_hash: String =
            conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            retained_hash,
            blake3::hash(before.as_bytes()).to_hex().to_string()
        );
        let retained_snapshot: String =
            conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(retained_snapshot, old_snapshot);
        Ok(())
    }
}
