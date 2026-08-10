# General deterministic entity validation — 2026-08-10

> Post-merge recognizer findings and corrected large-repository counts are
> recorded in
> [`general-entities-followups-2026-08-10.md`](general-entities-followups-2026-08-10.md).

## Scope

This slice adds six deterministic entity families to the structural graph:

- HTTP routes from router calls and controller/method decorators;
- GraphQL queries, mutations, and subscriptions from handler decorators and
  statically named client operations;
- environment-variable reads from `process.env`, computed access, and bounded
  environment APIs;
- database resources from Prisma/repository/query-builder API shapes;
- feature-flag checks from bounded flag APIs;
- external-service hosts from static HTTP URLs.

Every occurrence retains its exact span, extractor, provenance, confidence,
and detail payload. The projection uses separate relationship names:

```text
handles_route
handles_graphql
invokes_graphql
reads_env
reads_resource
writes_resource
checks_flag
calls_host
```

Route and GraphQL entities point to handlers. Code points to the configuration,
data, flag, operation, and host entities it uses. All API-pattern matches remain
`likely`; no general-entity recognizer claims runtime certainty.

Generic database API holders such as `repository`, `repo`, `model`, and
`entityManager` are rejected as resource identities. Concrete shapes such as
`prisma.user`, `db.insert(users)`, and `getRepository(User)` remain eligible.

## Freshness and automated validation

Schema v11 forces existing v10 files through extraction once. In addition, an
explicit `extraction_version` anchor now invalidates unchanged file hashes when
deterministic extractor semantics change without an SQL schema change. A test
changes that anchor and proves the next index reparses the unchanged file and
rebuilds its entity occurrence.

```text
cargo test: 84 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

The structural fixture covers both graph directions and verifies exact handler
attachment for a controller route. The extraction fixture covers each entity
family and rejects a generic repository-holder name.

## Fresh large-repository indexes

Both runs used isolated databases under `/tmp`, excluded dependency internals,
and completed with zero extraction failures.

| Repository | Revision | Files | General entities | Occurrences | Entity projection | Total |
|---|---:|---:|---:|---:|---:|---:|
| Twenty | installed 2026-08-10 checkout | 22,735 | 716 | 1,555 | 7.55 s | 41.95 s |
| n8n | `9d9e9bf97e` | 19,198 | 906 | 2,557 | 8.32 s | 54.42 s |

Twenty produced 537 GraphQL-operation identities, 110 environment variables,
47 routes, 9 external hosts, 7 database resources, and 6 feature flags. n8n
produced 485 routes, 354 environment variables, 41 database resources, 13
external hosts, 12 feature flags, and one statically named GraphQL operation.

Observed total time is 2.89 seconds above the contract-only Twenty run and 4.25
seconds above the contract-only n8n run. These are implementation-scale checks,
not controlled performance benchmarks; filesystem cache and host load were not
held constant.

## Decision

The requested general deterministic families are represented and projected.
They do not trigger workflow generation yet. Agent-facing entity lookup,
bounded paths, and the deterministic repository overview remain the final
surface layer before a held-out workflow-candidate rerun.
