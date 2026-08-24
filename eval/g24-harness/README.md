# G24 assumption harness

Executable checks for the assumptions in the G24 Markdown retrieval proposal
([docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md](../../docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md)
and the `Proposed G24` entry in [PLAN.md](../../PLAN.md)).

The proposal is prose. This crate turns the parts of it that make checkable
claims into tests that run against **real `git`** and the **real `jscout`
binary**, so a rule that cannot work is discovered before it is implemented
rather than after. Results of the first full run are in
[../results/g24-assumption-harness-2026-08-24.md](../results/g24-assumption-harness-2026-08-24.md).

This is a prototype of the *specified mechanisms*, not a copy of jscout code and
not an implementation of the feature. Nothing here ships in the binary.

## Running

```sh
cd eval/g24-harness
cargo test --tests
```

The `jscout_reality` suite needs a built binary. It is found automatically at
`target/{release,debug}/jscout` relative to the repository root, or set
`JSCOUT_BIN`. Tests that need it skip themselves when it is missing, so the rest
of the suite still runs on a fresh clone.

`git_provenance` needs a reasonably modern `git` (developed against 2.49). Every
git invocation runs under `git::hermetic_env()` with `GIT_CONFIG_GLOBAL` and
`GIT_CONFIG_SYSTEM` pointed at `/dev/null`, so a developer's own `blame.ignoreRevsFile`,
aliases, or signing config cannot leak into a measurement.

## Layout

| Path | Contents |
|---|---|
| `src/md.rs` | Front matter, block parsing with exact byte/line spans, chunking, embedding identity |
| `src/git.rs` | Real-git laboratory: disposable repos, controlled author/committer times, shallow clones, line-porcelain blame parsing |
| `src/proc.rs` | Process runner used to drive the real binary |
| `tests/core_smoke.rs` | Validates the *instrument* — spans round-trip, identities are stable, clones are genuinely shallow |
| `tests/git_provenance.rs` | Author-vs-committer time, shallow boundaries, replace refs, ignore-revs, blame cache-key invalidation |
| `tests/markdown_chunking.rs` | Corpus spec: front matter, spans, embedding-identity blast radius, oversized splits, stubs |
| `tests/history_matching.rs` | The five matching rules, lifecycle events, change flags, the failure gap, one-to-one property tests |
| `tests/freshness_ranking.rs` | The bounded reordering rule, and the superseded multiplicative model it replaced |
| `tests/membership_walk.rs` | Include/exclude/ignore precedence, hidden allowlist, size bound |
| `tests/jscout_reality.rs` | Claims about the *existing* system that motivated the separate database |

## Method

Two rules make the output trustworthy:

**Tests assert what was observed, not what the plan hoped.** Where reality
contradicts the proposal, the test keeps an assertion that pins the *actual*
behavior and a comment naming the divergence. The suite is green and still
documents every contradiction; no test was weakened to make it pass.

**Every contradiction was adversarially adjudicated.** In the first run, 20 of
29 flagged items turned out to be harness bugs — usually the test attributing a
claim the plan never made. Findings in the results document are only those that
survived an independent reproduction.

## Limitations

- Not covered by CI. The root `cargo` steps operate on the root package only,
  and this crate carries its own `[workspace]` table, so it is neither built nor
  linted by the `rust` job. Run it by hand.
- `src/md.rs` records the places the proposal did not decide something as
  `INVENTED:` comments. Those are choices this harness made to be able to run at
  all, not decisions the plan endorses.
- The `jscout_reality` suite only ever writes to temporary directories, and
  never invokes an embedding, model, or network path.
