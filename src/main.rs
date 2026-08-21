#![recursion_limit = "256"]

mod agent;
mod calls;
mod checker;
mod chunk;
mod compact;
mod config;
mod dependency;
mod embed;
mod entity;
mod file_role;
mod graph;
mod heur;
mod indexer;
mod inference;
mod io_policy;
mod llm;
mod mcp;
mod origin;
mod package_exports;
mod parse;
mod query;
mod recon;
mod scout;
mod scouting;
mod search;
mod semantic;
mod semantic_query;
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
    about = "Runtime-level JS/TS codebase indexer for RAG",
    version
)]
struct Cli {
    /// Explicit configuration file; repository commands otherwise use ROOT/.jscout.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect, validate, or initialize repository runtime configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
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
    /// Rebuild the structural snapshot (.jscout.db in the repo root)
    Index {
        /// Repository root
        root: PathBuf,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Index internals for these installed packages (comma-separated or repeatable)
        #[arg(
            long = "deps",
            value_delimiter = ',',
            conflicts_with = "no_dependencies"
        )]
        dependencies: Vec<String>,
        /// Ignore configured dependency packages for this index pass
        #[arg(long = "no-deps", conflicts_with = "dependencies")]
        no_dependencies: bool,
    },
    /// Embed code and/or semantic documents missing from the configured profile
    Embed {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Batch size per API call
        #[arg(long)]
        batch: Option<usize>,
        /// Restrict embeddings to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',')]
        file_origins: Vec<String>,
        /// Embed only the effective product corpus after fresh reconnaissance policy
        #[arg(long)]
        product: bool,
        /// Also embed current workflows, cards, summaries, concepts, and annotations
        #[arg(long)]
        semantic: bool,
        /// Embed only current semantic artifacts, without scanning code chunks
        #[arg(long, conflicts_with_all = ["product", "semantic"])]
        semantic_only: bool,
    },
    /// Hybrid search over the indexed repository
    Search {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Query: natural language and/or identifiers
        query: String,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Max results
        #[arg(short = 'k', long)]
        limit: Option<usize>,
        /// Restrict primary hits to a file role (repeatable)
        #[arg(long = "file-role")]
        file_roles: Vec<String>,
        /// Restrict hits and expansion to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',')]
        file_origins: Vec<String>,
        /// Attach matching persistent semantic memory, overriding repository configuration
        #[arg(long = "memory", conflicts_with = "no_memory")]
        memory: bool,
        /// Do not attach matching persistent semantic memory
        #[arg(long, conflicts_with = "memory")]
        no_memory: bool,
        /// Maximum matching semantic artifacts
        #[arg(long)]
        memory_limit: Option<usize>,
        /// Likely/certain graph hops allowed between hits and attached memory
        #[arg(long)]
        memory_depth: Option<usize>,
        /// Maximum graph nodes visited while connecting attached memory
        #[arg(long)]
        memory_nodes: Option<usize>,
        /// Maximum bytes in the complete rendered JSON response; debug JSON is unbounded when omitted
        #[arg(long)]
        response_bytes: Option<usize>,
        /// Enable vector search, overriding repository configuration
        #[arg(long, conflicts_with_all = ["no_vector", "lexical_only"])]
        vector: bool,
        /// Skip vector search even if a provider is configured
        #[arg(long, conflicts_with = "vector")]
        no_vector: bool,
        /// Enable cross-encoder reranking, overriding repository configuration
        #[arg(long, conflicts_with_all = ["no_rerank", "lexical_only"])]
        rerank: bool,
        /// Skip cross-encoder reranking even if it is configured
        #[arg(long, conflicts_with = "rerank")]
        no_rerank: bool,
        /// Use BM25 only (equivalent to --no-vector --no-rerank)
        #[arg(long)]
        lexical_only: bool,
        /// Output compact agent JSON
        #[arg(long, conflicts_with = "debug_json")]
        json: bool,
        /// Output the full diagnostic JSON representation
        #[arg(long, conflicts_with = "json")]
        debug_json: bool,
        /// Attach a separately labelled structural context pack (off by default)
        #[arg(long, conflicts_with = "no_expand")]
        expand: bool,
        /// Suppress structural expansion, overriding repository configuration
        #[arg(long, conflicts_with = "expand")]
        no_expand: bool,
        /// Structural expansion depth
        #[arg(long)]
        expand_depth: Option<usize>,
        /// Expansion projection: compact paths or diagnostic neighborhood
        #[arg(long, value_parser = ["paths", "neighborhood"])]
        expand_mode: Option<String>,
        /// Maximum search-hit anchors used as expansion seeds
        #[arg(long)]
        expand_seeds: Option<usize>,
        /// Maximum ranked continuation paths in path mode
        #[arg(long)]
        expand_paths: Option<usize>,
        /// Global expansion node budget
        #[arg(long)]
        expand_nodes: Option<usize>,
        /// Global expansion edge budget
        #[arg(long)]
        expand_edges: Option<usize>,
        /// Global serialized node/edge payload budget
        #[arg(long)]
        expand_bytes: Option<usize>,
        /// Lowest expansion confidence: certain, likely, or possible
        #[arg(long)]
        expand_min_confidence: Option<String>,
        /// Restrict expansion to a file role (repeatable; defaults to production/unknown)
        #[arg(long = "expand-file-role")]
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
        /// Append every incoming MCP request as JSONL, including tool arguments
        #[arg(long)]
        request_log: Option<PathBuf>,
        /// Evaluation tool surface: baseline or structural
        #[arg(long)]
        profile: Option<String>,
        /// Definition source representation: full or deterministic elided source
        #[arg(long)]
        source_view: Option<String>,
        /// MCP tool-result transport: auto, text, or structured
        #[arg(long)]
        result_transport: Option<String>,
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
        /// Optional conceptual or identifier query; empty lists newest records
        #[arg(default_value = "")]
        query: String,
        /// Disable semantic-artifact vector retrieval even when configured
        #[arg(long, conflicts_with = "vector")]
        no_vector: bool,
        /// Enable semantic-artifact vector retrieval, overriding repository configuration
        #[arg(long, conflicts_with = "no_vector")]
        vector: bool,
        /// Maximum returned artifacts
        #[arg(short = 'k', long, default_value_t = 20)]
        limit: usize,
        /// Restrict artifacts by type (repeatable or comma-separated)
        #[arg(long = "type", value_delimiter = ',')]
        artifact_types: Vec<String>,
        /// Restrict computed freshness: fresh, degraded, or stale
        #[arg(long, value_delimiter = ',')]
        freshness: Vec<String>,
        /// Load one artifact by id (historical artifacts are allowed)
        #[arg(long)]
        artifact: Option<i64>,
        /// Exact-artifact projection: compact, body, or full
        #[arg(long, value_parser = ["compact", "body", "full"])]
        view: Option<String>,
        /// Include retrieval diagnostics in discovery output; exact reads use --view full
        #[arg(long)]
        debug: bool,
        /// Restrict artifacts to those with direct evidence on this exact anchor
        #[arg(long)]
        anchor: Option<String>,
        /// Restrict artifacts to direct evidence in this exact indexed file
        #[arg(long)]
        file: Option<String>,
        /// Restrict artifacts to direct evidence in this current reconnaissance subject
        #[arg(long)]
        reconnaissance_subject: Option<String>,
        /// Restrict artifacts to direct semantic relations with this artifact id
        #[arg(long)]
        related_to: Option<i64>,
        /// Include superseded artifacts in list/search mode
        #[arg(long)]
        include_superseded: bool,
        /// Include exact, hash-verified source evidence; requires --artifact
        #[arg(long)]
        source: bool,
        /// Maximum source evidence rows
        #[arg(long, default_value_t = 1)]
        source_limit: usize,
        /// Maximum semantic-relation hops followed during source drill-down
        #[arg(long, default_value_t = 8)]
        source_depth: usize,
        /// Maximum source bytes per evidence row
        #[arg(long, default_value_t = semantic_query::DEFAULT_SOURCE_BYTE_LIMIT)]
        source_bytes: usize,
        /// Restrict semantic evidence/source files to origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
        /// Maximum bytes in the complete rendered JSON response
        #[arg(long, default_value_t = semantic_query::DEFAULT_RESPONSE_BYTE_LIMIT)]
        response_bytes: usize,
        /// Maximum direct evidence supports retained per artifact
        #[arg(long)]
        supports_per_artifact: Option<usize>,
        /// Maximum direct semantic relations returned
        #[arg(long, default_value_t = 40)]
        relation_limit: usize,
        /// Maximum deterministic file/chunk tags derived from fresh concepts
        #[arg(long, default_value_t = semantic_query::DEFAULT_CONCEPT_TAG_LIMIT)]
        concept_tag_limit: usize,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Deterministic repository overview with an optional fresh semantic overlay
    Overview {
        /// Repository root used to locate the default index
        root: PathBuf,
        /// Restrict deterministic inventory to file origins
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
        /// Maximum top-level repository areas
        #[arg(long, default_value_t = 20)]
        area_limit: usize,
        /// Maximum structural relation kinds
        #[arg(long, default_value_t = 30)]
        relation_limit: usize,
        /// Attach a separately labelled overlay of current fresh semantic memory
        #[arg(long)]
        semantic: bool,
        /// Maximum semantic overlay artifacts
        #[arg(long, default_value_t = 8)]
        semantic_limit: usize,
        /// Restrict semantic overlay types (cards are excluded by default)
        #[arg(long = "semantic-type", value_delimiter = ',')]
        semantic_types: Vec<String>,
        /// Maximum current reconnaissance classifications with cited explanations
        #[arg(long, default_value_t = 12)]
        reconnaissance_limit: usize,
        /// Exact reconnaissance subject key to drill into
        #[arg(long)]
        reconnaissance_subject: Option<String>,
        /// Include full explanation and cited evidence for one exact subject
        #[arg(long, requires = "reconnaissance_subject")]
        reconnaissance_detail: bool,
        /// Maximum bytes in the complete rendered JSON response
        #[arg(long, default_value_t = 24_000)]
        response_bytes: usize,
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
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Also embed new/changed chunks on each re-index (needs a provider)
        #[arg(long, conflicts_with = "no_embed")]
        embed: bool,
        /// Disable watched embedding configured for this repository
        #[arg(long, conflicts_with = "embed")]
        no_embed: bool,
        /// Restrict watched embedding to the effective product corpus
        #[arg(long, conflicts_with = "no_product")]
        product: bool,
        /// Disable product-only embedding configured for this repository
        #[arg(long, conflicts_with = "product")]
        no_product: bool,
        /// Keep these installed dependency packages in the watched index
        #[arg(
            long = "deps",
            value_delimiter = ',',
            conflicts_with = "no_dependencies"
        )]
        dependencies: Vec<String>,
        /// Ignore configured dependency packages while watching
        #[arg(long = "no-deps", conflicts_with = "dependencies")]
        no_dependencies: bool,
        /// Re-run TypeScript checker enrichment after relevant indexed changes
        #[arg(long, conflicts_with = "no_enrich")]
        enrich: bool,
        /// Disable watched checker enrichment configured for this repository
        #[arg(long, conflicts_with = "enrich")]
        no_enrich: bool,
        /// Hard deadline for each checker request in seconds
        #[arg(long)]
        enrich_timeout: Option<u64>,
        /// Checker sidecar entry file for development and diagnostics
        #[arg(long)]
        sidecar_path: Option<PathBuf>,
        /// Trailing quiet period before a change generation starts
        #[arg(long)]
        debounce_ms: Option<u64>,
        /// Full-refresh interval for missed-event recovery; zero disables it
        #[arg(long)]
        reconcile_seconds: Option<u64>,
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
        /// Maximum bytes in the complete rendered JSON response; debug JSON is unbounded when omitted
        #[arg(long)]
        response_bytes: Option<usize>,
        /// Output the full diagnostic JSON representation
        #[arg(long)]
        debug_json: bool,
    },
    /// Print or install the jscout agent-integration skill
    AgentGuide {
        /// Install into ROOT/.agents/skills/jscout/SKILL.md; print when omitted
        #[arg(long)]
        install: Option<PathBuf>,
    },
    /// Enrich exact member-call occurrences with bounded TypeScript checker facts
    Enrich {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Hard deadline for each checker request in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Restrict enrichment to repository-relative file paths or directory prefixes
        #[arg(long = "file")]
        files: Vec<String>,
        /// Restrict enrichment to workspace/package names
        #[arg(long = "package")]
        packages: Vec<String>,
        /// Restrict enrichment to called property names
        #[arg(long = "member")]
        members: Vec<String>,
        /// Restrict enrichment to file roles
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Explicitly stop after this many spread-ordered occurrences
        #[arg(long)]
        max_occurrences: Option<usize>,
        /// Include normally excluded roles, already-resolved calls, and inferred projects
        #[arg(long)]
        all: bool,
        /// Print the deterministic ownership/selection plan without building TypeScript Programs
        #[arg(long)]
        dry_run: bool,
        /// Recompute every selected project without exact-batch reuse or watch carry-forward
        #[arg(long)]
        full: bool,
        /// Checker sidecar entry file for development and diagnostics
        #[arg(long)]
        sidecar_path: Option<PathBuf>,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// TypeScript checker sidecar diagnostics
    Checker {
        #[command(subcommand)]
        command: CheckerCommand,
    },
    /// Model-gateway operations (generative calls run in a Node sidecar)
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    /// Optional local embedding and reranking service
    Inference {
        #[command(subcommand)]
        command: InferenceCommand,
    },
    /// Generative scouting over deterministic candidates (pi-ai gateway)
    Scout {
        #[command(subcommand)]
        command: ScoutCommand,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the effective non-secret configuration and value sources
    Show {
        /// Repository root
        root: PathBuf,
        /// Emit structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Validate configuration without running another command
    Validate {
        /// Repository root
        root: PathBuf,
    },
    /// Create a documented configuration template without overwriting
    Init {
        /// Repository root
        root: PathBuf,
    },
}

#[derive(Subcommand)]
enum CheckerCommand {
    /// Report TypeScript version, projects, config problems, and readiness
    Doctor {
        /// Repository root whose configured projects are discovered
        root: PathBuf,
        /// Hard deadline for project discovery in seconds
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Checker sidecar entry file for development and diagnostics
        #[arg(long)]
        sidecar_path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ScoutCommand {
    /// Evidence-backed repository scope and TypeScript-project classification
    Repository {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Provider-normalized reasoning effort; otherwise uses repository configuration
        #[arg(long)]
        reasoning: Option<String>,
        /// Explicit API billing/latency tier; rejected where unsupported
        #[arg(long)]
        service_tier: Option<String>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard command-level model-call budget, shared with mixed subdivision; accepts `all`
        #[arg(long, value_parser = parse_positive_count_or_all)]
        max_calls: usize,
        /// Maximum serialized evidence bytes sent to the model per subject
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Maximum initial plus subdivided subjects; accepts `all`
        #[arg(
            long,
            default_value = "all",
            value_parser = parse_positive_count_or_all
        )]
        max_subjects: usize,
        /// Warn without truncating when the final subject count exceeds this value
        #[arg(long, default_value_t = 512)]
        warn_subjects: usize,
        /// Maximum directory levels below an initial mixed subject
        #[arg(long, default_value_t = scouting::repository::DEFAULT_MAX_DEPTH)]
        max_depth: usize,
        /// Supersede completed identical runs instead of reusing them
        #[arg(long)]
        rebuild: bool,
        /// Print exact subjects, evidence, freshness, and bounds; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Hard deadline for each checker inventory request in seconds
        #[arg(long, default_value_t = 30)]
        checker_timeout: u64,
        /// Checker sidecar entry file for development and diagnostics
        #[arg(long)]
        sidecar_path: Option<PathBuf>,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
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
        /// Provider-normalized reasoning effort; otherwise uses repository configuration
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
    /// Evidence-backed cards for selected symbols, one run per subject anchor
    Cards {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Subject symbol anchors or uniquely resolvable symbol names (repeatable)
        #[arg(long = "anchor")]
        anchors: Vec<String>,
        /// Select card subjects from this exact indexed file (repeatable)
        #[arg(long = "file")]
        files: Vec<String>,
        /// Select card subjects from this current reconnaissance subject (repeatable)
        #[arg(long = "subject")]
        reconnaissance_subjects: Vec<String>,
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Provider-normalized reasoning effort; otherwise uses repository configuration
        #[arg(long)]
        reasoning: Option<String>,
        /// Explicit API billing/latency tier; rejected where unsupported
        #[arg(long)]
        service_tier: Option<String>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard request budget; required for automatic or file/subject-targeted selection
        #[arg(long)]
        max_calls: Option<usize>,
        /// Maximum serialized evidence bytes sent to the model
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Supersede a completed identical run instead of reusing it
        #[arg(long)]
        rebuild: bool,
        /// Print exact deterministic subjects and evidence budgets; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
    /// Hierarchical child-cited summaries over validated artifacts
    Summaries {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Summary level: file, module, or repository; omit to run all bottom-up
        #[arg(long)]
        level: Option<String>,
        /// Explicit scope keys (file:<path>, module:<pkg>, repo); requires --level
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Provider-normalized reasoning effort; otherwise uses repository configuration
        #[arg(long)]
        reasoning: Option<String>,
        /// Explicit API billing/latency tier; rejected where unsupported
        #[arg(long)]
        service_tier: Option<String>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard command-level request budget across all levels
        #[arg(long)]
        max_calls: usize,
        /// Maximum serialized evidence bytes sent to the model
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Supersede completed identical runs instead of reusing them
        #[arg(long)]
        rebuild: bool,
        /// Print exact per-level plans and budgets; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
    /// Evidence-backed concepts from exact workflow-name/card-domain-term vocabulary
    Concepts {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Exact vocabulary term to scout (repeatable); omit for automatic discovery
        #[arg(long = "term")]
        terms: Vec<String>,
        /// Exact pi-ai model; defaults to openai-codex:gpt-5.6-terra (plan-backed)
        #[arg(long)]
        model: Option<String>,
        /// Provider-normalized reasoning effort; otherwise uses repository configuration
        #[arg(long)]
        reasoning: Option<String>,
        /// Explicit API billing/latency tier; rejected where unsupported
        #[arg(long)]
        service_tier: Option<String>,
        /// Per-request wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Hard command-level model-run budget; required without --term
        #[arg(long)]
        max_calls: Option<usize>,
        /// Maximum serialized vocabulary/evidence bytes sent to the model
        #[arg(long, default_value_t = 240_000)]
        context_bytes: usize,
        /// Supersede completed identical runs instead of reusing them
        #[arg(long)]
        rebuild: bool,
        /// Print exact normalized groups, inputs, and budgets; make no model calls
        #[arg(long)]
        dry_run: bool,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Gateway entry file for development and diagnostics
        #[arg(long)]
        gateway_path: Option<PathBuf>,
    },
    /// Replace stale/degraded generated workflows, cards, summaries, and concepts using recorded inputs
    Refresh {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Refresh only these current generated artifacts (repeatable)
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

#[derive(Subcommand)]
enum InferenceCommand {
    /// Run the bundled Hugging Face/PyTorch sidecar through uv
    Serve {
        /// Directory containing inference/pyproject.toml and service.py
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Check the sidecar and print its effective model configuration
    Doctor {
        /// Service base URL; otherwise uses repository configuration or loopback:8792
        #[arg(long)]
        url: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Config { command } => run_config_command(command, cli.config.as_deref()),
        command => {
            let runtime = config::RuntimeConfig::load(command.root(), cli.config.as_deref())?;
            let legacy_keys = runtime.legacy_environment_keys();
            if !legacy_keys.is_empty() {
                eprintln!(
                    "warning: legacy environment configuration supplied {}; migrate these settings to {}",
                    legacy_keys.join(", "),
                    config::FILE_NAME,
                );
            }
            run_command(command, &runtime)
        }
    }
}

fn run_config_command(command: ConfigCommand, explicit: Option<&Path>) -> Result<()> {
    match command {
        ConfigCommand::Show { root, json } => {
            let config = config::RuntimeConfig::load(Some(&root), explicit)?;
            if json {
                println!("{}", config.show_json()?);
            } else {
                println!("{}", config.show_text());
            }
            Ok(())
        }
        ConfigCommand::Validate { root } => {
            let config = config::RuntimeConfig::load(Some(&root), explicit)?;
            println!(
                "configuration valid: {} ({})",
                config
                    .config_path
                    .as_deref()
                    .map_or_else(|| "<none>".to_string(), |path| path.display().to_string()),
                config.fingerprint
            );
            Ok(())
        }
        ConfigCommand::Init { root } => {
            let path = config::init(&root, explicit)?;
            println!("created {}", path.display());
            Ok(())
        }
    }
}

impl Command {
    fn root(&self) -> Option<&Path> {
        match self {
            Self::Stats { root }
            | Self::Chunks { root, .. }
            | Self::Index { root, .. }
            | Self::Embed { root, .. }
            | Self::Search { root, .. }
            | Self::Events { root, .. }
            | Self::Calls { root, .. }
            | Self::Mcp { root, .. }
            | Self::Annotate { root, .. }
            | Self::Memory { root, .. }
            | Self::Overview { root, .. }
            | Self::WorkflowCandidates { root, .. }
            | Self::Watch { root, .. }
            | Self::WhoUses { root, .. }
            | Self::Neighborhood { root, .. }
            | Self::Enrich { root, .. } => Some(root),
            Self::Checker {
                command: CheckerCommand::Doctor { root, .. },
            } => Some(root),
            Self::Scout { command } => Some(command.root()),
            Self::AgentGuide {
                install: Some(root),
            } => Some(root),
            Self::Llm { .. } | Self::Inference { .. } => Some(Path::new(".")),
            Self::AgentGuide { install: None } => None,
            Self::Config { command } => Some(command.root()),
        }
    }
}

impl ConfigCommand {
    fn root(&self) -> &Path {
        match self {
            Self::Show { root, .. } | Self::Validate { root } | Self::Init { root } => root,
        }
    }
}

impl ScoutCommand {
    fn root(&self) -> &Path {
        match self {
            Self::Repository { root, .. }
            | Self::Workflows { root, .. }
            | Self::Cards { root, .. }
            | Self::Summaries { root, .. }
            | Self::Concepts { root, .. }
            | Self::Refresh { root, .. } => root,
        }
    }
}

fn run_command(command: Command, runtime: &config::RuntimeConfig) -> Result<()> {
    let configured_database = runtime.effective.database.path.as_path();
    match command {
        Command::Config { .. } => unreachable!("configuration commands are dispatched first"),
        Command::Stats { root } => cmd_stats(&root),
        Command::Chunks { root, filter } => cmd_chunks(&root, filter.as_deref()),
        Command::Index {
            root,
            database,
            dependencies,
            no_dependencies,
        } => {
            let dependencies = if no_dependencies {
                Vec::new()
            } else if dependencies.is_empty() {
                runtime.effective.index.dependencies.clone()
            } else {
                dependencies
            };
            cmd_index(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                &dependencies,
                &runtime.effective.diagnostics,
            )
        }
        Command::Embed {
            root,
            database,
            batch,
            file_origins,
            product,
            semantic,
            semantic_only,
        } => cmd_embed(
            &root,
            Some(database.as_deref().unwrap_or(configured_database)),
            EmbedCommandOptions {
                batch: batch.unwrap_or(runtime.effective.embedding.batch),
                file_origins: if file_origins.is_empty() {
                    &runtime.effective.embedding.origins
                } else {
                    &file_origins
                },
                product,
                semantic,
                semantic_only,
            },
            runtime,
        ),
        Command::Search {
            root,
            query,
            database,
            limit,
            file_roles,
            file_origins,
            memory,
            no_memory,
            memory_limit,
            memory_depth,
            memory_nodes,
            response_bytes,
            vector,
            no_vector,
            rerank,
            no_rerank,
            lexical_only,
            json,
            debug_json,
            expand,
            no_expand,
            expand_depth,
            expand_mode,
            expand_seeds,
            expand_paths,
            expand_nodes,
            expand_edges,
            expand_bytes,
            expand_min_confidence,
            expand_file_roles,
        } => {
            let configured = &runtime.effective.search;
            let vector = if lexical_only || no_vector {
                false
            } else if vector {
                true
            } else {
                configured.vector
            };
            let rerank = if lexical_only || no_rerank {
                false
            } else if rerank {
                true
            } else {
                configured.rerank
            };
            let include_memory = if no_memory {
                false
            } else if memory {
                true
            } else {
                configured.attach_memory
            };
            let expand = if no_expand {
                false
            } else if expand {
                true
            } else {
                configured.expansion.enabled
            };
            let file_roles = if file_roles.is_empty() {
                configured.file_roles.clone()
            } else {
                file_roles
            };
            let file_origins = if file_origins.is_empty() {
                configured.origins.clone()
            } else {
                file_origins
            };
            let expand_file_roles = if expand_file_roles.is_empty() {
                configured.expansion.file_roles.clone()
            } else {
                expand_file_roles
            };
            let provider = if vector {
                embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )?
            } else {
                None
            };
            cmd_search(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                &query,
                provider.as_ref(),
                json,
                debug_json,
                search::SearchOptions {
                    limit: limit.unwrap_or(configured.limit),
                    expand,
                    file_roles,
                    file_origins: file_origins.clone(),
                    include_memory,
                    memory_limit: memory_limit.unwrap_or(configured.memory_limit),
                    memory_graph_depth: memory_depth.unwrap_or(configured.memory_depth),
                    memory_graph_node_limit: memory_nodes.unwrap_or(configured.memory_nodes),
                    rerank,
                    reranker: search::Reranker::from_settings(
                        &runtime.effective.reranker,
                        &runtime.effective.embedding,
                        &runtime.effective.inference,
                    ),
                    timing: runtime.effective.diagnostics.timing,
                    compact: json,
                    include_neighborhood_followups: true,
                    response_byte_limit: effective_search_response_byte_limit(
                        response_bytes,
                        configured.response_bytes,
                        debug_json,
                    ),
                    expansion: search::ExpansionOptions {
                        projection: search::ExpansionProjection::parse(
                            expand_mode.as_deref().unwrap_or(&configured.expansion.mode),
                        )?,
                        depth: expand_depth.unwrap_or(configured.expansion.depth),
                        seed_limit: expand_seeds.unwrap_or(configured.expansion.seeds),
                        path_limit: expand_paths.unwrap_or(configured.expansion.paths),
                        node_limit: expand_nodes.unwrap_or(configured.expansion.nodes),
                        edge_limit: expand_edges.unwrap_or(configured.expansion.edges),
                        byte_limit: expand_bytes.unwrap_or(configured.expansion.bytes),
                        min_confidence: expand_min_confidence
                            .unwrap_or_else(|| configured.expansion.min_confidence.clone()),
                        file_roles: expand_file_roles,
                        file_origins,
                    },
                },
            )
        }
        Command::Events {
            root,
            name,
            file_origins,
        } => cmd_events(&root, configured_database, name.as_deref(), &file_origins),
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
                Some(database.as_deref().unwrap_or(configured_database)),
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
            request_log,
            profile,
            source_view,
            result_transport,
        } => {
            let profile = profile.as_deref().unwrap_or(&runtime.effective.mcp.profile);
            let source_view = source_view
                .as_deref()
                .unwrap_or(&runtime.effective.mcp.source_view);
            let result_transport = result_transport
                .as_deref()
                .unwrap_or(&runtime.effective.mcp.result_transport);
            mcp::serve(
                &root,
                database.as_deref().unwrap_or(configured_database),
                telemetry
                    .as_deref()
                    .or(runtime.effective.telemetry.file.as_deref()),
                request_log
                    .as_deref()
                    .or(runtime.effective.telemetry.request_log.as_deref()),
                mcp::ServeOptions {
                    profile: mcp::ToolProfile::parse(profile)?,
                    source_view: scout::SourceView::parse(source_view)?,
                    result_transport: mcp::ResultTransportPolicy::parse(result_transport)?,
                },
                runtime,
            )
        }
        Command::Annotate {
            root,
            input,
            database,
        } => {
            let conn = open_database_for_write(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let input: semantic::AnnotateRequest = serde_json::from_slice(&std::fs::read(&input)?)?;
            let provider = embed::Provider::from_settings(
                &runtime.effective.embedding,
                &runtime.effective.inference,
            )?;
            let publication =
                semantic::annotate_request_with_provider(&root, &conn, provider.as_ref(), input)?;
            println!("{}", serde_json::to_string_pretty(&publication)?);
            Ok(())
        }
        Command::Memory {
            root,
            query,
            no_vector,
            vector,
            limit,
            artifact_types,
            freshness,
            artifact,
            view,
            debug,
            anchor,
            file,
            reconnaissance_subject,
            related_to,
            include_superseded,
            source,
            source_limit,
            source_depth,
            source_bytes,
            file_origins,
            response_bytes,
            supports_per_artifact,
            relation_limit,
            concept_tag_limit,
            database,
        } => {
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let vector = if no_vector {
                false
            } else if vector {
                true
            } else {
                runtime.effective.search.vector
            };
            let provider = if !vector {
                None
            } else {
                embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )?
            };
            let artifact_view = match view.as_deref() {
                Some(value) => semantic_query::ArtifactViewMode::parse(value)?,
                None if artifact.is_some() && !debug => semantic_query::ArtifactViewMode::Compact,
                None => semantic_query::ArtifactViewMode::Full,
            };
            let supports_per_artifact = supports_per_artifact.unwrap_or_else(|| {
                if artifact.is_some() && artifact_view != semantic_query::ArtifactViewMode::Full {
                    1
                } else {
                    8
                }
            });
            let result = semantic_query::query(
                &root,
                &conn,
                provider.as_ref(),
                &semantic_query::QueryOptions {
                    query,
                    artifact_id: artifact,
                    anchor,
                    file,
                    reconnaissance_subject,
                    related_to,
                    artifact_types,
                    freshness,
                    include_superseded,
                    limit,
                    include_source: source,
                    source_limit,
                    evidence_relation_depth: source_depth,
                    source_byte_limit: source_bytes,
                    file_origins,
                    response_byte_limit: response_bytes,
                    supports_per_artifact,
                    relation_limit,
                    concept_tag_limit,
                    artifact_view,
                    debug,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::Overview {
            root,
            file_origins,
            area_limit,
            relation_limit,
            semantic,
            semantic_limit,
            semantic_types,
            reconnaissance_limit,
            reconnaissance_subject,
            reconnaissance_detail,
            response_bytes,
            database,
        } => {
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let result = surface::overview_response(
                &conn,
                &surface::OverviewOptions {
                    file_origins,
                    area_limit,
                    relation_limit,
                    include_semantic: semantic,
                    semantic_limit,
                    semantic_types,
                    reconnaissance_limit,
                    reconnaissance_subject,
                    reconnaissance_detail,
                    response_byte_limit: response_bytes,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
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
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
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
            database,
            embed,
            no_embed,
            product,
            no_product,
            dependencies,
            no_dependencies,
            enrich,
            no_enrich,
            enrich_timeout,
            sidecar_path,
            debounce_ms,
            reconcile_seconds,
        } => {
            let configured = &runtime.effective.watch;
            let embed = if no_embed {
                false
            } else if embed {
                true
            } else {
                configured.embed
            };
            let product = if no_product {
                false
            } else if product {
                true
            } else {
                configured.product
            };
            let enrich = if no_enrich {
                false
            } else if enrich {
                true
            } else {
                configured.enrich
            };
            let dependencies = if no_dependencies {
                Vec::new()
            } else if dependencies.is_empty() {
                configured.dependencies.clone()
            } else {
                dependencies
            };
            let enrich_timeout = enrich_timeout.unwrap_or(configured.enrich_timeout_seconds);
            let debounce_ms = debounce_ms.unwrap_or(configured.debounce_ms);
            let reconcile_seconds = reconcile_seconds.unwrap_or(configured.reconcile_seconds);
            if product && !embed {
                anyhow::bail!(
                    "product-only watched embedding requires embedding; enable --embed or disable product-only mode"
                );
            }
            if enrich_timeout == 0 {
                anyhow::bail!("watch enrichment timeout must be greater than zero");
            }
            if debounce_ms == 0 {
                anyhow::bail!("watch debounce must be greater than zero");
            }
            if reconcile_seconds != 0 && reconcile_seconds.saturating_mul(1_000) <= debounce_ms {
                anyhow::bail!("watch reconciliation must exceed debounce or be zero");
            }
            let provider = if embed {
                embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )?
            } else {
                None
            };
            watch::watch(
                &root,
                &watch::WatchOptions {
                    database: Some(database.as_deref().unwrap_or(configured_database)),
                    embed_on_change: embed,
                    provider: provider.as_ref(),
                    embed_product_only: product,
                    dependencies: &dependencies,
                    enrich_on_change: enrich,
                    enrich_timeout: std::time::Duration::from_secs(enrich_timeout),
                    checker_sidecar: sidecar_path.as_deref().or(runtime
                        .effective
                        .sidecars
                        .checker
                        .as_deref()),
                    checker_node: &runtime.effective.sidecars.node,
                    timing: runtime.effective.diagnostics.timing,
                    debug: runtime.effective.diagnostics.debug,
                    debounce: std::time::Duration::from_millis(debounce_ms),
                    reconcile_interval: std::time::Duration::from_secs(reconcile_seconds),
                },
            )
        }
        Command::WhoUses {
            root,
            spec,
            json,
            file_origins,
        } => cmd_who_uses(&root, configured_database, &spec, json, &file_origins),
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
            response_bytes,
            debug_json,
        } => cmd_neighborhood(
            &root,
            configured_database,
            &anchor,
            response_bytes,
            debug_json,
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
        Command::Enrich {
            root,
            timeout,
            files,
            packages,
            members,
            roles,
            max_occurrences,
            all,
            dry_run,
            full,
            sidecar_path,
            database,
        } => {
            let report = checker::enrich(
                &root,
                &checker::EnrichOptions {
                    database: Some(database.as_deref().unwrap_or(configured_database)),
                    sidecar: sidecar_path.as_deref().or(runtime
                        .effective
                        .sidecars
                        .checker
                        .as_deref()),
                    node: &runtime.effective.sidecars.node,
                    timeout: std::time::Duration::from_secs(timeout),
                    files,
                    packages,
                    members,
                    roles,
                    max_occurrences,
                    include_all: all,
                    dry_run,
                    carry_forward: false,
                    force_full: full,
                    dirty_files: Vec::new(),
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Checker { command } => match command {
            CheckerCommand::Doctor {
                root,
                timeout,
                sidecar_path,
            } => checker::doctor(
                &root,
                sidecar_path.as_deref(),
                runtime.effective.sidecars.checker.as_deref(),
                &runtime.effective.sidecars.node,
                std::time::Duration::from_secs(timeout),
            ),
        },
        Command::Llm { command } => match command {
            LlmCommand::Doctor {
                model,
                gateway_path,
            } => llm::doctor(model.as_deref(), gateway_path.as_deref(), runtime),
        },
        Command::Inference { command } => match command {
            InferenceCommand::Serve { project } => inference::serve(
                project.as_deref(),
                &runtime.effective.inference,
                &runtime.effective.embedding,
                &runtime.effective.reranker,
            ),
            InferenceCommand::Doctor { url } => {
                inference::doctor(url.as_deref(), &runtime.effective.inference)
            }
        },
        Command::Scout { command } => match command {
            ScoutCommand::Repository {
                root,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                max_subjects,
                warn_subjects,
                max_depth,
                rebuild,
                dry_run,
                checker_timeout,
                sidecar_path,
                database,
                gateway_path,
            } => cmd_scout_repository(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                RepositoryScoutCommandOptions {
                    dry_run,
                    warn_subjects,
                    planning: scouting::repository::RepositoryPlanningOptions {
                        max_subjects,
                        max_depth,
                        checker_timeout: std::time::Duration::from_secs(checker_timeout),
                        checker_sidecar: sidecar_path.as_deref().or(runtime
                            .effective
                            .sidecars
                            .checker
                            .as_deref()),
                        checker_node: &runtime.effective.sidecars.node,
                    },
                    scout: scouting::repository::RepositoryScoutOptions {
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                        rebuild,
                        max_subjects,
                        max_depth,
                    },
                },
            ),
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
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::WorkflowScoutOptions {
                        seeds,
                        depth,
                        candidate_limit,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Cards {
                root,
                anchors,
                files,
                reconnaissance_subjects,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if anchors.is_empty()
                        || !files.is_empty()
                        || !reconnaissance_subjects.is_empty() =>
                    {
                        anyhow::bail!(
                            "automatic or file/subject-targeted card scouting requires --max-calls"
                        )
                    }
                    // One run per explicitly requested subject.
                    None => anchors.len(),
                };
                cmd_scout_cards(
                    &root,
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::CardScoutOptions {
                        anchors,
                        files,
                        reconnaissance_subjects,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Summaries {
                root,
                level,
                scopes,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => cmd_scout_summaries(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                dry_run,
                scouting::SummaryScoutOptions {
                    level,
                    scopes,
                    model: llm::config::resolve_model_setting(
                        model.as_deref(),
                        &runtime.effective.llm.model,
                    )?,
                    reasoning: llm::config::resolve_reasoning_setting(
                        reasoning.as_deref(),
                        runtime.effective.llm.reasoning.as_deref(),
                    ),
                    service_tier,
                    policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                    rebuild,
                    supersedes_artifact_id: None,
                },
            ),
            ScoutCommand::Concepts {
                root,
                terms,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if terms.is_empty() => {
                        anyhow::bail!("automatic concept scouting requires --max-calls")
                    }
                    None => terms.len(),
                };
                cmd_scout_concepts(
                    &root,
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::ConceptScoutOptions {
                        terms,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
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
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                &artifacts,
                dry_run,
                llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
            ),
        },
    }
}

fn open_database_for_write(root: &Path, database: Option<&Path>) -> Result<rusqlite::Connection> {
    match database {
        Some(path) => store::open_path(path),
        None => store::open(root),
    }
}

fn open_database_read_only(root: &Path, database: Option<&Path>) -> Result<rusqlite::Connection> {
    match database {
        Some(path) => store::open_path_read_only(path),
        None => store::open_read_only(root),
    }
}

fn cmd_neighborhood(
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

fn effective_search_response_byte_limit(
    requested: Option<usize>,
    configured: usize,
    debug_json: bool,
) -> usize {
    requested.unwrap_or(if debug_json { usize::MAX } else { configured })
}

fn render_cli_neighborhood(
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

struct EmbedCommandOptions<'a> {
    batch: usize,
    file_origins: &'a [String],
    product: bool,
    semantic: bool,
    semantic_only: bool,
}

fn cmd_embed(
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

fn cmd_search(
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

fn render_semantic_memory_text(artifacts: &[semantic::SemanticArtifact]) -> Result<String> {
    let mut rendered = String::from("\nsemantic memory (untrusted; verify in source):\n");
    for artifact in artifacts {
        rendered.push_str(&format!(
            "  #{} {} {} [{}] confidence={}\n",
            artifact.id,
            artifact.artifact_type,
            artifact.name.as_deref().unwrap_or("<unnamed>"),
            artifact.freshness,
            artifact.confidence,
        ));
        rendered.push_str("      ");
        rendered.push_str(&serde_json::to_string(&artifact.body)?);
        rendered.push('\n');
    }
    Ok(rendered)
}

fn cmd_index(
    root: &Path,
    database: Option<&Path>,
    dependencies: &[String],
    diagnostics: &config::DiagnosticsSettings,
) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = open_database_for_write(root, database)?;
    let o = indexer::refresh_repo_with_options(
        root,
        &conn,
        &indexer::IndexOptions {
            dependencies: dependencies.to_vec(),
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

fn cmd_calls(
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

fn cmd_events(
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

fn cmd_who_uses(
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
    let files = walk::source_files(root)?;
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
    let files = walk::source_files(root)?;
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
        "files:           {} ({} parsed, {} rejected)",
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
            "  reject: {}: {}",
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
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::WorkflowScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
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
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::scout_workflow_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

struct RepositoryScoutCommandOptions<'a> {
    dry_run: bool,
    warn_subjects: usize,
    planning: scouting::repository::RepositoryPlanningOptions<'a>,
    scout: scouting::repository::RepositoryScoutOptions,
}

fn cmd_scout_repository(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    options: RepositoryScoutCommandOptions<'_>,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::repository::plan(root, &conn, &options.planning)?;
    let initial_subjects = plan.items.len();
    if initial_subjects > options.warn_subjects {
        eprintln!(
            "warning: repository scout discovered {initial_subjects} initial subjects (warning threshold {}); no subjects will be truncated",
            options.warn_subjects
        );
    }
    if options.dry_run {
        let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::repository::dry_run_report(
                &conn,
                &mut gateway,
                &plan,
                &options.scout,
            )?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::repository::execute(root, &conn, &mut gateway, &options.scout, plan)?;
    if let Some(subjects) = batch.subjects_considered
        && initial_subjects <= options.warn_subjects
        && subjects > options.warn_subjects
    {
        eprintln!(
            "warning: mixed-scope subdivision increased repository scouting to {subjects} subjects (warning threshold {}); no subjects were truncated",
            options.warn_subjects
        );
    }
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn parse_positive_count_or_all(value: &str) -> std::result::Result<usize, String> {
    if value.eq_ignore_ascii_case("all") {
        return Ok(usize::MAX);
    }
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("expected a positive integer or `all`, received `{value}`"))?;
    if parsed == 0 {
        return Err("value must be greater than zero or `all`".into());
    }
    Ok(parsed)
}

fn cmd_scout_summaries(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::SummaryScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::summary_dry_run_report(
                root, &conn, &options
            )?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::scout_summaries(root, &conn, &mut gateway, &options)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_cards(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::CardScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::cards_with_selectors(
        root,
        &conn,
        &scouting::plan::CardSelectors {
            anchors: options.anchors.clone(),
            files: options.files.clone(),
            reconnaissance_subjects: options.reconnaissance_subjects.clone(),
        },
    )?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::card_dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::scout_card_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_concepts(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    dry_run: bool,
    options: scouting::ConceptScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::concepts(&conn, &options.terms)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::concept_dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::scout_concept_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_refresh(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    runtime: &config::RuntimeConfig,
    artifacts: &[i64],
    dry_run: bool,
    policy: llm::config::RequestPolicy,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
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
        println!("no stale or degraded generated workflows, cards, or summaries to refresh");
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path, runtime)?;
    let batch = scouting::scout_refresh(root, &conn, &mut gateway, selection, policy)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

/// Failed subjects are printed AND fail the process: scripts and agents key
/// on exit status. Incomplete refusals and reported policy skips are designed
/// outcomes and exit zero.
fn scout_batch_exit(batch: &scouting::ScoutBatchReport) -> Result<()> {
    let failed = batch
        .reports
        .iter()
        .filter(|report| report.status == "failed")
        .count();
    if failed > 0 {
        anyhow::bail!(
            "{failed} of {} scouting subject(s) failed; see the report above",
            batch.reports.len()
        );
    }
    Ok(())
}

fn print_scout_batch(batch: &scouting::ScoutBatchReport) {
    if let Some(subjects) = batch.subjects_considered {
        println!("subjects considered: {subjects}");
    }
    for report in &batch.reports {
        println!(
            "run {} [{}]: {} ({} candidates, billing path {})",
            report.run_id, report.kind, report.status, report.candidate_count, report.billing_path
        );
        println!("  subject: {}", report.subject);
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
        if let Some(failure) = &report.failure {
            println!("  failed: {failure}");
        }
        if let Some(artifact) = report.artifact_id {
            println!("  artifact: {artifact}");
        }
    }
    println!(
        "model calls: {}; reports: {}; failed subjects: {}; duplicate boundaries: {}; skipped by call budget: {}; over budget: {}; unresolvable: {}; unscoutable subjects: {}",
        batch.model_calls,
        batch.reports.len(),
        batch
            .reports
            .iter()
            .filter(|report| report.status == "failed")
            .count(),
        batch.duplicate_candidate_sets_skipped,
        batch.skipped_for_call_budget,
        batch.skipped_over_budget.len(),
        batch.skipped_unresolvable.len(),
        batch.skipped_unscoutable,
    );
    if batch.auto_limit_reached {
        println!("automatic selection reached its deterministic limit");
    }
    for (scope, coverage) in &batch.card_scope_coverage {
        println!(
            "  card scope {scope}: discovered {}; selected {}; omitted {}; reused {}; calls {}; completed {}; incomplete {}; failed {}; skipped call/context {}/{}",
            coverage.discovered,
            coverage.selected,
            coverage.omitted,
            coverage.reused,
            coverage.model_calls,
            coverage.completed,
            coverage.incomplete,
            coverage.failed,
            coverage.skipped_call_budget,
            coverage.skipped_context_budget,
        );
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

#[cfg(test)]
mod main_tests;
