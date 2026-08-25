use anyhow::{Context, Result};

use crate::cli::DocsCommand;
use crate::config;
use crate::docs::{retrieval, store};
use crate::{embed, search};

use super::core::{open_database_for_write, open_database_read_only};

pub(super) fn run(command: DocsCommand, runtime: &config::RuntimeConfig) -> Result<()> {
    let configured_database = runtime.effective.database.path.as_path();
    match command {
        DocsCommand::Status {
            root,
            database,
            json,
        } => {
            if !runtime.effective.docs.enabled {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "enabled": false, "active_corpus": false })
                    );
                } else {
                    println!("documentation: disabled ([docs] enabled = false)");
                }
                return Ok(());
            }
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let status = store::status(&conn)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "shared snapshot: {} root={}",
                    status.snapshot,
                    status.canonical_root.as_deref().unwrap_or("unknown")
                );
                println!(
                    "documentation: files={} chunks={} embeddable={} rejections={} cached_embeddings={} ready_vector_generations={}",
                    status.indexed_file_count,
                    status.chunk_count,
                    status.embeddable_chunk_count,
                    status.rejection_count,
                    status.cached_embedding_count,
                    status.ready_vector_generation_count,
                );
                for decision in &status.decisions {
                    if let (Some(path_base64), Some(path_encoding)) =
                        (&decision.path_base64, &decision.path_encoding)
                    {
                        println!(
                            "{}\t{}\t{}\tpath_encoding={}\tpath_base64={}",
                            decision.rule,
                            decision.subject,
                            decision.path,
                            path_encoding,
                            path_base64
                        );
                    } else {
                        println!("{}\t{}\t{}", decision.rule, decision.subject, decision.path);
                    }
                }
                for front_matter in status
                    .front_matter
                    .iter()
                    .filter(|entry| entry.state == "malformed_as_body")
                {
                    println!(
                        "front_matter={}\tfile\t{}",
                        front_matter.state, front_matter.path
                    );
                }
            }
            Ok(())
        }
        DocsCommand::Embed {
            root,
            database,
            batch,
            json,
        } => {
            ensure_docs_enabled(runtime, "embedding")?;
            let provider = embed::Provider::from_settings(
                &runtime.effective.embedding,
                &runtime.effective.inference,
            )?
            .context(
                "documentation embedding requires the repository [embedding] provider; BM25 search does not",
            )?;
            let conn = open_database_for_write(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let report = retrieval::embed_current(
                &conn,
                &provider,
                batch.unwrap_or(runtime.effective.embedding.batch),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "documentation vectors: snapshot={} embedded={} cached={} occurrences={} dimensions={} ready={}",
                    report.snapshot,
                    report.embedded,
                    report.cached_reused,
                    report.occurrences_materialized,
                    report
                        .dimensions
                        .map_or_else(|| "none".to_string(), |value| value.to_string()),
                    report.generation_published,
                );
            }
            Ok(())
        }
        DocsCommand::Search {
            root,
            query,
            database,
            limit,
            vector,
            no_vector,
            lexical_only,
            rerank,
            no_rerank,
            response_bytes,
            json,
            debug_json,
        } => {
            ensure_docs_enabled(runtime, "search")?;
            let defaults = &runtime.effective.docs.search;
            let use_vector = resolve_flag(vector, no_vector || lexical_only, defaults.vector);
            let use_reranker = resolve_flag(rerank, no_rerank || lexical_only, defaults.rerank);
            let provider = if use_vector {
                match embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                ) {
                    Ok(provider) => provider,
                    Err(error) if !vector => {
                        eprintln!(
                            "warning: documentation vectors unavailable ({error}); using BM25"
                        );
                        None
                    }
                    Err(error) => return Err(error).context("resolve required vector provider"),
                }
            } else {
                None
            };
            let reranker = use_reranker.then(|| {
                search::Reranker::from_settings(
                    &runtime.effective.reranker,
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )
            });
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let result = retrieval::search(
                &conn,
                &root,
                provider.as_ref(),
                &query,
                &retrieval::SearchOptions {
                    limit: limit.unwrap_or(defaults.limit),
                    response_bytes: response_bytes.unwrap_or(if debug_json {
                        usize::MAX
                    } else {
                        defaults.response_bytes
                    }),
                    output: if debug_json {
                        retrieval::SearchOutput::Pretty
                    } else if json {
                        retrieval::SearchOutput::Compact
                    } else {
                        retrieval::SearchOutput::Human
                    },
                    vector: use_vector,
                    vector_required: vector,
                    rerank: use_reranker,
                    reranker: reranker.flatten(),
                },
            )?;
            if debug_json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if json {
                print!("{}", retrieval::compact_search_string(&result)?);
            } else {
                print!("{}", retrieval::human_search_string(&result));
            }
            Ok(())
        }
    }
}

fn ensure_docs_enabled(runtime: &config::RuntimeConfig, operation: &str) -> Result<()> {
    if !runtime.effective.docs.enabled {
        anyhow::bail!(
            "documentation {operation} is disabled by `[docs] enabled = false`; enable it and run `jscout index`"
        );
    }
    Ok(())
}

const fn resolve_flag(enable: bool, disable: bool, configured: bool) -> bool {
    if disable { false } else { enable || configured }
}
