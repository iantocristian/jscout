use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{origin, scouting, semantic, semantic_query};

#[derive(Parser)]
#[command(
    name = "jscout",
    about = "Runtime-level JS/TS codebase indexer for RAG",
    version
)]
pub(super) struct Cli {
    /// Explicit configuration file; repository commands otherwise use ROOT/.jscout.toml
    #[arg(long, global = true)]
    pub(super) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Inspect, validate, or initialize repository runtime configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Index, embed, search, or inspect repository Markdown documentation
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
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
        /// Force a full code-vector consistency audit instead of incremental synchronization
        #[arg(long, conflicts_with = "semantic_only")]
        repair: bool,
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
        /// Max ranked results, or page size in exhaustive mode
        #[arg(short = 'k', long)]
        limit: Option<usize>,
        /// Traverse the complete source-content chunk match set in deterministic pages
        #[arg(long, conflicts_with_all = ["vector", "rerank", "expand", "memory"])]
        exhaustive: bool,
        /// Opaque continuation token from a previous exhaustive page
        #[arg(long, requires = "exhaustive")]
        cursor: Option<String>,
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
        /// Include normally excluded roles, other resolved calls, and every orphan scope;
        /// receiver value-flow calls stay excluded
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
pub(super) enum DocsCommand {
    /// Build and transactionally publish the current Markdown corpus and BM25 index
    Index {
        /// Repository root
        root: PathBuf,
        /// Use a documentation database at this path instead of the configured path
        #[arg(long)]
        database: Option<PathBuf>,
        /// Emit JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
    /// Materialize missing vectors for the current documentation snapshot
    Embed {
        /// Repository root whose documentation snapshot is already indexed
        root: PathBuf,
        /// Use a documentation database at this path instead of the configured path
        #[arg(long)]
        database: Option<PathBuf>,
        /// Batch size per embedding request
        #[arg(long)]
        batch: Option<usize>,
        /// Emit JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
    /// Search the current Markdown corpus with BM25 and optional vectors
    Search {
        /// Repository root whose documentation snapshot is already indexed
        root: PathBuf,
        /// Natural-language or identifier query
        query: String,
        /// Use a documentation database at this path instead of the configured path
        #[arg(long)]
        database: Option<PathBuf>,
        /// Maximum returned chunks
        #[arg(short = 'k', long)]
        limit: Option<usize>,
        /// Require vector participation; error instead of degrading to BM25
        #[arg(long, conflicts_with_all = ["no_vector", "lexical_only"])]
        vector: bool,
        /// Skip vector retrieval even when configured and ready
        #[arg(long, conflicts_with = "vector")]
        no_vector: bool,
        /// Use BM25 only (equivalent to --no-vector --no-rerank)
        #[arg(long)]
        lexical_only: bool,
        /// Enable model reranking, overriding repository configuration
        #[arg(long, conflicts_with_all = ["no_rerank", "lexical_only"])]
        rerank: bool,
        /// Skip model reranking even when configured
        #[arg(long, conflicts_with = "rerank")]
        no_rerank: bool,
        /// Disable bounded temporal reordering for relevance comparison
        #[arg(long)]
        no_freshness: bool,
        /// Maximum bytes in the complete rendered response
        #[arg(long)]
        response_bytes: Option<usize>,
        /// Emit compact agent JSON
        #[arg(long, conflicts_with = "debug_json")]
        json: bool,
        /// Emit full retrieval diagnostics as JSON
        #[arg(long, conflicts_with = "json")]
        debug_json: bool,
    },
    /// Report the published corpus, rejections, history, and vector readiness
    Status {
        /// Repository root
        root: PathBuf,
        /// Use a documentation database at this path instead of the configured path
        #[arg(long)]
        database: Option<PathBuf>,
        /// Emit JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum ConfigCommand {
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
pub(super) enum CheckerCommand {
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
pub(super) enum ScoutCommand {
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
pub(super) enum LlmCommand {
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
pub(super) enum InferenceCommand {
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
