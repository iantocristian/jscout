---
name: jscout
description: Localize and prove code in a jscout-indexed repository, and consult authored documentation when asked.
---

# jscout core

Localize first, then verify decisive claims in current source. Every response
carries a top-level `snapshot` (that surface's invalidation key) and
`publication_snapshot` (the index publication it observed).

## Tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_search` | `query` | `exhaustive`, `cursor`, `limit`, `formats`, `file_roles`, `vector`, `rerank`, `response_bytes` | every occurrence of a known identifier (`exhaustive: true`) or a ranked first look |
| `definition` | `anchor`+`snapshot` or `symbol` | `origins`, `formats`, `source_bytes` | the source of one symbol |
| `who_uses` | `anchor`+`snapshot` or `symbol` | `origins`, `formats` | callers and blast radius |
| `calls` | `method` | `receiver`, `args`, `arg_position` | member-call sites and option arguments |
| `file_outline` | `path` | `origins` | the symbols and spans of one file |
| `events` | — | `name` | string-keyed emit/listen wiring |
| `documentation_search` | `query` | `vector`, `require_vector`, `limit` | authored Markdown/MDX — only when the question is about docs or an instruction says to consult them |

## Flow 1: investigate a known identifier

1. `semantic_search` with `exhaustive: true` and the identifier. Read the first
   page. If it warns `broad_or_query` or the matches are off, abandon it
   immediately: refine the identifier or fall back to local text search.
   Never page merely because `next_cursor` exists.
2. For a valid traversal, copy `next_cursor` unchanged into `cursor` until
   `truncated: false`; the sum of page-local `returned` must equal
   `total_chunks`. On `response_budget_too_small ... minimum_bytes=N`, retry
   the same page with `response_bytes: N`.
3. Drill down with `definition`: one returned `sym:` anchor plus the response
   `snapshot`, both copied verbatim. Then `who_uses` for callers, `calls` for
   member methods, `file_outline` for one file.

## Flow 2: localize a fuzzy description

One ranked `semantic_search` (`vector: true`); the useful hits are the first
few. Then run Flow 1 on the identifiers it surfaced.

## Tips from production traces

- `limit` is a page size, not a session ceiling; completeness is
  `truncated: false`, never a ranked result count.
- Do not re-search or expand after localization; repeated expansion dominated
  wasted bytes.
- Copy anchors verbatim; shortened or invented anchors resolve to nothing.
- The same pattern elsewhere is a convention, not proof a change is safe.
- If `snapshot` changes mid-task, restart that surface's traversal.
- Documentation is a separate layer; prose is never runtime proof.

## On instruction

- Bug fix or feature: Flow 1 on the named code; read relevant docs first only
  when told to.
- Blast radius: `who_uses` on the exact anchor, then `calls` for members.
- Docs question: `documentation_search`, then verify behavior in code.
