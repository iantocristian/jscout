# Runtime-boundary entity validation — 2026-08-10

## Scope

This implementation adds snapshot-canonical runtime entities without turning
heuristic framework matches into ordinary calls. Canonical identity is separate
from source occurrences; every occurrence retains its role, byte/line span,
extractor, provenance, confidence, and detail payload.

Initial families:

- registry identifier: dispatch site to registered handler;
- data lifecycle: create/update/delete producer to lifecycle listener;
- job/queue/cron: producer to worker/handler;
- DI token: injection site to provider implementation.

The disposable structural projection exposes these as confidence-labelled
edges. A direct caller of a lifecycle/job producer may receive a synthetic
`*_via` edge with `entity-boundary-collapse` provenance. This collapses one
ordinary helper hop so a depth-two workflow query can cross the runtime
boundary; it does not claim that the caller performs the underlying write.

Runtime entity hubs have no backing file and remain traversable across corpus
boundaries. Their occurrence and symbol endpoints retain origin gating, so a
cross-boundary identity does not expose dependency internals unless dependency
origin is requested.

## Automated validation

```text
cargo test: 78 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

Fixtures cover:

- one imported registry identifier shared by dispatch and registration files;
- GraphQL record creation paired with a database-event listener;
- a caller one helper above the record write reaching the listener in two hops;
- Twenty-style `this.messageQueueService.add(Job.name, ...)` and
  `addCron({ jobName: Job.name, ... })` producer/handler matching;
- DI token injection/provider matching;
- repeated entity occurrences retaining separate evidence without duplicate
  traversal relationships;
- schema-v8 migration forcing entity re-extraction and snapshot invalidation.

## Fresh Twenty index

Repository: `/Users/cristian/git/twenty` at the installed 2026-08-10 checkout.
Database: isolated under `/tmp`; dependency internals excluded.

```text
22,735 files
75,095 chunks
298,700 references
0 extraction failures
10.83 s total indexing (warm filesystem-cache validation run)
20.82 ms entity materialization/projection
```

The first-pass registry/lifecycle index produced 102 registry entities and 47
data-lifecycle entities from 101 registered handlers, 15 dispatch sites, 21
lifecycle listeners, and 137 lifecycle producers.

After adding the Twenty/NestJS queue shapes, the same index produced 97 job
entities from 140 producer and 94 handler occurrences; 88 job identities had
both sides. It also produced 30 DI-token entities from 25 injection and 24
provider occurrences. The resolved projection contained no duplicate
`(source, target, kind)` runtime-entity relationships. When a decorator or
registration site has no explicit target expression, its owner fallback is now
labelled `registration-site-fallback` (or `provider-site-fallback`) instead of
reusing the framework-pattern provenance.

Template/static-string resolution remains a file-global, visit-order
approximation rather than scope-aware constant evaluation. These identities
remain `likely`, and the limitation is explicit in the extractor.

The two misses that blocked the frozen Twenty workflow-candidate gate are now
explicit paths:

```text
recallWebhookRouteHandler
  --dispatches (likely, framework-field)-->
PROCESS_RECALL_WEBHOOK_LOGIC_FUNCTION_UNIVERSAL_IDENTIFIER
  --registered_handler (likely, framework-pattern)-->
processRecallWebhookHandler

enqueueSlackAssistantRequestRecord
  --produces_lifecycle_via (likely, entity-boundary-collapse)-->
slackAssistantRequest.created
  --lifecycle_listener (likely, framework-pattern)-->
slackAssistantWorkerHandler
```

The canonical occurrence rows point to the actual registration, dispatch,
mutation, and trigger spans. No LLM calls were used.

## Decision

The deterministic representation defect identified by the failed gate is
repaired for its two known mechanisms. Do not rerun workflow candidate
generation yet: the contract plane, remaining deterministic entities, bounded
paths, and repository overview are deliberately sequenced after this runtime
slice. A future candidate-gate rerun still requires a new pre-registration and
held-out workflows; these two cases are regression fixtures, not confirmatory
evidence.
