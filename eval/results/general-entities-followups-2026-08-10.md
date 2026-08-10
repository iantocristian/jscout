# General-entity follow-up validation — 2026-08-10

## Scope

This follow-up addresses every reproduced finding from the merged
general-entities review:

- the decorator-only 512-byte forward lookup no longer applies to router or
  GraphQL API calls; an inline or unresolved handler keeps its entity
  occurrence without receiving a fabricated handler edge;
- router holders are case-insensitive and accept names ending in `Router`, so
  calls such as `usersRouter.get(...)` retain their named handler;
- qualified database holders such as `this.db` and `ctx.db` can derive their
  resource from the first argument, while `userRepository`, `userRepo`, and
  `UserModel` derive `user`; bare generic repository holders remain rejected;
- Apollo-style GraphQL options objects are not interpreted as key-per-operation
  objects;
- `config.get(...)` and `configService.get(...)` produce `config_key` /
  `reads_config` facts rather than environment variables;
- `getRepository(User)` and `getModel(User)` use `database_acquire` /
  `acquires_resource` rather than claiming a read.

Extraction version 3 forces one deterministic reparse after these recognizer
changes. Projection version 9 invalidates graph snapshots created with the old
handler fallback or relationship labels.

## Local automated validation

Validation was executed locally rather than delegated to CI:

```text
cargo fmt --all -- --check: passed
cargo test --all-targets --all-features: 95 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

Focused fixtures cover the exact inline-arrow/next-declaration failure, named
routers, qualified database receivers, repository/model suffixes, Apollo
options objects, config-versus-environment identity, and acquisition edges.

## Fresh Twenty index

The release binary indexed the installed 22,735-file Twenty checkout into an
isolated database with zero extraction failures:

```text
75,095 chunks
298,700 refs
18.60 s total
7.34 s structural projection
```

Corrected entity counts are:

| Family | Original | Follow-up | Occurrences |
|---|---:|---:|---:|
| Routes | 47 | 47 | 141 |
| GraphQL operations | 537 | 531 | 789 |
| Environment variables | 110 | 110 | 505 |
| Configuration keys | not separate | 211 | 503 |
| Database resources | 7 | 122 | 1,318 |
| External hosts | 9 | 9 | 10 |
| Feature flags | 6 | 6 | 11 |

The GraphQL reduction removes six option-key identities; no `query:query`,
`query:variables`, or `query:fetchPolicy` entity remains. A direct graph audit
also finds zero `handles_route` edges targeting a file node. The database
increase is the intended recovery of qualified NestJS/TypeORM-style holders,
not a change to confidence: these API-pattern facts remain `likely`.
