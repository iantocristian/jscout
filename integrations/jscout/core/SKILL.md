---
name: jscout
description: Localize and prove code in a jscout-indexed repository; consult authored docs on request.
---

# jscout core

Localize first, then verify in source. Responses carry `snapshot` (this
surface's invalidation key) and `publication_snapshot`.

## Tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_search` | `query` | `exhaustive`, `cursor`, `limit`, `origins`, `formats`, `file_roles`, `vector`, `rerank`, `response_bytes` | all occurrences of an identifier (`exhaustive: true`) or a ranked look |
| `definition` | `anchor`+`snapshot` or `symbol` | `origins`, `formats`, `source_bytes` | the source of one symbol |
| `who_uses` | `anchor`+`snapshot` or `symbol` | `origins`, `formats` | callers and blast radius |
| `calls` | `method` | `receiver`, `args`, `arg_position` | member-call sites and options |
| `file_outline` | `path` | `origins` | one file's symbols and spans |
| `events` | — | `name` | string-keyed emit/listen wiring |
| `documentation_search` | `query` | `vector`, `require_vector`, `limit` | authored Markdown/MDX; only for docs questions or when instructed |

## Flow 1: known identifier

1. `semantic_search` with `exhaustive: true` and the identifier. Read the first
   page. If it warns `broad_or_query` or the matches are off, abandon it
   immediately: refine it or use local text search.
   Never page merely because `next_cursor` exists.
2. For a valid traversal, copy `next_cursor` unchanged into `cursor` until
   `truncated: false`; the sum of page-local `returned` must equal
   `total_chunks`. On `response_budget_too_small ... minimum_bytes=N`, retry
   the same page with `response_bytes: N`.
3. Drill down with `definition`: one returned `sym:` anchor plus the response
   `snapshot`, both copied verbatim. Then `who_uses` for callers, `calls` for
   members, `file_outline` for one file.

Scope transfer: while paging, keep the original `query` and any explicit
`origins`, `formats`, and `file_roles` unchanged, or the cursor is rejected.
Carry explicitly supplied `origins` and `formats` into `definition` and
`who_uses`; if the search omitted them, keep them omitted, and never build
them from the echoed `scope`.

## Flow 2: fuzzy description

One ranked `semantic_search` (`vector: true`); the useful hits are the first
few. Then Flow 1 on the identifiers it surfaced.

## Tips

- `limit` is a page size, not a session ceiling; completeness is
  `truncated: false`.
- Do not re-search or expand after localization; that dominated wasted bytes.
- Copy anchors verbatim; invented or shortened ones resolve to nothing.
- The same pattern elsewhere is a convention, not proof a change is safe.
- If `snapshot` changes mid-task, restart that surface's traversal.
- Documentation is a separate layer; prose is never runtime proof.

## On instruction

- Bug fix or feature: Flow 1 on the named code; docs first only when told to.
- Blast radius: `who_uses` on the exact anchor, then `calls` for members.
- Docs question: `documentation_search`, then verify in code.
