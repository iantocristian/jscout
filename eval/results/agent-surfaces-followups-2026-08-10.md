# Agent-surface follow-up validation — 2026-08-10

## Scope

This follow-up addresses every finding from the merged agent-surfaces review:

- path search rejects graph scopes above 200 nodes or 800 edges, the MCP
  schema advertises the same maxima, and the handler clamps bypassed-schema
  requests before calling the structural API;
- path enumeration stops after 50,000 popped prefix states, exposes the actual
  `searched_states` work counter, and reports `truncated: true` when the cap
  binds;
- every path step exposes `reversed`, so a consumer does not have to infer
  target-to-source traversal by comparing the step and canonical edge;
- entity filters, occurrence counts, total-match count, ranking, and entity
  limit are applied in one SQL aggregate. Occurrences are hydrated only for
  the selected entities and are themselves filtered and limited in SQL;
- the repository-overview response-budget drop order is documented at the
  call site; and
- dependency overview areas preserve the complete package-instance prefix,
  including scoped names such as `dependency:@scope/pkg@version#hash`.

The state cap is independent of accepted-path count. This closes the dense
dead-end case where a small graph could enumerate an unbounded number of
simple path prefixes without ever reaching the target.

## Local automated validation

Validation was executed locally rather than delegated to CI:

```text
cargo fmt --all -- --check: passed
cargo test: 99 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
npm test: 37 passed
```

The dense-path fixture builds four fully connected 15-node layers: 60
intermediate nodes and 690 directed edges. Its target is disconnected, so no
accepted-path limit can stop enumeration. The result stops at exactly 50,000
searched states, returns no path, and reports truncation. Separate fixtures
cover reverse traversal, schema maxima, handler clamping, SQL total-match
counting after filters, and unscoped/scoped dependency areas.

## Fresh Twenty smoke check

The release binary indexed the installed 22,735-file Twenty checkout into an
isolated database with zero extraction failures:

```text
75,095 chunks
298,700 refs
14.37 s total
5.68 s structural projection
```

A default structural-profile `entities` call matched 14,009 entities. It ran
the aggregate plus occurrence hydration for only the 20 selected entities and
completed in 158 ms according to MCP telemetry. This is a smoke measurement,
not a comparative latency claim; the locked-in change is removal of the
pre-limit per-entity query count.
