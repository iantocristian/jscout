use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{params, Connection};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::graph::{self, FileGraph};
use crate::{file_role, parse, store, walk};

pub struct IndexOutcome {
    pub indexed: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub chunks: usize,
    pub refs: usize,
}

struct FileData {
    chunks: Vec<Chunk>,
    graph: FileGraph,
    lines: LineIndex,
}

pub fn make_resolver() -> Resolver {
    Resolver::new(ResolveOptions {
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
        condition_names: vec!["import".into(), "require".into(), "node".into(), "default".into()],
        main_fields: vec!["module".into(), "main".into()],
        tsconfig: Some(TsconfigDiscovery::Auto),
        ..ResolveOptions::default()
    })
}

/// Index (or re-index) a repository. Files whose content hash is unchanged
/// are skipped; changed files are fully replaced.
pub fn index_repo(root: &Path, conn: &Connection) -> Result<IndexOutcome> {
    let root = root.canonicalize()?;
    let files = walk::source_files(&root);
    let mut outcome = IndexOutcome {
        indexed: 0,
        unchanged: 0,
        failed: 0,
        chunks: 0,
        refs: 0,
    };

    let existing: HashMap<String, (i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT path, id, hash, role FROM files")?;
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
                let (nchunks, nrefs) = insert_file(conn, &rel, &hash, role, &data)?;
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
    conn.execute_batch("COMMIT")?;

    // Canonical rows are now newer than the traversal projection. Remove the
    // public snapshot before resolution so readers fail closed instead of
    // receiving an old graph under a current-looking anchor contract.
    conn.execute(
        "DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')",
        [],
    )?;
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
    rel: &str,
    hash: &str,
    role: &str,
    data: &FileData,
) -> Result<(usize, usize)> {
    conn.execute(
        "INSERT INTO files(path, hash, role) VALUES(?1, ?2, ?3)",
        params![rel, hash, role],
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
                rel,
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

/// Resolve every (file, request) pair to an in-repo file or external package.
pub fn resolve_module_edges(root: &Path, conn: &Connection) -> Result<()> {
    let resolver = make_resolver();
    let file_ids: HashMap<PathBuf, i64> = {
        let mut stmt = conn.prepare("SELECT path, id FROM files")?;
        let rows = stmt.query_map([], |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get::<_, i64>(1)?)))?;
        rows.filter_map(|r| r.ok()).map(|(p, id)| (root.join(p), id)).collect()
    };

    let pairs: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.id, f.path, r.request FROM files f
             JOIN (SELECT file_id, request FROM imports
                   UNION SELECT file_id, from_request FROM exports WHERE from_request IS NOT NULL
                   UNION SELECT file_id, target_request FROM refs WHERE target_request IS NOT NULL) r
             ON r.file_id = f.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM module_edges", [])?;
    let mut ins = conn.prepare_cached(
        "INSERT INTO module_edges(from_file, request, to_file, package) VALUES(?1,?2,?3,?4)",
    )?;
    let mut cache: HashMap<(PathBuf, String), (Option<i64>, Option<String>)> = HashMap::new();
    for (file_id, rel_path, request) in pairs {
        let importer = root.join(&rel_path);
        let key = (importer.clone(), request.clone());
        let (to_file, package) = cache
            .entry(key)
            .or_insert_with(|| match resolver.resolve_file(&importer, &request) {
                Ok(resolution) => {
                    let p = resolution.path().to_path_buf();
                    match file_ids.get(&p) {
                        Some(id) => (Some(*id), None),
                        None => (None, Some(package_name(&request))),
                    }
                }
                Err(_) => (None, Some(package_name(&request))),
            })
            .clone();
        ins.execute(params![file_id, request, to_file, package])?;
    }
    drop(ins);
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// "@scope/pkg/sub/path" -> "@scope/pkg"; "./x" stays as-is (unresolved relative).
fn package_name(request: &str) -> String {
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

    use super::index_repo;
    use crate::store;

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
}
