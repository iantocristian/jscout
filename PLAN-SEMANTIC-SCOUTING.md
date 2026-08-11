# jscout semantic scouting — implementation plan

> Status: active implementation plan, 2026-08-11. G1–G5 are complete; G6 is
> next.
>
> This plan is the executable continuation of SC-2 in
> [PLAN-KG-REVISED-CODEX.md](PLAN-KG-REVISED-CODEX.md). It supersedes that
> document's requirement to run another product-value gate before making LLM
> calls. Its trust, evidence, confidence, freshness, and candidate-closure
> rules remain in force.

## Decision

jscout remains a Rust application. Rust owns repository indexing, deterministic
candidate generation, prompts and schemas, validation, persistence, freshness,
retrieval, CLI/MCP behavior, and gateway lifecycle.

All **generative** model calls go through a small JavaScript sidecar built on
`@earendil-works/pi-ai`. The sidecar owns provider registration, model lookup,
credentials, provider-specific reasoning options, request execution,
cancellation, and normalized usage/error reporting. No provider SDK is called
directly from Rust.

This gives jscout the providers and the ChatGPT-plan OAuth path supported by
pi-ai. It does not mean every model behaves identically or supports every
option; the gateway must report capabilities and reject unsupported requests
instead of silently dropping options.

The existing embedding path is not part of this change. Embeddings are a
separate API shape and remain behind the current Rust `EmbeddingProvider`
implementation until a separate migration is justified.

## What to reuse

Use local implementations as owned source material, not runtime dependencies:

- `raggazzi-ingestion-eval/lib/pi.mjs`: copy and reduce its model-spec parser,
  reasoning normalization, model registry, custom OpenAI-compatible
  provider registration, Codex-plan validation, auth-path handling, and
  credential store.
- `dentavis-bot/pi-ai-bridge`: copy its request limits, normalized usage,
  cancellation, sanitized errors, and protocol tests.
- `dms/knowledge/DECISIONS.md`: preserve the decisions to use
  `createModels()` for Codex OAuth, read `~/.pi-ai/auth.json`, label the billing
  path, and call `complete()` rather than `completeSimple()` when provider
  options such as `serviceTier` must survive.

Do not copy DentaVis's fixed-port HTTP server. In jscout, Rust is the only
consumer and already controls the process lifecycle. A stdio sidecar is smaller,
has no listening socket, cannot collide on a port, and exits with its parent.

Use pi-ai's `builtinModels()` collection so the gateway exposes the complete
built-in provider catalog, then add validated custom OpenAI-compatible
providers. Lazy provider implementations keep this from loading every SDK on
startup. G1 pinned `@earendil-works/pi-ai` 0.84.1 exactly in the gateway
lockfile. Upgrade deliberately with a gateway contract run. Do not introduce
new uses of the deprecated `@mariozechner/pi-ai` package.

## Process boundary

```text
agent / CLI / MCP
        |
        v
Rust jscout
  index -> candidates -> evidence pack -> validate -> SQLite -> retrieval
                           |
                           | JSONL over stdin/stdout
                           v
                   Node pi-ai gateway
             provider registry -> auth -> model API
```

The gateway is a transport adapter, not an agent and not a semantic authority.
Do not add `pi-agent-core`, a tool-execution loop, repository access, or SQLite
access to the JavaScript process.

### Lifecycle

- `jscout scout ...` starts one long-lived child for the command and reuses it
  across requests.
- Rust launches Node with argument arrays, never a shell command.
- stdout is reserved for versioned JSONL protocol messages; stderr is reserved
  for sanitized diagnostics.
- The first implementation processes one completion at a time. Request IDs are
  still mandatory so cancellation and later concurrency do not change the
  protocol.
- EOF, malformed JSON, a timeout, or an unexpected child exit fails the current
  run without creating a semantic artifact.
- Ctrl-C sends `cancel`, waits for a bounded grace period, then terminates the
  child.
- An optional externally managed gateway can be added later behind the same
  Rust trait. It is not required for semantic v1.

### Gateway discovery

Resolution order:

1. `--gateway-path` for development and diagnostics;
2. `JSCOUT_PI_AI_GATEWAY`;
3. the companion gateway installed alongside jscout;
4. a clear error from `jscout llm doctor` describing the missing Node/package
   requirement.

Resolve Node from `JSCOUT_NODE`, then `PATH`. Both gateway settings identify a
file path, not a shell command string; Rust executes `node <gateway-path>` with
separate arguments.

Resolve the model from `--model`, then `JSCOUT_LLM_MODEL`, then the explicit
plan-backed default `openai-codex:gpt-5.6-terra`. The selected provider, model,
and billing path remain visible in diagnostics and the run ledger. There is no
automatic fallback to another provider, model, or billing path.

Initial configuration surface:

| Setting | Purpose |
|---|---|
| `--model provider:model` / `JSCOUT_LLM_MODEL` | Exact pi-ai provider and model; default `openai-codex:gpt-5.6-terra` |
| `--reasoning effort` / `JSCOUT_LLM_REASONING` | Provider-normalized reasoning policy |
| `JSCOUT_PI_AI_AUTH_FILE` | Codex OAuth store; default `~/.pi-ai/auth.json` |
| `JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS` | Validated JSON for local/custom compatible endpoints |
| `--service-tier` | Explicit API billing/latency tier; rejected where unsupported |
| `--timeout` | Per-request wall-clock limit |
| `--max-calls` | Hard command-level request budget |
| `--context-bytes` | Maximum serialized evidence bytes below the selected model's context limit |

`openai-codex:*` is recorded as a plan-backed provider. `openai:*` and custom
OpenAI-compatible providers are recorded separately. Reports must never pool or
compare them as if they were the same billing path.

## Versioned JSONL protocol

Every line is one JSON object with `protocol: 1`, a request `id`, and a `kind`.
The child must answer `hello` before Rust sends a completion.

### Rust to gateway

- `hello`: protocol negotiation.
- `capabilities`: list providers/models and whether the selected model supports
  tools, reasoning, service tier, caching, and usage reporting.
- `complete`: model, reasoning, messages, one submit-tool schema, timeout,
  session/cache options, and optional provider options.
- `cancel`: abort one request.
- `shutdown`: clean exit.

The stdin dispatcher must keep reading while a completion promise is active so
a `cancel` message can abort that request. Sequential completions do not permit
a blocking read/complete/read loop.

The gateway accepts at most 16 MiB per line initially. This is a corruption/
memory guard, not the semantic context budget. Rust independently bounds each
evidence pack using the selected model's reported context window, reserves room
for instructions/schema/output, and enforces `--context-bytes` before the pack
crosses the boundary. Do not hard-code a 200k-token product ceiling; models with
larger verified contexts may use them.

### Gateway to Rust

- `ready`: gateway and pi-ai versions.
- `capabilities_result`: normalized capability data.
- `started`: selected provider/model/API and credential source category, never
  the credential value.
- `result`: exactly one submit-tool call plus normalized stop reason and usage.
- `error`: stable error code, retryability, capacity classification, and a
  sanitized message.
- `canceled`: terminal cancellation acknowledgement.

Do not return or persist hidden reasoning. Text-only responses, multiple tool
calls, unknown tools, invalid JSON arguments, and incomplete streams are
protocol failures.

### Structured output

Rust sends one synthetic tool, such as `submit_workflow_classification`, with a
JSON Schema generated from Rust's versioned output contract. The gateway passes
it to pi-ai and returns the tool arguments without executing anything.

Provider-native JSON-schema response formats are not the common contract. Tool
calling is the portability layer; Rust/Serde validation is authoritative.

## Rust modules

Add these boundaries rather than putting process management into `semantic.rs`:

```text
src/llm/
  mod.rs          LlmGateway trait and shared request/result types
  process.rs      Node discovery, child lifecycle, JSONL framing, cancellation
  protocol.rs     protocol-v1 wire structs and error mapping
  config.rs       CLI/env/project resolution and billing-path labels

src/scouting/
  mod.rs          run orchestration and snapshot transaction
  evidence.rs     bounded, line-numbered R1/R2 evidence packs
  workflow.rs     candidate-closed workflow schema and validation
  cards.rs        symbol-card schema and selection
  hierarchy.rs    file/module/package/repository aggregation
  concepts.rs     concept extraction, normalization, and links
  refresh.rs      stale/degraded selection and superseding runs
```

`semantic.rs` remains the generic evidence-backed artifact store and retrieval
surface. Split it only where needed; do not duplicate support/freshness rules in
each scout.

## Run ledger and storage changes

The current `semantic_artifacts` and `semantic_supports` tables remain canonical.
Add a run ledger so failures, exclusions, cost, and provenance do not disappear:

```sql
scout_runs(
  id INTEGER PRIMARY KEY,
  scout_kind TEXT NOT NULL,
  status TEXT NOT NULL,              -- running | completed | incomplete | failed | canceled | superseded
  gateway_protocol INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  billing_path TEXT NOT NULL,        -- plan | api | custom
  reasoning TEXT,
  prompt_version TEXT NOT NULL,
  source_snapshot TEXT NOT NULL,
  input_fingerprint TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  usage_json TEXT,
  error_code TEXT,
  started_at TEXT NOT NULL,
  completed_at TEXT
);

scout_classifications(
  run_id INTEGER NOT NULL REFERENCES scout_runs(id) ON DELETE CASCADE,
  anchor_key TEXT NOT NULL,
  decision TEXT NOT NULL,            -- defining | supporting | excluded
  role TEXT,
  evidence_json TEXT NOT NULL,
  PRIMARY KEY(run_id, anchor_key)
);

semantic_relations(
  src_artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id) ON DELETE CASCADE,
  dst_artifact_id INTEGER NOT NULL REFERENCES semantic_artifacts(id),
  relation TEXT NOT NULL,            -- summarizes | names_concept | related_to
  claim_path TEXT NOT NULL,
  confidence TEXT NOT NULL,
  dst_fingerprint TEXT NOT NULL,
  PRIMARY KEY(src_artifact_id, dst_artifact_id, relation, claim_path)
);
```

Add nullable `scout_run_id`, `input_fingerprint`, and `artifact_fingerprint`
columns. Generated artifacts must supply all three; agent-authored artifacts
keep the first two NULL but receive an artifact fingerprint. Backfill existing
artifact fingerprints from canonical serialized body, provenance, and sorted
supports during migration, then require them in every new write path.

`semantic_relations` handles dependencies that source spans cannot express:
module summaries depend on file summaries, repository summaries depend on
package summaries, and concepts relate to cards/workflows or other concepts.
Every parent claim must reach at least one exact source support directly or
through its children. A changed child fingerprint degrades/stales the parent
even when the leaf source lines have not changed. This table also makes
concept/workflow/card joins queryable without parsing `body_json`.

The database stores the validated structured result and normalized usage. Raw
prompts, source packs, raw provider payloads, credentials, and hidden reasoning
are not stored by default. An explicit `--debug-llm-dir` may write redacted
request/response fixtures outside SQLite.

## First vertical slice: candidate-closed workflow scouting (implemented)

The first real command is:

```text
jscout scout workflows ROOT --seed ANCHOR [--seed ANCHOR ...] \
  --model openai-codex:gpt-5.6-terra --reasoning high
```

### Pipeline

1. In one bounded SQLite read snapshot, resolve all seed anchors and construct
   the candidate/evidence inputs. Release that read transaction before waiting
   on the model; no database snapshot is held open across network latency.
2. Use the implemented `workflow_candidates` traversal. Refuse truncated
   candidate sets; report whether the remedy is a narrower seed/depth or a
   higher supported deterministic limit. Never ask the model to interpret an
   unknown partial boundary.
3. Construct a bounded, line-numbered evidence pack from full source plus the
   relevant runtime entities, contracts, routes, GraphQL operations, data
   resources, flags, and external hosts. Full source remains the default;
   LLM-written pseudocode is not input evidence.
4. Hash the snapshot, seeds, ordered candidates, evidence, schema, prompt,
   gateway protocol, and model policy into `input_fingerprint`.
5. Reuse a completed matching run unless `--rebuild` is explicit.
6. Ask the model to name and describe the workflow and classify **every**
   candidate exactly once as `defining`, `supporting`, or `excluded`.
7. Require a concise role and one or more evidence line ranges for every
   included candidate. Exclusions retain a short reason in the run ledger but
   do not become semantic supports.
8. Rust validates candidate closure, unique anchors, line ranges, current file
   hashes, at least one defining participant, body limits, and confidence no
   higher than `likely`.
9. In the publication transaction, recheck the repository snapshot, file
   hashes, and referenced child artifact fingerprints. If any changed, mark the
   run incomplete and publish nothing against the old inputs.
10. Commit the completed run, classifications, artifact, supports, and semantic
    relations atomically. A failed, incomplete, or canceled run creates no
    artifact.

The model cannot add an anchor outside the deterministic candidate set. If it
believes a participant is missing, it can set `incomplete_reason`; Rust records
the run as incomplete and publishes nothing. Candidate expansion remains a Rust
change, not model improvisation.

### Workflow output contract

```json
{
  "name": "invoice payment retry",
  "description": "Retries a failed payment and records the resulting state.",
  "candidates": [
    {
      "anchor": "sym:...",
      "decision": "defining",
      "role": "initiates retry after a failed charge",
      "evidence": [{"start_line": 41, "end_line": 58}]
    },
    {
      "anchor": "sym:...",
      "decision": "excluded",
      "reason": "generic logging helper"
    }
  ],
  "incomplete_reason": null
}
```

## Refresh and staleness

- Indexing continues to mark artifacts fresh, degraded, or stale from support
  and context hashes; no model call occurs during `jscout index`.
- `jscout scout refresh` selects stale/degraded generated artifacts, reconstructs
  their seeds and scout configuration, and produces immutable successors.
- A changed `input_fingerprint` creates a new run. A matching completed run is
  reused.
- Refresh is explicit in semantic v1. Watch-mode background refresh is deferred
  until request budgets, cancellation, and rate-limit behavior are operationally
  understood.
- Superseded artifacts remain attributable and queryable by ID but disappear
  from default retrieval.

## Semantic v1 layers

All layers reuse the same gateway, run ledger, support validation, freshness,
and immutable supersession. They do not get bespoke model clients.

### S2 — automatic workflow seed selection (implemented in G5)

Derive seeds deterministically from repository entry surfaces: routes, GraphQL
operations, runtime-boundary handlers/producers, lifecycle listeners, job/queue/
cron handlers, DI providers, exported package entry points, and agent-supplied
anchors. Dedupe candidate fingerprints before calling the model. Require
`--max-calls`; support `--dry-run` to show exact seeds and evidence budgets.

### S3 — symbol cards (next: G6)

Generate cards only for exported symbols, entity endpoints, workflow
participants, and explicitly requested anchors. Cards describe purpose,
architectural role, side effects, invariants, failure modes, and normalized
domain terms. Each claim needs exact support. Do not generate one card per
chunk or restate signatures/calls already present in R1/R2.

### S4 — hierarchical summaries (planned: G7)

Build file summaries from validated cards/workflows plus deterministic file
topology, module/package summaries from child summaries, and a repository
summary from package/module artifacts. Every prose claim links to child
artifact claims that recursively reach exact source supports. Large parents use
selected child claims under a deterministic budget; they do not receive the
entire repository in one prompt.

### S5 — concepts (planned: G8)

Concepts are normalized semantic entities inferred from validated workflow/card
terms, not embedding-cluster labels and not free-form tags over every chunk.

- Propose concepts with aliases, definition, related artifact IDs, and supports.
- Merge only exact normalized aliases automatically. Ambiguous near-duplicates
  remain separate until an LLM merge proposal or agent correction is validated.
- Link concepts to workflow/card artifacts and derive chunk/file tags through
  their evidence overlaps. Do not ask the LLM to tag each chunk independently.
- Concept-to-concept links are `likely` or `possible` semantic claims with their
  own support fingerprints.
- When all supports stale, the concept becomes stale; refresh supersedes it.

This supplies the vocabulary needed for questions such as “which workflows
touch invoice reconciliation?” without replacing exact code evidence.

### S6 — retrieval and agent surfaces (planned: G9)

Add bounded commands and MCP fields for:

- workflows involving an anchor, entity, contract, file, or concept;
- a symbol card with freshness and exact evidence;
- concept lookup and related workflows/symbols;
- deterministic repository overview with optional fresh semantic overlays;
- explicit stale inclusion and drill-down to source.

Semantic memory stays opt-in in generic search until the caller requests
memory or uses a semantic-specific surface. Never blend stale prose into a
current-looking structural answer.

### S7 — packaging and operations (partially implemented; completes in G9)

- Add a companion Node package under `gateway/` with its own exact lockfile.
- Define the supported Node version and check it in `jscout llm doctor`.
- Package the gateway beside release binaries or publish a version-matched
  `@jscout/pi-ai-gateway`; a standalone Rust binary must fail clearly rather
  than attempting a hidden install.
- Report jscout, database schema, gateway protocol, gateway package, pi-ai,
  provider, and model versions in diagnostics.
- Document Codex-plan login/auth-file setup, API-key providers, custom
  OpenAI-compatible endpoints, proxy/TLS behavior, and log redaction.
- Add bounded retries only for errors classified as transient/capacity. Never
  retry auth, schema, invalid-request, or context-limit failures. Service-tier
  fallback must be explicit policy, not a hidden billing change.

## Implementation order and commit boundaries

| Phase | Deliverable | Status | Suggested commit |
|---|---|---|---|
| **G1** | Companion package, provider/auth registry, protocol-v1 gateway, fake-provider protocol tests | Complete | `feat(gateway): add pi-ai model sidecar` |
| **G2** | Rust gateway trait/process client, config, cancellation, `llm doctor` | Complete | `feat(llm): add pi-ai gateway client` |
| **G3** | Run-ledger migration and persistence API | Complete | `feat(scout): record model runs and classifications` |
| **G4** | Candidate evidence pack, workflow schema, exhaustive validator, explicit-seed command | Complete | `feat(scout): generate candidate-closed workflows` |
| **G5** | Fingerprint reuse, refresh/supersession, deterministic auto seeds and call budgets | Complete | `feat(scout): add incremental semantic refresh` |
| **G6** | Selected symbol cards | Next | `feat(scout): add evidence-backed symbol cards` |
| **G7** | File/module/package/repository hierarchy | Planned | `feat(scout): add hierarchical semantic summaries` |
| **G8** | Concepts, aliases, artifact links, derived evidence tags | Planned | `feat(scout): add semantic concepts` |
| **G9** | CLI/MCP retrieval surfaces, packaging, docs, redaction/diagnostics | Planned | `feat(scout): ship semantic memory surfaces` |

Each commit must leave migrations forward-compatible and all existing
deterministic behavior intact. Do not combine the Node gateway, schema migration,
and first semantic caller into one review unit.

## Verification policy

Implementation does not pause for another retrieval/value experiment. Until
G1–G9 are complete, verification is limited to engineering correctness:

- Rust compile, unit, migration, and existing regression tests;
- Node protocol/config/credential-store tests using a fake model provider;
- schema rejection, cancellation, timeout, child-crash, snapshot-race, and
  no-partial-write tests;
- deterministic golden evidence packs and freshness transitions;
- no paid or plan-backed model calls in the default test suite.

These tests establish that the machinery obeys its contracts; they do not
claim that semantic memory helps agents. After semantic v1 is complete, run
real end-to-end scouting with Sol or Terra on the installed n8n and Twenty
repositories, inspect the generated memory manually, repair implementation
defects, and only then compare real agent work with and without it.

## Semantic v1 completion boundary

Semantic v1 is complete when:

- one supported installation can call both a ChatGPT-plan model and an API-key
  model through the same gateway protocol;
- workflows, selected symbol cards, hierarchy, and concepts all use the common
  run/evidence/freshness engine;
- generated claims are candidate- or support-closed, never silently published
  from malformed/partial output;
- stale artifacts are visibly stale and can be refreshed into immutable
  successors;
- repository overview and semantic-specific queries drill down to exact source;
- the gateway is installable with jscout, diagnosable, cancellable, and does
  not leak credentials or prompt contents to normal logs;
- existing deterministic indexing and retrieval still work with Node absent.

## Explicitly out of scope for semantic v1

- replacing Rust indexing or deterministic extraction with an LLM;
- LLM-generated pseudocode as source truth;
- one model call per chunk;
- hidden background spending during index/watch;
- autonomous tool-using agents inside the gateway;
- direct provider SDK calls outside the gateway;
- automatic model fallback that changes provider or billing path;
- making stale semantic memory part of default structural search;
- migrating embeddings through pi-ai;
- Yarn PnP dependency indexing or tsserver/LSP enrichment.

## Upstream references

- [@earendil-works/pi-ai package documentation](https://www.npmjs.com/package/%40earendil-works/pi-ai)
- [earendil-works/pi repository](https://github.com/earendil-works/pi)
- [Deprecated @mariozechner/pi-ai package](https://www.npmjs.com/package/%40mariozechner/pi-ai)
