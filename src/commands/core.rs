use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    calls, chunk, compact, config, embed, formats, indexer, mcp, parse, query, search, semantic,
    stats, store, structural, walk,
};

pub(super) fn open_database_for_write(
    root: &Path,
    database: Option<&Path>,
) -> Result<rusqlite::Connection> {
    match database {
        Some(path) => store::open_path(path),
        None => store::open(root),
    }
}

pub(super) fn open_database_read_only(
    root: &Path,
    database: Option<&Path>,
) -> Result<rusqlite::Connection> {
    match database {
        Some(path) => store::open_path_read_only(path),
        None => store::open_read_only(root),
    }
}

pub(super) fn cmd_neighborhood(
    root: &Path,
    database: &Path,
    anchor: &str,
    response_bytes: Option<usize>,
    debug_json: bool,
    options: structural::NeighborhoodOptions,
) -> Result<()> {
    let conn = open_database_read_only(root, Some(database))?;
    let neighborhood = structural::neighborhood(&conn, anchor, &options)?;
    println!(
        "{}",
        render_cli_neighborhood(&neighborhood, response_bytes, debug_json)?
    );
    Ok(())
}

pub(super) fn render_cli_neighborhood(
    neighborhood: &structural::Neighborhood,
    response_bytes: Option<usize>,
    debug_json: bool,
) -> Result<String> {
    Ok(if debug_json && response_bytes.is_none() {
        serde_json::to_string_pretty(&neighborhood)?
    } else if debug_json {
        mcp::render_bounded_object_arrays(
            serde_json::to_value(neighborhood)?,
            &["edges", "nodes"],
            response_bytes.expect("checked above"),
        )?
    } else {
        compact::render_neighborhood(
            neighborhood,
            response_bytes.unwrap_or(search::DEFAULT_RESPONSE_BYTE_LIMIT),
        )?
    })
}

pub(super) struct EmbedCommandOptions<'a> {
    pub(super) batch: usize,
    pub(super) file_origins: &'a [String],
    pub(super) product: bool,
    pub(super) semantic: bool,
    pub(super) semantic_only: bool,
    pub(super) repair: bool,
}

pub(super) fn cmd_embed(
    root: &Path,
    database: Option<&Path>,
    options: EmbedCommandOptions<'_>,
    runtime: &config::RuntimeConfig,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let Some(provider) =
        embed::Provider::from_settings(&runtime.effective.embedding, &runtime.effective.inference)?
    else {
        anyhow::bail!("no embedding provider configured — set embedding.provider in .jscout.toml");
    };
    eprintln!("provider: {} model: {}", provider.name, provider.model);
    if !options.semantic_only {
        let report = embed::embed_missing_for_selection_report(
            &conn,
            &provider,
            options.batch,
            options.file_origins,
            options.product,
            options.repair,
        )?;
        println!(
            "code embeddings: missing={} embedded={} cached_reused={} occurrences_synced={}",
            report.missing, report.embedded, report.cached_reused, report.occurrences_synced
        );
    }
    if options.semantic || options.semantic_only {
        let report = embed::embed_semantic_missing_report(&conn, &provider, options.batch)?;
        println!(
            "semantic embeddings: missing={} embedded={} cached_reused={} occurrences_synced={}",
            report.missing, report.embedded, report.cached_reused, report.occurrences_synced
        );
    }
    Ok(())
}

pub(super) fn cmd_search(
    root: &Path,
    database: Option<&Path>,
    query: &str,
    provider: Option<&embed::Provider>,
    json: bool,
    debug_json: bool,
    options: search::SearchOptions,
) -> Result<()> {
    let conn = open_database_read_only(root, database)?;
    let result = search::search(&conn, provider, query, &options)?;
    if json {
        println!("{}", compact::search_string(&result)?);
        return Ok(());
    }
    if debug_json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("snapshot: {}", result.snapshot);
    println!(
        "retrieval: lexical={} vector={} reranker={}",
        result.retrieval.lexical, result.retrieval.vector, result.retrieval.reranker
    );
    if let Some(exhaustive) = &result.exhaustive {
        println!(
            "exhaustive: returned={} total_chunks={} truncated={} page_size={}",
            exhaustive.returned,
            exhaustive.total_chunks,
            exhaustive.truncated,
            exhaustive.effective.page_size,
        );
        println!("scope: {}", serde_json::to_string(&exhaustive.scope)?);
        for warning in &exhaustive.warnings {
            println!(
                "warning {}: {} terms={} total_chunks={}",
                warning.code,
                warning.message,
                warning.terms.join(","),
                warning.total_chunks
            );
        }
        if let Some(cursor) = &exhaustive.next_cursor {
            println!("next cursor: {cursor}");
        }
    }
    if let Some(action) = result.retrieval.vector_action {
        println!("vector action: {action}");
    }
    if let Some(attachment) = &result.semantic_attachment {
        println!(
            "semantic attachment: {} ({} connected candidates; graph depth {}, {} nodes{})",
            attachment.status,
            attachment.connected_candidates,
            attachment.graph_depth,
            attachment.graph_nodes,
            if attachment.graph_truncated {
                ", truncated"
            } else {
                ""
            }
        );
    }
    if result.hits.is_empty() && result.semantic_artifacts.is_empty() {
        println!("no results");
        return Ok(());
    }
    let exhaustive = result.exhaustive.is_some();
    for (i, h) in result.hits.iter().enumerate() {
        if exhaustive {
            let match_lines = h
                .match_lines
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{:2}. {}:{}-{} [{}] match_lines={match_lines}",
                i + 1,
                h.file,
                h.start_line,
                h.end_line,
                h.kind,
            );
            println!("      anchors: {}", h.anchors.join(", "));
            continue;
        }
        let name = h
            .name
            .as_deref()
            .map(|n| format!(" {n}"))
            .unwrap_or_default();
        println!(
            "{:2}. {}:{}-{} [{}{}] score={:.4}",
            i + 1,
            h.file,
            h.start_line,
            h.end_line,
            h.kind,
            name,
            h.score
        );
        for line in h.snippet.lines() {
            println!("      {line}");
        }
        if !h.uses.is_empty() {
            println!("      → uses: {}", h.uses.join(", "));
        }
        if !h.used_by.is_empty() {
            println!("      ← used by: {}", h.used_by.join(", "));
        }
        println!("      anchors: {}", h.anchors.join(", "));
    }
    if let Some(expansion) = &result.expansion {
        println!(
            "\nstructural expansion ({}): {} nodes, {} edges, {} bytes{}",
            expansion.projection.as_str(),
            expansion.nodes.len(),
            expansion.edges.len(),
            expansion.payload_bytes,
            if expansion.truncated {
                " (truncated)"
            } else {
                ""
            }
        );
        for edge in &expansion.edges {
            println!(
                "  {} -[{}:{}]-> {}",
                edge.source, edge.kind, edge.confidence, edge.target
            );
        }
    }
    if !result.semantic_artifacts.is_empty() {
        print!(
            "{}",
            render_semantic_memory_text(&result.semantic_artifacts)?
        );
    }
    Ok(())
}

pub(super) fn render_semantic_memory_text(
    artifacts: &[semantic::SemanticArtifact],
) -> Result<String> {
    let mut rendered = String::from("\nsemantic memory (untrusted; verify in source):\n");
    for artifact in artifacts {
        let _ = writeln!(
            rendered,
            "  #{} {} {} [{}] confidence={}",
            artifact.id,
            artifact.artifact_type,
            artifact.name.as_deref().unwrap_or("<unnamed>"),
            artifact.freshness,
            artifact.confidence,
        );
        rendered.push_str("      ");
        rendered.push_str(&serde_json::to_string(&artifact.body)?);
        rendered.push('\n');
    }
    Ok(rendered)
}

pub(super) fn cmd_index(
    root: &Path,
    database: Option<&Path>,
    dependencies: &[String],
    docs: &config::DocsSettings,
    diagnostics: &config::DiagnosticsSettings,
) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = open_database_for_write(root, database)?;
    let o = indexer::refresh_repo_with_options(
        root,
        &conn,
        &indexer::IndexOptions {
            dependencies: dependencies.to_vec(),
            docs_include: docs.indexing_include().to_vec(),
            docs_exclude: docs.indexing_exclude().to_vec(),
            docs_freshness: docs.indexing_freshness(),
            timing: diagnostics.timing,
            debug: diagnostics.debug,
            ..Default::default()
        },
    )?;
    // Manual `jscout index` is always a full snapshot refresh, so an
    // "unchanged" count would always read 0 and misreport the rebuild as failed
    // change detection. Watch reports reuse for its incremental generations.
    println!(
        "indexed {} files (removed={}, rejected={}) — {} chunks, {} refs in {:?}",
        o.indexed,
        o.removed,
        o.rejected,
        o.chunks,
        o.refs,
        started.elapsed()
    );
    if o.extraction_reset {
        println!("snapshot refresh: rebuilt disposable structural state");
    }
    if o.rust_files_with_parse_errors > 0 {
        println!(
            "Rust parse diagnostics: {} files, {} errors (indexed)",
            o.rust_files_with_parse_errors, o.rust_parse_error_count
        );
    }
    indexer::report_rejections(&o);
    if !dependencies.is_empty() {
        println!(
            "dependency corpus: {} packages, {} files / {} bytes, {} files / {} bytes skipped",
            o.dependency_packages,
            o.dependency_files,
            o.dependency_bytes,
            o.dependency_skipped,
            o.dependency_skipped_bytes
        );
        for plan in &o.dependency_plans {
            println!("  {plan}");
        }
    }
    Ok(())
}

pub(super) fn cmd_calls(
    root: &Path,
    database: Option<&Path>,
    query: &calls::CallQuery,
    json: bool,
) -> Result<()> {
    let conn = open_database_read_only(root, database)?;
    let result = calls::query(root, &conn, query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if result.matches.is_empty() {
        println!(
            "no matching call sites ({} candidate files scanned)",
            result.files_scanned
        );
        return Ok(());
    }
    for site in &result.matches {
        let receiver = site.receiver.as_deref().unwrap_or("<expr>");
        let options = site
            .matched_options
            .iter()
            .map(|option| match &option.value {
                Some(value) => format!("{}: {value}", option.key),
                None => option.key.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let argument = site
            .matched_argument
            .map(|position| format!("  [arg {position}: {options}]"))
            .unwrap_or_default();
        let anchor = site
            .anchor
            .as_deref()
            .map(|anchor| format!("  ({anchor})"))
            .unwrap_or_default();
        println!(
            "{}:{}-{}  {receiver}.{}({} args){argument}{anchor}",
            site.file, site.start_line, site.end_line, site.method, site.argument_count,
        );
    }
    println!(
        "\n{} match(es) in {} candidate file(s){}",
        result.matches.len(),
        result.files_scanned,
        if result.truncated {
            "; truncated by --limit"
        } else {
            ""
        }
    );
    Ok(())
}

pub(super) fn cmd_events(
    root: &Path,
    database: &Path,
    name: Option<&str>,
    file_origins: &[String],
) -> Result<()> {
    let conn = open_database_read_only(root, Some(database))?;
    let sites = query::events_in_origins(&conn, name, file_origins)?;
    if sites.is_empty() {
        println!("no event sites found");
        return Ok(());
    }
    let mut current = String::new();
    for s in &sites {
        if s.name != current {
            current.clone_from(&s.name);
            println!("\nevent '{current}'");
        }
        let ctx = s
            .chunk_name
            .as_deref()
            .map(|n| format!(" in {n}"))
            .unwrap_or_default();
        println!(
            "  [{}] {}:{} .{}(){}",
            s.role, s.file, s.line, s.method, ctx
        );
    }
    Ok(())
}

pub(super) fn cmd_who_uses(
    root: &Path,
    database: &Path,
    spec: &str,
    json: bool,
    file_origins: &[String],
) -> Result<()> {
    let conn = open_database_read_only(root, Some(database))?;
    let graph = query::ModuleGraph::load(&conn)?;
    let targets = query::find_symbols_in_origins(&conn, spec, file_origins)?;
    if targets.is_empty() {
        eprintln!("no symbol found for '{spec}'");
        std::process::exit(1);
    }
    for t in &targets {
        let usages = cli_who_uses_for_target(&conn, &graph, t, file_origins)?;
        if json {
            println!("{}", serde_json::json!({ "target": t, "usages": usages }));
            continue;
        }
        println!(
            "\n{} {} — {}:{} ({}{})",
            t.kind,
            t.name,
            t.file,
            t.line,
            if t.exported { "exported" } else { "internal" },
            if usages.is_empty() {
                ", no usages found"
            } else {
                ""
            },
        );
        let mut by_conf: std::collections::BTreeMap<&str, Vec<&query::Usage>> = Default::default();
        for u in &usages {
            by_conf.entry(u.confidence.as_str()).or_default().push(u);
        }
        for conf in ["certain", "likely", "possible"] {
            if let Some(list) = by_conf.get(conf) {
                println!("  [{conf}]");
                for u in list {
                    let ctx = u
                        .chunk_name
                        .as_deref()
                        .map(|n| format!(" in {n}"))
                        .unwrap_or_default();
                    let det = u
                        .detail
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default();
                    println!("    {}:{} {}{}{}", u.file, u.line, u.kind, ctx, det);
                }
            }
        }
    }
    Ok(())
}

fn cli_who_uses_for_target(
    conn: &rusqlite::Connection,
    graph: &query::ModuleGraph,
    target: &query::SymbolTarget,
    file_origins: &[String],
) -> Result<Vec<query::Usage>> {
    if let Some(anchor) = query::unique_anchor_for_symbol_target(conn, target)? {
        query::who_uses_anchor_in_origins(conn, &anchor, file_origins)
    } else {
        query::who_uses_in_origins(conn, graph, target.file_id, &target.name, file_origins)
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;

pub(super) fn cmd_chunks(root: &Path, filter: Option<&str>) -> Result<()> {
    let inventory = walk::source_inventory(root)?;
    let editions = crate::rust_lang::resolve_editions(
        root,
        &inventory.files,
        &inventory.cargo_manifests,
        &crate::fs_ops::OsFileSystem,
    )?;
    for rejection in &editions.rejections {
        eprintln!(
            "skip Rust edition input {}: {}",
            rejection.path.display(),
            rejection.error
        );
    }
    let files = inventory.files;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    use std::io::Write;
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file);
        if let Some(f) = filter
            && !rel.to_string_lossy().contains(f)
        {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let Some(format) = formats::repository_code_for_path(file) else {
            continue;
        };
        let chunks = match format.extractor {
            formats::Extractor::EcmaScript => parse::with_parsed(&source, file, |ret, _| {
                let chunker = chunk::Chunker::new(rel, &source, ret);
                chunker.chunk_program(&ret.program, &ret.program.comments)
            }),
            formats::Extractor::RustText => {
                crate::rust_lang::extract(rel, &source, editions.edition_for(file))
                    .map(|extraction| extraction.chunks)
            }
            formats::Extractor::Documentation => continue,
        };
        match chunks {
            Ok(chunks) => {
                for c in chunks {
                    serde_json::to_writer(&mut out, &c)?;
                    writeln!(out)?;
                }
            }
            Err(e) => eprintln!("skip {}: {}", rel.display(), e),
        }
    }
    Ok(())
}

pub(super) fn cmd_stats(root: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    let files = walk::source_files(root)?;
    let mut total = stats::FileStats::default();
    let mut parsed_files = 0usize;
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    let mut total_bytes = 0usize;
    let mut non_ecmascript_files = 0usize;

    for file in &files {
        if !formats::repository_code_for_path(file)
            .is_some_and(|format| format.structural == formats::StructuralPolicy::EcmaScript)
        {
            non_ecmascript_files += 1;
            continue;
        }
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                failed.push((file.clone(), e.to_string()));
                continue;
            }
        };
        total_bytes += source.len();
        match stats::file_stats(file, &source) {
            Ok(s) => {
                parsed_files += 1;
                total.functions += s.functions;
                total.arrow_functions += s.arrow_functions;
                total.classes += s.classes;
                total.methods += s.methods;
                total.jsx_components_defined += s.jsx_components_defined;
                total.imports += s.imports;
                total.exports += s.exports;
                total.type_only_nodes += s.type_only_nodes;
            }
            Err(e) => failed.push((file.clone(), e.to_string())),
        }
    }

    let elapsed = started.elapsed();
    println!("root:            {}", root.display());
    println!(
        "files:           {} ({} parsed, {} rejected)",
        files.len(),
        parsed_files,
        failed.len()
    );
    println!(
        "source size:     {:.1} MB",
        total_bytes as f64 / 1_048_576.0
    );
    if non_ecmascript_files > 0 {
        println!("non-JS/TS files: {non_ecmascript_files} (not included in AST stats)");
    }
    println!("functions:       {}", total.functions);
    println!("arrow functions: {}", total.arrow_functions);
    println!(
        "classes:         {} ({} methods)",
        total.classes, total.methods
    );
    println!("jsx components:  {}", total.jsx_components_defined);
    println!("imports:         {}", total.imports);
    println!("exports:         {}", total.exports);
    println!(
        "type-only nodes: {} (will be erased)",
        total.type_only_nodes
    );
    println!("elapsed:         {elapsed:?}");
    for (f, e) in failed.iter().take(5) {
        eprintln!(
            "  reject: {}: {}",
            f.display(),
            e.lines().next().unwrap_or("")
        );
    }
    Ok(())
}
