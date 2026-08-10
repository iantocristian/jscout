# Workflow logical-routing follow-up validation — 2026-08-10

## Scope

This follow-up addresses every finding from the merged workflow-routing
review:

- the general-entity and direct-code hub threshold is one named
  `WORKFLOW_HUB_DEGREE_LIMIT` rather than two unrelated literals;
- the load-bearing general-entity rule has fixtures on both sides of the
  threshold: five readers retain four peer clues, while fourteen readers of
  one environment variable retain none;
- high-degree DI traversal is directional. A provider does not fan out through
  a common token to every injection site, while an injection site can still
  resolve the concrete provider behind that token;
- eligible direct-code degrees and entity graph degrees are memoized for one
  workflow walk instead of being recomputed every time a neighbor is seen;
- the `(node, depth)` expansion identity is documented as intentional:
  stronger runtime-crossing rediscovery at another depth may propagate its
  score, while weaker repeats at the same depth remain suppressed; and
- workflow-candidate help, README text, and a regression fixture explicitly
  reject file seeds rather than choosing one of several operations in a file.

The DI rule targets the problematic direction rather than discarding DI
resolution entirely. For a token with one provider and fifteen injection
sites, provider-seeded traversal returns only the provider. Consumer-seeded
traversal returns that consumer and the concrete provider, without the other
fourteen consumers.

## Local automated validation

Validation was executed locally rather than delegated to CI:

```text
cargo fmt --all -- --check: passed
cargo test --all-targets --all-features: 101 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
npm test: 37 passed
```

Focused fixtures cover low- and high-degree general entities, high-degree DI
in both traversal directions, high-degree direct code, and the file-seed error
contract. The generated CLI help also states that workflow seeds must be
symbols and that file anchors are rejected.

## Fresh Twenty regression check

The release binary indexed the installed 22,735-file Twenty checkout into an
isolated database with zero extraction failures:

```text
75,095 chunks
298,700 refs
14.54 s total
5.81 s structural projection
```

The unchanged six-pair known-workflow gate still reports:

```text
matched: 24/24
micro recall: 100%
traversal truncation: 0/6
candidate truncation: 0/6
decision: pass
```

Raw report SHA-256:
`593b1b479de89649a8b78f08c24b8ac564c630ab97ee220e1ee7fbbe01b694a2`.

This remains a regression check tuned on known workflows. It does not create a
held-out recall claim or unblock semantic classification without the separate
pre-registered Stage A run.
