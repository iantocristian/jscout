# Next.js PR-replay evaluation plan

Date: 2026-08-15
Status: protocol and candidate slate; this document does not report evaluation results

## Purpose

Use real Next.js bugs and features as history-free implementation tasks for
comparing ordinary repository search with progressively enriched jscout
profiles. The agent receives the repository immediately before the real fix,
but cannot inspect the upstream history, issue, pull request, gold patch, or
hidden regression tests.

The unit under test is the agent's ability to localize, implement, and verify a
behavioral change. Matching the original patch is not required when an
alternative implementation satisfies the admitted behavioral oracle.

## Completed calibration coverage

The initial harness calibration used the **`headers()` detached snapshot** bug:

- Task id: `next-live-request-headers`
- Source: [issue #97049](https://github.com/vercel/next.js/issues/97049)
  and [fix #97166](https://github.com/vercel/next.js/pull/97166)
- Parent snapshot: `ac65c6b27c53df92c814b95326e2cfba7bc57a82`
- Reference fix: `3c97df56ead9d1df81b36f891ba5ac0724c4eec0`
- Behavioral problem: a read-only request-header view became detached from the
  mutable incoming request; the implementation had to remain live and sealed
  while hiding framework-internal headers and preserving cookie snapshot
  behavior.
- Execution model: `gpt-5.6-terra`, high reasoning
- Executed arms: grep control plus skill and forced treatments for structural,
  checker, checker + embedding, checker + scout, and checker + scout +
  embedding profiles: 11 arms in total.

The task definition is in
[`eval/tasks/next-calibration.json`](../eval/tasks/next-calibration.json). The
outcomes and raw-artifact location are intentionally kept in the separate
[`eval/results/next-calibration-live-headers-2026-08-14.md`](../eval/results/next-calibration-live-headers-2026-08-14.md)
report. No calibration outcome is repeated or treated as evidence in this
plan.

The headers task remains a harness-calibration case. It should not be reused as
the primary retrieval-value task.

## Isolation contract

### 1. Source snapshot

For each task, export the repository at the commit immediately before the
reference fix:

```bash
git -C ~/git/next.js archive <base-sha> | tar -x -C <case>/repo
```

The exported tree must not contain the upstream `.git` directory. Initialize a
synthetic repository inside it and reduce it to one baseline commit. A normal
worktree is insufficient: `git log --all`, commit messages, refs, and
`git show` can reveal the eventual fix.

Keep all of the following outside the agent sandbox:

- the complete Next.js clone;
- the original issue and pull request;
- reference commits and patches;
- regression tests introduced by the fix;
- the expected behavioral contract, admission records, and scoring data.

### 2. Preparation

Preparation is separate from agent execution:

1. Export the parent snapshot.
2. Run `corepack pnpm install --frozen-lockfile`.
3. Build the historical parent.
4. Prove that the hidden behavioral test fails on the parent.
5. Apply the reference production patch to a separate clone.
6. Prove that the same hidden test passes on the reference implementation.
7. Prepare the required jscout profile databases.

Every execution arm still starts from an independent copy-on-write clone of
the prepared source tree. This keeps source, installed dependencies, and build
outputs equivalent without rerunning a multi-gigabyte installation for every
agent.

The [Next.js testing guide](https://github.com/vercel/next.js/blob/canary/contributing/core/testing.md)
recommends building first and then using targeted modes such as
`test-dev-turbo`, `test-start-turbo`, and `testonly`.

### 3. Agent execution

During the agent turn:

- disable network access entirely; dependency installation belongs to
  preparation, not execution;
- provide only the synthetic repository and the tools declared by the arm;
- install the shipped jscout skill in every jscout arm and verify its hash
  after execution;
- do not provide prior conversations, task memory, issue text beyond the
  admitted story, or artifacts from another task or trial;
- record the exact arguments and response size for every jscout MCP request;
- retain the complete Codex event stream, command log, token accounting, and
  patch.

The current replay runner still permits package-registry network access during
the agent turn. Removing that access after preparation is a required harness
change, not an already-enforced property.

### 4. Source and database independence

Each arm gets:

- a fresh source-tree clone;
- an independent writable jscout database;
- an independent agent conversation and event stream;
- its own output and grading directories.

No mutable database or scout output is shared between arms. Within one
profile's `skill`/`forced` treatment pair, both databases should be cloned from
the same immutable prepared database. This deliberately holds checker facts,
embeddings, scout classifications, and model variance constant while leaving
each execution database independently writable.

Prepared databases must never be shared across tasks, snapshots, profiles, or
trials. Scout classifications and other semantic overlays are valid only for
the snapshot and preparation profile that produced them.

### 5. Grading

After the agent finishes:

1. Save its patch and final structured response.
2. Apply the patch to another clean clone of the prepared parent.
3. Overlay the admitted hidden regression tests.
4. Rebuild the submitted source when the target requires it.
5. Run only the relevant Next.js test targets.
6. Blindly adjudicate any reference-file omission before scoring it as a
   defect; a different implementation may satisfy the same contract.

Broad test suites and watch processes are not part of the default oracle. They
may be added only when the task cannot be graded with a bounded target, and
their output must be captured outside agent context or byte-capped.

## Task-admission requirements

A task is admitted only when all of these hold:

- The behavioral story omits issue/PR numbers, filenames, symbols, test names,
  and reference implementation details.
- The same execution model, with no tools or repository, cannot identify the
  implementation location from memory.
- The anchor certifier does not classify the story as identifier-anchored.
- The base snapshot installs and builds successfully.
- The hidden test fails on the base snapshot and passes with the reference
  production patch.
- The hidden test checks externally meaningful behavior rather than the
  reference patch's private API shape.
- The change arc is closed: semantically required follow-up fixes are either
  included in the reference scope or the task is rejected.
- The expected test command is bounded and deterministic enough to rerun in
  every arm.

## Evaluation matrix

`skill` is the product treatment: the jscout skill is installed, but the prompt
does not force tool use. `forced` is a separate capability/stress treatment:
the agent must use jscout for repository-wide discovery, but may directly read
localized files and may edit, build, and test normally. The treatments must
never be pooled.

| Profile | Prepared data | Treatments | Purpose |
|---|---|---|---|
| `grep` | none | `control` | Shell/filesystem-search baseline; no jscout MCP server. |
| `structural` | deterministic index and graph | `skill`, `forced` | Value of parser-derived retrieval and graph surfaces. |
| `checker` | structural + checker enrichment | `skill`, `forced` | Marginal value of TypeScript receiver/type resolution. |
| `checker-embed` | checker + full local embeddings/reranking | `skill`, `forced` | Marginal value of vector retrieval without scout filtering. |
| `checker-scout` | checker + repository scout overlay | `skill`, `forced` | Controlled additive scout comparison using the same checker facts. |
| `checker-scout-embed` | checker + scout + product-only embeddings | `skill`, `forced` | Controlled additive combined profile used in calibration. |
| `production-order` | structural + scout + checker + product-only embeddings | `skill`, `forced` | Operational ordering in which scouting can exclude tooling before checker and embedding work. Runner support is pending. |

The controlled additive profiles continue to enrich before scouting so they
can reuse the same checker fact set and isolate the scout overlay. The new
`production-order` profile is separate because changing the order changes both
the indexed corpus and preparation cost; it must not silently replace or be
pooled with `checker-scout-embed`.

Run at least two independent trials per task and counterbalance arm order. Use
one execution model and reasoning level within a comparison. Cross-model
pooling remains prohibited unless explicitly requested and labelled.

## Calibration-driven protocol changes

These changes apply to future Next.js tasks. They describe the evaluation
design, not calibration results.

1. **Treat the installed skill as the default product surface.** Keep forced
   jscout use as a separately labelled stress/capability arm.
2. **Retain both treatments.** Skill and forced runs answer different
   questions and must remain separate in reports.
3. **Add a production-order profile.** Measure `index -> scout -> checker ->
   product embed` separately from the additive `index -> checker -> scout ->
   product embed` profile used for controlled overlay comparisons.
4. **Clone prepared profile databases.** Prepare an expensive profile once,
   then give skill and forced arms byte-identical independent clones.
5. **Measure complete context pressure.** Record cumulative MCP response bytes,
   shell-command output bytes, the largest command output, and agent token
   usage. MCP bytes alone do not describe the context consumed by the run.
6. **Retain exact MCP request logs.** Every jscout arm must produce
   `jscout-requests.jsonl`, including tool name and arguments.
7. **Use bounded output surfaces.** Future runs use compact `definition` and
   grouped `who_uses` transport; full source remains explicit rather than the
   default.
8. **Include the JSX-in-`.js` parser correction.** The coverage fix is part of
   the common jscout baseline for future Next.js runs, not a treatment.
9. **Disable execution-time network access.** Installation and the initial
   build happen before the agent starts. This runner hardening is still
   pending.
10. **Use harder tasks and multiple trials.** The headers task calibrates the
    harness; it does not replace discriminating implementation tasks.

## Recorded metrics and artifacts

For every arm, retain:

- hidden-test and targeted-build status;
- the agent patch and final response;
- blindly adjudicated behavioral omissions;
- model, reasoning level, trial, profile, treatment, and execution order;
- input, cached-input, output, reasoning-output, and total tokens;
- wall time;
- jscout call count, failures, latency, and cumulative response bytes;
- exact jscout tool names and arguments;
- shell-command count, failures, cumulative output bytes, and largest output;
- agent-inspected files when recoverable from authoritative event artifacts;
- prepared-profile identity and database fingerprint.

Result reports must distinguish correctness, localization behavior, agent
cost, MCP payload, shell payload, and offline preparation cost. Do not combine
them into one score.

## Candidate task slate

| Priority | Candidate | Base | Reference | Scope | Status |
|---:|---|---|---|---|---|
| Calibration | `headers()` live sealed view | `ac65c6b` | `3c97df5` | 8 files | Already run as the initial 11-arm calibration. |
| 1 | Stale development cache for curl and route handlers | `70f8b67` | `286862e` | 26 files, roughly 700 changed lines | Recommended first hard implementation task. |
| 2 | Optimistic routing infinite prefetch loop | `7cb68c1` | `5942b37` | 26 files, roughly 900 additions | Hardest implementation task in this slate. |
| 3 | Server Actions on dynamic PPR fallback routes | `5e8f31f` | `1ab0f1a` | 13 files | Architecture and workflow-localization task. |
| 4 | Middleware Node request-stream hang | `a37068f` | `3bb780e` | 6 files | Small fix with a symptom remote from its cause. |
| 5 | Tag invalidation poisons later cache entries | `6f2db21` | `5da1c1a` | 12 files | Time-aware cache-semantics task. |
| Feature | Generate root-layout parameter types | `1d8e326` | `46e2114` | 14 files | Implementation-oriented feature task. |

### 1. Stale development cache for curl and route handlers

[Regression tests #96021](https://github.com/vercel/next.js/pull/96021) ·
[fix #96022](https://github.com/vercel/next.js/pull/96022)

- Base: `70f8b678877ba69f266e1522fcfacb95cfd3c76e`
- Reference: `286862e35bbc4fa7c023077cf794d5852063463a`
- Size: 26 files, roughly 700 changed lines
- Surfaces: browser HMR state, Webpack stats, Turbopack updates, request
  metadata, request/work stores, route handlers, and the cache wrapper

Anchor-free story:

> In development, cached server computations update after an edit for an
> existing browser tab, but remain stale for curl, fresh clients, and route
> handlers. Fix this for both bundlers without invalidating when no source
> content changed.

### 2. Optimistic routing causes an infinite prefetch loop

[Issue #97135](https://github.com/vercel/next.js/issues/97135) ·
[fix #97128](https://github.com/vercel/next.js/pull/97128)

- Base: `7cb68c12828a758492ea54251393b4f988aecd6e`
- Reference: `5942b37a42abdcbc7e0f28a087cf41d04ecf08c6`
- Size: 26 files, approximately 900 additions
- Surfaces: proxy rewrites, catch-all and parallel routes, route-tree
  validation, segment-cache decoding, and optimistic retry behavior

Anchor-free story:

> A production app with URL-rewriting middleware and a dynamic catch-all route
> continuously issues prefetch requests. A root parallel slot with a catch-all
> beside another dynamic segment can trigger the same behavior. Make
> optimistic routing terminate safely without breaking normal prefetching.

### 3. `headers()` returns a detached snapshot

[Issue #97049](https://github.com/vercel/next.js/issues/97049) ·
[fix #97166](https://github.com/vercel/next.js/pull/97166)

- Base: `ac65c6b27c53df92c814b95326e2cfba7bc57a82`
- Reference: `3c97df56ead9d1df81b36f891ba5ac0724c4eec0`
- Size: 8 files
- Status: completed calibration task
- Surfaces: request stores, live sealed header views, internal-header filtering,
  and proxy callback behavior

Calibration story:

> A request hook obtains a read-only header view, mutates the incoming request,
> then reads again. Both old and newly obtained views remain stale even though
> the request changed. Preserve a live sealed view while keeping internal
> framework headers invisible.

The executed task also required existing cookie snapshot behavior to remain
unchanged.

### 4. Server Actions on dynamic PPR fallback routes

[Fix #96932](https://github.com/vercel/next.js/pull/96932)

- Base: `5e8f31f7bdf7f564ec98a42e205f7e5b665398da`
- Reference: `1ab0f1ada438a699fc9a64818a2726268af5bfa2`
- Size: 13 files
- Surfaces: action handling, app rendering, postponed-state parsing, and
  Resume Data Cache state

Anchor-free story:

> An action request reaches a parameterized fallback route without concrete
> parameters while postponed deployment state is supplied. The action must
> execute and retain cached reads, but the fallback route must not render on
> either success or failure.

### 5. Middleware causes Node request streams to hang

[Fix #95607](https://github.com/vercel/next.js/pull/95607)

- Base: `a37068f525c86c79f1e973cd6712b8c6d0560cad`
- Reference: `3bb780e7d65f723297c93640d0ca24c730037770`
- Size: 6 files
- Surfaces: request cloning, replayable request bodies, Node stream state, and
  `Readable.toWeb()`

Anchor-free story:

> A body-bearing request that passes through middleware hangs when the route
> converts its incoming body into a Web stream. It succeeds without
> middleware. Preserve request replayability without making the downstream
> request appear writable.

### 6. Tag invalidation poisons subsequent cache entries

[Fix #96726](https://github.com/vercel/next.js/pull/96726)

- Base: `6f2db21c74533dc839a5019c451f19ed7f197a88`
- Reference: `5da1c1ae03d2ee39c27a2d6d8807c573c46b37f9`
- Size: 12 files
- Surfaces: tag invalidation, request-local cache reuse, and time-aware cache
  fill validity

Anchor-free story:

> After invalidating a tag, two sequential reads of the same cache during the
> follow-up render both recompute and return different values. Entries created
> after invalidation should be reusable, while fills begun before invalidation
> must remain stale.

### Feature: generate project-specific root-layout parameter types

[Feature #91019](https://github.com/vercel/next.js/pull/91019)

- Base: `1d8e326d1b360da4a439cf440316fe76a359bfd3`
- Reference: `46e2114ea28a39aab4c04c2451aa462af14e62df`
- Size: 14 files
- Surfaces: route analysis, build, development, explicit CLI type generation,
  declaration output, union shapes, optional parameters, and stale-output
  cleanup

Anchor-free story:

> Generate project-specific declarations for root-layout parameters during
> build, development, and explicit type generation. Multiple root layouts must
> union their shapes and mark parameters optional when absent from some roots.
> Remove stale output when there are no parameters.

## Architecture-only tasks

These are not fair patch-completion tasks. Use them to measure repository
understanding, workflow reconstruction, and evidence quality:

- [Staged static/dynamic cached navigation #90223](https://github.com/vercel/next.js/pull/90223):
  reconstruct the flow from server render through Resume Data Cache and RSC
  byte truncation into the client segment cache.
- [Component-level error recovery API #89688](https://github.com/vercel/next.js/pull/89688):
  identify every public export, environment-specific entrypoint, compiler
  alias, RSC restriction, Pages/App Router integration, and Rust import-map
  change needed to introduce the API.
- [Route discovery consolidation #88971](https://github.com/vercel/next.js/pull/88971):
  map the duplicated route-discovery pipeline across build, analysis, type
  generation, development, and MCP surfaces.

Do not mix architecture-answer scoring with implementation-task pass/fail.

## Next execution sequence

1. Enforce no network during the agent turn.
2. Add the separate production-order preparation profile.
3. Prepare and admit the stale-development-cache task, including its bounded
   Webpack and Turbopack oracles.
4. Run the complete matrix with at least two counterbalanced trials.
5. Select the next task based on uncovered subsystem diversity, not on which
   arm performed best in the first hard task.
