# G26 phase-1 initial treatment — preserved failure

This is the first treatment of the preregistered protocol-v2 manifest. It is
retained because it exposed a control-design error; it is not a passing G26
result.

- treatment revision: `74f248968f8567e683fbf98c1c1981f1ea97ff01`
- binary SHA-256: `8b2215ba28fac2ad228f4ef28fd9df8fd737730ed2d284d9ac84088f2126b963`
- raw report SHA-256: `eee38289fcbf9c3f1e1995be37ba32d218f4cb56e203a303c81dcc3605f9ee58`
- provider-free lexical retrieval: yes
- retrieval depth / file cutoff: 100 chunks / 10 files
- decision: **fail**

| Cohort | Stratum | Recall@10 | MRR | Baseline recall@10 | Baseline MRR |
| --- | --- | ---: | ---: | ---: | ---: |
| Rust | exact identifier | 1.0000 | 0.8889 | 0.0000 | 0.0000 |
| Rust | BM25 prose | 1.0000 | 0.7361 | 0.0000 | 0.0000 |
| JS/TS control | exact identifier | 1.0000 | 1.0000 | 1.0000 | 1.0000 |
| JS/TS control | BM25 prose, combined corpus | 0.9167 | 0.4341 | 1.0000 | 0.6181 |

`control-bm25-03` lost `checker/src/protocol.mjs` from the combined top ten.
The new higher-ranked results were Rust files implementing the same checker
protocol, planning, gateway, provider, and indexing concepts named by the
control queries. The feature therefore changed the shared corpus in exactly
the dimension the old control incorrectly required to remain fixed.

As a diagnostic only, projecting the unchanged raw top-100 treatment rankings
to the frozen JS/TS suffix set produced Recall@10 `1.0000` and MRR `0.6069`, a
drop of `0.0111` from baseline and inside the original `0.02` threshold. That
projection was selected after inspecting this treatment and cannot convert the
formal failure into a pass. A replacement control must be frozen prospectively
and evaluated on fresh holdout queries; mixed-corpus results should remain
reported because they describe the user-visible ranking.
