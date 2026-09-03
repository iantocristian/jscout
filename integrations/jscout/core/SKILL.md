---
name: jscout
description: Use the jscout repository index to search code before grep or rg whenever you fix a bug, implement a change, or answer a question in this JavaScript or TypeScript project: investigate known identifiers completely and answer cross-file questions with source-backed evidence.
---

# jscout core

Tools here are MCP tools on the jscout server, not shell commands.
Localize first, then verify in source. Responses carry `snapshot` (this
surface's key) and `publication_snapshot`.

## Tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_search` | `query` | `exhaustive`, `cursor`, `limit`, `origins`, `formats`, `file_roles`, `vector`, `rerank`, `response_bytes` | every occurrence (`exhaustive: true`) or a ranked look |
| `definition` | `anchor`+`snapshot` or `symbol` | `origins`, `formats`, `source_bytes` | one symbol's source |
| `who_uses` | `anchor`+`snapshot` or `symbol` | `origins`, `formats` | callers |
| `calls` | `method` | `receiver`, `args`, `arg_position` | member-call sites |
| `file_outline` | `path` | `origins` | one file's symbols |
| `events` | — | `name` | emit/listen wiring |
| `documentation_search` | `query` | `vector`, `require_vector`, `limit` | authored Markdown/MDX when asked |

## Flow 1: known identifier

1. `semantic_search` with `exhaustive: true` and the identifier. Read the first
   page. If it warns `broad_or_query` or the matches are off, abandon it:
   refine it or use local text search.
   Never page merely because `next_cursor` exists.
2. For a valid traversal, copy `next_cursor` unchanged into `cursor` until
   `truncated: false`; page-local `returned` must sum to
   `total_chunks`. On `response_budget_too_small ... minimum_bytes=N`, retry
   the same page with `response_bytes: N`.
3. Drill down with `definition`: one returned `sym:` anchor plus the response
   `snapshot`, both copied verbatim. Then `who_uses` for callers, `calls`
   for members, `file_outline` for a file.

Scope transfer: while paging, keep the original `query` and any explicit
`origins`, `formats`, and `file_roles` unchanged, or the cursor is rejected.
Carry explicitly supplied `origins` and `formats` into `definition` and
`who_uses`; if the search omitted them, keep them omitted, and never build
them from the echoed `scope`.

## Flow 2: fuzzy query

One ranked `semantic_search` (`vector: true`), then Flow 1 on the identifiers
it surfaced.

## Tips

- `limit` is a page size, not a session ceiling; done means
  `truncated: false`.
- Do not re-search or expand after localization.
- Copy anchors verbatim; edited ones resolve to nothing.
- A matching pattern elsewhere is a convention, not proof a change is safe.
- If `snapshot` changes mid-task, restart that surface's traversal.
- Documentation prose is never runtime proof.

## On instruction

- Bug fix or feature: Flow 1 on the named code; docs only when told to.
- Blast radius: `who_uses` on the anchor, then `calls` for members.
- Docs question: `documentation_search`, then verify in code.
