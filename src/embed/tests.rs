use rusqlite::Connection;

use super::{
    DOCUMENT_TEXT_FORMAT, ProfileSpec, Protocol, Provider, ResolvedProfile, VectorFailureKind,
    code_vector_failure_action, embed_missing_for_selection_report, embed_semantic_missing_report,
    embed_text, ensure_profile, exact_semantic_vector_search, exact_vector_search,
    existing_profile, materialize_cached_embeddings, missing_embedding_documents,
    profile_fingerprint, ready_search_profile, semantic_embedding_documents,
    semantic_vector_failure, semantic_vector_failure_action, semantic_vector_index_has_gaps,
    semantic_vector_table, sync_semantic_vector_index, sync_vector_index, synchronize_vector_index,
    validate_endpoint, vec_to_blob, vector_failure, vector_index_has_completed_sync,
    vector_index_needs_sync, vector_search, vector_table, vector_table_exists,
};
use crate::config::{EmbeddingSettings, InferenceSettings};

#[test]
fn provider_is_constructed_from_resolved_settings_without_environment_reads() -> anyhow::Result<()>
{
    let provider = Provider::from_settings(
        &EmbeddingSettings {
            provider: Some("local".to_string()),
            model: Some("example/embed".to_string()),
            revision: Some("immutable-revision".to_string()),
            url: None,
            api_key_env: None,
            query_prefix: None,
            batch: 64,
            origins: crate::origin::defaults(),
        },
        &InferenceSettings {
            url: "http://127.0.0.1:9876/".to_string(),
            host: "127.0.0.1".to_string(),
            port: 9876,
            project: None,
            uv: "uv".to_string(),
            allow_remote: false,
            batch_size: 16,
            max_length: 4096,
            model_cache_root: None,
        },
    )?
    .expect("local provider");
    assert_eq!(provider.name, "local");
    assert_eq!(provider.model, "example/embed");
    assert_eq!(provider.url, "http://127.0.0.1:9876/embed");
    assert_eq!(provider.revision.as_deref(), Some("immutable-revision"));
    Ok(())
}

fn insert_policy(conn: &Connection, file_id: i64, role: &str, suffix: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO scout_runs(
           scout_kind,status,gateway_protocol,provider,model,billing_path,
           prompt_version,source_snapshot,input_fingerprint,request_hash,
           config_json,started_at,completed_at
         ) VALUES('repository','completed',1,'test','test','custom',
                  'test','snapshot',?1,?1,'{}','now','now')",
        [format!("policy-{suffix}")],
    )?;
    let run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO repository_classifications(
           run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
           explanation,citations_json,evidence_fingerprint,
           classification_fingerprint,source_snapshot,created_at
         ) VALUES(?1,?2,'area','{}',0,?3,'likely','test','[\"E001\"]',
                  ?2,?2,'snapshot','now')",
        rusqlite::params![run_id, format!("area:{suffix}"), role],
    )?;
    let classification_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO repository_file_policy(
           file_id,classification_id,subject_key,scope_role,effective_role,
           source_hash,depth
         ) VALUES(?1,?2,?3,?4,?4,'hash',0)",
        rusqlite::params![file_id, classification_id, format!("area:{suffix}"), role],
    )?;
    Ok(())
}

#[test]
fn profile_fingerprint_changes_with_configuration() {
    let first = profile_fingerprint("local", "m", r#"{"dtype":"float16"}"#);
    let second = profile_fingerprint("local", "m", r#"{"dtype":"float32"}"#);
    assert_ne!(first, second);
}

#[test]
fn semantic_vector_failure_actions_distinguish_service_from_index() {
    let inference = semantic_vector_failure(
        VectorFailureKind::Inference,
        anyhow::anyhow!("connection refused"),
    );
    assert_eq!(
        semantic_vector_failure_action(&inference),
        "start or repair the configured embedding service, then retry"
    );
    let index =
        semantic_vector_failure(VectorFailureKind::Index, anyhow::anyhow!("profile missing"));
    assert_eq!(
        semantic_vector_failure_action(&index),
        "run jscout embed <root> --semantic-only"
    );
    let code_index = vector_failure(
        "code",
        VectorFailureKind::Index,
        anyhow::anyhow!("profile missing"),
    );
    assert_eq!(
        code_vector_failure_action(&code_index),
        "run jscout embed <root> --repair"
    );
}

#[test]
fn device_only_legacy_profile_is_reused_without_duplication() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    let embedding = serde_json::json!({
        "model": "BAAI/bge-m3",
        "dimensions": 2,
        "revision": "pinned",
        "configuration": {
            "pooling": "cls",
            "normalized": true,
            "max_length": 4096,
            "dtype": "float16"
        }
    });
    let legacy_config = serde_json::json!({
        "protocol": "jscout-local-v1",
        "device": "mps",
        "document_text": DOCUMENT_TEXT_FORMAT,
        "embedding": embedding
    })
    .to_string();
    connection.execute(
        "INSERT INTO embedding_profiles(
           provider, model, config_fingerprint, dimensions, config_json
         ) VALUES('local', 'BAAI/bge-m3', ?1, 2, ?2)",
        rusqlite::params![
            profile_fingerprint("local", "BAAI/bge-m3", &legacy_config),
            legacy_config
        ],
    )?;
    let legacy_id = connection.last_insert_rowid();
    let config_json = serde_json::json!({
        "protocol": "jscout-local-v1",
        "document_text": DOCUMENT_TEXT_FORMAT,
        "embedding": embedding
    })
    .to_string();
    let spec = ProfileSpec {
        provider: "local".into(),
        model: "BAAI/bge-m3".into(),
        fingerprint: profile_fingerprint("local", "BAAI/bge-m3", &config_json),
        config_json,
        dimensions: Some(2),
    };

    assert_eq!(existing_profile(&connection, &spec)?.unwrap().id, legacy_id);
    assert_eq!(ensure_profile(&connection, &spec, 2)?.id, legacy_id);
    let profiles: i64 =
        connection.query_row("SELECT count(*) FROM embedding_profiles", [], |row| {
            row.get(0)
        })?;
    assert_eq!(profiles, 1);
    Ok(())
}

#[test]
fn document_embedding_text_is_content_addressed_and_utf8_bounded() {
    assert_eq!(
        embed_text("export const answer = 42;"),
        "export const answer = 42;"
    );
    let long = "é".repeat(20_000);
    let embedded = embed_text(&long);
    assert!(embedded.len() <= 24_000);
    assert!(embedded.is_char_boundary(embedded.len()));
    assert!(!embedded.contains("// file:"));
}

#[test]
fn semantic_embedding_documents_include_meaning_and_current_anchors() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type,canonical_name,body_json,model,prompt_version,
           confidence,source_snapshot,created_at,artifact_fingerprint
         ) VALUES('card','old','{\"purpose\":\"old route\"}','test','card/v1',
                  'likely','snapshot','now','old')",
        [],
    )?;
    let old_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO semantic_artifacts(
           supersedes_artifact_id,artifact_type,canonical_name,body_json,
           model,prompt_version,confidence,source_snapshot,created_at,
           artifact_fingerprint
         ) VALUES(?1,'card','resolveRoute',
                  '{\"purpose\":\"Preserves rewrite state during fallback.\"}',
                  'test','card/v1','likely','snapshot','now','current')",
        [old_id],
    )?;
    let current_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO semantic_supports(
           artifact_id,claim_path,anchor_key,evidence_file,
           evidence_start_line,evidence_end_line,source_hash,context_hash,confidence
         ) VALUES(?1,'/purpose','sym:src/cache.ts#::resolveRoute@1',
                  'src/cache.ts',10,20,'source','context','likely')",
        [current_id],
    )?;

    let documents = semantic_embedding_documents(&connection)?;
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].artifact_id, current_id);
    assert!(documents[0].content.contains("Preserves rewrite state"));
    assert!(
        documents[0]
            .content
            .contains("sym:src/cache.ts#::resolveRoute@1")
    );
    Ok(())
}

#[test]
fn semantic_vectors_materialize_and_rank_independently_of_code_chunks() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    for (name, body, fingerprint) in [
        ("rewrite", r#"{"purpose":"rewrite fallback"}"#, "rewrite-fp"),
        ("headers", r#"{"purpose":"request headers"}"#, "headers-fp"),
    ] {
        connection.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type,canonical_name,body_json,model,prompt_version,
               confidence,source_snapshot,created_at,artifact_fingerprint
             ) VALUES('card',?1,?2,'test','card/v1','likely','snapshot','now',?3)",
            rusqlite::params![name, body, fingerprint],
        )?;
    }
    connection.execute(
        "INSERT INTO embedding_profiles(
           provider,model,config_fingerprint,dimensions,config_json
         ) VALUES('test','test','profile',2,'{}')",
        [],
    )?;
    let profile = ResolvedProfile {
        id: connection.last_insert_rowid(),
        dimensions: 2,
    };
    let documents = semantic_embedding_documents(&connection)?;
    for (document, vector) in documents
        .iter()
        .zip([vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]])
    {
        connection.execute(
            "INSERT INTO semantic_embeddings(document_hash,profile_id,vec)
             VALUES(?1,?2,?3)",
            rusqlite::params![document.document_hash, profile.id, vec_to_blob(&vector)],
        )?;
    }
    sync_semantic_vector_index(&connection, &profile, &documents)?;

    let scores = exact_semantic_vector_search(&connection, &profile, &[1.0_f32, 0.0], 2)?;
    assert_eq!(scores[0].0, documents[0].artifact_id);
    assert_eq!(
        connection.query_row(
            "SELECT count(*) FROM semantic_embedding_index_entries",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        2
    );
    let table = semantic_vector_table(2)?;
    assert_eq!(
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })?,
        2
    );
    assert!(!semantic_vector_index_has_gaps(
        &connection,
        profile.id,
        &table
    )?);
    connection.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [1])?;
    assert!(semantic_vector_index_has_gaps(
        &connection,
        profile.id,
        &table
    )?);
    Ok(())
}

#[test]
fn embedding_profile_versions_the_document_text_format() -> anyhow::Result<()> {
    let provider = Provider {
        name: "openai-compatible".into(),
        model: "tiny".into(),
        url: "https://example.test/v1/embeddings".into(),
        key: None,
        protocol: Protocol::OpenAi,
        query_prefix: String::new(),
        revision: None,
    };
    let profile = provider.profile()?;
    let config: serde_json::Value = serde_json::from_str(&profile.config_json)?;
    assert_eq!(config["document_text"], DOCUMENT_TEXT_FORMAT);

    let markdown_profile = provider.profile_for(crate::docs::CHUNK_FORMAT_VERSION)?;
    let markdown_config: serde_json::Value = serde_json::from_str(&markdown_profile.config_json)?;
    assert_eq!(
        markdown_config["document_text"],
        crate::docs::CHUNK_FORMAT_VERSION
    );
    assert_ne!(profile.fingerprint, markdown_profile.fingerprint);

    let old_config = serde_json::json!({
        "protocol": "openai-embeddings-v1",
        "url": "https://example.test/v1/embeddings",
        "query_prefix": ""
    })
    .to_string();
    assert_ne!(
        profile.fingerprint,
        profile_fingerprint("openai-compatible", "tiny", &old_config)
    );
    Ok(())
}

#[test]
fn missing_embeddings_are_selected_once_per_content_hash() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    for (path, hash, content) in [
        ("a.ts", "same", "export const x = 1;"),
        ("nested/a.ts", "same", "export const x = 1;"),
        ("b.ts", "other", "export const y = 2;"),
    ] {
        connection.execute(
            "INSERT INTO files(path,hash,role,origin,corpus,format)
             VALUES(?1,?2,'production','repository','code','typescript')",
            rusqlite::params![path, format!("file-{path}")],
        )?;
        let file_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO chunks(
               file_id, kind, scope_chain, symbols, start, end,
               start_line, end_line, hash, content
             ) VALUES(?1, 'module', '', '', 0, 1, 1, 1, ?2, ?3)",
            rusqlite::params![file_id, hash, content],
        )?;
    }

    let documents = missing_embedding_documents(
        &connection,
        "missing-profile",
        None,
        &["repository".into()],
        false,
    )?;
    assert_eq!(documents.len(), 2);
    assert_eq!(
        documents
            .iter()
            .map(|document| document.hash.as_str())
            .collect::<Vec<_>>(),
        ["other", "same"]
    );
    Ok(())
}

#[test]
fn fully_cached_pass_reports_reuse_and_synced_occurrences() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    let provider = Provider {
        name: "openai-compatible".into(),
        model: "tiny".into(),
        url: "https://example.test/v1/embeddings".into(),
        key: None,
        protocol: Protocol::OpenAi,
        query_prefix: String::new(),
        revision: None,
    };
    let profile = ensure_profile(&connection, &provider.profile()?, 2)?;
    for (path, hash, content) in [
        ("a.ts", "same", "export const x = 1;"),
        ("nested/a.ts", "same", "export const x = 1;"),
        ("b.ts", "other", "export const y = 2;"),
    ] {
        connection.execute(
            "INSERT INTO files(path,hash,role,origin,corpus,format)
             VALUES(?1,?2,'production','repository','code','typescript')",
            rusqlite::params![path, format!("file-{path}")],
        )?;
        let file_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO chunks(
               file_id, kind, scope_chain, symbols, start, end,
               start_line, end_line, hash, content
             ) VALUES(?1, 'module', '', '', 0, 1, 1, 1, ?2, ?3)",
            rusqlite::params![file_id, hash, content],
        )?;
    }
    for hash in ["same", "other"] {
        connection.execute(
            "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES(?1, ?2, ?3)",
            rusqlite::params![hash, profile.id, vec_to_blob(&[1.0, 0.0])],
        )?;
    }

    let code = embed_missing_for_selection_report(
        &connection,
        &provider,
        16,
        &["repository".into()],
        false,
        false,
    )?;
    assert_eq!(code.missing, 0);
    assert_eq!(code.embedded, 0);
    assert_eq!(code.cached_reused, 2);
    assert_eq!(code.occurrences_synced, 3);
    assert!(!code.canceled);

    connection.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type,canonical_name,body_json,model,prompt_version,
           confidence,source_snapshot,created_at,artifact_fingerprint
         ) VALUES('card','cache','{\"purpose\":\"cache\"}','test','card/v1',
                  'likely','snapshot','now','semantic')",
        [],
    )?;
    let documents = semantic_embedding_documents(&connection)?;
    assert_eq!(documents.len(), 1);
    connection.execute(
        "INSERT INTO semantic_embeddings(document_hash, profile_id, vec)
         VALUES(?1, ?2, ?3)",
        rusqlite::params![
            documents[0].document_hash,
            profile.id,
            vec_to_blob(&[1.0, 0.0])
        ],
    )?;

    let semantic = embed_semantic_missing_report(&connection, &provider, 16)?;
    assert_eq!(semantic.missing, 0);
    assert_eq!(semantic.embedded, 0);
    assert_eq!(semantic.cached_reused, 1);
    assert_eq!(semantic.occurrences_synced, 1);
    assert!(!semantic.canceled);
    Ok(())
}

#[test]
fn product_embedding_selection_uses_fresh_repository_policy_with_neutral_fallback()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    let mut file_ids = std::collections::BTreeMap::new();
    for (path, hash, deterministic_role) in [
        ("docs/runtime.ts", "runtime", "documentation"),
        ("src/tool.ts", "tooling", "production"),
        ("src/default.ts", "default", "production"),
        ("docs/default.ts", "docs-default", "documentation"),
        ("src/runtime.test.ts", "runtime-test", "test"),
    ] {
        connection.execute(
            "INSERT INTO files(path,hash,role,origin,corpus,format)
             VALUES(?1,?2,?3,'repository','code','typescript')",
            rusqlite::params![path, format!("file-{hash}"), deterministic_role],
        )?;
        let file_id = connection.last_insert_rowid();
        file_ids.insert(hash, file_id);
        connection.execute(
            "INSERT INTO chunks(
               file_id,kind,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'module','','',0,1,1,1,?2,?3)",
            rusqlite::params![file_id, hash, format!("content-{hash}")],
        )?;
    }
    insert_policy(&connection, file_ids["runtime"], "runtime", "runtime")?;
    insert_policy(&connection, file_ids["tooling"], "tooling", "tooling")?;
    insert_policy(
        &connection,
        file_ids["runtime-test"],
        "runtime",
        "runtime-test",
    )?;
    connection.execute(
        "UPDATE repository_file_policy SET effective_role='test'
         WHERE file_id=?1",
        [file_ids["runtime-test"]],
    )?;

    let documents = missing_embedding_documents(
        &connection,
        "missing-profile",
        None,
        &["repository".into()],
        true,
    )?;
    assert_eq!(
        documents
            .iter()
            .map(|document| document.hash.as_str())
            .collect::<Vec<_>>(),
        ["default", "runtime"]
    );
    Ok(())
}

#[test]
fn embedding_endpoints_reject_credentials_and_non_http_schemes() {
    assert!(validate_endpoint("http://127.0.0.1:8000/v1/embeddings").is_ok());
    assert!(validate_endpoint("https://gateway.example/v1/embeddings").is_ok());
    assert!(validate_endpoint("https://secret@gateway.example/v1/embeddings").is_err());
    assert!(validate_endpoint("file:///tmp/embeddings").is_err());
}

#[test]
fn sqlite_vec_materializes_current_chunk_occurrences() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('a.ts','f','production','repository','code','typescript')",
        [],
    )?;
    let file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [file_id],
    )?;
    let chunk_id = connection.last_insert_rowid();
    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".to_string(),
        model: "tiny".to_string(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;
    let table = vector_table(2)?;
    let found: (i64, f64) = connection.query_row(
        &format!(
            "SELECT i.chunk_id, v.distance FROM {table} v
             JOIN embedding_index_entries i ON i.id=v.rowid
             WHERE v.embedding MATCH ?1 AND v.k=1
               AND v.profile_id=?2 AND v.origin='repository'"
        ),
        rusqlite::params![vec_to_blob(&[1.0, 0.0]), profile.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(found.0, chunk_id);
    assert!(found.1 < 0.0001);

    connection.pragma_update(None, "query_only", true)?;
    assert_eq!(ready_search_profile(&connection, &spec)?.id, profile.id);
    connection.pragma_update(None, "query_only", false)?;

    assert!(!vector_index_needs_sync(&connection, profile.id)?);
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 2, 3, 2, 2, 'same', 'alpha')",
        [file_id],
    )?;
    assert!(
        vector_index_needs_sync(&connection, profile.id)?,
        "a new occurrence of cached content must invalidate materialization"
    );
    materialize_cached_embeddings(&connection)?;
    assert!(!vector_index_needs_sync(&connection, profile.id)?);
    let materialized: i64 = connection.query_row(
        "SELECT count(*) FROM embedding_index_entries WHERE profile_id=?1",
        [profile.id],
        |row| row.get(0),
    )?;
    assert_eq!(materialized, 2);

    crate::store::delete_file(&connection, file_id)?;
    let vector_rows: i64 =
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
    assert_eq!(vector_rows, 0, "file deletion must purge virtual rows");
    let cache_rows: i64 =
        connection.query_row("SELECT count(*) FROM embeddings", [], |row| row.get(0))?;
    assert_eq!(cache_rows, 1, "content-addressed cache should survive");
    Ok(())
}

#[test]
fn code_vectors_partition_knn_by_format_and_skip_ineligible_only_queries() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    let mut chunks = std::collections::BTreeMap::new();
    for (id, path, format, hash, vector) in [
        (
            1,
            "src/nearest.js",
            "javascript",
            "javascript-nearest",
            [1.0_f32, 0.0],
        ),
        (
            2,
            "src/other.js",
            "javascript",
            "javascript-other",
            [0.9_f32, 0.1],
        ),
        (
            3,
            "src/only.ts",
            "typescript",
            "typescript-only",
            [0.0_f32, 1.0],
        ),
    ] {
        connection.execute(
            "INSERT INTO files(id,path,hash,role,origin,corpus,format)
             VALUES(?1,?2,?3,'production','repository','code',?4)",
            rusqlite::params![id, path, format!("file-{hash}"), format],
        )?;
        connection.execute(
            "INSERT INTO chunks(
               file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'module','needle','','needle',0,6,1,1,?2,'needle')",
            rusqlite::params![id, hash],
        )?;
        chunks
            .entry(format)
            .or_insert_with(|| connection.last_insert_rowid());
        connection.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES(?1,?2,?3)",
            rusqlite::params![hash, profile.id, vec_to_blob(&vector)],
        )?;
    }
    materialize_cached_embeddings(&connection)?;

    let table = vector_table(profile.dimensions)?;
    let partitions = connection
        .prepare(&format!(
            "SELECT format,COUNT(*) FROM {table} GROUP BY format ORDER BY format"
        ))?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        partitions,
        [("javascript".to_string(), 2), ("typescript".to_string(), 1)]
    );

    let typescript_row: (i64, Vec<u8>) = connection.query_row(
        "SELECT entry.id,embedding.vec
         FROM embedding_index_entries entry
         JOIN chunks chunk ON chunk.id=entry.chunk_id
         JOIN files file ON file.id=chunk.file_id
         JOIN embeddings embedding
           ON embedding.chunk_hash=chunk.hash AND embedding.profile_id=entry.profile_id
         WHERE file.format='typescript'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    connection.execute(
        &format!("DELETE FROM {table} WHERE rowid=?1"),
        [typescript_row.0],
    )?;
    connection.execute(
        &format!(
            "INSERT INTO {table}(rowid,embedding,profile_id,origin,format)
             VALUES(?1,?2,?3,'repository',NULL)"
        ),
        rusqlite::params![typescript_row.0, typescript_row.1, profile.id],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;
    assert_eq!(
        connection.query_row(
            &format!("SELECT format FROM {table} WHERE rowid=?1"),
            [typescript_row.0],
            |row| row.get::<_, String>(0),
        )?,
        "typescript",
        "a full repair must restore a stale vector partition from file metadata"
    );

    let origins = ["repository".to_string()];
    let javascript = ["javascript".to_string()];
    let typescript = ["typescript".to_string()];
    let rust = ["rust".to_string()];
    assert_eq!(
        exact_vector_search(&connection, &profile, &[1.0, 0.0], 1, &origins, &javascript,)?
            .first()
            .map(|result| result.0),
        Some(chunks["javascript"])
    );
    assert_eq!(
        exact_vector_search(&connection, &profile, &[1.0, 0.0], 1, &origins, &typescript,)?
            .first()
            .map(|result| result.0),
        Some(chunks["typescript"]),
        "format must constrain sqlite-vec before k is applied"
    );
    assert!(
        exact_vector_search(&connection, &profile, &[1.0, 0.0], 10, &origins, &rust)?.is_empty()
    );
    assert_eq!(
        exact_vector_search(&connection, &profile, &[1.0, 0.0], 10, &origins, &[])?.len(),
        3,
        "an omitted format allowlist keeps every vector-capable code format"
    );

    let unconfigured_provider = Provider {
        name: "openai-compatible".into(),
        model: "not-materialized".into(),
        url: "https://example.test/v1/embeddings".into(),
        key: None,
        protocol: Protocol::OpenAi,
        query_prefix: String::new(),
        revision: None,
    };
    let skipped = vector_search(
        &connection,
        &unconfigured_provider,
        "needle",
        10,
        &origins,
        &rust,
    )?;
    assert!(skipped.ranking.is_empty());
    assert_eq!(skipped.timings.embedding_query, std::time::Duration::ZERO);
    assert_eq!(skipped.timings.vector_index, std::time::Duration::ZERO);
    Ok(())
}

#[test]
fn documentation_hash_collision_never_materializes_in_code_vectors() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute_batch(
        "INSERT INTO files(id,path,hash,role,origin,corpus,format)
           VALUES(1,'src/main.ts','code-file','production','repository','code','typescript');
         INSERT INTO chunks(
           id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(1,1,'function','run','','run',0,5,1,1,'collision','alpha');
         INSERT INTO files(id,path,hash,role,origin,corpus,format)
           VALUES(2,'README.md','doc-file','documentation','repository','docs','markdown');
         INSERT INTO chunks(
           id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(2,2,'markdown_section',NULL,'','',0,5,1,1,'collision','alpha');
         INSERT INTO doc_chunk_meta(
           chunk_id,title,breadcrumb,nearest_heading,ordinal,
           embedding_identity,front_matter_state
         ) VALUES(2,'README','Guide','Guide',0,'doc-identity','absent');",
    )?;
    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES('collision',?1,?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;

    materialize_cached_embeddings(&connection)?;
    let materialized = connection
        .prepare(
            "SELECT entry.chunk_id
             FROM embedding_index_entries entry
             WHERE entry.profile_id=?1 ORDER BY entry.chunk_id",
        )?
        .query_map([profile.id], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(materialized, [1]);
    assert_eq!(
        connection.query_row(
            "SELECT count(*) FROM doc_embedding_index_entries",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );

    let missing = missing_embedding_documents(
        &connection,
        &spec.fingerprint,
        Some(profile.id),
        &["repository".into()],
        false,
    )?;
    assert!(missing.is_empty());
    Ok(())
}

#[test]
fn rust_chunks_are_not_code_embedding_candidates_or_vector_entries() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    let mut chunk_ids = std::collections::BTreeMap::new();
    for (path, format, hash, content) in [
        (
            "src/eligible.ts",
            "typescript",
            "typescript-hash",
            "export const collisionNeedle = true;",
        ),
        (
            "src/ineligible.rs",
            "rust",
            "rust-hash",
            "pub const collisionNeedle: bool = true;",
        ),
    ] {
        connection.execute(
            "INSERT INTO files(path,hash,role,origin,corpus,format)
             VALUES(?1,?2,'production','repository','code',?3)",
            rusqlite::params![path, format!("file-{hash}"), format],
        )?;
        let file_id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO chunks(
               file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
             ) VALUES(?1,'module','collisionNeedle','','collisionNeedle',0,40,1,1,?2,?3)",
            rusqlite::params![file_id, hash, content],
        )?;
        chunk_ids.insert(format, connection.last_insert_rowid());
    }

    let candidates = missing_embedding_documents(
        &connection,
        "missing-profile",
        None,
        &["repository".into()],
        false,
    )?;
    assert_eq!(
        candidates
            .iter()
            .map(|document| document.hash.as_str())
            .collect::<Vec<_>>(),
        ["typescript-hash"]
    );

    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    for (hash, vector) in [
        ("typescript-hash", [0.0_f32, 1.0]),
        ("rust-hash", [1.0_f32, 0.0]),
    ] {
        connection.execute(
            "INSERT INTO embeddings(chunk_hash,profile_id,vec) VALUES(?1,?2,?3)",
            rusqlite::params![hash, profile.id, vec_to_blob(&vector)],
        )?;
    }

    materialize_cached_embeddings(&connection)?;
    let indexed_formats = || -> anyhow::Result<Vec<String>> {
        let mut statement = connection.prepare(
            "SELECT file.format
             FROM embedding_index_entries entry
             JOIN chunks chunk ON chunk.id=entry.chunk_id
             JOIN files file ON file.id=chunk.file_id
             WHERE entry.profile_id=?1
             ORDER BY file.path",
        )?;
        let rows = statement.query_map([profile.id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    };
    assert_eq!(indexed_formats()?, ["typescript"]);

    sync_vector_index(&connection, Some(profile.id))?;
    assert_eq!(indexed_formats()?, ["typescript"]);
    let table = vector_table(profile.dimensions)?;
    assert_eq!(
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })?,
        1
    );

    connection.execute(
        "INSERT INTO embedding_index_entries(chunk_id,profile_id) VALUES(?1,?2)",
        rusqlite::params![chunk_ids["rust"], profile.id],
    )?;
    let stale_row_id = connection.last_insert_rowid();
    connection.execute(
        &format!(
            "INSERT INTO {table}(rowid,embedding,profile_id,origin,format)
             VALUES(?1,?2,?3,'repository','rust')"
        ),
        rusqlite::params![stale_row_id, vec_to_blob(&[1.0, 0.0]), profile.id],
    )?;
    let surfaced = exact_vector_search(
        &connection,
        &profile,
        &[1.0, 0.0],
        2,
        &["repository".into()],
        &[],
    )?;
    assert_eq!(
        surfaced
            .iter()
            .map(|(chunk_id, _)| *chunk_id)
            .collect::<Vec<_>>(),
        [chunk_ids["typescript"]]
    );
    assert!(vector_index_needs_sync(&connection, profile.id)?);

    synchronize_vector_index(&connection, &profile, false)?;
    assert_eq!(indexed_formats()?, ["typescript"]);
    assert_eq!(
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })?,
        1
    );
    assert!(!vector_index_needs_sync(&connection, profile.id)?);
    Ok(())
}

#[test]
fn documentation_file_deletion_purges_only_its_vector_occurrence() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute_batch(
        "INSERT INTO embedding_profiles(
           id,provider,model,config_fingerprint,dimensions,config_json
         ) VALUES(1,'test','tiny','profile',2,'{}');
         INSERT INTO embeddings(chunk_hash,profile_id,vec)
           VALUES('identity-a',1,x'0000000000000000');
         INSERT INTO files(id,path,hash,role,origin,corpus,format)
           VALUES(1,'a.md','file-a','documentation','repository','docs','markdown');
         INSERT INTO files(id,path,hash,role,origin,corpus,format)
           VALUES(2,'b.md','file-b','documentation','repository','docs','markdown');
         INSERT INTO chunks(
           id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(1,1,'markdown_section',NULL,'','',0,1,1,1,'source-a','a');
         INSERT INTO chunks(
           id,file_id,kind,name,scope_chain,symbols,start,end,start_line,end_line,hash,content
         ) VALUES(2,2,'markdown_section',NULL,'','',0,1,1,1,'source-b','b');
         INSERT INTO doc_chunk_meta(
           chunk_id,title,breadcrumb,nearest_heading,ordinal,
           embedding_identity,front_matter_state
         ) VALUES(1,'A','','A',0,'identity-a','absent');
         INSERT INTO doc_chunk_meta(
           chunk_id,title,breadcrumb,nearest_heading,ordinal,
           embedding_identity,front_matter_state
         ) VALUES(2,'B','','B',0,'identity-b','absent');
         INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
           VALUES(1,'A','','','a','a.md');
         INSERT INTO docs_fts(rowid,title,metadata,breadcrumb,body,path)
           VALUES(2,'B','','','b','b.md');
         INSERT INTO doc_embedding_index_entries(id,chunk_id,profile_id)
           VALUES(1,1,1);
         INSERT INTO doc_embedding_index_entries(id,chunk_id,profile_id)
           VALUES(2,2,1);
         INSERT INTO doc_vector_generations(
           snapshot,profile_id,dimensions,chunk_format_version
         ) VALUES('snapshot',1,2,'documentation-v1');
         CREATE VIRTUAL TABLE vec_doc_embeddings_2 USING vec0(
           embedding FLOAT[2] distance_metric=cosine,
           profile_id INTEGER PARTITION KEY
         );
         INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id)
           VALUES(1,x'0000000000000000',1);
         INSERT INTO vec_doc_embeddings_2(rowid,embedding,profile_id)
           VALUES(2,x'0000000000000000',1);",
    )?;

    crate::store::delete_file(&connection, 1)?;

    let state: (i64, i64, i64, i64, i64, i64) = connection.query_row(
        "SELECT
           (SELECT count(*) FROM files),
           (SELECT count(*) FROM doc_chunk_meta),
           (SELECT count(*) FROM docs_fts),
           (SELECT count(*) FROM doc_embedding_index_entries),
           (SELECT count(*) FROM vec_doc_embeddings_2),
           (SELECT count(*) FROM doc_vector_generations)",
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
    assert_eq!(state, (1, 1, 1, 1, 1, 0));
    assert_eq!(
        connection.query_row("SELECT count(*) FROM embeddings", [], |row| row
            .get::<_, i64>(0))?,
        1,
        "shared embedding cache must survive documentation file deletion"
    );
    Ok(())
}

#[test]
fn incremental_vector_sync_materializes_a_reused_chunk_rowid() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('old.ts','old-file','production','repository','code','typescript')",
        [],
    )?;
    let old_file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [old_file_id],
    )?;
    let old_chunk_id = connection.last_insert_rowid();

    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;
    let table = vector_table(profile.dimensions)?;
    assert!(vector_index_has_completed_sync(&connection, profile.id)?);

    crate::store::delete_file(&connection, old_file_id)?;
    assert!(
        vector_index_has_completed_sync(&connection, profile.id)?,
        "ordinary source deletion must preserve the completion marker"
    );
    let durable_rows: i64 = connection.query_row(
        "SELECT count(*) FROM embedding_index_entries WHERE profile_id=?1",
        [profile.id],
        |row| row.get(0),
    )?;
    let vector_rows: i64 =
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
    assert_eq!((durable_rows, vector_rows), (0, 0));

    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('new.ts','new-file','production','repository','code','typescript')",
        [],
    )?;
    let new_file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [new_file_id],
    )?;
    let new_chunk_id = connection.last_insert_rowid();
    assert_eq!(
        new_chunk_id, old_chunk_id,
        "fixture must exercise SQLite rowid reuse"
    );

    synchronize_vector_index(&connection, &profile, false)?;
    let materialized_chunk_id: i64 = connection.query_row(
        &format!(
            "SELECT entry.chunk_id
             FROM embedding_index_entries entry
             JOIN {table} vector ON vector.rowid=entry.id
             WHERE entry.profile_id=?1"
        ),
        [profile.id],
        |row| row.get(0),
    )?;
    assert_eq!(materialized_chunk_id, new_chunk_id);
    assert!(!vector_index_needs_sync(&connection, profile.id)?);
    Ok(())
}

#[test]
fn incremental_vector_sync_leaves_full_audit_to_explicit_repair() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('a.ts','file-a','production','repository','code','typescript')",
        [],
    )?;
    let file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [file_id],
    )?;
    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;
    let table = vector_table(profile.dimensions)?;

    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 2, 3, 2, 2, 'same', 'alpha')",
        [file_id],
    )?;
    synchronize_vector_index(&connection, &profile, false)?;
    let counts = || -> anyhow::Result<(i64, i64)> {
        Ok((
            connection.query_row(
                "SELECT count(*) FROM embedding_index_entries WHERE profile_id=?1",
                [profile.id],
                |row| row.get(0),
            )?,
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?,
        ))
    };
    assert_eq!(counts()?, (2, 2));

    connection.execute(&format!("DROP TABLE {table}"), [])?;
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 4, 5, 3, 3, 'same', 'alpha')",
        [file_id],
    )?;
    synchronize_vector_index(&connection, &profile, false)?;
    assert_eq!(
        counts()?,
        (3, 3),
        "a missing virtual table requires a complete rebuild before incremental publication"
    );

    let missing_row: i64 = connection.query_row(
        "SELECT min(id) FROM embedding_index_entries WHERE profile_id=?1",
        [profile.id],
        |row| row.get(0),
    )?;
    connection.execute(
        &format!("DELETE FROM {table} WHERE rowid=?1"),
        [missing_row],
    )?;
    synchronize_vector_index(&connection, &profile, false)?;
    assert_eq!(
        counts()?,
        (3, 2),
        "incremental synchronization must not pay for a full virtual-row audit"
    );
    synchronize_vector_index(&connection, &profile, true)?;
    assert_eq!(counts()?, (3, 3));

    connection.execute(&format!("DROP TABLE {table}"), [])?;
    ensure_profile(&connection, &spec, profile.dimensions)?;
    assert!(
        !vector_table_exists(&connection, profile.dimensions)?,
        "resolving an existing profile must not recreate an empty vector table"
    );
    synchronize_vector_index(&connection, &profile, false)?;
    assert_eq!(counts()?, (3, 3));

    connection.execute(&format!("DROP TABLE {table}"), [])?;
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 6, 7, 4, 4, 'same', 'alpha')",
        [file_id],
    )?;
    materialize_cached_embeddings(&connection)?;
    assert_eq!(
        counts()?,
        (4, 4),
        "index-time materialization must also rebuild a missing completed table"
    );
    Ok(())
}

#[test]
fn missing_vector_table_search_reports_repair_and_recovers() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('a.ts','f','production','repository','code','typescript')",
        [],
    )?;
    let file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [file_id],
    )?;
    let chunk_id = connection.last_insert_rowid();

    let provider = Provider {
        name: "openai-compatible".into(),
        model: "tiny".into(),
        url: "https://example.test/v1/embeddings".into(),
        key: None,
        protocol: Protocol::OpenAi,
        query_prefix: String::new(),
        revision: None,
    };
    let spec = provider.profile()?;
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;

    let table = vector_table(profile.dimensions)?;
    connection.execute(&format!("DROP TABLE {table}"), [])?;
    let failure = vector_search(
        &connection,
        &provider,
        "alpha",
        1,
        &["repository".into()],
        &[],
    )
    .expect_err("search must fail closed when the vector table is missing");
    assert!(
        failure
            .to_string()
            .contains("vector index table is missing")
    );
    assert_eq!(
        code_vector_failure_action(&failure),
        "run jscout embed <root> --repair"
    );

    synchronize_vector_index(&connection, &profile, true)?;

    connection.pragma_update(None, "query_only", true)?;
    let ready = ready_search_profile(&connection, &spec)?;
    let results = exact_vector_search(
        &connection,
        &ready,
        &[1.0, 0.0],
        1,
        &["repository".into()],
        &[],
    )?;
    assert_eq!(results.first().map(|result| result.0), Some(chunk_id));
    Ok(())
}

#[test]
fn failed_marker_invalidation_does_not_publish_an_empty_vector_table() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = crate::store::open(directory.path())?;
    connection.execute(
        "INSERT INTO files(path,hash,role,origin,corpus,format)
         VALUES('a.ts','f','production','repository','code','typescript')",
        [],
    )?;
    let file_id = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO chunks(
           file_id, kind, scope_chain, symbols, start, end, start_line, end_line, hash, content
         ) VALUES(?1, 'function', '', '', 0, 1, 1, 1, 'same', 'alpha')",
        [file_id],
    )?;
    let config_json = "{}".to_string();
    let spec = ProfileSpec {
        provider: "test".into(),
        model: "tiny".into(),
        fingerprint: profile_fingerprint("test", "tiny", &config_json),
        config_json,
        dimensions: Some(2),
    };
    let profile = ensure_profile(&connection, &spec, 2)?;
    connection.execute(
        "INSERT INTO embeddings(chunk_hash, profile_id, vec) VALUES('same', ?1, ?2)",
        rusqlite::params![profile.id, vec_to_blob(&[1.0, 0.0])],
    )?;
    sync_vector_index(&connection, Some(profile.id))?;
    let table = vector_table(profile.dimensions)?;
    connection.execute(&format!("DROP TABLE {table}"), [])?;
    connection.execute_batch(
        "CREATE TRIGGER reject_vector_marker_delete
         BEFORE DELETE ON meta
         WHEN OLD.key LIKE 'embedding_index_synced_v1:%'
         BEGIN SELECT RAISE(ABORT, 'marker delete rejected'); END;",
    )?;

    let failure = synchronize_vector_index(&connection, &profile, false)
        .expect_err("forced marker invalidation failure");
    assert!(failure.to_string().contains("marker delete rejected"));
    assert!(
        !vector_table_exists(&connection, profile.dimensions)?,
        "a failed invalidation must not publish an empty table beside a stale ready marker"
    );
    let readiness = ready_search_profile(&connection, &spec)
        .expect_err("the missing table must remain fail-closed");
    assert!(
        readiness
            .to_string()
            .contains("vector index table is missing")
    );
    Ok(())
}
