# jscout

A fast JavaScript/TypeScript codebase indexer for RAG and agent retrieval, written in Rust on [oxc](https://oxc.rs).

The runtime value graph remains primary: functions, classes, components, calls,
renders, and executable module edges work the same way for JavaScript and
TypeScript. Type-only bindings never become runtime edges. A separate
documentary contract plane indexes interfaces, aliases, enums, decorators,
schemas, and exported API types without claiming they execute.

## Project documents

- [PLAN.md](PLAN.md) is the single current architecture and roadmap.
- [eval/](eval/) contains dated evaluation protocols and results.
- [presentations/](presentations/) contains dated, non-normative explanatory
  artifacts; they are not revision material unless explicitly requested.

## Getting started

```bash
cargo build --release            # binary at target/release/jscout
jscout index /path/to/repo       # build .jscout.db beside the sources
jscout search /path/to/repo "checkout inventory"
```

Everything deterministic — indexing, search, graph traversal, the MCP server —
is the Rust binary alone, with no runtime dependencies. Generative scouting
(`jscout scout …`, `jscout llm doctor`) additionally requires Node >= 22.19.0
because model calls run through the bundled pi-ai gateway sidecar. Release
archives contain the gateway and its pinned dependencies; a source checkout
needs one additional install step:

```bash
npm ci --prefix gateway          # exact package-lock installation
jscout llm doctor                # verifies node, gateway, auth, and the model
```

The gateway entry (`gateway/src/main.mjs`) is discovered beside the jscout
binary in an installed layout, or at the repository root when running
`target/{debug,release}/jscout` in development. `--gateway-path` or
`JSCOUT_PI_AI_GATEWAY` overrides discovery explicitly; there is no fallback
search beyond these locations. The default model
`openai-codex:gpt-5.6-terra` bills to a ChatGPT plan through pi-ai's OAuth
credential store; see [Configuration](#configuration) for the complete
environment surface and auth setup.

## Commands

```
jscout index <root>            # build/update .jscout.db (incremental, content-hash based)
                               #   --database PATH isolates index/memory state
                               #   --deps pkg,@scope/pkg indexes named dependency internals
jscout search <root> "query"   # hybrid BM25 + embedding search (BM25-only without a provider)
                               #   add --expand for a bounded structural context pack
jscout who-uses <root> SPEC    # all usage sites of a symbol, grouped by confidence
jscout neighborhood <root> A   # bounded structural traversal around an anchor
jscout workflow-candidates R S # experimental fingerprinted candidate-set diagnostic
jscout events <root> [name]    # string-keyed event wiring (emit/listen sites)
jscout calls <root> METHOD     # exact member-call sites matched on the AST
                               #   --arg merge=replace --receiver wave.card --json
jscout watch <root> [--embed]  # hash-incremental parse; projection is currently rebuilt
                               #   repeat --deps from index to retain that corpus
jscout embed <root>            # embed chunks missing embeddings (cached by content hash)
jscout entities <root> [query] # runtime, contract, route, config, data, flag, host entities
jscout paths <root> A B        # bounded ranked paths between exact boundaries
jscout overview <root>         # deterministic cold-start inventory
  --semantic                   #   optional current/fresh untrusted memory overlay
jscout mcp <root>              # MCP stdio server: code, graph, entity, overview,
                               #   semantic_memory, exact evidence, and annotate tools
jscout memory <root> [query]   # bounded semantic artifacts, relations, and freshness
  --anchor EXACT_ANCHOR --source
                               #   code-to-memory join + hash-verified source drill-down
jscout memory <root> --artifact ID --source
                               #   inspect a current or historical artifact exactly
jscout annotate <root> in.json # write a validated semantic artifact
jscout llm doctor              # verify Node, pi-ai, plan auth, and default model capabilities
jscout scout workflows R       # auto-select deterministic workflow entry surfaces
  --max-calls N                #   default: openai-codex:gpt-5.6-terra via ChatGPT plan
jscout scout workflows R       # classify one agent-supplied workflow boundary
  --seed ANCHOR                #   repeat --seed to define one multi-seed boundary
jscout scout cards R           # evidence-backed cards for selected symbols
  --max-calls N                #   --anchor SPEC selects subjects explicitly (repeatable)
jscout scout summaries R       # bottom-up file/module/repository summaries over artifacts
  --max-calls N                #   --level file|module|repository, --scope KEY (repeatable)
jscout scout concepts R        # concepts from exact workflow-name/card-domain-term vocabulary
  --max-calls N                #   --term TEXT selects normalized groups explicitly (repeatable)
jscout scout refresh R         # replace stale/degraded workflows, cards, summaries, and concepts
  --max-calls N                #   reuses each artifact's recorded model/configuration
jscout stats <root>            # parse stats
jscout chunks <root>           # dump AST-aware chunks as JSONL
jscout agent-guide             # print agent integration guidance
jscout agent-guide --install R # install a project-local jscout skill
```

Build a distributable archive containing both the Rust binary and installed
gateway:

```bash
scripts/package-release.sh              # host target
scripts/package-release.sh TARGET_TRIPLE
```

The archive is written under `target/release-packages/`. Extract it anywhere,
put its directory on `PATH`, and keep the adjacent `gateway/` directory with
the binary. Deterministic indexing and retrieval never start Node.

`SPEC` is `NAME` or `path-substring:NAME`, e.g. `getUser` or `services/user:getUser`.

Workflow-candidate seeds must each resolve uniquely to a symbol.
File anchors are rejected because a file can contain multiple unrelated
operations; choose an exported symbol or pass its exact returned `sym:` anchor.

`A` accepts a returned node key, a repo-relative file path, a symbol name, or
`path-substring:NAME`. Every neighborhood includes the current repository
snapshot. When reusing an anchor after edits, pass that value with `--snapshot`;
jscout re-resolves stale symbol anchors by path, scope, and name, and returns an
error with candidates instead of guessing when the identity is ambiguous.
Traversal defaults to `certain`/`likely` edges. Use
`--min-confidence possible` to include unresolved string-event hubs and other
explicit candidates. Unknown-receiver member calls are projected through
property hubs; use depth two to traverse from a candidate symbol to possible
callers without materializing every call-site × symbol pair.

## Semantic scouting

`jscout scout workflows`, `cards`, `summaries`, and `concepts` make
schema-constrained model calls through the bundled pi-ai gateway. Workflow and
concept runs additionally require exhaustive candidate classification.
Generative calls default to
`openai-codex:gpt-5.6-terra`, which uses the ChatGPT-plan OAuth path; `--model`
and `JSCOUT_LLM_MODEL` remain explicit overrides. Request policy is explicit
per command: `--timeout` (default 300 s per request), `--max-calls` (a hard
command-level budget), `--context-bytes` (default 240 000 serialized evidence
bytes, also checked against the selected model's context window),
`--reasoning`, `--service-tier` (rejected where the provider API does not
support tiers), and `--rebuild` (supersede a completed identical run instead
of reusing it). See [Configuration](#configuration) for the complete
environment surface.

Without `--seed`, scouting derives bounded seeds from routes, GraphQL
operations, runtime handlers/producers, lifecycle/job/DI boundaries, and
exported package/application entry files. Export seeding is deliberately
limited to manifest-resolved entries and conventional `index`, `main`,
`server`, `app`, or `entry` filenames. Routes/GraphQL/handlers rank ahead of
producers and dispatchers, which rank ahead of DI injection sites. Equal
deterministic candidate fingerprints are collapsed before any call. Automatic
mode requires an explicit `--max-calls`; completed matching runs are reused
before that budget is spent, and one over-budget boundary is reported and
skipped without blocking smaller boundaries. `--dry-run` prints the exact
resolved seeds, candidate fingerprints, candidate counts, evidence file
counts, and evidence bytes without starting Node or contacting a model.

`jscout scout cards` writes one evidence-backed card per selected symbol.
Without `--anchor`, subjects are the union of exported production symbols,
runtime boundary endpoints, and participants of current published workflows,
deduped by anchor and capped at 1024 with the discovered count and the
per-source breakdown of everything discovery found reported.
`--anchor` selects subjects explicitly — each resolves uniquely to a symbol,
like a workflow seed, and each becomes its own run; automatic mode requires
`--max-calls`, explicit mode defaults it to the number of anchors. Evidence is
the subject's declaring file plus its deterministic depth-1 edges, which the
prompt forbids restating: a card carries purpose, architectural role, domain
terms, side effects, invariants, and failure modes, never signatures or call
lists. Every individual claim cites its own line ranges in that file and is
published as its own support at `likely` confidence; an optional field the
model cannot support is omitted rather than guessed, and a claim without exact
evidence fails the run instead of downgrading it. `--dry-run` prints the
selected subjects, evidence bytes, per-item request bytes, and budget
decisions without starting Node or contacting a model.

`jscout scout summaries` writes one summary per scope, strictly bottom-up over
already-validated artifacts and never over raw source. Levels are `file`
(children: current cards and workflows), `module` (children: the file summaries
of one workspace package), and `repository` (children: module summaries plus
file summaries no package owns); omitting `--level` runs all three in that
order under a single `--max-calls` budget, so a module summary is planned from
the file summaries the same invocation just published. Every claim must cite
the enumerated child references it rests on, and each citation is published as
a `summarizes` relation pinned to that child's artifact fingerprint. Every
planned child also becomes a whole-summary input dependency, whether cited or
not — uncited prose never validates, and a scope with no current children is
not a summary subject at all. Publication rechecks inside the transaction that
the scope still has exactly the planned child set and that every child remains
current with its pinned fingerprint, so a child added, removed, or replaced
mid-flight refuses the write rather than publishing immediately stale prose.
Freshness then propagates upward: a missing, superseded, or changed child
stales its parent, and a current-but-not-fresh child degrades it, even when the
parent's own text never changed. `--scope KEY` selects scopes explicitly and
requires `--level`, since scope keys are level-specific. `--dry-run` prints the
per-level plans, child counts, and request bytes without starting Node or
contacting a model.

`jscout scout concepts` writes one concept per exact normalized vocabulary
group. Discovery considers only current fingerprinted workflows whose `/name`
claim has exact supports and current fingerprinted cards whose string-valued
`/domain_terms/N` claim has exact supports; other body fields and unsupported
prose are excluded. The versioned normalizer applies Unicode NFKC, Unicode
lowercase, and trimmed/collapsed whitespace while preserving punctuation, so
case and compatibility variants group together but `invoice-id` and
`invoice id` do not. The model supplies a repository-specific definition and
cites deterministic child references; it cannot choose the concept identity,
invent aliases, or add children. Published aliases exhaustively reproduce the
observed NFKC/whitespace-normalized display spellings, and the concept copies
none of their spans onto newly generated prose. Instead, claim-level
`related_to` relations pin each child fingerprint and preserve the drill-down
through that child's exact source supports.

Without `--term`, all bounded groups are planned and `--max-calls` is required.
Repeatable `--term TEXT` selects an existing group through the same normalizer
and defaults the call budget to the number of supplied terms. Oversized groups
are skipped in automatic mode and fail explicitly selected runs rather than
being truncated. Publication atomically rechecks the snapshot, child
fingerprints, exact child set, and concept lineage. Near-duplicate matching and
many-lineage merging are deliberately not implemented: punctuation, fuzzy
similarity, stemming, and embedding proximity never cause an implicit merge.
`--dry-run` prints the normalized groups, exact aliases, child/support counts,
input bytes, skips, and budget decisions without starting Node or contacting a
model.

Generated workflows record their resolved seeds, traversal limits, service
tier, model, and reasoning policy in the run ledger; cards record their
subject anchor, summaries their level and scope key, and concepts their
normalized vocabulary group the same way. After
indexing exposes source or structural-context drift, `jscout scout refresh
--max-calls N` selects current stale/degraded generated workflows, cards, and
summaries, and concepts and publishes immutable successors. A summary needs no
rule of its own here: child drift already makes it non-fresh, so it selects
naturally and is replanned against the children that are current now; a concept
is replanned from the currently supported exact vocabulary group. Index and
watch never make model calls. Runs created before replay configuration was
stored remain visible but are reported as non-refreshable; jscout does not
guess their original boundary. A stale target whose recorded seed or scope no
longer resolves is reported and skipped without blocking other refreshes.

## Semantic retrieval

`jscout memory` and the structural-profile MCP `semantic_memory` tool query
persistent memory independently of BM25/vector code ranking. They filter by
artifact type, computed freshness, exact evidence anchor, direct relation, or
historical artifact id. Current artifacts are the default; an exact historical
id reports `current: false` and its `superseded_by` successor.

When the selected results include current, fresh concepts, the response also
contains deterministic `concept_tags`: deduplicated file-level associations
and chunk-level associations for each indexed chunk whose line range overlaps
an exact support reached through the concept's claim-level child relations.
These are derived R2 localization hints, not stored claims and not additional
model output. `--concept-tag-limit N` (MCP:
`concept_tag_limit`, default 40, maximum 200) bounds them independently; the
complete response-byte budget removes whole concept tags before source,
relations, or semantic artifacts and reports the omitted count.

Add `--source` (MCP: `include_source=true`) to follow the artifact's pinned
outgoing claim-citation relations to leaf supports. Empty-path whole-input
dependencies remain visible in the relation section but are never presented as
claim evidence. Every returned path identifies the intermediate artifact,
relation, and JSON claim pointer; distinct claims that cite the same child stay
distinct. `--source-depth` bounds relation traversal and the response reports
depth/path truncation and skipped cycles explicitly.

Source resolution uses the indexed file identity—including virtual dependency
paths—then reads disk and compares the current bytes with the indexed hash. A
disk change that has not been indexed returns `source_status: "index-stale"`.
After re-indexing changed source, an older support returns
`source_status: "source-stale"`. Neither case returns a misleading excerpt; an
unavailable file is also explicit. Structural-context drift is reported
separately as support freshness and does not hide hash-verified source bytes.

`jscout overview` and MCP `repository_overview` return the deterministic corpus
inventory from one pinned SQLite read snapshot. Generated memory is absent by
default. `--semantic`/`include_semantic=true` adds a separately labelled,
untrusted overlay containing only current artifacts whose computed freshness is
`fresh`; cards require explicit type selection. Whole-response byte budgets
drop overlay artifacts before deterministic inventory and report every
omission. Generated prose never changes search scores.

## Configuration

jscout is configured through CLI flags and process environment variables; it
never auto-loads a `.env` file. [.env.example](.env.example) is the safe
copy-paste template. CLI flags always win over environment variables.

Generative scouting (`jscout scout …`, `jscout llm doctor`):

| Variable | Effect |
|---|---|
| `JSCOUT_LLM_MODEL` | Exact `provider:model` for generative calls; default `openai-codex:gpt-5.6-terra` (ChatGPT-plan OAuth). Overridden by `--model`. |
| `JSCOUT_LLM_REASONING` | Provider-normalized reasoning effort; unset means provider default. Overridden by `--reasoning`. |
| `JSCOUT_PI_AI_AUTH_FILE` | pi-ai OAuth credential store read by the gateway; default `~/.pi-ai/auth.json`. |
| `JSCOUT_PI_AI_OPENAI_BASE_URL` | Replace only the built-in `openai` provider endpoint. The model catalog, Responses transport, and `OPENAI_API_KEY` auth remain intact. |
| `JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS` | Validated JSON array of additional local OpenAI-compatible providers (see below). |
| `JSCOUT_PI_AI_GATEWAY` | Path to the gateway entry file when it is not discoverable beside the binary. Overridden by `--gateway-path`. Names a file, never a shell command. |
| `JSCOUT_NODE` | Node executable used to launch the gateway; default is `node` on `PATH`. Names a file, never a shell command. |

The plan-backed `openai-codex:*` default reads pi-ai's OAuth store; jscout
never creates or writes credentials, so sign in with pi-ai's own tooling
first. API-key providers use their standard environment variables through
pi-ai's built-in registry (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`GEMINI_API_KEY`, …). `jscout llm doctor` reports exactly which provider,
auth path, and billing path the selected model resolves to; plan, API, and
custom billing paths are recorded distinctly and never pooled. Doctor performs
no completion and spends no model tokens: it verifies the local runtime,
gateway protocol, model catalog, and configured auth path, but it cannot prove
that an account has remaining quota or that a remote endpoint is currently
healthy.

For API-key Terra through a non-default OpenAI gateway, configure the built-in
provider rather than declaring a custom provider:

```bash
export OPENAI_API_KEY='...'
export JSCOUT_LLM_MODEL='openai:gpt-5.6-terra'
export JSCOUT_PI_AI_OPENAI_BASE_URL='https://gateway.example.com/v1'
jscout llm doctor
```

The endpoint must implement the OpenAI Responses API, streaming, and tool
calls. `llm doctor` prints the resolved endpoint. URLs containing credentials
are rejected, as are URL query strings and fragments; put the key only in
`OPENAI_API_KEY`. `JSCOUT_PI_AI_GATEWAY` is unrelated: it names the local Node
sidecar file, not the remote API endpoint.

Custom OpenAI-compatible providers target local keyless servers (Ollama,
LM Studio, vLLM); the gateway sends a placeholder API key:

```json
[{"id": "local", "baseUrl": "http://127.0.0.1:11434/v1",
  "models": [{"id": "qwen3:32b", "contextWindow": 131072, "maxTokens": 32768}]}]
```

The gateway owns one visible retry layer: at most two retries with 500 ms then
1,000 ms backoff, only for classified connection, timeout, rate-limit, overload, or
capacity failures. Every attempt keeps the exact provider, model, service tier,
and billing path. Auth, schema, context-window, quota, credit, and billing
failures are terminal; there is no hidden provider/model/tier fallback. The
command timeout includes retries and backoff. The first Ctrl-C sends
cancellation to the active gateway request; a second Ctrl-C, or an interrupt
when no request is active, forces exit status 130.

Normal gateway errors use stable controlled messages. Provider exception text,
prompts, tool arguments, and credential values are not written to stderr or the
run ledger; common key/token forms are redacted again at the protocol boundary.
Do not enable third-party HTTP debug logging in a shell that contains secrets.

jscout does not install a global proxy agent or reinterpret proxy variables;
transport behavior remains provider-adapter specific. For a mandatory proxy,
prefer an explicit compatible endpoint via
`JSCOUT_PI_AI_OPENAI_BASE_URL`. Node reads `NODE_EXTRA_CA_CERTS` at process
startup for private certificate authorities. Standard public HTTPS needs no
TLS configuration. Keep proxy credentials out of endpoint URLs and environment
values likely to be printed by unrelated tooling.

Retrieval and diagnostics:

| Variable | Effect |
|---|---|
| `VOYAGE_API_KEY`, `OPENAI_API_KEY`, `JSCOUT_EMBED_URL`, `JSCOUT_EMBED_KEY`, `JSCOUT_EMBED_MODEL`, `JSCOUT_EMBED_PROVIDER`, `JSCOUT_QUERY_PREFIX` | Optional embedding providers; see [Embeddings](#embeddings-optional). |
| `JSCOUT_RERANK_URL`, `JSCOUT_RERANK_MODEL`, `JSCOUT_RERANK_TOP`, `JSCOUT_RERANK_CHARS` | Optional cross-encoder reranking; see [Reranking](#reranking-optional). |
| `JSCOUT_TIMING` | Print per-stage latency to stderr during search and indexing. |
| `JSCOUT_DEBUG` | Print per-file extraction progress to stderr during indexing. |
| `JSCOUT_TELEMETRY_FILE`, `JSCOUT_SESSION_ID`, `JSCOUT_TASK_ID`, `JSCOUT_PROFILE_LABEL` | Opt-in MCP telemetry and run labels; see [MCP integration](#mcp-integration). |

Indexing continues past file-local read and extraction errors. The final count
is followed by every failed path, its stage (`read` or `extract`), and the
underlying error on stderr; `watch` prints the same detail on each cycle.

## Call-site queries

`jscout calls` answers "where is this option passed to this method?" with
exact AST matching instead of line-based text joins:

```bash
jscout calls /path/to/repo insert --arg merge=replace --receiver wave.card --json
```

Candidate files come from the index (member-call names plus full-text
argument tokens); matches re-parse those files, so every hit reports the
complete call span — a multiline call owns every line inside it — the
static receiver chain (`dbs.wave.card`), the matched argument position, and
the innermost enclosing declaration anchor. All `--arg KEY[=VALUE]` filters
must match top-level literal properties of the same object-literal argument;
`--arg-position` pins which argument that is. Candidate files are
hash-verified against disk, and drift fails the query instead of answering
from a stale index. Receiver identity stays checker-less: which
implementation actually handles the call remains an explicit candidate-set
question (`who-uses`, property hubs), never a silent guess. The same query
is exposed as the `calls` MCP tool.

## Search anchors and expansion

Search returns a repository snapshot plus ranked hits. Every hit includes a
`file_role`, a `file_anchor`, and one or more snapshot-scoped `anchors`
projected from the chunk's overlapping declarations. Roles are deterministic:
`production`, `test`, `fixture`, `generated`, `documentation`, or `unknown`.
Use repeatable `--file-role` flags to restrict primary hits. Chunks remain
retrieval units; they do not become graph identity.

Structural expansion is off by default and does not alter search scores. Add
`--expand` to attach a separately labelled context pack:

```bash
jscout search /path/to/repo "checkout inventory" --json --expand \
  --response-bytes 24000 \
  --expand-depth 1 --expand-seeds 3 \
  --expand-nodes 40 --expand-edges 120 --expand-bytes 24000
```

Expansion defaults to `production` and `unknown` file-backed nodes while
retaining structural hubs. Use repeatable `--expand-file-role` flags to opt
tests, fixtures, generated files, or documentation back in. Explicitly
included non-production nodes receive deterministic ranking penalties before
the traversal and global node/byte budgets are consumed.

`--response-bytes` caps the complete pretty-printed JSON envelope: hits,
expansion, budget metadata, and serialization overhead. The result reports its
actual `rendered_bytes`, original `unbudgeted_bytes`, and any omitted content.
The expansion node, edge, and payload limits are subordinate budgets shared
across all search-hit seeds. `--expand-min-confidence` defaults to `likely`;
use `possible` only when explicit unresolved candidates are useful.

Matching semantic memory is attached to CLI and structural-profile search by
default; use `--no-memory` or `--memory-limit` to control it. Every artifact carries
evidence supports and a computed `fresh`, `degraded`, or `stale` label. The
complete response-byte limit includes semantic artifacts.

## Confidence tiers

- **certain** — resolved through binding analysis + Node module resolution (incl. package.json `exports`, tsconfig `paths`, barrel/star re-exports, CommonJS `require` with literals, dynamic `import('...')` literals).
- **possible** — name-matched member calls (`x.getUser()`): candidates listed, never silently dropped. This is the honest checker-less answer for calls through type annotations.

When an otherwise-certain reference resolves to multiple same-named root
symbols, the traversal projection emits every candidate at `possible`
confidence and includes ambiguity details instead of dropping the edge.

Event wiring (`emit('x')` ↔ `on('x')`) is surfaced by the `events` tool/command.

## Scoped dependency indexing

Dependency internals are opt-in and package-scoped. jscout never walks all of
`node_modules`:

```bash
jscout index /path/to/repo --deps zod,lodash
```

The selector list is authoritative. A later `jscout index` without `--deps`
removes the dependency corpus; use the same `--deps` values with `jscout watch`
to retain it. Discovery starts from real indexed importers and Node resolution,
so distinct installed versions become distinct package instances. An unused
root installation may also be selected by its exact package name. Package
roots and resolved modules are canonicalized before identity is assigned:
pnpm store links deduplicate to one physical instance, while declared
workspace links remain first-party code.

Each package is independently bounded to 10,000 files, 100 MiB total source,
and 2 MiB per file. Hidden directories, nested `node_modules`, minified names,
and strongly bundled long-line artifacts are skipped; a package entry point is
retained even when it looks bundled so the boundary does not disappear. When
the manifest exposes a `source` field or `source` export condition, that source
tree is preferred. Otherwise the active `exports`/`module`/`main` runtime tree
is indexed. Source-map reconstruction and Yarn Plug'n'Play zip archives are not
supported yet; PnP is rejected explicitly instead of being treated as a
missing package.

Indexed dependency files remain invisible to normal retrieval. All read
surfaces default to `repository,workspace`; include `dependency` explicitly:

```bash
# Dependency internals only
jscout search /path/to/repo "parse object schema" --origin dependency

# Cross the first-party/package boundary in one expansion
jscout search /path/to/repo "validate credentials" --expand --json \
  --origin repository,workspace,dependency

# Create vectors for dependency chunks (content-hash dedup still applies)
jscout embed /path/to/repo --origin dependency
```

The same `origins` allowlist is available on MCP search, definition,
who-uses, file-outline, events, and neighborhood calls. Search filtering is
applied before BM25/vector candidate ranking. Without dependency visibility,
the structural graph still exposes a versioned package-instance boundary hub;
indexed modules sit behind that hub and enter traversal only when their origin
is allowed.

Canonical runtime-entity nodes are also file-less boundary hubs. Their source
occurrences and symbol endpoints retain file origin, so an entity identity can
join first- and third-party evidence without making dependency-backed endpoint
code visible unless `dependency` is included in the caller's origin filter.

## Embeddings (optional)

Search works BM25-only out of the box. For hybrid semantic search set one of:

- `VOYAGE_API_KEY` — uses `voyage-code-3`
- `OPENAI_API_KEY` — uses `text-embedding-3-small`
- `JSCOUT_EMBED_URL` (+ `JSCOUT_EMBED_KEY`) — any OpenAI-compatible endpoint (Ollama, LM Studio, vLLM)

Overrides: `JSCOUT_EMBED_MODEL`, `JSCOUT_EMBED_PROVIDER=voyage|openai|none`.

Asymmetric models: when the model name contains `nomic-embed-code` or `coderankembed`,
queries are automatically prefixed with `"Represent this query for searching relevant
code: "` (documents embed raw). Override with `JSCOUT_QUERY_PREFIX`.

LM Studio example (loads `nomic-embed-code` GGUF, serves OpenAI-compatible API):

```bash
JSCOUT_EMBED_URL=http://localhost:1234/v1/embeddings \
JSCOUT_EMBED_MODEL=text-embedding-nomic-embed-code \
jscout embed /path/to/repo
```

Embeddings are keyed by (chunk content hash, model): unchanged code is never
re-embedded, and multiple models can coexist in one index.

## Reranking (optional)

Set `JSCOUT_RERANK_URL` to a cross-encoder service speaking
`POST {query, candidates:[{id,text}]}` → `{scores:[{id,score}]}` (e.g. a local
bge-reranker-v2-m3). The top RRF candidates are reranked before final ordering.
Tuning: `JSCOUT_RERANK_TOP` (candidate pool, default 50), `JSCOUT_RERANK_CHARS`
(per-candidate truncation, default 4000), `JSCOUT_RERANK_MODEL`.
Diagnostics: `JSCOUT_TIMING=1` prints per-stage latency (bm25 / embed-query /
vector-scan / rerank) to stderr on search and structural-projection stage
timings during indexing.

## MCP integration

```json
{
  "mcpServers": {
    "jscout": { "command": "/path/to/jscout", "args": ["mcp", "/path/to/repo"] }
  }
}
```

Run `jscout index` (or `jscout watch`) beside it to keep the DB fresh.

MCP metadata alone does not reliably cause every agent client to select a
repository tool. Install the shipped project-local guide so supported agents
receive an explicit integration contract:

```bash
jscout agent-guide --install /path/to/repo
```

The command creates `.agents/skills/jscout/SKILL.md` and refuses to overwrite
an existing guide. Use `jscout agent-guide` to print the same text for clients
that consume `AGENTS.md` or another instruction format.

For controlled evaluation, `jscout mcp` accepts `--profile baseline` (no
semantic memory, `annotate`, `neighborhood`, or search expansion) and
`--profile structural` (the default). `--database PATH` separates the index
and semantic-memory state from the source root for isolated warm/cold runs.
See [eval/README.md](eval/README.md) for the paired-run protocol and grader.

`definition` returns full source by default. `jscout mcp --source-view elided`
enables the experimental deterministic renderer, and each call can override it
with `view: "full"` or `view: "elided"`. Both representations obey the same
per-definition `source_bytes` ceiling and report original/rendered byte counts.
The first SC-1 agent run found no compression on the artifacts selected by the
elided arm, so elision remains experimental rather than becoming the default.
The first discriminating three-arm run found no outcome gain over grep: both
grep and structural answered 4/4 exactly, while structural inspected fewer
files at substantially higher agent-token cost. See
[eval/results/ai-pipe-discriminating-2026-08-07.md](eval/results/ai-pipe-discriminating-2026-08-07.md).

For opt-in agent-behavior measurement, start MCP with
`--telemetry .jscout-telemetry.jsonl` or set `JSCOUT_TELEMETRY_FILE`. The JSONL
records tool name, latency, success, response size, session, and snapshot. It
does not record queries, arguments, source, or results. Set
`JSCOUT_SESSION_ID` to correlate calls from one evaluation run and
`JSCOUT_TASK_ID` to join it to an evaluation task. Profile and task labels are
included in each record; `JSCOUT_PROFILE_LABEL` overrides the recorded
profile label. Expanded searches also record aggregate node totals
and `expansion_role_counts`; no paths or source are added to telemetry.
Semantic calls add only aggregate artifact returned/written counts and
fresh/degraded/stale totals.

## Storage

Everything lives in one SQLite file, `.jscout.db`, in the repo root (add it to
`.gitignore`): chunks + FTS5 (BM25), symbols, import/export tables, classified
references, event/member-call sites, embeddings, and a disposable
`graph_nodes`/`resolved_edges` traversal projection. The projection is rebuilt
after indexing so barrel changes can reroute references in otherwise unchanged
files without leaving stale graph edges behind. Runtime module links use
`import`/`imports_package`; requests found only in type bindings use the
documentary `imports_types`/`imports_package_types` kinds. File roles live on canonical
file rows and are refreshed even when source hashes are unchanged. Files also
carry `repository`, `workspace`, or `dependency` origin plus optional package
instance/path identity. Package instances record canonical root, name, version,
locator, manifest hash, and completeness status.

Agent-authored and generated `workflow`, `card`, `summary`, `concept`, and
`annotation` records live in separate `semantic_artifacts`/
`semantic_supports` tables; they never become structural edges. Workflow
participants are explicitly `defining` (the minimal stable
cross-file skeleton) or `supporting` (internal helpers and leaf operations), so
evidence does not flatten every related function into an equal boundary.
Supports store source and direct structural-context fingerprints.
Re-indexing preserves memory and makes source/context drift visible instead of
silently deleting or serving the record as current.

Workflow writes use a direct request: `participants` is top-level and every
participant carries its own anchor, role, scope, evidence file/span, and
confidence. jscout constructs the stored body and support pointers. Generic
`body`/`supports` input is reserved for `annotation` records. Writers retain
every distinct stable cross-file production stage as a participant; internal
or leaf stages are `supporting`, not compressed into another participant's
prose role.
