mod agent;
mod calls;
mod checker;
mod chunk;
mod compact;
mod dependency;
mod embed;
mod entity;
mod file_role;
mod graph;
mod heur;
mod indexer;
mod inference;
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
    /// Rebuild the structural snapshot (.jscout.db in the repo root)
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
    /// Embed chunks missing from the explicitly configured provider profile
    Embed {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Use an index database at this path instead of ROOT/.jscout.db
        #[arg(long)]
        database: Option<PathBuf>,
        /// Batch size per API call
        #[arg(long, default_value_t = 64)]
        batch: usize,
        /// Restrict embeddings to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
        /// Embed only the effective product corpus after fresh reconnaissance policy
        #[arg(long)]
        product: bool,
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
        #[arg(short = 'k', long, default_value_t = search::DEFAULT_RESULT_LIMIT)]
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
        /// Skip cross-encoder reranking even if it is configured
        #[arg(long)]
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
        /// Append every incoming MCP request as JSONL, including tool arguments
        #[arg(long)]
        request_log: Option<PathBuf>,
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
        /// Restrict artifacts by type (repeatable or comma-separated)
        #[arg(long = "type", value_delimiter = ',')]
        artifact_types: Vec<String>,
        /// Restrict computed freshness: fresh, degraded, or stale
        #[arg(long, value_delimiter = ',')]
        freshness: Vec<String>,
        /// Load one artifact by id (historical artifacts are allowed)
        #[arg(long)]
        artifact: Option<i64>,
        /// Restrict artifacts to those with direct evidence on this exact anchor
        #[arg(long)]
        anchor: Option<String>,
        /// Restrict artifacts to direct semantic relations with this artifact id
        #[arg(long)]
        related_to: Option<i64>,
        /// Include superseded artifacts in list/search mode
        #[arg(long)]
        include_superseded: bool,
        /// Include exact, hash-verified source evidence (follows summary children)
        #[arg(long)]
        source: bool,
        /// Maximum source evidence rows
        #[arg(long, default_value_t = 12)]
        source_limit: usize,
        /// Maximum semantic-relation hops followed during source drill-down
        #[arg(long, default_value_t = 8)]
        source_depth: usize,
        /// Maximum source bytes per evidence row
        #[arg(long, default_value_t = semantic_query::DEFAULT_SOURCE_BYTE_LIMIT)]
        source_bytes: usize,
        /// Restrict source drill-down to file origins (dependency is opt-in)
        #[arg(long = "origin", value_delimiter = ',', default_values_t = origin::defaults())]
        file_origins: Vec<String>,
        /// Maximum bytes in the complete rendered JSON response
        #[arg(long, default_value_t = semantic_query::DEFAULT_RESPONSE_BYTE_LIMIT)]
        response_bytes: usize,
        /// Maximum direct evidence supports retained per artifact
        #[arg(long, default_value_t = 8)]
        supports_per_artifact: usize,
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
        #[arg(long)]
        embed: bool,
        /// Keep these installed dependency packages in the watched index
        #[arg(long = "deps", value_delimiter = ',')]
        dependencies: Vec<String>,
        /// Re-run TypeScript checker enrichment after relevant indexed changes
        #[arg(long)]
        enrich: bool,
        /// Hard deadline for each checker request in seconds
        #[arg(long, default_value_t = 300)]
        enrich_timeout: u64,
        /// Checker sidecar entry file for development and diagnostics
        #[arg(long)]
        sidecar_path: Option<PathBuf>,
        /// Trailing quiet period before a change generation starts
        #[arg(long, default_value_t = 2_000)]
        debounce_ms: u64,
        /// Full-refresh interval for missed-event recovery; zero disables it
        #[arg(long, default_value_t = 600)]
        reconcile_seconds: u64,
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
        /// Maximum bytes in the complete rendered JSON response
        #[arg(long, default_value_t = 24_000)]
        response_bytes: usize,
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
        /// Include normally excluded roles and already-resolved calls
        #[arg(long)]
        all: bool,
        /// Print the deterministic ownership/selection plan without building TypeScript Programs
        #[arg(long)]
        dry_run: bool,
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
        /// Provider-normalized reasoning effort; falls back to JSCOUT_LLM_REASONING
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
    /// Evidence-backed cards for selected symbols, one run per subject anchor
    Cards {
        /// Repository root (must be indexed)
        root: PathBuf,
        /// Subject symbol anchors or uniquely resolvable symbol names (repeatable)
        #[arg(long = "anchor")]
        anchors: Vec<String>,
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
        /// Hard command-level request budget; required without --anchor
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
        /// Provider-normalized reasoning effort; falls back to JSCOUT_LLM_REASONING
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
        /// Provider-normalized reasoning effort; falls back to JSCOUT_LLM_REASONING
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
        /// Service base URL; defaults to JSCOUT_INFERENCE_URL or loopback:8792
        #[arg(long)]
        url: Option<String>,
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
            database,
            batch,
            file_origins,
            product,
        } => cmd_embed(&root, database.as_deref(), batch, &file_origins, product),
        Command::Search {
            root,
            query,
            database,
            limit,
            file_roles,
            file_origins,
            no_memory,
            memory_limit,
            response_bytes,
            no_vector,
            no_rerank,
            lexical_only,
            json,
            debug_json,
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
            database.as_deref(),
            &query,
            no_vector || lexical_only,
            json,
            debug_json,
            search::SearchOptions {
                limit,
                expand,
                file_roles,
                file_origins: file_origins.clone(),
                include_memory: !no_memory,
                memory_limit,
                rerank: !(no_rerank || lexical_only),
                compact: json,
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
            request_log,
            profile,
            source_view,
        } => mcp::serve(
            &root,
            database.as_deref(),
            telemetry.as_deref(),
            request_log.as_deref(),
            mcp::ToolProfile::parse(&profile)?,
            scout::SourceView::parse(&source_view)?,
        ),
        Command::Annotate {
            root,
            input,
            database,
        } => {
            let conn = open_database_for_write(&root, database.as_deref())?;
            let input: semantic::AnnotateRequest = serde_json::from_slice(&std::fs::read(&input)?)?;
            let artifact = semantic::annotate_request(&root, &conn, input)?;
            println!("{}", serde_json::to_string_pretty(&artifact)?);
            Ok(())
        }
        Command::Memory {
            root,
            query,
            limit,
            artifact_types,
            freshness,
            artifact,
            anchor,
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
            let conn = open_database_read_only(&root, database.as_deref())?;
            let result = semantic_query::query(
                &root,
                &conn,
                &semantic_query::QueryOptions {
                    query,
                    artifact_id: artifact,
                    anchor,
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
            response_bytes,
            database,
        } => {
            let conn = open_database_read_only(&root, database.as_deref())?;
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
            let conn = open_database_read_only(&root, database.as_deref())?;
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
            dependencies,
            enrich,
            enrich_timeout,
            sidecar_path,
            debounce_ms,
            reconcile_seconds,
        } => watch::watch(
            &root,
            &watch::WatchOptions {
                database: database.as_deref(),
                embed_on_change: embed,
                dependencies: &dependencies,
                enrich_on_change: enrich,
                enrich_timeout: std::time::Duration::from_secs(enrich_timeout),
                checker_sidecar: sidecar_path.as_deref(),
                debounce: std::time::Duration::from_millis(debounce_ms),
                reconcile_interval: std::time::Duration::from_secs(reconcile_seconds),
            },
        ),
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
            response_bytes,
            debug_json,
        } => cmd_neighborhood(
            &root,
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
            sidecar_path,
            database,
        } => {
            let report = checker::enrich(
                &root,
                &checker::EnrichOptions {
                    database: database.as_deref(),
                    sidecar: sidecar_path.as_deref(),
                    timeout: std::time::Duration::from_secs(timeout),
                    files,
                    packages,
                    members,
                    roles,
                    max_occurrences,
                    include_all: all,
                    dry_run,
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
                std::time::Duration::from_secs(timeout),
            ),
        },
        Command::Llm { command } => match command {
            LlmCommand::Doctor {
                model,
                gateway_path,
            } => llm::doctor(model.as_deref(), gateway_path.as_deref()),
        },
        Command::Inference { command } => match command {
            InferenceCommand::Serve { project } => inference::serve(project.as_deref()),
            InferenceCommand::Doctor { url } => inference::doctor(url.as_deref()),
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
                database.as_deref(),
                gateway_path.as_deref(),
                dry_run,
                warn_subjects,
                scouting::repository::RepositoryPlanningOptions {
                    max_subjects,
                    max_depth,
                    checker_timeout: std::time::Duration::from_secs(checker_timeout),
                    checker_sidecar: sidecar_path.as_deref(),
                },
                scouting::repository::RepositoryScoutOptions {
                    model: llm::config::resolve_model(model.as_deref())?,
                    reasoning: llm::config::resolve_reasoning(reasoning.as_deref()),
                    service_tier,
                    policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?,
                    rebuild,
                    max_subjects,
                    max_depth,
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
            ScoutCommand::Cards {
                root,
                anchors,
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
                    None if anchors.is_empty() => {
                        anyhow::bail!("automatic card scouting requires --max-calls")
                    }
                    // One run per explicitly requested subject.
                    None => anchors.len(),
                };
                cmd_scout_cards(
                    &root,
                    database.as_deref(),
                    gateway_path.as_deref(),
                    dry_run,
                    scouting::CardScoutOptions {
                        anchors,
                        model: llm::config::resolve_model(model.as_deref())?,
                        reasoning: llm::config::resolve_reasoning(reasoning.as_deref()),
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
                database.as_deref(),
                gateway_path.as_deref(),
                dry_run,
                scouting::SummaryScoutOptions {
                    level,
                    scopes,
                    model: llm::config::resolve_model(model.as_deref())?,
                    reasoning: llm::config::resolve_reasoning(reasoning.as_deref()),
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
                    database.as_deref(),
                    gateway_path.as_deref(),
                    dry_run,
                    scouting::ConceptScoutOptions {
                        terms,
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
    anchor: &str,
    response_bytes: usize,
    debug_json: bool,
    options: structural::NeighborhoodOptions,
) -> Result<()> {
    let conn = store::open_read_only(root)?;
    let neighborhood = structural::neighborhood(&conn, anchor, &options)?;
    let rendered = if debug_json {
        mcp::render_bounded_object_arrays(
            serde_json::to_value(neighborhood)?,
            &["edges", "nodes"],
            response_bytes,
        )?
    } else {
        compact::render_neighborhood(&neighborhood, response_bytes)?
    };
    println!("{rendered}");
    Ok(())
}

fn cmd_embed(
    root: &Path,
    database: Option<&Path>,
    batch: usize,
    file_origins: &[String],
    product: bool,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let Some(provider) = embed::Provider::from_env()? else {
        anyhow::bail!(
            "no embedding provider configured — set JSCOUT_EMBED_PROVIDER to local, voyage, or openai"
        );
    };
    eprintln!("provider: {} model: {}", provider.name, provider.model);
    let (done, total) =
        embed::embed_missing_for_selection(&conn, &provider, batch, file_origins, product)?;
    println!("embedded {done}/{total} chunks");
    Ok(())
}

fn cmd_search(
    root: &Path,
    database: Option<&Path>,
    query: &str,
    no_vector: bool,
    json: bool,
    debug_json: bool,
    options: search::SearchOptions,
) -> Result<()> {
    let conn = open_database_read_only(root, database)?;
    let provider = if no_vector {
        None
    } else {
        embed::Provider::from_env()?
    };
    let result = search::search(&conn, provider.as_ref(), query, &options)?;
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

fn cmd_index(root: &Path, database: Option<&Path>, dependencies: &[String]) -> Result<()> {
    let started = std::time::Instant::now();
    let conn = open_database_for_write(root, database)?;
    let o = indexer::refresh_repo_with_options(
        root,
        &conn,
        &indexer::IndexOptions {
            dependencies: dependencies.to_vec(),
            ..Default::default()
        },
    )?;
    // `jscout index` and every watcher generation are full snapshot refreshes,
    // so an "unchanged" count would always read 0 and misreport the rebuild as
    // failed change detection.
    println!(
        "indexed {} files ({} failed) — {} chunks, {} refs in {:?}",
        o.indexed,
        o.failed,
        o.chunks,
        o.refs,
        started.elapsed()
    );
    if o.extraction_reset {
        println!("snapshot refresh: rebuilt disposable structural state");
    }
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

fn cmd_events(root: &Path, name: Option<&str>, file_origins: &[String]) -> Result<()> {
    let conn = store::open_read_only(root)?;
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
    let conn = store::open_read_only(root)?;
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
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_workflow_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_repository(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    dry_run: bool,
    warn_subjects: usize,
    planning: scouting::repository::RepositoryPlanningOptions<'_>,
    options: scouting::repository::RepositoryScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::repository::plan(root, &conn, &planning)?;
    let initial_subjects = plan.items.len();
    if initial_subjects > warn_subjects {
        eprintln!(
            "warning: repository scout discovered {initial_subjects} initial subjects (warning threshold {warn_subjects}); no subjects will be truncated"
        );
    }
    if dry_run {
        let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::repository::dry_run_report(
                &conn,
                &mut gateway,
                &plan,
                &options,
            )?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::repository::execute(root, &conn, &mut gateway, &options, plan)?;
    if let Some(subjects) = batch.subjects_considered
        && initial_subjects <= warn_subjects
        && subjects > warn_subjects
    {
        eprintln!(
            "warning: mixed-scope subdivision increased repository scouting to {subjects} subjects (warning threshold {warn_subjects}); no subjects were truncated"
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
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_summaries(root, &conn, &mut gateway, &options)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_cards(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
    dry_run: bool,
    options: scouting::CardScoutOptions,
) -> Result<()> {
    let conn = open_database_for_write(root, database)?;
    let plan = scouting::plan::cards(root, &conn, &options.anchors)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&scouting::card_dry_run_report(&plan, &options)?)?
        );
        return Ok(());
    }
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_card_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_concepts(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
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
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
    let batch = scouting::scout_concept_plan(root, &conn, &mut gateway, &options, plan)?;
    print_scout_batch(&batch);
    scout_batch_exit(&batch)
}

fn cmd_scout_refresh(
    root: &Path,
    database: Option<&Path>,
    gateway_path: Option<&Path>,
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
    let mut gateway = llm::process::ProcessGateway::launch(gateway_path)?;
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
mod main_tests {
    use std::path::PathBuf;

    use anyhow::Result;
    use serde_json::json;

    use super::{Cli, Command, ScoutCommand, render_semantic_memory_text};
    use crate::semantic::SemanticArtifact;
    use clap::Parser;

    #[test]
    fn text_search_memory_is_renderable_without_code_hits() -> Result<()> {
        let rendered = render_semantic_memory_text(&[SemanticArtifact {
            id: 7,
            supersedes: None,
            artifact_type: "workflow".into(),
            name: Some("invoice settlement".into()),
            trust: "untrusted".into(),
            body: json!({ "description": "settles an invoice" }),
            model: "test".into(),
            prompt_version: "test/v1".into(),
            confidence: "likely".into(),
            source_snapshot: "snapshot".into(),
            created_at: "now".into(),
            freshness: "fresh".into(),
            supports: Vec::new(),
            relevance: 1.0,
        }])?;
        assert!(rendered.contains("semantic memory (untrusted; verify in source)"));
        assert!(rendered.contains("invoice settlement [fresh]"));
        Ok(())
    }

    #[test]
    fn lexical_only_and_rerank_controls_parse_independently() {
        let Cli { command } = Cli::try_parse_from([
            "jscout",
            "search",
            ".",
            "query",
            "--no-vector",
            "--no-rerank",
        ])
        .expect("explicit controls parse");
        let Command::Search {
            no_vector,
            no_rerank,
            lexical_only,
            ..
        } = command
        else {
            panic!("expected search")
        };
        assert!(no_vector);
        assert!(no_rerank);
        assert!(!lexical_only);

        let Cli { command } =
            Cli::try_parse_from(["jscout", "search", ".", "query", "--lexical-only"])
                .expect("lexical shortcut parses");
        let Command::Search { lexical_only, .. } = command else {
            panic!("expected search")
        };
        assert!(lexical_only);
    }

    #[test]
    fn repository_scout_accepts_explicit_all_without_hiding_the_warning_threshold() {
        let Cli { command } = Cli::try_parse_from([
            "jscout",
            "scout",
            "repository",
            ".",
            "--max-calls",
            "all",
            "--max-subjects",
            "all",
            "--warn-subjects",
            "512",
        ])
        .expect("unbounded repository scout parses");
        let Command::Scout {
            command:
                ScoutCommand::Repository {
                    max_calls,
                    max_subjects,
                    warn_subjects,
                    ..
                },
        } = command
        else {
            panic!("expected repository scout")
        };
        assert_eq!(max_calls, usize::MAX);
        assert_eq!(max_subjects, usize::MAX);
        assert_eq!(warn_subjects, 512);
        assert!(
            Cli::try_parse_from(["jscout", "scout", "repository", ".", "--max-calls", "0",])
                .is_err()
        );
    }

    #[test]
    fn search_and_embed_accept_external_database_paths() {
        let Cli { command } = Cli::try_parse_from([
            "jscout",
            "search",
            ".",
            "query",
            "--database",
            "/tmp/search.db",
        ])
        .expect("external search database parses");
        let Command::Search { database, .. } = command else {
            panic!("expected search")
        };
        assert_eq!(database, Some(PathBuf::from("/tmp/search.db")));

        let Cli { command } =
            Cli::try_parse_from(["jscout", "embed", ".", "--database", "/tmp/embed.db"])
                .expect("external embed database parses");
        let Command::Embed { database, .. } = command else {
            panic!("expected embed")
        };
        assert_eq!(database, Some(PathBuf::from("/tmp/embed.db")));
    }

    #[test]
    fn compact_and_debug_json_modes_parse_without_ambiguity() {
        let Cli { command } =
            Cli::try_parse_from(["jscout", "search", ".", "query", "--debug-json"])
                .expect("debug search output parses");
        let Command::Search {
            json, debug_json, ..
        } = command
        else {
            panic!("expected search")
        };
        assert!(!json);
        assert!(debug_json);
        assert!(
            Cli::try_parse_from(["jscout", "search", ".", "query", "--json", "--debug-json"])
                .is_err()
        );

        let Cli { command } =
            Cli::try_parse_from(["jscout", "neighborhood", ".", "root", "--debug-json"])
                .expect("debug neighborhood output parses");
        let Command::Neighborhood { debug_json, .. } = command else {
            panic!("expected neighborhood")
        };
        assert!(debug_json);
    }

    #[test]
    fn watch_checker_enrichment_controls_parse_independently() {
        let Cli { command } = Cli::try_parse_from([
            "jscout",
            "watch",
            ".",
            "--embed",
            "--enrich",
            "--enrich-timeout",
            "45",
            "--sidecar-path",
            "checker.mjs",
            "--database",
            "watch.db",
            "--debounce-ms",
            "750",
            "--reconcile-seconds",
            "30",
        ])
        .expect("watch enrichment controls parse");
        let Command::Watch {
            embed,
            enrich,
            enrich_timeout,
            sidecar_path,
            database,
            debounce_ms,
            reconcile_seconds,
            ..
        } = command
        else {
            panic!("expected watch")
        };
        assert!(embed);
        assert!(enrich);
        assert_eq!(enrich_timeout, 45);
        assert_eq!(sidecar_path, Some(PathBuf::from("checker.mjs")));
        assert_eq!(database, Some(PathBuf::from("watch.db")));
        assert_eq!(debounce_ms, 750);
        assert_eq!(reconcile_seconds, 30);
    }

    #[test]
    fn enrichment_plan_controls_parse_without_implying_a_default_cap() {
        let Cli { command } = Cli::try_parse_from([
            "jscout",
            "enrich",
            ".",
            "--dry-run",
            "--file",
            "packages/core",
            "--package",
            "@scope/core",
            "--member",
            "insert",
            "--role",
            "test",
            "--max-occurrences",
            "25",
            "--all",
        ])
        .expect("enrichment plan controls parse");
        let Command::Enrich {
            files,
            packages,
            members,
            roles,
            max_occurrences,
            all,
            dry_run,
            ..
        } = command
        else {
            panic!("expected enrich")
        };
        assert_eq!(files, ["packages/core"]);
        assert_eq!(packages, ["@scope/core"]);
        assert_eq!(members, ["insert"]);
        assert_eq!(roles, ["test"]);
        assert_eq!(max_occurrences, Some(25));
        assert!(all);
        assert!(dry_run);

        let Cli { command } =
            Cli::try_parse_from(["jscout", "enrich", "."]).expect("default enrich parses");
        let Command::Enrich {
            max_occurrences, ..
        } = command
        else {
            panic!("expected enrich")
        };
        assert_eq!(max_occurrences, None);
    }
}
