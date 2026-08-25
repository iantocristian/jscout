# G24 documentation freshness — Phase 3 candidate evaluation

Date: 2026-08-26

## Outcome

Git-basis freshness ships available but disabled by default. None of movement
bounds 1, 2, or 3 passed the pre-registered guardrails in every retrieval
posture.

Every bound corrected four comparable obsolete-first conflicts per posture,
with no intended conflict reversed. Across all conflict cases, current-first
ordering increased from 6/12 to 10/12. The cost was broader than the intended
conflict: all bounds introduced four recent-irrelevant-over-evergreen
inversions in both hybrid and hybrid-plus-reranker retrieval, where the
no-freshness baseline had none. Bounds 2 and 3 additionally failed the
pre-registered Recall@5 guardrail.

The selected default is `freshness = false`, with no movement bound selected.
The implementation retains its dormant built-in value of 2:

```toml
[docs.search]
freshness = false
max_rank_movement = 2
```

The evaluation did not select or prefer that dormant value. Operators can opt
in with bounds 1–3, and `--no-freshness` restores the relevance order for an
individual search without removing provenance from results.

## Inputs and protocol

- Source: clean commit `da9bd0786051329ececd821a3a61afdc985b9fb4`;
  `jscout 0.4.0`; binary SHA-256
  `795579b5ad4955852b840c409003e3d92671f487f2c7bb05e703e458dfad74a0`.
- Corpus: 12 logical queries and 52 query-by-variant cases across clean,
  depth-2 shallow, non-Git, dirty, staged, and untracked repositories.
- Matrix: lexical, hybrid, and hybrid-plus-reranker retrieval, each with
  freshness disabled and with movement bounds 1, 2, and 3. The 624 scored
  cases were each executed twice with one generated checkout, database,
  binary, provider configuration, response budget, and candidate depth per
  variant.
- Retrieval: one request at `k = 20`, sliced offline at 1/3/5/10, with an
  explicit 1 MiB response budget.
- Models: pinned `BAAI/bge-m3` revision
  `5617a9f61b028005a4858fdac845db406aefb181` and
  `BAAI/bge-reranker-v2-m3` revision
  `953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e` on MPS/float16.
- Manifest SHA-256:
  `ff9525a3e29a6734c88ca780ffac10223e1c0305e993aa8b3abbb0c82365d089`.
- Frozen Phase 2 report SHA-256:
  `fedcada375257cad28facd340be25938b0d73f4c2ce70c6b6c1181ebd8fab954`.
- Machine result:
  [`g24-docs-retrieval-phase3-2026-08-26.json`](g24-docs-retrieval-phase3-2026-08-26.json),
  SHA-256
  `2eb15bdc6c23670d91c878b9534b7a9692966c6f8d5f28937cbec3984cdf153c`.

## Validity gates

| Gate | Result |
|---|---:|
| Phase 2 lexical/fallback parity carried from the frozen report | pass |
| Phase 2 disabled-treatment ranked identities | pass, 156/156 |
| Repeated ranked order | pass, 624/624 cases |
| Movement never exceeds the configured bound | pass, 9/9 comparisons |
| Reported base ranks and signed movement are consistent | pass |
| Unknown provenance remains stationary | pass |
| Git and observed provenance never cross | pass; vacuous because Phase 3 emits no observed basis |
| Enabled treatments start from the disabled candidate order | pass |
| Required vector/reranker stages active | pass |
| Response complete and every returned source current | pass |
| Pinned service configuration unchanged before/after | pass |
| Source worktree clean | pass |

Shallow-boundary, non-Git, untracked-file, and newly added staged-file content
remains `unknown`. Modified lines in already tracked files use `working_tree`.
Those barriers are part of the candidate ordering, not missing-data
exceptions.

## Aggregate retrieval

The table reports current-answer recall. `Current-first` counts all
query-by-variant conflict pairs, including the shallow and non-Git negative
controls. `Evergreen inversions` counts a recent irrelevant answer ranked
above its evergreen answer.

| Profile | Treatment | R@1 | R@3 | R@5 | R@10 | MRR | Current-first | Evergreen inversions |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| lexical | disabled | 70.2% | 88.5% | 100% | 100% | 0.846 | 6/12 | 6 |
| lexical | bound 1 | 51.0% | 88.5% | 100% | 100% | 0.746 | 10/12 | 6 |
| lexical | bound 2 | 43.3% | 88.5% | 100% | 100% | 0.698 | 10/12 | 6 |
| lexical | bound 3 | 39.4% | 78.8% | 96.2% | 100% | 0.657 | 10/12 | 6 |
| hybrid | disabled | 69.2% | 99.0% | 100% | 100% | 0.875 | 6/12 | 0 |
| hybrid | bound 1 | 63.5% | 99.0% | 100% | 100% | 0.833 | 10/12 | 4 |
| hybrid | bound 2 | 42.3% | 91.3% | 99.0% | 100% | 0.718 | 10/12 | 4 |
| hybrid | bound 3 | 40.4% | 81.7% | 99.0% | 100% | 0.669 | 10/12 | 4 |
| hybrid + reranker | disabled | 80.8% | 100% | 100% | 100% | 0.933 | 6/12 | 0 |
| hybrid + reranker | bound 1 | 73.1% | 100% | 100% | 100% | 0.894 | 10/12 | 4 |
| hybrid + reranker | bound 2 | 53.8% | 100% | 100% | 100% | 0.772 | 10/12 | 4 |
| hybrid + reranker | bound 3 | 53.8% | 96.2% | 100% | 100% | 0.753 | 10/12 | 4 |

Each enabled treatment changed 36 of 52 orders in every retrieval posture.
The observed maximum absolute movement was exactly its configured bound. Older
conflicts remained visible at ranks 5 and 10 in every arm.

## Guardrail decision

| Bound | Lexical | Hybrid | Hybrid + reranker | Decision |
|---:|---|---|---|---|
| 1 | pass | fail: 4 new evergreen inversions | fail: 4 new evergreen inversions | reject |
| 2 | pass | fail: R@5 regression and 4 new inversions | fail: 4 new inversions | reject |
| 3 | fail: R@5 and evergreen-top-five retention | fail: R@5 regression and 4 new inversions | fail: 4 new inversions | reject |

The result does not reject Git provenance or the bounded algorithm. Provenance
is still indexed and returned, and every ordering invariant passed. It rejects
using generic recency as the default final-ranking policy on this corpus.

## Limits

This is a small synthetic corpus with one independent authored-instruction
conflict expressed by two query framings. It is sufficient for the declared
default gate, not for a general claim about documentation quality. A future
default change requires a new pre-registered evaluation; it must not reinterpret
this failed run or change the corpus after seeing these results.
