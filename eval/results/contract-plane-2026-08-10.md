# Contract-plane validation — 2026-08-10

## Scope

This slice adds a documentary plane that is structurally separate from runtime
control-flow edges. Canonical anchors use the `contract:` namespace and retain
source occurrences, exact spans, provenance, and confidence.

Deterministic extraction covers:

- interfaces, type aliases, and enums;
- parameter and return types on exported functions, exported arrows, and
  public methods of exported classes;
- decorators;
- DTO and validation-schema declarations;
- referenced contract names inside declarations and exported APIs.

Type-only imports, exports, barrels, and workspace resolution have separate
storage from runtime imports and exports. They cannot create `call`, `use`, or
other runtime edges. Exact TypeScript and decorator syntax is labelled
`certain` only on documentary edge kinds. DTO/schema recognizers remain
`likely` and name their convention in provenance.

The structural edge kinds are:

```text
declares_contract
accepts_contract
returns_contract
references_contract
decorated_by
```

Built-in generic wrappers such as `Promise`, `Array`, `Pick`, and `Omit` are
not materialized as repository contracts.

## Automated validation

```text
cargo test: 80 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

The integration fixture proves a type-only re-export barrel resolves an
exported function's `User` parameter and `UserResult` return to the defining
interface and type-alias anchors. The same fixture asserts that no runtime
reference edge is created from those type bindings and that canonical contract
nodes carry their defining file for origin/role filtering.

Schema v10 widens the evidence plane and forces one safe re-extraction from v9.
The migration discards only derived entity/projection rows.

## Fresh large-repository indexes

Both runs used isolated databases under `/tmp`, excluded dependency internals,
and completed with zero extraction failures.

| Repository | Revision | Files | Chunks | Refs | Contract entities | Occurrences | Entity projection | Total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Twenty | installed 2026-08-10 checkout | 22,735 | 75,095 | 298,700 | 15,949 | 78,179 | 7.22 s | 39.06 s |
| n8n | `9d9e9bf97e` | 19,198 | 92,215 | 404,999 | 15,357 | 71,057 | 7.00 s | 50.17 s |

On Twenty, 13,819 contract nodes resolve to a defining file and 2,130 remain
unbacked because they are unresolved or external. The backed set includes
1,081 generated-file nodes, so existing file-role filters can exclude them
before traversal budgets. n8n has 13,228 backed and 2,129 unbacked nodes.

The indexing cost is material. Relative to the runtime-boundary-only Twenty
run recorded on the same checkout, total time increases from 31.28 seconds to
39.06 seconds, about 25%. Most of the added cost is inserting roughly 78k
occurrences and their documentary edges, not module lookup. This is accepted
for the first contract slice but should be included in future scale work; it is
not evidence that default expansion should grow broader.

## Decision

The contract representation is ready for review as a separate plane. It does
not trigger a workflow-candidate rerun. General deterministic entities,
agent-facing bounded paths, and the deterministic repository overview still
land before a held-out workflow rerun.
