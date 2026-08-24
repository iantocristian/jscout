use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::CHUNK_FORMAT_VERSION;
use super::corpus::{Corpus, Decision, DocBlock, DocFile};

pub const APPLICATION_ID: i64 = 0x4A53_444F;
pub const SCHEMA_VERSION: i64 = 1;

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

const SCHEMA: &str = r#"
CREATE TABLE doc_store_meta(
  id INTEGER PRIMARY KEY CHECK(id = 1),
  schema_version INTEGER NOT NULL,
  canonical_root BLOB NOT NULL,
  canonical_root_display TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE doc_snapshots(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  observed_at INTEGER NOT NULL,
  corpus_fingerprint BLOB NOT NULL,
  chunk_format TEXT NOT NULL,
  inventory_count INTEGER NOT NULL,
  indexed_file_count INTEGER NOT NULL,
  rejection_count INTEGER NOT NULL
);

CREATE TABLE doc_current_snapshot(
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id)
);

CREATE TABLE doc_inventory(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  path TEXT NOT NULL,
  subject TEXT NOT NULL,
  rule TEXT NOT NULL,
  detail TEXT,
  path_base64 TEXT,
  path_encoding TEXT,
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id)
);

CREATE TABLE doc_files(
  path TEXT PRIMARY KEY,
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id),
  file_hash BLOB NOT NULL,
  byte_len INTEGER NOT NULL,
  line_count INTEGER NOT NULL,
  title TEXT NOT NULL,
  description TEXT,
  tags_json TEXT NOT NULL,
  front_matter_state TEXT NOT NULL
);

CREATE TABLE doc_block_contents(
  content_hash BLOB PRIMARY KEY,
  body TEXT NOT NULL
);

CREATE TABLE doc_block_observations(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  logical_id BLOB NOT NULL,
  predecessor_observation_id INTEGER REFERENCES doc_block_observations(id),
  lifecycle TEXT NOT NULL CHECK(lifecycle IN ('baseline', 'added', 'continued', 'removed')),
  body_changed INTEGER NOT NULL CHECK(body_changed IN (0, 1)),
  context_changed INTEGER NOT NULL CHECK(context_changed IN (0, 1)),
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id),
  path TEXT NOT NULL,
  content_hash BLOB NOT NULL,
  match_confidence TEXT NOT NULL CHECK(match_confidence IN ('none', 'exact', 'neighbor-anchored'))
);
CREATE INDEX doc_block_observations_logical
  ON doc_block_observations(logical_id, id);
CREATE INDEX doc_block_observations_snapshot
  ON doc_block_observations(snapshot_id, lifecycle);

CREATE TABLE doc_block_occurrences(
  logical_id BLOB PRIMARY KEY,
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id),
  current_observation_id INTEGER NOT NULL REFERENCES doc_block_observations(id),
  path TEXT NOT NULL REFERENCES doc_files(path) ON DELETE CASCADE,
  block_order INTEGER NOT NULL,
  source_start INTEGER NOT NULL,
  source_end INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  content_hash BLOB NOT NULL REFERENCES doc_block_contents(content_hash),
  block_kind TEXT NOT NULL,
  breadcrumb TEXT NOT NULL,
  nearest_heading TEXT,
  freshness_basis TEXT NOT NULL CHECK(freshness_basis IN ('observed', 'unknown')),
  freshness_sequence INTEGER,
  UNIQUE(path, block_order)
);
CREATE INDEX doc_block_occurrences_path_order
  ON doc_block_occurrences(path, block_order);

CREATE TABLE doc_path_states(
  path TEXT PRIMARY KEY,
  ever_seen INTEGER NOT NULL CHECK(ever_seen IN (0, 1)),
  continuity_broken INTEGER NOT NULL CHECK(continuity_broken IN (0, 1)),
  last_rule TEXT NOT NULL,
  updated_snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id)
);

CREATE TABLE doc_chunks(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id),
  path TEXT NOT NULL REFERENCES doc_files(path) ON DELETE CASCADE,
  chunk_order INTEGER NOT NULL,
  source_start INTEGER NOT NULL,
  source_end INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  breadcrumb TEXT NOT NULL,
  nearest_heading TEXT,
  rendered_body TEXT NOT NULL,
  provider_text TEXT,
  rendered_body_hash BLOB NOT NULL,
  embedding_identity BLOB,
  stub INTEGER NOT NULL CHECK(stub IN (0, 1)),
  freshness_basis TEXT NOT NULL CHECK(freshness_basis IN ('observed', 'unknown')),
  freshness_sequence INTEGER,
  UNIQUE(path, chunk_order)
);
CREATE INDEX doc_chunks_snapshot ON doc_chunks(snapshot_id);
CREATE INDEX doc_chunks_embedding_identity ON doc_chunks(embedding_identity);

CREATE TABLE doc_chunk_blocks(
  chunk_id INTEGER NOT NULL REFERENCES doc_chunks(id) ON DELETE CASCADE,
  block_position INTEGER NOT NULL,
  logical_id BLOB NOT NULL REFERENCES doc_block_occurrences(logical_id),
  PRIMARY KEY(chunk_id, block_position),
  UNIQUE(chunk_id, logical_id)
);

CREATE VIRTUAL TABLE doc_chunks_fts USING fts5(
  title,
  metadata,
  breadcrumb,
  body,
  path,
  tokenize="unicode61 tokenchars '_$'"
);

CREATE TABLE doc_embedding_profiles(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  config_fingerprint BLOB NOT NULL UNIQUE,
  dimensions INTEGER NOT NULL,
  config_json TEXT NOT NULL
);

CREATE TABLE doc_embeddings(
  embedding_identity BLOB NOT NULL,
  profile_id INTEGER NOT NULL REFERENCES doc_embedding_profiles(id),
  vec BLOB NOT NULL,
  PRIMARY KEY(embedding_identity, profile_id)
);

CREATE TABLE doc_embedding_index_entries(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chunk_id INTEGER NOT NULL REFERENCES doc_chunks(id) ON DELETE CASCADE,
  embedding_identity BLOB NOT NULL,
  profile_id INTEGER NOT NULL REFERENCES doc_embedding_profiles(id),
  UNIQUE(chunk_id, profile_id)
);
CREATE INDEX doc_embedding_index_entries_profile
  ON doc_embedding_index_entries(profile_id, chunk_id);

CREATE TABLE doc_vector_generations(
  snapshot_id INTEGER NOT NULL REFERENCES doc_snapshots(id),
  profile_id INTEGER NOT NULL REFERENCES doc_embedding_profiles(id),
  dimensions INTEGER NOT NULL,
  chunk_format TEXT NOT NULL,
  ready_at INTEGER NOT NULL,
  PRIMARY KEY(snapshot_id, profile_id, dimensions, chunk_format)
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Publication {
    pub snapshot_id: i64,
    pub observed_at: i64,
    pub corpus_fingerprint: String,
    pub inventory_count: usize,
    pub indexed_file_count: usize,
    pub rejection_count: usize,
    pub block_count: usize,
    pub chunk_count: usize,
    pub observations_added: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub database_path: String,
    pub canonical_root: String,
    pub snapshot_id: Option<i64>,
    pub observed_at: Option<i64>,
    pub corpus_fingerprint: Option<String>,
    pub inventory_count: usize,
    pub indexed_file_count: usize,
    pub rejection_count: usize,
    pub block_count: usize,
    pub chunk_count: usize,
    pub embeddable_chunk_count: usize,
    pub cached_embedding_count: usize,
    pub ready_vector_generation_count: usize,
    pub observation_count: usize,
    pub front_matter: Vec<FrontMatterStatus>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrontMatterStatus {
    pub path: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub chunk_id: i64,
    pub snapshot_id: i64,
    pub path: String,
    pub title: String,
    pub breadcrumb: String,
    pub nearest_heading: Option<String>,
    pub rendered_body: String,
    pub source_start: u64,
    pub source_end: u64,
    pub start_line: u32,
    pub end_line: u32,
    pub file_hash: String,
    #[serde(skip_serializing, default)]
    pub file_byte_len: u64,
    #[serde(skip_serializing, default)]
    pub embedding_identity: Vec<u8>,
    pub score: f64,
    pub stub: bool,
    pub freshness_basis: String,
    pub freshness_sequence: Option<i64>,
    pub freshness_observed_at: Option<i64>,
}

#[derive(Debug)]
pub struct DocsStore {
    conn: Connection,
    database_path: PathBuf,
    canonical_root: PathBuf,
    canonical_root_key: Vec<u8>,
}

impl DocsStore {
    pub fn open(
        root: &Path,
        configured_path: Option<&Path>,
        main_database_path: Option<&Path>,
    ) -> Result<Self> {
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("canonicalize documentation root {}", root.display()))?;
        ensure!(
            canonical_root.is_dir(),
            "documentation root is not a directory: {}",
            canonical_root.display()
        );

        let database_path = resolve_database_path(&canonical_root, configured_path)?;
        let main_path = main_database_path.unwrap_or_else(|| Path::new(crate::store::DB_FILE));
        reject_database_alias(&canonical_root, &database_path, main_path)?;

        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "create documentation database directory {}",
                    parent.display()
                )
            })?;
        }

        register_sqlite_vec();
        let conn = Connection::open(&database_path)
            .with_context(|| format!("open documentation database {}", database_path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let canonical_root_key = platform_path_key(&canonical_root);
        initialize_or_validate(&conn, &canonical_root, &canonical_root_key, &database_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        Ok(Self {
            conn,
            database_path,
            canonical_root,
            canonical_root_key,
        })
    }

    pub fn open_read_only(
        root: &Path,
        configured_path: Option<&Path>,
        main_database_path: Option<&Path>,
    ) -> Result<Self> {
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("canonicalize documentation root {}", root.display()))?;
        ensure!(
            canonical_root.is_dir(),
            "documentation root is not a directory: {}",
            canonical_root.display()
        );
        let database_path = resolve_database_path(&canonical_root, configured_path)?;
        let main_path = main_database_path.unwrap_or_else(|| Path::new(crate::store::DB_FILE));
        reject_database_alias(&canonical_root, &database_path, main_path)?;
        ensure!(
            database_path.is_file(),
            "documentation database `{}` does not exist; run `jscout docs index {}` first",
            database_path.display(),
            canonical_root.display()
        );

        register_sqlite_vec();
        let conn =
            Connection::open_with_flags(&database_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| {
                    format!(
                        "open documentation database {} read-only",
                        database_path.display()
                    )
                })?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let canonical_root_key = platform_path_key(&canonical_root);
        initialize_or_validate(&conn, &canonical_root, &canonical_root_key, &database_path)?;
        conn.pragma_update(None, "query_only", "ON")?;
        let store = Self {
            conn,
            database_path,
            canonical_root,
            canonical_root_key,
        };
        ensure!(
            store.current_snapshot_id()?.is_some(),
            "documentation database `{}` has no published snapshot; run `jscout docs index {}` first",
            store.database_path.display(),
            store.canonical_root.display()
        );
        Ok(store)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub fn current_snapshot_id(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT snapshot_id FROM doc_current_snapshot WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("read current documentation snapshot")
    }

    pub fn publish(&mut self, corpus: &Corpus) -> Result<Publication> {
        self.publish_inner(corpus, false)
    }

    fn publish_inner(&mut self, corpus: &Corpus, fail_before_commit: bool) -> Result<Publication> {
        ensure!(
            platform_path_key(&corpus.canonical_root) == self.canonical_root_key,
            "captured documentation corpus belongs to a different canonical root"
        );
        let expected_fingerprint = compute_corpus_fingerprint(&corpus.files)?;
        ensure!(
            corpus
                .fingerprint
                .eq_ignore_ascii_case(&expected_fingerprint),
            "documentation corpus fingerprint does not match its indexed files"
        );

        let observed_at = unix_timestamp()?;
        let inventory_count = corpus.decisions.len();
        let indexed_file_count = corpus.files.len();
        let rejection_count = corpus
            .decisions
            .iter()
            .filter(|decision| decision.rule != "indexed")
            .count();
        let block_count = corpus.files.iter().map(|file| file.blocks.len()).sum();
        let chunk_count = corpus.files.iter().map(|file| file.chunks.len()).sum();
        let fingerprint = decode_hex_32(&corpus.fingerprint)
            .context("decode documentation corpus fingerprint")?;

        let tx = self
            .conn
            .transaction()
            .context("begin documentation publication")?;
        tx.execute(
            "INSERT INTO doc_snapshots(
               observed_at, corpus_fingerprint, chunk_format,
               inventory_count, indexed_file_count, rejection_count
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                observed_at,
                fingerprint,
                CHUNK_FORMAT_VERSION,
                sql_i64(inventory_count)?,
                sql_i64(indexed_file_count)?,
                sql_i64(rejection_count)?
            ],
        )?;
        let snapshot_id = tx.last_insert_rowid();

        let history = apply_history(&tx, snapshot_id, corpus)?;
        replace_current_projection(&tx, snapshot_id, corpus, &history.current)?;
        update_path_states(&tx, snapshot_id, corpus, &history.previous_paths)?;

        tx.execute(
            "INSERT INTO doc_current_snapshot(singleton, snapshot_id) VALUES(1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET snapshot_id=excluded.snapshot_id",
            [snapshot_id],
        )?;
        tx.execute(
            "DELETE FROM doc_block_contents
             WHERE NOT EXISTS(
               SELECT 1 FROM doc_block_occurrences o
               WHERE o.content_hash=doc_block_contents.content_hash
             )",
            [],
        )?;

        if fail_before_commit {
            bail!("injected documentation publication failure before commit");
        }
        tx.commit().context("commit documentation publication")?;

        Ok(Publication {
            snapshot_id,
            observed_at,
            corpus_fingerprint: expected_fingerprint,
            inventory_count,
            indexed_file_count,
            rejection_count,
            block_count,
            chunk_count,
            observations_added: history.observations_added,
        })
    }

    pub fn status(&self) -> Result<Status> {
        // Pin every status statement to one WAL snapshot. A publisher may
        // atomically replace the current projection on another connection.
        let _read_snapshot = self
            .conn
            .unchecked_transaction()
            .context("begin documentation status read snapshot")?;
        let snapshot = self
            .conn
            .query_row(
                "SELECT s.id, s.observed_at, s.corpus_fingerprint,
                        s.inventory_count, s.indexed_file_count, s.rejection_count
                 FROM doc_current_snapshot c
                 JOIN doc_snapshots s ON s.id=c.snapshot_id
                 WHERE c.singleton=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;

        let (
            snapshot_id,
            observed_at,
            corpus_fingerprint,
            inventory_count,
            file_count,
            rejection_count,
        ) = if let Some((id, observed, fingerprint, inventory, files, rejections)) = snapshot {
            (
                Some(id),
                Some(observed),
                Some(encode_hex(&fingerprint)),
                usize_from_sql(inventory)?,
                usize_from_sql(files)?,
                usize_from_sql(rejections)?,
            )
        } else {
            (None, None, None, 0, 0, 0)
        };

        let block_count = count(&self.conn, "doc_block_occurrences")?;
        let chunk_count = count(&self.conn, "doc_chunks")?;
        let embeddable_chunk_count: usize = usize_from_sql(self.conn.query_row(
            "SELECT count(*) FROM doc_chunks WHERE embedding_identity IS NOT NULL AND stub=0",
            [],
            |row| row.get(0),
        )?)?;
        let cached_embedding_count = count(&self.conn, "doc_embeddings")?;
        let ready_vector_generation_count = if let Some(snapshot_id) = snapshot_id {
            usize_from_sql(self.conn.query_row(
                "SELECT count(*) FROM doc_vector_generations WHERE snapshot_id=?1",
                [snapshot_id],
                |row| row.get(0),
            )?)?
        } else {
            0
        };
        let observation_count = count(&self.conn, "doc_block_observations")?;
        let front_matter = self.front_matter_status()?;
        let decisions = self.decisions()?;

        Ok(Status {
            database_path: self.database_path.to_string_lossy().into_owned(),
            canonical_root: self.canonical_root.to_string_lossy().into_owned(),
            snapshot_id,
            observed_at,
            corpus_fingerprint,
            inventory_count,
            indexed_file_count: file_count,
            rejection_count,
            block_count,
            chunk_count,
            embeddable_chunk_count,
            cached_embedding_count,
            ready_vector_generation_count,
            observation_count,
            front_matter,
            decisions,
        })
    }

    fn front_matter_status(&self) -> Result<Vec<FrontMatterStatus>> {
        let mut statement = self.conn.prepare(
            "SELECT path, front_matter_state FROM doc_files
             ORDER BY path COLLATE BINARY",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(FrontMatterStatus {
                path: row.get(0)?,
                state: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("read documentation front-matter status")
    }

    pub fn decisions(&self) -> Result<Vec<Decision>> {
        let mut statement = self.conn.prepare(
            "SELECT path, subject, rule, detail, path_base64, path_encoding
             FROM doc_inventory
             ORDER BY path COLLATE BINARY,
                      COALESCE(path_base64, '') COLLATE BINARY,
                      subject COLLATE BINARY",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Decision {
                path: row.get(0)?,
                subject: row.get(1)?,
                rule: row.get(2)?,
                detail: row.get(3)?,
                path_base64: row.get(4)?,
                path_encoding: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("read documentation inventory")
    }

    pub fn lexical_search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let limit = sql_i64(limit)?;
        let mut statement = self.conn.prepare(
            "SELECT c.id, c.snapshot_id, c.path, f.title, c.breadcrumb,
                    c.nearest_heading, c.rendered_body, c.source_start, c.source_end,
                    c.start_line, c.end_line, f.file_hash, f.byte_len,
                    COALESCE(c.embedding_identity, X''),
                    -bm25(doc_chunks_fts, 4.0, 2.0, 2.0, 1.0, 0.25) AS score,
                    c.stub, c.freshness_basis, c.freshness_sequence,
                    freshness_snapshot.observed_at
             FROM doc_chunks_fts
             JOIN doc_chunks c ON c.id=doc_chunks_fts.rowid
             JOIN doc_files f ON f.path=c.path
             JOIN doc_current_snapshot current ON current.snapshot_id=c.snapshot_id
             LEFT JOIN doc_snapshots freshness_snapshot
               ON freshness_snapshot.id=c.freshness_sequence
             WHERE doc_chunks_fts MATCH ?1
             ORDER BY score DESC,
                      c.path COLLATE BINARY ASC,
                      c.source_start ASC,
                      c.source_end ASC,
                      c.rendered_body_hash ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![query, limit], |row| {
            Ok(SearchHit {
                chunk_id: row.get(0)?,
                snapshot_id: row.get(1)?,
                path: row.get(2)?,
                title: row.get(3)?,
                breadcrumb: row.get(4)?,
                nearest_heading: row.get(5)?,
                rendered_body: row.get(6)?,
                source_start: row_u64(row, 7)?,
                source_end: row_u64(row, 8)?,
                start_line: row_u32(row, 9)?,
                end_line: row_u32(row, 10)?,
                file_hash: encode_hex(&row.get::<_, Vec<u8>>(11)?),
                file_byte_len: row_u64(row, 12)?,
                embedding_identity: row.get(13)?,
                score: row.get(14)?,
                stub: row.get::<_, i64>(15)? != 0,
                freshness_basis: row.get(16)?,
                freshness_sequence: row.get(17)?,
                freshness_observed_at: row.get(18)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("search documentation for `{query}`"))
    }
}

#[derive(Debug)]
struct OldOccurrence {
    logical_id: Vec<u8>,
    observation_id: i64,
    path: String,
    content_hash: Vec<u8>,
    block_kind: String,
    breadcrumb: String,
    nearest_heading: Option<String>,
    freshness_basis: String,
    freshness_sequence: Option<i64>,
}

#[derive(Debug, Clone)]
struct CurrentOccurrence {
    logical_id: Vec<u8>,
    observation_id: i64,
    freshness_basis: String,
    freshness_sequence: Option<i64>,
}

struct HistoryResult {
    current: BTreeMap<(String, u64), CurrentOccurrence>,
    previous_paths: BTreeSet<String>,
    observations_added: usize,
}

fn apply_history(tx: &Transaction<'_>, snapshot_id: i64, corpus: &Corpus) -> Result<HistoryResult> {
    let old = load_old_occurrences(tx)?;
    let previous_paths = old.iter().map(|block| block.path.clone()).collect();
    let path_states = load_path_states(tx)?;
    let has_previous_snapshot: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM doc_current_snapshot WHERE singleton=1)",
        [],
        |row| row.get(0),
    )?;
    let read_gaps: BTreeSet<&str> = corpus
        .decisions
        .iter()
        .filter(|decision| decision.subject == "file" && decision.rule == "read-error")
        .map(|decision| decision.path.as_str())
        .collect();

    let mut old_by_path: BTreeMap<&str, Vec<&OldOccurrence>> = BTreeMap::new();
    for occurrence in &old {
        old_by_path
            .entry(&occurrence.path)
            .or_default()
            .push(occurrence);
    }
    let mut new_by_path: BTreeMap<&str, Vec<&DocBlock>> = BTreeMap::new();
    for file in &corpus.files {
        let blocks = new_by_path.entry(&file.path).or_default();
        let mut ordered: Vec<_> = file.blocks.iter().collect();
        ordered.sort_by_key(|block| block.ordinal);
        ensure_unique_ordinals(&file.path, &ordered)?;
        blocks.extend(ordered);
    }

    let mut current = BTreeMap::new();
    let mut observations_added = 0;
    let all_paths: BTreeSet<_> = old_by_path
        .keys()
        .copied()
        .chain(new_by_path.keys().copied())
        .collect();

    for path in all_paths {
        let old_blocks = old_by_path.get(path).cloned().unwrap_or_default();
        let new_blocks = new_by_path.get(path).cloned().unwrap_or_default();
        let (new_to_old, confidence) = match_blocks(&old_blocks, &new_blocks);
        let mut old_matched = vec![false; old_blocks.len()];

        for (new_index, block) in new_blocks.iter().enumerate() {
            let content_hash = decode_hex_32(&block.content_hash)
                .with_context(|| format!("decode block hash for {path}:{}", block.ordinal))?;
            if let Some(old_index) = new_to_old[new_index] {
                old_matched[old_index] = true;
                let predecessor = old_blocks[old_index];
                let context_changed = predecessor.breadcrumb != block.breadcrumb
                    || predecessor.nearest_heading != block.nearest_heading
                    || predecessor.block_kind != block.kind;
                let body_changed = predecessor.content_hash != content_hash;
                let (observation_id, freshness_basis, freshness_sequence) =
                    if body_changed || context_changed {
                        let observation_id = insert_observation(
                            tx,
                            snapshot_id,
                            &predecessor.logical_id,
                            Some(predecessor.observation_id),
                            "continued",
                            body_changed,
                            context_changed,
                            path,
                            &content_hash,
                            confidence[new_index].unwrap_or("neighbor-anchored"),
                        )?;
                        observations_added += 1;
                        if body_changed {
                            (observation_id, "observed".to_owned(), Some(snapshot_id))
                        } else {
                            (
                                observation_id,
                                predecessor.freshness_basis.clone(),
                                predecessor.freshness_sequence,
                            )
                        }
                    } else {
                        (
                            predecessor.observation_id,
                            predecessor.freshness_basis.clone(),
                            predecessor.freshness_sequence,
                        )
                    };
                current.insert(
                    (path.to_owned(), block.ordinal),
                    CurrentOccurrence {
                        logical_id: predecessor.logical_id.clone(),
                        observation_id,
                        freshness_basis,
                        freshness_sequence,
                    },
                );
            } else {
                let state = path_states.get(path);
                let baseline = !has_previous_snapshot
                    || state.is_some_and(|(_, continuity_broken)| *continuity_broken);
                let lifecycle = if baseline { "baseline" } else { "added" };
                let logical_id = occurrence_id(snapshot_id, path, block.ordinal, &content_hash);
                let observation_id = insert_observation(
                    tx,
                    snapshot_id,
                    &logical_id,
                    None,
                    lifecycle,
                    false,
                    false,
                    path,
                    &content_hash,
                    "none",
                )?;
                observations_added += 1;
                current.insert(
                    (path.to_owned(), block.ordinal),
                    CurrentOccurrence {
                        logical_id,
                        observation_id,
                        freshness_basis: if baseline { "unknown" } else { "observed" }.to_owned(),
                        freshness_sequence: (!baseline).then_some(snapshot_id),
                    },
                );
            }
        }

        if !read_gaps.contains(path) {
            for (old_index, occurrence) in old_blocks.iter().enumerate() {
                if !old_matched[old_index] {
                    insert_observation(
                        tx,
                        snapshot_id,
                        &occurrence.logical_id,
                        Some(occurrence.observation_id),
                        "removed",
                        false,
                        false,
                        path,
                        &occurrence.content_hash,
                        "none",
                    )?;
                    observations_added += 1;
                }
            }
        }
    }

    Ok(HistoryResult {
        current,
        previous_paths,
        observations_added,
    })
}

fn load_old_occurrences(tx: &Transaction<'_>) -> Result<Vec<OldOccurrence>> {
    let mut statement = tx.prepare(
        "SELECT logical_id, current_observation_id, path, block_order, content_hash,
                block_kind, breadcrumb, nearest_heading, freshness_basis,
                freshness_sequence
         FROM doc_block_occurrences
         ORDER BY path COLLATE BINARY, block_order",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(OldOccurrence {
            logical_id: row.get(0)?,
            observation_id: row.get(1)?,
            path: row.get(2)?,
            content_hash: row.get(4)?,
            block_kind: row.get(5)?,
            breadcrumb: row.get(6)?,
            nearest_heading: row.get(7)?,
            freshness_basis: row.get(8)?,
            freshness_sequence: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("load previous documentation block projection")
}

fn load_path_states(tx: &Transaction<'_>) -> Result<HashMap<String, (bool, bool)>> {
    let mut statement =
        tx.prepare("SELECT path, ever_seen, continuity_broken FROM doc_path_states")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (row.get::<_, i64>(1)? != 0, row.get::<_, i64>(2)? != 0),
        ))
    })?;
    rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("load documentation path continuity states")
}

fn ensure_unique_ordinals(path: &str, blocks: &[&DocBlock]) -> Result<()> {
    for pair in blocks.windows(2) {
        ensure!(
            pair[0].ordinal != pair[1].ordinal,
            "duplicate block ordinal {} in {path}",
            pair[0].ordinal
        );
    }
    Ok(())
}

fn match_blocks(
    old: &[&OldOccurrence],
    new: &[&DocBlock],
) -> (Vec<Option<usize>>, Vec<Option<&'static str>>) {
    let mut new_to_old = vec![None; new.len()];
    let mut old_to_new = vec![None; old.len()];
    let mut confidence = vec![None; new.len()];

    let mut old_hashes: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (index, block) in old.iter().enumerate() {
        old_hashes
            .entry(&block.content_hash)
            .or_default()
            .push(index);
    }
    let decoded_new: Vec<Option<Vec<u8>>> = new
        .iter()
        .map(|block| decode_hex_32(&block.content_hash).ok())
        .collect();
    let mut new_hashes: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (index, hash) in decoded_new.iter().enumerate() {
        if let Some(hash) = hash {
            new_hashes.entry(hash).or_default().push(index);
        }
    }

    for (hash, old_indexes) in &old_hashes {
        let Some(new_indexes) = new_hashes.get(hash) else {
            continue;
        };
        if old_indexes.len() == 1 && new_indexes.len() == 1 {
            pair_match(
                old_indexes[0],
                new_indexes[0],
                "exact",
                &mut old_to_new,
                &mut new_to_old,
                &mut confidence,
            );
        }
    }

    loop {
        let mut progress = false;
        for (hash, old_indexes) in &old_hashes {
            let Some(new_indexes) = new_hashes.get(hash) else {
                continue;
            };
            let mut old_groups: HashMap<(Vec<u8>, Vec<u8>), Vec<usize>> = HashMap::new();
            for &old_index in old_indexes {
                if old_to_new[old_index].is_some() {
                    continue;
                }
                if let Some(signature) = old_anchor_signature(old_index, old, &old_to_new) {
                    old_groups.entry(signature).or_default().push(old_index);
                }
            }
            let mut new_groups: HashMap<(Vec<u8>, Vec<u8>), Vec<usize>> = HashMap::new();
            for &new_index in new_indexes {
                if new_to_old[new_index].is_some() {
                    continue;
                }
                if let Some(signature) = new_anchor_signature(new_index, old, &new_to_old) {
                    new_groups.entry(signature).or_default().push(new_index);
                }
            }
            for (signature, old_group) in old_groups {
                let Some(new_group) = new_groups.get(&signature) else {
                    continue;
                };
                if old_group.len() == 1 && new_group.len() == 1 {
                    pair_match(
                        old_group[0],
                        new_group[0],
                        "exact",
                        &mut old_to_new,
                        &mut new_to_old,
                        &mut confidence,
                    );
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }

    let mut anchors: Vec<(usize, usize)> = old_to_new
        .iter()
        .enumerate()
        .filter_map(|(old_index, new_index)| new_index.map(|new_index| (old_index, new_index)))
        .collect();
    anchors.sort_unstable();
    for pair in anchors.windows(2) {
        let (old_left, new_left) = pair[0];
        let (old_right, new_right) = pair[1];
        if new_left >= new_right {
            continue;
        }
        let old_unmatched: Vec<_> = ((old_left + 1)..old_right)
            .filter(|index| old_to_new[*index].is_none())
            .collect();
        let new_unmatched: Vec<_> = ((new_left + 1)..new_right)
            .filter(|index| new_to_old[*index].is_none())
            .collect();
        let intervening_old_match = ((old_left + 1)..old_right).any(|i| old_to_new[i].is_some());
        let intervening_new_match = ((new_left + 1)..new_right).any(|i| new_to_old[i].is_some());
        if !intervening_old_match
            && !intervening_new_match
            && old_unmatched.len() == 1
            && new_unmatched.len() == 1
        {
            pair_match(
                old_unmatched[0],
                new_unmatched[0],
                "neighbor-anchored",
                &mut old_to_new,
                &mut new_to_old,
                &mut confidence,
            );
        }
    }

    (new_to_old, confidence)
}

fn pair_match(
    old_index: usize,
    new_index: usize,
    match_confidence: &'static str,
    old_to_new: &mut [Option<usize>],
    new_to_old: &mut [Option<usize>],
    confidence: &mut [Option<&'static str>],
) {
    if old_to_new[old_index].is_none() && new_to_old[new_index].is_none() {
        old_to_new[old_index] = Some(new_index);
        new_to_old[new_index] = Some(old_index);
        confidence[new_index] = Some(match_confidence);
    }
}

fn old_anchor_signature(
    index: usize,
    old: &[&OldOccurrence],
    old_to_new: &[Option<usize>],
) -> Option<(Vec<u8>, Vec<u8>)> {
    let previous = (0..index)
        .rev()
        .find(|candidate| old_to_new[*candidate].is_some())?;
    let next = ((index + 1)..old.len()).find(|candidate| old_to_new[*candidate].is_some())?;
    Some((
        old[previous].logical_id.clone(),
        old[next].logical_id.clone(),
    ))
}

fn new_anchor_signature(
    index: usize,
    old: &[&OldOccurrence],
    new_to_old: &[Option<usize>],
) -> Option<(Vec<u8>, Vec<u8>)> {
    let previous = (0..index)
        .rev()
        .find_map(|candidate| new_to_old[candidate])?;
    let next = ((index + 1)..new_to_old.len()).find_map(|candidate| new_to_old[candidate])?;
    Some((
        old[previous].logical_id.clone(),
        old[next].logical_id.clone(),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "observation columns are intentionally explicit at the SQLite boundary"
)]
fn insert_observation(
    tx: &Transaction<'_>,
    snapshot_id: i64,
    logical_id: &[u8],
    predecessor: Option<i64>,
    lifecycle: &str,
    body_changed: bool,
    context_changed: bool,
    path: &str,
    content_hash: &[u8],
    match_confidence: &str,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO doc_block_observations(
           logical_id, predecessor_observation_id, lifecycle, body_changed,
           context_changed, snapshot_id, path, content_hash, match_confidence
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            logical_id,
            predecessor,
            lifecycle,
            i64::from(body_changed),
            i64::from(context_changed),
            snapshot_id,
            path,
            content_hash,
            match_confidence
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn replace_current_projection(
    tx: &Transaction<'_>,
    snapshot_id: i64,
    corpus: &Corpus,
    current: &BTreeMap<(String, u64), CurrentOccurrence>,
) -> Result<()> {
    tx.execute("DELETE FROM doc_chunks_fts", [])?;
    tx.execute("DELETE FROM doc_chunk_blocks", [])?;
    tx.execute("DELETE FROM doc_embedding_index_entries", [])?;
    tx.execute("DELETE FROM doc_chunks", [])?;
    tx.execute("DELETE FROM doc_block_occurrences", [])?;
    tx.execute("DELETE FROM doc_files", [])?;
    tx.execute("DELETE FROM doc_inventory", [])?;

    let mut decisions: Vec<_> = corpus.decisions.iter().collect();
    decisions.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| left.path_base64.cmp(&right.path_base64))
            .then_with(|| left.subject.as_bytes().cmp(right.subject.as_bytes()))
    });
    for decision in decisions {
        tx.execute(
            "INSERT INTO doc_inventory(
               path, subject, rule, detail, path_base64, path_encoding, snapshot_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                decision.path,
                decision.subject,
                decision.rule,
                decision.detail,
                decision.path_base64,
                decision.path_encoding,
                snapshot_id
            ],
        )?;
    }

    let mut files: Vec<_> = corpus.files.iter().collect();
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    for pair in files.windows(2) {
        ensure!(
            pair[0].path != pair[1].path,
            "duplicate documentation path {}",
            pair[0].path
        );
    }

    for file in files {
        insert_file(tx, snapshot_id, file)?;
        let mut blocks: Vec<_> = file.blocks.iter().collect();
        blocks.sort_by_key(|block| block.ordinal);
        ensure_unique_ordinals(&file.path, &blocks)?;

        let mut occurrence_by_ordinal: BTreeMap<u64, &CurrentOccurrence> = BTreeMap::new();
        for block in &blocks {
            let occurrence = current
                .get(&(file.path.clone(), block.ordinal))
                .ok_or_else(|| {
                    anyhow!(
                        "missing history projection for {}:{}",
                        file.path,
                        block.ordinal
                    )
                })?;
            let content_hash = decode_hex_32(&block.content_hash).with_context(|| {
                format!("decode block hash for {}:{}", file.path, block.ordinal)
            })?;
            ensure!(
                blake3::hash(block.body.as_bytes()).as_bytes() == content_hash.as_slice(),
                "block {}:{} content hash does not match its source body",
                file.path,
                block.ordinal
            );
            ensure_source_span(
                &file.path,
                "block",
                block.ordinal,
                block.source_start,
                block.source_end,
                file.byte_len,
            )?;
            ensure!(
                block.source_end - block.source_start == block.body.len() as u64,
                "block {}:{} source span length does not match its body",
                file.path,
                block.ordinal
            );
            insert_block_content(tx, &content_hash, &block.body)?;
            tx.execute(
                "INSERT INTO doc_block_occurrences(
                   logical_id, snapshot_id, current_observation_id, path, block_order,
                   source_start, source_end, start_line, end_line, content_hash,
                   block_kind, breadcrumb, nearest_heading, freshness_basis,
                   freshness_sequence
                 ) VALUES(
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15
                 )",
                params![
                    occurrence.logical_id,
                    snapshot_id,
                    occurrence.observation_id,
                    file.path,
                    sql_u64(block.ordinal)?,
                    sql_u64(block.source_start)?,
                    sql_u64(block.source_end)?,
                    sql_u64(block.line_start)?,
                    sql_u64(block.line_end)?,
                    content_hash,
                    block.kind,
                    block.breadcrumb,
                    block.nearest_heading,
                    occurrence.freshness_basis,
                    occurrence.freshness_sequence
                ],
            )?;
            occurrence_by_ordinal.insert(block.ordinal, occurrence);
        }

        let mut chunks: Vec<_> = file.chunks.iter().collect();
        chunks.sort_by_key(|chunk| chunk.ordinal);
        for pair in chunks.windows(2) {
            ensure!(
                pair[0].ordinal != pair[1].ordinal,
                "duplicate chunk ordinal {} in {}",
                pair[0].ordinal,
                file.path
            );
        }
        for chunk in chunks {
            ensure_source_span(
                &file.path,
                "chunk",
                chunk.ordinal,
                chunk.source_start,
                chunk.source_end,
                file.byte_len,
            )?;
            let embedding_identity = chunk
                .embedding_identity
                .as_deref()
                .map(decode_hex_32)
                .transpose()
                .with_context(|| {
                    format!(
                        "decode embedding identity for {}:{}",
                        file.path, chunk.ordinal
                    )
                })?;
            ensure!(
                chunk.is_stub == embedding_identity.is_none(),
                "chunk {}:{} has inconsistent stub/embedding identity",
                file.path,
                chunk.ordinal
            );
            if chunk.is_stub {
                ensure!(
                    chunk.embedding_text.is_none() && chunk.rendered_body.is_empty(),
                    "document stub {}:{} must have empty body and no provider text",
                    file.path,
                    chunk.ordinal
                );
            } else {
                let provider_text = chunk.embedding_text.as_deref().ok_or_else(|| {
                    anyhow!(
                        "non-stub chunk {}:{} has no provider text",
                        file.path,
                        chunk.ordinal
                    )
                })?;
                let expected_text =
                    embedding_text(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
                ensure!(
                    provider_text == expected_text,
                    "chunk {}:{} provider text violates the documentation serialization contract",
                    file.path,
                    chunk.ordinal
                );
                let expected_identity =
                    embedding_identity_for(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
                ensure!(
                    embedding_identity.as_deref() == Some(expected_identity.as_slice()),
                    "chunk {}:{} embedding identity violates the documentation serialization contract",
                    file.path,
                    chunk.ordinal
                );
                ensure!(
                    provider_text.len() <= 24_000,
                    "chunk {}:{} exceeds the documentation provider byte bound",
                    file.path,
                    chunk.ordinal
                );
            }
            let rendered_body_hash = blake3::hash(chunk.rendered_body.as_bytes());
            let mut seen_block_ordinals = BTreeSet::new();
            let mut freshness_sequence = None;
            for ordinal in &chunk.block_ordinals {
                ensure!(
                    seen_block_ordinals.insert(*ordinal),
                    "chunk {}:{} repeats block ordinal {ordinal}",
                    file.path,
                    chunk.ordinal
                );
                let occurrence = occurrence_by_ordinal.get(ordinal).ok_or_else(|| {
                    anyhow!(
                        "chunk {}:{} references missing block ordinal {ordinal}",
                        file.path,
                        chunk.ordinal
                    )
                })?;
                freshness_sequence = freshness_sequence.max(occurrence.freshness_sequence);
            }
            let freshness_basis = if freshness_sequence.is_some() {
                "observed"
            } else {
                "unknown"
            };
            tx.execute(
                "INSERT INTO doc_chunks(
                   snapshot_id, path, chunk_order, source_start, source_end,
                   start_line, end_line, breadcrumb, nearest_heading, rendered_body,
                   provider_text, rendered_body_hash, embedding_identity, stub,
                   freshness_basis, freshness_sequence
                 ) VALUES(
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16
                 )",
                params![
                    snapshot_id,
                    file.path,
                    sql_u64(chunk.ordinal)?,
                    sql_u64(chunk.source_start)?,
                    sql_u64(chunk.source_end)?,
                    sql_u64(chunk.line_start)?,
                    sql_u64(chunk.line_end)?,
                    chunk.breadcrumb,
                    chunk.nearest_heading,
                    chunk.rendered_body,
                    chunk.embedding_text,
                    rendered_body_hash.as_bytes(),
                    embedding_identity,
                    i64::from(chunk.is_stub),
                    freshness_basis,
                    freshness_sequence
                ],
            )?;
            let chunk_id = tx.last_insert_rowid();
            let metadata = lexical_metadata(file);
            tx.execute(
                "INSERT INTO doc_chunks_fts(rowid, title, metadata, breadcrumb, body, path)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    chunk_id,
                    file.title,
                    metadata,
                    chunk.breadcrumb,
                    chunk.rendered_body,
                    file.path
                ],
            )?;
            for (position, ordinal) in chunk.block_ordinals.iter().enumerate() {
                let occurrence = occurrence_by_ordinal[ordinal];
                tx.execute(
                    "INSERT INTO doc_chunk_blocks(chunk_id, block_position, logical_id)
                     VALUES(?1, ?2, ?3)",
                    params![chunk_id, sql_i64(position)?, occurrence.logical_id],
                )?;
            }
        }
    }
    Ok(())
}

fn insert_file(tx: &Transaction<'_>, snapshot_id: i64, file: &DocFile) -> Result<()> {
    let file_hash = decode_hex_32(&file.content_hash)
        .with_context(|| format!("decode file hash for {}", file.path))?;
    let tags_json = serde_json::to_string(&file.tags)?;
    tx.execute(
        "INSERT INTO doc_files(
           path, snapshot_id, file_hash, byte_len, line_count,
           title, description, tags_json, front_matter_state
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            file.path,
            snapshot_id,
            file_hash,
            sql_u64(file.byte_len)?,
            sql_u64(file.line_count)?,
            file.title,
            file.description,
            tags_json,
            file.front_matter_state
        ],
    )?;
    Ok(())
}

fn ensure_source_span(
    path: &str,
    kind: &str,
    ordinal: u64,
    start: u64,
    end: u64,
    file_len: u64,
) -> Result<()> {
    ensure!(
        start <= end && end <= file_len,
        "{kind} {path}:{ordinal} has an invalid source span {start}..{end} for {file_len} bytes"
    );
    Ok(())
}

fn insert_block_content(tx: &Transaction<'_>, hash: &[u8], body: &str) -> Result<()> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT body FROM doc_block_contents WHERE content_hash=?1",
            [hash],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        ensure!(existing == body, "documentation block hash collision");
    } else {
        tx.execute(
            "INSERT INTO doc_block_contents(content_hash, body) VALUES(?1, ?2)",
            params![hash, body],
        )?;
    }
    Ok(())
}

fn lexical_metadata(file: &DocFile) -> String {
    let mut parts = Vec::new();
    if let Some(description) = file
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        parts.push(description);
    }
    parts.extend(file.tags.iter().map(String::as_str));
    parts.join(" ")
}

fn update_path_states(
    tx: &Transaction<'_>,
    snapshot_id: i64,
    corpus: &Corpus,
    previous_paths: &BTreeSet<String>,
) -> Result<()> {
    let indexed_paths: BTreeSet<_> = corpus.files.iter().map(|file| file.path.as_str()).collect();
    let decisions: BTreeMap<_, _> = corpus
        .decisions
        .iter()
        .filter(|decision| decision.subject == "file")
        .map(|decision| (decision.path.as_str(), decision.rule.as_str()))
        .collect();

    for path in &indexed_paths {
        tx.execute(
            "INSERT INTO doc_path_states(
               path, ever_seen, continuity_broken, last_rule, updated_snapshot_id
             ) VALUES(?1, 1, 0, 'indexed', ?2)
             ON CONFLICT(path) DO UPDATE SET
               ever_seen=1, continuity_broken=0, last_rule='indexed',
               updated_snapshot_id=excluded.updated_snapshot_id",
            params![path, snapshot_id],
        )?;
    }
    for (path, rule) in &decisions {
        if indexed_paths.contains(path) {
            continue;
        }
        if *rule == "read-error" {
            tx.execute(
                "INSERT INTO doc_path_states(
                   path, ever_seen, continuity_broken, last_rule, updated_snapshot_id
                 ) VALUES(?1, 0, 1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                   continuity_broken=1, last_rule=excluded.last_rule,
                   updated_snapshot_id=excluded.updated_snapshot_id",
                params![path, rule, snapshot_id],
            )?;
        } else {
            tx.execute(
                "UPDATE doc_path_states
                 SET last_rule=?2, updated_snapshot_id=?3 WHERE path=?1",
                params![path, rule, snapshot_id],
            )?;
        }
    }
    for path in previous_paths {
        if !indexed_paths.contains(path.as_str()) && !decisions.contains_key(path.as_str()) {
            tx.execute(
                "UPDATE doc_path_states
                 SET last_rule='absent', updated_snapshot_id=?2 WHERE path=?1",
                params![path, snapshot_id],
            )?;
        }
    }
    Ok(())
}

fn occurrence_id(snapshot_id: i64, path: &str, ordinal: u64, content_hash: &[u8]) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-doc-occurrence-v1\0");
    hasher.update(&snapshot_id.to_be_bytes());
    hasher.update(&(path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update(&ordinal.to_be_bytes());
    hasher.update(content_hash);
    hasher.finalize().as_bytes().to_vec()
}

fn embedding_text(nearest_heading: Option<&str>, rendered_body: &str) -> String {
    match nearest_heading {
        Some(heading) => format!("{heading}\n\n{rendered_body}"),
        None => rendered_body.to_owned(),
    }
}

fn embedding_identity_for(nearest_heading: Option<&str>, rendered_body: &str) -> Vec<u8> {
    let heading = nearest_heading.unwrap_or("");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-doc-embedding-v1\0");
    hasher.update(&[u8::from(nearest_heading.is_some())]);
    hasher.update(&(heading.len() as u64).to_be_bytes());
    hasher.update(heading.as_bytes());
    hasher.update(&(rendered_body.len() as u64).to_be_bytes());
    hasher.update(rendered_body.as_bytes());
    hasher.finalize().as_bytes().to_vec()
}

fn compute_corpus_fingerprint(files: &[DocFile]) -> Result<String> {
    let mut files: Vec<_> = files.iter().collect();
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-doc-corpus-v1\0");
    hasher.update(&(files.len() as u64).to_be_bytes());
    for file in files {
        let hash = decode_hex_32(&file.content_hash)
            .with_context(|| format!("decode file hash for {}", file.path))?;
        hasher.update(&(file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(&hash);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn decode_hex_32(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len() == 64,
        "expected a 32-byte lowercase hexadecimal digest"
    );
    let mut decoded = Vec::with_capacity(32);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hexadecimal digest"),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn sql_i64(value: usize) -> Result<i64> {
    i64::try_from(value).context("value does not fit SQLite INTEGER")
}

fn sql_u64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value does not fit SQLite INTEGER")
}

fn usize_from_sql(value: i64) -> Result<usize> {
    usize::try_from(value).context("negative or oversized SQLite count")
}

fn count(conn: &Connection, table: &str) -> Result<usize> {
    ensure!(
        matches!(
            table,
            "doc_block_occurrences" | "doc_chunks" | "doc_embeddings" | "doc_block_observations"
        ),
        "unsupported count table"
    );
    let query = format!("SELECT count(*) FROM {table}");
    usize_from_sql(conn.query_row(&query, [], |row| row.get(0))?)
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn row_u32(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value: i64 = row.get(index)?;
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[cfg(test)]
#[expect(
    clippy::items_after_test_module,
    reason = "platform-specific open helpers remain grouped after the store unit tests"
)]
mod tests {
    use super::*;
    use crate::docs::corpus::{DocChunk, DocFile};
    use tempfile::TempDir;

    fn test_store(temp: &TempDir) -> Result<DocsStore> {
        DocsStore::open(temp.path(), None, None)
    }

    fn decision(path: &str, rule: &str) -> Decision {
        Decision {
            path: path.to_owned(),
            subject: "file".to_owned(),
            rule: rule.to_owned(),
            detail: None,
            path_base64: None,
            path_encoding: None,
        }
    }

    fn block(ordinal: u64, start: u64, body: &str) -> DocBlock {
        DocBlock {
            ordinal,
            kind: "paragraph".to_owned(),
            source_start: start,
            source_end: start + body.len() as u64,
            line_start: ordinal + 1,
            line_end: ordinal + 1,
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            body: body.to_owned(),
            rendered_body: body.to_owned(),
            breadcrumb: "Guide".to_owned(),
            nearest_heading: Some("Guide".to_owned()),
        }
    }

    fn file(path: &str, bodies: &[&str]) -> DocFile {
        let blocks: Vec<_> = bodies
            .iter()
            .scan(0_u64, |start, body| {
                let block = block(0, *start, body);
                *start += body.len() as u64 + 1;
                Some(block)
            })
            .enumerate()
            .map(|(ordinal, mut block)| {
                block.ordinal = ordinal as u64;
                block.line_start = ordinal as u64 + 1;
                block.line_end = ordinal as u64 + 1;
                block
            })
            .collect();
        let rendered_body = bodies.join("\n\n");
        let source_end = blocks.last().map_or(0, |block| block.source_end);
        let embedding_text = format!("Guide\n\n{rendered_body}");
        let embedding_identity = encode_hex(&embedding_identity_for(Some("Guide"), &rendered_body));
        let source = bodies.join("\n");
        DocFile {
            path: path.to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            byte_len: source.len() as u64,
            line_count: bodies.len() as u64,
            title: "Guide".to_owned(),
            description: Some("Test documentation".to_owned()),
            tags: vec!["test".to_owned()],
            front_matter_state: "absent".to_owned(),
            headings: Vec::new(),
            chunks: vec![DocChunk {
                ordinal: 0,
                source_start: 0,
                source_end,
                line_start: 1,
                line_end: bodies.len() as u64,
                breadcrumb: "Guide".to_owned(),
                nearest_heading: Some("Guide".to_owned()),
                rendered_body,
                embedding_text: Some(embedding_text),
                embedding_identity: Some(embedding_identity),
                block_ordinals: blocks.iter().map(|block| block.ordinal).collect(),
                is_stub: false,
            }],
            blocks,
        }
    }

    fn corpus(root: &Path, files: Vec<DocFile>, decisions: Vec<Decision>) -> Result<Corpus> {
        let fingerprint = compute_corpus_fingerprint(&files)?;
        Ok(Corpus {
            canonical_root: root.canonicalize()?,
            fingerprint,
            files,
            decisions,
        })
    }

    #[test]
    fn store_identity_rejects_cross_root_and_main_database_aliases() -> Result<()> {
        let first = TempDir::new()?;
        let second = TempDir::new()?;
        let docs_path = first.path().join("docs.db");
        let store = DocsStore::open(first.path(), Some(&docs_path), None)?;
        let application_id: i64 =
            store
                .connection()
                .pragma_query_value(None, "application_id", |row| row.get(0))?;
        assert_eq!(application_id, APPLICATION_ID);
        drop(store);

        let cross_root = DocsStore::open(second.path(), Some(&docs_path), None);
        assert!(
            cross_root
                .unwrap_err()
                .to_string()
                .contains("different repository root")
        );

        let main_path = first.path().join("main.db");
        Connection::open(&main_path)?.execute("CREATE TABLE marker(id INTEGER)", [])?;
        let direct_alias = DocsStore::open(first.path(), Some(&main_path), Some(&main_path));
        assert!(
            direct_alias
                .unwrap_err()
                .to_string()
                .contains("aliases the main database")
        );

        let hard_link = first.path().join("hard-link.db");
        fs::hard_link(&main_path, &hard_link)?;
        let hard_alias = DocsStore::open(first.path(), Some(&hard_link), Some(&main_path));
        assert!(
            hard_alias
                .unwrap_err()
                .to_string()
                .contains("aliases the main database")
        );
        Ok(())
    }

    #[test]
    fn read_only_open_requires_and_preserves_a_published_store() -> Result<()> {
        let temp = TempDir::new()?;
        let missing = DocsStore::open_read_only(temp.path(), None, None);
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("jscout docs index")
        );

        let mut writable = test_store(&temp)?;
        let input = corpus(
            temp.path(),
            vec![file("guide.md", &["read only body"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let publication = writable.publish(&input)?;
        drop(writable);

        let read_only = DocsStore::open_read_only(temp.path(), None, None)?;
        assert_eq!(
            read_only.current_snapshot_id()?,
            Some(publication.snapshot_id)
        );
        assert_eq!(read_only.lexical_search("body", 10)?.len(), 1);
        assert!(
            read_only
                .connection()
                .execute("DELETE FROM doc_chunks", [])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn failed_replacement_keeps_last_good_projection() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let first = corpus(
            temp.path(),
            vec![file("guide.md", &["alpha token"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let publication = store.publish(&first)?;
        let replacement = corpus(
            temp.path(),
            vec![file("guide.md", &["beta token"])],
            vec![decision("guide.md", "indexed")],
        )?;
        assert!(store.publish_inner(&replacement, true).is_err());
        assert_eq!(store.current_snapshot_id()?, Some(publication.snapshot_id));
        assert_eq!(store.lexical_search("alpha", 10)?.len(), 1);
        assert!(store.lexical_search("beta", 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn read_transaction_pins_one_projection_across_concurrent_publication() -> Result<()> {
        let temp = TempDir::new()?;
        let mut writer = test_store(&temp)?;
        let first = corpus(
            temp.path(),
            vec![file("guide.md", &["alpha generation"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let first_publication = writer.publish(&first)?;
        let reader = DocsStore::open_read_only(temp.path(), None, None)?;

        let read_snapshot = reader.connection().unchecked_transaction()?;
        assert_eq!(
            reader.current_snapshot_id()?,
            Some(first_publication.snapshot_id)
        );
        let second = corpus(
            temp.path(),
            vec![file("guide.md", &["beta generation"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let second_publication = writer.publish(&second)?;

        assert_eq!(
            reader.current_snapshot_id()?,
            Some(first_publication.snapshot_id)
        );
        assert_eq!(reader.lexical_search("alpha", 10)?.len(), 1);
        assert!(reader.lexical_search("beta", 10)?.is_empty());
        drop(read_snapshot);

        assert_eq!(
            reader.current_snapshot_id()?,
            Some(second_publication.snapshot_id)
        );
        assert_eq!(reader.lexical_search("beta", 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn weighted_lexical_search_returns_current_source_metadata() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let input = corpus(
            temp.path(),
            vec![file("guide.md", &["configure the needle provider"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let published = store.publish(&input)?;
        let hits = store.lexical_search("needle", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snapshot_id, published.snapshot_id);
        assert_eq!(hits[0].path, "guide.md");
        assert_eq!(hits[0].breadcrumb, "Guide");
        assert!(hits[0].score.is_finite());
        assert_eq!(hits[0].file_hash, input.files[0].content_hash);
        Ok(())
    }

    #[test]
    fn status_reports_malformed_front_matter_as_body() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let mut malformed = file("guide.md", &["---\ntitle: [\n---"]);
        malformed.front_matter_state = "malformed_as_body".to_owned();
        let input = corpus(
            temp.path(),
            vec![malformed],
            vec![decision("guide.md", "indexed")],
        )?;
        store.publish(&input)?;
        assert_eq!(
            store.status()?.front_matter,
            [FrontMatterStatus {
                path: "guide.md".to_owned(),
                state: "malformed_as_body".to_owned(),
            }]
        );
        Ok(())
    }

    #[test]
    fn unchanged_blocks_do_not_append_observations() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let first = corpus(
            temp.path(),
            vec![file("guide.md", &["first", "second"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let initial = store.publish(&first)?;
        assert_eq!(initial.observations_added, 2);
        let second = store.publish(&first)?;
        assert_eq!(second.observations_added, 0);
        assert_eq!(store.status()?.observation_count, 2);
        Ok(())
    }

    #[test]
    fn inserted_block_adds_one_observation_and_preserves_neighbors() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let first = corpus(
            temp.path(),
            vec![file("guide.md", &["anchor one", "anchor two"])],
            vec![decision("guide.md", "indexed")],
        )?;
        store.publish(&first)?;
        let second = corpus(
            temp.path(),
            vec![file("guide.md", &["anchor one", "inserted", "anchor two"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let publication = store.publish(&second)?;
        assert_eq!(publication.observations_added, 1);
        let added: i64 = store.connection().query_row(
            "SELECT count(*) FROM doc_block_observations
             WHERE snapshot_id=?1 AND lifecycle='added'",
            [publication.snapshot_id],
            |row| row.get(0),
        )?;
        assert_eq!(added, 1);
        let hits = store.lexical_search("inserted", 10)?;
        assert_eq!(hits[0].freshness_basis, "observed");
        assert_eq!(hits[0].freshness_sequence, Some(publication.snapshot_id));
        assert!(hits[0].freshness_observed_at.is_some());
        Ok(())
    }

    #[test]
    fn ambiguous_duplicate_blocks_get_no_guessed_predecessors() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let duplicated = corpus(
            temp.path(),
            vec![file("guide.md", &["same", "same"])],
            vec![decision("guide.md", "indexed")],
        )?;
        store.publish(&duplicated)?;
        let replacement = store.publish(&duplicated)?;
        assert_eq!(replacement.observations_added, 4);
        let guessed: i64 = store.connection().query_row(
            "SELECT count(*) FROM doc_block_observations
             WHERE snapshot_id=?1 AND lifecycle='added'
               AND predecessor_observation_id IS NOT NULL",
            [replacement.snapshot_id],
            |row| row.get(0),
        )?;
        assert_eq!(guessed, 0);
        let added: i64 = store.connection().query_row(
            "SELECT count(*) FROM doc_block_observations
             WHERE snapshot_id=?1 AND lifecycle='added'",
            [replacement.snapshot_id],
            |row| row.get(0),
        )?;
        assert_eq!(added, 2);
        Ok(())
    }

    #[test]
    fn permanent_read_gap_emits_no_removal_and_recovery_is_baseline() -> Result<()> {
        let temp = TempDir::new()?;
        let mut store = test_store(&temp)?;
        let indexed = corpus(
            temp.path(),
            vec![file("guide.md", &["stable body"])],
            vec![decision("guide.md", "indexed")],
        )?;
        let first = store.publish(&indexed)?;
        let first_logical: Vec<u8> = store.connection().query_row(
            "SELECT logical_id FROM doc_block_occurrences",
            [],
            |row| row.get(0),
        )?;

        let gap = corpus(
            temp.path(),
            Vec::new(),
            vec![decision("guide.md", "read-error")],
        )?;
        let gap_publication = store.publish(&gap)?;
        assert_eq!(gap_publication.observations_added, 0);
        let removed: i64 = store.connection().query_row(
            "SELECT count(*) FROM doc_block_observations
             WHERE snapshot_id=?1 AND lifecycle='removed'",
            [gap_publication.snapshot_id],
            |row| row.get(0),
        )?;
        assert_eq!(removed, 0);

        let recovered = store.publish(&indexed)?;
        assert_eq!(recovered.observations_added, 1);
        let (lifecycle, predecessor, recovered_logical): (String, Option<i64>, Vec<u8>) =
            store.connection().query_row(
                "SELECT o.lifecycle, o.predecessor_observation_id, o.logical_id
                 FROM doc_block_observations o WHERE o.snapshot_id=?1",
                [recovered.snapshot_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(lifecycle, "baseline");
        assert_eq!(predecessor, None);
        assert_ne!(recovered_logical, first_logical);
        assert_eq!(first.snapshot_id + 2, recovered.snapshot_id);
        Ok(())
    }
}

pub fn resolve_database_path(root: &Path, configured_path: Option<&Path>) -> Result<PathBuf> {
    let configured = configured_path.unwrap_or_else(|| Path::new(super::DB_FILE));
    let candidate = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        root.join(configured)
    };
    resolve_path_allow_missing(&candidate).with_context(|| {
        format!(
            "resolve documentation database path {}",
            candidate.display()
        )
    })
}

fn initialize_or_validate(
    conn: &Connection,
    canonical_root: &Path,
    canonical_root_key: &[u8],
    database_path: &Path,
) -> Result<()> {
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;

    if application_id == 0 && user_tables == 0 {
        let now = unix_timestamp()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let initialized = (|| -> Result<()> {
            conn.pragma_update(None, "application_id", APPLICATION_ID)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            conn.execute_batch(SCHEMA)?;
            conn.execute(
                "INSERT INTO doc_store_meta(
                   id, schema_version, canonical_root, canonical_root_display, created_at
                 ) VALUES(1, ?1, ?2, ?3, ?4)",
                params![
                    SCHEMA_VERSION,
                    canonical_root_key,
                    canonical_root.to_string_lossy(),
                    now
                ],
            )?;
            Ok(())
        })();
        if let Err(error) = initialized {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error).context("initialize documentation database");
        }
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }

    if application_id != APPLICATION_ID {
        bail!(
            "database `{}` is not a jscout documentation database (application_id=0x{application_id:08X})",
            database_path.display()
        );
    }

    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        user_version == SCHEMA_VERSION,
        "documentation database `{}` uses schema v{user_version}, but this jscout requires v{SCHEMA_VERSION}",
        database_path.display()
    );
    let (stored_schema, stored_root): (i64, Vec<u8>) = conn
        .query_row(
            "SELECT schema_version, canonical_root FROM doc_store_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .with_context(|| {
            format!(
                "documentation database `{}` has an incomplete identity record",
                database_path.display()
            )
        })?;
    ensure!(
        stored_schema == SCHEMA_VERSION,
        "documentation store metadata schema mismatch"
    );
    ensure!(
        stored_root == canonical_root_key,
        "documentation database `{}` is bound to a different repository root",
        database_path.display()
    );
    Ok(())
}

fn reject_database_alias(root: &Path, docs_path: &Path, main_path: &Path) -> Result<()> {
    let main_candidate = if main_path.is_absolute() {
        main_path.to_path_buf()
    } else {
        root.join(main_path)
    };
    let normalized_main = resolve_path_allow_missing(&main_candidate)
        .with_context(|| format!("resolve main database path {}", main_candidate.display()))?;
    if docs_path == normalized_main || same_file_identity(docs_path, &normalized_main)? {
        bail!(
            "documentation database `{}` aliases the main database `{}`; configure a separate [docs.database].path",
            docs_path.display(),
            normalized_main.display()
        );
    }
    Ok(())
}

fn resolve_path_allow_missing(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("canonicalize {}", path.display()));
    }

    let mut missing = Vec::new();
    let mut existing = path;
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            anyhow!(
                "cannot resolve path with no existing ancestor: {}",
                path.display()
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            anyhow!(
                "cannot resolve path with no existing ancestor: {}",
                path.display()
            )
        })?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("canonicalize existing ancestor {}", existing.display()))?;
    for component in missing.into_iter().rev() {
        match Path::new(&component).components().next() {
            Some(Component::Normal(_)) => resolved.push(component),
            _ => bail!("invalid documentation database path component"),
        }
    }
    Ok(resolved)
}

#[cfg(unix)]
fn platform_path_key(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn platform_path_key(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn platform_path_key(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn same_file_identity(left: &Path, right: &Path) -> Result<bool> {
    if optional_metadata(left)?.is_none() {
        return Ok(false);
    }
    if optional_metadata(right)?.is_none() {
        return Ok(false);
    }
    same_file::is_same_file(left, right).with_context(|| {
        format!(
            "compare database file identity for {} and {}",
            left.display(),
            right.display()
        )
    })
}

fn optional_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("inspect database path {}", path.display()))
        }
    }
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock predates Unix epoch")?
        .as_secs();
    i64::try_from(seconds).context("system timestamp does not fit SQLite INTEGER")
}
