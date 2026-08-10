# Deterministic agent-surface validation — 2026-08-10

> Post-merge availability, query-shape, traversal-direction, and dependency
> area fixes are recorded in
> [`agent-surfaces-followups-2026-08-10.md`](agent-surfaces-followups-2026-08-10.md).

## Scope

This slice exposes the deterministic graph through three structural-profile MCP
tools:

- `repository_overview`: a bounded cold-start map with repository totals,
  origin and file-role counts, monorepo areas, entity inventory, and top graph
  relationships;
- `entities`: canonical entity lookup with plane/type/role/file-role/origin
  filters and exact occurrence spans, provenance, confidence, and detail;
- `paths`: ranked, bounded simple paths between two current anchors using the
  same confidence, relation, hub, distance, file-role, and origin policy as
  neighborhood traversal.

All three responses carry the current structural snapshot and use the shared
whole-response byte-budget envelope. They are absent from the baseline MCP
profile. The server instructions recommend one overview at cold start, entity
lookup for deterministic boundaries, and paths for cross-boundary drill-down.

## Automated validation

```text
cargo test: 87 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

Fixtures verify entity evidence filtering, bounded overview aggregation,
ranked path composition, baseline-profile exclusion, structural-profile tool
calls, and final rendered-byte limits.

## Twenty smoke check

Repository: `/Users/cristian/git/twenty` at
`02a187d065354872c0f318b0723a1e7d8762ae00`. Database: the fresh general-entity
index created earlier on 2026-08-10.

The overview rendered 5,597 bytes under a 12,000-byte budget and reported:

```text
22,735 files
75,095 chunks
76,555 symbols
80,086 entity occurrences
547,350 graph edges
```

Its bounded areas included `twenty-front`, `twenty-server`, and `twenty-apps`
rather than enumerating individual files. An entity lookup for
`slackAssistantRequest.created` rendered 2,543 bytes under an 8,000-byte
budget and returned the production producer/listener occurrences with exact
source spans and extractor provenance.

## Known-workflow regression rerun

Only after all deterministic entity and agent-surface layers were present, the
existing six-pair Twenty candidate gate was rerun unchanged:

```text
matched: 22/24 (91.7% micro recall)
every pair recall >= 60%: yes
no truncation: no
decision: fail
```

This is a regression set observed during the prior design round, not held-out
confirmatory evidence. The runtime representation repaired the original
handoffs: the Recall registered handler is now issued, and both missing
mechanisms are visible as explicit graph paths. The remaining misses are one
ordinary call beyond those handoffs:

```text
enqueueSlackAssistantRequest
  -> enqueueSlackAssistantRequestRecord
  -> slackAssistantRequest.created
  -> slackAssistantWorkerHandler

recallWebhookRouteHandler
  -> registry identifier
  -> processRecallWebhookHandler
  -> handleRecallWebhook
```

The old generic depth-two neighborhood counts each entity-hub edge as a graph
hop. It also admits documentary and general-entity paths that should not be in
the default workflow candidate plane. Raising generic depth or the 31-symbol
cap would amplify noise and would not satisfy the registered no-truncation
condition.

## Decision

The agent-facing deterministic surfaces are implemented. Workflow candidate
generation remains blocked. Its next revision must use workflow-specific
logical traversal: collapse a two-edge runtime entity handoff into one logical
transition, allow bounded continuation on the other side, and exclude contract
and general-entity edges unless a workflow query explicitly asks for them.

Any run on these six pairs after that change remains a regression check. A new
claim still requires a pre-registration and held-out workflows before semantic
classification.
