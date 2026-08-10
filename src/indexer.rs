use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{params, Connection};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::dependency::{self, DependencyLimits};
use crate::graph::{self, FileGraph};
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
    pub chunks: usize,
    pub refs: usize,
    pub dependency_packages: usize,
    pub dependency_files: usize,
    pub dependency_bytes: u64,
    pub dependency_skipped: usize,
    pub dependency_skipped_bytes: u64,
    pub dependency_plans: Vec<String>,
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

/// Export conditions the resolver enables. The workspace layer mirrors these
/// when it translates package.json `exports` targets, so alias-mapped entries
/// agree with what the resolver itself would pick.
pub(crate) const RESOLVE_CONDITIONS: &[&str] = &["import", "require", "node", "default"];

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
            (".js".into(), vec![".ts".into(), ".tsx".into(), ".js".into(), ".jsx".into()]),
            (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
            (".cjs".into(), vec![".cts".into(), ".cjs".into()]),
        ],
        condition_names: RESOLVE_CONDITIONS.iter().map(|c| (*c).to_string()).collect(),
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
    let root = root.canonicalize()?;
    let files = walk::source_files(&root);
    let mut outcome = IndexOutcome {
        indexed: 0,
        unchanged: 0,
        failed: 0,
        chunks: 0,
        refs: 0,
        dependency_packages: 0,
        dependency_files: 0,
        dependency_bytes: 0,
        dependency_skipped: 0,
        dependency_skipped_bytes: 0,
        dependency_plans: Vec::new(),
    };

    let existing: HashMap<String, (i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, id, hash, role FROM files WHERE origin!='dependency'",
        )?;
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

    let mut seen: std::collections::HashSet<String> = Default::default();
    conn.execute_batch("BEGIN")?;
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().into_owned();
        seen.insert(rel.clone());
        let Ok(source) = std::fs::read_to_string(file) else {
            outcome.failed += 1;
            continue;
        };
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let role = file_role::classify(Path::new(&rel), &source);
        if let Some((id, old_hash, old_role)) = existing.get(&rel)
            && *old_hash == hash {
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
                eprintln!("skip {rel}: {e}");
                outcome.failed += 1;
            }
        }
    }
    // Remove files that disappeared from disk.
    for (path, (id, _, _)) in &existing {
        if !seen.contains(path) {
            store::delete_file(conn, *id)?;
        }
    }

    // Commit canonical rows and snapshot invalidation atomically. Every
    // following dependency/resolution step can fail, so the previous graph
    // must stop being public before control enters that phase.
    conn.execute(
        "DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')",
        [],
    )?;
    conn.execute_batch("COMMIT")?;

    let workspace = crate::workspace::WorkspaceMap::build(&root);
    let discovered = dependency::discover(
        &root,
        conn,
        &options.dependencies,
        &workspace,
    )?;
    let plans = dependency::plan_packages(&discovered, options.dependency_limits)?;
    let instances = dependency::synchronize_instances(&root, conn, &workspace, &plans)?;
    index_dependency_files(conn, &plans, &instances, &mut outcome)?;

    resolve_module_edges(&root, conn)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('root', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [root.to_string_lossy()],
    )?;
    let snapshot = crate::structural::compute_snapshot(conn)?;
    let projection_started = std::time::Instant::now();
    crate::structural::rebuild_projection(conn, &snapshot)?;
    if std::env::var_os("JSCOUT_TIMING").is_some() {
        eprintln!("timing structural-projection={:?}", projection_started.elapsed());
    }
    Ok(outcome)
}

fn extract_file(abs: &Path, rel: &str, source: &str) -> Result<FileData> {
    parse::with_parsed(source, abs, |ret, semantic| {
        let chunker = Chunker::new(Path::new(rel), source, ret);
        let chunks = chunker.chunk_program(&ret.program, &ret.program.comments);
        let graph = graph::extract(ret, semantic);
        FileData { chunks, graph, lines: LineIndex::new(source) }
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
            let kind = serde_json::to_value(c.kind)?.as_str().unwrap_or("module").to_string();
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
        ins_exp.execute(params![file_id, e.export_name, e.local_name, e.from_request, e.from_name])?;
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
        "INSERT INTO member_calls(file_id, chunk_id, start, line, prop, object)
         VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for m in &data.graph.member_calls {
        ins_mc.execute(params![
            file_id,
            chunk_for(m.span_start),
            m.span_start,
            data.lines.line(m.span_start),
            m.prop,
            m.object,
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
            let package_id = *instances
                .get(&plan.package.canonical_root)
                .ok_or_else(|| anyhow::anyhow!("dependency package instance was not synchronized"))?;
            for file in &plan.files {
                let display = dependency_display_path(&plan.package, &file.package_path);
                let source = match std::fs::read_to_string(&file.source_path) {
                    Ok(source) => source,
                    Err(_) => {
                        seen.insert(display);
                        outcome.failed += 1;
                        continue;
                    }
                };
                if dependency::should_skip_minified(&file.source_path, &source, file.forced_entry) {
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
                        eprintln!("skip {display}: {error}");
                        outcome.failed += 1;
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
            let physical = if origin == "dependency" {
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
            Ok((PathBuf::from(row.get::<_, String>(0)?), row.get::<_, i64>(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    package_roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));

    let pairs: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.id, r.request FROM files f
             JOIN (SELECT file_id, request FROM imports
                   UNION SELECT file_id, from_request FROM exports WHERE from_request IS NOT NULL
                   UNION SELECT file_id, target_request FROM refs WHERE target_request IS NOT NULL) r
             ON r.file_id = f.id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM module_edges", [])?;
    let mut ins = conn.prepare_cached(
        "INSERT INTO module_edges(
           from_file, request, to_file, package, resolution, package_instance_id
         ) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    type Resolved = (Option<i64>, Option<String>, Option<&'static str>, Option<i64>);
    let mut cache: HashMap<(PathBuf, String), Resolved> = HashMap::new();
    for (file_id, request) in pairs {
        let (importer, dependency_importer) = importer_paths
            .get(&file_id)
            .ok_or_else(|| anyhow::anyhow!("indexed importer {file_id} has no physical path"))?
            .clone();
        let key = (importer.clone(), request.clone());
        let (to_file, package, resolution, package_instance) = cache
            .entry(key)
            .or_insert_with(|| match if dependency_importer {
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
                        None => {
                            let package_instance = package_roots
                                .iter()
                                .find(|(root, _)| path.starts_with(root))
                                .map(|(_, id)| *id);
                            (None, Some(package_name(&request)), None, package_instance)
                        }
                    }
                }
                Err(_) => (None, Some(package_name(&request)), None, None),
            })
            .clone();
        ins.execute(params![
            file_id,
            request,
            to_file,
            package,
            resolution,
            package_instance,
        ])?;
    }
    drop(ins);
    conn.execute_batch("COMMIT")?;
    Ok(())
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

    use super::{IndexOptions, index_repo, index_repo_with_options};
    use crate::{origin, query, search, store, structural};

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
        fs::write(app.join("src/helper.ts"), "export const helper = () => 1;\n")?;

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
        fs::write(repo.path().join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")?;

        // Library package: main points at untracked dist output, but the
        // module field names the source entry directly (manifest truth).
        let lib = repo.path().join("packages/lib");
        fs::create_dir_all(lib.join("src/utils"))?;
        fs::write(
            lib.join("package.json"),
            r#"{"name": "@acme/lib", "main": "dist/index.js", "module": "src/index.ts"}"#,
        )?;
        fs::write(lib.join("src/index.ts"), "export const greet = () => 'hi';\n")?;
        fs::write(lib.join("src/utils/format.ts"), "export const fmt = (s: string) => s;\n")?;

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
        fs::write(tools.join("src/inner/scrub.ts"), "export const scrub = (s: string) => s;\n")?;

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
            (Some("packages/lib/src/index.ts".into()), None, Some("workspace".into()))
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
        assert_eq!(fmt_call, ("likely".into(), "semantic+resolver-inferred".into()));
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
        fs::write(repo.path().join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")?;

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
        fs::write(app.join("src/helper.ts"), "export const helper = () => 1;\n")?;
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
        assert_eq!(edge("./helper")?, (Some("packages/app/src/helper.ts".into()), None));
        assert_eq!(edge("@acme/lib")?, (Some("packages/lib/src/index.ts".into()), None));
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
            &IndexOptions { dependencies: selected.clone(), ..Default::default() },
        )?;
        assert_eq!(first.dependency_packages, 1);
        assert_eq!(first.dependency_files, 2);
        assert!(first.dependency_bytes > 0);

        let package: (String, String, String) = conn.query_row(
            "SELECT origin, name, version FROM package_instances WHERE name='selected-dep'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(package, ("dependency".into(), "selected-dep".into(), "1.2.3".into()));
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
            query::find_symbols_in_origins(
                &conn,
                "dependencyOnlyMarker",
                &origin::defaults(),
            )?
            .is_empty()
        );
        let dependency_definitions = query::find_symbols_in_origins(
            &conn,
            "dependencyOnlyMarker",
            &["dependency".into()],
        )?;
        assert_eq!(dependency_definitions.len(), 1);
        assert_eq!(dependency_definitions[0].file_origin, "dependency");
        let first_party_anchor = structural::resolve_current_anchor_in_origins(
            &conn,
            "internal",
            &origin::defaults(),
        )?;
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
        assert!(default_boundary.nodes.iter().any(|node| node.key == package_hub.0));
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
            &IndexOptions { dependencies: selected, ..Default::default() },
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
            conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| row.get(0))?;

        let changed =
            "import value from 'selected-dep';\nexport const after = value + 1;\n";
        fs::write(&main, changed)?;
        fs::remove_dir_all(&dependency)?;
        let error = index_repo_with_options(repo.path(), &conn, &options)
            .err()
            .expect("missing selected dependency must fail the run");
        assert!(error.to_string().contains("not installed or resolvable"));

        let committed_hash: String = conn.query_row(
            "SELECT hash FROM files WHERE path='main.ts'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(committed_hash, blake3::hash(changed.as_bytes()).to_hex().to_string());
        let public_snapshot_rows: i64 = conn.query_row(
            "SELECT count(*) FROM meta
             WHERE key IN ('snapshot', 'projection_version')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(public_snapshot_rows, 0, "stale snapshot remained public: {old_snapshot}");
        Ok(())
    }
}
