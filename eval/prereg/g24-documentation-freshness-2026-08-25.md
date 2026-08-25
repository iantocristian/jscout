# Pre-registration — G24 documentation freshness

Registered: 2026-08-25, before Phase 3 implementation begins.

This document fixes the comparison and decision rule for G24's proposed
Git-basis freshness reorder. It does not authorize the observation ledger,
historical search, contradiction detection, or model-visible temporal
metadata. Once Phase 3 implementation begins, corrections belong in a dated
addendum rather than a silent rewrite.

## Change under test

Phase 3 may attach current-checkout Git provenance to documentation chunks and
apply the bounded adjacent-swap reorder specified by G24 after BM25/vector RRF
and optional reranking. The proposed `max_rank_movement = 2` is a hypothesis,
not the selected default. `--no-freshness` must preserve relevance order while
still reporting provenance.

The fixed Atlas Relay corpus is generated from
[`../fixtures/docs-retrieval/manifest.json`](../fixtures/docs-retrieval/manifest.json).
It contains:

- current and obsolete conflicting instructions with known author order;
- old evergreen specifications beside recent keyword-heavy noise;
- canonical guides beside a recent changelog;
- formatting-only and heading-only commits;
- a pure rename and a section split;
- dirty tracked, staged tracked, and untracked documents;
- clean full-history, shallow, and non-Git copies of the same checkout; and
- both Markdown and MDX retrieval units.

Gold labels are stable logical answer IDs resolved by repository-relative path
and heading breadcrumb. Database-local chunk IDs and snapshot IDs are never
gold identities.

## Frozen Phase 2 baseline

Before Phase 3 code, run the checked-in harness against the same binary and
fixture in four explicit arms:

| Arm | Retrieval treatment |
|---|---|
| `lexical` | `--lexical-only` |
| `fallback` | default vector request with no provider and `--no-rerank` |
| `hybrid` | required vector participation and `--no-rerank` |
| `hybrid-rerank` | required vector participation and required active reranking |

The vector arms use the pinned local `BAAI/bge-m3` revision
`5617a9f61b028005a4858fdac845db406aefb181`. The reranker arm uses
`BAAI/bge-reranker-v2-m3` revision
`953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e`. A degraded, not-ready,
truncated, source-mismatched, or nondeterministically ordered arm is invalid;
it is not scored as a retrieval miss. The fallback and lexical ranked
identities must be exactly equal.

Every query is issued once at `k = 20` per repetition and sliced offline for
smaller cutoffs. Changing `k` would also change the internal candidate pool and
is therefore a different treatment.

## Phase 3 arms

The candidate report must use one Phase 3 binary and one generated checkout per
variant. Hold corpus bytes, query text, provider, model revisions, reranker
posture, response budget, and candidate depth fixed.

For each of lexical, hybrid, and hybrid-plus-reranker retrieval, compare:

- freshness disabled;
- freshness enabled with movement bound 1;
- freshness enabled with movement bound 2; and
- freshness enabled with movement bound 3.

The no-freshness arm must match the Phase 2 ranked identities after normalizing
only newly added temporal response fields. Freshness comparisons use each
same-run no-freshness arm, never a different model or snapshot.

## Metrics

Report per arm and per corpus variant:

- current-answer Recall@1, @3, @5, and @10 plus mean reciprocal rank;
- current-before-obsolete ordering for every comparable conflict pair;
- obsolete-conflict visibility@5 and @10;
- evergreen answer rank and recent-irrelevant-over-evergreen inversions;
- exact changed-order count and top-k entrants/exits caused only by freshness;
- each hit's base rank, final rank, movement, basis, and basis value;
- maximum absolute movement and movement histogram; and
- hybrid lift relative to the same no-freshness lexical treatment.

Rename, split, formatting-only, heading-only, shallow, and non-Git cases remain
separately visible rather than being averaged away.

## Hard validity gates

A candidate arm is invalid if any of the following occurs:

1. A hit moves farther than its configured bound.
2. An unknown-basis hit moves.
3. Git-basis and observed-basis hits reorder against one another.
4. The disabled-freshness order differs from the Phase 2 baseline beyond the
   newly added temporal fields.
5. Required vector or reranker participation is not active.
6. Search truncates, returns a non-current source slice, changes order across
   identical repetitions, or reads a snapshot different from the indexed one.

## Default-selection rule

A movement bound passes only when, in every measured retrieval posture:

1. all hard validity gates pass;
2. current-answer Recall@5 and Recall@10 do not fall below that posture's
   no-freshness baseline;
3. no evergreen answer present in the baseline top five leaves the top five,
   and no new recent-irrelevant-over-evergreen inversion appears;
4. every obsolete conflict visible in the baseline top ten remains visible in
   the top ten; and
5. at least one intended comparable conflict changes from obsolete-first to
   current-first without reversing another intended conflict.

Among passing bounds, choose the smallest bound attaining the best number of
current-first conflict pairs, breaking any remaining tie by higher
current-answer Recall@3 and then lower changed-order count. If no bound passes,
freshness remains off by default. The evaluation may therefore reject all of
1, 2, and 3; the plan's proposed value of 2 receives no preference.

Hybrid lift and reranker effects are reported but do not excuse a failed
freshness guardrail. Conversely, a weak vector result does not justify changing
the embedding model inside this experiment.

## Artifacts and amendments

The Phase 2 report records corpus, manifest, harness, binary, and non-secret
configuration fingerprints plus exact ranked identities and component scores.
The Phase 3 report additionally records the Phase 2 report hash and all
temporal fields. Materialized Git repositories, databases, provider logs, and
secrets remain outside the product repository.

A fixture, gold-label, or scorer defect discovered before Phase 3 requires a
dated addendum and a regenerated Phase 2 baseline. A result-driven corpus or
threshold change invalidates default selection for that run.

