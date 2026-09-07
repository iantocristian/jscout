---
name: jscout
description: "Use the jscout repository index to search code before grep or rg whenever you fix a bug, implement a change, or answer a question in this JavaScript or TypeScript project: investigate known identifiers completely and answer cross-file questions with source-backed evidence."
---

# jscout core

MCP tools. Localize, then verify in source. Responses carry `snapshot`
(surface key) and `publication_snapshot`.

## Tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_search` | query or path | `path`, `path_prefix`, `exhaustive`, `match_mode`, `allow_broad`, `cursor`, `limit`, `origins`, `formats`, `file_roles`, `vector`, `rerank`, `response_bytes` | code |
| `definition` | `anchor`+`snapshot` or `symbol` | `origins`, `formats`, `source_bytes` | source |
| `who_uses` | `anchor`+`snapshot` or `symbol` | `origins`, `formats` | callers |
| `calls` | `method` | `receiver`, `args`, `arg_position` | member calls |
| `file_outline` | `path` | `origins` | symbols |
| `events` | — | `name` | emit/listen |
| `documentation_search` | query or path | `path`, `path_prefix`, `vector`, `require_vector`, `limit` | Markdown/MDX when asked |

## Flow 1: known identifier

1. `semantic_search` with `exhaustive: true`. Default `match_mode: "all"`
   requires all tokens in one chunk; `"any"` is explicit OR. A `broad_or_query`
   with `confirmation_required: true` delivers counts, no hits/cursor:
   refine, or retry with `allow_broad: true` if that OR set is intended.
   Abandon off-target matches.
2. For a valid traversal, copy `next_cursor` unchanged into `cursor` until
   `truncated: false`; page-local `returned` must sum to
   `total_chunks`. On `response_budget_too_small ... minimum_bytes=N`, retry
   the same page with `response_bytes: N`.
3. `definition`: returned `sym:` anchor and `snapshot`, copied verbatim.

Scope transfer: preserve query, match mode, paths, roles, origins, formats.
Carry explicit origins/formats into `definition` and `who_uses`; keep omitted
ones omitted, never infer from the echoed `scope`. With a cursor,
`allow_broad` need not repeat.
`source_meta.partial` flags incomplete cached source.

## Flow 2: fuzzy query

One ranked `semantic_search` (`vector: true`), then Flow 1 on its identifiers.

## Path lookup

Both searches take literal repo-relative `path` or directory `path_prefix`;
filters intersect before limits. `src/app/` excludes `src/apple`.
Omit query for bounded indexed chunks without inference:
`documentation_search({path: "README.md"})`.

## Tips

- `limit` is not a session ceiling; done means `truncated: false`.
- Never page merely because `next_cursor` exists.
- Do not re-search or expand after localization.
- A pattern is a convention, not proof.
- If `snapshot` changes mid-task, restart that surface's traversal.
- Docs prose is never runtime proof.

## On instruction

- Fix/feature: Flow 1 on named code; docs only when told to.
- Blast radius: `who_uses`, then `calls`.
- Docs question: `documentation_search`, then verify in code.
