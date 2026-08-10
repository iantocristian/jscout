mod agent;
mod chunk;
mod dependency;
mod embed;
mod file_role;
mod graph;
mod heur;
mod indexer;
mod mcp;
mod parse;
mod query;
mod search;
mod scout;
mod semantic;
mod stats;
mod store;
mod structural;
mod walk;
mod watch;
mod workspace;

use std::path::{Path, PathBuf};

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
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Index internals for these installed packages (comma-separated or repeatable)
        #[arg(long = "deps", value_delimiter = ',')]
        dependencies: Vec<String>,
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
        /// Restrict primary hits to a file role (repeatable)
        #[arg(long = "file-role")]
        file_roles: Vec<String>,
        /// Do not attach matching persistent semantic memory
        #[arg(long)]
        no_memory: bool,
        /// Maximum matching semantic artifacts
        #[arg(long, default_value_t = 4)]
        memory_limit: usize,
        /// Maximum bytes in the complete rendered JSON response
        #[arg(long, default_value_t = search::DEFAULT_RESPONSE_BYTE_LIMIT)]
        response_bytes: usize,
        /// Skip vector search even if a provider is configured
        #[arg(long)]
        no_vector: bool,
        /// Output JSON
        #[arg(long)]
        json: bool,
        /// Attach a separately labelled structural context pack (off by default)
        #[arg(long)]
        expand: bool,
        /// Structural expansion depth
        #[arg(long, default_value_t = 1)]
        expand_depth: usize,
        /// Maximum search-hit anchors used as expansion seeds
        #[arg(long, default_value_t = 3)]
        expand_seeds: usize,
        /// Global expansion node budget
        #[arg(long, default_value_t = 40)]
        expand_nodes: usize,
        /// Global expansion edge budget
        #[arg(long, default_value_t = 120)]
        expand_edges: usize,
        /// Global serialized node/edge payload budget
        #[arg(long, default_value_t = 24_000)]
        expand_bytes: usize,
        /// Lowest expansion confidence: certain, likely, or possible
        #[arg(long, default_value = "likely")]
        expand_min_confidence: String,
        /// Restrict expansion to a file role (repeatable; defaults to production/unknown)
        #[arg(long = "expand-file-role", default_values_t = [String::from("production"), String::from("unknown")])]
        expand_file_roles: Vec<String>,
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
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Append privacy-minimal tool-call metrics as JSONL (no queries or results)
        #[arg(long)]
        telemetry: Option<PathBuf>,
        /// Evaluation tool surface: baseline or structural
        #[arg(long, default_value = "structural")]
        profile: String,
        /// Definition source representation: full or deterministic elided source
        #[arg(long, default_value = "full")]
        source_view: String,
    },
    /// Persist an evidence-backed workflow or repository annotation
    Annotate {
        /// Repository root whose source supports the annotation
        root: PathBuf,
        /// JSON file containing the annotate tool input
        input: PathBuf,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Search persistent semantic memory and report freshness
    Memory {
        /// Repository root used to locate the default index
        root: PathBuf,
        /// Optional lexical query; empty lists the newest records
        #[arg(default_value = "")]
        query: String,
        /// Maximum returned artifacts
        #[arg(short = 'k', long, default_value_t = 20)]
        limit: usize,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Enumerate a bounded production-symbol candidate set for workflow scouting
    WorkflowCandidates {
        /// Repository root used to resolve candidate evidence
        root: PathBuf,
        /// Current workflow seed anchors or uniquely resolvable symbol names
        #[arg(required = true)]
        seeds: Vec<String>,
        /// Optional expected structural snapshot
        #[arg(long)]
        snapshot: Option<String>,
        /// Ranked traversal depth
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Maximum issued symbol candidates
        #[arg(long, default_value_t = semantic::MAX_WORKFLOW_CANDIDATES)]
        candidate_limit: usize,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
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
    /// Traverse the snapshot-safe structural graph around a file or symbol
    Neighborhood {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Node key, file path, symbol name, or path-substring:symbol
        anchor: String,
        /// Snapshot carried with a saved anchor; stale anchors are re-resolved
        #[arg(long)]
        snapshot: Option<String>,
        /// Maximum traversal depth
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Edge direction: in, out, or both
        #[arg(long, default_value = "both")]
        direction: String,
        /// Maximum returned nodes
        #[arg(long, default_value_t = 50)]
        node_limit: usize,
        /// Maximum returned edges
        #[arg(long, default_value_t = 200)]
        edge_limit: usize,
        /// Lowest confidence to include: certain, likely, or possible
        #[arg(long, default_value = "likely")]
        min_confidence: String,
        /// Restrict traversal to an edge kind (repeatable)
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// Restrict traversal to a file role (repeatable)
        #[arg(long = "file-role")]
        file_roles: Vec<String>,
    },
    /// Print or install the jscout agent-integration skill
    AgentGuide {
        /// Install into ROOT/.agents/skills/jscout/SKILL.md; print when omitted
        #[arg(long)]
        install: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Stats { root } => cmd_stats(&root),
        Command::Chunks { root, filter } => cmd_chunks(&root, filter.as_deref()),
        Command::Index { root, database, dependencies } => {
            cmd_index(&root, database.as_deref(), &dependencies)
        }
        Command::Embed { root, batch } => cmd_embed(&root, batch),
        Command::Search {
            root,
            query,
            limit,
            file_roles,
            no_memory,
            memory_limit,
            response_bytes,
            no_vector,
            json,
            expand,
            expand_depth,
            expand_seeds,
            expand_nodes,
            expand_edges,
            expand_bytes,
            expand_min_confidence,
            expand_file_roles,
        } => cmd_search(
            &root,
            &query,
            no_vector,
            json,
            search::SearchOptions {
                limit,
                expand,
                file_roles,
                include_memory: !no_memory,
                memory_limit,
                response_byte_limit: response_bytes,
                expansion: search::ExpansionOptions {
                    depth: expand_depth,
                    seed_limit: expand_seeds,
                    node_limit: expand_nodes,
                    edge_limit: expand_edges,
                    byte_limit: expand_bytes,
                    min_confidence: expand_min_confidence,
                    file_roles: expand_file_roles,
                },
            },
        ),
        Command::Events { root, name } => cmd_events(&root, name.as_deref()),
        Command::Mcp { root, database, telemetry, profile, source_view } => {
            mcp::serve(
                &root,
                database.as_deref(),
                telemetry.as_deref(),
                mcp::ToolProfile::parse(&profile)?,
                scout::SourceView::parse(&source_view)?,
            )
        }
        Command::Annotate { root, input, database } => {
            let conn = open_database(&root, database.as_deref())?;
            let input: semantic::AnnotateRequest =
                serde_json::from_slice(&std::fs::read(&input)?)?;
            let artifact = semantic::annotate_request(&root, &conn, input)?;
            println!("{}", serde_json::to_string_pretty(&artifact)?);
            Ok(())
        }
        Command::Memory { root, query, limit, database } => {
            let conn = open_database(&root, database.as_deref())?;
            let artifacts = semantic::search(&conn, &query, limit)?;
            println!("{}", serde_json::to_string_pretty(&artifacts)?);
            Ok(())
        }
        Command::WorkflowCandidates {
            root,
            seeds,
            snapshot,
            depth,
            candidate_limit,
            database,
        } => {
            let conn = open_database(&root, database.as_deref())?;
            let candidates = semantic::workflow_candidates(
                &root,
                &conn,
                &seeds,
                &semantic::WorkflowCandidateOptions {
                    expected_snapshot: snapshot,
                    depth,
                    candidate_limit,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&candidates)?);
            Ok(())
        }
        Command::Watch { root, embed } => watch::watch(&root, embed),
        Command::WhoUses { root, spec, json } => cmd_who_uses(&root, &spec, json),
        Command::Neighborhood {
            root,
            anchor,
            snapshot,
            depth,
            direction,
            node_limit,
            edge_limit,
            min_confidence,
            kinds,
            file_roles,
        } => cmd_neighborhood(
            &root,
            &anchor,
            structural::NeighborhoodOptions {
                expected_snapshot: snapshot,
                depth,
                direction,
                node_limit,
                edge_limit,
                min_confidence,
                kinds,
                penalize_file_roles: !file_roles.is_empty(),
                file_roles,
            },
        ),
        Command::AgentGuide { install } => {
            if let Some(root) = install {
                let target = agent::install(&root)?;
                println!("installed {}", target.display());
            } else {
                print!("{}", agent::GUIDE);
            }
            Ok(())
        }
    }
}

fn open_database(root: &Path, database: Option<&Path>) -> Result<rusqlite::Connection> {
    match database {
        Some(path) => store::open_path(path),
        None => store::open(root),
    }
}

fn cmd_neighborhood(
    root: &Path,
    anchor: &str,
    options: structural::NeighborhoodOptions,
) -> Result<()> {
    let conn = store::open(root)?;
    let neighborhood = structural::neighborhood(&conn, anchor, &options)?;
    println!("{}", serde_json::to_string_pretty(&neighborhood)?);
    Ok(())
}

fn cmd_embed(root: &Path, batch: usize) -> Result<()> {
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

fn cmd_search(
    root: &Path,
    query: &str,
    no_vector: bool,
    json: bool,
    options: search::SearchOptions,
) -> Result<()> {
    let conn = store::open(root)?;
    let provider = if no_vector { None } else { embed::Provider::from_env() };
    let result = search::search(&conn, provider.as_ref(), query, &options)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("snapshot: {}", result.snapshot);
    if result.hits.is_empty() {
        println!("no results");
        return Ok(());
    }
    for (i, h) in result.hits.iter().enumerate() {
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
        println!("      anchors: {}", h.anchors.join(", "));
    }
    if let Some(expansion) = &result.expansion {
        println!(
            "\nstructural expansion: {} nodes, {} edges, {} bytes{}",
            expansion.nodes.len(),
            expansion.edges.len(),
            expansion.payload_bytes,
            if expansion.truncated { " (truncated)" } else { "" }
        );
        for edge in &expansion.edges {
            println!(
                "  {} -[{}:{}]-> {}",
                edge.source, edge.kind, edge.confidence, edge.target
            );
        }
    }
    Ok(())
}

fn cmd_index(root: &Path, database: Option<&Path>, dependencies: &[String]) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = open_database(root, database)?;
    let o = indexer::index_repo_with_options(
        root,
        &conn,
        &indexer::IndexOptions {
            dependencies: dependencies.to_vec(),
            ..Default::default()
        },
    )?;
    println!(
        "indexed {} files ({} unchanged, {} failed) — {} chunks, {} refs in {:?}",
        o.indexed, o.unchanged, o.failed, o.chunks, o.refs, started.elapsed()
    );
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

fn cmd_events(root: &Path, name: Option<&str>) -> Result<()> {
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

fn cmd_who_uses(root: &Path, spec: &str, json: bool) -> Result<()> {
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

fn cmd_chunks(root: &Path, filter: Option<&str>) -> Result<()> {
    let files = walk::source_files(root);
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    use std::io::Write;
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file);
        if let Some(f) = filter
            && !rel.to_string_lossy().contains(f) {
                continue;
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

fn cmd_stats(root: &Path) -> Result<()> {
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
