# AFFiNE independent verification and extension

Date: 2026-08-14
Corpus: AFFiNE at `0f349af8ee` (`canary`), reusing the preserved database from the prior experiment
Binary: source byte-identical to the prior run's branch (merged as PR #25); fresh release build
Author: Claude (independent of the prior ChatGPT experiment)
Model spend: 28 calls, 73,507 tokens, 100% `plan` billing path

Related: [prior analysis](affine-experiment-analysis-2026-08-14.md) · [prior proposed fixes](affine-proposed-fixes-2026-08-14.md)

## Method

Two tracks. Track A adversarially re-verified every load-bearing claim in the prior
analysis before fixes get built on them. Track B covered what that experiment did
not test: the generative scouting layer end to end (bounded real model calls),
`workflow-candidates` against checker enrichment, `jscout calls`, search with
semantic artifacts attached, and the coverage arithmetic. The AFFiNE tree and its
root database were not modified (md5-verified before/after); all writes ran on a
scratch copy that preserved the 32,182 embeddings.

## Verdicts on the prior experiment's claims

| Claim | Verdict |
|---|---|
| `file_role.rs:73` misclassifies singular `doc`; ≥62 files affected | **Confirmed, understated** — 52 of the repo's 103 documentation-role files are production code (50.5%); 10/10 sampled are production; 7,698 LOC |
| `--file-role production` loses the gateway→storage edge | **Confirmed exactly** (13→12 edges, `pushDocUpdates` gone) |
| Reranker unreliable as default; promotes tests | **Nuanced** — 2 of 4 queries hurt, 2 helped; the permission query was solved *only* by rerank (rank 10→1); test pollution unchanged |
| Rerank adds ~5–10 s | **Corrected** — +3.1–3.4 s (16× on a 0.21 s hybrid baseline) |
| Rerank pool starvation returns fewer results | **Not reproduced** — 10/10 hits in every run |
| eslint tsconfig re-queries all 49,142 eligible → 97,793 | **Confirmed exactly**; 4.32 GB vs 1.53 GB largest real project (2.83×) |
| 29,931 facts / 20,539 checker edges | **Confirmed exactly** (17,997 likely + 2,542 possible) |
| Compact projection saves 63% | **Confirmed** — 64.0% measured; graph responses compact 93% (1,358 → 95 bytes/edge) |
| Parallel reads transiently fail to open the DB | **Refuted as concurrency** — 524 concurrent readers, 0 failures; the error reproduces deterministically from a single process when the `-shm` sidecar is absent and the directory is unwritable |
| `pushDocUpdates` 0-return still broadcasts `accepted: true` | **Confirmed, severity raised** — invalid or oversized (>32 MiB) Yjs updates are rejected for persistence yet relayed verbatim to every peer, and the sender is told accepted |
| Latest commit primarily Rust, invisible to search | **Confirmed** (145/177 changed lines; identifiers return bare `no results`) |
| 34,172 distinct hashes embedded; five duplicates | **Refuted** — 32,182 distinct hashes; 1,995 duplicate chunk rows. The wrong numbers trace to a real defect (below) |

## What the prior experiment could not see

### Scouting works on AFFiNE — and the reason matters

The prior run produced zero semantic artifacts, so its assessment covered only
deterministic and retrieval surfaces. Bounded real scouting here: 28 calls,
27 succeeded, **0 refusals** — against n8n's 7/8 refusal rate.

The cause is measurable, not luck: **checker enrichment feeds workflow
candidates on this corpus**. All 7 checker `member_call` targets for the probed
production seeds entered their candidate sets (`SpaceSyncGateway::onReceiveDocUpdate`
carried 6/6, including `SyncSocketAdapter::push` and `buildBroadcastPayload`).
Candidate sets arrived populated (10, 3, 1, 1, 10, 2), so the model had
something honest to classify. The n8n starvation pattern did not recur.

Artifact quality, judged line-by-line against source: workflows and cards are
strong — the queue-worker workflow's 15 support spans all land correctly; the
`JobQueue::add` card cites the exact `encodeURIComponent(jobId)` line for its
URI-encoding invariant. Two systematic weaknesses: one workflow drew a
plausible-but-arbitrary boundary because its seed was a generic `ConfigProvider`
DI token (candidate-set artifact, not a real workflow), and concepts are the
weak phase — 4/4 grounded but ~1/4 useful, because response-field names
(`compatibility`) qualify as vocabulary.

### The doc-role bug reaches further than retrieval

Beyond the filtering and the 0.4 ranking penalty (which the prior analysis did
not mention), the scouting substrate gates on `file.role='production'` in
`structural.rs:2867` and `plan.rs:847,917`. A seed inside one of the 52
misclassified files is dropped **from its own candidate set** — a real `@Cron`
job returned 0 candidates. 433 symbols and 61 exported top-level symbols are
currently unscoutable on AFFiNE. Note also that the prior experiment's entire
retrieval comparison ran with `--file-role production`, so the bug was silently
active in both arms of its reranker measurements.

### New defects neither experiment had caught

1. **`jscout embed` wastes ~6% of the most expensive phase.** `embed.rs:433`
   selects `DISTINCT` over the whole tuple, not the hash: 34,172 rows for
   32,182 distinct hashes → 1,990 embeddings computed and discarded by
   `INSERT OR IGNORE`. This is also the exact origin of the prior report's
   incorrect dedup numbers.
2. **Embeddings are mis-keyed relative to their text.** The embedded text
   includes the path/scope/symbol header, but the key is `(chunk_hash,
   profile_id)`: 892 hashes appear under multiple paths (2,882 chunks), each
   keeping a vector computed from one arbitrary path.
3. **"Read-only" queries are not filesystem-side-effect-free.** The WAL `-shm`
   sidecar is created and written on every read query; a read-write command's
   clean close deletes `-wal`/`-shm`, changing the next reader's conditions.
   This is the true mechanism behind the prior run's transient open failures.
4. **One failed card subject fails the whole command** (rc=1 after 11 of 12
   artifacts had published), and a malformed structured-output submission is
   billed with no retry or repair (1,492 tokens lost to a single-quoted JSON
   fragment).
5. **Expansion budgets can produce relationship-free context packs** — 60 nodes,
   0 edges in 38 KB at raised budgets; defaults stay balanced.
6. **`calls --receiver` cannot express the corpus's two dominant patterns** —
   `client.to(room).emit(...)` and fluent permission chains both collapse to
   `<expr>`. Spans, arguments, and option extraction are otherwise exact,
   including both broadcast sites of the confirmed sync bug.
7. **Campaign D1 (search budget starvation) reproduces on a third corpus** at
   sub-default budgets, and at defaults a 1-hit search inflates 2 KB → 19 KB
   from uncapped attached supports. Shed order still evicts primary hits first.

## Where my conclusions differ from the prior analysis

The prior executive conclusion stands: the phases are independently valuable
and the architecture separation is right. Adjustments:

- **The reranker verdict should be "costly and context-starved", not
  "degrading".** Two of four queries improved, one materially. The fix order
  is: give the reranker the same path/scope/symbol header embeddings get, move
  role filtering before pool construction, *then* judge quality. Opt-in default
  is still correct — for the 16× latency alone.
- **Fix #12 (storage/concurrency) is resolved, not open.** No concurrency bug
  exists at 524 readers. The work item is instead: handle the missing-`-shm` /
  unwritable-directory case gracefully in `open_path_read_only`, and document
  that WAL readers need a writable directory.
- **The doc-role fix is bigger than a classifier patch.** Its acceptance
  checks must include scouting (a seed in a reclassified file regains its own
  candidate set) and ranking (the 0.4 penalty), not just expansion filters.
- **Scouting readiness is better than the n8n campaign suggested** — on a
  corpus where enrichment feeds candidates, the refusal problem largely
  disappears. This strengthens the case for prioritizing the checker-edge
  visibility items (their fixes #8) and de-prioritizes new refusal machinery.

## Priority list (mine)

1. `doc` file-role fix with scouting + ranking acceptance checks (their #1,
   widened).
2. Search shed order + supports cap (campaign D1, now three-corpus confirmed) —
   this precedes compact transport because it is a correctness hole, not a
   size optimization.
3. Compact agent transport (their #4; 64%/93% measured headroom).
4. `embed.rs` DISTINCT-by-hash + an explicit decision on the embedding key
   (content-only text, or path in the key).
5. Reranker context + pre-filtering, then re-measure before any default flip.
6. Tooling-tsconfig ownership (their #3, confirmed 2.83× waste).
7. Coverage disclosure (their #6; Swift outnumbers Rust 573:200 — both
   invisible).
8. Per-subject failure isolation and exit-code semantics for card batches;
   one bounded repair retry for malformed structured output.
9. Read-only open hardening (the real #12).

The AFFiNE sync defect (`accepted: true` on fully-filtered pushes, invalid
updates relayed to peers) is confirmed with a complete evidence chain and is
worth reporting upstream independently of jscout work.
