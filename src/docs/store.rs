use std::cmp::Ordering;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::corpus::Decision;

/// A read-only summary of the documentation projection inside the current
/// shared structural snapshot. Documentation has no database or snapshot of
/// its own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub snapshot: String,
    pub canonical_root: Option<String>,
    pub inventory_count: usize,
    pub indexed_file_count: usize,
    pub rejection_count: usize,
    pub chunk_count: usize,
    pub embeddable_chunk_count: usize,
    pub cached_embedding_count: usize,
    pub ready_vector_generation_count: usize,
    pub front_matter: Vec<FrontMatterStatus>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrontMatterStatus {
    pub path: String,
    pub state: String,
}

/// One current-snapshot documentation chunk hydrated from the shared
/// `files`/`chunks` tables and the documentation ranking sidecars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub chunk_id: i64,
    pub snapshot: String,
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
    pub embedding_identity: Option<String>,
    pub score: f64,
    pub stub: bool,
}

pub fn current_snapshot(conn: &Connection) -> Result<String> {
    crate::structural::current_snapshot(conn)
}

/// Read status from one pinned SQLite snapshot so a concurrent `jscout index`
/// cannot mix inventory and chunk counts from different publications.
pub fn status(conn: &Connection) -> Result<Status> {
    crate::store::with_read_snapshot(conn, "jscout_docs_status", || status_inner(conn))
}

fn status_inner(conn: &Connection) -> Result<Status> {
    let snapshot = current_snapshot(conn)?;
    let canonical_root = conn
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .optional()?;
    let inventory_count = count_query(conn, "SELECT COUNT(*) FROM doc_inventory", [])?;
    let indexed_file_count = count_query(
        conn,
        "SELECT COUNT(DISTINCT c.file_id)
         FROM chunks c JOIN doc_chunk_meta m ON m.chunk_id=c.id",
        [],
    )?;
    let rejection_count = count_query(
        conn,
        "SELECT COUNT(*) FROM doc_inventory WHERE rule!='indexed'",
        [],
    )?;
    let chunk_count = count_query(conn, "SELECT COUNT(*) FROM doc_chunk_meta", [])?;
    let embeddable_chunk_count = count_query(
        conn,
        "SELECT COUNT(*) FROM doc_chunk_meta WHERE embedding_identity IS NOT NULL",
        [],
    )?;
    let cached_embedding_count = count_query(
        conn,
        "SELECT COUNT(*)
         FROM embeddings e
         WHERE EXISTS(
           SELECT 1 FROM doc_chunk_meta m
           WHERE m.embedding_identity=e.chunk_hash
         )",
        [],
    )?;
    let ready_vector_generation_count = count_query(
        conn,
        "SELECT COUNT(*) FROM doc_vector_generations WHERE snapshot=?1",
        [&snapshot],
    )?;

    Ok(Status {
        snapshot,
        canonical_root,
        inventory_count,
        indexed_file_count,
        rejection_count,
        chunk_count,
        embeddable_chunk_count,
        cached_embedding_count,
        ready_vector_generation_count,
        front_matter: front_matter_status(conn)?,
        decisions: decisions(conn)?,
    })
}

pub fn decisions(conn: &Connection) -> Result<Vec<Decision>> {
    let mut statement = conn.prepare(
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

fn front_matter_status(conn: &Connection) -> Result<Vec<FrontMatterStatus>> {
    let mut statement = conn.prepare(
        "SELECT f.path, MIN(m.front_matter_state)
         FROM files f
         JOIN chunks c ON c.file_id=f.id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         GROUP BY f.id, f.path
         ORDER BY f.path COLLATE BINARY",
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

/// Weighted documentation BM25. User input is treated as terms rather than
/// exposed as FTS syntax, and all exact-score ties use the normative source
/// key before the result is truncated.
pub fn lexical_search(
    conn: &Connection,
    snapshot: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let query = fts_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let mut statement = conn.prepare(
        "SELECT c.id, f.path, m.title, m.breadcrumb, m.nearest_heading,
                docs_fts.body, c.start, c.end, c.start_line, c.end_line,
                f.hash, m.embedding_identity,
                -bm25(docs_fts, 4.0, 2.0, 2.0, 1.0, 0.25) AS score,
                c.kind
         FROM docs_fts
         JOIN chunks c ON c.id=docs_fts.rowid
         JOIN files f ON f.id=c.file_id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         WHERE docs_fts MATCH ?1",
    )?;
    let rows = statement.query_map([query.as_str()], |row| {
        row_to_hit(row, snapshot, 0.0, Some(12))
    })?;
    let mut hits = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("search documentation for `{query}`"))?;
    hits.sort_by(compare_hits);
    hits.truncate(limit);
    Ok(hits)
}

pub(crate) fn load_hit(
    conn: &Connection,
    snapshot: &str,
    chunk_id: i64,
    score: f64,
) -> Result<SearchHit> {
    conn.query_row(
        "SELECT c.id, f.path, m.title, m.breadcrumb, m.nearest_heading,
                docs_fts.body, c.start, c.end, c.start_line, c.end_line,
                f.hash, m.embedding_identity, c.kind
         FROM chunks c
         JOIN files f ON f.id=c.file_id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         JOIN docs_fts ON docs_fts.rowid=c.id
         WHERE c.id=?1",
        [chunk_id],
        |row| row_to_hit(row, snapshot, score, None),
    )
    .with_context(|| format!("load documentation chunk {chunk_id}"))
}

fn row_to_hit(
    row: &rusqlite::Row<'_>,
    snapshot: &str,
    fallback_score: f64,
    score_index: Option<usize>,
) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        chunk_id: row.get(0)?,
        snapshot: snapshot.to_owned(),
        path: row.get(1)?,
        title: row.get(2)?,
        breadcrumb: row.get(3)?,
        nearest_heading: row.get(4)?,
        rendered_body: row.get(5)?,
        source_start: row_u64(row, 6)?,
        source_end: row_u64(row, 7)?,
        start_line: row_u32(row, 8)?,
        end_line: row_u32(row, 9)?,
        file_hash: row.get(10)?,
        embedding_identity: row.get(11)?,
        score: match score_index {
            Some(index) => row.get(index)?,
            None => fallback_score,
        },
        stub: row.get::<_, String>(score_index.map_or(12, |_| 13))? == "markdown_document",
    })
}

fn compare_hits(left: &SearchHit, right: &SearchHit) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| source_key(left).cmp(&source_key(right)))
}

fn source_key(hit: &SearchHit) -> (&str, u64, u64, [u8; 32]) {
    (
        &hit.path,
        hit.source_start,
        hit.source_end,
        *blake3::hash(hit.rendered_body.as_bytes()).as_bytes(),
    )
}

/// Treat user input as terms rather than exposing the FTS5 query language.
fn fts_query(query: &str) -> String {
    query
        .split(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '$')
        })
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn count_query<P: rusqlite::Params>(conn: &Connection, sql: &str, params: P) -> Result<usize> {
    let value = conn.query_row(sql, params, |row| row.get::<_, i64>(0))?;
    usize::try_from(value).context("documentation count is negative")
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn row_u32(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value = row.get::<_, i64>(index)?;
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::params;

    use super::*;

    fn fixture() -> Result<(tempfile::TempDir, Connection)> {
        let root = tempfile::tempdir()?;
        let conn = crate::store::open(root.path())?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('snapshot','shared-1')",
            [],
        )?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('root',?1)",
            [root.path().to_string_lossy()],
        )?;
        for (path, hash, content, title, body, start) in [
            ("z.md", "01", "needle beta", "Needle", "needle beta", 0_i64),
            ("a.md", "02", "needle alpha", "Guide", "needle alpha", 0_i64),
        ] {
            conn.execute(
                "INSERT INTO files(path,hash,role) VALUES(?1,?2,'documentation')",
                params![path, hash],
            )?;
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO chunks(file_id,kind,name,symbols,start,end,start_line,end_line,hash,content)
                 VALUES(?1,'markdown_section',NULL,'',?2,?3,1,1,?4,?5)",
                params![file_id, start, content.len() as i64, hash, content],
            )?;
            let chunk_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO doc_chunk_meta(chunk_id,title,breadcrumb,ordinal,embedding_identity,front_matter_state)
                 VALUES(?1,?2,'',0,?3,'none')",
                params![chunk_id, title, hash],
            )?;
            conn.execute(
                "INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
                 VALUES(?1,?2,'','',?3,?4)",
                params![chunk_id, title, body, path],
            )?;
        }
        conn.execute(
            "INSERT INTO doc_inventory(path,subject,rule) VALUES('a.md','file','indexed')",
            [],
        )?;
        Ok((root, conn))
    }

    #[test]
    fn status_and_lexical_search_use_the_shared_snapshot() -> Result<()> {
        let (_root, conn) = fixture()?;
        let status = status(&conn)?;
        assert_eq!(status.snapshot, "shared-1");
        assert_eq!(status.indexed_file_count, 2);
        assert_eq!(status.chunk_count, 2);

        let hits = lexical_search(&conn, &status.snapshot, "needle", 10)?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].snapshot, "shared-1");
        assert_eq!(hits[0].title, "Needle");
        Ok(())
    }

    #[test]
    fn fts_input_is_terms_not_query_syntax() -> Result<()> {
        let (_root, conn) = fixture()?;
        let hits = lexical_search(&conn, "shared-1", "needle OR NOT (", 10)?;
        assert_eq!(hits.len(), 2);
        Ok(())
    }
}
