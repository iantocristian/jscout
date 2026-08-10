# Contract-plane follow-up validation — 2026-08-10

## Scope

This follow-up addresses the confirmed findings from the merged contract-plane
review:

- module requests that exist only in `contract_imports` or `contract_exports`
  carry `module_edges.type_only=1` and project as `imports_types` or
  `imports_package_types`, never runtime `import` edges;
- function, arrow, method, class, interface, type-alias, nested function-type,
  constructor-type, and mapped-type parameters are excluded from contract
  references while referenced constraints and concrete types remain;
- failed relative contract resolution uses an `unresolved:` key without a
  package identity instead of an `external:` key;
- runtime-only `ModuleGraph` consumers no longer load contract exports;
- the 512-byte decorator-to-declaration fallback and full watch-cycle
  projection cost are documented explicitly.

Schema v12 adds the derived `type_only` classification and invalidates stale
snapshots. Extraction version 2 forces one reparse so indexes created before
the generic-scope fix cannot look current while retaining false entities.

## Automated validation

```text
cargo test --all-targets --all-features: 91 passed
cargo clippy --all-targets --all-features -- -D warnings: passed
```

The integration fixture asserts that `import type` through a type-only barrel
resolves to its canonical contract while its file relationship is
`imports_types`/`type-resolver`. It also asserts that no runtime `import` edge
is emitted and that an unresolved relative barrel is not given an external
package identity. A separate extraction fixture covers generic scopes, and a
v11 migration fixture locks in snapshot invalidation and the safe legacy
default.

## Fresh Twenty index

The follow-up release binary indexed the installed 22,735-file Twenty checkout
into an isolated database with zero failures:

```text
75,095 chunks
298,700 refs
19.19 s total
7.56 s structural projection
```

The previous contract-plane run produced 15,949 contract entities: 13,819
backed and 2,130 unbacked. With scoped generic filtering, the same checkout
produces 15,041: the backed set remains 13,819 and the unbacked set falls to
1,222. The 908 removed entities were generic parameters that had no repository
definition.

Module classification on that index is:

| Classification | Canonical edges | Projected kind | Projected edges |
|---|---:|---|---:|
| Runtime or mixed runtime/type | 97,661 | `import` | 97,661 |
| Type-only | 19,185 | `imports_types` | 19,185 |

A direct join over canonical and projected edges finds zero type-only requests
projected as `import` or `imports_package`.
