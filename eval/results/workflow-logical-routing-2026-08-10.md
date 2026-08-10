# Workflow logical-routing regression — 2026-08-10

## Scope

The generic structural neighborhood has been replaced inside
`workflow_candidates` with a workflow-specific logical walk:

- `call`, `render`, and `extend` remain direct one-hop code transitions;
- complementary registry, lifecycle, job, and DI edges on the same runtime
  entity collapse into one logical producer-to-handler transition;
- runtime handoffs rank ahead of ordinary helper fan-out and may continue into
  code on the other side;
- low-degree GraphQL, database-resource, environment, feature-flag, and
  external-host entities may associate code, but those associations are
  terminal clues rather than traversal bridges;
- general entities with more than 12 graph incidents are rejected as
  repository-wide hubs;
- a code symbol with more than 12 eligible production code neighbors remains a
  candidate but is terminal, preventing generic helpers from fanning out to
  every caller;
- contract/documentary edges do not participate in workflow traversal.

The existing minimum confidence, production-file policy, repository/workspace
origin policy, logical depth, 31-symbol candidate cap, and node/edge budgets
remain unchanged.

The thresholds and ranking were implemented after inspecting the already-known
regression failures. This is implementation tuning on a regression set, not a
pre-registered evaluation.

## Automated validation

```text
cargo test: 88 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

Fixtures cover:

- registry handoff followed by an ordinary call within logical depth two;
- ordinary call followed by a lifecycle handoff within logical depth two;
- shared contracts not creating workflow candidates;
- low-degree general-entity association;
- high-degree code helpers remaining visible but not bridging to every caller.

## Known Twenty regression

Repository: `/Users/cristian/git/twenty` at
`02a187d065354872c0f318b0723a1e7d8762ae00`. Database: the fresh 2026-08-10
index containing the runtime, contract, and general entity planes.

The six-pair candidate gate was rerun without changing its task set, seeds,
depth, minimum confidence, candidate cap, or pass thresholds.

| Workflow | Candidates | Recall | Traversal truncated | Candidate truncated |
|---|---:|---:|---:|---:|
| Fireflies synchronization | 28 | 4/4 | no | no |
| PDL post-install seeding | 9 | 3/3 | no | no |
| Slack assistant queue | 30 | 4/4 | no | no |
| Recall callback lifecycle | 21 | 5/5 | no | no |
| Document generation | 26 | 5/5 | no | no |
| Self-hosting identity linkage | 3 | 3/3 | no | no |
| **Total** | **117** | **24/24 (100%)** | **0/6** | **0/6** |

The gate decision is `pass`: micro recall is 100%, every pair exceeds the 60%
floor, and no traversal or candidate set is truncated. No LLM calls were made.

Raw report SHA-256:
`7530164348cb29c895d7f9a84948d7153301f5e4b219999955b726483630f7c9`.
The raw report and database remain outside the repository.

## Decision

The known representation and delivery failures are regression-covered. This
does not establish workflow-candidate recall on unseen code. The next valid
step is a new pre-registration with held-out workflows, followed by Stage A
candidate recall. Semantic classification remains blocked until that Stage A
passes.
