use std::path::Path;

use anyhow::Result;
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

CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  hash TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'unknown'
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
  package TEXT                    -- external package name
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

    conn.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version','6')
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

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use super::{open, open_path};

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
        assert_eq!(version, "6");
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
        assert_eq!(version, "6");
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
        assert_eq!(version, "6");
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
        assert_eq!(version, "6");
        assert_eq!((artifacts, supports), (0, 0));
        Ok(())
    }
}
