# Addendum — G24 conflict-arm correction

Registered: 2026-08-25, after the first Phase 2 baseline and before Phase 3
implementation.

This addendum corrects one corpus-admission defect under the amendment rule in
[`g24-documentation-freshness-2026-08-25.md`](g24-documentation-freshness-2026-08-25.md).
It does not change a metric, threshold, model, retrieval treatment, movement
bound, or default-selection rule.

## Defect

The original manifest had one conflict query, `auth-current-vs-v1`. Its query
explicitly named Relay v2, so all four Phase 2 retrieval postures already
ranked the current v2 instruction ahead of the obsolete v1 instruction in all
six repository variants. The default-selection rule requires at least one
intended comparable pair to change from obsolete-first to current-first.
Consequently, the original corpus could not demonstrate the behavior Phase 3
is meant to test, regardless of whether the implementation was correct.

## Correction

The manifest adds `ambiguous-api-key-header`, a second query over the same
authored bytes and the same current/obsolete qrels:

> What API-key HTTP header should an Atlas Relay publisher send with publish
> requests?

The wording represents a realistic stale premise: the obsolete v1 document
answers it directly, while the current v2 guide corrects the premise and says
to use `Authorization` instead. A pre-Phase-3 probe placed the obsolete v1
section at rank 1 and the current v2 section at rank 2 under lexical, hybrid,
and hybrid-plus-reranker retrieval on the clean repository. They are therefore
within the smallest proposed movement bound.

The clean, dirty, staged, and untracked variants retain usable full Git
provenance for this pair and are intended positive correction arms. The
shallow and non-Git variants remain negative provenance controls: they must not
invent comparable freshness merely to fix the relevance order.

No fixture document, Git commit, qrel, or existing query changed. The
pre-correction baseline is discarded. The canonical Phase 2 report must be
regenerated from the amended manifest before Phase 3 begins.
