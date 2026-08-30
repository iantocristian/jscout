use std::cmp::Ordering;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::corpus::Decision;
use crate::publication::{Identities, Plane};

/// A read-only summary of the documentation plane in the shared database.
/// Vector generations bind the documentation digest rather than the global
/// publication marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub snapshot: String,
    pub publication_snapshot: String,
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

/// One current-documentation-digest chunk hydrated from the shared
/// `files`/`chunks` tables and the documentation ranking sidecars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub chunk_id: i64,
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub breadcrumb: String,
    pub nearest_heading: Option<String>,
    pub rendered_body: String,
    #[serde(skip_serializing, default)]
    pub source_start: u64,
    #[serde(skip_serializing, default)]
    pub source_end: u64,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing, default)]
    pub file_hash: String,
    #[serde(skip_serializing, default)]
    pub embedding_identity: Option<String>,
    pub freshness_basis: String,
    #[serde(skip_serializing, default)]
    pub freshness_author_time: Option<i64>,
    #[serde(skip_serializing, default)]
    pub freshness_committer_time: Option<i64>,
    // Operational Git failures are emitted by indexing. They are not part of
    // a hit's stable retrieval contract or the documentation identity.
    #[serde(skip_serializing, default)]
    pub freshness_detail: Option<String>,
    pub score: f64,
    pub stub: bool,
}

pub fn current_snapshot(conn: &Connection) -> Result<String> {
    crate::store::validate_published_contracts(conn)?;
    crate::publication::current_documentation_digest(conn)
}

/// Read status from one pinned SQLite snapshot so a concurrent `jscout index`
/// cannot mix inventory and chunk counts from different publications.
pub fn status(conn: &Connection) -> Result<Status> {
    crate::store::with_read_snapshot(conn, "jscout_docs_status", || status_inner(conn))
}

fn status_inner(conn: &Connection) -> Result<Status> {
    crate::store::validate_published_contracts(conn)?;
    let identity = Identities::read(conn)?.response(Plane::Documentation);
    let metadata_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationMetadata);
    let vector_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationVector);
    let canonical_root = conn
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .optional()?;
    let inventory_count = count_query(conn, "SELECT COUNT(*) FROM doc_inventory", [])?;
    let indexed_file_count = count_query(
        conn,
        "SELECT COUNT(*) FROM files
         WHERE corpus='docs'
           AND format IN (SELECT value FROM json_each(?1))",
        [&metadata_formats],
    )?;
    let rejection_count = count_query(
        conn,
        "SELECT COUNT(*) FROM doc_inventory WHERE rule!='indexed'",
        [],
    )?;
    let chunk_count = count_query(
        conn,
        "SELECT COUNT(*)
         FROM chunks c JOIN files f ON f.id=c.file_id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))",
        [&metadata_formats],
    )?;
    let embeddable_chunk_count = count_query(
        conn,
        "SELECT COUNT(*)
         FROM doc_chunk_meta m
         JOIN chunks c ON c.id=m.chunk_id
         JOIN files f ON f.id=c.file_id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
           AND m.embedding_identity IS NOT NULL",
        [&vector_formats],
    )?;
    let cached_embedding_count = count_query(
        conn,
        "SELECT COUNT(*)
         FROM embeddings e
         WHERE EXISTS(
           SELECT 1
           FROM doc_chunk_meta m
           JOIN chunks c ON c.id=m.chunk_id
           JOIN files f ON f.id=c.file_id
           WHERE f.corpus='docs'
             AND f.format IN (SELECT value FROM json_each(?1))
             AND m.embedding_identity=e.chunk_hash
         )",
        [&vector_formats],
    )?;
    let ready_vector_generation_count = count_query(
        conn,
        "SELECT COUNT(*) FROM doc_vector_generations WHERE snapshot=?1",
        [&identity.snapshot],
    )?;

    Ok(Status {
        snapshot: identity.snapshot,
        publication_snapshot: identity.publication_snapshot,
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
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationMetadata);
    let mut statement = conn.prepare(
        "SELECT f.path, MIN(m.front_matter_state)
         FROM files f
         JOIN chunks c ON c.file_id=f.id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         WHERE f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?1))
         GROUP BY f.id, f.path
         ORDER BY f.path COLLATE BINARY",
    )?;
    let rows = statement.query_map([&eligible_formats], |row| {
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
pub fn lexical_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let query = fts_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationLexical);
    let mut statement = conn.prepare(
        "SELECT c.id, f.path, m.title, m.description, m.tags_json,
                m.breadcrumb, m.nearest_heading, docs_fts.body,
                c.start, c.end, c.start_line, c.end_line,
                f.hash, m.embedding_identity, m.freshness_basis,
                m.freshness_author_time, m.freshness_committer_time,
                m.freshness_detail,
                -bm25(docs_fts, 4.0, 2.0, 2.0, 1.0, 0.25) AS score,
                c.kind
         FROM docs_fts
         JOIN chunks c ON c.id=docs_fts.rowid
         JOIN files f ON f.id=c.file_id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         WHERE docs_fts MATCH ?1
           AND f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = statement.query_map([query.as_str(), eligible_formats.as_str()], |row| {
        row_to_hit(row, 0.0, Some(18))
    })?;
    let mut hits = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("search documentation for `{query}`"))?;
    hits.sort_by(compare_hits);
    hits.truncate(limit);
    Ok(hits)
}

pub(crate) fn load_hit(conn: &Connection, chunk_id: i64, score: f64) -> Result<SearchHit> {
    let eligible_formats =
        crate::formats::eligible_ids_json(crate::formats::Capability::DocumentationLexical);
    conn.query_row(
        "SELECT c.id, f.path, m.title, m.description, m.tags_json,
                m.breadcrumb, m.nearest_heading, docs_fts.body,
                c.start, c.end, c.start_line, c.end_line,
                f.hash, m.embedding_identity, m.freshness_basis,
                m.freshness_author_time, m.freshness_committer_time,
                m.freshness_detail, c.kind
         FROM chunks c
         JOIN files f ON f.id=c.file_id
         JOIN doc_chunk_meta m ON m.chunk_id=c.id
         JOIN docs_fts ON docs_fts.rowid=c.id
         WHERE c.id=?1
           AND f.corpus='docs'
           AND f.format IN (SELECT value FROM json_each(?2))",
        rusqlite::params![chunk_id, eligible_formats],
        |row| row_to_hit(row, score, None),
    )
    .with_context(|| format!("load documentation chunk {chunk_id}"))
}

fn row_to_hit(
    row: &rusqlite::Row<'_>,
    fallback_score: f64,
    score_index: Option<usize>,
) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        chunk_id: row.get(0)?,
        path: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        tags: row_tags(row, 4)?,
        breadcrumb: row.get(5)?,
        nearest_heading: row.get(6)?,
        rendered_body: row.get(7)?,
        source_start: row_u64(row, 8)?,
        source_end: row_u64(row, 9)?,
        start_line: row_u32(row, 10)?,
        end_line: row_u32(row, 11)?,
        file_hash: row.get(12)?,
        embedding_identity: row.get(13)?,
        freshness_basis: row.get(14)?,
        freshness_author_time: row.get(15)?,
        freshness_committer_time: row.get(16)?,
        freshness_detail: row.get(17)?,
        score: match score_index {
            Some(index) => row.get(index)?,
            None => fallback_score,
        },
        stub: row.get::<_, String>(score_index.map_or(18, |_| 19))? == "markdown_document",
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

fn row_tags(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Vec<String>> {
    let encoded = row.get::<_, String>(index)?;
    serde_json::from_str(&encoded).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
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
        crate::publication::Identities::publish_test(&conn, "code-1", "shared-1", "provenance-1")?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('extraction_version',?1)",
            [crate::entity::EXTRACTION_VERSION],
        )?;
        conn.execute(
            "INSERT INTO meta(key,value)
             VALUES('documentation_chunk_format_version',?1)",
            [crate::docs::CHUNK_FORMAT_VERSION],
        )?;
        for format in crate::formats::ALL {
            conn.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)",
                params![
                    crate::formats::contract_meta_key(format),
                    format.extractor_version
                ],
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key,value)
             VALUES('documentation_provenance_format_version',?1)",
            [crate::docs::PROVENANCE_FORMAT_VERSION],
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
                "INSERT INTO files(path,hash,role,corpus,format)
                 VALUES(?1,?2,'documentation','docs','markdown')",
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
        conn.execute_batch(
            "INSERT INTO files(id,path,hash,role,corpus,format)
               VALUES(100,'empty.md','empty-doc','documentation','docs','markdown');
             INSERT INTO files(id,path,hash,role,corpus,format)
               VALUES(101,'leak.ts','code-file','production','code','typescript');
             INSERT INTO chunks(
               id,file_id,kind,name,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(101,101,'module',NULL,'',0,11,1,1,
                      'code-chunk','needle leak');
             INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
               VALUES(101,'Leak','','','needle leak','leak.ts');",
        )?;
        conn.execute(
            "INSERT INTO doc_inventory(path,subject,rule) VALUES('a.md','file','indexed')",
            [],
        )?;
        Ok((root, conn))
    }

    #[test]
    fn status_exposes_documentation_and_publication_identities() -> Result<()> {
        let (_root, conn) = fixture()?;
        let status = status(&conn)?;
        assert_eq!(status.snapshot, "shared-1");
        assert_eq!(
            status.publication_snapshot,
            crate::publication::current_publication_snapshot(&conn)?
        );
        assert_eq!(status.indexed_file_count, 3);
        assert_eq!(status.chunk_count, 2);

        let hits = lexical_search(&conn, "needle", 10)?;
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.path.ends_with(".md")));
        assert_eq!(hits[0].title, "Needle");
        let serialized = serde_json::to_value(&hits[0])?;
        for private in ["snapshot", "source_start", "source_end", "file_hash"] {
            assert!(serialized.get(private).is_none(), "serialized {private}");
        }
        Ok(())
    }

    #[test]
    fn front_matter_metadata_round_trips_through_lexical_and_direct_loads() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("guide.md"),
            "---\n\
             title: Release Guide\n\
             description: Recoverable deployment metadata\n\
             tags:\n\
               - alpha\n\
               - \"comma,tag\"\n\
               - \"\"\n\
               - \"snowman ☃\"\n\
             ---\n\
             # Deployment\n\n\
             Use the blue channel.\n",
        )?;
        let conn = crate::store::open(root.path())?;
        crate::indexer::index_repo(root.path(), &conn)?;
        let lexical = lexical_search(&conn, "recoverable metadata", 10)?;
        assert_eq!(lexical.len(), 1);
        let hit = &lexical[0];
        assert_eq!(hit.title, "Release Guide");
        assert_eq!(
            hit.description.as_deref(),
            Some("Recoverable deployment metadata")
        );
        assert_eq!(hit.tags, ["alpha", "comma,tag", "", "snowman ☃"]);

        let loaded = load_hit(&conn, hit.chunk_id, 0.25)?;
        assert_eq!(loaded.description, hit.description);
        assert_eq!(loaded.tags, hit.tags);
        assert_eq!(loaded.score, 0.25);
        Ok(())
    }

    #[test]
    fn fts_input_is_terms_not_query_syntax() -> Result<()> {
        let (_root, conn) = fixture()?;
        let hits = lexical_search(&conn, "needle OR NOT (", 10)?;
        assert_eq!(hits.len(), 2);
        Ok(())
    }
}
