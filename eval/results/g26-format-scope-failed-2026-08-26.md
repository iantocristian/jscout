# G26 format-scoped lexical retrieval — preserved failure

This is the prospective protocol-v3 treatment from
`eval/prereg/g26-format-scope-2026-08-26.json`. The filtered-format contract
passed. The mixed-language relevance gate failed and remains failed; no
threshold, query, judgment, or returned ranking was changed after inspection.

- baseline revision: `444540e3a29b68bfd5adc42bc4df99b5c5a92386`
- treatment/corpus revision: `10e1cd54e052fd5cc4ab81b7483c4d14f6f8621f`
- baseline report SHA-256: `8fdd05161f5bd46056729c84245c2a601f1c87ce4048df8bb8df38a033abe9d6`
- treatment report SHA-256: `44058234ed253c786003f2c5cd9ad53edaee3552e3ba51b90e37e7f65fa94589`
- manifest SHA-256: `ff1ceb6ebd93c9f0749c28b78132603365534de6a6b04d48f8e21b0f472ed87e`
- copied query payload SHA-256: `ed7f93d3c9b9ebd36071c3c2b2f08a149ddbcb35a61b8316b94124ed24e1c7be`
- runner SHA-256: `89ba45940d22d6b49f88aa4ce19897d738553e3ffd71164f5393588779efe572`
- retrieval depth / file cutoff: 100 chunks / 10 first-occurrence-deduplicated files
- provider-free lexical retrieval: yes
- decision under the frozen manifest: **fail**

## Gate outcomes

| Gate | Baseline | Treatment | Threshold | Outcome |
| --- | ---: | ---: | ---: | --- |
| filtered JS/TS Recall@10 | 1.0000 | 1.0000 | no decrease | pass |
| filtered JS/TS mean MRR | 0.8833 | 0.8854 | drop <= 0.0200 | pass |
| baseline top-five gold retained | — | no displacement | rank <= 10 | pass |
| mixed source-qrel mean nDCG@10 | — | 0.5084 | >= 0.7000 | **fail** |

All filtered treatment hits were JavaScript or TypeScript. The exact-query
file lists were unchanged. The prose lists reordered because the shared FTS5
table still computes document frequency and average document length over Rust,
but the filtered metrics did not regress. This supports the chosen role of
`formats` as candidate scoping rather than statistics isolation.

## What the failed mixed score establishes

The mixed top tens contained 165 Rust and 75 JavaScript files. This was not a
simple case of irrelevant Rust replacing relevant JavaScript: 19 of 24 queries
returned at least one authored positive from both languages, and none returned
no authored positive. It was nevertheless a real coverage/ranking failure:
only 68 of 138 authored positive files appeared in the first ten.

| Authored positives | Retrieved at 10 | Recall@10 |
| --- | ---: | ---: |
| JavaScript | 26 / 55 | 0.4727 |
| Rust | 42 / 83 | 0.5060 |
| Combined | 68 / 138 | 0.4928 |

The score is also downward-biased and is not an absolute relevance estimate.
The frozen judgments were prospective source-authored positive qrels, not an
adjudicated retrieval pool: all 138 judgments had grades 1–3, no explicit
grade-zero files existed, and tests/fixtures were omitted. Only 68 of the 240
returned top-ten slots were judged. The metric assigned zero gain to the other
172 slots as preregistered even though at least 25 were direct behavior tests
of the query. The formal result therefore stays failed, but a future nDCG gate
must pool and blindly adjudicate every candidate before scoring.

## Diagnostic causes

- Rust contributed 888 lexical chunks averaging 4,028 bytes; JavaScript
  contributed 844 chunks averaging 824 bytes. Phase-1 Rust lossless chunks
  combine much more unrelated text per retrieval unit.
- Ranked lexical queries OR-join every prose token. Generic terms therefore
  match broad Rust chunks and unrelated test, scouting, MCP, and indexing code.
- The FTS tokenizer preserves `_` and treats camel-case identifiers as one
  token. Prose terms such as `service tier`, `complete request`, and `submit
  tool` do not match `service_tier`, `CompleteRequest`, or `SubmitTool`.
- Rust lossless chunks retain all comments. The ECMAScript chunker attaches
  only one immediately preceding comment token, so a contiguous descriptive
  header can be absent from the searchable chunk that follows it.
- The first ten raw chunk hits averaged 6.25 unique files. Repeated chunks from
  one file are a user-visible diversity problem even though the evaluation's
  file deduplication masks it.

These observations do not justify per-format FTS tables or a language quota.
Any corrective treatment needs a new committed revision and a new prospective
evaluation; this inspected holdout cannot be used as confirmatory evidence for
a change tuned against it.
