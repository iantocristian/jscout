---
name: jscout
description: Use the jscout repository index to localize definitions, callers, workflows, events, and structural context before broad filesystem search. Apply when answering repository questions, planning changes, or assessing blast radius in an indexed JavaScript or TypeScript project.
---

# jscout repository guide

Use jscout as the primary localization interface for unfamiliar repository
questions, then verify decisive claims in source.

- Start with `semantic_search`. Keep expansion off for exact lookups.
- For blast-radius, multi-hop, or workflow questions, use
  `semantic_search` with `expand=true` and a small depth/budget.
- Use `definition` for exact source and `who_uses` for direct callers/usages.
- Use `file_outline` after localizing a file and `events` for string-keyed
  emit/listener wiring.
- Use `neighborhood` for targeted drill-down when an exact anchor is already
  known. Expanded search is the normal discovery surface.
- Treat `possible` confidence as a candidate, not a fact. It includes
  unresolved receiver/member-call and event relationships.
- Refine the query or drill into exact definitions before increasing result
  budgets. Graph context can contain irrelevant structural neighbors.
- If jscout returns no relevant evidence, fall back to repository-local search.

Never treat graph reachability as runtime proof. Dynamic registration,
configuration, reflection, and computed imports still require source or runtime
verification.
