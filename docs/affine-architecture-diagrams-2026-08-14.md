# jscout architecture diagrams

Date: 2026-08-14
Corpus: AFFiNE at `0f349af8ee` (`canary`)
Tested jscout commit: `1d0d9b0` (merged via PR #25)

Related documents:

- [AFFiNE experiment analysis](affine-experiment-analysis-2026-08-14.md)
- [Proposed fixes and next steps](affine-proposed-fixes-2026-08-14.md)
- [Comprehensive experiment output](affine-experiment-full-output-2026-08-14.md)

## 1. How the pieces fit together

```mermaid
flowchart LR
    SRC["JavaScript and TypeScript source"] --> IDX["jscout index"]
    IDX --> FTS["AST chunks and FTS5 / BM25"]
    IDX --> STRUCT["Symbols, imports, exports, references and deterministic entities"]
    STRUCT --> GRAPH["Structural graph"]

    FTS --> EMB["jscout embed — optional"]
    EMB --> ECACHE["Durable BGE-M3 vectors keyed by chunk hash"]
    ECACHE --> VEC["SQLite vector occurrence index"]

    QUERY["Agent query"] --> BM25["BM25 candidates"]
    QUERY --> VEC
    BM25 --> RRF["Reciprocal-rank fusion"]
    VEC --> RRF
    RRF --> RERANK["Cross-encoder reranker — optional"]
    RERANK --> HITS["Ranked chunk hits"]
    HITS --> EXPAND["Structural expansion — optional"]
    GRAPH --> EXPAND

    TSP["TypeScript projects and member-call occurrences"] --> ENRICH["jscout enrich — optional"]
    ENRICH --> FACTS["Checker facts with provenance and confidence"]
    FACTS --> GRAPH

    SCOUT["LLM scouting — optional, later layer"] --> MEMORY["Versioned semantic artifacts and workflow memory"]
    MEMORY --> QUERY
```

The three expensive/intelligent layers are independent:

- `embed` changes semantic retrieval, not the graph;
- `enrich` changes graph expansion, not vectors;
- scouting creates a separate semantic-memory overlay rather than changing code chunks or claiming deterministic truth.

## 2. What exists after each phase

```mermaid
flowchart TD
    P0["Source checkout"] --> P1["After jscout index"]
    P1 --> P2["After jscout embed"]
    P2 --> P3["After jscout enrich"]
    P3 --> P4["After scouting — future optional layer"]

    P1 --- D1["Files and roles<br/>AST chunks and FTS<br/>symbols and references<br/>entities and deterministic edges"]
    P2 --- D2["Everything from index<br/>plus code-chunk vectors<br/>plus vector occurrence index"]
    P3 --- D3["Everything from embed<br/>plus TypeScript checker facts<br/>plus likely or possible member-call edges"]
    P4 --- D4["Everything from enrich<br/>plus generated workflow concepts<br/>with supports, provenance and freshness"]
```

## 3. Exactly what is embedded

```mermaid
flowchart LR
    PATH["Repository-relative path"] --> TEXT["Embedding text"]
    SCOPE["Enclosing scope"] --> TEXT
    SYMBOL["Symbol name"] --> TEXT
    CODE["Chunk source, capped at 24 KB"] --> TEXT
    TEXT --> MODEL["BAAI/bge-m3"]
    MODEL --> VECTOR["1024-dimensional vector"]
    HASH["Chunk content hash"] --> KEY["Embedding cache key"]
    PROFILE["Provider, model, revision, dimensions and device"] --> KEY
    VECTOR --> KEY

    GRAPH["Graph edges"] -. "not embedded" .-> OMIT["Separate structural state"]
    CHECKER["Checker facts"] -. "not embedded" .-> OMIT
    MEMORY["Semantic artifacts"] -. "not embedded as code chunks" .-> OMIT
```

Conceptually, the model receives:

```text
// file: packages/example/src/file.ts
// scope: ExampleClass
// symbol: run
<chunk source text>
```

AFFiNE's maximum indexed chunk was 7,977 bytes, so no chunk hit the 24 KB embedding cap.

## 4. Query execution

```mermaid
flowchart TD
    Q["Question or identifiers"] --> LEX["BM25 over chunks_fts"]
    Q --> QEMB["Embed query when a provider is configured"]
    QEMB --> ANN["Vector nearest-neighbor candidates"]
    LEX --> FUSE["RRF fusion"]
    ANN --> FUSE
    FUSE --> RR{"Reranker enabled?"}
    RR -->|"No"| RESULTS["Ranked hits"]
    RR -->|"Yes"| CROSS["Cross-encoder over candidate content"]
    CROSS --> RESULTS
    RESULTS --> ROLE["Origin and role filtering"]
    ROLE --> EX{"Expansion requested?"}
    EX -->|"No"| RESPONSE["Search response"]
    EX -->|"Yes"| SEEDS["Resolve hit anchors"]
    SEEDS --> TRAVERSE["Ranked, budgeted graph traversal"]
    TRAVERSE --> RESPONSE
```

The AFFiNE run showed that the current reranker should not be treated as an unconditional improvement. Hybrid search without reranking was the most reliable default during this experiment.

## 5. Enrichment execution

```mermaid
flowchart TD
    CALLS["Unresolved static member-call occurrences"] --> OWNERS["Assign owning TypeScript projects"]
    CONFIGS["Discovered tsconfig files"] --> OWNERS
    OWNERS --> PLAN["Deterministic, resumable batch plan"]
    PLAN --> PROJECT["One isolated TypeScript Program per project"]
    PROJECT --> ANSWER["Resolve receiver type and declaration at exact occurrence"]
    ANSWER --> STAGE["Stage facts in SQLite"]
    STAGE --> RECONCILE["Reconcile answers across owning projects"]
    RECONCILE --> CONF["Assign likely or possible confidence"]
    CONF --> PUBLISH["Atomically activate checker batch"]
    PUBLISH --> GRAPH["Project checker member_call edges"]
```

AFFiNE exposed an ownership problem in this flow: package projects checked their occurrences, then `tsconfig.eslint.json` claimed and rechecked all 49,142 eligible occurrences. The run succeeded, but the tooling-only aggregate project dominated memory and wall time.

## 6. Reindexing, embeddings and enrichment lifecycle

```mermaid
flowchart LR
    CHANGE["Checkout, branch switch or source edit"] --> REINDEX["jscout index"]
    REINDEX --> NEW["Fresh structural snapshot"]
    REINDEX --> HASHES["Current chunk hashes"]
    HASHES --> CACHE{"Cached vector for hash and profile?"}
    CACHE -->|"Yes"| REMAT["Rematerialize vector occurrence"]
    CACHE -->|"No"| REEMBED["Embed missing chunk"]
    REEMBED --> REMAT

    REINDEX --> INVALID["Previous checker batch is not structural truth for new snapshot"]
    INVALID --> RERUN["Rerun enrich when checker edges are wanted"]
    RERUN --> NEW
```

This reflects the intended rebuild philosophy:

- structural state is cheap and replaceable;
- embeddings are expensive and durable by content hash;
- checker enrichment is a snapshot-scoped overlay that can be recomputed;
- watcher behavior is a separate operational concern.

## 7. Recommended agent workflow

```mermaid
flowchart TD
    ASK["Repository question"] --> SEARCH["Hybrid search without reranking"]
    SEARCH --> SEED["Choose a concrete file, function or method"]
    SEED --> EXACT["Use calls, events or who-uses when an exact surface applies"]
    SEED --> NEIGHBOR["Focused neighborhood by relation and confidence"]
    EXACT --> READ["Read defining source spans"]
    NEIGHBOR --> READ
    READ --> GAP{"Workflow boundary still missing?"}
    GAP -->|"No"| ANSWER["Answer with file and line evidence"]
    GAP -->|"Yes"| FALLBACK["Targeted rg or language-native tooling"]
    FALLBACK --> READ
```

Search localizes. Enrichment supplies occurrence-specific TypeScript dispatch. Source inspection still establishes argument values, branch ordering, mutations and behavior.

## 8. Current coverage boundary

```mermaid
flowchart LR
    REPO["AFFiNE repository"] --> JS["JavaScript and TypeScript"]
    REPO --> OTHER["Rust, Swift, Kotlin and other languages"]
    JS --> INDEXED["Indexed, embedded and TypeScript-enriched"]
    OTHER --> UNINDEXED["Not represented by the current jscout corpus"]

    INDEXED --> STATIC["Static calls, imports, contracts and known entities"]
    INDEXED --> DYNAMIC["Decorators, runtime registries, GraphQL transport and interface dispatch"]
    STATIC --> AVAILABLE["Partly or fully traversable"]
    DYNAMIC --> PARTIAL["Partially represented; several joins remain missing"]
```

The current overview does not make this boundary prominent enough. A "complete embedding corpus" currently means complete coverage of indexed JavaScript/TypeScript chunks, not complete coverage of the repository.
