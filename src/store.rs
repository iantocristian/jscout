use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

pub const DB_FILE: &str = ".jscout.db";

pub fn db_path(root: &Path) -> std::path::PathBuf {
    root.join(DB_FILE)
}

pub fn open(root: &Path) -> Result<Connection> {
    open_path(&db_path(root))
}

/// Open an index database independently of the repository root. Evaluation
/// uses this to give warm and cold sessions isolated semantic-memory state
/// while both read the same frozen source tree.
pub fn open_path(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    init_schema(&conn)?;
    Ok(conn)
}

pub(crate) fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE IF NOT EXISTS package_instances(
  id INTEGER PRIMARY KEY,
  origin TEXT NOT NULL CHECK(origin IN ('workspace', 'dependency')),
  name TEXT NOT NULL,
  version TEXT,
  canonical_root TEXT UNIQUE NOT NULL,
  locator TEXT NOT NULL,
  manifest_hash TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'complete'
    CHECK(status IN ('complete', 'truncated', 'failed'))
);
CREATE INDEX IF NOT EXISTS idx_package_instances_name
  ON package_instances(name, origin);

CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  hash TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'unknown',
  origin TEXT NOT NULL DEFAULT 'repository'
    CHECK(origin IN ('repository', 'workspace', 'dependency')),
  package_instance_id INTEGER REFERENCES package_instances(id) ON DELETE CASCADE,
  package_path TEXT
);

CREATE TABLE IF NOT EXISTS chunks(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT,
  scope_chain TEXT NOT NULL DEFAULT '',
  symbols TEXT NOT NULL DEFAULT '',
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  hash TEXT NOT NULL,
  content TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(hash);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  content, name, symbols, path,
  tokenize="unicode61 tokenchars '_$'"
);

CREATE TABLE IF NOT EXISTS symbols(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  decl_start INTEGER NOT NULL,
  decl_end INTEGER NOT NULL,
  scope_chain TEXT NOT NULL DEFAULT '',
  line INTEGER NOT NULL,
  exported INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);

CREATE TABLE IF NOT EXISTS exports(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  export_name TEXT NOT NULL,      -- '*' for star re-exports
  local_name TEXT,                -- set for local exports
  from_request TEXT,              -- set for re-exports
  from_name TEXT                  -- imported name for re-exports; '*' for star
);
CREATE INDEX IF NOT EXISTS idx_exports_file ON exports(file_id);

CREATE TABLE IF NOT EXISTS imports(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  local_name TEXT NOT NULL,
  imported_name TEXT NOT NULL,    -- 'default' | '*' | name
  request TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file_id);

CREATE TABLE IF NOT EXISTS module_edges(
  from_file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  request TEXT NOT NULL,
  to_file INTEGER,                -- resolved in-repo file
  package TEXT,                   -- external package name
  resolution TEXT,                -- resolver | workspace | workspace-inferred
  package_instance_id INTEGER REFERENCES package_instances(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_module_edges_from ON module_edges(from_file);
CREATE INDEX IF NOT EXISTS idx_module_edges_to ON module_edges(to_file);

CREATE TABLE IF NOT EXISTS refs(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  start INTEGER NOT NULL,
  line INTEGER NOT NULL,
  kind TEXT NOT NULL,             -- call | render | extend | use
  confidence TEXT NOT NULL,       -- certain | likely | possible
  target_request TEXT,            -- module request when target is imported
  target_name TEXT NOT NULL,
  local INTEGER NOT NULL DEFAULT 0,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_refs_file ON refs(file_id);
CREATE INDEX IF NOT EXISTS idx_refs_target ON refs(target_name);

CREATE TABLE IF NOT EXISTS events(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  line INTEGER NOT NULL,
  role TEXT NOT NULL,             -- emit | listen
  name TEXT NOT NULL,
  method TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_name ON events(name);

CREATE TABLE IF NOT EXISTS member_calls(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  start INTEGER NOT NULL,
  line INTEGER NOT NULL,
  prop TEXT NOT NULL,
  object TEXT
);
CREATE INDEX IF NOT EXISTS idx_member_calls_prop ON member_calls(prop);

-- Source-local deterministic evidence. Identifier identities remain raw here
-- until module resolution can group them under canonical entities.
CREATE TABLE IF NOT EXISTS entity_sites(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER REFERENCES chunks(id) ON DELETE SET NULL,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  plane TEXT NOT NULL CHECK(plane IN ('runtime', 'general')),
  entity_type TEXT NOT NULL,
  role TEXT NOT NULL,
  identity_kind TEXT NOT NULL CHECK(identity_kind IN ('literal', 'reference')),
  identity_name TEXT NOT NULL,
  identity_start INTEGER NOT NULL,
  target_name TEXT,
  target_start INTEGER,
  extractor TEXT NOT NULL,
  provenance TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('certain', 'likely', 'possible')),
  detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entity_sites_file ON entity_sites(file_id);
CREATE INDEX IF NOT EXISTS idx_entity_sites_identity
  ON entity_sites(entity_type, identity_name);

CREATE TABLE IF NOT EXISTS entities(
  id INTEGER PRIMARY KEY,
  entity_key TEXT UNIQUE NOT NULL,
  plane TEXT NOT NULL CHECK(plane IN ('runtime', 'general')),
  entity_type TEXT NOT NULL,
  name TEXT NOT NULL,
  identity_anchor TEXT,
  meta_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entities_type_name ON entities(entity_type, name);

CREATE TABLE IF NOT EXISTS entity_occurrences(
  id INTEGER PRIMARY KEY,
  entity_id INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  site_id INTEGER UNIQUE NOT NULL REFERENCES entity_sites(id) ON DELETE CASCADE,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER REFERENCES chunks(id) ON DELETE SET NULL,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  role TEXT NOT NULL,
  extractor TEXT NOT NULL,
  provenance TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('certain', 'likely', 'possible')),
  detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entity_occurrences_entity
  ON entity_occurrences(entity_id, role);
CREATE INDEX IF NOT EXISTS idx_entity_occurrences_file
  ON entity_occurrences(file_id, start);

CREATE TABLE IF NOT EXISTS entity_edges(
  id INTEGER PRIMARY KEY,
  occurrence_id INTEGER NOT NULL REFERENCES entity_occurrences(id) ON DELETE CASCADE,
  target_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('certain', 'likely', 'possible')),
  provenance TEXT NOT NULL,
  detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entity_edges_occurrence
  ON entity_edges(occurrence_id, kind);
CREATE INDEX IF NOT EXISTS idx_entity_edges_target
  ON entity_edges(target_key, kind);

CREATE TABLE IF NOT EXISTS embeddings(
  chunk_hash TEXT NOT NULL,
  model TEXT NOT NULL,
  dim INTEGER NOT NULL,
  vec BLOB NOT NULL,
  PRIMARY KEY (chunk_hash, model)
);

CREATE TABLE IF NOT EXISTS graph_nodes(
  node_key TEXT PRIMARY KEY,
  node_kind TEXT NOT NULL,
  native_table TEXT,
  native_id INTEGER,
  display_name TEXT NOT NULL,
  file_id INTEGER,
  line INTEGER,
  meta_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_display ON graph_nodes(display_name);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_file ON graph_nodes(file_id);

CREATE TABLE IF NOT EXISTS resolved_edges(
  id INTEGER PRIMARY KEY,
  src_key TEXT NOT NULL,
  dst_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL,
  provenance TEXT NOT NULL,
  source_file_id INTEGER,
  source_ref_id INTEGER,
  line INTEGER,
  detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_resolved_edges_src
  ON resolved_edges(src_key, confidence, kind);
CREATE INDEX IF NOT EXISTS idx_resolved_edges_dst
  ON resolved_edges(dst_key, confidence, kind);

CREATE TABLE IF NOT EXISTS semantic_artifacts(
  id INTEGER PRIMARY KEY,
  supersedes_artifact_id INTEGER REFERENCES semantic_artifacts(id),
  artifact_type TEXT NOT NULL,
  canonical_name TEXT,
  body_json TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  source_snapshot TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_semantic_artifacts_type_name
  ON semantic_artifacts(artifact_type, canonical_name);
CREATE INDEX IF NOT EXISTS idx_semantic_artifacts_supersedes
  ON semantic_artifacts(supersedes_artifact_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_artifacts_one_successor
  ON semantic_artifacts(supersedes_artifact_id)
  WHERE supersedes_artifact_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS semantic_supports(
  artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id) ON DELETE CASCADE,
  claim_path TEXT NOT NULL,
  anchor_key TEXT NOT NULL,
  role TEXT,
  evidence_file TEXT NOT NULL,
  evidence_start_line INTEGER NOT NULL CHECK(evidence_start_line > 0),
  evidence_end_line INTEGER NOT NULL CHECK(evidence_end_line >= evidence_start_line),
  source_hash TEXT NOT NULL,
  context_hash TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible'))
);
CREATE INDEX IF NOT EXISTS idx_semantic_supports_artifact
  ON semantic_supports(artifact_id);
CREATE INDEX IF NOT EXISTS idx_semantic_supports_anchor
  ON semantic_supports(anchor_key);
"#,
    )?;
    migrate(conn)?;
    // These indexes refer to columns introduced by migrations, so create them
    // only after legacy tables have been upgraded.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_files_origin ON files(origin);
         CREATE INDEX IF NOT EXISTS idx_files_package_instance ON files(package_instance_id);
         CREATE INDEX IF NOT EXISTS idx_module_edges_package_instance
           ON module_edges(package_instance_id);",
    )?;
    Ok(())
}

/// Pin a multi-statement read to one SQLite snapshot. The savepoint also nests
/// safely when search expansion calls neighborhood traversal.
pub(crate) fn with_read_snapshot<T>(
    conn: &Connection,
    savepoint: &'static str,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    conn.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
    match read() {
        Ok(value) => {
            conn.execute_batch(&format!("RELEASE {savepoint}"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch(&format!(
                "ROLLBACK TO {savepoint}; RELEASE {savepoint}"
            ));
            Err(error)
        }
    }
}

/// Migrations only preserve canonical/source rows where that is safe. Graph
/// projections are disposable and are rebuilt by the next index operation.
fn migrate(conn: &Connection) -> Result<()> {
    let version: u32 = conn
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .ok()
        .and_then(|v: String| v.parse().ok())
        .unwrap_or(0);

    // v1 -> v2: embeddings PK was chunk_hash alone, so different models
    // overwrote each other's vectors. The table is a cache — drop and rebuild.
    if version < 2 {
        let pk_cols: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('embeddings') WHERE pk > 0",
            [],
            |r| r.get(0),
        )?;
        if pk_cols < 2 {
            conn.execute_batch(
                "DROP TABLE IF EXISTS embeddings;
                 CREATE TABLE embeddings(
                   chunk_hash TEXT NOT NULL,
                   model TEXT NOT NULL,
                   dim INTEGER NOT NULL,
                   vec BLOB NOT NULL,
                   PRIMARY KEY (chunk_hash, model)
                 );",
            )?;
        }
    }

    if version < 3 {
        if !has_column(conn, "symbols", "decl_start")? {
            conn.execute("ALTER TABLE symbols ADD COLUMN decl_start INTEGER", [])?;
        }
        if !has_column(conn, "symbols", "decl_end")? {
            conn.execute("ALTER TABLE symbols ADD COLUMN decl_end INTEGER", [])?;
        }
        if !has_column(conn, "symbols", "scope_chain")? {
            conn.execute(
                "ALTER TABLE symbols ADD COLUMN scope_chain TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        conn.execute(
            "UPDATE symbols
             SET decl_start = COALESCE(decl_start, start),
                 decl_end = COALESCE(decl_end, end)",
            [],
        )?;

        if !has_column(conn, "refs", "id")? || !has_column(conn, "refs", "start")? {
            conn.execute_batch(
                "ALTER TABLE refs RENAME TO refs_v2;
                 CREATE TABLE refs(
                   id INTEGER PRIMARY KEY,
                   file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                   chunk_id INTEGER,
                   start INTEGER NOT NULL,
                   line INTEGER NOT NULL,
                   kind TEXT NOT NULL,
                   confidence TEXT NOT NULL,
                   target_request TEXT,
                   target_name TEXT NOT NULL,
                   local INTEGER NOT NULL DEFAULT 0,
                   detail TEXT
                 );
                 INSERT INTO refs(
                   file_id, chunk_id, start, line, kind, confidence,
                   target_request, target_name, local, detail
                 )
                 SELECT file_id, chunk_id, 0, line, kind, confidence,
                        target_request, target_name, local, detail
                 FROM refs_v2;
                 DROP TABLE refs_v2;
                 CREATE INDEX idx_refs_file ON refs(file_id);
                 CREATE INDEX idx_refs_target ON refs(target_name);",
            )?;
        }

        // Force canonical extraction once so declaration spans and reference
        // offsets are populated for repositories indexed under schema v2.
        conn.execute("UPDATE files SET hash = ''", [])?;
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    if version < 4 {
        if !has_column(conn, "member_calls", "start")? {
            conn.execute(
                "ALTER TABLE member_calls ADD COLUMN start INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }

        // Member-call ownership in the traversal projection requires exact
        // source offsets. Force canonical extraction because legacy rows can
        // only be migrated with a placeholder offset.
        conn.execute("UPDATE files SET hash = ''", [])?;
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    if version < 5 {
        if !has_column(conn, "files", "role")? {
            conn.execute(
                "ALTER TABLE files ADD COLUMN role TEXT NOT NULL DEFAULT 'unknown'",
                [],
            )?;
        }
        // Role-aware result payloads must not appear current until every file
        // has passed through the classifier on the next index operation.
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    // The idempotent schema batch creates v6 semantic tables. Preserve their
    // rows across later source indexing so support fingerprints can expose
    // stale and degraded memory instead of silently deleting it.

    // v6 -> v7: module_edges gains resolution provenance so the projector can
    // distinguish heuristic workspace mappings from direct resolver results.
    // Edges are rebuilt on every index; only invalidate the public snapshot so
    // stale certain-labelled projections stop serving. (Fresh databases get
    // the column from the schema batch; has_column makes this idempotent.)
    if version < 7 {
        if !has_column(conn, "module_edges", "resolution")? {
            conn.execute("ALTER TABLE module_edges ADD COLUMN resolution TEXT", [])?;
        }
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    // v7 -> v8: package instances become the ownership boundary for
    // workspace and dependency files. Existing repository files remain
    // first-party by default; the next index operation can refine workspace
    // membership without forcing unchanged source through extraction again.
    if version < 8 {
        if !has_column(conn, "files", "origin")? {
            conn.execute(
                "ALTER TABLE files ADD COLUMN origin TEXT NOT NULL DEFAULT 'repository'
                 CHECK(origin IN ('repository', 'workspace', 'dependency'))",
                [],
            )?;
        }
        if !has_column(conn, "files", "package_instance_id")? {
            conn.execute(
                "ALTER TABLE files ADD COLUMN package_instance_id INTEGER
                 REFERENCES package_instances(id) ON DELETE CASCADE",
                [],
            )?;
        }
        if !has_column(conn, "files", "package_path")? {
            conn.execute("ALTER TABLE files ADD COLUMN package_path TEXT", [])?;
        }
        if !has_column(conn, "module_edges", "package_instance_id")? {
            conn.execute(
                "ALTER TABLE module_edges ADD COLUMN package_instance_id INTEGER
                 REFERENCES package_instances(id) ON DELETE SET NULL",
                [],
            )?;
        }
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    // v8 -> v9: runtime/general entities preserve per-site spans and trust
    // labels separately from snapshot-canonical identity. Existing files must
    // pass through extraction once to populate entity_sites.
    if version < 9 {
        conn.execute("UPDATE files SET hash = ''", [])?;
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute("DELETE FROM meta WHERE key IN ('snapshot', 'projection_version')", [])?;
    }

    conn.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version','9')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"
    );
    Ok(conn.query_row(&sql, [column], |r| r.get::<_, i64>(0))? != 0)
}

/// Remove a file and all derived rows (chunks/symbols/refs cascade).
/// FTS rows are removed explicitly since fts5 isn't FK-aware.
pub fn delete_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute("DELETE FROM files WHERE id = ?1", [file_id])?;
    Ok(())
}

/// Resolve an indexed file's display identity to the physical source path.
/// Repository and workspace paths remain root-relative. Dependency paths are
/// virtual display keys, so their package-relative path is joined to the
/// canonical package-instance root instead.
pub fn file_source_path(conn: &Connection, root: &Path, file_id: i64) -> Result<PathBuf> {
    let (path, origin, package_path, package_root): (
        String,
        String,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT f.path, f.origin, f.package_path, p.canonical_root
             FROM files f
             LEFT JOIN package_instances p ON p.id = f.package_instance_id
             WHERE f.id = ?1",
            [file_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .with_context(|| format!("indexed file {file_id} does not exist"))?;

    match origin.as_str() {
        "repository" | "workspace" => Ok(root.join(path)),
        "dependency" => {
            let package_path = package_path.context("dependency file has no package-relative path")?;
            let package_root = package_root.context("dependency file has no package instance")?;
            Ok(PathBuf::from(package_root).join(package_path))
        }
        other => bail!("indexed file {file_id} has invalid origin `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use super::{file_source_path, open, open_path};

    #[test]
    fn opens_an_index_database_outside_the_repository_root() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("isolated-memory.db");
        let conn = open_path(&database)?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "9");
        assert!(database.is_file());
        Ok(())
    }

    #[test]
    fn migrates_v2_symbols_and_references_without_preserving_stale_projection() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let db_path = repo.path().join(".jscout.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '2');
             CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT, hash TEXT);
             INSERT INTO files VALUES(1, 'old.ts', 'old-hash');
             CREATE TABLE symbols(
               id INTEGER PRIMARY KEY, file_id INTEGER, name TEXT, kind TEXT,
               start INTEGER, end INTEGER, line INTEGER, exported INTEGER
             );
             INSERT INTO symbols VALUES(1, 1, 'old', 'function', 3, 8, 1, 1);
             CREATE TABLE refs(
               file_id INTEGER, chunk_id INTEGER, line INTEGER, kind TEXT,
               confidence TEXT, target_request TEXT, target_name TEXT,
               local INTEGER, detail TEXT
             );
             INSERT INTO refs VALUES(1, NULL, 1, 'call', 'certain', NULL, 'old', 1, NULL);
             CREATE INDEX idx_refs_file ON refs(file_id);
             CREATE INDEX idx_refs_target ON refs(target_name);",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(version, "9");
        let symbol: (i64, i64, String) = conn.query_row(
            "SELECT decl_start, decl_end, scope_chain FROM symbols WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!(symbol, (3, 8, String::new()));
        let reference: (i64, i64) = conn.query_row(
            "SELECT id, start FROM refs WHERE target_name='old'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert!(reference.0 > 0);
        assert_eq!(reference.1, 0);
        let hash: String = conn.query_row("SELECT hash FROM files WHERE id=1", [], |r| r.get(0))?;
        assert!(hash.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_v3_member_calls_and_forces_offset_reextraction() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let db_path = repo.path().join(".jscout.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '3');
             CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT, hash TEXT);
             INSERT INTO files VALUES(1, 'old.ts', 'old-hash');
             CREATE TABLE member_calls(
               file_id INTEGER, chunk_id INTEGER, line INTEGER,
               prop TEXT, object TEXT
             );
             INSERT INTO member_calls VALUES(1, NULL, 7, 'load', 'client');",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(version, "9");
        let member_call: (i64, i64) = conn.query_row(
            "SELECT start, line FROM member_calls WHERE prop='load'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(member_call, (0, 7));
        let hash: String = conn.query_row("SELECT hash FROM files WHERE id=1", [], |r| r.get(0))?;
        assert!(hash.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_v4_files_with_unknown_roles_and_invalidates_snapshot() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let db_path = repo.path().join(".jscout.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '4');
             INSERT INTO meta VALUES('snapshot', 'stale');
             INSERT INTO meta VALUES('projection_version', '2');
             CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT, hash TEXT);
             INSERT INTO files VALUES(1, 'old.ts', 'old-hash');",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let role: String = conn.query_row("SELECT role FROM files WHERE id=1", [], |r| r.get(0))?;
        assert_eq!(role, "unknown");
        let snapshots: i64 = conn.query_row(
            "SELECT COUNT(*) FROM meta WHERE key IN ('snapshot', 'projection_version')",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(snapshots, 0);
        Ok(())
    }

    #[test]
    fn migrates_v5_by_adding_empty_semantic_memory_tables() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let database = repo.path().join(".jscout.db");
        let conn = Connection::open(&database)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '5');",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        let artifacts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM semantic_artifacts",
            [],
            |row| row.get(0),
        )?;
        let supports: i64 = conn.query_row(
            "SELECT COUNT(*) FROM semantic_supports",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "9");
        assert_eq!((artifacts, supports), (0, 0));
        Ok(())
    }

    #[test]
    fn migrates_v6_by_adding_module_edge_resolution_provenance() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let database = repo.path().join(".jscout.db");
        let conn = Connection::open(&database)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '6');
             INSERT INTO meta VALUES('snapshot', 'stale');
             INSERT INTO meta VALUES('projection_version', '2');
             CREATE TABLE module_edges(
               from_file INTEGER NOT NULL, request TEXT NOT NULL,
               to_file INTEGER, package TEXT
             );
             INSERT INTO module_edges VALUES(1, './x', 2, NULL);",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(version, "9");
        // Legacy rows read as NULL resolution (treated as plain resolver).
        let resolution: Option<String> = conn.query_row(
            "SELECT resolution FROM module_edges WHERE from_file=1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(resolution, None);
        let snapshots: i64 = conn.query_row(
            "SELECT COUNT(*) FROM meta WHERE key IN ('snapshot', 'projection_version')",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(snapshots, 0);
        Ok(())
    }

    #[test]
    fn migrates_v7_with_first_party_origin_and_package_identity_columns() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let database = repo.path().join(".jscout.db");
        let conn = Connection::open(&database)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '7');
             INSERT INTO meta VALUES('snapshot', 'stale');
             CREATE TABLE files(
               id INTEGER PRIMARY KEY, path TEXT, hash TEXT,
               role TEXT NOT NULL DEFAULT 'unknown'
             );
             INSERT INTO files VALUES(1, 'src/old.ts', 'old-hash', 'production');
             CREATE TABLE module_edges(
               from_file INTEGER NOT NULL, request TEXT NOT NULL,
               to_file INTEGER, package TEXT, resolution TEXT
             );",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "9");
        let identity: (String, Option<i64>, Option<String>) = conn.query_row(
            "SELECT origin, package_instance_id, package_path FROM files WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(identity, ("repository".into(), None, None));
        let package_column = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('module_edges')
             WHERE name='package_instance_id'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(package_column, 1);
        let snapshots: i64 = conn.query_row(
            "SELECT COUNT(*) FROM meta WHERE key IN ('snapshot', 'projection_version')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(snapshots, 0);
        Ok(())
    }

    #[test]
    fn migrates_v8_with_entity_planes_and_forces_reextraction() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let database = repo.path().join(".jscout.db");
        let conn = Connection::open(&database)?;
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '8');
             INSERT INTO meta VALUES('snapshot', 'stale');
             CREATE TABLE files(
               id INTEGER PRIMARY KEY, path TEXT, hash TEXT,
               role TEXT NOT NULL DEFAULT 'unknown',
               origin TEXT NOT NULL DEFAULT 'repository',
               package_instance_id INTEGER, package_path TEXT
             );
             INSERT INTO files VALUES(
               1, 'src/old.ts', 'old-hash', 'production', 'repository', NULL, NULL
             );",
        )?;
        drop(conn);

        let conn = open(repo.path())?;
        let version: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "9");
        let hash: String =
            conn.query_row("SELECT hash FROM files WHERE id=1", [], |row| row.get(0))?;
        assert!(hash.is_empty());
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE type='table' AND name IN (
               'entity_sites', 'entities', 'entity_occurrences', 'entity_edges'
             )",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 4);
        let snapshots: i64 = conn.query_row(
            "SELECT count(*) FROM meta WHERE key IN ('snapshot', 'projection_version')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(snapshots, 0);
        Ok(())
    }

    #[test]
    fn resolves_repository_and_dependency_file_source_paths() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let dependency = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        conn.execute(
            "INSERT INTO files(path, hash, role) VALUES('src/main.ts', 'a', 'production')",
            [],
        )?;
        let repository_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO package_instances(
               origin, name, version, canonical_root, locator, manifest_hash, status
             ) VALUES('dependency', 'left-pad', '1.3.0', ?1, 'node_modules/left-pad', 'm', 'complete')",
            [dependency.path().to_string_lossy()],
        )?;
        let package = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files(
               path, hash, role, origin, package_instance_id, package_path
             ) VALUES('dependency:left-pad@1.3.0/index.js', 'b', 'production',
                      'dependency', ?1, 'index.js')",
            [package],
        )?;
        let dependency_file = conn.last_insert_rowid();

        assert_eq!(
            file_source_path(&conn, repo.path(), repository_file)?,
            repo.path().join("src/main.ts")
        );
        assert_eq!(
            file_source_path(&conn, repo.path(), dependency_file)?,
            dependency.path().join("index.js")
        );
        Ok(())
    }
}
