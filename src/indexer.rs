use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{Connection, params};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::dependency::{self, DependencyLimits};
use crate::fs_ops::{FileSystem, OsFileSystem};
use crate::graph::{self, FileGraph};
use crate::package_exports::RESOLVE_CONDITIONS;
use crate::{file_role, io_policy, parse, store, walk};

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    pub dependencies: Vec<String>,
    pub dependency_limits: DependencyLimits,
    pub timing: bool,
    pub debug: bool,
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
    index_repo_impl(
        root,
        conn,
        options,
        true,
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::new(&OsFileSystem),
    )
}

#[cfg(test)]
pub(crate) fn index_repo_with_fs(
    root: &Path,
    conn: &Connection,
    fs: &impl FileSystem,
) -> Result<IndexOutcome> {
    index_repo_with_options_and_fs(root, conn, &IndexOptions::default(), fs)
}

#[cfg(test)]
pub(crate) fn index_repo_with_options_and_fs(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    fs: &impl FileSystem,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        true,
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::new(fs),
    )
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
    index_repo_impl(
        root,
        conn,
        options,
        true,
        IndexMode::Incremental,
        CheckerRetention::PreserveActiveForWatch,
        IndexOperation::new(&OsFileSystem),
    )
}

/// Rebuild every repository-derived row for the current checkout while
/// preserving content-addressed embeddings and semantic memory.
pub fn refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        true,
        IndexMode::FullRefresh,
        CheckerRetention::Drop,
        IndexOperation::new(&OsFileSystem),
    )
}

/// Full structural refresh for the watcher. Unlike manual `jscout index`, it
/// keeps only the previous active checker batch as a hidden carry source for
/// the following enrichment phase.
pub fn watch_full_refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        true,
        IndexMode::FullRefresh,
        CheckerRetention::PreserveActiveForWatch,
        IndexOperation::new(&OsFileSystem),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    Incremental,
    FullRefresh,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckerRetention {
    Drop,
    PreserveActiveForWatch,
}

/// Environment capabilities shared by every filesystem-sensitive phase of a
/// single indexing operation. User policy remains plain data in
/// `IndexOptions`; this private context carries the replaceable runtime seam.
struct IndexOperation<'a, F: FileSystem> {
    fs: &'a F,
}

impl<'a, F: FileSystem> IndexOperation<'a, F> {
    const fn new(fs: &'a F) -> Self {
        Self { fs }
    }
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
    index_repo_impl(
        root,
        conn,
        options,
        false,
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::new(&OsFileSystem),
    )
}

fn index_repo_impl<F: FileSystem>(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    allow_extraction_reset: bool,
    mode: IndexMode,
    checker_retention: CheckerRetention,
    operation: IndexOperation<'_, F>,
) -> Result<IndexOutcome> {
    let root = root.canonicalize()?;
    let inventory = walk::source_inventory(&root)?;
    let workspace_discovery =
        crate::workspace::WorkspaceMap::discover_with_fs(&root, &inventory.files, operation.fs)?;
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
            let source = match operation.fs.read_to_string(file) {
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
            if options.debug {
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
        let discovered =
            dependency::discover(&root, conn, &options.dependencies, &workspace, operation.fs)?;
        let plans =
            dependency::plan_packages(&discovered, options.dependency_limits, operation.fs)?;
        let prepared = prepare_dependency_files(&plans, &mut outcome, operation.fs)?;

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
    // Manual indexing always resets the optional checker plane. Watch keeps
    // one old active batch hidden for the immediately following per-project
    // carry step; projection still rejects a mismatched source snapshot.
    let checker_batches_changed = match checker_retention {
        CheckerRetention::Drop => store::clear_checker_batches(conn)?,
        CheckerRetention::PreserveActiveForWatch => {
            store::preserve_active_checker_batch_for_watch(conn)?
        }
    };
    let current = ProjectionIdentity {
        snapshot: Some(snapshot.clone()),
        projection_version: Some(crate::structural::PROJECTION_VERSION.to_string()),
        resolution_hash: Some(resolution.clone()),
    };
    let projection_started = std::time::Instant::now();
    if previous == current && !checker_batches_changed {
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
        if options.timing {
            eprintln!("timing structural-projection=skipped (unchanged)");
        }
        crate::recon::reconcile_file_policy_after_index(&root, conn);
        return Ok(outcome);
    }
    crate::structural::rebuild_projection_with_timing(conn, &snapshot, options.timing)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('resolution_hash', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&resolution],
    )?;
    if options.timing {
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
    fs: &impl FileSystem,
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
            let source = match fs.read_to_string(&file.source_path) {
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
mod tests;
