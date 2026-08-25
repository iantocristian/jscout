use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;

use anyhow::Result;

use super::{
    IndexOptions, incremental_refresh_repo_with_options, index_repo, index_repo_with_fs,
    index_repo_with_options, index_repo_with_options_and_fs,
    index_repo_with_post_replacement_failure, index_repo_without_extraction_reset,
    refresh_repo_with_options,
};
use crate::test_fs::{FaultFileSystem, FileOperation};
use crate::{docs, embed, origin, query, search, semantic, store, structural};

type MarkdownChunkRow = (
    i64,
    String,
    Option<String>,
    String,
    i64,
    i64,
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<String>,
    String,
);

#[test]
fn shared_index_routes_markdown_without_polluting_code_search_or_graphs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "import './README.md';\nexport const codeOnlyMarker = 1;\n",
    )?;
    let markdown = b"\xEF\xBB\xBF---\r\ntitle: Unified Guide\r\ndescription: shared database\r\ntags: [alpha, beta]\r\n---\r\n# Start\r\n\r\nUse docsMarker now.\r\n";
    fs::write(repo.path().join("README.md"), markdown)?;
    let conn = store::open(repo.path())?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!((outcome.indexed, outcome.rejected), (2, 0));
    let identities = conn
        .prepare("SELECT path, role, corpus, format FROM files ORDER BY path")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(identities.contains(&(
        "README.md".to_owned(),
        "documentation".to_owned(),
        "docs".to_owned(),
        "markdown".to_owned(),
    )));
    assert!(identities.contains(&(
        "main.ts".to_owned(),
        "production".to_owned(),
        "code".to_owned(),
        "typescript".to_owned(),
    )));

    let (
        chunk_id,
        kind,
        name,
        symbols,
        start,
        end,
        content,
        title,
        breadcrumb,
        nearest_heading,
        ordinal,
        embedding_identity,
        front_matter_state,
    ): MarkdownChunkRow = conn.query_row(
        "SELECT chunk.id, chunk.kind, chunk.name, chunk.symbols,
                chunk.start, chunk.end, chunk.content,
                metadata.title, metadata.breadcrumb, metadata.nearest_heading,
                metadata.ordinal, metadata.embedding_identity,
                metadata.front_matter_state
         FROM chunks chunk
         JOIN files file ON file.id=chunk.file_id
         JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
         WHERE file.path='README.md'",
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
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
            ))
        },
    )?;
    assert_eq!(kind, "markdown_section");
    assert_eq!(name, None);
    assert_eq!(symbols, "");
    let start = usize::try_from(start)?;
    let end = usize::try_from(end)?;
    assert_eq!(content.as_bytes(), &markdown[start..end]);
    assert_eq!(title, "Unified Guide");
    assert_eq!(breadcrumb, "Start");
    assert_eq!(nearest_heading.as_deref(), Some("Start"));
    assert_eq!(ordinal, 0);
    assert!(embedding_identity.is_some());
    assert_eq!(front_matter_state, "valid");

    let fts: (String, String, String, String, String) = conn.query_row(
        "SELECT title, metadata, breadcrumb, body, path
         FROM docs_fts WHERE rowid=?1",
        [chunk_id],
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
    assert_eq!(
        fts,
        (
            "Unified Guide".to_owned(),
            "shared database alpha beta".to_owned(),
            "Start".to_owned(),
            "Use docsMarker now.".to_owned(),
            "README.md".to_owned(),
        )
    );
    let docs_match: i64 = conn.query_row(
        "SELECT count(*) FROM docs_fts WHERE docs_fts MATCH 'docsMarker'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(docs_match, 1);
    let code_pollution: i64 = conn.query_row(
        "SELECT count(*)
         FROM chunks_fts
         JOIN chunks chunk ON chunk.id=chunks_fts.rowid
         WHERE EXISTS (
           SELECT 1 FROM doc_chunk_meta doc WHERE doc.chunk_id=chunk.id
         )",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(code_pollution, 0);
    for table in ["symbols", "imports", "exports", "refs", "events"] {
        let query = format!(
            "SELECT count(*) FROM {table} item
             JOIN files file ON file.id=item.file_id
             WHERE file.path='README.md'"
        );
        let rows: i64 = conn.query_row(&query, [], |row| row.get(0))?;
        assert_eq!(rows, 0, "documentation leaked into {table}");
    }
    let documentation_file_id: i64 =
        conn.query_row("SELECT id FROM files WHERE path='README.md'", [], |row| {
            row.get(0)
        })?;
    let code_file_id: i64 =
        conn.query_row("SELECT id FROM files WHERE path='main.ts'", [], |row| {
            row.get(0)
        })?;
    let module_graph = query::ModuleGraph::load(&conn)?;
    assert!(module_graph.paths.contains_key(&code_file_id));
    assert!(
        !module_graph.paths.contains_key(&documentation_file_id),
        "documentation must not enter the code module-graph inventory"
    );
    let module_edges: i64 = conn.query_row(
        "SELECT count(*) FROM module_edges
         WHERE from_file=?1 OR to_file=?1",
        [documentation_file_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        module_edges, 0,
        "Markdown must not be a module-resolution importer or target"
    );
    let structural_nodes: i64 = conn.query_row(
        "SELECT count(*) FROM graph_nodes WHERE file_id=?1",
        [documentation_file_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        structural_nodes, 0,
        "Markdown must not enter the structural graph"
    );
    let structural_edges: i64 = conn.query_row(
        "SELECT count(*)
         FROM resolved_edges edge
         JOIN graph_nodes node
           ON node.node_key=edge.src_key OR node.node_key=edge.dst_key
         WHERE node.file_id=?1",
        [documentation_file_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        structural_edges, 0,
        "Markdown must not contribute structural edges"
    );
    let indexed_decision: i64 = conn.query_row(
        "SELECT count(*) FROM doc_inventory
         WHERE path='README.md' AND subject='file' AND rule='indexed'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_decision, 1);
    Ok(())
}

#[test]
fn mdx_uses_the_docs_corpus_without_entering_code_surfaces() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export const codeOnlyNeedle = 1;\n",
    )?;
    fs::write(
        repo.path().join("guide.mdx"),
        concat!(
            "import PreambleOnlyNeedle from './preamble-only'\n",
            "export const preambleMetadataNeedle = { title: 'Guide' }\n\n",
            "# Component guide {/* headingCommentNeedle */}\n\n",
            "<Widget mode=\"safe\">ActualInnerNeedle</Widget>\n\n",
            "<Badge label=\"Deprecated\" since={version} />\n\n",
            "Visible before {/* jsxCommentNeedle */} visible after.\n\n",
            "`{/* protectedCommentNeedle */}`\n\n",
            "export const mdxOnlyNeedle = 'documentation text';\n",
        ),
    )?;
    let conn = store::open(repo.path())?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!((outcome.indexed, outcome.rejected), (2, 0));
    let identity: (String, String, String) = conn.query_row(
        "SELECT corpus, format, role FROM files WHERE path='guide.mdx'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        identity,
        ("docs".into(), "mdx".into(), "documentation".into())
    );
    let inert_chunk: (String, Option<String>, String) = conn.query_row(
        "SELECT chunk.kind, chunk.name, chunk.symbols
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.mdx' AND chunk.content LIKE '%mdxOnlyNeedle%'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        inert_chunk,
        ("markdown_section".into(), None, String::new())
    );

    let raw_comment_rows: i64 = conn.query_row(
        "SELECT count(*)
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.mdx' AND chunk.content LIKE '%jsxCommentNeedle%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(raw_comment_rows, 1, "raw source slice lost the JSX comment");
    let retrieval_chunks: i64 = conn.query_row(
        "SELECT count(*)
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.mdx'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retrieval_chunks, 1, "ESM preamble became a retrieval unit");
    for (query, expected) in [
        ("PreambleOnlyNeedle", 0_i64),
        ("preambleMetadataNeedle", 0),
        ("headingCommentNeedle", 0),
        ("jsxCommentNeedle", 0),
        ("ActualInnerNeedle", 1),
        ("Badge", 1),
        ("label", 1),
        ("Deprecated", 1),
        ("version", 1),
        ("protectedCommentNeedle", 1),
        ("\"Badge label Deprecated\"", 1),
    ] {
        let rows: i64 = conn.query_row(
            "SELECT count(*) FROM docs_fts WHERE docs_fts MATCH ?1",
            [query],
            |row| row.get(0),
        )?;
        assert_eq!(rows, expected, "docs FTS query {query}");
    }

    let snapshot = structural::current_snapshot(&conn)?;
    let docs_hits = docs::store::lexical_search(&conn, &snapshot, "mdxOnlyNeedle", 10)?;
    assert_eq!(docs_hits.len(), 1);
    assert_eq!(docs_hits[0].path, "guide.mdx");
    assert!(
        search::search(
            &conn,
            None,
            "mdxOnlyNeedle",
            &search::SearchOptions::default(),
        )?
        .hits
        .is_empty(),
        "MDX prose entered ordinary code search"
    );
    let code_fts_rows: i64 = conn.query_row(
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'mdxOnlyNeedle'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(code_fts_rows, 0);
    let graph_rows: i64 = conn.query_row(
        "SELECT count(*)
         FROM graph_nodes node JOIN files file ON file.id=node.file_id
         WHERE file.path='guide.mdx'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(graph_rows, 0);
    for table in ["symbols", "imports", "exports", "refs", "events"] {
        let query = format!(
            "SELECT count(*) FROM {table} item
             JOIN files file ON file.id=item.file_id
             WHERE file.path='guide.mdx'"
        );
        let rows: i64 = conn.query_row(&query, [], |row| row.get(0))?;
        assert_eq!(rows, 0, "MDX leaked into {table}");
    }
    Ok(())
}

#[test]
fn ordinary_embedding_never_requests_mdx_documents() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("guide.mdx"),
        "# Guide\n\nProviderCallSentinel appears only in MDX.\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let provider = embed::Provider::from_settings(
        &crate::config::EmbeddingSettings {
            provider: Some("openai".into()),
            model: Some("unreachable-test-model".into()),
            revision: None,
            url: Some("http://127.0.0.1:1/v1/embeddings".into()),
            api_key_env: None,
            query_prefix: None,
            batch: 8,
            origins: origin::defaults(),
        },
        &crate::config::InferenceSettings {
            url: "http://127.0.0.1:1/".into(),
            host: "127.0.0.1".into(),
            port: 1,
            project: None,
            uv: "uv".into(),
            allow_remote: false,
            batch_size: 8,
            max_length: 4_096,
            model_cache_root: None,
        },
    )?
    .expect("configured provider");

    // The endpoint is deliberately unreachable. Any accidental MDX
    // selection would turn this into a provider-call failure.
    let report = embed::embed_missing_for_selection_report(
        &conn,
        &provider,
        8,
        &origin::defaults(),
        false,
        false,
    )?;

    assert_eq!(
        (report.missing, report.embedded, report.occurrences_synced),
        (0, 0, 0)
    );
    Ok(())
}

#[test]
fn empty_documentation_policy_removes_the_prior_docs_corpus() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const retained = 1;\n")?;
    fs::write(repo.path().join("guide.md"), "# Markdown\n\nRemove me.\n")?;
    fs::write(repo.path().join("guide.mdx"), "# MDX\n\nRemove me too.\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let initial_docs: i64 = conn.query_row(
        "SELECT count(*) FROM files WHERE corpus='docs'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(initial_docs, 2);

    let disabled = index_repo_with_options(
        repo.path(),
        &conn,
        &IndexOptions {
            docs_include: Vec::new(),
            docs_exclude: Vec::new(),
            ..Default::default()
        },
    )?;

    assert_eq!(
        (disabled.indexed, disabled.unchanged, disabled.removed),
        (0, 1, 2)
    );
    let docs_state: (i64, i64, i64, i64) = conn.query_row(
        "SELECT
           (SELECT count(*) FROM files WHERE corpus='docs'),
           (SELECT count(*) FROM doc_chunk_meta),
           (SELECT count(*) FROM docs_fts),
           (SELECT count(*) FROM doc_inventory)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(docs_state, (0, 0, 0, 0));
    let retained_code: i64 = conn.query_row(
        "SELECT count(*) FROM files WHERE path='main.ts' AND corpus='code'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retained_code, 1);
    Ok(())
}

#[test]
fn incremental_index_repairs_explicit_corpus_and_format_mismatches() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
    fs::write(repo.path().join("README.md"), "# Guide\n\nCurrent docs.\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    conn.execute(
        "UPDATE files SET corpus='docs', format='javascript' WHERE path='main.ts'",
        [],
    )?;
    conn.execute(
        "UPDATE files SET format='plain_text' WHERE path='README.md'",
        [],
    )?;

    let repaired = index_repo(repo.path(), &conn)?;
    assert_eq!((repaired.indexed, repaired.unchanged), (2, 0));
    let identities = conn
        .prepare("SELECT path, corpus, format FROM files ORDER BY path")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        identities,
        [
            ("README.md".into(), "docs".into(), "markdown".into()),
            ("main.ts".into(), "code".into(), "typescript".into()),
        ]
    );
    Ok(())
}

#[test]
fn incremental_index_repairs_same_hash_mdx_format_mismatches() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("guide.mdx"),
        "# Guide\n\nStable MDX body.\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let original_hash: String =
        conn.query_row("SELECT hash FROM files WHERE path='guide.mdx'", [], |row| {
            row.get(0)
        })?;
    conn.execute(
        "UPDATE files SET format='markdown' WHERE path='guide.mdx'",
        [],
    )?;

    let repaired = index_repo(repo.path(), &conn)?;

    assert_eq!((repaired.indexed, repaired.unchanged), (1, 0));
    let repaired_identity: (String, String, String) = conn.query_row(
        "SELECT hash, corpus, format FROM files WHERE path='guide.mdx'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        repaired_identity,
        (original_hash, "docs".into(), "mdx".into())
    );
    Ok(())
}

#[test]
fn markdown_sidecar_persists_same_heading_ordinals_not_global_chunk_order() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("ordinals.md"),
        "# Repeated\n\nFirst.\n\n---\n\nSecond.\n\n# Repeated\n\nThird.\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    let ordinals = conn
        .prepare(
            "SELECT metadata.ordinal
             FROM doc_chunk_meta metadata
             JOIN chunks chunk ON chunk.id=metadata.chunk_id
             JOIN files file ON file.id=chunk.file_id
             WHERE file.path='ordinals.md'
             ORDER BY chunk.start, chunk.end, chunk.id",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(ordinals, [0, 1, 0]);
    Ok(())
}

#[test]
fn markdown_membership_and_replacement_share_the_normal_index_lifecycle() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let admitted = repo.path().join("guide.md");
    fs::write(&admitted, "# Guide\n\nFirst body.\n")?;
    fs::write(repo.path().join("draft.md"), "# Draft\n\nDo not index.\n")?;
    let conn = store::open(repo.path())?;
    let options = IndexOptions {
        docs_exclude: vec!["draft.md".to_owned()],
        ..IndexOptions::default()
    };

    let first = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!((first.indexed, first.unchanged, first.removed), (1, 0, 0));
    let excluded: i64 = conn.query_row(
        "SELECT count(*) FROM doc_inventory
         WHERE path='draft.md' AND rule='excluded'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(excluded, 1);
    let absent: i64 = conn.query_row(
        "SELECT count(*) FROM files WHERE path='draft.md'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(absent, 0);

    let second = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!((second.indexed, second.unchanged), (0, 1));
    fs::write(&admitted, "# Guide\n\nReplacement body.\n")?;
    let replaced = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!(
        (replaced.indexed, replaced.unchanged, replaced.removed),
        (1, 0, 0)
    );
    let replacement: String = conn.query_row(
        "SELECT docs_fts.body FROM docs_fts
         JOIN chunks chunk ON chunk.id=docs_fts.rowid
         JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.md'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(replacement, "Replacement body.");

    fs::remove_file(admitted)?;
    let removed = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!(
        (removed.indexed, removed.unchanged, removed.removed),
        (0, 0, 1)
    );
    let doc_rows: i64 =
        conn.query_row("SELECT count(*) FROM doc_chunk_meta", [], |row| row.get(0))?;
    let docs_fts_rows: i64 =
        conn.query_row("SELECT count(*) FROM docs_fts", [], |row| row.get(0))?;
    assert_eq!((doc_rows, docs_fts_rows), (0, 0));
    Ok(())
}

#[test]
fn reports_the_file_and_stage_for_rejected_reads() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("bad.ts"), [0xff, 0xfe])?;
    let conn = store::open(repo.path())?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!(outcome.rejected, 1);
    assert_eq!(outcome.rejections.len(), 1);
    assert_eq!(outcome.rejections[0].path, "bad.ts");
    assert_eq!(outcome.rejections[0].stage, "read");
    assert!(!outcome.rejections[0].error.is_empty());
    assert!(!structural::current_snapshot(&conn)?.is_empty());
    Ok(())
}

#[test]
fn file_disappearance_after_inventory_is_a_removal_not_a_retry() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let retained = repo.path().join("retained.ts");
    let vanished = repo.path().join("vanished.ts");
    fs::write(&retained, "export const retained = 1;\n")?;
    fs::write(&vanished, "export const vanished = 1;\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    let fault_fs = FaultFileSystem::default();
    fault_fs.fail(
        vanished.canonicalize()?,
        std::io::Error::from(ErrorKind::NotFound),
    );
    let outcome = index_repo_with_fs(repo.path(), &conn, &fault_fs)?;

    assert_eq!(outcome.rejected, 0);
    assert_eq!(outcome.removed, 1);
    let paths = conn
        .prepare("SELECT path FROM files ORDER BY path")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(paths, vec!["retained.ts"]);
    Ok(())
}

#[test]
fn retryable_source_read_preserves_the_published_snapshot() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = repo.path().join("main.ts");
    let before = "export const before = 1;\n";
    fs::write(&source, before)?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let old_snapshot = structural::current_snapshot(&conn)?;

    fs::write(&source, "export const after = 2;\n")?;
    #[cfg(unix)]
    let transient_error = std::io::Error::from_raw_os_error(libc::EMFILE);
    #[cfg(not(unix))]
    let transient_error = std::io::Error::from(ErrorKind::Interrupted);
    let fault_fs = FaultFileSystem::default();
    fault_fs.fail(source.canonicalize()?, transient_error);
    let error = index_repo_with_fs(repo.path(), &conn, &fault_fs)
        .err()
        .expect("retryable source read must abort preparation");

    assert!(error.to_string().contains("retryable read failure"));
    let retained_hash: String =
        conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        retained_hash,
        blake3::hash(before.as_bytes()).to_hex().to_string()
    );
    assert_eq!(structural::current_snapshot(&conn)?, old_snapshot);
    Ok(())
}

#[test]
fn failure_after_canonical_replacement_restores_the_last_good_publication() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = repo.path().join("main.ts");
    let guide = repo.path().join("guide.md");
    let code_before = "export const beforeCode = 1;\n";
    let docs_before = "# Guide\n\nBefore documentation.\n";
    fs::write(&source, code_before)?;
    fs::write(&guide, docs_before)?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    let old_snapshot = structural::current_snapshot(&conn)?;
    let old_rows = conn
        .prepare("SELECT path, hash FROM files ORDER BY path")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let old_doc_body: String = conn.query_row("SELECT body FROM docs_fts", [], |row| row.get(0))?;

    fs::write(&source, "export const afterCode = 2;\n")?;
    fs::write(&guide, "# Guide\n\nAfter documentation.\n")?;
    let error = index_repo_with_post_replacement_failure(repo.path(), &conn)
        .err()
        .expect("the post-replacement failure seam must abort publication");

    assert!(
        error
            .to_string()
            .contains("injected failure after canonical replacement")
    );
    assert!(
        conn.is_autocommit(),
        "failed refresh left a transaction open"
    );
    assert_eq!(structural::current_snapshot(&conn)?, old_snapshot);
    let retained_rows = conn
        .prepare("SELECT path, hash FROM files ORDER BY path")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(retained_rows, old_rows);
    let retained_code: String = conn.query_row(
        "SELECT chunk.content
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='main.ts' AND chunk.kind='module'",
        [],
        |row| row.get(0),
    )?;
    assert!(retained_code.contains("beforeCode"));
    assert!(!retained_code.contains("afterCode"));
    let retained_doc_body: String =
        conn.query_row("SELECT body FROM docs_fts", [], |row| row.get(0))?;
    assert_eq!(retained_doc_body, old_doc_body);
    assert_eq!(retained_doc_body, "Before documentation.");
    Ok(())
}

#[test]
fn documentation_contract_change_rechunks_docs_and_rotates_the_shared_snapshot() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export const unchangedCode = 1;\n",
    )?;
    fs::write(repo.path().join("guide.md"), "# Guide\n\nCurrent body.\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    let code_chunk_id: i64 = conn.query_row(
        "SELECT chunk.id
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='main.ts' AND chunk.kind='module'",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE doc_chunk_meta SET nearest_heading='stale-format-marker'",
        [],
    )?;
    conn.execute(
        "UPDATE meta SET value='documentation-v0'
         WHERE key='documentation_chunk_format_version'",
        [],
    )?;
    let old_format_snapshot = structural::compute_snapshot(&conn)?;
    conn.execute(
        "UPDATE meta SET value=?1 WHERE key='snapshot'",
        [&old_format_snapshot],
    )?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!((outcome.indexed, outcome.unchanged), (1, 1));
    let retained_code_chunk_id: i64 = conn.query_row(
        "SELECT chunk.id
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='main.ts' AND chunk.kind='module'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retained_code_chunk_id, code_chunk_id);
    let nearest_heading: String =
        conn.query_row("SELECT nearest_heading FROM doc_chunk_meta", [], |row| {
            row.get(0)
        })?;
    assert_eq!(nearest_heading, "Guide");
    let persisted_format: String = conn.query_row(
        "SELECT value FROM meta WHERE key='documentation_chunk_format_version'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(persisted_format, crate::docs::CHUNK_FORMAT_VERSION);
    assert_ne!(structural::current_snapshot(&conn)?, old_format_snapshot);
    Ok(())
}

#[cfg(unix)]
#[test]
fn previously_indexed_unreadable_subtree_reports_removal_magnitude() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir()?;
    let locked = repo.path().join("locked");
    fs::create_dir_all(&locked)?;
    fs::write(repo.path().join("good.ts"), "export const good = 1;\n")?;
    fs::write(locked.join("first.ts"), "export const first = 1;\n")?;
    fs::write(locked.join("second.ts"), "export const second = 2;\n")?;
    let conn = store::open(repo.path())?;
    let initial = index_repo(repo.path(), &conn)?;
    assert_eq!(
        (initial.indexed, initial.removed, initial.rejected),
        (3, 0, 0)
    );

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;
    let result = index_repo(repo.path(), &conn);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700))?;
    let outcome = result?;

    assert_eq!(outcome.indexed, 0);
    assert_eq!(outcome.unchanged, 1);
    assert_eq!(outcome.removed, 2);
    assert_eq!(outcome.rejected, 1);
    assert_eq!(outcome.rejections[0].path, "locked");
    assert_eq!(outcome.rejections[0].stage, "walk");
    let paths = conn
        .prepare("SELECT path FROM files ORDER BY path")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(paths, vec!["good.ts"]);
    assert!(!structural::current_snapshot(&conn)?.is_empty());
    Ok(())
}

#[test]
fn workspace_boundary_rejections_remain_visible_while_sources_index() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("package.json"),
        r#"{"workspaces":["packages/*"]}"#,
    )?;
    fs::create_dir_all(repo.path().join("packages/broken/src"))?;
    fs::write(
        repo.path().join("packages/broken/package.json"),
        "{ not-json",
    )?;
    fs::write(
        repo.path().join("packages/broken/src/index.ts"),
        "export const indexed = 1;\n",
    )?;
    let conn = store::open(repo.path())?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!(outcome.indexed, 1);
    assert!(outcome.rejections.iter().any(|rejection| {
        rejection.path == "packages/broken/package.json" && rejection.stage == "workspace-manifest"
    }));
    Ok(())
}

#[test]
fn full_refresh_preserves_source_less_workspace_identities() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join(".git"))?;
    fs::write(
        repo.path().join("package.json"),
        r#"{"workspaces":["packages/*"]}"#,
    )?;
    fs::write(repo.path().join(".gitignore"), "packages/ignored/src/\n")?;
    fs::create_dir_all(repo.path().join("packages/dist-only/dist"))?;
    fs::write(
        repo.path().join("packages/dist-only/package.json"),
        r#"{"name":"dist-only","main":"dist/index.js"}"#,
    )?;
    fs::write(
        repo.path().join("packages/dist-only/dist/index.js"),
        "module.exports = 1;\n",
    )?;
    fs::create_dir_all(repo.path().join("packages/ignored/src"))?;
    fs::write(
        repo.path().join("packages/ignored/package.json"),
        r#"{"name":"ignored-source","main":"src/index.ts"}"#,
    )?;
    fs::write(
        repo.path().join("packages/ignored/src/index.ts"),
        "export const ignored = true;\n",
    )?;
    fs::write(repo.path().join("main.ts"), "export const main = true;\n")?;
    let conn = store::open(repo.path())?;

    refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

    let workspace_names = conn
        .prepare(
            "SELECT name FROM package_instances
             WHERE origin='workspace' ORDER BY name",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(workspace_names, vec!["dist-only", "ignored-source"]);
    Ok(())
}

#[test]
fn indexes_js_files_containing_jsx() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("page.js"),
        "export default function Page() { return <main>Hello</main>; }\n",
    )?;
    let conn = store::open(repo.path())?;

    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!((outcome.indexed, outcome.rejected), (1, 0));
    let identity: (String, String) = conn.query_row(
        "SELECT corpus, format FROM files WHERE path='page.js'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(identity, ("code".into(), "javascript".into()));
    let chunks: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))?;
    assert!(chunks > 0);
    Ok(())
}

#[test]
fn extraction_version_change_forces_unchanged_files_through_extraction() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export function run() { return process.env.API_KEY; }\n",
    )?;
    let conn = store::open(repo.path())?;
    let first = index_repo(repo.path(), &conn)?;
    assert_eq!(first.indexed, 1);
    let second = index_repo(repo.path(), &conn)?;
    assert_eq!((second.indexed, second.unchanged), (0, 1));
    let current_contract_snapshot = structural::current_snapshot(&conn)?;

    conn.execute(
        "UPDATE meta SET value='legacy' WHERE key='extraction_version'",
        [],
    )?;
    let legacy_contract_snapshot = structural::compute_snapshot(&conn)?;
    assert_ne!(legacy_contract_snapshot, current_contract_snapshot);
    conn.execute(
        "UPDATE meta SET value=?1 WHERE key='snapshot'",
        [&legacy_contract_snapshot],
    )?;
    let third = index_repo(repo.path(), &conn)?;
    assert_eq!((third.indexed, third.unchanged), (1, 0));
    assert_ne!(
        structural::current_snapshot(&conn)?,
        legacy_contract_snapshot,
        "an extraction-contract refresh must rotate the published snapshot"
    );
    let extraction_version: String = conn.query_row(
        "SELECT value FROM meta WHERE key='extraction_version'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(extraction_version, crate::entity::EXTRACTION_VERSION);
    let environment_occurrences: i64 = conn.query_row(
        "SELECT count(*) FROM entity_occurrences occurrence
         JOIN entities entity ON entity.id=occurrence.entity_id
         WHERE entity.plane='general' AND entity.entity_type='environment_variable'
           AND entity.name='API_KEY'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(environment_occurrences, 1);
    Ok(())
}

#[test]
fn unresolved_non_package_imports_carry_no_package_identity() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("style.module.scss"),
        ".a { color: red; }\n",
    )?;
    fs::write(
        repo.path().join("main.ts"),
        "import styles from './style.module.scss';\n\
         import Tree from './Tree.vue';\n\
         import cover from '~/assets/cover.png';\n\
         import app from '@/components/app';\n\
         import internal from '#internal/widget';\n\
         import icon from 'C:\\\\assets\\\\icon.svg';\n\
         import missing from 'not-installed-pkg';\n\
         import scoped from '@scope/not-installed/subpath';\n\
         export const view = () => [styles, Tree, cover, app, internal, icon, missing, scoped];\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    let edge = |request: &str| -> Result<(Option<i64>, Option<String>, Option<String>)> {
        Ok(conn.query_row(
            "SELECT edge.to_file, edge.package, edge.resolution
             FROM module_edges edge
             JOIN files source ON source.id=edge.from_file
             WHERE source.path='main.ts' AND edge.request=?1",
            [request],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    };
    // Resolved to a real but un-indexable asset, and not resolvable at
    // all: both keep the edge as evidence with no package identity.
    assert_eq!(
        edge("./style.module.scss")?,
        (None, None, Some("unresolved".into()))
    );
    assert_eq!(edge("./Tree.vue")?, (None, None, Some("unresolved".into())));
    // Bundler aliases, package-import aliases, and Windows paths are not
    // installable package identities.
    assert_eq!(
        edge("~/assets/cover.png")?,
        (None, None, Some("unresolved".into()))
    );
    assert_eq!(
        edge("@/components/app")?,
        (None, None, Some("unresolved".into()))
    );
    assert_eq!(
        edge("#internal/widget")?,
        (None, None, Some("unresolved".into()))
    );
    assert_eq!(
        edge(r"C:\assets\icon.svg")?,
        (None, None, Some("unresolved".into()))
    );
    // Bare specifiers stay classified as external packages.
    assert_eq!(
        edge("not-installed-pkg")?,
        (None, Some("not-installed-pkg".into()), None)
    );
    assert_eq!(
        edge("@scope/not-installed/subpath")?,
        (None, Some("@scope/not-installed".into()), None)
    );

    let bogus_packages: i64 = conn.query_row(
        "SELECT count(*) FROM graph_nodes
         WHERE node_key LIKE 'pkg:.%' OR node_key LIKE 'pkg:/%'
            OR node_key LIKE 'pkg:~%' OR node_key LIKE 'pkg:@/%'
            OR node_key LIKE 'pkg:#%' OR node_key LIKE 'pkg:C:%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        bogus_packages, 0,
        "relative requests must not mint pkg: nodes"
    );
    let package_hub: i64 = conn.query_row(
        "SELECT count(*) FROM graph_nodes WHERE node_key='pkg:not-installed-pkg'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(package_hub, 1);
    let unresolved_projected: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE kind IN ('import','imports_types','imports_package','imports_package_types')
           AND (detail_json LIKE '%Tree.vue%' OR detail_json LIKE '%scss%')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(unresolved_projected, 0);
    let dangling: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges edge
         LEFT JOIN graph_nodes node ON node.node_key=edge.dst_key
         WHERE node.node_key IS NULL",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(dangling, 0);
    Ok(())
}

#[test]
fn noop_reindex_republishes_projection_without_rebuild() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("lib.ts"), "export const lib = 1;\n")?;
    fs::write(
        repo.path().join("main.ts"),
        "import { lib } from 'lib';\nexport const main = () => lib;\n",
    )?;
    let conn = store::open(repo.path())?;
    let meta_snapshot = || -> Result<String> {
        Ok(
            conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
                row.get(0)
            })?,
        )
    };
    let edge_count = || -> Result<i64> {
        Ok(conn.query_row("SELECT count(*) FROM resolved_edges", [], |row| row.get(0))?)
    };

    let first = index_repo(repo.path(), &conn)?;
    assert!(first.projection_rebuilt);
    let original_snapshot = meta_snapshot()?;
    let original_edges = edge_count()?;
    let original_neighborhood = structural::neighborhood(
        &conn,
        "main.ts:main",
        &structural::NeighborhoodOptions::default(),
    )?;

    let second = index_repo(repo.path(), &conn)?;
    assert!(!second.projection_rebuilt, "no-op must keep the projection");
    assert_eq!(meta_snapshot()?, original_snapshot);
    assert_eq!(edge_count()?, original_edges);

    // Resolution inputs live outside indexed content: a new tsconfig
    // remaps 'lib' onto ./lib.ts without changing any indexed file. The
    // graph and its public snapshot must both change.
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions": {"paths": {"lib": ["./lib.ts"]}}}"#,
    )?;
    let third = index_repo(repo.path(), &conn)?;
    assert!(
        third.projection_rebuilt,
        "resolution change without content change must rebuild"
    );
    assert_ne!(meta_snapshot()?, original_snapshot);
    let updated_neighborhood = structural::neighborhood(
        &conn,
        &original_neighborhood.resolved_anchor,
        &structural::NeighborhoodOptions {
            expected_snapshot: Some(original_neighborhood.snapshot),
            ..Default::default()
        },
    )?;
    assert_eq!(updated_neighborhood.anchor_status, "re-resolved");
    let target: Option<String> = conn.query_row(
        "SELECT target.path FROM module_edges edge
         JOIN files source ON source.id=edge.from_file
         LEFT JOIN files target ON target.id=edge.to_file
         WHERE source.path='main.ts' AND edge.request='lib'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(target, Some("lib.ts".into()));
    let fourth = index_repo(repo.path(), &conn)?;
    assert!(!fourth.projection_rebuilt);

    fs::write(repo.path().join("lib.ts"), "export const lib = 2;\n")?;
    let fifth = index_repo(repo.path(), &conn)?;
    assert!(fifth.projection_rebuilt, "content change must rebuild");
    assert_ne!(meta_snapshot()?, original_snapshot);

    fs::remove_file(repo.path().join("main.ts"))?;
    let sixth = index_repo(repo.path(), &conn)?;
    assert!(sixth.projection_rebuilt, "deletion must rebuild");
    let seventh = index_repo(repo.path(), &conn)?;
    assert!(!seventh.projection_rebuilt);
    Ok(())
}

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
    fs::write(
        app.join("src/helper.ts"),
        "export const helper = () => 1;\n",
    )?;

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
    fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;

    // Library package: main points at untracked dist output, but the
    // module field names the source entry directly (manifest truth).
    let lib = repo.path().join("packages/lib");
    fs::create_dir_all(lib.join("src/utils"))?;
    fs::write(
        lib.join("package.json"),
        r#"{"name": "@acme/lib", "main": "dist/index.js", "module": "src/index.ts"}"#,
    )?;
    fs::write(
        lib.join("src/index.ts"),
        "export const greet = () => 'hi';\n",
    )?;
    fs::write(
        lib.join("src/utils/format.ts"),
        "export const fmt = (s: string) => s;\n",
    )?;

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
    fs::write(
        tools.join("src/inner/scrub.ts"),
        "export const scrub = (s: string) => s;\n",
    )?;

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
        (
            Some("packages/lib/src/index.ts".into()),
            None,
            Some("workspace".into())
        )
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
    assert_eq!(
        fmt_call,
        ("likely".into(), "semantic+resolver-inferred".into())
    );
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
    fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;

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
    fs::write(
        app.join("src/helper.ts"),
        "export const helper = () => 1;\n",
    )?;
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
    assert_eq!(
        edge("./helper")?,
        (Some("packages/app/src/helper.ts".into()), None)
    );
    assert_eq!(
        edge("@acme/lib")?,
        (Some("packages/lib/src/index.ts".into()), None)
    );
    Ok(())
}

#[test]
fn workspace_ownership_uses_literal_path_prefixes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/my_pkg\n",
    )?;
    let workspace = repo.path().join("packages/my_pkg");
    fs::create_dir_all(workspace.join("src"))?;
    fs::write(
        workspace.join("package.json"),
        r#"{"name":"my-pkg","version":"1.0.0"}"#,
    )?;
    fs::write(workspace.join("src/index.ts"), "export const owned = 1;\n")?;
    let sibling = repo.path().join("packages/my1pkg/src");
    fs::create_dir_all(&sibling)?;
    fs::write(sibling.join("index.ts"), "export const sibling = 1;\n")?;

    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let origins: (String, String) = conn.query_row(
        "SELECT
           (SELECT origin FROM files WHERE path='packages/my_pkg/src/index.ts'),
           (SELECT origin FROM files WHERE path='packages/my1pkg/src/index.ts')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(origins, ("workspace".into(), "repository".into()));
    Ok(())
}

#[test]
fn indexes_scoped_dependency_selected_by_exact_name() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "import { scoped } from '@scope/pkg';\nexport const result = scoped();\n",
    )?;
    let dependency = repo.path().join("node_modules/@scope/pkg");
    fs::create_dir_all(&dependency)?;
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"@scope/pkg","version":"2.0.0","main":"index.js"}"#,
    )?;
    fs::write(
        dependency.join("index.js"),
        "export const scoped = () => 2;\n",
    )?;

    let conn = store::open(repo.path())?;
    index_repo_with_options(
        repo.path(),
        &conn,
        &IndexOptions {
            dependencies: vec!["@scope/pkg".into()],
            ..Default::default()
        },
    )?;
    let resolved: (String, String, String) = conn.query_row(
        "SELECT package.name, package.version, target.package_path
         FROM module_edges edge
         JOIN package_instances package ON package.id=edge.package_instance_id
         JOIN files target ON target.id=edge.to_file
         JOIN files source ON source.id=edge.from_file
         WHERE source.path='main.ts' AND edge.request='@scope/pkg'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        resolved,
        ("@scope/pkg".into(), "2.0.0".into(), "index.js".into())
    );
    Ok(())
}

#[test]
fn non_retryable_dependency_rejections_remove_stale_rows() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "import value from 'selected-dep';\nexport const result = value;\n",
    )?;
    let dependency = repo.path().join("node_modules/selected-dep");
    fs::create_dir_all(&dependency)?;
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"selected-dep","version":"1.0.0","main":"index.js"}"#,
    )?;
    let entry = dependency.join("index.js");
    fs::write(
        &entry,
        "export const dependencyStaleMarker = true;\nexport default 1;\n",
    )?;

    let conn = store::open(repo.path())?;
    let options = IndexOptions {
        dependencies: vec!["selected-dep".into()],
        ..Default::default()
    };
    index_repo_with_options(repo.path(), &conn, &options)?;

    fs::write(&entry, [0xff, 0xfe])?;
    let unreadable = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!(unreadable.rejected, 1);
    assert_eq!(unreadable.rejections[0].stage, "read");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM files WHERE origin='dependency'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'dependencyStaleMarker'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );

    fs::write(&entry, "export default 2;\n")?;
    index_repo_with_options(repo.path(), &conn, &options)?;
    fs::write(&entry, "export default function Broken() { return <main>")?;
    let unparseable = index_repo_with_options(repo.path(), &conn, &options)?;
    assert_eq!(unparseable.rejected, 1);
    assert_eq!(unparseable.rejections[0].stage, "extract");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM files WHERE origin='dependency'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    assert!(!structural::current_snapshot(&conn)?.is_empty());
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
        &IndexOptions {
            dependencies: selected.clone(),
            ..Default::default()
        },
    )?;
    assert_eq!(first.dependency_packages, 1);
    assert_eq!(first.dependency_files, 2);
    assert!(first.dependency_bytes > 0);

    let package: (String, String, String) = conn.query_row(
        "SELECT origin, name, version FROM package_instances WHERE name='selected-dep'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        package,
        ("dependency".into(), "selected-dep".into(), "1.2.3".into())
    );
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
        query::find_symbols_in_origins(&conn, "dependencyOnlyMarker", &origin::defaults(),)?
            .is_empty()
    );
    let dependency_definitions =
        query::find_symbols_in_origins(&conn, "dependencyOnlyMarker", &["dependency".into()])?;
    assert_eq!(dependency_definitions.len(), 1);
    assert_eq!(dependency_definitions[0].file_origin, "dependency");
    let first_party_anchor =
        structural::resolve_current_anchor_in_origins(&conn, "internal", &origin::defaults())?;
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
    assert!(
        default_boundary
            .nodes
            .iter()
            .any(|node| node.key == package_hub.0)
    );
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
        &IndexOptions {
            dependencies: selected,
            ..Default::default()
        },
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

/// Render one query with every rowid replaced by content identity, in a
/// total order over all output columns, so two databases built by
/// different code paths can be compared byte-for-byte.
fn dump_section(conn: &rusqlite::Connection, sql: &str) -> Result<String> {
    let columns = conn.prepare(sql)?.column_count();
    let order: Vec<String> = (1..=columns).map(|index| index.to_string()).collect();
    let wrapped = format!("SELECT * FROM ({sql}) ORDER BY {}", order.join(","));
    let mut stmt = conn.prepare(&wrapped)?;
    let mut rows = stmt.query([])?;
    let mut out = String::new();
    while let Some(row) = rows.next()? {
        for index in 0..columns {
            use rusqlite::types::ValueRef;
            match row.get_ref(index)? {
                ValueRef::Null => out.push_str("<null>"),
                ValueRef::Integer(value) => out.push_str(&value.to_string()),
                ValueRef::Real(value) => out.push_str(&value.to_string()),
                ValueRef::Text(value) => out.push_str(&String::from_utf8_lossy(value)),
                ValueRef::Blob(value) => {
                    let _ = write!(out, "<blob:{}>", value.len());
                }
            }
            out.push('\x1f');
        }
        out.push('\n');
    }
    Ok(out)
}

/// Every canonical and projected table, keyed by paths/keys/spans instead
/// of rowids, including the stable published snapshot identity.
fn canonical_dump(conn: &rusqlite::Connection) -> Result<Vec<(&'static str, String)>> {
    const SECTIONS: &[(&str, &str)] = &[
        (
            "counts",
            "SELECT (SELECT count(*) FROM chunks),
                    (SELECT count(*) FROM chunks_fts),
                    (SELECT count(*) FROM doc_chunk_meta),
                    (SELECT count(*) FROM docs_fts)",
        ),
        (
            "files",
            "SELECT f.path, f.hash, f.corpus, f.format, f.role, f.origin, f.package_path,
                    p.origin, p.name, p.version, p.canonical_root, p.locator,
                    p.manifest_hash, p.status
             FROM files f LEFT JOIN package_instances p ON p.id=f.package_instance_id",
        ),
        (
            "chunks",
            "SELECT f.path, c.kind, c.name, c.scope_chain, c.symbols, c.start, c.end,
                    c.start_line, c.end_line, c.hash, c.content
             FROM chunks c JOIN files f ON f.id=c.file_id",
        ),
        (
            "chunks_fts",
            "SELECT f.path, c.start, fts.content, fts.name, fts.symbols, fts.path
             FROM chunks_fts fts
             JOIN chunks c ON c.id=fts.rowid
             JOIN files f ON f.id=c.file_id",
        ),
        (
            "doc_inventory",
            "SELECT path, subject, rule, detail, path_base64, path_encoding
             FROM doc_inventory",
        ),
        (
            "doc_chunk_meta",
            "SELECT file.path, chunk.start, metadata.title,
                    metadata.description, metadata.tags_json,
                    metadata.breadcrumb, metadata.nearest_heading,
                    metadata.ordinal, metadata.embedding_identity,
                    metadata.front_matter_state
             FROM doc_chunk_meta metadata
             JOIN chunks chunk ON chunk.id=metadata.chunk_id
             JOIN files file ON file.id=chunk.file_id",
        ),
        (
            "docs_fts",
            "SELECT file.path, chunk.start, fts.title, fts.metadata,
                    fts.breadcrumb, fts.body, fts.path
             FROM docs_fts fts
             JOIN chunks chunk ON chunk.id=fts.rowid
             JOIN files file ON file.id=chunk.file_id",
        ),
        (
            "symbols",
            "SELECT f.path, s.name, s.kind, s.start, s.end, s.decl_start, s.decl_end,
                    s.scope_chain, s.line, s.exported
             FROM symbols s JOIN files f ON f.id=s.file_id",
        ),
        (
            "imports",
            "SELECT f.path, i.local_name, i.imported_name, i.request
             FROM imports i JOIN files f ON f.id=i.file_id",
        ),
        (
            "exports",
            "SELECT f.path, e.export_name, e.local_name, e.from_request, e.from_name
             FROM exports e JOIN files f ON f.id=e.file_id",
        ),
        (
            "contract_imports",
            "SELECT f.path, i.local_name, i.imported_name, i.request
             FROM contract_imports i JOIN files f ON f.id=i.file_id",
        ),
        (
            "contract_exports",
            "SELECT f.path, e.export_name, e.local_name, e.from_request, e.from_name
             FROM contract_exports e JOIN files f ON f.id=e.file_id",
        ),
        (
            "module_edges",
            "SELECT src.path, e.request, dst.path, e.package, e.resolution,
                    p.name, p.version, p.canonical_root, e.type_only
             FROM module_edges e
             JOIN files src ON src.id=e.from_file
             LEFT JOIN files dst ON dst.id=e.to_file
             LEFT JOIN package_instances p ON p.id=e.package_instance_id",
        ),
        (
            "refs",
            "SELECT f.path, c.start, r.start, r.line, r.kind, r.confidence,
                    r.target_request, r.target_name, r.local, r.detail
             FROM refs r
             JOIN files f ON f.id=r.file_id
             LEFT JOIN chunks c ON c.id=r.chunk_id",
        ),
        (
            "events",
            "SELECT f.path, c.start, e.line, e.role, e.name, e.method
             FROM events e
             JOIN files f ON f.id=e.file_id
             LEFT JOIN chunks c ON c.id=e.chunk_id",
        ),
        (
            "member_calls",
            "SELECT f.path, c.start, m.start, m.end, m.line, m.end_line,
                    m.prop, m.object, m.receiver, m.receiver_start, m.receiver_end,
                    m.property_start, m.property_end, m.receiver_unbound
             FROM member_calls m
             JOIN files f ON f.id=m.file_id
             LEFT JOIN chunks c ON c.id=m.chunk_id",
        ),
        (
            "receiver_value_flows",
            "SELECT f.path, v.call_start, v.call_end, v.receiver_kind,
                    v.class_name, v.class_start, v.value_kind, v.target_kind,
                    v.target_name, v.target_start
             FROM receiver_value_flows v JOIN files f ON f.id=v.file_id",
        ),
        (
            "function_return_flows",
            "SELECT f.path, v.function_name, v.function_start, v.function_async,
                    v.return_index, v.value_kind, v.target_kind, v.target_name,
                    v.target_start
             FROM function_return_flows v JOIN files f ON f.id=v.file_id",
        ),
        (
            "value_binding_flows",
            "SELECT f.path, v.binding_name, v.binding_start, v.value_kind,
                    v.target_kind, v.target_name, v.target_start
             FROM value_binding_flows v JOIN files f ON f.id=v.file_id",
        ),
        (
            "class_value_flows",
            "SELECT f.path, v.class_name, v.class_start, v.super_name,
                    v.super_start, v.super_kind
             FROM class_value_flows v JOIN files f ON f.id=v.file_id",
        ),
        (
            "instance_method_value_flows",
            "SELECT f.path, v.class_start, v.method_name, v.method_start
             FROM instance_method_value_flows v JOIN files f ON f.id=v.file_id",
        ),
        (
            "class_member_value_flow_blockers",
            "SELECT f.path, v.class_start, v.member_name
             FROM class_member_value_flow_blockers v JOIN files f ON f.id=v.file_id",
        ),
        (
            "entity_sites",
            "SELECT f.path, c.start, s.start, s.end, s.line, s.end_line, s.plane,
                    s.entity_type, s.role, s.identity_kind, s.identity_name,
                    s.identity_start, s.target_name, s.target_start, s.extractor,
                    s.provenance, s.confidence, s.detail_json
             FROM entity_sites s
             JOIN files f ON f.id=s.file_id
             LEFT JOIN chunks c ON c.id=s.chunk_id",
        ),
        (
            "entities",
            "SELECT entity_key, plane, entity_type, name, identity_anchor, meta_json
             FROM entities",
        ),
        (
            "entity_occurrences",
            "SELECT en.entity_key, f.path, site.start, o.start, o.end, o.line,
                    o.end_line, o.role, o.extractor, o.provenance, o.confidence,
                    o.detail_json
             FROM entity_occurrences o
             JOIN entities en ON en.id=o.entity_id
             JOIN entity_sites site ON site.id=o.site_id
             JOIN files f ON f.id=o.file_id",
        ),
        (
            // detail_json embeds occurrence/site rowid pointers, which
            // every full re-index reassigns; the join already pins the
            // same identity by content.
            "entity_edges",
            "SELECT en.entity_key, o.start, e.target_key, e.kind, e.confidence,
                    e.provenance,
                    json_remove(e.detail_json, '$.entityOccurrenceId', '$.entitySiteId')
             FROM entity_edges e
             JOIN entity_occurrences o ON o.id=e.occurrence_id
             JOIN entities en ON en.id=o.entity_id",
        ),
        (
            "graph_nodes",
            "SELECT n.node_key, n.node_kind, n.native_table, n.display_name,
                    f.path, n.line, n.meta_json
             FROM graph_nodes n LEFT JOIN files f ON f.id=n.file_id",
        ),
        (
            "resolved_edges",
            "SELECT e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                    f.path, e.line,
                    json_remove(e.detail_json, '$.entityOccurrenceId', '$.entitySiteId')
             FROM resolved_edges e LEFT JOIN files f ON f.id=e.source_file_id",
        ),
        ("scout_runs", "SELECT * FROM scout_runs"),
        (
            "scout_classifications",
            "SELECT * FROM scout_classifications",
        ),
        ("semantic_artifacts", "SELECT * FROM semantic_artifacts"),
        ("semantic_supports", "SELECT * FROM semantic_supports"),
        ("semantic_relations", "SELECT * FROM semantic_relations"),
        (
            "embeddings",
            "SELECT e.chunk_hash, p.provider, p.model, p.config_fingerprint, p.dimensions
             FROM embeddings e JOIN embedding_profiles p ON p.id=e.profile_id",
        ),
        ("meta", "SELECT key, value FROM meta"),
    ];
    SECTIONS
        .iter()
        .map(|(name, sql)| Ok((*name, dump_section(conn, sql)?)))
        .collect()
}

#[test]
fn full_refresh_rebuilds_snapshot_and_preserves_expensive_planes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export function run() { return 'stable'; }\n",
    )?;
    let conn = store::open(repo.path())?;
    let first = index_repo(repo.path(), &conn)?;
    assert_eq!((first.indexed, first.unchanged), (1, 0));

    let snapshot = structural::current_snapshot(&conn)?;
    let (chunk_hash, source_hash): (String, String) = conn.query_row(
        "SELECT chunk.hash, file.hash
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='main.ts' ORDER BY chunk.id LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let anchor = "file:main.ts";
    let context_hash = semantic::context_hash(&conn, anchor)?;

    // Durable cache and memory rows deliberately coexist with disposable
    // rows in one database.
    conn.execute_batch(
        "INSERT INTO embedding_profiles(
           id, provider, model, config_fingerprint, dimensions, config_json
         ) VALUES(1, 'test', 'tiny', 'test-profile', 2, '{}');
         INSERT INTO checker_enrichment_batches(
           id, source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(1, 'old', '5.9.3', 'test', 'checker-fp', 1,
                  '2026-01-01T00:00:00Z', 1);
         INSERT INTO package_instances(
           id, origin, name, canonical_root, locator, manifest_hash
         ) VALUES(99, 'dependency', 'obsolete', '/obsolete/package',
                  'obsolete@1', 'manifest');
         INSERT INTO scout_runs(
           id, scout_kind, status, gateway_protocol, provider, model,
           billing_path, prompt_version, source_snapshot,
           input_fingerprint, request_hash, started_at, completed_at
         ) VALUES(7, 'workflow', 'completed', 1, 'test', 'test-model',
                  'api', 'v1', 'old', 'memory-fp', 'request',
                  '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z');
         INSERT INTO scout_classifications(
           run_id, anchor_key, decision, role, evidence_json
         ) VALUES(7, 'file:main.ts', 'defining', 'entry', '{}');
         INSERT INTO semantic_artifacts(
           id, artifact_type, canonical_name, body_json, model,
           prompt_version, confidence, source_snapshot, created_at,
           scout_run_id, input_fingerprint, artifact_fingerprint
         ) VALUES(3, 'annotation', 'stable behavior', '{}', 'test-model',
                  'v1', 'likely', 'old', '2026-01-01T00:01:00Z', 7,
                  'memory-fp', 'artifact-fp');",
    )?;
    conn.execute(
        "UPDATE checker_enrichment_batches SET source_snapshot=?1 WHERE id=1",
        [&snapshot],
    )?;
    conn.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES(?1, 1, ?2)",
        rusqlite::params![chunk_hash, vec![0_u8; 8]],
    )?;
    conn.execute(
        "INSERT INTO semantic_supports(
           artifact_id, claim_path, anchor_key, role, evidence_file,
           evidence_start_line, evidence_end_line, source_hash,
           context_hash, confidence
         ) VALUES(3, '$', ?1, 'evidence', 'main.ts', 1, 1, ?2, ?3, 'likely')",
        rusqlite::params![anchor, source_hash, context_hash],
    )?;
    embed::materialize_cached_embeddings(&conn)?;
    assert_eq!(
        semantic::load_artifact(&conn, 3)?.unwrap().freshness,
        "fresh"
    );

    let refreshed = refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
    assert!(refreshed.extraction_reset);
    assert_eq!(
        (refreshed.indexed, refreshed.unchanged, refreshed.rejected),
        (1, 0, 0)
    );
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);

    let counts: (i64, i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT
           (SELECT count(*) FROM embedding_profiles),
           (SELECT count(*) FROM embeddings),
           (SELECT count(*) FROM embedding_index_entries),
           (SELECT count(*) FROM semantic_artifacts),
           (SELECT count(*) FROM checker_enrichment_batches),
           (SELECT count(*) FROM package_instances WHERE id=99)",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    assert_eq!(counts, (1, 1, 1, 1, 0, 0));
    assert_eq!(
        semantic::load_artifact(&conn, 3)?.unwrap().freshness,
        "fresh"
    );

    fs::write(
        repo.path().join("main.ts"),
        "export function run() { return 'changed'; }\n",
    )?;
    refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM checker_enrichment_batches",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
        "a checker batch must not survive a different structural snapshot"
    );
    Ok(())
}

#[test]
fn watcher_incremental_refresh_preserves_active_and_newest_staging_carry_sources() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    conn.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1, '5.9.3', 'test', 'checker-fp', 1,
                  '2026-01-01T00:00:00Z', 1)",
        [&snapshot],
    )?;
    let active_batch = conn.last_insert_rowid();
    conn.execute_batch(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, plan_fingerprint,
           created_at, active
         ) VALUES('superseded-a', '5.9.3', 'test', '', 1, 'plan-a',
                  '2026-01-02T00:00:00Z', 0);
         INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, plan_fingerprint,
           created_at, active
         ) VALUES('superseded-b', '5.9.3', 'test', '', 1, 'plan-b',
                  '2026-01-03T00:00:00Z', 0);",
    )?;
    let newest_staging = conn.last_insert_rowid();

    fs::write(repo.path().join("main.ts"), "export const value = 2;\n")?;
    incremental_refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

    let current = structural::current_snapshot(&conn)?;
    let retained = conn
        .prepare("SELECT id, active FROM checker_enrichment_batches ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        retained,
        vec![(active_batch, true), (newest_staging, false)]
    );
    assert_ne!(snapshot, current);
    Ok(())
}

#[test]
fn incremental_read_failure_removes_the_stale_file_row() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = repo.path().join("main.ts");
    fs::write(&source, "export const value = 1;\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    fs::write(&source, [0xff, 0xfe])?;
    let outcome =
        incremental_refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

    assert_eq!(outcome.rejected, 1);
    assert_eq!(outcome.removed, 1);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM files WHERE path='main.ts'",
            [],
            |row| { row.get::<_, i64>(0) }
        )?,
        0
    );
    Ok(())
}

#[test]
fn incremental_and_full_refresh_publish_the_same_structural_identity() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir(repo.path().join(".git"))?;
    fs::write(
        repo.path().join("main.ts"),
        "import { aliased } from '@alias';\n\
         import { stable } from './stable';\n\
         import { edited } from './edited';\n\
         import { removed } from './removed';\n\
         import { renamed } from './old-name';\n\
         export const total = aliased + stable + edited + removed + renamed;\n",
    )?;
    fs::write(
        repo.path().join("alias-a.ts"),
        "export const aliased = 10;\n",
    )?;
    fs::write(
        repo.path().join("alias-b.ts"),
        "export const aliased = 20;\n",
    )?;
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@alias":["alias-a.ts"]}}}"#,
    )?;
    fs::write(repo.path().join("stable.ts"), "export const stable = 1;\n")?;
    fs::write(repo.path().join("edited.ts"), "export const edited = 2;\n")?;
    fs::write(
        repo.path().join("removed.ts"),
        "export const removed = 3;\n",
    )?;
    fs::write(
        repo.path().join("old-name.ts"),
        "export const renamed = 4;\n",
    )?;
    fs::write(
        repo.path().join("guide.md"),
        "# Existing guide\n\nBefore documentation.\n",
    )?;
    fs::write(
        repo.path().join("removed.md"),
        "# Removed guide\n\nThis disappears.\n",
    )?;
    fs::write(repo.path().join(".gitignore"), "later-visible.ts\n")?;
    fs::write(
        repo.path().join("later-visible.ts"),
        "export const admittedAfterIgnoreChange = true;\n",
    )?;
    fs::write(
        repo.path().join("later-hidden.md"),
        "# Hidden later\n\nInitially admitted documentation.\n",
    )?;

    let incremental = store::open_path(&repo.path().join("incremental.db"))?;
    let full = store::open_path(&repo.path().join("full.db"))?;
    refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
    refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;

    fs::write(repo.path().join("edited.ts"), "export const edited = 20;\n")?;
    fs::remove_file(repo.path().join("removed.ts"))?;
    fs::rename(
        repo.path().join("old-name.ts"),
        repo.path().join("renamed.ts"),
    )?;
    fs::write(repo.path().join("added.ts"), "export const added = 5;\n")?;
    fs::write(
        repo.path().join("guide.md"),
        "# Existing guide\n\nAfter documentation.\n",
    )?;
    fs::remove_file(repo.path().join("removed.md"))?;
    fs::write(
        repo.path().join("added.mdx"),
        "# Added guide\n\nNew MDX documentation.\n",
    )?;
    fs::write(repo.path().join(".gitignore"), "later-hidden.md\n")?;
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@alias":["alias-b.ts"]}}}"#,
    )?;
    fs::write(
        repo.path().join("main.ts"),
        "import { aliased } from '@alias';\n\
         import { stable } from './stable';\n\
         import { edited } from './edited';\n\
         import { added } from './added';\n\
         import { renamed } from './renamed';\n\
         export const total = aliased + stable + edited + added + renamed;\n",
    )?;

    let incremental_outcome =
        incremental_refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
    let full_outcome = refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;
    assert_eq!(
        (
            incremental_outcome.indexed,
            incremental_outcome.unchanged,
            incremental_outcome.removed,
        ),
        (7, 3, 4)
    );
    assert_eq!((full_outcome.indexed, full_outcome.unchanged), (10, 0));

    let incremental_alias_target: Option<String> = incremental.query_row(
        "SELECT target.path FROM module_edges edge
         JOIN files source ON source.id=edge.from_file
         LEFT JOIN files target ON target.id=edge.to_file
         WHERE source.path='main.ts' AND edge.request='@alias'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(incremental_alias_target.as_deref(), Some("alias-b.ts"));

    let incremental_resolution = structural::compute_resolution_hash(&incremental)?;
    let full_resolution = structural::compute_resolution_hash(&full)?;
    let incremental_snapshot = structural::current_snapshot(&incremental)?;
    let full_snapshot = structural::current_snapshot(&full)?;
    assert_eq!(incremental_resolution, full_resolution);
    assert_eq!(incremental_snapshot, full_snapshot);
    assert_eq!(canonical_dump(&incremental)?, canonical_dump(&full)?);

    incremental.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1, '5.9.3', 'test', 'checker-fp', 1,
                  '2026-01-01T00:00:00Z', 1)",
        [&incremental_snapshot],
    )?;
    refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
    assert_eq!(structural::current_snapshot(&incremental)?, full_snapshot);
    assert_eq!(
        incremental.query_row(
            "SELECT count(*) FROM checker_enrichment_batches",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
        "manual full indexing always resets checker enrichment"
    );
    Ok(())
}

#[test]
fn identical_manual_full_refresh_clears_the_exact_checker_batch() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("service.ts"),
        "export class Service { load() {} }\n\
         export function run(service: Service) { service.load(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let (
        member_call_id,
        source_file_id,
        source_hash,
        call_start,
        call_end,
        receiver_start,
        receiver_end,
        property_start,
        property_end,
    ) = conn.query_row(
        "SELECT call.rowid, file.id, file.hash, call.start, call.end,
                call.receiver_start, call.receiver_end,
                call.property_start, call.property_end
         FROM member_calls call JOIN files file ON file.id=call.file_id
         WHERE call.prop='load'",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    )?;
    let (target, target_hash, target_start, target_end): (String, String, i64, i64) = conn
        .query_row(
            "SELECT node.node_key, file.hash, symbol.decl_start, symbol.decl_end
             FROM graph_nodes node
             JOIN symbols symbol
               ON node.native_table='symbols' AND node.native_id=symbol.id
             JOIN files file ON file.id=symbol.file_id
             WHERE node.display_name='load'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let target_fingerprint =
        crate::checker::target_fingerprint(&target, &target_hash, target_start, target_end);
    conn.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1,'5.9.3','test','inputs',1,datetime('now'),1)",
        [&snapshot],
    )?;
    let batch_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO checker_project_runs(
           batch_id, project_id, status, selected_occurrences,
           completed_occurrences, checker_input_fingerprint, updated_at
         ) VALUES(?1,'tsconfig.json','completed',1,1,'inputs',datetime('now'))",
        [batch_id],
    )?;
    conn.execute(
        "INSERT INTO checker_enrichments(
           batch_id, member_call_id, source_file_id, source_file, source_hash,
           call_start, call_end, receiver_start, receiver_end,
           property_start, property_end, project_id, receiver_type,
           target_anchor, target_fingerprint, confidence, provenance,
           checker_input_fingerprint
         ) VALUES(
           ?1,?2,?3,'service.ts',?4,?5,?6,?7,?8,?9,?10,
           'tsconfig.json','Service',?11,?12,'likely','checker','inputs'
         )",
        rusqlite::params![
            batch_id,
            member_call_id,
            source_file_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
            target,
            target_fingerprint,
        ],
    )?;
    conn.execute(
        "INSERT INTO checker_occurrence_projects(
           batch_id, member_call_id, source_file, source_hash,
           call_start, call_end, receiver_start, receiver_end,
           property_start, property_end, project_id,
           checker_input_fingerprint, status
         ) VALUES(?1,?2,'service.ts',?3,?4,?5,?6,?7,?8,?9,
                  'tsconfig.json','inputs','resolved')",
        rusqlite::params![
            batch_id,
            member_call_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
        ],
    )?;
    structural::rebuild_projection(&conn, &snapshot)?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM resolved_edges
             WHERE provenance='checker' AND dst_key=?1",
            [&target],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );

    refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;
    let counts: (i64, i64) = conn.query_row(
        "SELECT
           (SELECT count(*) FROM checker_enrichment_batches),
           (SELECT count(*) FROM resolved_edges
              WHERE provenance='checker' AND dst_key=?1)",
        [&target],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(counts, (0, 0));
    Ok(())
}

#[test]
fn forced_reextraction_reset_matches_per_file_replacement() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("pnpm-workspace.yaml"),
        "packages:\n  - packages/*\n",
    )?;
    let lib = repo.path().join("packages/lib");
    fs::create_dir_all(lib.join("src"))?;
    fs::write(
        lib.join("package.json"),
        r#"{"name": "@acme/lib", "module": "src/index.ts"}"#,
    )?;
    fs::write(
        lib.join("src/index.ts"),
        "export const greet = (name: string) => `hi ${name}`;\n\
         export interface Shape { id: string }\n",
    )?;
    fs::write(
        repo.path().join("helper.ts"),
        "export const helper = (value: string) => value.trim();\n",
    )?;
    fs::write(
        repo.path().join("main.ts"),
        "import { greet } from '@acme/lib';\n\
         import type { Shape } from '@acme/lib';\n\
         import { EventEmitter } from 'node:events';\n\
         import { helper } from './helper';\n\
         import { inner } from 'selected-dep';\n\
         import missing from 'not-installed-pkg';\n\
         const emitter = new EventEmitter();\n\
         emitter.on('ready', () => greet('x'));\n\
         emitter.emit('ready');\n\
         export function main(shape: Shape) {\n\
           const key = process.env.API_KEY;\n\
           return helper(greet(key ?? shape.id)) + inner() + missing;\n\
         }\n\
         export const spans = emitter.listeners(\n\
           'ready',\n\
         );\n",
    )?;
    let dependency = repo.path().join("node_modules/selected-dep");
    fs::create_dir_all(&dependency)?;
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"selected-dep","version":"1.2.3","main":"index.js"}"#,
    )?;
    fs::write(
        dependency.join("index.js"),
        "export { inner } from './inner.js';\n",
    )?;
    fs::write(
        dependency.join("inner.js"),
        "export const inner = () => 42;\n",
    )?;

    let databases = tempfile::tempdir()?;
    let per_file = store::open_path(&databases.path().join("per-file.db"))?;
    let reset = store::open_path(&databases.path().join("reset.db"))?;
    let options = IndexOptions {
        dependencies: vec!["selected-dep".into()],
        ..Default::default()
    };
    for conn in [&per_file, &reset] {
        let outcome = index_repo_with_options(repo.path(), conn, &options)?;
        assert!(!outcome.extraction_reset, "initial index must not reset");
        // Semantic memory that must survive a forced re-extraction on
        // both paths: a completed scout run, its classification, and an
        // artifact with one support.
        conn.execute_batch(
            "INSERT INTO scout_runs(
               id, scout_kind, status, gateway_protocol, provider, model,
               billing_path, prompt_version, source_snapshot,
               input_fingerprint, request_hash, started_at, completed_at
             ) VALUES(7, 'workflow', 'completed', 1, 'test', 'test-model',
                      'api', 'v1', 'snap', 'fp', 'req',
                      '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z');
             INSERT INTO scout_classifications(
               run_id, anchor_key, decision, role, evidence_json
             ) VALUES(7, 'sym:main.ts#::main@10', 'defining', 'entry', '{}');
             INSERT INTO semantic_artifacts(
               id, artifact_type, canonical_name, body_json, model,
               prompt_version, confidence, source_snapshot, created_at,
               scout_run_id, input_fingerprint, artifact_fingerprint
             ) VALUES(3, 'workflow', 'checkout', '{}', 'test-model', 'v1',
                      'likely', 'snap', '2026-01-01T00:01:00Z', 7, 'fp', 'af');
             INSERT INTO semantic_supports(
               artifact_id, claim_path, anchor_key, role, evidence_file,
               evidence_start_line, evidence_end_line, source_hash,
               context_hash, confidence
             ) VALUES(3, '$.steps[0]', 'sym:main.ts#::main@10', 'entry',
                      'main.ts', 10, 13, 'sh', 'ch', 'likely');",
        )?;
        // The v15-style forced re-extraction: clear every hash and
        // invalidate the disposable projection and its public identity.
        conn.execute("UPDATE files SET hash = ''", [])?;
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute(
            "DELETE FROM meta
             WHERE key IN ('snapshot', 'projection_version', 'resolution_hash')",
            [],
        )?;
    }

    let slow = index_repo_without_extraction_reset(repo.path(), &per_file, &options)?;
    assert!(!slow.extraction_reset);
    let fast = index_repo_with_options(repo.path(), &reset, &options)?;
    assert!(fast.extraction_reset, "cleared hashes must take the reset");
    assert_eq!(
        (fast.indexed, fast.unchanged, fast.rejected),
        (slow.indexed, slow.unchanged, slow.rejected)
    );

    for ((section, slow_rows), (_, fast_rows)) in canonical_dump(&per_file)?
        .iter()
        .zip(canonical_dump(&reset)?)
    {
        assert_eq!(
            slow_rows, &fast_rows,
            "section `{section}` diverged between per-file and reset paths"
        );
    }

    // Equality alone cannot prove survival; pin the preserved rows and a
    // live FTS index on the reset path explicitly.
    let (runs, artifacts, supports, classifications): (i64, i64, i64, i64) = reset.query_row(
        "SELECT (SELECT count(*) FROM scout_runs),
                (SELECT count(*) FROM semantic_artifacts),
                (SELECT count(*) FROM semantic_supports),
                (SELECT count(*) FROM scout_classifications)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!((runs, artifacts, supports, classifications), (1, 1, 1, 1));
    let greet_hits: i64 = reset.query_row(
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'greet'",
        [],
        |row| row.get(0),
    )?;
    assert!(greet_hits > 0, "rebuilt FTS index must serve matches");
    Ok(())
}

#[test]
fn extraction_reset_triggers_only_at_majority_cleared() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for name in ["one", "two", "three"] {
        fs::write(
            repo.path().join(format!("{name}.ts")),
            format!("export const {name} = 1;\n"),
        )?;
    }
    let conn = store::open(repo.path())?;
    let first = index_repo(repo.path(), &conn)?;
    assert!(!first.extraction_reset);

    let second = index_repo(repo.path(), &conn)?;
    assert!(!second.extraction_reset, "no-op run must stay incremental");
    assert_eq!((second.indexed, second.unchanged), (0, 3));

    conn.execute("UPDATE files SET hash='' WHERE path='one.ts'", [])?;
    let minority = index_repo(repo.path(), &conn)?;
    assert!(
        !minority.extraction_reset,
        "one cleared hash out of three must replace per file"
    );
    assert_eq!((minority.indexed, minority.unchanged), (1, 2));

    conn.execute(
        "UPDATE files SET hash='' WHERE path IN ('one.ts', 'two.ts')",
        [],
    )?;
    let majority = index_repo(repo.path(), &conn)?;
    assert!(majority.extraction_reset, "majority cleared must reset");
    assert_eq!((majority.indexed, majority.unchanged), (3, 0));
    Ok(())
}

#[test]
fn dependency_failure_rolls_back_before_snapshot_invalidation() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let main = repo.path().join("main.ts");
    let before = "import value from 'selected-dep';\nexport const before = value;\n";
    fs::write(&main, before)?;
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
        conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
            row.get(0)
        })?;

    let changed = "import value from 'selected-dep';\nexport const after = value + 1;\n";
    fs::write(&main, changed)?;
    fs::remove_dir_all(&dependency)?;
    let error = index_repo_with_options(repo.path(), &conn, &options)
        .err()
        .expect("missing selected dependency must fail the run");
    assert!(error.to_string().contains("not installed or resolvable"));

    let retained_hash: String =
        conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        retained_hash,
        blake3::hash(before.as_bytes()).to_hex().to_string(),
        "failed dependency preparation committed first-party changes"
    );
    let retained_snapshot: String =
        conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        retained_snapshot, old_snapshot,
        "failed dependency preparation removed the published snapshot"
    );
    Ok(())
}

#[test]
fn retryable_dependency_read_preserves_the_published_snapshot() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let main = repo.path().join("main.ts");
    let before = "import value from 'selected-dep';\nexport const before = value;\n";
    fs::write(&main, before)?;
    let dependency = repo.path().join("node_modules/selected-dep");
    fs::create_dir_all(&dependency)?;
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"selected-dep","version":"1.0.0","main":"index.js"}"#,
    )?;
    let entry = dependency.join("index.js");
    fs::write(&entry, "export default 1;\n")?;

    let conn = store::open(repo.path())?;
    let options = IndexOptions {
        dependencies: vec!["selected-dep".into()],
        ..Default::default()
    };
    index_repo_with_options(repo.path(), &conn, &options)?;
    let old_snapshot: String =
        conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
            row.get(0)
        })?;

    fs::write(
        &main,
        "import value from 'selected-dep';\nexport const after = value + 1;\n",
    )?;
    let fault_fs = FaultFileSystem::default();
    fault_fs.fail_operation(
        FileOperation::ReadToString,
        entry.canonicalize()?,
        std::io::Error::from(ErrorKind::Interrupted),
    );
    let error = index_repo_with_options_and_fs(repo.path(), &conn, &options, &fault_fs)
        .err()
        .expect("retryable dependency read must fail preparation");
    assert!(error.to_string().contains("retryable read failure"));

    let retained_hash: String =
        conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        retained_hash,
        blake3::hash(before.as_bytes()).to_hex().to_string()
    );
    let retained_snapshot: String =
        conn.query_row("SELECT value FROM meta WHERE key='snapshot'", [], |row| {
            row.get(0)
        })?;
    assert_eq!(retained_snapshot, old_snapshot);
    Ok(())
}
