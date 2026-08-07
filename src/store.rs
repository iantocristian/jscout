use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

pub const DB_FILE: &str = ".jscout.db";

pub fn db_path(root: &Path) -> std::path::PathBuf {
    root.join(DB_FILE)
}

pub fn open(root: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path(root))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  hash TEXT NOT NULL
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
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
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
"#,
    )?;
    migrate(conn)?;
    Ok(())
}

/// v1 -> v2: embeddings PK was chunk_hash alone, so different models
/// overwrote each other's vectors. The table is a cache — drop and rebuild.
fn migrate(conn: &Connection) -> Result<()> {
    let version: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .ok();
    if version.as_deref() != Some("2") {
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
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version','2')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
    }
    Ok(())
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
