# Embedded TSR checker integration

Jscout will offer an experimental in-process TypeScript checker selected by
`--experimental-tsr`. The binary will link the compiler and its standard-library
declarations. Selecting it must not launch Node, `tsrust`, a language server, or
a compiler sidecar. The existing TypeScript sidecar remains the default.

The checker phase enriches indexed member-call occurrences with declaration
targets. This change does not turn enrichment into a diagnostic or emit command,
and does not replace Oxc structural indexing.

## Implementation

1. Pin the required `tsr_*` library crates to compiler revision
   `2009f6ec101358966902698f9ac2a7ff71ed57c3`, from `ferrotype/tsr`. Compile them
   into ordinary release binaries, without a local checkout dependency. Use
   `tsr_embed::Session` and `tsr_bundled::BundledFs`, not the CLI or API server.
2. Introduce a backend boundary for capabilities, configuration-only ownership
   planning, member resolution, and input validation. Preserve the existing
   scheduler, package policy, graph anchoring, confidence calculation, staging,
   publication, and carry/resume validation. Pass backend selection explicitly
   through enrich, watch, checker doctor, and repository scouting.
3. Implement project discovery with TSR's configuration parser and resolver.
   Preserve config inheritance, overlapping owners, tooling-owner filtering,
   project references, and deterministic inferred scopes grouped by package and
   compiler family with the existing 150-root limit. Dry-run must not create a
   compiler program. Report invalid configurations instead of inventing options.
   Skip inaccessible/disappearing discovery subdirectories as the sidecar does;
   preserve configuration diagnostics and cancellation.
4. Build one immutable compiler session for each selected project. Resolve the
   exact indexed call, receiver, and property byte spans; reject source drift.
   Keep union candidates, alias resolution, overload implementation/signature
   selection, declaration provenance, and refusal of any/unknown receivers.
   Convert results to owned declaration records before dropping the session.
5. Record the inputs actually observed by configuration and program loading,
   including package manifests and relevant missing paths. Validate input bytes
   before publication. Backend identity, compiler revision, embedded libraries,
   adapter semantics, effective options, and project membership participate in
   reuse identity. Switching backends must invalidate cached checker results.
   Directory observations hash subdirectories and compiler-relevant file names,
   while exact file reads/lookups remain unfiltered. Watch targets are a separate
   policy: visible repository inputs use normal incremental classification;
   ignored/unindexed and external inputs retain explicit coverage. Discard
   ancestor directory probes as event targets, filter unrelated outside events,
   and subscribe to missing directory chains from their nearest existing parent.
   Next.js validation exposed deferred filesystem queries during type printing:
   resolve the complete selected project set once into owned answers before
   staging, so every batch uses the same complete manifest. Preparation has
   128-query cooperative request boundaries; ordinary batch delivery reuses
   those answers. Interrupted preparation restarts for that project.
6. Treat embedded timeouts as cooperative deadlines. Check cancellation and
   deadlines around filesystem operations and query boundaries; discard late
   results and retire failed sessions. Do not abandon running worker threads or
   claim the sidecar's process-kill deadline guarantee. Catch compiler panics at
   the adapter boundary and report failures without publishing partial answers.
   Repeated Ctrl-C retains the process-exit escape hatch.
7. Document the flag, compiler identity, timeout behavior, and remaining parity
   limits. Keep Node requirements for the independent generative gateway clear.

## Validation

- Run focused Rust tests for member resolution, exact Unicode spans, overloads,
  unions, imports/aliases, JSDoc, unknown receivers, embedded libraries, project
  ownership, malformed configs, inferred scopes, stale inputs, backend cache
  isolation, cancellation/deadlines, and failure handling.
- Compare the embedded adapter with the existing Node adapter on shared fixtures.
  Run the embedded CLI with an unusable Node setting/PATH and an invalid sidecar
  setting to establish that it has no checker subprocess dependency.
- Run formatting, the relevant test suites, and Clippy. Existing default-backend
  behavior must continue to pass.
- Test the main `/Users/cristian/git/next.js` checkout at its observed revision
  (initially `0f36a7df88245ed6f1d035f169b69c5a2f4b1b13`). Keep experiment
  databases and captures outside that checkout. Index the actual repository,
  compare configuration-only plans, then run both backends on the same selected
  production occurrences with their real owning configurations and installed
  dependencies. Record commands, selection, coverage, failures, candidate/fact
  differences, elapsed time, and memory where measured. Expand to an uncapped
  production pass if the initial runs expose no blocking correctness issue.
- Exercise exact reuse and changed-input invalidation. Attribute differences
  rather than equating equal fact counts with semantic parity. TSR targets
  TypeScript 7; the current bundled sidecar uses TypeScript 5.9.3, and the
  repository may supply another version.

## Acceptance

The flag must select a genuinely embedded path for every checker operation. The
default remains compatible, exact source/target validation remains enforced,
compiler failures cannot fabricate successful facts, and the Next.js report
must distinguish completed validation from unsupported or unmeasured behavior.
Performance improvements are not assumed from the implementation language.

Completed implementation and main-checkout validation are recorded in
[embedded-tsr-next-results.md](embedded-tsr-next-results.md).
