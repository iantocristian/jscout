# Twenty workflow candidate gate — 2026-08-09

## Decision

**Fail Stage A and do not run semantic classification.** The registered frozen
index produced 62.5% micro candidate recall, two pairs fell below the 60% floor,
and all six pairs truncated. A deterministic resolver repair raised recall to
87.5%, but the rerun still missed the 90% micro threshold and all six candidate
sets still truncated. No Terra or Sol calls were made.

This blocks `record_workflow` and the candidate-classification treatment. It
does not reverse the passing semantic-memory retrieval result: retrieval helps
when the stored artifact is good, while broad workflow discovery remains
unproven.

## Registered run

- Pre-registration: `eval/prereg/candidate-closed-scouting-2026-08-09.md`.
- Repository: Twenty at `02a187d065354872c0f318b0723a1e7d8762ae00`.
- Source snapshot:
  `5fb20574f2b8c69d57aa0fd87d980abff911a164723ffc762bab75be24e149b2`.
- Six admitted workflow pairs; seeds resolved mechanically from session-1 gold
  production symbols only.
- Fixed traversal: both directions, depth 2, minimum `likely`, production
  candidates only, maximum 31 candidates, existing ranking and traversal
  budgets.

| Workflow | Candidates | Recall | Traversal truncated | Candidate truncated |
|---|---:|---:|---:|---:|
| Fireflies synchronization | 31 | 2/4 | yes | yes |
| PDL post-install seeding | 31 | 2/3 | yes | yes |
| Slack assistant queue | 31 | 2/4 | yes | yes |
| Recall callback lifecycle | 31 | 3/5 | yes | yes |
| Document generation | 31 | 3/5 | yes | yes |
| Self-hosting identity linkage | 31 | 3/3 | yes | yes |
| **Total** | 186 | **15/24 (62.5%)** | **6/6** | **6/6** |

The first failure exposed a structural correctness defect rather than a prompt
or weight problem. jscout configured automatic tsconfig discovery but invoked
the resolver's directory API, for which automatic discovery is explicitly
disabled. Twenty's package-local `src/*` aliases therefore became the external
package node `pkg:src`. That node had 76,408 incident projected edges, creating
thousands of false two-hop neighbors.

## Deterministic repair diagnostic

The resolver now resolves from the importing file, enabling nearest-tsconfig
discovery. A regression test covers a package-local `src/*` alias. A fresh
index over the same source snapshot resolved 26,419 `src/*` requests in-repo;
only five remained unresolved, and `pkg:src` fell to 12 incident edges.

This repair did not change candidate limits, graph weights, edge kinds, seeds,
tasks, or source. The fresh-index rerun was diagnostic because the defect was
found after observing the registered result.

| Workflow | Candidates | Recall | Traversal truncated | Candidate truncated |
|---|---:|---:|---:|---:|
| Fireflies synchronization | 31 | 4/4 | yes | yes |
| PDL post-install seeding | 31 | 3/3 | yes | yes |
| Slack assistant queue | 31 | 3/4 | yes | yes |
| Recall callback lifecycle | 31 | 3/5 | yes | yes |
| Document generation | 31 | 5/5 | yes | yes |
| Self-hosting identity linkage | 31 | 3/3 | yes | yes |
| **Total** | 186 | **21/24 (87.5%)** | **6/6** | **6/6** |

Remaining misses:

- Slack: `slackAssistantWorkerHandler`, reached through a record-created
  trigger rather than a direct call.
- Recall: `processRecallWebhookHandler` and `handleRecallWebhook`, reached
  through a returned universal function identifier and then a registered
  handler.

The top 31 candidates were mostly real local helpers, constants, and sibling
operations. Increasing the cap or retuning weights would not model the missing
handoffs and would weaken the bounded-classification premise.

## Architectural consequence

Generic bidirectional symbol neighborhoods are not sufficient workflow
candidate generators. Before another semantic-classification run, deterministic
structure needs explicit runtime-boundary entities and edges for at least:

1. registry/dispatch identities that connect a returned or passed identifier
   to the handler registered under that identity;
2. data-lifecycle events that connect record creation/update operations to
   registered database-event handlers.

Candidate issuance should traverse those boundary edges and collapse ordinary
helper subgraphs instead of classifying every depth-2 symbol. The current six
pairs become a regression set, not a clean confirmatory set for that redesign.
A new Stage A must be pre-registered on held-out workflows before any LLM calls.

## Result records

- Registered report SHA-256:
  `a8b85d1f9353fc941e9762eb6091d1983c55b702aacd15a53a4f43a05abc33c1`.
- Registered database SHA-256:
  `e17f0281f51d1363c356ffc37b2d7475ce3edabe148ff3d410eb030265e5685b`.
- Resolver-repair report SHA-256:
  `a3a2f689d180b0265a846fb8b1dee6d3c2e4706a477569326bfbf8e2d3747050`.
- Resolver-repair database SHA-256:
  `02cd6d79e551f36de2d980df63da3c91329cb77193885d42963a70008afb020f`.

Raw reports and databases remain outside the repository.
