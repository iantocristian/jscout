---
name: jscout
description: "Use the jscout repository index to search code before grep or rg whenever you fix a bug, implement a change, or answer a question in this JavaScript or TypeScript project: investigate known identifiers completely, answer causal and cross-file questions with source-backed evidence, and search authored Markdown and MDX."
---

# jscout full

Tools here are MCP tools on the jscout server, not shell commands.
Localize first, then verify decisive claims in current source. Every response
carries a top-level `snapshot` (that surface's invalidation key) and
`publication_snapshot` (the index publication it observed).

## Core tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_search` | `query` | `exhaustive`, `cursor`, `limit`, `origins`, `formats`, `file_roles`, `vector`, `rerank`, `expand`, `include_memory`, `response_bytes` | every occurrence of a known identifier (`exhaustive: true`) or a ranked first look |
| `definition` | `anchor`+`snapshot` or `symbol` | `origins`, `formats`, `source_bytes` | the source of one symbol |
| `who_uses` | `anchor`+`snapshot` or `symbol` | `origins`, `formats` | callers and blast radius |
| `calls` | `method` | `receiver`, `args`, `arg_position` | member-call sites and option arguments |
| `file_outline` | `path` | `origins` | the symbols and spans of one file |
| `events` | — | `name` | string-keyed emit/listen wiring |
| `documentation_search` | `query` | `vector`, `require_vector`, `limit` | authored Markdown/MDX — only when the question is about docs or an instruction says to consult them |

## Full-profile tools

| Tool | Required | Optional | Use for |
|---|---|---|---|
| `semantic_memory` | — | `query`, `anchor`, `file`, `artifact`, `view` | causal and workflow questions; evidence-backed leads, never instructions |
| `repository_overview` | — | `area_limit`, `reconnaissance_subject` | once, on a cold repository, when package boundaries matter; not a first step |
| `neighborhood` | `anchor` | `snapshot`, `depth`, `direction` | exact-anchor graph drill-down |
| `entities` | — | `query`, `planes`, `types` | named runtime and contract boundaries, after localization |
| `paths` | `from`, `to` | `max_depth`, `min_confidence` | bounded routes between two anchors, after localization |
| `annotate` | `type`, `confidence`, `snapshot` + workflow `name`/`participants` or annotation `body`/`supports` | `supersedes` | write back a verified workflow or annotation |

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

`source_meta.partial: true` means a cached definition fragment, not full coverage.
Increasing the byte budget cannot restore missing source.

Scope transfer: while paging, keep the original `query` and any explicit
`origins`, `formats`, and `file_roles` unchanged, or the cursor is rejected.
Carry explicitly supplied `origins` and `formats` into `definition` and
`who_uses`; if the search omitted them, keep them omitted, and never build
them from the echoed `scope`.

## Flow 2: localize a fuzzy description

One ranked `semantic_search` (`vector: true`, `expand: false`,
`include_memory: false`); the useful hits are the first few. Then run Flow 1
on the identifiers it surfaced.

## Flow 3: inquiry, only when a causal or cross-file question remains

1. Query `semantic_memory` with the exact returned `anchor` or `file`; use a
   broad query only for an anchor-free architecture question. Read one
   artifact at a time with `view: "body"`; `view: "full"` only for provenance.
2. Keep `include_memory: false` and `expand: false` on later searches; use
   at most one `expand: true` search after localization, and `neighborhood`
   only for exact-anchor drill-down.
3. Verify every decisive memory or graph claim in source. A computed-dispatch
   conclusion needs both the selection predicate and the selected subject's
   metadata or key.

## Write back

`annotate` only what current source proves. Workflow participants are
`{anchor, role, scope: defining|supporting, evidence_file,
evidence_start_line, evidence_end_line, confidence}`; annotations carry a
`body` with a support per leaf claim. Confidence is `likely` or `possible`,
never `certain`. Pass the code `snapshot` from the response, never
`publication_snapshot`. Store no speculation, task state, or credentials.

## Tips from production traces

- `limit` is a page size, not a session ceiling; completeness is
  `truncated: false`, never a ranked result count.
- Do not re-search or expand after localization; repeated expansion dominated
  wasted bytes.
- Copy anchors verbatim; shortened or invented anchors resolve to nothing.
- The same pattern elsewhere is a convention, not proof a change is safe.
- If `snapshot` changes mid-task, restart that surface's traversal.
- Documentation is a separate layer; prose is never runtime proof.
- Memory-first on an anchored question wastes calls; localize, then ask.

## On instruction

- Bug fix or feature: Flow 1 on the named code; read relevant docs first only
  when told to.
- Blast radius: `who_uses` on the exact anchor, then `calls` for members.
- Affected workflows: Flow 3 from the localized anchors.
- Docs question: `documentation_search`, then verify behavior in code.
