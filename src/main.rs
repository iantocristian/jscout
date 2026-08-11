mod agent;
mod calls;
mod chunk;
mod dependency;
mod embed;
mod entity;
mod file_role;
mod graph;
mod heur;
mod indexer;
mod llm;
mod mcp;
mod origin;
mod package_exports;
mod parse;
mod query;
mod scout;
mod scouting;
mod search;
mod semantic;
mod stats;
mod store;
mod structural;
mod surface;
mod walk;
mod watch;
mod workspace;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "jscout",
    about = "Runtime-level JS/TS codebase indexer for RAG"
)]
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
        /// Restrict embeddings to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
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
        /// Restrict hits and expansion to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
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
        /// Restrict sites to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
    },
    /// Exact member-call sites by method, receiver chain, and argument options
    Calls {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Method name, e.g. insert
        method: String,
        /// Option filter KEY or KEY=VALUE; repeatable, all must match the
        /// same object-literal argument
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Restrict the options object to this 1-based argument position
        #[arg(long)]
        arg_position: Option<usize>,
        /// Dotted suffix the static receiver chain must end with, e.g. wave.card
        #[arg(long)]
        receiver: Option<String>,
        /// Restrict calls to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
        /// Maximum reported matches
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Emit the full JSON result
        #[arg(long)]
        json: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
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
        /// Current symbol anchors or uniquely resolvable symbol names; file anchors are rejected
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
        /// Keep these installed dependency packages in the watched index
        #[arg(long = "deps", value_delimiter = ',')]
        dependencies: Vec<String>,
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
        /// Restrict targets and usages to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
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
        /// Restrict traversal to backing-file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
    },
    /// Print or install the jscout agent-integration skill
    AgentGuide {
        /// Install into ROOT/.agents/skills/jscout/SKILL.md; print when omitted
        #[arg(long)]
        install: Option<PathBuf>,
    },
    /// Model-gateway operations (generative calls run in a Node sidecar)
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    /// Generative scouting over deterministic candidates (pi-ai gateway)
    Scout {
        #[command(subcommand)]
        command: ScoutCommand,
    },
}

#[derive(Subcommand)]
enum ScoutCommand {
    /// Candidate-closed workflow classification from explicit or automatic seeds
    Workflows {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Seed symbol anchors or uniquely resolvable symbol names (repeatable)
        #[arg(long = "seed")]
        seeds: Vec<String>,
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Provider-normalized reasoning effort; falls back to JSCOUT_LLM_REASONING
        #[arg(long)]
        reasoning: Option<String>,
        /// Explicit API billing/latency tier; rejected where unsupported
        #[arg(long)]
        service_tier: Option<String>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard command-level request budget
        #[arg(long)]
        max_calls: Option<usize>,
        /// Maximum serialized evidence bytes sent to the model
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Deterministic candidate traversal depth
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Maximum deterministic candidates
        #[arg(long, default_value_t = semantic::MAX_WORKFLOW_CANDIDATES)]
        candidate_limit: usize,
        /// Supersede a completed identical run instead of reusing it
        #[arg(long)]
        rebuild: bool,
        /// Print exact deterministic seeds/candidate/evidence budgets; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
    /// Replace stale/degraded generated workflows using their recorded inputs
    Refresh {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Refresh only these current generated workflow artifacts (repeatable)
        #[arg(long = "artifact")]
        artifacts: Vec<i64>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard command-level request budget
        #[arg(long)]
        max_calls: usize,
        /// Maximum serialized evidence bytes sent to the model
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Print selected artifacts and exact replacement inputs; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum LlmCommand {
    /// Diagnose node, gateway, provider, and model availability
    Doctor {
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Stats { root } => cmd_stats(&root),
        Command::Chunks { root, filter } => cmd_chunks(&root, filter.as_deref()),
        Command::Index {
            root,
            database,
            dependencies,
        } => cmd_index(&root, database.as_deref(), &dependencies),
        Command::Embed {
            root,
            batch,
            file_origins,
        } => cmd_embed(&root, batch, &file_origins),
        Command::Search {
            root,
            query,
            limit,
            file_roles,
            file_origins,
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
                file_origins: file_origins.clone(),
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
                    file_origins,
                },
            },
        ),
        Command::Events {
            root,
            name,
            file_origins,
        } => cmd_events(&root, name.as_deref(), &file_origins),
        Command::Calls {
            root,
            method,
            args,
            arg_position,
            receiver,
            file_origins,
            limit,
            json,
            database,
        } => {
            let filters = args
                .iter()
                .map(|text| calls::ArgFilter::parse(text))
                .collect::<Result<Vec<_>>>()?;
            cmd_calls(
                &root,
                database.as_deref(),
                &calls::CallQuery {
                    method,
                    args: filters,
                    arg_position,
                    receiver_suffix: receiver,
                    file_origins,
                    limit,
                },
                json,
            )
        }
        Command::Mcp {
            root,
            database,
            telemetry,
            profile,
            source_view,
        } => mcp::serve(
            &root,
            database.as_deref(),
            telemetry.as_deref(),
            mcp::ToolProfile::parse(&profile)?,
            scout::SourceView::parse(&source_view)?,
        ),
        Command::Annotate {
            root,
            input,
            database,
        } => {
            let conn = open_database(&root, database.as_deref())?;
            let input: semantic::AnnotateRequest = serde_json::from_slice(&std::fs::read(&input)?)?;
            let artifact = semantic::annotate_request(&root, &conn, input)?;
            println!("{}", serde_json::to_string_pretty(&artifact)?);
            Ok(())
        }
        Command::Memory {
            root,
            query,
            limit,
            database,
        } => {
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
        Command::Watch {
            root,
            embed,
            dependencies,
        } => watch::watch(&root, embed, &dependencies),
        Command::WhoUses {
            root,
            spec,
            json,
            file_origins,
        } => cmd_who_uses(&root, &spec, json, &file_origins),
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
            file_origins,
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
                file_origins,
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
        Command::Llm { command } => match command {
            LlmCommand::Doctor {
                model,
                gateway_path,
            } => llm::doctor(model.as_deref(), gateway_path.as_deref()),
        },
        Command::Scout { command } => match command {
            ScoutCommand::Workflows {
                root,
                seeds,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                depth,
                candidate_limit,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if seeds.is_empty() => {
                        anyhow::bail!("automatic workflow scouting requires --max-calls")
                    }
                    None => 1,
                };
                cmd_scout_workflows(
                    &root,
                    database.as_deref(),
                    gateway_path.as_deref(),
                    dry_run,
                    scouting::WorkflowScoutOptions {
                        seeds,
                        depth,
                        candidate_limit,
                        model: llm::config::resolve_model(model.as_deref())?,
                        reasoning: llm::config::resolve_reasoning(reasoning.as_deref()),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Refresh {
                root,
                artifacts,
                timeout,
                max_calls,
                context_bytes,
                dry_run,
                database,
                gateway_path,
            } => cmd_scout_refresh(
                &root,
                database.as_deref(),
                gateway_path.as_deref(),
                &artifacts,
                dry_run,
                llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
            ),
        },
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

fn cmd_embed(root: &Path, batch: usize, file_origins: &[String]) -> Result<()> {
    let conn = store::open(root)?;
    let Some(provider) = embed::Provider::from_env() else {
        anyhow::bail!(
            "no embedding provider configured — set VOYAGE_API_KEY, OPENAI_API_KEY, \
             or JSCOUT_EMBED_URL (OpenAI-compatible, e.g. Ollama)"
        );
    };
    eprintln!("provider: {} model: {}", provider.name, provider.model);
    let (done, total) = embed::embed_missing_for_origins(&conn, &provider, batch, file_origins)?;
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
    let provider = if no_vector {
        None
    } else {
        embed::Provider::from_env()
    };
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
            "\nstructural expansion: {} nodes, {} edges, {} bytes{}",
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
        o.indexed,
        o.unchanged,
        o.failed,
        o.chunks,
        o.refs,
        started.elapsed()
    );
    indexer::report_failures(&o);
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

fn cmd_calls(
    root: &Path,
    database: Option<&Path>,
    query: &calls::CallQuery,
    json: bool,
) -> Result<()> {
    let conn = open_database(root, database)?;
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

fn cmd_events(root: &Path, name: Option<&str>, file_origins: &[String]) -> Result<()> {
    let conn = store::open(root)?;
    let sites = query::events_in_origins(&conn, name, file_origins)?;
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

fn cmd_who_uses(root: &Path, spec: &str, json: bool, file_origins: &[String]) -> Result<()> {
    let conn = store::open(root)?;
    let graph = query::ModuleGraph::load(&conn)?;
    let targets = query::find_symbols_in_origins(&conn, spec, file_origins)?;
    if targets.is_empty() {
        eprintln!("no symbol found for '{spec}'");
        std::process::exit(1);
    }
    for t in &targets {
        let usages = query::who_uses_in_origins(&conn, &graph, t.file_id, &t.name, file_origins)?;
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

fn cmd_chunks(root: &Path, filter: Option<&str>) -> Result<()> {
    let files = walk::source_files(root);
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
    println!(
        "files:           {} ({} parsed, {} failed)",
        files.len(),
        parsed_files,
        failed.len()
    );
    println!(
        "source size:     {:.1} MB",
        total_bytes as f64 / 1_048_576.0
    );
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
    println!("elapsed:         {:?}", elapsed);
    for (f, e) in failed.iter().take(5) {
        eprintln!(
            "  fail: {}: {}",
            f.display(),
            e.lines().next().unwrap_or("")
        );
    }
    Ok(())
}

fn cmd_scout_workflows(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    dry_run: bool,
    options: scouting::WorkflowScoutOptions,
) -> Result<()> {
    let conn = open_database(root, database)?;
    let plan = scouting::plan::workflows(
        root,
        &conn,
        &options.seeds,
        options.depth,
        options.candidate_limit,
    )?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_workflow_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    Ok(())
}

fn cmd_scout_refresh(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    artifacts: &[i64],
    dry_run: bool,
    policy: llm::config::RequestPolicy,
) -> Result<()> {
    let conn = open_database(root, database)?;
    let selection = scouting::refresh::select(&conn, artifacts)?;
    if dry_run {
        let plans = scouting::plan_refresh(root, &conn, &selection)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "dry_run": true,
                "max_calls": policy.max_calls,
                "context_bytes": policy.context_bytes,
                "selection": selection.summary,
                "plans": plans,
            }))?
        );
        return Ok(());
    }
    if !selection.summary.skipped_fresh.is_empty() {
        println!(
            "skipped fresh artifacts: {:?}",
            selection.summary.skipped_fresh
        );
    }
    if !selection.summary.unsupported_legacy.is_empty() {
        println!(
            "cannot refresh pre-G5 artifacts without recorded configuration: {:?}",
            selection.summary.unsupported_legacy
        );
    }
    if selection.targets.is_empty() {
        println!("no stale or degraded generated workflows to refresh");
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_refresh(root, &conn, &mut gateway, selection, policy)?;
    print_scout_batch(&batch);
    Ok(())
}

fn print_scout_batch(batch: &scouting::WorkflowBatchReport) {
    for report in &batch.reports {
        println!(
            "run {}: {} ({} candidates, billing path {})",
            report.run_id, report.status, report.candidate_count, report.billing_path
        );
        if let Some(started) = &report.started {
            println!(
                "  model: {}:{} via {} (auth {})",
                started.provider, started.model, started.api, started.auth_source
            );
        }
        for (decision, count) in &report.decisions {
            println!("  {decision}: {count}");
        }
        if let Some(usage) = &report.usage {
            println!(
                "  usage: {} in / {} out / {} total tokens",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            );
        }
        if let Some(reason) = &report.incomplete_reason {
            println!("  incomplete: {reason}");
        }
        if let Some(artifact) = report.artifact_id {
            println!("  artifact: {artifact}");
        }
    }
    println!(
        "model calls: {}; reports: {}; duplicate boundaries: {}; skipped by call budget: {}; over budget: {}; unresolvable: {}; unscoutable seeds: {}",
        batch.model_calls,
        batch.reports.len(),
        batch.duplicate_candidate_sets_skipped,
        batch.skipped_for_call_budget,
        batch.skipped_over_budget.len(),
        batch.skipped_unresolvable.len(),
        batch.skipped_unscoutable,
    );
    if batch.auto_seed_limit_reached {
        println!("automatic seed discovery reached its deterministic limit");
    }
    for skipped in &batch.skipped_over_budget {
        println!(
            "  skipped over budget: {}: {}",
            skipped.subject, skipped.reason
        );
    }
    for skipped in &batch.skipped_unresolvable {
        println!(
            "  skipped unresolvable: {}: {}",
            skipped.subject, skipped.reason
        );
    }
}
