# Workspace cross-package edge resolution — 2026-08-10

## Summary

Module resolution misclassified monorepo-internal cross-package imports as
external packages, so the cross-package module graph was effectively absent
on workspace monorepos. Two independent defects, both fixed:

1. **No workspace mapping.** Bare specifiers like `n8n-workflow` cannot
   resolve without an installed `node_modules` (and resolve into untracked
   `dist/` or symlink paths with one), so they fell back to external
   `package` classification. Fixed by `src/workspace.rs`: workspace globs
   (`pnpm-workspace.yaml` / package.json `workspaces`) are expanded to a
   package-name → source alias table fed into the resolver
   (entry file, `src/` for subpaths, per-subpath `exports` translation,
   `name/dist/*` mirror for build-output imports).
2. **Unloadable tsconfig poisoned all resolution.** n8n's per-package
   tsconfigs `extends: "@n8n/typescript-config/…"` — a bare workspace
   specifier that is unresolvable without node_modules. With
   `TsconfigDiscovery::Auto` that failed *every* resolution under those
   tsconfigs, including relative imports (30,794 of 31,220 n8n relative
   imports unresolved). Fixed by retrying failed resolutions with an
   identical resolver minus tsconfig discovery.

## Method

`jscout index` on unmodified checkouts, neither with `node_modules`
installed. Before = binary at `00b89e4`; after = this change. The
workspace-named external count is edges whose `package` equals one of the
repo's own workspace package names (77 for n8n, 18 for Twenty) — each such
edge is a monorepo-internal import misclassified as external.

## Edge counts

| Repo | Files | Edges | Internal before | Internal after | Workspace-named external before | after |
|---|---:|---:|---:|---:|---:|---:|
| n8n (pnpm workspaces) | 19,198 | 64,555 | 426 | 41,560 | 11,515 | 10 |
| Twenty (yarn workspaces) | 22,735 | 97,661 | 67,426 | 79,355 | 11,931 | 2 |

The 12 residual workspace-named external edges are legitimately
unresolvable to indexed files: `.vue` components, `.json` imports
(`@n8n/i18n/locales/en.json`, `…/package.json`), dist-only lint/config
packages with no tracked source, and one absent subpath
(`twenty-sdk/front-component-renderer/build`).

## Resolution provenance

Workspace mappings are heuristics of varying strength, so every module edge
records how it was resolved (`module_edges.resolution`) and the structural
projector downgrades heuristic mappings from `certain` to `likely` —
including references that reach their target across such an edge:

| Repo | `resolver` (direct) | `workspace` (manifest-backed) | `workspace-inferred` (heuristic) | external |
|---|---:|---:|---:|---:|
| n8n | 30,055 | 5,293 | 6,212 | 22,995 |
| Twenty | 67,426 | 0 | 11,929 | 18,306 |

`workspace` means the package.json named the source file itself (e.g. n8n's
`module: src/index.ts` fields); `workspace-inferred` covers source
conventions, dist-mirroring, and unique-name search. Twenty's manifests only
ever point at dist, so all its cross-package mappings are inferred.

Twenty's baseline internal count was already high because its relative
imports resolved; its cross-package edges were still absent. n8n's baseline
was pathological (both defects compounding): even relative imports failed.

## Implication for recorded evals

The n8n structural-arm evals recorded before this date ran against a graph
missing ~41k of 41.6k resolvable internal edges (cross-package *and* most
relative edges). Cross-package blast-radius is where the structural arm
should help, so those n8n numbers underestimate it; re-run before citing.
