use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::DocsCommand;
use crate::config;
use crate::docs::{corpus, retrieval, store};
use crate::{embed, search};

pub(super) fn run(command: DocsCommand, runtime: &config::RuntimeConfig) -> Result<()> {
    match command {
        DocsCommand::Index {
            root,
            database,
            json,
        } => {
            let options = corpus::CorpusOptions {
                include: runtime.effective.docs.include.clone(),
                exclude: runtime.effective.docs.exclude.clone(),
                ..corpus::CorpusOptions::default()
            };
            let captured = corpus::scan(&root, &options)?;
            let mut docs_store = store::DocsStore::open(
                &root,
                Some(documentation_database(
                    database.as_deref(),
                    &runtime.effective.docs.database.path,
                )),
                Some(&runtime.effective.database.path),
            )?;
            let publication = docs_store.publish(&captured)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&publication)?);
            } else {
                println!(
                    "published documentation snapshot {}: {} files, {} blocks, {} chunks, {} rejections",
                    publication.snapshot_id,
                    publication.indexed_file_count,
                    publication.block_count,
                    publication.chunk_count,
                    publication.rejection_count,
                );
            }
            Ok(())
        }
        DocsCommand::Status {
            root,
            database,
            json,
        } => {
            let docs_store = store::DocsStore::open_read_only(
                &root,
                Some(documentation_database(
                    database.as_deref(),
                    &runtime.effective.docs.database.path,
                )),
                Some(&runtime.effective.database.path),
            )?;
            let status = docs_store.status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("documentation database: {}", status.database_path);
                println!(
                    "snapshot: {} files={} blocks={} chunks={} rejections={} embeddings={} ready_vector_generations={}",
                    status
                        .snapshot_id
                        .map_or_else(|| "none".to_string(), |id| id.to_string()),
                    status.indexed_file_count,
                    status.block_count,
                    status.chunk_count,
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
            let provider = embed::Provider::from_settings(
                &runtime.effective.embedding,
                &runtime.effective.inference,
            )?
            .context(
                "documentation embedding requires the repository [embedding] provider; BM25 search does not",
            )?;
            let mut docs_store = store::DocsStore::open(
                &root,
                Some(documentation_database(
                    database.as_deref(),
                    &runtime.effective.docs.database.path,
                )),
                Some(&runtime.effective.database.path),
            )?;
            let report = retrieval::embed_current(
                &mut docs_store,
                &provider,
                batch.unwrap_or(runtime.effective.embedding.batch),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "documentation vectors: snapshot={} embedded={} cached={} occurrences={} dimensions={} ready={}",
                    report.snapshot_id,
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
            no_freshness,
            response_bytes,
            json,
            debug_json,
        } => {
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
            let docs_store = store::DocsStore::open_read_only(
                &root,
                Some(documentation_database(
                    database.as_deref(),
                    &runtime.effective.docs.database.path,
                )),
                Some(&runtime.effective.database.path),
            )?;
            let result = retrieval::search(
                &docs_store,
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
                    freshness: runtime.effective.docs.freshness && !no_freshness,
                    max_rank_movement: runtime.effective.docs.max_rank_movement,
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

fn documentation_database<'a>(explicit: Option<&'a Path>, configured: &'a Path) -> &'a Path {
    explicit.unwrap_or(configured)
}

const fn resolve_flag(enable: bool, disable: bool, configured: bool) -> bool {
    if disable { false } else { enable || configured }
}
