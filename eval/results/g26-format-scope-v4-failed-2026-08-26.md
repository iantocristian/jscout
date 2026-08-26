# G26 format-scoped lexical retrieval v4 — decision-grade failure

Protocol v4 repaired the incomplete-judgment defect in the preserved v3
evaluation. It used a fresh source-only mixed holdout, pooled the first ten
deduplicated files from both arms plus authored positive recall sentinels,
removed arm/rank/score provenance, and assigned an explicit blind `0`–`3` qrel
to every pooled candidate before scoring. The resulting decision is **fail**:
the treatment improved substantially over the Rust-free baseline, but did not
reach the frozen absolute mixed-relevance threshold.

## Frozen provenance

- baseline revision: `444540e3a29b68bfd5adc42bc4df99b5c5a92386`
- treatment/corpus revision: `5b7d3f62406a782ef957f260104955691f661155`
- manifest SHA-256: `b7f330d5aeae5b96b0eb25ef77a12a8f5ce0cf93c8d2b00381d430d428c0c9a6`
- runner SHA-256: `502c72b2227cb258095d5ebf3a2d9c9ea0d187e1f2a02ff9b3285da693f1d9e5`
- baseline report SHA-256: `557bb4b4287bf922226db22a5fd4547a1a6b6813a7f99e7f64da5ee659532434`
- treatment report SHA-256: `ac2948a437c22131daeac16f2b5b3e556462a709befd193a0fce7ba3959e0845`
- [blinded pool](g26-format-scope-v4-pool-2026-08-26.json) file SHA-256:
  `e6b8d1a1719d4f427a62a609795af6a21214ec0c63cadf46d0c1ffc5742567f8`
- internal pool SHA-256: `6e850924f0c33114ab3a85582b867b29eb8048e90bee58941e42393368e65d03`
- [complete qrels](g26-format-scope-v4-qrels-2026-08-26.json) file SHA-256:
  `1d3bb1183984531bab59cccf6ce9341b1adab5145e051ffcccec4f741a50b991`
- [score report](g26-format-scope-v4-score-report-2026-08-26.json) SHA-256:
  `8cba051de7b81d07c591c093b2c9c9de4d0ece9d5ddcb2679be34c0ecf3982b9`
- provider-free lexical retrieval: yes
- retrieval depth / file cutoff: 100 chunks / 10
  first-occurrence-deduplicated files
- formal decision under the frozen manifest: **fail**

## Gate outcomes

| Gate | Baseline | Treatment | Threshold | Outcome |
| --- | ---: | ---: | ---: | --- |
| filtered JS/TS Recall@10 | 1.000000 | 1.000000 | no decrease | pass |
| filtered JS/TS mean MRR | 0.883333 | 0.885417 | drop <= 0.020000 | pass |
| baseline top-five positive retained | — | no displacement | rank <= 10 | pass |
| mixed mean nDCG@10, absolute | 0.310561 | 0.597555 | treatment >= 0.700000 | **fail** |
| mixed mean nDCG@10, relative | 0.310561 | 0.597555 | drop <= 0.020000 | pass (`+0.286994`) |
| mixed judged@10 | 1.000000 | 1.000000 | 1.000000 in both arms | pass |

Every returned file through rank ten in both mixed arms had an explicit qrel.
Unlike v3, no unjudged result was silently assigned zero gain, so the v4 mixed
decision is suitable for choosing the next retrieval treatment.

## Descriptive outcomes

| Measure | Baseline | Treatment | Change |
| --- | ---: | ---: | ---: |
| authored-positive Recall@10 | 0.319697 | 0.550000 | +0.230303 |
| JavaScript files in mixed top tens | 220 | 60 | -160 |
| Rust files in mixed top tens | 0 | 160 | +160 |

The filtered gates validate format-scoped JS/TS retrieval as a regression
boundary. On the mixed holdout, adding Rust improved both blind graded relevance
and authored-positive recall over the Rust-free baseline. The failure is
therefore not evidence for per-language FTS statistics, a language quota, or a
language weight: the new language supplied relevant results rather than merely
displacing better JS/TS results.

The absolute `0.597555` nDCG@10 remains below the frozen `0.70` requirement.
Phase-1 broad lossless Rust chunks are not sufficient for mixed retrieval, so
G26 advances to Phase 2a named, item-local Rust chunks. Exact definitions,
exact occurrences, Rust vectors, and module edges remain disabled. Identifier
aliases must not be appended to the broad phase-1 chunks; any alias experiment
belongs to the item-local projection and requires its own prospective test.

## Relationship to prior failures

The original phase-1 failure and the protocol-v3 failure remain preserved.
V4 does not regrade or erase either result. It supersedes only v3's
decision-quality limitation by using a complete blind pool:

- [initial phase-1 failure](g26-rust-phase1-initial-failed-2026-08-26.md)
- [protocol-v3 preserved failure](g26-format-scope-failed-2026-08-26.md)
