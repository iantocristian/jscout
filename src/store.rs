use std::path::{Path, PathBuf};
use std::sync::Once;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

pub const DB_FILE: &str = ".jscout.db";
pub const SCHEMA_VERSION: &str = "33";
const DURABLE_SCHEMA_FLOOR: u32 = 16;

static SQLITE_VEC: Once = Once::new();

fn register_sqlite_vec() {
    SQLITE_VEC.call_once(|| unsafe {
        let entry = std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(entry));
    });
}

/// FTS5 mirror of chunk content. FTS5 tables are not foreign-key aware, so
/// this table is maintained explicitly alongside `chunks` — and recreated
/// wholesale by [`reset_extraction_state`], which must use the exact same
/// definition.
const CHUNKS_FTS_CREATE: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  content, name, symbols, path,
  tokenize="unicode61 tokenchars '_$'"
);
"#;

/// Independent BM25 corpus for authored repository documentation. Its rowids
/// are shared `chunks.id` values, but documentation rows are never mirrored
/// into `chunks_fts`, so admitting Markdown cannot alter code term statistics.
const DOCS_FTS_CREATE: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
  title, metadata, breadcrumb, body, path,
  tokenize="unicode61 tokenchars '_$'"
);
"#;

pub fn db_path(root: &Path) -> std::path::PathBuf {
    root.join(DB_FILE)
}

pub fn open(root: &Path) -> Result<Connection> {
    open_path(&db_path(root))
}

/// Open an already-published index without creating files, migrating schema,
/// or acquiring write authority. Query surfaces use this path so a typo or an
/// unindexed checkout cannot silently create a database that looks usable.
pub fn open_read_only(root: &Path) -> Result<Connection> {
    open_path_read_only(&db_path(root))
}

pub fn open_path_read_only(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        bail!(
            "index database `{}` does not exist; run `jscout index` first",
            path.display()
        );
    }
    register_sqlite_vec();
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open index database {} read-only", path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "query_only", "ON")?;

    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .with_context(|| {
            format!(
                "index database `{}` has no readable schema; run `jscout index`",
                path.display()
            )
        })?;
    if version != SCHEMA_VERSION {
        bail!(
            "index database `{}` uses schema v{version}, but this jscout requires v{SCHEMA_VERSION}; run `jscout index`",
            path.display()
        );
    }
    let (has_snapshot, projection_version): (bool, Option<String>) = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM meta WHERE key='snapshot'
         ), (
           SELECT value FROM meta WHERE key='projection_version'
         )",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some(projection_version) = projection_version.filter(|_| has_snapshot) else {
        bail!(
            "index database `{}` has no published structural snapshot; run `jscout index`",
            path.display()
        );
    };
    if projection_version != crate::structural::PROJECTION_VERSION {
        bail!(
            "index database `{}` uses structural projection v{projection_version}, but this jscout requires v{}; run `jscout index`",
            path.display(),
            crate::structural::PROJECTION_VERSION,
        );
    }
    validate_published_contracts(&conn)?;
    Ok(conn)
}

/// Reject source-derived rows produced by an incompatible binary contract.
/// Indexing is the only repair path; read and embedding surfaces must not
/// reinterpret old rows under the current extractor, Markdown format, or
/// documentation-provenance contract.
pub(crate) fn validate_published_contracts(conn: &Connection) -> Result<()> {
    let (extraction_version, documentation_chunk_format, documentation_provenance_format): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT
               (SELECT value FROM meta WHERE key='extraction_version'),
               (SELECT value FROM meta
                WHERE key='documentation_chunk_format_version'),
               (SELECT value FROM meta
                WHERE key='documentation_provenance_format_version')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let extraction_version = extraction_version.as_deref().unwrap_or("missing");
    if extraction_version != crate::entity::EXTRACTION_VERSION {
        bail!(
            "published index uses code extraction contract v{extraction_version}, but this jscout requires v{}; run `jscout index`",
            crate::entity::EXTRACTION_VERSION,
        );
    }
    let documentation_chunk_format = documentation_chunk_format.as_deref().unwrap_or("missing");
    if documentation_chunk_format != crate::docs::CHUNK_FORMAT_VERSION {
        bail!(
            "published index uses documentation chunk format {documentation_chunk_format}, but this jscout requires {}; run `jscout index`",
            crate::docs::CHUNK_FORMAT_VERSION,
        );
    }
    let documentation_provenance_format = documentation_provenance_format
        .as_deref()
        .unwrap_or("missing");
    if documentation_provenance_format != crate::docs::PROVENANCE_FORMAT_VERSION {
        bail!(
            "published index uses documentation provenance format {documentation_provenance_format}, but this jscout requires {}; run `jscout index`",
            crate::docs::PROVENANCE_FORMAT_VERSION,
        );
    }
    Ok(())
}

/// Open an index database independently of the repository root. Evaluation
/// uses this to give warm and cold sessions isolated semantic-memory state
/// while both read the same frozen source tree.
pub fn open_path(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create index database directory {} for {}",
                parent.display(),
                path.display()
            )
        })?;
    }
    register_sqlite_vec();
    let conn = Connection::open(path)
        .with_context(|| format!("open index database {} for writing", path.display()))?;
    let has_schema: bool = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'
         )",
        [],
        |row| row.get(0),
    )?;
    if has_schema {
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .context("index database has no schema version")?;
        if version != SCHEMA_VERSION {
            let parsed = version.parse::<u32>().with_context(|| {
                format!(
                    "index database `{}` has invalid schema version `{version}`",
                    path.display()
                )
            })?;
            if parsed < DURABLE_SCHEMA_FLOOR
                || parsed
                    > SCHEMA_VERSION
                        .parse::<u32>()
                        .expect("numeric schema version")
            {
                bail!(
                    "index database `{}` uses unsupported durable schema v{version}; preserve the old file if its embedding cache or semantic memory matters, then create a fresh index",
                    path.display()
                );
            }
            rebuild_legacy_disposable_schema(&conn)?;
        }
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    init_schema(&conn)?;
    Ok(conn)
}

/// One compatibility boundary replaces the historical per-version ladder.
/// Schemas at or above the durable floor already have the current embedding
/// cache and semantic-memory shapes; discard every source-derived table and
/// let the current schema recreate it. Dynamic sqlite-vec tables are retained
/// but emptied because their rows materialize snapshot-local chunk IDs.
fn rebuild_legacy_disposable_schema(conn: &Connection) -> Result<()> {
    let vector_tables = {
        let mut statement = conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND (
               (name GLOB 'vec_embeddings_[0-9]*'
                AND substr(name, length('vec_embeddings_') + 1) NOT GLOB '*[^0-9]*')
               OR
               (name GLOB 'vec_doc_embeddings_[0-9]*'
                AND substr(name, length('vec_doc_embeddings_') + 1) NOT GLOB '*[^0-9]*')
               OR
               (name GLOB 'vec_semantic_embeddings_[0-9]*'
                AND substr(name, length('vec_semantic_embeddings_') + 1) NOT GLOB '*[^0-9]*')
             )
             ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        for table in vector_tables {
            let dimensions = table
                .strip_prefix("vec_semantic_embeddings_")
                .or_else(|| table.strip_prefix("vec_doc_embeddings_"))
                .or_else(|| table.strip_prefix("vec_embeddings_"))
                .unwrap_or_default();
            if dimensions.is_empty() || !dimensions.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("invalid sqlite-vec table name `{table}`");
            }
            conn.execute(&format!("DELETE FROM {table}"), [])?;
        }
        conn.execute_batch(
            "DROP VIEW IF EXISTS code_chunks;
             DROP VIEW IF EXISTS code_files;
             DROP TABLE IF EXISTS doc_vector_generations;
             DROP TABLE IF EXISTS doc_embedding_index_entries;
             DROP TABLE IF EXISTS embedding_index_entries;
             DROP TABLE IF EXISTS semantic_embedding_index_entries;
             DROP TABLE IF EXISTS checker_occurrence_projects;
             DROP TABLE IF EXISTS checker_project_inputs;
             DROP TABLE IF EXISTS checker_project_runs;
             DROP TABLE IF EXISTS checker_input_files;
             DROP TABLE IF EXISTS checker_enrichments;
             DROP TABLE IF EXISTS checker_enrichment_batches;
             DROP TABLE IF EXISTS repository_file_policy;
             DROP TABLE IF EXISTS repository_current_classifications;
             DROP TABLE IF EXISTS entity_edges;
             DROP TABLE IF EXISTS entity_occurrences;
             DROP TABLE IF EXISTS entities;
             DROP TABLE IF EXISTS entity_sites;
             DROP TABLE IF EXISTS resolved_edges;
             DROP TABLE IF EXISTS graph_nodes;
             DROP TABLE IF EXISTS contract_imports;
             DROP TABLE IF EXISTS contract_exports;
             DROP TABLE IF EXISTS module_edges;
             DROP TABLE IF EXISTS refs;
             DROP TABLE IF EXISTS events;
             DROP TABLE IF EXISTS receiver_value_flows;
             DROP TABLE IF EXISTS function_return_flows;
             DROP TABLE IF EXISTS value_binding_flows;
             DROP TABLE IF EXISTS instance_method_value_flows;
             DROP TABLE IF EXISTS class_member_value_flow_blockers;
             DROP TABLE IF EXISTS class_value_flows;
             DROP TABLE IF EXISTS member_calls;
             DROP TABLE IF EXISTS imports;
             DROP TABLE IF EXISTS exports;
             DROP TABLE IF EXISTS docs_fts;
             DROP TABLE IF EXISTS chunks_fts;
             DROP TABLE IF EXISTS doc_file_provenance;
             DROP TABLE IF EXISTS doc_chunk_meta;
             DROP TABLE IF EXISTS doc_blame_cache;
             DROP TABLE IF EXISTS doc_inventory;
             DROP TABLE IF EXISTS chunks;
             DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS files;
             DROP TABLE IF EXISTS package_instances;
             DELETE FROM meta
             WHERE key IN (
               'root', 'snapshot', 'projection_version', 'resolution_hash',
               'extraction_version', 'documentation_chunk_format_version',
               'documentation_provenance_format_version'
             ) OR key LIKE 'embedding_index_synced_v1:%'
               OR key LIKE 'semantic_embedding_index_synced_v1:%';
             UPDATE meta SET value='33' WHERE key='schema_version';",
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

pub(crate) fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
INSERT INTO meta(key, value) VALUES('schema_version', '33')
  ON CONFLICT(key) DO UPDATE SET value=excluded.value;

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
  corpus TEXT NOT NULL CHECK(corpus IN ('code', 'docs')),
  format TEXT NOT NULL CHECK(length(trim(format)) > 0),
  role TEXT NOT NULL DEFAULT 'unknown',
  origin TEXT NOT NULL DEFAULT 'repository'
    CHECK(origin IN ('repository', 'workspace', 'dependency')),
  package_instance_id INTEGER REFERENCES package_instances(id) ON DELETE CASCADE,
  package_path TEXT
);
CREATE INDEX IF NOT EXISTS idx_files_corpus ON files(corpus);

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

-- Documentation chunks share files/chunks and the structural snapshot, while
-- their retrieval-only fields remain in this disposable sidecar. Corpus
-- membership is an explicit property of `files`; this table cannot make a
-- code file into documentation by accident or omission.
CREATE TABLE IF NOT EXISTS doc_chunk_meta(
  chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  description TEXT,
  tags_json TEXT NOT NULL DEFAULT '[]',
  breadcrumb TEXT NOT NULL DEFAULT '',
  nearest_heading TEXT,
  ordinal INTEGER NOT NULL,
  embedding_identity TEXT,
  front_matter_state TEXT NOT NULL,
  freshness_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK(freshness_basis IN ('git', 'working_tree', 'observed', 'unknown')),
  freshness_author_time INTEGER,
  freshness_committer_time INTEGER,
  freshness_detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_doc_chunk_embedding_identity
  ON doc_chunk_meta(embedding_identity);

-- Current-snapshot provenance identity and diagnostic for each admitted
-- documentation file. This sidecar is disposable with the file rows.
CREATE TABLE IF NOT EXISTS doc_file_provenance(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  projection_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  detail TEXT
);

-- Rebuildable blame mappings. One current entry per indexed-root-scoped path
-- prevents this cache from becoming a document-history store, while the
-- complete key avoids reuse across roots, worktree edits, rewritten path
-- history, Git conversion-state changes, or clone deepening.
CREATE TABLE IF NOT EXISTS doc_blame_cache(
  path_scope TEXT NOT NULL,
  path TEXT NOT NULL,
  bytes_hash TEXT NOT NULL,
  converted_blob_oid TEXT NOT NULL,
  path_tip TEXT NOT NULL,
  shallow_fingerprint TEXT NOT NULL,
  attribution_json TEXT NOT NULL,
  format_version TEXT NOT NULL,
  PRIMARY KEY(path_scope,path)
);

CREATE TRIGGER IF NOT EXISTS doc_chunk_meta_requires_docs_insert
BEFORE INSERT ON doc_chunk_meta
WHEN NOT EXISTS (
  SELECT 1
  FROM chunks chunk
  JOIN files file ON file.id=chunk.file_id
  WHERE chunk.id=NEW.chunk_id AND file.corpus='docs'
)
BEGIN
  SELECT RAISE(ABORT, 'doc_chunk_meta requires a docs-corpus file');
END;

CREATE TRIGGER IF NOT EXISTS doc_chunk_meta_requires_docs_update
BEFORE UPDATE OF chunk_id ON doc_chunk_meta
WHEN NOT EXISTS (
  SELECT 1
  FROM chunks chunk
  JOIN files file ON file.id=chunk.file_id
  WHERE chunk.id=NEW.chunk_id AND file.corpus='docs'
)
BEGIN
  SELECT RAISE(ABORT, 'doc_chunk_meta requires a docs-corpus file');
END;

CREATE TRIGGER IF NOT EXISTS files_docs_sidecar_preserves_corpus
BEFORE UPDATE OF corpus ON files
WHEN NEW.corpus!='docs' AND EXISTS (
  SELECT 1
  FROM chunks chunk
  JOIN doc_chunk_meta doc ON doc.chunk_id=chunk.id
  WHERE chunk.file_id=OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'a file with doc_chunk_meta must remain in the docs corpus');
END;

CREATE TRIGGER IF NOT EXISTS chunks_docs_sidecar_preserves_corpus
BEFORE UPDATE OF file_id ON chunks
WHEN EXISTS (SELECT 1 FROM doc_chunk_meta doc WHERE doc.chunk_id=OLD.id)
 AND NOT EXISTS (SELECT 1 FROM files file WHERE file.id=NEW.file_id AND file.corpus='docs')
BEGIN
  SELECT RAISE(ABORT, 'a chunk with doc_chunk_meta requires a docs-corpus file');
END;

-- Keep the code/docs corpus boundary in one schema object instead of asking
-- every code consumer to reproduce it. Format identifies the parser family;
-- corpus alone determines which retrieval plane owns the file.
CREATE VIEW IF NOT EXISTS code_files AS
SELECT file.*
FROM files file
WHERE file.corpus='code';

CREATE VIEW IF NOT EXISTS code_chunks AS
SELECT chunk.*
FROM chunks chunk
JOIN code_files file ON file.id=chunk.file_id;

-- Current-snapshot membership diagnostics. Pruned directories and rejected
-- files have no files/chunks row, so this projection is intentionally not
-- foreign-keyed to the admitted corpus.
CREATE TABLE IF NOT EXISTS doc_inventory(
  path TEXT NOT NULL,
  subject TEXT NOT NULL,
  rule TEXT NOT NULL,
  detail TEXT,
  path_base64 TEXT,
  path_encoding TEXT
);
CREATE INDEX IF NOT EXISTS idx_doc_inventory_path_subject
  ON doc_inventory(path, subject);

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

-- Type/documentary module bindings are deliberately separate from runtime
-- imports and exports. Structural call resolution must never infer execution
-- from a type-only relationship.
CREATE TABLE IF NOT EXISTS contract_exports(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  export_name TEXT NOT NULL,
  local_name TEXT,
  from_request TEXT,
  from_name TEXT
);
CREATE INDEX IF NOT EXISTS idx_contract_exports_file ON contract_exports(file_id);

CREATE TABLE IF NOT EXISTS contract_imports(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  local_name TEXT NOT NULL,
  imported_name TEXT NOT NULL,
  request TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_contract_imports_file ON contract_imports(file_id);

CREATE TABLE IF NOT EXISTS module_edges(
  from_file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  request TEXT NOT NULL,
  to_file INTEGER,                -- resolved in-repo file
  package TEXT,                   -- external package name
  resolution TEXT,                -- resolver | workspace | workspace-inferred
  package_instance_id INTEGER REFERENCES package_instances(id) ON DELETE SET NULL,
  type_only INTEGER NOT NULL DEFAULT 0 CHECK(type_only IN (0, 1))
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
-- Receiver-value projection resolves an extracted binding by its exact source
-- span. The leading file_id column still serves existing file-only lookups.
CREATE INDEX IF NOT EXISTS idx_refs_file_start ON refs(file_id, start);
CREATE INDEX IF NOT EXISTS idx_refs_target ON refs(target_name);

CREATE TABLE IF NOT EXISTS events(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  line INTEGER NOT NULL,
  role TEXT NOT NULL,             -- emit | listen
  name TEXT NOT NULL,
  method TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_file ON events(file_id);
CREATE INDEX IF NOT EXISTS idx_events_name ON events(name);

CREATE TABLE IF NOT EXISTS member_calls(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  chunk_id INTEGER,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL DEFAULT 0,   -- complete CallExpression span end
  line INTEGER NOT NULL,
  end_line INTEGER NOT NULL DEFAULT 0,
  prop TEXT NOT NULL,
  object TEXT,
  receiver TEXT,                    -- full static chain, e.g. dbs.wave.card
  receiver_start INTEGER NOT NULL DEFAULT 0,
  receiver_end INTEGER NOT NULL DEFAULT 0,
  property_start INTEGER NOT NULL DEFAULT 0,
  property_end INTEGER NOT NULL DEFAULT 0,
  receiver_unbound INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_member_calls_file
  ON member_calls(file_id, receiver_start, prop);
CREATE INDEX IF NOT EXISTS idx_member_calls_prop ON member_calls(prop);

-- Closed syntax-and-binding facts for occurrence-specific receiver value flow.
-- Absence means the extractor deliberately gave up; projection never fills a
-- missing fact by name alone.
CREATE TABLE IF NOT EXISTS receiver_value_flows(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  call_start INTEGER NOT NULL,
  call_end INTEGER NOT NULL,
  receiver_kind TEXT NOT NULL CHECK(receiver_kind IN ('this', 'value')),
  class_name TEXT,
  class_start INTEGER,
  value_kind TEXT CHECK(value_kind IN ('construct', 'factory', 'binding')),
  target_kind TEXT CHECK(target_kind IN ('identifier', 'member')),
  target_name TEXT,
  target_start INTEGER,
  CHECK(
    (receiver_kind='this' AND class_name IS NOT NULL AND class_start IS NOT NULL
      AND value_kind IS NULL AND target_kind IS NULL
      AND target_name IS NULL AND target_start IS NULL)
    OR
    (receiver_kind='value' AND class_name IS NULL AND class_start IS NULL
      AND value_kind IS NOT NULL AND target_kind IS NOT NULL
      AND target_name IS NOT NULL AND target_start IS NOT NULL)
  ),
  UNIQUE(file_id, call_start, call_end)
);
CREATE INDEX IF NOT EXISTS idx_receiver_value_flows_call
  ON receiver_value_flows(file_id, call_start, call_end);

-- A function appears here only when it has at least one return and every
-- return is a supported construct/binding/factory shape. One unsupported
-- return suppresses the complete summary.
CREATE TABLE IF NOT EXISTS function_return_flows(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  function_name TEXT NOT NULL,
  function_start INTEGER NOT NULL,
  function_async INTEGER NOT NULL CHECK(function_async IN (0, 1)),
  return_index INTEGER NOT NULL,
  value_kind TEXT NOT NULL CHECK(value_kind IN ('construct', 'factory', 'binding')),
  target_kind TEXT NOT NULL CHECK(target_kind IN ('identifier', 'member')),
  target_name TEXT NOT NULL,
  target_start INTEGER NOT NULL,
  UNIQUE(file_id, function_start, return_index)
);
CREATE INDEX IF NOT EXISTS idx_function_return_flows_binding
  ON function_return_flows(file_id, function_start);

CREATE TABLE IF NOT EXISTS value_binding_flows(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  binding_name TEXT NOT NULL,
  binding_start INTEGER NOT NULL,
  value_kind TEXT NOT NULL CHECK(value_kind IN ('construct', 'factory', 'binding')),
  target_kind TEXT NOT NULL CHECK(target_kind IN ('identifier', 'member')),
  target_name TEXT NOT NULL,
  target_start INTEGER NOT NULL,
  PRIMARY KEY(file_id, binding_start)
);

CREATE TABLE IF NOT EXISTS class_value_flows(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  class_name TEXT NOT NULL,
  class_start INTEGER NOT NULL,
  super_name TEXT,
  super_start INTEGER,
  super_kind TEXT CHECK(super_kind IN ('identifier', 'member')),
  CHECK((super_name IS NULL) = (super_start IS NULL)),
  CHECK((super_name IS NULL) = (super_kind IS NULL)),
  PRIMARY KEY(file_id, class_start)
);
CREATE INDEX IF NOT EXISTS idx_class_value_flows_name
  ON class_value_flows(file_id, class_name);

CREATE TABLE IF NOT EXISTS instance_method_value_flows(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  class_start INTEGER NOT NULL,
  method_name TEXT NOT NULL,
  method_start INTEGER NOT NULL,
  PRIMARY KEY(file_id, method_start)
);

CREATE TABLE IF NOT EXISTS class_member_value_flow_blockers(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  class_start INTEGER NOT NULL,
  member_name TEXT NOT NULL,
  PRIMARY KEY(file_id, class_start, member_name)
);

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
  plane TEXT NOT NULL CHECK(plane IN ('runtime', 'contract', 'general')),
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
  plane TEXT NOT NULL CHECK(plane IN ('runtime', 'contract', 'general')),
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

CREATE TABLE IF NOT EXISTS embedding_profiles(
  id INTEGER PRIMARY KEY,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  config_fingerprint TEXT UNIQUE NOT NULL,
  dimensions INTEGER NOT NULL,
  config_json TEXT NOT NULL
);

-- Content-addressed cache. Multiple chunks with the same hash share these
-- bytes, while embedding_index_entries materializes each current occurrence.
CREATE TABLE IF NOT EXISTS embeddings(
  chunk_hash TEXT NOT NULL,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  vec BLOB NOT NULL,
  PRIMARY KEY (chunk_hash, profile_id)
);

-- Generated semantic artifacts use their own document namespace. Their
-- compact descriptive text is content-addressed independently of code chunks,
-- so unchanged cards/workflows/summaries reuse vectors across snapshots.
CREATE TABLE IF NOT EXISTS semantic_embeddings(
  document_hash TEXT NOT NULL,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  vec BLOB NOT NULL,
  PRIMARY KEY (document_hash, profile_id)
);

CREATE TABLE IF NOT EXISTS embedding_index_entries(
  id INTEGER PRIMARY KEY,
  chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  UNIQUE (chunk_id, profile_id)
);
CREATE INDEX IF NOT EXISTS idx_embedding_entries_profile
  ON embedding_index_entries(profile_id, chunk_id);

-- Documentation vector occurrences use their own sqlite-vec tables because
-- sqlite-vec applies KNN's k before a relational corpus filter can run.
CREATE TABLE IF NOT EXISTS doc_embedding_index_entries(
  id INTEGER PRIMARY KEY,
  chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  UNIQUE (chunk_id, profile_id)
);
CREATE INDEX IF NOT EXISTS idx_doc_embedding_entries_profile
  ON doc_embedding_index_entries(profile_id, chunk_id);

CREATE TABLE IF NOT EXISTS doc_vector_generations(
  snapshot TEXT NOT NULL,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  dimensions INTEGER NOT NULL,
  chunk_format_version TEXT NOT NULL,
  PRIMARY KEY (snapshot, profile_id, dimensions, chunk_format_version)
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
CREATE INDEX IF NOT EXISTS idx_graph_nodes_native
  ON graph_nodes(native_id, native_table);

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

-- Canonical TypeScript-checker facts are exact-snapshot public data. A watch
-- refresh may temporarily retain the prior active batch plus the most useful
-- superseded staging batch as hidden sources for validated, per-project carry
-- into the next snapshot; manual indexing clears the plane.
-- Source/target identities are deliberately not foreign keys because a
-- projection rebuild must not cascade through canonical checker facts.
CREATE TABLE IF NOT EXISTS checker_enrichment_batches(
  id INTEGER PRIMARY KEY,
  source_snapshot TEXT NOT NULL,
  checker_version TEXT NOT NULL,
  checker_source TEXT NOT NULL,
  checker_input_fingerprint TEXT NOT NULL,
  sidecar_protocol INTEGER NOT NULL,
  plan_fingerprint TEXT NOT NULL DEFAULT '',
  selected_occurrences INTEGER NOT NULL DEFAULT 0,
  total_projects INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0, 1))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_checker_one_active_batch
  ON checker_enrichment_batches(active) WHERE active = 1;
CREATE INDEX IF NOT EXISTS idx_checker_staging_plan
  ON checker_enrichment_batches(source_snapshot, plan_fingerprint, active);

CREATE TABLE IF NOT EXISTS checker_project_runs(
  batch_id INTEGER NOT NULL REFERENCES checker_enrichment_batches(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'partial', 'failed')),
  selected_occurrences INTEGER NOT NULL,
  completed_occurrences INTEGER NOT NULL DEFAULT 0,
  planning_fingerprint TEXT NOT NULL DEFAULT '',
  checker_input_fingerprint TEXT,
  execution_kind TEXT NOT NULL DEFAULT 'checked'
    CHECK(execution_kind IN ('checked', 'carried', 'mixed')),
  peak_rss_bytes INTEGER NOT NULL DEFAULT 0,
  peak_heap_bytes INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(batch_id, project_id)
);

CREATE TABLE IF NOT EXISTS checker_project_inputs(
  batch_id INTEGER NOT NULL,
  project_id TEXT NOT NULL,
  input_kind TEXT NOT NULL CHECK(input_kind IN ('repository', 'absolute')),
  input_path TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  PRIMARY KEY(batch_id, project_id, input_kind, input_path),
  FOREIGN KEY(batch_id, project_id)
    REFERENCES checker_project_runs(batch_id, project_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS checker_enrichments(
  id INTEGER PRIMARY KEY,
  batch_id INTEGER NOT NULL REFERENCES checker_enrichment_batches(id) ON DELETE CASCADE,
  member_call_id INTEGER NOT NULL,
  source_file_id INTEGER NOT NULL,
  source_file TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  call_start INTEGER NOT NULL,
  call_end INTEGER NOT NULL,
  receiver_start INTEGER NOT NULL,
  receiver_end INTEGER NOT NULL,
  property_start INTEGER NOT NULL,
  property_end INTEGER NOT NULL,
  project_id TEXT NOT NULL,
  receiver_type TEXT,
  target_anchor TEXT NOT NULL,
  target_fingerprint TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  provenance TEXT NOT NULL CHECK(provenance = 'checker'),
  checker_input_fingerprint TEXT NOT NULL,
  UNIQUE(batch_id, member_call_id, project_id, target_anchor)
);
CREATE INDEX IF NOT EXISTS idx_checker_enrichments_source
  ON checker_enrichments(source_file, call_start);
CREATE INDEX IF NOT EXISTS idx_checker_enrichments_target
  ON checker_enrichments(target_anchor);
-- One row per owning-project answer, including `unknown`. Facts record mapped
-- targets; this table preserves coverage so partial checker success stays
-- visible in the exact-snapshot projection.
CREATE TABLE IF NOT EXISTS checker_occurrence_projects(
  batch_id INTEGER NOT NULL REFERENCES checker_enrichment_batches(id) ON DELETE CASCADE,
  member_call_id INTEGER NOT NULL,
  source_file TEXT NOT NULL DEFAULT '',
  source_hash TEXT NOT NULL DEFAULT '',
  call_start INTEGER NOT NULL DEFAULT 0,
  call_end INTEGER NOT NULL DEFAULT 0,
  receiver_start INTEGER NOT NULL DEFAULT 0,
  receiver_end INTEGER NOT NULL DEFAULT 0,
  property_start INTEGER NOT NULL DEFAULT 0,
  property_end INTEGER NOT NULL DEFAULT 0,
  project_id TEXT NOT NULL,
  checker_input_fingerprint TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('resolved', 'unknown', 'failed')),
  PRIMARY KEY(batch_id, member_call_id, project_id)
);
-- One row per generative model run. Failures, exclusions, cost, and
-- provenance stay attributable; artifacts reference the run that produced
-- them. Statuses: running | completed | incomplete | failed | canceled |
-- superseded. Billing paths (plan | api | custom) are never pooled.
CREATE TABLE IF NOT EXISTS scout_runs(
  id INTEGER PRIMARY KEY,
  scout_kind TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN (
    'running', 'completed', 'incomplete', 'failed', 'canceled', 'superseded'
  )),
  gateway_protocol INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  billing_path TEXT NOT NULL CHECK(billing_path IN ('plan', 'api', 'custom')),
  reasoning TEXT,
  prompt_version TEXT NOT NULL,
  source_snapshot TEXT NOT NULL,
  input_fingerprint TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  config_json TEXT NOT NULL DEFAULT '{}',
  usage_json TEXT,
  error_code TEXT,
  started_at TEXT NOT NULL,
  completed_at TEXT
);
-- One live claim per input: a second scout of the same inputs either reuses
-- the completed run or fails loudly against the in-flight one. --rebuild
-- supersedes the completed run first.
CREATE UNIQUE INDEX IF NOT EXISTS idx_scout_runs_active
  ON scout_runs(scout_kind, input_fingerprint)
  WHERE status IN ('running', 'completed');
CREATE INDEX IF NOT EXISTS idx_scout_runs_status ON scout_runs(status, scout_kind);

-- G13 repository reconnaissance is policy metadata, not semantic graph
-- memory. Classifications are immutable and durable across disposable source
-- snapshots. `evidence_fingerprint` is deliberately snapshot-free: it covers
-- exact subject membership, selected representative content, and deterministic
-- disk evidence.
CREATE TABLE IF NOT EXISTS repository_classifications(
  id INTEGER PRIMARY KEY,
  run_id INTEGER UNIQUE NOT NULL REFERENCES scout_runs(id) ON DELETE CASCADE,
  subject_key TEXT NOT NULL,
  subject_kind TEXT NOT NULL CHECK(subject_kind IN ('package', 'area', 'project')),
  selector_json TEXT NOT NULL,
  parent_subject_key TEXT,
  depth INTEGER NOT NULL CHECK(depth >= 0),
  role TEXT NOT NULL CHECK(role IN (
    'runtime', 'tooling', 'documentation', 'test', 'generated',
    'mixed', 'unknown'
  )),
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  explanation TEXT NOT NULL,
  citations_json TEXT NOT NULL,
  cited_evidence_json TEXT NOT NULL DEFAULT '[]',
  evidence_fingerprint TEXT NOT NULL,
  classification_fingerprint TEXT NOT NULL,
  source_snapshot TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_classifications_subject
  ON repository_classifications(subject_key, id DESC);
CREATE INDEX IF NOT EXISTS idx_repository_classifications_evidence
  ON repository_classifications(subject_key, evidence_fingerprint, id DESC);

-- Snapshot-local acceleration plane. It is rebuilt deterministically from
-- fresh, likely scope classifications after indexing/scouting; stale,
-- possible, mixed, and unknown results never hide or penalize a file.
CREATE TABLE IF NOT EXISTS repository_file_policy(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  classification_id INTEGER NOT NULL REFERENCES repository_classifications(id),
  subject_key TEXT NOT NULL,
  scope_role TEXT NOT NULL CHECK(scope_role IN (
    'runtime', 'tooling', 'documentation', 'test', 'generated'
  )),
  effective_role TEXT NOT NULL CHECK(effective_role IN (
    'runtime', 'tooling', 'documentation', 'test', 'fixture', 'generated'
  )),
  source_hash TEXT NOT NULL,
  depth INTEGER NOT NULL CHECK(depth >= 0)
);
CREATE INDEX IF NOT EXISTS idx_repository_file_policy_classification
  ON repository_file_policy(classification_id);

-- Current, exact-fingerprint scope classifications, including neutral
-- possible/mixed/unknown results. This disposable projection lets read-only
-- overview calls explain the active semantic policy without pretending that
-- immutable historical rows are current after a branch or membership change.
CREATE TABLE IF NOT EXISTS repository_current_classifications(
  classification_id INTEGER PRIMARY KEY REFERENCES repository_classifications(id) ON DELETE CASCADE,
  subject_key TEXT UNIQUE NOT NULL,
  subject_kind TEXT NOT NULL CHECK(subject_kind IN ('package', 'area')),
  role TEXT NOT NULL CHECK(role IN (
    'runtime', 'tooling', 'documentation', 'test', 'generated',
    'mixed', 'unknown'
  )),
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  explanation TEXT NOT NULL,
  citations_json TEXT NOT NULL,
  cited_evidence_json TEXT NOT NULL,
  member_count INTEGER NOT NULL CHECK(member_count >= 0),
  deterministic_roles_json TEXT NOT NULL,
  effective_roles_json TEXT NOT NULL,
  conflict_files INTEGER NOT NULL CHECK(conflict_files >= 0),
  depth INTEGER NOT NULL CHECK(depth >= 0),
  prompt_version TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_current_role
  ON repository_current_classifications(role, confidence, subject_key);

-- Every deterministic candidate's decision for a run, including exclusions
-- (which never become semantic supports).
CREATE TABLE IF NOT EXISTS scout_classifications(
  run_id INTEGER NOT NULL REFERENCES scout_runs(id) ON DELETE CASCADE,
  anchor_key TEXT NOT NULL,
  decision TEXT NOT NULL CHECK(decision IN ('defining', 'supporting', 'excluded')),
  role TEXT,
  evidence_json TEXT NOT NULL,
  PRIMARY KEY(run_id, anchor_key)
);

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
  created_at TEXT NOT NULL,
  scout_run_id INTEGER REFERENCES scout_runs(id),
  input_fingerprint TEXT,
  artifact_fingerprint TEXT
);

-- Artifact-to-artifact dependencies that source spans cannot express
-- (summaries over child artifacts, concept links). `dst_fingerprint` pins
-- the dependent's view of the child so a changed child degrades the parent
-- even when leaf source lines did not change.
CREATE TABLE IF NOT EXISTS semantic_relations(
  src_artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id) ON DELETE CASCADE,
  dst_artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id),
  -- `names_concept` is reserved for a future explicit source-artifact ->
  -- concept assertion. Current generated concepts point in the opposite
  -- direction (concept -> evidence-bearing child) and therefore use
  -- `related_to`.
  relation TEXT NOT NULL CHECK(relation IN ('summarizes', 'names_concept', 'related_to')),
  claim_path TEXT NOT NULL,
  confidence TEXT NOT NULL CHECK(confidence IN ('likely', 'possible')),
  dst_fingerprint TEXT NOT NULL,
  PRIMARY KEY(src_artifact_id, dst_artifact_id, relation, claim_path)
);
CREATE INDEX IF NOT EXISTS idx_semantic_relations_dst
  ON semantic_relations(dst_artifact_id);
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

-- Snapshot-independent materialization identity for semantic-vector KNN.
-- The virtual sqlite-vec table has no foreign keys, so this regular table is
-- authoritative and its rows are repaired by `jscout embed --semantic`.
CREATE TABLE IF NOT EXISTS semantic_embedding_index_entries(
  id INTEGER PRIMARY KEY,
  artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id) ON DELETE CASCADE,
  profile_id INTEGER NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
  document_hash TEXT NOT NULL,
  UNIQUE (artifact_id, profile_id)
);
CREATE INDEX IF NOT EXISTS idx_semantic_embedding_entries_profile
  ON semantic_embedding_index_entries(profile_id, artifact_id);
",
    )?;
    conn.execute_batch(CHUNKS_FTS_CREATE)?;
    conn.execute_batch(DOCS_FTS_CREATE)?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_files_origin ON files(origin);
         CREATE INDEX IF NOT EXISTS idx_files_package_instance ON files(package_instance_id);
         CREATE INDEX IF NOT EXISTS idx_module_edges_package_instance
           ON module_edges(package_instance_id);",
    )?;
    // The cited-evidence payload was added while schema v20 was under review.
    // Keep databases created by an earlier v20 commit usable without dropping
    // their new durable reconnaissance history.
    let has_cited_evidence: bool = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM pragma_table_info('repository_classifications')
           WHERE name='cited_evidence_json'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_cited_evidence {
        conn.execute(
            "ALTER TABLE repository_classifications
             ADD COLUMN cited_evidence_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
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
            let _ = conn.execute_batch(&format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}"));
            Err(error)
        }
    }
}

/// Canonical provenance fields of one semantic artifact.
pub(crate) struct ArtifactIdentity<'a> {
    pub artifact_type: &'a str,
    pub canonical_name: Option<&'a str>,
    pub body_json: &'a str,
    pub model: &'a str,
    pub prompt_version: &'a str,
    pub confidence: &'a str,
    pub source_snapshot: &'a str,
}

/// Canonical content identity of a semantic artifact: serialized body,
/// provenance, confidence, snapshot, and every support in sorted order.
/// Shared by every artifact write and hierarchical freshness check.
pub(crate) fn artifact_fingerprint(
    identity: &ArtifactIdentity<'_>,
    supports: &mut [Vec<String>],
) -> String {
    supports.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-semantic-artifact\0");
    for part in [
        identity.artifact_type,
        identity.canonical_name.unwrap_or(""),
        identity.body_json,
        identity.model,
        identity.prompt_version,
        identity.confidence,
        identity.source_snapshot,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    for support in supports {
        for field in support.iter() {
            hasher.update(field.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"\x01");
    }
    hasher.finalize().to_hex().to_string()
}

/// Wholesale replacement for per-file deletion when (nearly) every file is
/// about to be re-extracted, e.g. after an extractor-version change clears
/// file hashes.
/// Cascading [`delete_file`] through tens of thousands of files re-scans the
/// large evidence tables and the FTS index once per file; truncating every
/// extraction-derived table and the disposable projection outright keeps a
/// forced re-index at fresh-index cost. The caller owns the surrounding
/// transaction and must re-insert every file before committing. Semantic
/// memory (`scout_runs`, `scout_classifications`, semantic_*), package identity
/// (`package_instances`), checker facts, and the content-addressed embedding
/// cache survive. [`reset_snapshot_state`] widens this to every
/// snapshot-derived table for the normal fixed-snapshot refresh path.
pub(crate) fn reset_extraction_state(conn: &Connection) -> Result<()> {
    crate::embed::clear_vector_rows(conn)?;
    // Children before parents, so foreign-key enforcement only ever checks
    // already-emptied referencing tables. `entities` and the graph tables are
    // disposable projection state rebuilt by the next projection pass.
    conn.execute_batch(
        "DELETE FROM repository_file_policy;
         DELETE FROM repository_current_classifications;
         DELETE FROM doc_vector_generations;
         DELETE FROM doc_embedding_index_entries;
         DELETE FROM embedding_index_entries;
         DELETE FROM entity_edges;
         DELETE FROM entity_occurrences;
         DELETE FROM entities;
         DELETE FROM entity_sites;
         DELETE FROM refs;
         DELETE FROM events;
         DELETE FROM receiver_value_flows;
         DELETE FROM function_return_flows;
         DELETE FROM value_binding_flows;
         DELETE FROM instance_method_value_flows;
         DELETE FROM class_member_value_flow_blockers;
         DELETE FROM class_value_flows;
         DELETE FROM member_calls;
         DELETE FROM imports;
         DELETE FROM exports;
         DELETE FROM contract_imports;
         DELETE FROM contract_exports;
         DELETE FROM module_edges;
         DELETE FROM doc_file_provenance;
         DELETE FROM doc_chunk_meta;
         DELETE FROM doc_inventory;
         DELETE FROM chunks;
         DELETE FROM files;
         DELETE FROM resolved_edges;
         DELETE FROM graph_nodes;
         DROP TABLE docs_fts;
         DROP TABLE chunks_fts;",
    )?;
    conn.execute_batch(CHUNKS_FTS_CREATE)?;
    conn.execute_batch(DOCS_FTS_CREATE)?;
    Ok(())
}

/// Delete the cheap source-derived rows for one repository snapshot while
/// preserving the content-addressed embedding cache, durable semantic memory,
/// and temporarily hidden checker batches. After computing the rebuilt
/// snapshot, the caller applies its checker-retention policy before projection;
/// projection accepts only the exact current-snapshot batch. The caller owns
/// the transaction and must not publish a new snapshot marker until extraction,
/// resolution, projection, and cached-vector rematerialization have completed.
pub(crate) fn reset_snapshot_state(conn: &Connection) -> Result<()> {
    reset_extraction_state(conn)?;
    conn.execute_batch(
        "DELETE FROM package_instances;
         DELETE FROM meta
         WHERE key IN ('root', 'snapshot', 'projection_version', 'resolution_hash');",
    )?;
    Ok(())
}

/// Manual fixed-snapshot indexing starts the optional checker plane from
/// scratch. This is deliberately caller policy rather than a user-facing
/// retention flag.
pub(crate) fn clear_checker_batches(conn: &Connection) -> Result<bool> {
    let changed = conn.execute("DELETE FROM checker_enrichment_batches", [])? != 0;
    Ok(changed)
}

/// Keep the active publication plus one superseded staging carry source.
///
/// Prefer the newest inactive batch containing at least one fully completed,
/// coverage-complete project. This matters after a crash between opening an
/// empty destination and copying its predecessor: the empty destination has a
/// newer row id but must not displace the useful completed source. If no
/// inactive batch has reusable coverage, retain only the newest marker.
/// Ordinary projection still requires the active batch's source snapshot to
/// equal the current snapshot.
pub(crate) fn preserve_checker_carry_source_for_watch(conn: &Connection) -> Result<bool> {
    let changed = conn.execute(
        "DELETE FROM checker_enrichment_batches
         WHERE active=0
           AND id!=(
             SELECT candidate.id FROM checker_enrichment_batches candidate
             WHERE candidate.active=0
             ORDER BY EXISTS(
               SELECT 1 FROM checker_project_runs run
               WHERE run.batch_id=candidate.id
                 AND run.status='completed'
                 AND run.selected_occurrences>0
                 AND run.completed_occurrences=run.selected_occurrences
                 AND (
                   SELECT count(*) FROM checker_occurrence_projects coverage
                   WHERE coverage.batch_id=run.batch_id
                     AND coverage.project_id=run.project_id
                 )=run.selected_occurrences
                 AND NOT EXISTS(
                   SELECT 1 FROM checker_occurrence_projects coverage
                   WHERE coverage.batch_id=run.batch_id
                     AND coverage.project_id=run.project_id
                     AND coverage.status='failed'
                 )
             ) DESC, candidate.id DESC
             LIMIT 1
           )",
        [],
    )? != 0;
    Ok(changed)
}

/// Remove a file and all derived rows (chunks/symbols/refs cascade).
/// FTS rows are removed explicitly since fts5 isn't FK-aware.
pub fn delete_file(conn: &Connection, file_id: i64) -> Result<()> {
    let exists = conn
        .query_row("SELECT 1 FROM files WHERE id=?1", [file_id], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?
        .is_some();
    if !exists {
        return Ok(());
    }
    crate::embed::delete_vector_rows_for_file(conn, file_id)?;
    conn.execute(
        "DELETE FROM docs_fts WHERE rowid IN (SELECT id FROM chunks WHERE file_id = ?1)",
        [file_id],
    )?;
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
            let package_path =
                package_path.context("dependency file has no package-relative path")?;
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

    use super::{
        DB_FILE, SCHEMA_VERSION, delete_file, file_source_path, open, open_path,
        open_path_read_only, open_read_only, reset_extraction_state,
    };

    fn index_columns(conn: &Connection, index: &str) -> Result<Vec<String>> {
        let mut statement =
            conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let rows = statement.query_map([index], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn relation_columns(conn: &Connection, relation: &str) -> Result<Vec<String>> {
        let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
        let rows = statement.query_map([relation], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    #[test]
    fn read_only_open_never_creates_or_migrates_an_index() -> Result<()> {
        let missing_root = tempfile::tempdir()?;
        let missing_database = missing_root.path().join(DB_FILE);
        let error =
            open_read_only(missing_root.path()).expect_err("missing read-only index must fail");
        assert!(error.to_string().contains("does not exist"));
        assert!(!missing_database.exists());

        let old_directory = tempfile::tempdir()?;
        let old_database = old_directory.path().join("old.db");
        let old = Connection::open(&old_database)?;
        old.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '14');
             INSERT INTO meta VALUES('snapshot', 'old');
             INSERT INTO meta VALUES('projection_version', 'old');",
        )?;
        drop(old);
        let error = open_path_read_only(&old_database)
            .expect_err("old schema must not migrate during read-only open");
        assert!(error.to_string().contains("schema v14"));
        let error = open_path(&old_database).expect_err("writer must not migrate old schemas");
        assert!(error.to_string().contains("unsupported durable schema v14"));
        let unchanged = Connection::open(&old_database)?.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        assert_eq!(unchanged, "14");
        Ok(())
    }

    #[test]
    fn writer_open_creates_a_missing_database_parent() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let parent = directory.path().join("nested/state");
        let database = parent.join("index.db");

        let error = open_path_read_only(&database)
            .expect_err("read-only open must not create a configured output directory");
        assert!(error.to_string().contains("does not exist"));
        assert!(!parent.exists());

        let conn = open_path(&database)?;
        drop(conn);
        assert!(parent.is_dir());
        assert!(database.is_file());
        Ok(())
    }

    #[test]
    fn v16_durable_floor_preserves_cache_and_memory_while_rebuilding_snapshot_schema() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("floor.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            r#"INSERT INTO files(path, hash, corpus, format, role)
               VALUES('old.ts', 'source', 'code', 'typescript', 'production');
             INSERT INTO chunks(
               file_id, kind, start, end, start_line, end_line, hash, content
             ) VALUES(1, 'module', 0, 1, 1, 1, 'chunk', 'x');
             INSERT INTO embedding_profiles(
               id, provider, model, config_fingerprint, dimensions, config_json
             ) VALUES(1, 'test', 'tiny', 'profile', 2, '{}');
             INSERT INTO embeddings(chunk_hash, profile_id, vec)
             VALUES('chunk', 1, X'0000000000000000');
             INSERT INTO semantic_artifacts(
               id, artifact_type, canonical_name, body_json, model,
               prompt_version, confidence, source_snapshot, created_at,
               input_fingerprint, artifact_fingerprint
             ) VALUES(1, 'annotation', 'memory', '{}', 'agent', 'v1',
                      'likely', 'old', '2026-01-01T00:00:00Z', 'input', 'artifact');
             INSERT INTO scout_runs(
               id, scout_kind, status, gateway_protocol, provider, model,
               billing_path, prompt_version, source_snapshot, input_fingerprint,
               request_hash, started_at, completed_at
             ) VALUES(1, 'repository', 'completed', 1, 'test', 'model', 'custom',
                      'repository-recon/v1', 'old', 'recon-input', 'request',
                      '2026-01-01T00:00:00Z', '2026-01-01T00:00:01Z');
             INSERT INTO repository_classifications(
               id, run_id, subject_key, subject_kind, selector_json, depth,
               role, confidence, explanation, citations_json,
               evidence_fingerprint, classification_fingerprint,
               source_snapshot, created_at
             ) VALUES(1, 1, 'area:repository:src', 'area',
                      '{"kind":"repository_area","scope":"src","direct_only":false}',
                      0, 'runtime', 'likely', 'runtime source', '["E001"]',
                      'evidence', 'classification', 'old', '2026-01-01T00:00:01Z');
             INSERT INTO repository_file_policy(
               file_id, classification_id, subject_key, scope_role,
               effective_role, source_hash, depth
             ) VALUES(1, 1, 'area:repository:src', 'runtime', 'runtime',
                      'source', 0);
             INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES('old', '5.9.3', 'test', 'checker', 1,
                      '2026-01-01T00:00:00Z', 1);
             UPDATE meta SET value='16' WHERE key='schema_version';"#,
        )?;
        crate::embed::materialize_cached_embeddings(&conn)?;
        drop(conn);

        let upgraded = open_path(&database)?;
        let counts: (i64, i64, i64, i64, i64, i64, i64) = upgraded.query_row(
            "SELECT
               (SELECT count(*) FROM embedding_profiles),
               (SELECT count(*) FROM embeddings),
               (SELECT count(*) FROM semantic_artifacts),
               (SELECT count(*) FROM repository_classifications),
               (SELECT count(*) FROM repository_file_policy),
               (SELECT count(*) FROM files),
               (SELECT count(*) FROM checker_enrichment_batches)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        assert_eq!(counts, (1, 1, 1, 1, 0, 0, 0));
        let materialized: (i64, i64) = upgraded.query_row(
            "SELECT
               (SELECT count(*) FROM embedding_index_entries),
               (SELECT count(*) FROM vec_embeddings_2)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(materialized, (0, 0));
        let version: String = upgraded.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn v32_rebuild_discards_phase3_source_state_and_contract_marker() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v32.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            "INSERT INTO files(id,path,hash,corpus,format,role,origin)
               VALUES(1,'README.md','file','docs','markdown','documentation','repository');
             INSERT INTO chunks(
               id,file_id,kind,name,scope_chain,symbols,start,end,
               start_line,end_line,hash,content
             ) VALUES(1,1,'markdown_section',NULL,'','',0,4,1,1,'chunk','body');
             INSERT INTO doc_chunk_meta(
               chunk_id,title,breadcrumb,ordinal,front_matter_state,
               freshness_basis,freshness_author_time
             ) VALUES(1,'README','',0,'absent','git',10);
             INSERT INTO doc_file_provenance(
               file_id,projection_hash,status,detail
             ) VALUES(1,'projection','resolved',NULL);
             INSERT INTO doc_blame_cache(
               path_scope,path,bytes_hash,converted_blob_oid,path_tip,
               shallow_fingerprint,attribution_json,format_version
             ) VALUES(
               'scope','README.md','file','converted','tip','shallow','[]','test-v1'
             );
             INSERT INTO meta(key,value)
               VALUES('documentation_provenance_format_version','test-v1');
             UPDATE meta SET value='32' WHERE key='schema_version';",
        )?;
        drop(conn);

        let upgraded = open_path(&database)?;
        let state: (String, i64, i64, i64, i64) = upgraded.query_row(
            "SELECT
               (SELECT value FROM meta WHERE key='schema_version'),
               (SELECT count(*) FROM meta
                WHERE key='documentation_provenance_format_version'),
               (SELECT count(*) FROM doc_chunk_meta),
               (SELECT count(*) FROM doc_file_provenance),
               (SELECT count(*) FROM doc_blame_cache)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        assert_eq!(state, (SCHEMA_VERSION.into(), 0, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn early_v20_reconnaissance_table_gains_auditable_cited_evidence() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("early-v20.db");
        let conn = open_path(&database)?;
        conn.execute(
            "ALTER TABLE repository_classifications DROP COLUMN cited_evidence_json",
            [],
        )?;
        drop(conn);

        let reopened = open_path(&database)?;
        let columns: i64 = reopened.query_row(
            "SELECT count(*) FROM pragma_table_info('repository_classifications')
             WHERE name='cited_evidence_json'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(columns, 1);
        Ok(())
    }

    #[test]
    fn genuine_v15_embedding_schema_is_rejected_without_mutation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v15.db");
        let legacy = Connection::open(&database)?;
        legacy.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '15');
             CREATE TABLE embeddings(
               chunk_hash TEXT NOT NULL,
               model TEXT NOT NULL,
               dim INTEGER NOT NULL,
               vec BLOB NOT NULL,
               PRIMARY KEY(chunk_hash, model)
             );
             INSERT INTO embeddings VALUES(
               'old', 'ambiguous-model', 2, X'0000000000000000'
             );",
        )?;
        drop(legacy);

        let error = open_path(&database).expect_err("v15 predates durable embedding profiles");
        assert!(error.to_string().contains("unsupported durable schema v15"));

        let unchanged = Connection::open(&database)?;
        let version: String = unchanged.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        let legacy_rows: i64 =
            unchanged.query_row("SELECT count(*) FROM embeddings", [], |row| row.get(0))?;
        let legacy_columns: i64 = unchanged.query_row(
            "SELECT count(*) FROM pragma_table_info('embeddings')
             WHERE name IN ('model', 'dim')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "15");
        assert_eq!(legacy_rows, 1);
        assert_eq!(legacy_columns, 2);
        Ok(())
    }

    #[test]
    fn published_index_opens_query_only_and_rejects_stale_projection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("published.db");
        let writer = open_path(&database)?;
        writer.execute(
            "INSERT INTO meta(key, value) VALUES('snapshot', 'snapshot')",
            [],
        )?;
        writer.execute(
            "INSERT INTO meta(key, value) VALUES('projection_version', ?1)",
            [crate::structural::PROJECTION_VERSION],
        )?;
        writer.execute(
            "INSERT INTO meta(key, value) VALUES('extraction_version', ?1)",
            [crate::entity::EXTRACTION_VERSION],
        )?;
        writer.execute(
            "INSERT INTO meta(key, value)
             VALUES('documentation_chunk_format_version', ?1)",
            [crate::docs::CHUNK_FORMAT_VERSION],
        )?;
        writer.execute(
            "INSERT INTO meta(key, value)
             VALUES('documentation_provenance_format_version', ?1)",
            [crate::docs::PROVENANCE_FORMAT_VERSION],
        )?;
        drop(writer);

        let reader = open_path_read_only(&database)?;
        let snapshot: String =
            reader.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(snapshot, "snapshot");
        assert!(
            reader
                .execute(
                    "INSERT INTO meta(key, value) VALUES('forbidden', 'write')",
                    []
                )
                .is_err(),
            "query-only connection accepted a write"
        );
        drop(reader);

        let writer = Connection::open(&database)?;
        writer.execute(
            "UPDATE meta SET value='stale' WHERE key='projection_version'",
            [],
        )?;
        drop(writer);
        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must reject stale structural projections");
        assert!(error.to_string().contains("structural projection vstale"));
        let unchanged: String = Connection::open(&database)?.query_row(
            "SELECT value FROM meta WHERE key='projection_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(unchanged, "stale");

        let writer = Connection::open(&database)?;
        writer.execute(
            "UPDATE meta SET value=?1 WHERE key='projection_version'",
            [crate::structural::PROJECTION_VERSION],
        )?;
        writer.execute(
            "UPDATE meta SET value='legacy' WHERE key='extraction_version'",
            [],
        )?;
        drop(writer);
        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must reject stale extraction contracts");
        assert!(
            error
                .to_string()
                .contains("code extraction contract vlegacy")
        );

        let writer = Connection::open(&database)?;
        writer.execute(
            "UPDATE meta SET value=?1 WHERE key='extraction_version'",
            [crate::entity::EXTRACTION_VERSION],
        )?;
        writer.execute(
            "UPDATE meta SET value='documentation-v0'
             WHERE key='documentation_chunk_format_version'",
            [],
        )?;
        drop(writer);
        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must reject mismatched documentation contracts");
        assert!(
            error
                .to_string()
                .contains("documentation chunk format documentation-v0")
        );

        let writer = Connection::open(&database)?;
        writer.execute(
            "UPDATE meta SET value=?1
             WHERE key='documentation_chunk_format_version'",
            [crate::docs::CHUNK_FORMAT_VERSION],
        )?;
        writer.execute(
            "UPDATE meta SET value='documentation-provenance-v0'
             WHERE key='documentation_provenance_format_version'",
            [],
        )?;
        drop(writer);
        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must reject stale documentation provenance");
        assert!(
            error
                .to_string()
                .contains("documentation provenance format documentation-provenance-v0")
        );
        Ok(())
    }

    #[test]
    fn v30_rebuild_installs_explicit_file_classification() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v30.db");
        let legacy = Connection::open(&database)?;
        legacy.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema_version', '30');
             CREATE TABLE files(
               id INTEGER PRIMARY KEY,
               path TEXT UNIQUE NOT NULL,
               hash TEXT NOT NULL,
               role TEXT NOT NULL DEFAULT 'unknown',
               origin TEXT NOT NULL DEFAULT 'repository',
               package_instance_id INTEGER,
               package_path TEXT
             );
             INSERT INTO files(path, hash) VALUES('old.ts', 'old');",
        )?;
        drop(legacy);

        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must reject pre-classification schema v30");
        assert!(error.to_string().contains("schema v30"));

        let rebuilt = open_path(&database)?;
        let version: String = rebuilt.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        let columns = relation_columns(&rebuilt, "files")?;
        assert!(columns.iter().any(|column| column == "corpus"));
        assert!(columns.iter().any(|column| column == "format"));
        let files: i64 = rebuilt.query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
        assert_eq!(files, 0);
        Ok(())
    }

    #[test]
    fn v28_fts_mirror_requires_writer_rebuild() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v28.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            "INSERT INTO files(id, path, hash, corpus, format, role)
               VALUES(1, 'old.ts', 'file', 'code', 'typescript', 'production');
             INSERT INTO chunks(
               id, file_id, kind, start, end, start_line, end_line, hash, content
             ) VALUES(1, 1, 'module', 0, 1, 1, 1, 'chunk', 'x');
             INSERT INTO chunks_fts(rowid, content, name, symbols, path)
             VALUES(1, 'x', '', '', 'old.ts');
             UPDATE meta SET value='28' WHERE key='schema_version';",
        )?;
        drop(conn);

        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must not use the pre-sanitization FTS mirror");
        assert!(error.to_string().contains("schema v28"));

        let upgraded = open_path(&database)?;
        let state: (String, i64, i64) = upgraded.query_row(
            "SELECT
               (SELECT value FROM meta WHERE key='schema_version'),
               (SELECT count(*) FROM chunks),
               (SELECT count(*) FROM chunks_fts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(state, (SCHEMA_VERSION.into(), 0, 0));
        Ok(())
    }

    #[test]
    fn v27_upgrade_installs_receiver_flow_tables_and_read_only_rejects_old_schema() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v27.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            "DROP TABLE receiver_value_flows;
             DROP TABLE function_return_flows;
             DROP TABLE value_binding_flows;
             DROP TABLE instance_method_value_flows;
             DROP TABLE class_member_value_flow_blockers;
             DROP TABLE class_value_flows;
             DROP INDEX idx_refs_file_start;
             CREATE INDEX idx_refs_file ON refs(file_id);
             UPDATE meta SET value='27' WHERE key='schema_version';",
        )?;
        drop(conn);

        let error = open_path_read_only(&database)
            .expect_err("read-only consumers must not migrate schema v27");
        assert!(error.to_string().contains("schema v27"));
        let unchanged: String = Connection::open(&database)?.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(unchanged, "27");

        let upgraded = open_path(&database)?;
        let installed: i64 = upgraded.query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE type='table' AND name IN (
               'receiver_value_flows', 'function_return_flows',
               'value_binding_flows', 'instance_method_value_flows',
               'class_member_value_flow_blockers', 'class_value_flows'
             )",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(installed, 6);
        assert_eq!(
            index_columns(&upgraded, "idx_refs_file_start")?,
            ["file_id", "start"]
        );
        let version: String = upgraded.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        Ok(())
    }

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
        assert_eq!(version, SCHEMA_VERSION);
        assert!(database.is_file());
        Ok(())
    }

    #[test]
    fn v24_rebuild_installs_native_graph_lookup_index() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v24.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            "DROP INDEX idx_graph_nodes_native;
             UPDATE meta SET value='24' WHERE key='schema_version';",
        )?;
        drop(conn);

        let migrated = open_path(&database)?;
        let version: String = migrated.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(
            index_columns(&migrated, "idx_graph_nodes_native")?,
            ["native_id", "native_table"]
        );
        Ok(())
    }

    #[test]
    fn v25_rebuild_replaces_member_call_file_index() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("v25.db");
        let conn = open_path(&database)?;
        conn.execute_batch(
            "DROP INDEX idx_member_calls_file;
             CREATE INDEX idx_member_calls_file ON member_calls(file_id);
             UPDATE meta SET value='25' WHERE key='schema_version';",
        )?;
        drop(conn);

        let migrated = open_path(&database)?;
        let version: String = migrated.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(
            index_columns(&migrated, "idx_member_calls_file")?,
            ["file_id", "receiver_start", "prop"]
        );
        Ok(())
    }

    #[test]
    fn indexes_high_volume_evidence_tables_by_file() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        assert_eq!(index_columns(&conn, "idx_events_file")?, ["file_id"]);
        assert_eq!(
            index_columns(&conn, "idx_member_calls_file")?,
            ["file_id", "receiver_start", "prop"]
        );
        Ok(())
    }

    #[test]
    fn code_corpus_views_are_installed_with_the_shared_table_shapes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        assert_eq!(index_columns(&conn, "idx_files_corpus")?, ["corpus"]);
        let views = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type='view' AND name IN ('code_files','code_chunks')
                 ORDER BY name",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(views, ["code_chunks", "code_files"]);
        assert_eq!(
            relation_columns(&conn, "code_files")?,
            relation_columns(&conn, "files")?
        );
        assert_eq!(
            relation_columns(&conn, "code_chunks")?,
            relation_columns(&conn, "chunks")?
        );
        Ok(())
    }

    #[test]
    fn files_require_a_valid_corpus_and_nonempty_format() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;

        conn.execute(
            "INSERT INTO files(path,hash,corpus,format)
             VALUES('valid.ts','valid','code','typescript')",
            [],
        )?;
        conn.execute(
            "INSERT INTO files(path,hash,corpus,format)
             VALUES('valid.md','valid','docs','markdown')",
            [],
        )?;
        conn.execute(
            "INSERT INTO files(path,hash) VALUES('missing.ts','missing')",
            [],
        )
        .expect_err("corpus and format must be explicit");
        conn.execute(
            "INSERT INTO files(path,hash,corpus,format)
             VALUES('invalid.txt','invalid','other','text')",
            [],
        )
        .expect_err("unknown corpus must be rejected");
        conn.execute(
            "INSERT INTO files(path,hash,corpus,format)
             VALUES('empty.ts','empty','code','   ')",
            [],
        )
        .expect_err("empty format must be rejected");
        Ok(())
    }

    #[test]
    fn deleting_a_file_is_idempotent_after_its_rows_are_gone() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        conn.execute_batch(
            "INSERT INTO files(id,path,hash,corpus,format,role,origin)
               VALUES(1,'README.md','doc-file','docs','markdown','documentation','repository');
             INSERT INTO chunks(
               id,file_id,kind,name,scope_chain,symbols,start,end,
               start_line,end_line,hash,content
             ) VALUES(1,1,'markdown_section',NULL,'','',0,4,1,1,'doc-chunk','body');
             INSERT INTO doc_chunk_meta(
               chunk_id,title,breadcrumb,nearest_heading,ordinal,
               embedding_identity,front_matter_state
             ) VALUES(1,'README','',NULL,0,NULL,'absent');
             INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
               VALUES(1,'README','','','body','README.md');",
        )?;

        delete_file(&conn, 1)?;
        delete_file(&conn, 1)?;

        let remaining: (i64, i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM files),
               (SELECT count(*) FROM chunks),
               (SELECT count(*) FROM doc_chunk_meta),
               (SELECT count(*) FROM docs_fts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(remaining, (0, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn code_corpus_views_exclude_entire_documentation_files() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        conn.execute_batch(
            "INSERT INTO files(id,path,hash,corpus,format,role,origin) VALUES
               (1,'src/main.ts','code-file','code','typescript','production','repository'),
               (2,'README.md','doc-file','docs','markdown','documentation','repository'),
               (3,'empty.md','empty-doc','docs','markdown','documentation','repository');
             INSERT INTO chunks(
               id,file_id,kind,name,scope_chain,symbols,start,end,
               start_line,end_line,hash,content
             ) VALUES
               (1,1,'function','run','','run',0,3,1,1,'code-chunk','run'),
               (2,2,'markdown_section',NULL,'','',0,3,1,1,'doc-chunk','doc'),
               (3,1,'function','more','','more',4,8,2,2,'more-code','more');
             INSERT INTO doc_chunk_meta(
               chunk_id,title,breadcrumb,nearest_heading,ordinal,
               embedding_identity,front_matter_state
             ) VALUES
               (2,'README','',NULL,0,NULL,'absent');",
        )?;

        let code_file_ids = conn
            .prepare("SELECT id FROM code_files ORDER BY id")?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let code_chunk_ids = conn
            .prepare("SELECT id FROM code_chunks ORDER BY id")?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(code_file_ids, [1]);
        assert_eq!(code_chunk_ids, [1, 3]);
        let sidecar_error = conn
            .execute(
                "INSERT INTO doc_chunk_meta(
                   chunk_id,title,breadcrumb,ordinal,front_matter_state
                 ) VALUES(1,'Code','',0,'absent')",
                [],
            )
            .expect_err("documentation metadata must not define corpus membership");
        assert!(
            sidecar_error
                .to_string()
                .contains("doc_chunk_meta requires a docs-corpus file")
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))?,
            3
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM chunks", [], |row| row
                .get::<_, i64>(0))?,
            3
        );

        reset_extraction_state(&conn)?;
        let visible_after_reset: i64 = conn.query_row(
            "SELECT (SELECT count(*) FROM code_files)
                  + (SELECT count(*) FROM code_chunks)",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(visible_after_reset, 0);
        Ok(())
    }

    #[test]
    fn documentation_projection_schema_is_disposable_but_cache_survives_reset() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        let meta_columns = conn
            .prepare("SELECT name FROM pragma_table_info('doc_chunk_meta') ORDER BY cid")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(
            meta_columns,
            [
                "chunk_id",
                "title",
                "description",
                "tags_json",
                "breadcrumb",
                "nearest_heading",
                "ordinal",
                "embedding_identity",
                "front_matter_state",
                "freshness_basis",
                "freshness_author_time",
                "freshness_committer_time",
                "freshness_detail",
            ]
        );
        assert_eq!(
            relation_columns(&conn, "doc_file_provenance")?,
            ["file_id", "projection_hash", "status", "detail"]
        );
        assert_eq!(
            relation_columns(&conn, "doc_blame_cache")?,
            [
                "path_scope",
                "path",
                "bytes_hash",
                "converted_blob_oid",
                "path_tip",
                "shallow_fingerprint",
                "attribution_json",
                "format_version",
            ]
        );
        let fts_columns = conn
            .prepare("SELECT name FROM pragma_table_info('docs_fts') ORDER BY cid")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(
            fts_columns,
            ["title", "metadata", "breadcrumb", "body", "path"]
        );

        conn.execute_batch(
            "INSERT INTO embedding_profiles(
               id,provider,model,config_fingerprint,dimensions,config_json
             ) VALUES(1,'test','tiny','profile',2,'{}');
             INSERT INTO embeddings(chunk_hash,profile_id,vec)
               VALUES('doc-identity',1,x'0000000000000000');
             INSERT INTO files(id,path,hash,corpus,format,role,origin)
               VALUES(1,'README.md','file','docs','markdown','documentation','repository');
             INSERT INTO chunks(
               id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(1,1,'markdown_document',NULL,'','',0,0,1,1,'source','');
             INSERT INTO doc_chunk_meta(
               chunk_id,title,breadcrumb,nearest_heading,ordinal,
               embedding_identity,front_matter_state
             ) VALUES(1,'README','',NULL,0,'doc-identity','absent');
             INSERT INTO doc_file_provenance(
               file_id,projection_hash,status,detail
             ) VALUES(1,'projection','resolved',NULL);
             INSERT INTO doc_blame_cache(
               path_scope,path,bytes_hash,converted_blob_oid,path_tip,
               shallow_fingerprint,attribution_json,format_version
             ) VALUES(
               'scope','README.md','file','converted','tip','shallow','[]',
               'test-contract'
             );
             INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
               VALUES(1,'README','','','','README.md');
             INSERT INTO doc_inventory(path,subject,rule)
               VALUES('README.md','file','indexed');
             INSERT INTO doc_embedding_index_entries(id,chunk_id,profile_id)
               VALUES(1,1,1);
             INSERT INTO doc_vector_generations(
               snapshot,profile_id,dimensions,chunk_format_version
             ) VALUES('snapshot',1,2,'documentation-v1');
             CREATE VIRTUAL TABLE vec_doc_embeddings_2 USING vec0(
               embedding FLOAT[2] distance_metric=cosine,
               profile_id INTEGER PARTITION KEY,
               snapshot TEXT PARTITION KEY
             );
             INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id,snapshot)
               VALUES(1,x'0000000000000000',1,'snapshot');",
        )?;

        reset_extraction_state(&conn)?;
        let state: (i64, i64, i64, i64, i64, i64, i64, i64) = conn.query_row(
            "SELECT
               (SELECT count(*) FROM files),
               (SELECT count(*) FROM docs_fts),
               (SELECT count(*) FROM doc_chunk_meta),
               (SELECT count(*) FROM doc_file_provenance),
               (SELECT count(*) FROM doc_inventory),
               (SELECT count(*) FROM vec_doc_embeddings_2),
               (SELECT count(*) FROM embeddings),
               (SELECT count(*) FROM doc_blame_cache)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;
        assert_eq!(state, (0, 0, 0, 0, 0, 0, 1, 1));
        Ok(())
    }

    #[test]
    fn resolves_repository_and_dependency_file_source_paths() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let dependency = tempfile::tempdir()?;
        let conn = open(repo.path())?;
        conn.execute(
            "INSERT INTO files(path, hash, corpus, format, role)
             VALUES('src/main.ts', 'a', 'code', 'typescript', 'production')",
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
               path, hash, corpus, format, role, origin,
               package_instance_id, package_path
             ) VALUES('dependency:left-pad@1.3.0/index.js', 'b',
                      'code', 'javascript', 'production', 'dependency', ?1, 'index.js')",
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
