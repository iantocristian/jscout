# G24 documentation retrieval — Phase 2 baseline

Date: 2026-08-25

## Outcome

The Phase 2 baseline is recorded and the Phase 3 implementation prerequisite
is satisfied. No freshness movement bound or default is selected by this
report.

The first baseline exposed that the original conflict query was already
current-first in every retrieval posture. It was discarded. The
[pre-registered addendum](../prereg/g24-documentation-freshness-addendum-2026-08-25.md)
adds one stale-premise query over the same current and obsolete authored
instructions. In the regenerated baseline, the obsolete instruction is rank 1
and the current instruction rank 2 in every posture and repository variant,
so Phase 3 has a real, bounded correction opportunity.

## Inputs and protocol

- Source: clean commit `1fca0bf7a3eb60d246becd8114bc520e49177958`;
  `jscout 0.4.0`; binary SHA-256
  `386051a4e4fc6942b58d9029d888bedb9d352c94ffd2a7f24dd0d59117619419`.
- Corpus: 12 logical queries and 52 query-by-variant cases across clean,
  depth-2 shallow, non-Git, dirty, staged, and untracked repositories.
- Retrieval: one request at `k = 20`, sliced offline at 1/3/5/10, with an
  explicit 1 MiB response budget. Every case was repeated identically.
- Models: pinned `BAAI/bge-m3` revision
  `5617a9f61b028005a4858fdac845db406aefb181` and
  `BAAI/bge-reranker-v2-m3` revision
  `953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e` on MPS/float16.
- Manifest SHA-256:
  `ff9525a3e29a6734c88ca780ffac10223e1c0305e993aa8b3abbb0c82365d089`.
- Machine result:
  [`g24-docs-retrieval-phase2-2026-08-25.json`](g24-docs-retrieval-phase2-2026-08-25.json),
  SHA-256
  `fedcada375257cad28facd340be25938b0d73f4c2ce70c6b6c1181ebd8fab954`.

## Validity gates

| Gate | Result |
|---|---:|
| Lexical vs provider-absent fallback exact ranked-identity parity | pass, 52/52 |
| Repeated ranked order | pass, 208/208 runs |
| Explicit response budget and no truncation | pass |
| Search snapshot equals indexed snapshot | pass |
| Every returned source slice is current | pass |
| Required vector/reranker stages active | pass |
| Six post-embed vector generations ready | pass |
| Pinned service configuration unchanged before/after | pass |
| Obsolete-first conflict within movement bounds in every posture | pass |
| Source worktree clean | pass |

## Aggregate retrieval

Recall averages all gold current answers; the split-section query has two gold
answers. `Conflict current-first` counts query-by-variant conflict pairs.

| Profile | R@1 | R@3 | R@5 | R@10 | MRR | Conflict current-first | Older visible @5/@10 | Evergreen inversions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| lexical | 70.2% | 88.5% | 100% | 100% | 0.846 | 6/12 | 100% / 100% | 6 |
| fallback | 70.2% | 88.5% | 100% | 100% | 0.846 | 6/12 | 100% / 100% | 6 |
| hybrid | 69.2% | 99.0% | 100% | 100% | 0.875 | 6/12 | 100% / 100% | 0 |
| hybrid + reranker | 80.8% | 100% | 100% | 100% | 0.933 | 6/12 | 100% / 100% | 0 |

Relative to lexical retrieval, hybrid changes all 52 ranked orders, moves
Recall@1 by -1.0 percentage point and Recall@3 by +10.6 points, and removes all
six recent-noise-over-evergreen inversions. Adding the reranker changes all 52
orders, improves Recall@1 by 10.6 points and Recall@3 by 11.5 points, and also
removes every evergreen inversion. These are retrieval-treatment differences,
not freshness effects.

## Per-variant results

| Variant | Cases | Lexical R@1 / R@3 | Hybrid R@1 / R@3 | Hybrid + reranker R@1 / R@3 |
|---|---:|---:|---:|---:|
| clean | 8 | 68.8% / 87.5% | 68.8% / 100% | 81.3% / 100% |
| shallow | 8 | 68.8% / 87.5% | 68.8% / 100% | 81.3% / 100% |
| non-Git | 8 | 68.8% / 87.5% | 68.8% / 100% | 81.3% / 100% |
| dirty | 9 | 72.2% / 88.9% | 72.2% / 100% | 83.3% / 100% |
| staged | 10 | 70.0% / 90.0% | 65.0% / 95.0% | 75.0% / 100% |
| untracked | 9 | 72.2% / 88.9% | 72.2% / 100% | 83.3% / 100% |

The freshness-sensitive auth query is obsolete rank 1/current rank 2 in all
24 posture-by-variant cases. Full-Git clean, dirty, staged, and untracked
variants are positive correction arms; shallow and non-Git are provenance
negative controls. The evergreen partition answer is rank 4 under lexical,
rank 2 under hybrid, and rank 1 after reranking. The staged retry-budget answer
is rank 2 under both provider-backed postures. Every current answer is present
by rank 5, and every obsolete conflict remains visible through rank 10.

## Limits and next gate

This is a small synthetic corpus. It verifies treatment plumbing, invariants,
and a controlled relevance-versus-freshness conflict; it is not a general
documentation-quality benchmark. The positive freshness case is one
independent instruction conflict with two query framings. The evergreen and
working-tree cases are guardrails against improving that conflict by globally
promoting recent text.

Phase 3 must now add Git provenance, `--no-freshness`, and movement bounds 1–3,
then satisfy the unchanged pre-registered hard gates and default-selection
rule. If no bound passes, freshness remains off by default.
