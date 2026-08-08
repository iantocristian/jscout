# Two-session memory protocol (SC-2a gate)

The product thesis — *persistent memory makes the second agent session start
warmer* — is the one claim no suite has tested. All localization suites to
date measure single-session retrieval, the mode where grep's zero-setup
advantage is maximal and memory amortization is worth exactly nothing. This
protocol is the SC-2a gate: workflow synthesis and `annotate` write-back ship
beyond experiment status only if they pass it.

## Design

Task **pairs** (A, B) within one repository, related through a shared workflow
or subsystem, where B benefits from understanding built during A but is not
answerable from A's final answer text alone.

- **Session 1** solves task A.
- **Session 2** (fresh agent, no conversational carryover) solves task B.

Two arms, differing only in what session 2 inherits:

| Arm | Session 2 starts with |
|---|---|
| **cold** | structural index only (post-file-roles build) |
| **warm** | structural index + semantic artifacts produced during session 1 (scout workflow records and/or `annotate` write-backs, with their fingerprints) |

In the warm arm, session 1 runs with write-back enabled and is instructed it
may record findings; whatever it stores is what session 2 gets — no manual
curation. Curated-memory quality is a different experiment; this one tests the
honest loop.

## Controls

- Same model/reasoning/CLI pinned across arms; profile order counterbalanced;
  ≥3 trials per pair; task-clustered bootstrap over pairs (reuse
  `eval-pooled-report.mjs` machinery).
- Frozen commit; cold arm gets a byte-identical index with semantic tables
  empty. The session-1 run used for a warm trial is that trial's own — session
  1 cost is recorded and reported, never discarded.
- **Transfer-triviality check** (task admission): B is rejected if its gold
  files+symbols can be produced from A's final answer text by a no-repo,
  no-tools model call. Memory helping through *evidence* is the product;
  answer-key copying is a broken task.
- **Anchor discipline**: B tasks must include workflow-membership questions
  ("which workflows does this code participate in", "what else breaks if this
  contract changes") certified `anchor-free` or `weak` by
  `eval-anchor-certify.mjs` — the L1 suites showed anchored localization is
  grep-saturated; memory must be tested where grep has no handle.
- **Staleness sub-arm** (small, e.g. 2 pairs): between sessions, apply a real
  edit to one workflow participant. Verify session 2 receives the artifact
  visibly `stale`/`degraded`, and that it re-verifies rather than repeating
  the stale claim. A single silent-stale served as fresh fails the gate
  outright, regardless of efficiency results.

## Outcomes

**Primary:** session-2 cost to adjudicated-correct answer — total tokens,
tool calls, wall time — warm minus cold, paired per (pair, trial).

**Secondary:** session-2 correctness parity; artifact usage rate (telemetry:
did session 2 actually retrieve/cite a semantic artifact, or win without it);
combined two-session cost (warm must not merely shift cost into session 1's
write-back overhead).

**Minimum interesting effect (registered):** warm arm shows **≥20% median
session-2 token reduction** at correctness parity, with artifacts actually
retrieved in the winning runs (a win with zero artifact reads is a
confound, not a pass). Below 10%, the memory layer fails its gate and SC-2b/c
do not proceed on efficiency grounds — only a capability result (warm answers
a workflow question cold cannot) would justify continuing.

## Scale

Start: one repository (Twenty or n8n), 6 pairs × 3 trials × 2 arms = 36
session-2 runs plus 18 session-1 runs. Expand to the second repository only if
the first result is directionally positive but under-powered.

## Reporting

Same discipline as the post-cutoff suite: frozen fingerprints for both arms'
databases, execution model recorded per row (the pooled report now refuses
cross-model pooling), artifact contents archived with the run, decision
section states only what the intervals support.
