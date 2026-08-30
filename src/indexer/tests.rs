use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::process::Command;

use anyhow::Result;

use super::{
    IndexOptions, incremental_refresh_repo_rebinding_checker,
    incremental_refresh_repo_with_options, index_repo, index_repo_with_fs, index_repo_with_options,
    index_repo_with_options_and_fs, index_repo_with_post_replacement_failure,
    index_repo_with_rust_extraction_failure, refresh_repo_with_options,
    watch_full_refresh_repo_rebinding_checker, watch_full_refresh_repo_with_options,
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

fn git_test_command(root: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn git_test_commit(root: &std::path::Path, message: &str, date: &str) -> Result<()> {
    git_test_command(root, &["add", "--all"])?;
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn index_repo_with_docs_freshness(
    root: &std::path::Path,
    conn: &rusqlite::Connection,
) -> Result<super::IndexOutcome> {
    index_repo_with_options(
        root,
        conn,
        &IndexOptions {
            docs_freshness: true,
            ..IndexOptions::default()
        },
    )
}

#[test]
fn documentation_provenance_is_default_off_and_freshness_queries_fail_closed() -> Result<()> {
    if Command::new("git").arg("--version").output().is_err() {
        return Ok(());
    }
    assert!(!IndexOptions::default().docs_freshness);

    let repo = tempfile::tempdir()?;
    git_test_command(repo.path(), &["init", "--quiet"])?;
    git_test_command(
        repo.path(),
        &["config", "user.email", "jscout@example.invalid"],
    )?;
    git_test_command(repo.path(), &["config", "user.name", "jscout test"])?;
    fs::write(
        repo.path().join("README.md"),
        "# Guide\n\nCurrent provenance guidance.\n",
    )?;
    git_test_commit(repo.path(), "initial", "2024-01-01T00:00:00+00:00")?;

    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let read_marker = || -> Result<String> {
        conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [docs::PROVENANCE_ENABLED_META_KEY],
            |row| row.get(0),
        )
        .map_err(Into::into)
    };
    let read_digest = || -> Result<String> {
        conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [docs::PROVENANCE_DIGEST_META_KEY],
            |row| row.get(0),
        )
        .map_err(Into::into)
    };
    let read_provenance = || -> Result<(String, String, Option<i64>, Option<i64>)> {
        conn.query_row(
            "SELECT provenance.status, metadata.freshness_basis,
                    metadata.freshness_author_time,
                    metadata.freshness_committer_time
             FROM files file
             JOIN chunks chunk ON chunk.file_id=file.id
             JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
             JOIN doc_file_provenance provenance ON provenance.file_id=file.id
             WHERE file.path='README.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(Into::into)
    };
    assert_eq!(read_marker()?, "false");
    let disabled_snapshot = structural::current_snapshot(&conn)?;
    let disabled_digest = read_digest()?;
    assert_eq!(
        read_provenance()?,
        ("disabled".into(), "unknown".into(), None, None)
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM doc_blame_cache", [], |row| row
            .get::<_, i64>(0))?,
        0
    );

    let lexical_options = docs::retrieval::SearchOptions {
        response_bytes: usize::MAX,
        output: docs::retrieval::SearchOutput::Debug,
        vector: false,
        rerank: false,
        ..docs::retrieval::SearchOptions::default()
    };
    assert_eq!(
        docs::retrieval::search(
            &conn,
            repo.path(),
            None,
            "current provenance guidance",
            &lexical_options,
        )?
        .hits
        .len(),
        1
    );
    let freshness_options = docs::retrieval::SearchOptions {
        freshness: true,
        ..lexical_options.clone()
    };
    let error = docs::retrieval::search(
        &conn,
        repo.path(),
        None,
        "current provenance guidance",
        &freshness_options,
    )
    .expect_err("a false provenance marker must fail closed");
    assert!(error.to_string().contains("provenance is not indexed"));

    conn.execute(
        "DELETE FROM meta WHERE key=?1",
        [docs::PROVENANCE_ENABLED_META_KEY],
    )?;
    let error = docs::retrieval::search(
        &conn,
        repo.path(),
        None,
        "current provenance guidance",
        &freshness_options,
    )
    .expect_err("a missing provenance marker must fail closed");
    assert!(error.to_string().contains("run `jscout index`"));

    index_repo_with_docs_freshness(repo.path(), &conn)?;
    assert_eq!(read_marker()?, "true");
    assert_eq!(structural::current_snapshot(&conn)?, disabled_snapshot);
    let enabled_digest = read_digest()?;
    assert_ne!(enabled_digest, disabled_digest);
    assert_eq!(
        read_provenance()?,
        (
            "resolved".into(),
            "git".into(),
            Some(1_704_067_200),
            Some(1_704_067_200),
        )
    );
    assert_eq!(
        docs::retrieval::search(
            &conn,
            repo.path(),
            None,
            "current provenance guidance",
            &freshness_options,
        )?
        .diagnostics
        .freshness_status,
        docs::retrieval::FreshnessStatus::Active
    );
    conn.execute(
        "UPDATE meta SET value='documentation-provenance-stale'
         WHERE key='documentation_provenance_format_version'",
        [],
    )?;
    let error = docs::retrieval::search(
        &conn,
        repo.path(),
        None,
        "current provenance guidance",
        &freshness_options,
    )
    .expect_err("a stale enabled provenance format must fail closed");
    assert!(error.to_string().contains("provenance uses format"));
    conn.execute(
        "DELETE FROM meta WHERE key='documentation_provenance_format_version'",
        [],
    )?;
    let error = docs::retrieval::search(
        &conn,
        repo.path(),
        None,
        "current provenance guidance",
        &freshness_options,
    )
    .expect_err("a missing enabled provenance format must fail closed");
    assert!(error.to_string().contains("uses format missing"));
    assert_eq!(
        docs::retrieval::search(
            &conn,
            repo.path(),
            None,
            "current provenance guidance",
            &lexical_options,
        )?
        .hits
        .len(),
        1,
        "freshness-disabled search must not require the provenance contract"
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM doc_blame_cache", [], |row| row
            .get::<_, i64>(0))?,
        1
    );

    index_repo(repo.path(), &conn)?;
    assert_eq!(read_marker()?, "false");
    assert_eq!(structural::current_snapshot(&conn)?, disabled_snapshot);
    assert_eq!(read_digest()?, disabled_digest);
    assert_eq!(
        read_provenance()?,
        ("disabled".into(), "unknown".into(), None, None)
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM doc_blame_cache", [], |row| row
            .get::<_, i64>(0))?,
        1,
        "disabling freshness must preserve reusable blame-cache entries"
    );
    Ok(())
}

#[test]
fn indexer_publishes_git_provenance_and_ignores_noncontributing_comment_age() -> Result<()> {
    if Command::new("git").arg("--version").output().is_err() {
        return Ok(());
    }
    let repo = tempfile::tempdir()?;
    git_test_command(repo.path(), &["init", "--quiet"])?;
    git_test_command(
        repo.path(),
        &["config", "user.email", "jscout@example.invalid"],
    )?;
    git_test_command(repo.path(), &["config", "user.name", "jscout test"])?;
    fs::write(
        repo.path().join("README.md"),
        "# Guide\n\nCurrent guidance.\n\n<!-- private note one -->\n",
    )?;
    fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
    git_test_commit(repo.path(), "initial", "2001-01-01T00:00:00+00:00")?;

    let conn = store::open(repo.path())?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    let read_provenance = || -> Result<(String, Option<i64>, String, i64)> {
        conn.query_row(
            "SELECT metadata.freshness_basis, metadata.freshness_author_time,
                    provenance.status, chunk.id
             FROM files file
             JOIN chunks chunk ON chunk.file_id=file.id
             JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
             JOIN doc_file_provenance provenance ON provenance.file_id=file.id
             WHERE file.path='README.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(Into::into)
    };
    let initial = read_provenance()?;
    assert_eq!(initial.0, "git");
    assert_eq!(initial.1, Some(978_307_200));
    assert_eq!(initial.2, "resolved");
    assert_eq!(
        conn.query_row("SELECT count(*) FROM doc_blame_cache", [], |row| row
            .get::<_, i64>(0))?,
        1
    );

    fs::write(repo.path().join("main.ts"), "export const value = 2;\n")?;
    git_test_commit(repo.path(), "unrelated code", "2010-01-01T00:00:00+00:00")?;
    let unrelated = index_repo_with_docs_freshness(repo.path(), &conn)?;
    assert!(unrelated.unchanged >= 1);
    assert_eq!(read_provenance()?.3, initial.3);

    fs::write(
        repo.path().join("README.md"),
        "# Guide\n\nCurrent guidance.\n\n<!-- private note two -->\n",
    )?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    let dirty_comment = read_provenance()?;
    assert_eq!(
        (dirty_comment.0.as_str(), dirty_comment.1),
        ("git", initial.1)
    );
    let dirty_comment_snapshot = structural::current_snapshot(&conn)?;

    git_test_commit(repo.path(), "comment only", "2020-01-01T00:00:00+00:00")?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    assert_eq!(read_provenance()?.1, initial.1);
    assert_eq!(
        structural::current_snapshot(&conn)?,
        dirty_comment_snapshot,
        "committing only stripped comments must not rename the search snapshot"
    );

    fs::write(
        repo.path().join("README.md"),
        "# Guide\n\nNew guidance.\n\n<!-- private note two -->\n",
    )?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    let dirty_body = read_provenance()?;
    assert_eq!(dirty_body.0, "working_tree");
    assert_eq!(dirty_body.1, None);

    git_test_commit(repo.path(), "current guidance", "2024-01-01T00:00:00+00:00")?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    let committed_body = read_provenance()?;
    assert_eq!(committed_body.0, "git");
    assert_eq!(committed_body.1, Some(1_704_067_200));
    Ok(())
}

#[test]
fn provenance_only_author_rewrite_rotates_its_digest_without_structural_churn() -> Result<()> {
    if Command::new("git").arg("--version").output().is_err() {
        return Ok(());
    }
    let repo = tempfile::tempdir()?;
    git_test_command(repo.path(), &["init", "--quiet"])?;
    git_test_command(
        repo.path(),
        &["config", "user.email", "jscout@example.invalid"],
    )?;
    git_test_command(repo.path(), &["config", "user.name", "jscout test"])?;
    fs::write(
        repo.path().join("README.md"),
        "# First\n\nShared retrieval phrase in the first section.\n\n\
         # Second\n\nShared retrieval phrase in the second section.\n",
    )?;
    git_test_commit(repo.path(), "initial", "2001-01-01T00:00:00+00:00")?;

    let conn = store::open(repo.path())?;
    index_repo_with_docs_freshness(repo.path(), &conn)?;
    let code_snapshot = structural::current_snapshot(&conn)?;
    let snapshot = docs::store::current_snapshot(&conn)?;
    let publication_snapshot = crate::publication::current_publication_snapshot(&conn)?;
    let provenance_digest: String = conn.query_row(
        "SELECT value FROM meta WHERE key=?1",
        [docs::PROVENANCE_DIGEST_META_KEY],
        |row| row.get(0),
    )?;
    let initial = {
        let mut statement = conn.prepare(
            "SELECT file.id, chunk.id, metadata.embedding_identity,
                    metadata.freshness_author_time,
                    metadata.freshness_committer_time
             FROM files file
             JOIN chunks chunk ON chunk.file_id=file.id
             JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
             WHERE file.path='README.md'
             ORDER BY chunk.start, chunk.end, chunk.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    assert_eq!(initial.len(), 2);
    assert_eq!(initial[0].0, initial[1].0);

    let profile_config = serde_json::json!({
        "document_text": docs::CHUNK_FORMAT_VERSION,
    })
    .to_string();
    conn.execute(
        "INSERT INTO embedding_profiles(
           provider,model,config_fingerprint,dimensions,config_json
         ) VALUES('test','tiny','docs-amend-profile',2,?1)",
        [profile_config],
    )?;
    let profile_id = conn.last_insert_rowid();
    for (_, _, embedding_identity, _, _) in &initial {
        conn.execute(
            "INSERT OR IGNORE INTO embeddings(chunk_hash,profile_id,vec)
             VALUES(?1,?2,X'0000803F00000000')",
            rusqlite::params![embedding_identity, profile_id],
        )?;
    }
    docs::retrieval::rematerialize_cached_generations(&conn, &snapshot)?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM doc_vector_generations
             WHERE snapshot=?1 AND profile_id=?2",
            rusqlite::params![snapshot, profile_id],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );

    let amend = Command::new("git")
        .args([
            "commit",
            "--amend",
            "--no-edit",
            "--quiet",
            "--date=2002-01-01T00:00:00+00:00",
        ])
        .current_dir(repo.path())
        .env("GIT_COMMITTER_DATE", "2002-01-01T00:00:00+00:00")
        .output()?;
    anyhow::ensure!(
        amend.status.success(),
        "git amend failed: {}",
        String::from_utf8_lossy(&amend.stderr)
    );

    let refreshed = index_repo_with_docs_freshness(repo.path(), &conn)?;
    assert_eq!((refreshed.indexed, refreshed.unchanged), (1, 0));
    assert!(
        !refreshed.projection_rebuilt,
        "provenance-only history changes must not rebuild the structural projection"
    );
    assert_eq!(structural::current_snapshot(&conn)?, code_snapshot);
    assert_eq!(docs::store::current_snapshot(&conn)?, snapshot);
    assert_ne!(
        crate::publication::current_publication_snapshot(&conn)?,
        publication_snapshot
    );
    assert_ne!(
        conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [docs::PROVENANCE_DIGEST_META_KEY],
            |row| row.get::<_, String>(0),
        )?,
        provenance_digest,
        "the isolated provenance digest must record rewritten attribution"
    );
    let current = {
        let mut statement = conn.prepare(
            "SELECT file.id, chunk.id, metadata.embedding_identity,
                    metadata.freshness_author_time,
                    metadata.freshness_committer_time
             FROM files file
             JOIN chunks chunk ON chunk.file_id=file.id
             JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
             WHERE file.path='README.md'
             ORDER BY chunk.start, chunk.end, chunk.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    assert_eq!(
        current
            .iter()
            .map(|row| (&row.0, &row.1, &row.2))
            .collect::<Vec<_>>(),
        initial
            .iter()
            .map(|row| (&row.0, &row.1, &row.2))
            .collect::<Vec<_>>()
    );
    assert!(
        current
            .iter()
            .zip(&initial)
            .all(|(after, before)| after.3 != before.3 && after.4 != before.4)
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM doc_vector_generations
             WHERE snapshot=?1 AND profile_id=?2",
            rusqlite::params![snapshot, profile_id],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM doc_embedding_index_entries WHERE profile_id=?1",
            [profile_id],
            |row| row.get::<_, i64>(0),
        )?,
        2
    );
    Ok(())
}

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

    let docs_hits = docs::store::lexical_search(&conn, "mdxOnlyNeedle", 10)?;
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
    index_repo_with_docs_freshness(repo.path(), &conn)?;

    let old_snapshot = structural::current_snapshot(&conn)?;
    let old_provenance_marker: String = conn.query_row(
        "SELECT value FROM meta WHERE key=?1",
        [docs::PROVENANCE_ENABLED_META_KEY],
        |row| row.get(0),
    )?;
    assert_eq!(old_provenance_marker, "true");
    let old_provenance_digest: String = conn.query_row(
        "SELECT value FROM meta WHERE key=?1",
        [docs::PROVENANCE_DIGEST_META_KEY],
        |row| row.get(0),
    )?;
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
    assert_eq!(
        conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [docs::PROVENANCE_ENABLED_META_KEY],
            |row| row.get::<_, String>(0),
        )?,
        old_provenance_marker,
        "failed refresh published a mismatched provenance readiness marker"
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [docs::PROVENANCE_DIGEST_META_KEY],
            |row| row.get::<_, String>(0),
        )?,
        old_provenance_digest,
        "failed refresh published a mismatched provenance digest"
    );
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
fn documentation_contract_change_rechunks_docs_without_rotating_code() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export const unchangedCode = 1;\n",
    )?;
    fs::write(repo.path().join("guide.md"), "# Guide\n\nCurrent body.\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let code_digest = structural::current_snapshot(&conn)?;
    let provenance_digest = crate::publication::Identities::read(&conn)?.provenance;

    let code_chunk_id: i64 = conn.query_row(
        "SELECT chunk.id
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='main.ts' AND chunk.kind='module'",
        [],
        |row| row.get(0),
    )?;
    let old_doc_chunk_id: i64 = conn.query_row(
        "SELECT chunk.id
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.md'",
        [],
        |row| row.get(0),
    )?;
    let old_profile_config = serde_json::json!({
        "document_text": "documentation-v0",
    })
    .to_string();
    conn.execute(
        "INSERT INTO embedding_profiles(
           provider,model,config_fingerprint,dimensions,config_json
         ) VALUES('test','tiny','old-docs-contract',2,?1)",
        [&old_profile_config],
    )?;
    let old_profile_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO doc_embedding_index_entries(id,chunk_id,profile_id)
         VALUES(1,?1,?2)",
        rusqlite::params![old_doc_chunk_id, old_profile_id],
    )?;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE vec_doc_embeddings_2 USING vec0(
           embedding FLOAT[2] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY
         );",
    )?;
    conn.execute(
        "INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id)
         VALUES(1,X'0000803F00000000',?1)",
        [old_profile_id],
    )?;
    // This row has no relational owner, so replacing guide.md cannot remove it.
    // Only the documentation-contract rematerialization purge can clear it.
    conn.execute(
        "INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id)
         VALUES(999,X'0000803F00000000',?1)",
        [old_profile_id],
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
    for format in [crate::formats::MARKDOWN, crate::formats::MDX] {
        conn.execute(
            "UPDATE meta SET value='documentation-v0' WHERE key=?1",
            [format!("format_contract_version:{format}")],
        )?;
    }
    let old_format_documentation_digest = crate::publication::compute_documentation_digest(&conn)?;
    conn.execute(
        "INSERT INTO doc_vector_generations(
           snapshot,profile_id,dimensions,chunk_format_version
         ) VALUES(?1,?2,2,'documentation-v0')",
        rusqlite::params![old_format_documentation_digest, old_profile_id],
    )?;
    crate::publication::Identities::publish_test(
        &conn,
        &code_digest,
        &old_format_documentation_digest,
        &provenance_digest,
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
    assert_eq!(structural::current_snapshot(&conn)?, code_digest);
    assert_ne!(
        docs::store::current_snapshot(&conn)?,
        old_format_documentation_digest
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM doc_vector_generations", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM doc_embedding_index_entries",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM vec_doc_embeddings_2", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );

    let current_doc_chunk_id: i64 = conn.query_row(
        "SELECT chunk.id
         FROM chunks chunk JOIN files file ON file.id=chunk.file_id
         WHERE file.path='guide.md'",
        [],
        |row| row.get(0),
    )?;
    let current_profile_config = serde_json::json!({
        "document_text": docs::CHUNK_FORMAT_VERSION,
    })
    .to_string();
    conn.execute(
        "INSERT INTO embedding_profiles(
           provider,model,config_fingerprint,dimensions,config_json
         ) VALUES('test','tiny','current-docs-contract',2,?1)",
        [&current_profile_config],
    )?;
    let current_profile_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO doc_embedding_index_entries(chunk_id,profile_id) VALUES(?1,?2)",
        rusqlite::params![current_doc_chunk_id, current_profile_id],
    )?;
    let reused_row_id = conn.last_insert_rowid();
    assert_eq!(reused_row_id, 1);
    conn.execute(
        "INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id)
         VALUES(?1,X'0000803F00000000',?2)",
        rusqlite::params![reused_row_id, current_profile_id],
    )?;
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
    let current_identities = crate::publication::Identities::read(&conn)?;

    conn.execute(
        "UPDATE meta SET value='legacy' WHERE key='extraction_version'",
        [],
    )?;
    conn.execute(
        "UPDATE meta SET value='legacy'
         WHERE key='format_contract_version:typescript'",
        [],
    )?;
    let legacy_contract_snapshot = structural::compute_snapshot(&conn)?;
    assert_ne!(legacy_contract_snapshot, current_contract_snapshot);
    crate::publication::Identities::publish_test(
        &conn,
        &legacy_contract_snapshot,
        &current_identities.documentation,
        &current_identities.provenance,
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
                    f.parse_error_count,
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
        "manual indexing must discard a checker batch that cannot be validated"
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
        "a checker batch from another code digest must not enter the projection"
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
fn rust_phase_one_indexes_lossless_lexical_chunks_and_current_diagnostics() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let valid_source = concat!(
        "//! module docs\r\n",
        "/* outer /* nested */ comment */\r\n",
        "pub fn borrowed<'a>(value: &'a str) -> &'a str {\r\n",
        "    let raw = r###\"raw marker 🦀\"###;\r\n",
        "    let bytes = b\"byte marker\";\r\n",
        "    let _ = (raw, bytes);\r\n",
        "    value\r\n",
        "}\r\n",
    );
    let malformed_source = "pub fn broken( {\n    let searchable_malformed_tail_marker = 1;\n";
    fs::write(repo.path().join("main.ts"), "export const stableTs = 1;\n")?;
    fs::write(repo.path().join("valid.rs"), valid_source)?;
    fs::write(repo.path().join("broken.rs"), malformed_source)?;
    fs::write(repo.path().join("empty.rs"), "")?;
    let conn = store::open(repo.path())?;

    let first = index_repo(repo.path(), &conn)?;
    assert_eq!(first.rust_files_with_parse_errors, 1);
    assert!(first.rust_parse_error_count > 0);

    let identities = conn
        .prepare(
            "SELECT path,corpus,format,parse_error_count
             FROM files ORDER BY path",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(identities.len(), 4);
    assert!(identities.contains(&("valid.rs".into(), "code".into(), "rust".into(), 0)));
    assert!(identities.contains(&("main.ts".into(), "code".into(), "typescript".into(), 0,)));
    assert!(
        identities
            .iter()
            .any(|row| { row.0 == "broken.rs" && row.1 == "code" && row.2 == "rust" && row.3 > 0 })
    );
    assert!(identities.contains(&("empty.rs".into(), "code".into(), "rust".into(), 0)));
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM chunks chunk
             JOIN files file ON file.id=chunk.file_id WHERE file.path='empty.rs'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );

    for (path, source) in [("valid.rs", valid_source), ("broken.rs", malformed_source)] {
        let rows = conn
            .prepare(
                "SELECT chunk.kind,chunk.name,chunk.scope_chain,chunk.symbols,
                        chunk.start,chunk.end,chunk.start_line,chunk.end_line,chunk.content
                 FROM chunks chunk JOIN files file ON file.id=chunk.file_id
                 WHERE file.path=?1 ORDER BY chunk.start",
            )?
            .query_map([path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert!(!rows.is_empty());
        let mut cursor = 0usize;
        for (kind, name, scope, symbols, start, end, start_line, end_line, content) in rows {
            let start = usize::try_from(start)?;
            let end = usize::try_from(end)?;
            assert_eq!(kind, "rust_text");
            assert_eq!(name, None);
            assert_eq!(scope, "");
            assert_eq!(symbols, "");
            assert_eq!(start, cursor);
            assert!(end > start && end - start <= 8_000);
            assert_eq!(content.as_bytes(), &source.as_bytes()[start..end]);
            assert!(start_line > 0 && end_line >= start_line);
            cursor = end;
        }
        assert_eq!(cursor, source.len(), "{path} was not partitioned gap-free");
    }

    let hit = search::search(
        &conn,
        None,
        "searchable_malformed_tail_marker",
        &search::SearchOptions {
            rerank: false,
            include_memory: false,
            expand: false,
            ..search::SearchOptions::default()
        },
    )?;
    assert!(hit.hits.iter().any(|hit| hit.file == "broken.rs"));

    for table in [
        "symbols",
        "imports",
        "exports",
        "contract_imports",
        "contract_exports",
        "refs",
        "events",
        "member_calls",
        "receiver_value_flows",
        "function_return_flows",
        "value_binding_flows",
        "class_value_flows",
        "instance_method_value_flows",
        "class_member_value_flow_blockers",
        "entity_sites",
    ] {
        let sql = format!(
            "SELECT COUNT(*) FROM {table} item
             JOIN files file ON file.id=item.file_id WHERE file.format='rust'"
        );
        assert_eq!(
            conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?,
            0,
            "Rust leaked into {table}"
        );
    }
    for (label, sql) in [
        (
            "doc sidecar",
            "SELECT COUNT(*) FROM doc_chunk_meta meta
             JOIN chunks chunk ON chunk.id=meta.chunk_id
             JOIN files file ON file.id=chunk.file_id WHERE file.format='rust'",
        ),
        (
            "graph nodes",
            "SELECT COUNT(*) FROM graph_nodes node
             JOIN files file ON file.id=node.file_id WHERE file.format='rust'",
        ),
        (
            "module edges",
            "SELECT COUNT(*) FROM module_edges edge
             JOIN files file ON file.id=edge.from_file WHERE file.format='rust'",
        ),
        (
            "code vectors",
            "SELECT COUNT(*) FROM embedding_index_entries entry
             JOIN chunks chunk ON chunk.id=entry.chunk_id
             JOIN files file ON file.id=chunk.file_id WHERE file.format='rust'",
        ),
        (
            "documentation vectors",
            "SELECT COUNT(*) FROM doc_embedding_index_entries entry
             JOIN chunks chunk ON chunk.id=entry.chunk_id
             JOIN files file ON file.id=chunk.file_id WHERE file.format='rust'",
        ),
    ] {
        assert_eq!(
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))?,
            0,
            "{label}"
        );
    }

    fs::write(
        repo.path().join("broken.rs"),
        "pub fn repaired() { let searchable_malformed_tail_marker = 1; }\n",
    )?;
    let repaired = index_repo(repo.path(), &conn)?;
    assert_eq!(
        (
            repaired.rust_files_with_parse_errors,
            repaired.rust_parse_error_count,
        ),
        (0, 0)
    );
    fs::remove_file(repo.path().join("broken.rs"))?;
    let deleted = index_repo(repo.path(), &conn)?;
    assert_eq!(
        (
            deleted.rust_files_with_parse_errors,
            deleted.rust_parse_error_count,
        ),
        (0, 0)
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path='broken.rs'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    Ok(())
}

fn assert_invalid_utf8_rust_is_rejected(full_refresh: bool) -> Result<()> {
    let repo = tempfile::tempdir()?;
    let rust = repo.path().join("stable.rs");
    fs::write(&rust, "pub fn stable_utf8_marker() {}\n")?;
    fs::write(
        repo.path().join("healthy.ts"),
        "export const healthyMarker = 1;\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    conn.execute(
        "UPDATE meta SET value='stale-rust-contract'
         WHERE key='format_contract_version:rust'",
        [],
    )?;

    fs::write(&rust, [0xff, 0xfe, b'\n'])?;
    let result = if full_refresh {
        refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())
    } else {
        index_repo(repo.path(), &conn)
    }?;
    assert!(conn.is_autocommit());
    assert_eq!((result.rejected, result.removed), (1, 1));
    assert_eq!(result.rejections[0].path, "stable.rs");
    assert_eq!(result.rejections[0].stage, "read");
    assert!(!result.rejections[0].error.is_empty());
    assert_eq!(result.extraction_reset, full_refresh);
    assert_eq!(
        (result.indexed, result.unchanged),
        if full_refresh { (1, 0) } else { (0, 1) }
    );
    assert_ne!(structural::current_snapshot(&conn)?, snapshot);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path='stable.rs'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'healthyMarker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM meta WHERE key='format_contract_version:rust'",
            [],
            |row| row.get::<_, String>(0),
        )?,
        crate::formats::by_id(crate::formats::RUST)
            .expect("Rust format")
            .extractor_version
    );
    Ok(())
}

#[test]
fn invalid_utf8_rust_is_rejected_during_incremental_indexing() -> Result<()> {
    assert_invalid_utf8_rust_is_rejected(false)
}

#[test]
fn invalid_utf8_rust_is_rejected_during_full_refresh() -> Result<()> {
    assert_invalid_utf8_rust_is_rejected(true)
}

#[test]
fn rust_extraction_invariant_failure_rejects_only_that_file() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("healthy.ts"),
        "export const healthyInvariantMarker = 1;\n",
    )?;
    fs::write(repo.path().join("broken.rs"), "pub fn before() {}\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let previous_snapshot = structural::current_snapshot(&conn)?;

    fs::write(repo.path().join("broken.rs"), "pub fn after() {}\n")?;
    let outcome = index_repo_with_rust_extraction_failure(repo.path(), &conn)?;

    assert_eq!(
        (
            outcome.indexed,
            outcome.unchanged,
            outcome.removed,
            outcome.rejected,
        ),
        (0, 1, 1, 1)
    );
    assert_eq!(outcome.rejections[0].path, "broken.rs");
    assert_eq!(outcome.rejections[0].stage, "extract");
    assert!(
        outcome.rejections[0]
            .error
            .contains("injected Rust extraction invariant failure")
    );
    assert_ne!(structural::current_snapshot(&conn)?, previous_snapshot);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path='broken.rs'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM chunks_fts
             WHERE chunks_fts MATCH 'healthyInvariantMarker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    Ok(())
}

#[test]
fn cargo_edition_change_reextracts_unchanged_rust_source() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src"))?;
    let manifest = repo.path().join("Cargo.toml");
    let rust = repo.path().join("src/lib.rs");
    fs::create_dir_all(repo.path().join("other/src"))?;
    fs::write(
        &manifest,
        "[package]\nname='edition-probe'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    fs::write(&rust, "pub fn gen() {}\n")?;
    fs::write(
        repo.path().join("other/Cargo.toml"),
        "[package]\nname='unchanged-edition'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    fs::write(
        repo.path().join("other/src/lib.rs"),
        "pub fn unchanged_edition_control() {}\n",
    )?;
    fs::write(
        repo.path().join("main.ts"),
        "export const editionControl = 1;\n",
    )?;
    let conn = store::open(repo.path())?;
    let first = index_repo(repo.path(), &conn)?;
    assert_eq!(first.rust_parse_error_count, 0);
    let (source_hash, first_context): (String, String) = conn.query_row(
        "SELECT file.hash,
                (SELECT value FROM meta WHERE key='rust_edition_context_fingerprint')
         FROM files file WHERE file.path='src/lib.rs'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let first_snapshot = structural::current_snapshot(&conn)?;

    fs::write(
        &manifest,
        "[package]\nname='edition-probe'\nversion='0.1.0'\nedition='2024'\n",
    )?;
    let second = index_repo(repo.path(), &conn)?;
    assert_eq!(
        (second.indexed, second.unchanged, second.extraction_reset),
        (1, 2, false)
    );
    assert_eq!(second.rust_files_with_parse_errors, 1);
    assert!(second.rust_parse_error_count > 0);
    let (new_source_hash, second_context): (String, String) = conn.query_row(
        "SELECT file.hash,
                (SELECT value FROM meta WHERE key='rust_edition_context_fingerprint')
         FROM files file WHERE file.path='src/lib.rs'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        new_source_hash, source_hash,
        "files.hash remains a byte hash"
    );
    assert_ne!(second_context, first_context);
    assert_ne!(structural::current_snapshot(&conn)?, first_snapshot);
    Ok(())
}

fn normalize_g26_phase_zero_sections(sections: &mut [(String, String)]) {
    for (name, value) in sections.iter_mut() {
        if name == "files" {
            *value = value
                .lines()
                .map(|line| {
                    let mut fields = line.split('\x1f').collect::<Vec<_>>();
                    // G26 adds `files.parse_error_count`; it is the only
                    // file-column addition normalized by this pre-registry
                    // golden.
                    if fields.len() == 16 {
                        fields.remove(7);
                    }
                    format!("{}\n", fields.join("\x1f"))
                })
                .collect();
        } else if name == "meta" {
            *value = value
                .lines()
                .filter(|line| {
                    let key = line.split('\x1f').next().unwrap_or_default();
                    key != "schema_version"
                        && key != "root"
                        && key != "snapshot"
                        && key != "code_digest"
                        && key != "documentation_digest"
                        && key != "documentation_provenance_enabled"
                        && key != "documentation_provenance_digest"
                        && key != "documentation_provenance_format_version"
                        && !key.starts_with("format_contract_version:")
                })
                .map(|line| format!("{line}\n"))
                .collect();
        }
    }
}

fn g26_phase_zero_normalized_dump(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>> {
    let mut sections = canonical_dump(conn)?
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect::<Vec<_>>();
    normalize_g26_phase_zero_sections(&mut sections);
    sections.push((
        "code_vector_candidates".into(),
        dump_section(
            conn,
            "SELECT file.path, chunk.start, chunk.hash
             FROM chunks chunk JOIN files file ON file.id=chunk.file_id
             WHERE file.corpus='code'",
        )?,
    ));
    sections.push((
        "docs_vector_candidates".into(),
        dump_section(
            conn,
            "SELECT file.path, chunk.start, metadata.embedding_identity
             FROM doc_chunk_meta metadata
             JOIN chunks chunk ON chunk.id=metadata.chunk_id
             JOIN files file ON file.id=chunk.file_id
             WHERE metadata.embedding_identity IS NOT NULL",
        )?,
    ));
    sections.push((
        "checker_membership".into(),
        dump_section(
            conn,
            "SELECT path FROM code_files
             WHERE origin IN ('repository','workspace')
               AND format IN ('javascript','typescript')",
        )?,
    ));
    let graph = query::ModuleGraph::load(conn)?;
    let mut paths = graph.paths.into_values().collect::<Vec<_>>();
    paths.sort();
    sections.push(("resolver_membership".into(), serde_json::to_string(&paths)?));
    let code = search::search(
        conn,
        None,
        "phaseZeroHelper",
        &search::SearchOptions::default(),
    )?;
    sections.push((
        "public_code_search".into(),
        serde_json::to_string(
            &code
                .hits
                .iter()
                .map(|hit| {
                    (
                        hit.file.as_str(),
                        hit.kind.as_str(),
                        hit.name.as_deref(),
                        hit.start_line,
                        hit.end_line,
                        hit.match_reason,
                    )
                })
                .collect::<Vec<_>>(),
        )?,
    ));
    Ok(sections)
}

#[test]
fn phase_zero_registry_refactor_matches_pre_registry_golden() -> Result<()> {
    // This readable pre-registry dump pins canonical rows, FTS, candidate
    // membership, resolver/checker membership, and public code/docs search.
    // The same registry boundary is exercised through its real consumers by
    // the vector-provider, checker-planning, watch-classification, and exact-
    // collision tests in their owning modules.
    let baseline: serde_json::Value = serde_json::from_str(include_str!(
        "../../eval/prereg/g26-phase-zero-baseline-2026-08-25.json"
    ))?;
    assert_eq!(
        baseline["baseline_revision"],
        "4ad4ea2d9b94f7268d155ab3ff9bf27b78625393"
    );

    let repo = tempfile::tempdir()?;
    fs::create_dir(repo.path().join("src"))?;
    fs::write(
        repo.path().join("package.json"),
        r#"{"name":"phase-zero-fixture","type":"module"}"#,
    )?;
    fs::write(
        repo.path().join("src/main.ts"),
        "import { phaseZeroHelper } from './helper.js';\nexport function phaseZeroEntry() { return phaseZeroHelper(); }\n",
    )?;
    fs::write(
        repo.path().join("src/helper.js"),
        "export function phaseZeroHelper() { return 'stable'; }\n",
    )?;
    fs::write(
        repo.path().join("guide.md"),
        "---\ntitle: Phase Zero Guide\ntags: [stable]\n---\n# Start\n\nphaseZeroDocumentation markdown body.\n",
    )?;
    fs::write(
        repo.path().join("guide.mdx"),
        "import Badge from './badge'\n\n# MDX Guide\n\n<Badge label=\"Stable\">phaseZeroDocumentation component body</Badge>\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM files
             WHERE format!='rust' AND parse_error_count!=0",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
    );
    let mut sections = g26_phase_zero_normalized_dump(&conn)?;
    let docs_result = docs::retrieval::search(
        &conn,
        repo.path(),
        None,
        "phaseZeroDocumentation",
        &docs::retrieval::SearchOptions {
            vector: false,
            rerank: false,
            ..Default::default()
        },
    )?;
    sections.push((
        "public_docs_search".into(),
        serde_json::to_string(
            &docs_result
                .hits
                .iter()
                .map(|hit| {
                    (
                        hit.document.path.as_str(),
                        hit.document.title.as_str(),
                        hit.document.breadcrumb.as_str(),
                        hit.document.start_line,
                        hit.document.end_line,
                    )
                })
                .collect::<Vec<_>>(),
        )?,
    ));
    let mut expected =
        serde_json::from_value::<Vec<(String, String)>>(baseline["sections"].clone())?;
    // #111 added documentation provenance publication metadata after this
    // baseline was recorded. Normalize that independent delta on both sides so
    // this test remains specific to the G26 registry refactor.
    normalize_g26_phase_zero_sections(&mut expected);
    assert_eq!(
        sections, expected,
        "phase-0 canonical/public differential changed; sections={sections:#?}"
    );
    Ok(())
}

#[test]
fn phase_zero_format_marker_bootstrap_is_otherwise_byte_identical() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.js"), "export const jsMarker = 1;\n")?;
    fs::write(repo.path().join("main.ts"), "export const tsMarker = 1;\n")?;
    fs::write(repo.path().join("guide.md"), "# Markdown\n\nBody.\n")?;
    fs::write(
        repo.path().join("guide.mdx"),
        "# MDX\n\n<Badge>Body</Badge>\n",
    )?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let ids = conn
        .prepare(
            "SELECT file.path,file.id,chunk.id
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             ORDER BY file.path,chunk.start",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let before = canonical_dump(&conn)?;
    conn.execute(
        "DELETE FROM meta WHERE key LIKE 'format_contract_version:%'",
        [],
    )?;

    let outcome = index_repo(repo.path(), &conn)?;
    assert_eq!((outcome.indexed, outcome.unchanged), (0, 4));
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);
    let retained_ids = conn
        .prepare(
            "SELECT file.path,file.id,chunk.id
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             ORDER BY file.path,chunk.start",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(retained_ids, ids);
    assert_eq!(canonical_dump(&conn)?, before);
    for format in crate::formats::ALL {
        let value: String = conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            [crate::formats::contract_meta_key(format)],
            |row| row.get(0),
        )?;
        assert_eq!(value, format.extractor_version);
    }
    Ok(())
}

#[test]
fn absent_format_contracts_are_repaired_without_invalidating_a_javascript_snapshot() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.js"), "export const stable = 1;\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let snapshot = structural::current_snapshot(&conn)?;
    let before = canonical_dump(&conn)?;
    let ids = conn
        .prepare(
            "SELECT file.id, chunk.id
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             WHERE file.path='main.js' ORDER BY chunk.start",
        )?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for format in ["typescript", "markdown", "mdx", "rust"] {
        conn.execute(
            "UPDATE meta SET value='future-contract'
             WHERE key=?1",
            [format!("format_contract_version:{format}")],
        )?;
    }

    // The legacy global markers remain compatibility gates. Per-format
    // markers only matter while rows produced by that format are present.
    let reader = store::open_path_read_only(&repo.path().join(store::DB_FILE))?;
    drop(reader);

    let outcome = index_repo(repo.path(), &conn)?;
    assert_eq!((outcome.indexed, outcome.unchanged), (0, 1));
    assert!(!outcome.projection_rebuilt);
    assert!(!outcome.extraction_reset);
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);
    let retained_ids = conn
        .prepare(
            "SELECT file.id, chunk.id
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             WHERE file.path='main.js' ORDER BY chunk.start",
        )?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(retained_ids, ids);
    assert_eq!(canonical_dump(&conn)?, before);
    Ok(())
}

#[test]
fn rust_contract_invalidation_reextracts_only_rust() -> Result<()> {
    let repo = tempfile::tempdir()?;
    for (path, source) in [
        ("main.js", "export const jsMarker = 1;\n"),
        ("main.ts", "export const tsMarker = 1;\n"),
        ("lib.rs", "pub fn rust_marker() {}\n"),
        ("guide.md", "# Markdown\n\nBody.\n"),
        ("guide.mdx", "# MDX\n\nBody.\n"),
    ] {
        fs::write(repo.path().join(path), source)?;
    }
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    let original_snapshot = structural::current_snapshot(&conn)?;
    let original_identities = crate::publication::Identities::read(&conn)?;
    let original_ids = conn
        .prepare(
            "SELECT file.path,file.id,MIN(chunk.id)
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             GROUP BY file.id,file.path ORDER BY file.path",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()?;
    conn.execute(
        "UPDATE meta SET value='rust-text-v0'
         WHERE key='format_contract_version:rust'",
        [],
    )?;
    let stale_snapshot = structural::compute_snapshot(&conn)?;
    assert_ne!(stale_snapshot, original_snapshot);
    crate::publication::Identities::publish_test(
        &conn,
        &stale_snapshot,
        &original_identities.documentation,
        &original_identities.provenance,
    )?;

    let outcome = index_repo(repo.path(), &conn)?;
    assert_eq!((outcome.indexed, outcome.unchanged), (1, 4));
    assert!(!outcome.extraction_reset);
    assert_ne!(structural::current_snapshot(&conn)?, stale_snapshot);
    let current_ids = conn
        .prepare(
            "SELECT file.path,file.id,MIN(chunk.id)
             FROM files file JOIN chunks chunk ON chunk.file_id=file.id
             GROUP BY file.id,file.path ORDER BY file.path",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()?;
    for path in ["guide.md", "guide.mdx", "main.js", "main.ts"] {
        assert_eq!(
            current_ids.get(path),
            original_ids.get(path),
            "{path} moved"
        );
    }
    assert_ne!(current_ids.get("lib.rs"), original_ids.get("lib.rs"));
    Ok(())
}

#[test]
fn rust_incremental_add_edit_delete_matches_full_refresh() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const tsStable = 1;\n")?;
    fs::write(
        repo.path().join("edited.rs"),
        "pub fn edited() -> u8 { 1 }\n",
    )?;
    fs::write(repo.path().join("removed.rs"), "pub fn removed() {}\n")?;
    let incremental = store::open_path(&repo.path().join("incremental.db"))?;
    let full = store::open_path(&repo.path().join("full.db"))?;
    refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
    refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;

    fs::write(
        repo.path().join("edited.rs"),
        "pub fn edited() -> u8 { let changed = 2; changed }\n",
    )?;
    fs::remove_file(repo.path().join("removed.rs"))?;
    fs::write(repo.path().join("added.rs"), "pub fn added() {}\n")?;
    incremental_refresh_repo_with_options(repo.path(), &incremental, &IndexOptions::default())?;
    refresh_repo_with_options(repo.path(), &full, &IndexOptions::default())?;

    assert_eq!(
        structural::current_snapshot(&incremental)?,
        structural::current_snapshot(&full)?
    );
    assert_eq!(canonical_dump(&incremental)?, canonical_dump(&full)?);
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
        "manual full indexing discards a checker batch that cannot be validated"
    );
    Ok(())
}

fn seed_active_checker_publication(
    repo: &std::path::Path,
    conn: &rusqlite::Connection,
    absolute_input: &std::path::Path,
) -> Result<(i64, String, i64, i64)> {
    let snapshot = structural::current_snapshot(conn)?;
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
        "SELECT call.rowid,file.id,file.hash,call.start,call.end,
                call.receiver_start,call.receiver_end,
                call.property_start,call.property_end
         FROM member_calls call JOIN files file ON file.id=call.file_id
         WHERE file.path='service.ts' AND call.prop='load'",
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
            "SELECT node.node_key,file.hash,symbol.decl_start,symbol.decl_end
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
           source_snapshot,checker_version,checker_source,
           checker_input_fingerprint,sidecar_protocol,created_at,active
         ) VALUES(?1,'5.9.3','test','inputs',1,datetime('now'),1)",
        [&snapshot],
    )?;
    let batch_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO checker_project_runs(
           batch_id,project_id,status,selected_occurrences,
           completed_occurrences,checker_input_fingerprint,execution_kind,updated_at
         ) VALUES(?1,'tsconfig.json','completed',1,1,'inputs','checked',datetime('now'))",
        [batch_id],
    )?;
    let absolute_bytes = fs::read(absolute_input)?;
    let absolute_hash = blake3::hash(&absolute_bytes).to_hex().to_string();
    conn.execute(
        "INSERT INTO checker_project_inputs(
           batch_id,project_id,input_kind,input_path,source_hash
         ) VALUES(?1,'tsconfig.json','repository','service.ts',?2)",
        rusqlite::params![batch_id, source_hash],
    )?;
    conn.execute(
        "INSERT INTO checker_project_inputs(
           batch_id,project_id,input_kind,input_path,source_hash
         ) VALUES(?1,'tsconfig.json','absolute',?2,?3)",
        rusqlite::params![batch_id, absolute_input.to_string_lossy(), absolute_hash,],
    )?;
    conn.execute(
        "INSERT INTO checker_enrichments(
           batch_id,member_call_id,source_file_id,source_file,source_hash,
           call_start,call_end,receiver_start,receiver_end,
           property_start,property_end,project_id,receiver_type,
           target_anchor,target_fingerprint,confidence,provenance,
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
           batch_id,member_call_id,source_file,source_hash,
           call_start,call_end,receiver_start,receiver_end,
           property_start,property_end,project_id,
           checker_input_fingerprint,status
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
    structural::rebuild_projection(conn, &snapshot)?;
    assert!(repo.join("service.ts").is_file());
    Ok((batch_id, target, source_file_id, member_call_id))
}

fn checker_rebind_fixture() -> Result<(
    tempfile::TempDir,
    tempfile::TempDir,
    rusqlite::Connection,
    i64,
    String,
    i64,
    i64,
)> {
    checker_rebind_fixture_with_fragmented_ids(false)
}

fn fragmented_checker_rebind_fixture() -> Result<(
    tempfile::TempDir,
    tempfile::TempDir,
    rusqlite::Connection,
    i64,
    String,
    i64,
    i64,
)> {
    checker_rebind_fixture_with_fragmented_ids(true)
}

fn checker_rebind_fixture_with_fragmented_ids(
    fragment_ids: bool,
) -> Result<(
    tempfile::TempDir,
    tempfile::TempDir,
    rusqlite::Connection,
    i64,
    String,
    i64,
    i64,
)> {
    let repo = tempfile::tempdir()?;
    if fragment_ids {
        fs::write(
            repo.path().join("000-removed.ts"),
            "declare const warmupProbe: { warmup(): void };\nwarmupProbe.warmup();\n",
        )?;
    }
    fs::write(
        repo.path().join("service.ts"),
        "export class Service { load() {} }\n\
         export function run(service: Service) { service.load(); }\n",
    )?;
    let external = tempfile::tempdir()?;
    let input = external.path().join("lib.d.ts");
    fs::write(&input, "declare interface ExternalInput { stable: true }\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;
    if fragment_ids {
        fs::remove_file(repo.path().join("000-removed.ts"))?;
        index_repo(repo.path(), &conn)?;
    }
    let (batch, target, source_file, member_call) =
        seed_active_checker_publication(repo.path(), &conn, &input)?;
    Ok((
        repo,
        external,
        conn,
        batch,
        target,
        source_file,
        member_call,
    ))
}

#[test]
fn manual_docs_only_refresh_preserves_a_valid_checker_publication() -> Result<()> {
    let (repo, _external, conn, _batch, target, _, _) = checker_rebind_fixture()?;
    let before = crate::publication::Identities::read(&conn)?;
    fs::write(
        repo.path().join("README.md"),
        "# Service\n\nThe service loads its current state.\n",
    )?;

    refresh_repo_with_options(repo.path(), &conn, &IndexOptions::default())?;

    let after = crate::publication::Identities::read(&conn)?;
    assert_eq!(after.code, before.code);
    assert_ne!(after.documentation, before.documentation);
    assert_ne!(after.publication, before.publication);
    assert_eq!(
        conn.query_row(
            "SELECT source_snapshot FROM checker_enrichment_batches WHERE active=1",
            [],
            |row| row.get::<_, String>(0),
        )?,
        after.code
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE provenance='checker' AND dst_key=?1",
            [&target],
            |row| row.get::<_, i64>(0),
        )?,
        1,
        "a docs-only manual publication must keep validated checker edges projected"
    );
    Ok(())
}

#[test]
fn docs_only_refresh_does_not_initialize_code_vector_storage() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const stable = 1;\n")?;
    fs::write(repo.path().join("README.md"), "# Before\n\nOld prose.\n")?;
    let conn = store::open(repo.path())?;
    index_repo(repo.path(), &conn)?;

    conn.execute_batch(
        "INSERT INTO embedding_profiles(
           provider,model,config_fingerprint,dimensions,config_json
         ) VALUES('test','tiny','code-only-profile',2,'{}');",
    )?;
    assert!(!conn.query_row(
        "SELECT EXISTS(
               SELECT 1 FROM sqlite_master
               WHERE type='table' AND name='vec_embeddings_2'
             )",
        [],
        |row| row.get::<_, bool>(0),
    )?);

    fs::write(repo.path().join("README.md"), "# After\n\nNew prose.\n")?;
    let outcome = index_repo(repo.path(), &conn)?;

    assert_eq!((outcome.indexed, outcome.unchanged), (1, 1));
    assert!(
        !conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_master
               WHERE type='table' AND name='vec_embeddings_2'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?,
        "documentation changes must not scan or initialize the code-vector plane"
    );
    Ok(())
}

#[test]
fn rust_only_incremental_refresh_rebinds_fresh_checker_facts_without_a_provider() -> Result<()> {
    let (repo, external, conn, batch, target, _, _) = checker_rebind_fixture()?;
    let old_snapshot = structural::current_snapshot(&conn)?;
    fs::write(repo.path().join("000-rust.rs"), "pub fn added() {}\n")?;

    let outcome =
        incremental_refresh_repo_rebinding_checker(repo.path(), &conn, &IndexOptions::default())?;
    let current = structural::current_snapshot(&conn)?;
    assert_ne!(current, old_snapshot);
    assert!(outcome.checker_rebound);
    let (rebound_batch, rebound_snapshot): (i64, String) = conn.query_row(
        "SELECT id,source_snapshot FROM checker_enrichment_batches WHERE active=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_ne!(rebound_batch, batch);
    assert_eq!(rebound_snapshot, current);
    assert_eq!(
        conn.query_row(
            "SELECT source_snapshot,active
             FROM checker_enrichment_batches WHERE id=?1",
            [batch],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )?,
        (old_snapshot, false),
    );
    for table in [
        "checker_project_runs",
        "checker_project_inputs",
        "checker_occurrence_projects",
        "checker_enrichments",
    ] {
        let source_rows = conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE batch_id=?1"),
            [batch],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(
            conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE batch_id=?1"),
                [rebound_batch],
                |row| row.get::<_, i64>(0),
            )?,
            source_rows,
            "{table} was not cloned into the rebound publication",
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT execution_kind FROM checker_project_runs
             WHERE batch_id=?1 AND project_id='tsconfig.json'",
            [rebound_batch],
            |row| row.get::<_, String>(0),
        )?,
        "carried",
    );
    assert_eq!(
        conn.query_row(
            "SELECT execution_kind FROM checker_project_runs
             WHERE batch_id=?1 AND project_id='tsconfig.json'",
            [batch],
            |row| row.get::<_, String>(0),
        )?,
        "checked",
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE provenance='checker' AND dst_key=?1",
            [&target],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );

    fs::write(
        external.path().join("lib.d.ts"),
        "declare interface ExternalInput { changed: true }\n",
    )?;
    fs::write(
        repo.path().join("000-rust.rs"),
        "pub fn added() { let changed = true; }\n",
    )?;
    let stale =
        incremental_refresh_repo_rebinding_checker(repo.path(), &conn, &IndexOptions::default())?;
    assert!(!stale.checker_rebound);
    assert_ne!(structural::current_snapshot(&conn)?, current);
    assert_eq!(
        conn.query_row(
            "SELECT source_snapshot FROM checker_enrichment_batches WHERE id=?1 AND active=1",
            [rebound_batch],
            |row| row.get::<_, String>(0),
        )?,
        current
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    Ok(())
}

#[test]
fn rust_only_refresh_does_not_rebind_across_a_checker_format_contract_change() -> Result<()> {
    let (repo, _external, conn, batch, _, _, _) = checker_rebind_fixture()?;
    let old_snapshot = structural::current_snapshot(&conn)?;
    conn.execute(
        "UPDATE meta SET value='stale-typescript-contract'
         WHERE key='format_contract_version:typescript'",
        [],
    )?;
    fs::write(repo.path().join("000-rust.rs"), "pub fn added() {}\n")?;

    let outcome =
        incremental_refresh_repo_rebinding_checker(repo.path(), &conn, &IndexOptions::default())?;
    let current_snapshot = structural::current_snapshot(&conn)?;

    assert_ne!(current_snapshot, old_snapshot);
    assert!(!outcome.checker_rebound);
    assert_eq!(
        conn.query_row(
            "SELECT source_snapshot FROM checker_enrichment_batches WHERE id=?1 AND active=1",
            [batch],
            |row| row.get::<_, String>(0),
        )?,
        old_snapshot,
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
    );
    Ok(())
}

#[test]
fn stale_checker_inputs_are_unpublished_even_when_rust_snapshot_is_unchanged() -> Result<()> {
    let (repo, external, conn, batch, _, _, _) = checker_rebind_fixture()?;
    let snapshot = structural::current_snapshot(&conn)?;
    fs::write(
        external.path().join("lib.d.ts"),
        "declare interface ExternalInput { changed: true }\n",
    )?;

    let outcome =
        incremental_refresh_repo_rebinding_checker(repo.path(), &conn, &IndexOptions::default())?;
    assert!(!outcome.checker_rebound);
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);
    assert!(!conn.query_row(
        "SELECT active FROM checker_enrichment_batches WHERE id=?1",
        [batch],
        |row| row.get::<_, bool>(0),
    )?);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    Ok(())
}

fn assert_identical_full_refresh_remaps_fragmented_checker_rowids(
    refresh: fn(
        &std::path::Path,
        &rusqlite::Connection,
        &IndexOptions,
    ) -> Result<super::IndexOutcome>,
    preserve_old_batch: bool,
) -> Result<()> {
    let (repo, _external, conn, batch, target, old_file_id, old_call_id) =
        fragmented_checker_rebind_fixture()?;
    let snapshot = structural::current_snapshot(&conn)?;

    let outcome = refresh(repo.path(), &conn, &IndexOptions::default())?;
    assert!(outcome.extraction_reset);
    assert!(outcome.projection_rebuilt);
    assert!(!outcome.checker_rebound);
    assert_eq!(structural::current_snapshot(&conn)?, snapshot);

    let current_ids: (i64, i64) = conn.query_row(
        "SELECT file.id,call.rowid FROM files file
         JOIN member_calls call ON call.file_id=file.id
         WHERE file.path='service.ts' AND call.prop='load'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_ne!(
        current_ids,
        (old_file_id, old_call_id),
        "the fixture must reach the same code digest through fragmented row IDs"
    );
    let (current_batch, current_batch_snapshot): (i64, String) = conn.query_row(
        "SELECT id,source_snapshot FROM checker_enrichment_batches WHERE active=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_ne!(current_batch, batch);
    assert_eq!(current_batch_snapshot, snapshot);
    assert_eq!(
        conn.query_row(
            "SELECT source_file_id,member_call_id
             FROM checker_enrichments WHERE batch_id=?1",
            [current_batch],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?,
        current_ids,
        "the retained checker publication must use rebuilt extraction row IDs"
    );
    let old_batch_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM checker_enrichment_batches WHERE id=?1",
        [batch],
        |row| row.get(0),
    )?;
    if preserve_old_batch {
        assert_eq!(old_batch_count, 1);
        assert!(!conn.query_row(
            "SELECT active FROM checker_enrichment_batches WHERE id=?1",
            [batch],
            |row| row.get::<_, bool>(0),
        )?);
    } else {
        assert_eq!(old_batch_count, 0);
    }
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE provenance='checker' AND dst_key=?1 AND source_ref_id=?2",
            rusqlite::params![target, current_ids.1],
            |row| row.get::<_, i64>(0),
        )?,
        1,
        "the retained checker fact must re-project against the rebuilt canonical row"
    );
    Ok(())
}

#[test]
fn identical_manual_full_refresh_remaps_fragmented_checker_rowids() -> Result<()> {
    assert_identical_full_refresh_remaps_fragmented_checker_rowids(refresh_repo_with_options, false)
}

#[test]
fn identical_watch_full_refresh_remaps_fragmented_checker_rowids() -> Result<()> {
    assert_identical_full_refresh_remaps_fragmented_checker_rowids(
        watch_full_refresh_repo_with_options,
        true,
    )
}

fn assert_full_refresh_fails_closed_when_checker_rows_cannot_be_remapped(
    refresh: fn(
        &std::path::Path,
        &rusqlite::Connection,
        &IndexOptions,
    ) -> Result<super::IndexOutcome>,
    preserve_old_batch: bool,
) -> Result<()> {
    let (repo, _external, conn, batch, _target, _file_id, _call_id) = checker_rebind_fixture()?;
    conn.execute(
        "UPDATE checker_enrichments SET call_start=call_start+1 WHERE batch_id=?1",
        [batch],
    )?;

    let outcome = refresh(repo.path(), &conn, &IndexOptions::default())?;
    assert!(outcome.extraction_reset);
    assert!(!outcome.checker_rebound);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM checker_enrichment_batches WHERE active=1",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
        "an unremappable checker publication must not remain active",
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges WHERE provenance='checker'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0,
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM checker_enrichment_batches WHERE id=?1",
            [batch],
            |row| row.get::<_, i64>(0),
        )?,
        i64::from(preserve_old_batch),
    );
    Ok(())
}

#[test]
fn manual_full_refresh_drops_unremappable_checker_rows() -> Result<()> {
    assert_full_refresh_fails_closed_when_checker_rows_cannot_be_remapped(
        refresh_repo_with_options,
        false,
    )
}

#[test]
fn watch_full_refresh_hides_unremappable_checker_rows_as_carry() -> Result<()> {
    assert_full_refresh_fails_closed_when_checker_rows_cannot_be_remapped(
        watch_full_refresh_repo_with_options,
        true,
    )
}

#[test]
fn rust_only_full_refresh_rebinds_checker_rows_after_rowids_move() -> Result<()> {
    let (repo, _external, conn, batch, target, old_file_id, old_call_id) =
        checker_rebind_fixture()?;
    let old_snapshot = structural::current_snapshot(&conn)?;
    fs::write(
        repo.path().join("000-rust.rs"),
        "pub fn sorted_first() {}\n",
    )?;

    let outcome =
        watch_full_refresh_repo_rebinding_checker(repo.path(), &conn, &IndexOptions::default())?;
    assert!(outcome.extraction_reset);
    assert!(outcome.checker_rebound);
    let (new_file_id, new_call_id): (i64, i64) = conn.query_row(
        "SELECT file.id,call.rowid FROM files file
         JOIN member_calls call ON call.file_id=file.id
         WHERE file.path='service.ts' AND call.prop='load'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_ne!((new_file_id, new_call_id), (old_file_id, old_call_id));
    let rebound_batch: i64 = conn.query_row(
        "SELECT id FROM checker_enrichment_batches WHERE active=1",
        [],
        |row| row.get(0),
    )?;
    assert_ne!(rebound_batch, batch);
    assert_eq!(
        conn.query_row(
            "SELECT source_snapshot,active
             FROM checker_enrichment_batches WHERE id=?1",
            [batch],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )?,
        (old_snapshot, false),
    );
    let rebound_ids: (i64, i64) = conn.query_row(
        "SELECT source_file_id,member_call_id
         FROM checker_enrichments WHERE batch_id=?1",
        [rebound_batch],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(rebound_ids, (new_file_id, new_call_id));
    assert_eq!(
        conn.query_row(
            "SELECT source_file_id,member_call_id
             FROM checker_enrichments WHERE batch_id=?1",
            [batch],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?,
        (old_file_id, old_call_id),
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE provenance='checker' AND dst_key=?1",
            [&target],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    Ok(())
}

#[test]
fn identical_manual_full_refresh_discards_an_unproven_checker_batch() -> Result<()> {
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
