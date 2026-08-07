mod chunk;
mod embed;
mod graph;
mod heur;
mod indexer;
mod mcp;
mod parse;
mod query;
mod search;
mod stats;
mod store;
mod walk;
mod watch;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jscout", about = "Runtime-level JS/TS codebase indexer for RAG")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse a repository and print structural statistics
    Stats {
        /// Repository root
        root: PathBuf,
    },
    /// Dump AST-aware chunks as JSONL
    Chunks {
        /// Repository root
        root: PathBuf,
        /// Only emit chunks for files whose path contains this substring
        #[arg(long)]
        filter: Option<String>,
    },
    /// Build or update the index database (.jscout.db in the repo root)
    Index {
        /// Repository root
        root: PathBuf,
    },
    /// Embed chunks that don't have embeddings yet (needs an API key, see README)
    Embed {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Batch size per API call
        #[arg(long, default_value_t = 64)]
        batch: usize,
    },
    /// Hybrid search over the indexed repository
    Search {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Query: natural language and/or identifiers
        query: String,
        /// Max results
        #[arg(short = 'k', long, default_value_t = 8)]
        limit: usize,
        /// Skip vector search even if a provider is configured
        #[arg(long)]
        no_vector: bool,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },
    /// List string-keyed event wiring (emit/listen sites)
    Events {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Only show sites for this event name
        name: Option<String>,
    },
    /// Serve the index over MCP (stdio) for agent integration
    Mcp {
        /// Repository root (must be indexed)
        root: PathBuf,
    },
    /// Watch a repository and re-index on change
    Watch {
        /// Repository root
        root: PathBuf,
        /// Also embed new/changed chunks on each re-index (needs a provider)
        #[arg(long)]
        embed: bool,
    },
    /// Show all usages of a symbol: NAME or path-substring:NAME
    WhoUses {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Symbol spec, e.g. "getUser" or "services/user:getUser"
        spec: String,
        /// Output JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Stats { root } => cmd_stats(&root),
        Command::Chunks { root, filter } => cmd_chunks(&root, filter.as_deref()),
        Command::Index { root } => cmd_index(&root),
        Command::Embed { root, batch } => cmd_embed(&root, batch),
        Command::Search { root, query, limit, no_vector, json } => {
            cmd_search(&root, &query, limit, no_vector, json)
        }
        Command::Events { root, name } => cmd_events(&root, name.as_deref()),
        Command::Mcp { root } => mcp::serve(&root),
        Command::Watch { root, embed } => watch::watch(&root, embed),
        Command::WhoUses { root, spec, json } => cmd_who_uses(&root, &spec, json),
    }
}

fn cmd_embed(root: &PathBuf, batch: usize) -> Result<()> {
    let conn = store::open(root)?;
    let Some(provider) = embed::Provider::from_env() else {
        anyhow::bail!(
            "no embedding provider configured — set VOYAGE_API_KEY, OPENAI_API_KEY, \
             or JSCOUT_EMBED_URL (OpenAI-compatible, e.g. Ollama)"
        );
    };
    eprintln!("provider: {} model: {}", provider.name, provider.model);
    let (done, total) = embed::embed_missing(&conn, &provider, batch)?;
    println!("embedded {done}/{total} chunks");
    Ok(())
}

fn cmd_search(root: &PathBuf, query: &str, limit: usize, no_vector: bool, json: bool) -> Result<()> {
    let conn = store::open(root)?;
    let provider = if no_vector { None } else { embed::Provider::from_env() };
    let hits = search::search(&conn, provider.as_ref(), query, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("no results");
        return Ok(());
    }
    for (i, h) in hits.iter().enumerate() {
        let name = h.name.as_deref().map(|n| format!(" {n}")).unwrap_or_default();
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
    }
    Ok(())
}

fn cmd_index(root: &PathBuf) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = store::open(root)?;
    let o = indexer::index_repo(root, &conn)?;
    println!(
        "indexed {} files ({} unchanged, {} failed) — {} chunks, {} refs in {:?}",
        o.indexed, o.unchanged, o.failed, o.chunks, o.refs, started.elapsed()
    );
    Ok(())
}

fn cmd_events(root: &PathBuf, name: Option<&str>) -> Result<()> {
    let conn = store::open(root)?;
    let sites = query::events(&conn, name)?;
    if sites.is_empty() {
        println!("no event sites found");
        return Ok(());
    }
    let mut current = String::new();
    for s in &sites {
        if s.name != current {
            current = s.name.clone();
            println!("\nevent '{current}'");
        }
        let ctx = s.chunk_name.as_deref().map(|n| format!(" in {n}")).unwrap_or_default();
        println!("  [{}] {}:{} .{}(){}", s.role, s.file, s.line, s.method, ctx);
    }
    Ok(())
}

fn cmd_who_uses(root: &PathBuf, spec: &str, json: bool) -> Result<()> {
    let conn = store::open(root)?;
    let graph = query::ModuleGraph::load(&conn)?;
    let targets = query::find_symbols(&conn, spec)?;
    if targets.is_empty() {
        eprintln!("no symbol found for '{spec}'");
        std::process::exit(1);
    }
    for t in &targets {
        let usages = query::who_uses(&conn, &graph, t.file_id, &t.name)?;
        if json {
            println!(
                "{}",
                serde_json::json!({ "target": t, "usages": usages })
            );
            continue;
        }
        println!(
            "\n{} {} — {}:{} ({}{})",
            t.kind,
            t.name,
            t.file,
            t.line,
            if t.exported { "exported" } else { "internal" },
            if usages.is_empty() { ", no usages found" } else { "" },
        );
        let mut by_conf: std::collections::BTreeMap<&str, Vec<&query::Usage>> = Default::default();
        for u in &usages {
            by_conf.entry(u.confidence.as_str()).or_default().push(u);
        }
        for conf in ["certain", "likely", "possible"] {
            if let Some(list) = by_conf.get(conf) {
                println!("  [{conf}]");
                for u in list {
                    let ctx = u.chunk_name.as_deref().map(|n| format!(" in {n}")).unwrap_or_default();
                    let det = u.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
                    println!("    {}:{} {}{}{}", u.file, u.line, u.kind, ctx, det);
                }
            }
        }
    }
    Ok(())
}

fn cmd_chunks(root: &PathBuf, filter: Option<&str>) -> Result<()> {
    let files = walk::source_files(root);
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    use std::io::Write;
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file);
        if let Some(f) = filter {
            if !rel.to_string_lossy().contains(f) {
                continue;
            }
        }
        let Ok(source) = std::fs::read_to_string(file) else { continue };
        let chunks = parse::with_parsed(&source, file, |ret, _| {
            let chunker = chunk::Chunker::new(rel, &source, ret);
            chunker.chunk_program(&ret.program, &ret.program.comments)
        });
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

fn cmd_stats(root: &PathBuf) -> Result<()> {
    let started = std::time::Instant::now();
    let files = walk::source_files(root);
    let mut total = stats::FileStats::default();
    let mut parsed_files = 0usize;
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    let mut total_bytes = 0usize;

    for file in &files {
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
    println!("files:           {} ({} parsed, {} failed)", files.len(), parsed_files, failed.len());
    println!("source size:     {:.1} MB", total_bytes as f64 / 1_048_576.0);
    println!("functions:       {}", total.functions);
    println!("arrow functions: {}", total.arrow_functions);
    println!("classes:         {} ({} methods)", total.classes, total.methods);
    println!("jsx components:  {}", total.jsx_components_defined);
    println!("imports:         {}", total.imports);
    println!("exports:         {}", total.exports);
    println!("type-only nodes: {} (will be erased)", total.type_only_nodes);
    println!("elapsed:         {:?}", elapsed);
    for (f, e) in failed.iter().take(5) {
        eprintln!("  fail: {}: {}", f.display(), e.lines().next().unwrap_or(""));
    }
    Ok(())
}
