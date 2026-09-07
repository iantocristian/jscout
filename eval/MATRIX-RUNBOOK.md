# Matrix runbook

Canonical operating procedure for local jscout capability replays, including the
Next.js cases. Start here, not by copying the last campaign's launch command.
Historical reports remain evidence; this file owns the current procedure.

Verified against replay harness `7cb67da`, results commit `838c1ed`, product main
`9947f4d`, Codex CLI **0.153.4**, Node **24.15.0**, and the completed September 5–6
campaigns. Reviewed September 7, 2026. A different binary, harness, dependency tree
or host requires the relevant preflight again. This document does not launch a
matrix, change product defaults, or authorize additional paid attempts.

Navigation: [launch gate](#1-the-operating-rule) · [experiment identity](#2-freeze-the-right-experiment) ·
[permissions](#3-permissions-two-boundaries-not-one) · [setup/config](#4-filesystem-build-and-task-configuration) ·
[browser/tests](#5-browser-generator-and-native-test-preflight) · [inference/DBs](#6-inference-and-database-reuse) ·
[oracle](#7-certify-the-oracle-before-charging-solvers) · [launch](#8-launch-and-observe) ·
[recovery](#9-recovery-without-corrupting-the-experiment) · [cleanup](#10-cleanup-is-a-completion-gate) ·
[reporting](#11-report-what-happened-not-what-the-labels-imply) · [incident index](#12-incident-index-and-evidence).

## 1. The operating rule

**No solver starts until the entire campaign is ready.** Build/setup success,
an HTTP health response, and a prepared-database manifest are individually
insufficient. The gate must cover the actual solver boundary, browser/test path,
inference profile, frozen data, oracle and cleanup mechanism.

The successful workflow has three layers:

| Layer | Responsibility | What actually exists |
| --- | --- | --- |
| Repository harness | Parent export, setup, profile preparation/reuse, guide install, solver dispatch, patch capture, original grade | [eval-run-replay.mjs](../scripts/eval-run-replay.mjs) and sibling scripts |
| Campaign supervisor | Freeze/readiness gates, native permissions, service ownership, whole-stage deadlines, continuation accounting, supplemental scheduling | September 5–6 campaign-local scripts; **not a generic repo command** |
| Operator | Scope/model decisions, preflight evidence, infrastructure recovery approval, treatment audit, final publication | This runbook and a retained campaign record |

Do not claim the repository runner automatically enforces the extra campaign
gates. Do not rerun an old `prepare.mjs`, `run.mjs` or `continue-run.mjs`: they
contain absolute paths, old readiness names, and one-off recovery assumptions.
Use their verified mechanisms when preparing a new, reviewed launch record.

### Launch checklist

- [ ] Exact cases/parents, comparison arms, model/effort, order, budgets and retry
  policy recorded; no ambiguous reference to an older numbered to-do list.
- [ ] Product binary, harness/dependencies/schemas, tasksets, guide, prompts,
  infrastructure patches and configuration frozen independently.
- [ ] Outer native permissions and **nested solver** permissions tested; process
  inspection, loopback, actual browser connect/test/teardown and MPS work.
- [ ] Parent setup includes build; prescribed generator and test commands work
  with the real task-specific package manager, compiler and browser.
- [ ] Parent-negative and reference-positive oracle controls pass admission.
  A negative is the intended assertion failure, not a crash or timeout.
- [ ] Every required prepared DB exists, is closed, matches the frozen hash and
  correct source inventory, and passes profile/freshness/retrieval checks.
- [ ] Intended reuse verified; no unintended scouting or embedding can start.
- [ ] Full stage deadlines and owned-process cleanup tested; disk and wakefulness
  budget sufficient; no competing preparation load during timed solvers.
- [ ] Final readiness record published **last**; expected session list checked.

If any box fails, stop before dispatch. Resolve the failing layer; do not spend
a solver attempt to diagnose the harness.

## 2. Freeze the right experiment

Record these fields in one campaign manifest, with full commits and SHA-256
hashes rather than mutable labels such as `main`, `latest`, or `full scout`:

| Field | Required content |
| --- | --- |
| Identity | Unique campaign/trial IDs, date, owner, absolute artifact/work roots |
| Cases | Task ID, exact parent/reference, story hash, admitted hidden bundle, source inventory |
| Treatment | Data profile, instruction treatment, workflow, order, expected cells |
| Models | Solver model **and effort**; separately scouting model/effort/budgets, or explicit reuse/no generation |
| Product | Product commit, release-binary hash, checker/inference/gateway code and lockfiles |
| Harness | Runner and sibling scripts, schemas, task JSON, guide and rendered prompts |
| Runtime | Node/Codex/Python/package-manager versions, inference fingerprint, browser/TS pins, non-secret env |
| Data | Original and prepared DB hashes, manifest, vector/memory retention and readiness evidence |
| Grading | Public verification instructions, admission/grade commands, supplemental protocol, parent/reference controls |
| Budget | Solver, setup, build, enrichment, scouting, embedding and grading deadlines; concurrency; cold/warm policy |
| Recovery | What may resume, when another model invocation requires approval, immutable evidence policy |

Do not substitute the headers/stale-cache calibration for the later Sol
problem-solving cohorts merely because both are Next.js. The two cases used in
September 6 were:

| Task | Parent | Reference | Retained admitted bundle |
| --- | --- | --- | --- |
| `next-optimistic-prefetch` | `7cb68c12828a758492ea54251393b4f988aecd6e` | `5942b37a42abdcbc7e0f28a087cf41d04ecf08c6` | `next-optimistic-prefetch-2026-08-15/prepared` |
| `next-root-params-types` | `1d8e326d1b360da4a439cf440316fe76a359bfd3` | `46e2114ea28a39aab4c04c2451aa462af14e62df` | `next-root-params-types-2026-08-17/prepared` |

These are not the committed `eval/tasks/next-calibration.json` case. Their
audited task copies live in the external September 6 bundle listed in section 12.
Keep gold/reference code and hidden tests outside every solver workspace.
Infrastructure-only pin patches are separately reviewed and applied uniformly.

For an instruction comparison, hold data constant: `memory-embed` plus
`skill`, `memory`, `efficiency`, `forced`; `grep` produces `control`. Memory
availability is not an instruction treatment. Do not regenerate scouting per
instruction arm. New cases need fresh source preparation/indexing and come last
when the agreed sequence says so.

Always specify model and effort. The current runner defaults to **Terra/high**,
not the last model discussed in the thread. Scouting is a separate setting;
September 5 used Astra/low, while September 6 reused that work. A solver setting
does not select the scouting model. Do not introduce Sol/high scouting by default.

## 3. Permissions: two boundaries, not one

| Process | Required operating boundary |
| --- | --- |
| Outer setup/supervisor | Approved access to the campaign roots, dependency caches, loopback, native process inspection, and owned children |
| Inference service | Native hardware access; verify the **observed** device/dtype, not merely successful startup |
| Browser sidecar | Native Chromium startup; owned loopback endpoint; task-exact Playwright version |
| Solver commands | Explicit `workspace-write` sandbox, unattended bounded execution, frozen task environment |
| Original/supplemental grader | Its own tested runtime and supervision; cannot assume the solver's browser is still alive |
| Publishing | Scoped Git metadata permission; ordinary new commits/pushes only |

In the desktop app, request the narrowly scoped native/escalated execution needed
for the supervisor/service/test step. In a terminal, use an approved native
process. **A nested child cannot undo restrictions inherited from its parent.**
Our `--native` campaign switch only changed log names; it granted no permission.
Do not disable the solver sandbox to fix outer Chromium/MPS/`ps` failures, and do
not evade a denied request with another launch route.

OpenAI distinguishes sandbox capability from approval policy. `never` means no
approval prompts, not unrestricted access. Writable roots can still protect
`.git`, `.agents` and `.codex`; Git worktree metadata can be protected at its
resolved parent location. This explains `index.lock: Operation not permitted`
without implying broken Git. See [official permissions guidance](https://learn.chatgpt.com/docs/agent-approvals-security)
and [sandboxing](https://learn.chatgpt.com/docs/sandboxing).

The **verified harness policy**, not a new global Codex configuration, is:

```text
codex exec --ignore-user-config --ephemeral --skip-git-repo-check
  --sandbox workspace-write --cd <owned-arm-workspace>
  --model <explicit-model> --json --output-schema <schema>
  --output-last-message <artifact>
  -c model_reasoning_effort="<explicit-effort>"
  -c approval_policy="never"
  -c sandbox_workspace_write.network_access=true
  -c features.multi_agent=false
  -c features.apps=false
  -c features.browser_use=false
  -c features.computer_use=false
  -c features.plugins=false
  -c tools.web_search=false
```

Let `codexArgs()` construct the actual argv; the block is an audit checklist, not
a substitute shell invocation. `--ignore-user-config` still uses existing auth;
it is not proof that managed requirements, rules, filesystem reads or shared
caches are isolated. Do not copy credentials or redefine `HOME`/`CODEX_HOME`.

Network capability is on because the historical macOS no-network sandbox also
blocked loopback. External network access is **prompt-restricted and audited,
not technically disabled**. Do not call these runs hermetic. Changing to a
network proxy/allowlist is a separate policy and preflight change, not something
to slip into one arm. Current official controls are documented at the links above.

Native preflight must establish `ps` access **before expensive setup**. Spawn a
disposable owned child and prove observation/teardown with the intended supervisor.
A missing cleanup log is not evidence of no survivors: the repository helper
currently treats a failed process listing as an empty list.

## 4. Filesystem, build and task configuration

Use a fresh, short, owned work root, for example `mktemp -d /private/tmp/jr.XXXXXX`.
Keep durable artifacts outside disposable arm trees. Long nested monorepo paths
have exceeded package-manager filename limits. `/tmp` and `/private/tmp` can name
the same macOS tree; canonicalize before ownership and cross-cell checks.

Suggested artifact layout:

```text
campaign/
  protocol.md                 # scope, order, budgets, amendment policy
  freeze.json                 # hashes of actual inputs
  readiness-final.json         # last gate, references the freeze
  bin/  harness/  tasks/       # frozen executable/configuration copies
  preparation/                # commands, versions, logs, inventory/retention audits
  prepared-databases/<task>/<profile>.db{,.manifest.json}
  services/                   # health, PID/start identity, stdout/stderr, exit
  solvers/<case>/              # invocation, responses, telemetry, per-arm artifacts
  supplemental/               # independently frozen native grader and results
  analysis/                   # reconciliation and manual treatment audit
  RESULTS.md
```

### Build the selected product once

Use a clean detached worktree at the verified main SHA; do not reset or switch a
dirty user checkout. Record `git rev-parse HEAD`, `git status --short`,
`node --version`, `codex --version`, `corepack --version`, and the resolved
executables. Build with `cargo build --release --locked`, then hash/copy that
binary to the campaign. Freeze the checker and inference code/dependencies too.
Do not keep rebuilding a mutable binary path while attempts are running.

Separate product changes from harness changes: the September 6 product was
`9947f4d`, while the extra instruction/timing harness was `7cb67da`.
Check availability of the exact flags in the installed CLI before launch:

```sh
codex exec --help
node --check scripts/eval-run-replay.mjs
node --test scripts/eval-run-replay.test.mjs
node --test scripts/eval-pr-replay.test.mjs
```

These are help/syntax/unit/stub checks, not paid model attempts or certification
of native browser/watch behavior. Freeze the whole runner dependency set, not
just `eval-run-replay.mjs`; it imports siblings and reads schema/preload files.

### Prepare the historical source correctly

1. Export the exact parent, not the current Next.js checkout.
2. Initialize a synthetic Git repository **before install/build/index**. A bare
   archive has different Git-dependent ignore semantics; this admitted generated
   codemod files in our first September 6 prefetch preparation.
3. Apply only declared infrastructure pins; install and build with the task's
   exact setup command. No upstream history/remotes enter the solver workspace.
4. Check path/content inventory against the intended parent plus explicit infra
   differences. Generated files can affect indexing even when tracked Git is clean.
5. Install the current shipped guide using `agent-guide --install ...`, not the
   command that only prints it. Hash it and ensure only indexed arms receive it.

The repository runner already creates the synthetic baseline and installs the
guide. A separate database-preparation script must match those semantics. Local
amendments of that unpublished synthetic baseline are not amendments of a pushed
PR commit. Never amend/force-push published project history without explicit consent.

### Task JSON: settings that matter

```json
{
  "schema_version": 1,
  "suite": "owned-next-replay",
  "setup_command": "<verified install AND build command>",
  "test_command": "<admission/grading fallback command>",
  "grade_test_command": "<frozen hidden oracle command>",
  "browser_server": "required",
  "execution_environment": {
    "NEXT_SKIP_ISOLATE": "1",
    "WATCHPACK_POLLING": "250"
  },
  "tasks": [{ "id": "<id>", "parent": "<full-sha>", "sha": "<reference-sha>", "story": "<behavior-only story>" }]
}
```

This is a field template, not an admitted task. Preserve each historical case's
tested settings rather than applying it wholesale. Important semantics:

- `build_command` is **not executed** by this runner. Compose the build into
  `setup_command`; build changed production source again before grading so tests
  cannot accidentally use stale `dist`.
- Task JSON `profiles`/`treatments` do **not** select this runner's arms; CLI flags do.
- Hidden command precedence is task grade → suite grade → task test → suite test.
  The visible prompt must not expose hidden regression filenames that name the fix.
- `test_command` is **not automatically shown to the solver**. `promptFor()` uses
  `story`, task `execution_notes` and generic test guidance, not that field. Put
  a prescribed safe/public test or generator command in task `execution_notes`
  and verify the rendered prompt. A tested task field is not proof the agent
  received those instructions.
- The admission helper does not automatically reproduce all grade overrides and
  `execution_environment`; certify the **actual final oracle/environment** explicitly.
- Environment values must be strings. `execution_environment` is retained in
  results and passed through command-line configuration: **no secrets here**.
- Browser spawn inherits the **outer runner environment**, not just the merged
  task environment. Export the case's browser cache/PATH there too.
- Record inherited overrides, especially `NEXT_TEST_PKG_PATHS`, `NODE_OPTIONS`,
  `NEXT_*`, `JSCOUT_*` and `GIT_*`. Clear unrelated overrides in the owned grader
  environment and restore only the frozen required values. Do not dump secret env.

### Known Next.js pins and traps

| Setting | Verified use / limitation |
| --- | --- |
| Node `24.15.0` | September 5–6 host/runtime pin |
| Nested pnpm `10.33.0` | Root-param fixture uses a frozen PATH-head shim; top-level Corepack did not stop nested calls falling through to global pnpm 11 |
| TypeScript `6.0.3`, `@types/node` `26.1.0` | Root infrastructure-only pin; TS7 broke historical fixtures. Verify the compiler loaded by the actual fixture |
| `@swc/helpers` `0.5.15` | Repaired root supplemental app dependency; verify resolution from **fixture app root**, not only Next's package directory |
| Playwright `1.58.2` / Chromium headless-shell `1208` | Historical root case; not interchangeable with prefetch's `1.61.0` / `1228` |
| `PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_SKIP_BROWSER_GC=1` | Campaign-local browser cache; set in outer runner as well as case environment |
| `ulimit -n 65536` | Required by solver wrapper; also apply to native stages needing it. Did **not** universally cure EMFILE |
| `NEXT_SKIP_ISOLATE=1` | Committed Next browser tasksets/prefetch avoid nested dependency install; root's historical isolated-fixture oracle has its own pinned path |
| `WATCHPACK_POLLING=250` | Committed Next/prefetch mitigation. Root probes also tried `100`; polling did not establish all live dev behavior as passing |

Pins belong to the case, not global developer configuration. Verify the actual
resolved pnpm/compiler/browser from the fixture context. Missing historical
temporary shims must be recreated as reviewed campaign artifacts, not bypassed
with whatever executable is currently on PATH.

## 5. Browser, generator and native test preflight

Certify **three separate paths**: solver-visible tests, original hidden grade,
and supplemental grade. Their environment/lifecycle is not identical.

1. Resolve Playwright from the prepared workspace, install its exact browser
   revision during approved setup if absent, and launch/close it natively.
2. Start the owned sidecar, then run a representative Next test through the
   **same historical helper and nested solver sandbox boundary**. Prove endpoint
   consumption, assertions and teardown—not just a listening port.
3. If an old helper ignores `NEXT_TEST_BROWSER_WS_ENDPOINT` and calls
   `chromium.launch`, freeze a reviewed infrastructure adaptation or mark that
   route unready. An advertised endpoint alone did not fix root-param tests.
4. Test the actual fixture-generator invocation under solver permissions and
   verify files were created. `pnpm new-test` invoked npm/cache work, hit EPERM,
   and sometimes returned outer exit 0. The installed `node-plop` route worked
   in observed attempts; freeze an exact verified invocation before offering it.
5. Exercise a bounded dev edit/request/teardown if the task requires live changes.
   EMFILE and real route-update failures must not be conflated. Do not certify
   all dev behavior because polling removed one resource error.

For tasksets verified on the prepared dependency tree, the runner teaches:

```sh
HEADLESS=true NEXT_SKIP_ISOLATE=1 NEXT_TEST_MODE=start pnpm testonly <test-path>
```

Use the case's actual command/flags. `node run-tests.js` can replace the supplied
endpoint; historical `test-start-turbo` also hung in `afterAll`/`next.destroy()`.
A passing assertion followed by stuck teardown is not a clean timed run. Avoid
persistent compiler/watch commands such as `pnpm --filter=next dev`; use one-shot
builds and bounded test servers as the task permits.

Browser policy is `required`, `auto`, or `disabled`. `auto` starts only when the
workspace resolves Playwright; launch errors still abort before model dispatch.
`required` is appropriate for an admitted browser-required case, but do not
silently change a historical comparison's policy. September 6 root used `auto`
because its registered oracle was file/type-check based while the available
sidecar also enabled the solver teardown preload/process registry.

That preload is a **bounded compatibility helper**, not general supervision:
it tracks selected Node/package-manager launches and substitutes a descendant
registry for sandbox-denied `pgrep -P`. It does not cover every generator, native
process or arbitrary dev server. The sidecar stops **before original grading**;
the original grader needs its own certified browser path when browser-dependent.

## 6. Inference and database reuse

### Inference: verify what the MCP process will use

Own a separate free loopback port when the existing service's version/profile
does not match. Record PID/start identity, code/env/version, port, logs and exit.
Do not stop or replace a user's existing inference process.

Provision the frozen inference project during approved setup, using its
`pyproject.toml` and `uv.lock` together. From the owned service working directory
(whose configuration is recorded), the supported native-supervised launch is:

```sh
uv sync --project "$EVAL_INFERENCE_PROJECT" --locked
env JSCOUT_INFERENCE_HOST=127.0.0.1 \
  JSCOUT_INFERENCE_PORT="$EVAL_INFERENCE_PORT" \
  JSCOUT_INFERENCE_BATCH_SIZE=16 JSCOUT_INFERENCE_MAX_LENGTH=4096 \
  JSCOUT_EMBED_PROVIDER=local JSCOUT_EMBED_MODEL=BAAI/bge-m3 \
  JSCOUT_EMBED_REVISION=5617a9f61b028005a4858fdac845db406aefb181 \
  JSCOUT_RERANK_MODEL=BAAI/bge-reranker-v2-m3 \
  JSCOUT_RERANK_REVISION=953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e \
  HF_HUB_OFFLINE=1 \
  "$EVAL_BINARY" inference serve --project "$EVAL_INFERENCE_PROJECT"
```

Use the pinned values appropriate to the DB being reused; the example records
September's profile. Validate that the selected port is free first. The service
binds using **HOST/PORT**; client **URL** alone does not change its listener.
Inference commands read the current working directory's configuration. Do not
add a new `.jscout.toml` to the historical Next source just to start the service.

Bundled distributions launch a locked cached environment; an explicit development
`--project` uses normal uv project behavior. Verify the actual Python/dependency
environment and unchanged lockfile before freezing readiness; do not assume a
copied binary brought its runtime with it. September's supervisor instead invoked
an already provisioned, verified Python directly with the frozen `service.py`.
See [inference setup](../docs/inference.md) for provisioning, bundled/development
resolution and model caches. Model downloads, if needed, are approved setup work
before offline launch—not solver work.

The September 6 service used BGE-M3 revision
`5617a9f61b028005a4858fdac845db406aefb181`, reranker-v2-M3 revision
`953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e`, batch 16 and max length 4096.
Observed embedding fingerprint: **MPS, float16, 1024 dimensions, CLS pooling,
normalized**. These are historical reuse constraints, not new universal defaults.
`HF_HUB_OFFLINE=1` works only after model/dependency caches are provisioned.

Check all of these before vector writes or timed solvers:

- Service `/health` including the complete embedding runtime configuration and
  model revisions, against `embedding_profiles.config_json` in the copied DB.
- Actual embedding **and reranking** requests; a healthy embedding endpoint alone
  does not prove the reranker is loaded/usable.
- Actual `tools/list` and task-independent retrieval through the exact launcher
  and MCP configuration the solver receives; record provider/degradation metrics.
- Declared warm-up before timed attempts, or explicitly cold timing. Do not warm
  the service during the first measured arm and call that arm equivalent.

The outer sandbox once selected CPU/float32 instead of MPS/float16. Stop on a
profile mismatch; do not silently accept fallback or create another profile and
continue claiming byte-identical reuse. The old missing-versus-null revision bug
was a product defect subsequently fixed; do not mask compatibility errors blindly.

**Important environment trap:** `JSCOUT_INFERENCE_URL` is not in the current
runner's MCP forwarding allowlist. Setting it only in task/outer env is not
enough. September 6 used an executable campaign launcher of this form:

```sh
#!/bin/sh
export JSCOUT_INFERENCE_URL=http://127.0.0.1:<owned-port>
exec /absolute/campaign/bin/jscout "$@"
```

Replace placeholders before use; hash the launcher and pass it to `--jscout`.
It must write no banner to stdout (MCP uses stdout). Embedded profiles separately
set `JSCOUT_EMBED_PROVIDER=local`. The non-secret MCP selectors forwarded today
are `JSCOUT_PI_AI_GATEWAY`, `JSCOUT_NODE`, `JSCOUT_LLM_MODEL`,
`JSCOUT_LLM_REASONING`, and `JSCOUT_PI_AI_OPENAI_BASE_URL`. Freeze paths/revisions
for checker/inference/gateway helpers too; a frozen Rust executable can otherwise
invoke mutable external helpers. Verify the real MCP connection, not just `curl`
to the service you intended to use.

### Upgrade copies, then certify them

1. Hash the original and match prior authoritative readiness. Quiesce writers;
   make a consistent SQLite backup or clone a closed, self-contained DB.
   Never copy only a live main file and assume its WAL was included.
2. Work on an exclusive new evaluation copy. The runner's file-copy helper
   copies DB/WAL/SHM separately; it is **not a live-backup guarantee**.
3. Rebuild the exact parent environment and source inventory first. With the
   frozen launcher/runtime, the verified upgrade sequence is:

   ```sh
   npm ci --prefix "$EVAL_CHECKER_ROOT"
   env JSCOUT_CHECKER_SIDECAR="$EVAL_CHECKER_ROOT/src/main.mjs" \
     "$EVAL_JSCOUT" checker doctor "$EVAL_PARENT_WORKSPACE"
   "$EVAL_JSCOUT" index "$EVAL_PARENT_WORKSPACE" --database "$EVAL_DB_COPY"
   env JSCOUT_CHECKER_SIDECAR="$EVAL_CHECKER_ROOT/src/main.mjs" \
     "$EVAL_JSCOUT" enrich "$EVAL_PARENT_WORKSPACE" --database "$EVAL_DB_COPY"
   env JSCOUT_EMBED_PROVIDER=local \
     "$EVAL_JSCOUT" embed "$EVAL_PARENT_WORKSPACE" --database "$EVAL_DB_COPY" --product --semantic
   ```

   Variables must resolve to the owned launcher, parent workspace and **copy**.
   `EVAL_CHECKER_ROOT` is the frozen checker tree plus locked dependencies, not
   a mutable unrelated checkout. A copied standalone binary cannot discover it
   automatically; a complete verified release bundle is the alternative.
   The standalone embed command needs its explicit provider—automatic replay
   profile environment does not apply here. See [sidecar setup](../docs/installation.md).
   Indexing does not itself certify checker freshness. Extraction 7→8 required
   reindex/checker work; valid code/memory vectors survived.
4. Compare content identities and vector **BLOBs keyed by text hash/profile**,
   not just counts. Audit semantic artifacts, supports, relations, scouting
   runs, classifications/policies and effective freshness/retrieval. Preserve
   legitimate fresh/degraded/stale distinctions; never force freshness.
5. Explain every source-inventory delta. September 6 deliberately excluded the
   old installed guide and installed the current guide per arm; generated
   codemod files were an error, not a harmless extra corpus.
6. Close writers, checkpoint the **copy** and verify it is self-contained before
   hashing/cloning. `mode=ro&immutable=1` is for sealed no-WAL/no-SHM evidence;
   use normal read-only SQLite semantics for a live database.
7. Record actual reused/new vectors and actual scouting calls, plus unsuccessful
   preparation. September 6 ended with zero new stored vectors, but its failed
   preparations had done extra work; neither fact erases the other.

`embed --product --semantic --repair` is the supported materialization repair
path used in recovery. The campaign's surgical SQL deletion of unintended cache
entries was an audited emergency on backed-up copies, **not the standard recipe**.
Prefer rebuilding a fresh copy from the original after fixing source/runtime
configuration. Do not rerun expensive provider stages to repair a reporting-only
metadata export or extend a read-only audit timeout.

Prepared files are `<prepared-root>/<task-id>/<profile>.db` plus
`.manifest.json`. Built-in manifest validation covers schema/task/parent/profile/
stages—not binary, guide, config, provider, scout budgets, DB bytes or freshness.
The additional hash/readiness gate is mandatory for this workflow.

**Missing prepared DB means the runner may prepare one**, including generative
scouting. Preflight every required file yourself. A matching but old manifest
does not upgrade a database either: prepare and certify copies outside the timed
matrix, then clone the same prepared DB for all treatments.

When new scouting is explicitly in scope, pin its own model, effort, gateway and
budgets. Repository scouting defaults to `all/all`; workflow/card/summary defaults
are 32/64/32 calls. Publication after a partial failure may be accepted by the
runner. Report accepted/incomplete/validation-rejected/provider-failed/cap-skipped
subjects separately; an accepted stage is not complete memory coverage.

## 7. Certify the oracle before charging solvers

Run clean parent-negative and reference-positive controls under the **final**
setup, compiler, browser, environment and full hidden command. Revalidate after
changing a pin, shim, helper, build/test route or execution boundary. Keep their
workspaces/results inaccessible to solvers.

Admission evidence needs command, exit/signal, expected test counts and expected
assertion failures, plus no infrastructure failure markers. An arbitrary nonzero
parent exit is not an admitted negative. A failed positive blocks launch.
Capture full logs externally; the original grade JSON retains only a tail.

Freeze any stronger story-derived supplemental suite before inspecting solver
patches/results. Run it uniformly after the solver matrix, never feed its hidden
findings into later cells. Retain original and supplemental grades separately.
Reference patch agreement and unadjudicated omission metrics are not correctness.

Certify that separate fixture too: assertion/ownership self-tests, native parent
S-state negatives in build/dev/typegen, and an actual dev HTTP-200/fixture-marker
smoke before interpreting saved-patch grades. Original-oracle controls do not
certify a new fixture. September's tiny self-tests loaded TypeScript 6.0.2;
native checks established the task-pinned 6.0.3 and real packaging/runtime path.
Missing or changed retained evidence is an **inconclusive evidence failure**,
not a feature failure or an assumed pass.

For the root-params suite, the verified stronger contract is:

- Build/dev/typegen × single/multiple/parameterless/removed-root states.
- Actual zero-argument namespace getter calls; reject `any`, `unknown`, `never`,
  wrong scalar and wrong optionality. Declaration strings or `ReturnType` alone
  did not catch the permissive ambient placeholder.
- Verify assertion source unchanged **and loaded**, effective strict null checks,
  no `noCheck`, exact compiler and effective symbol/type resolution.
- Let the product establish generated-type inclusion; do not manually inject
  `.next` declarations into the fixture to manufacture success.
- Preserve generated state through S→M→M0→N. Removal needs real prior output;
  deleting `.next` before N makes the test vacuous.
- Build/package each patch's own Next from its exact parent plus frozen infra;
  reject inherited `NEXT_TEST_PKG_PATHS` or shared mutable built packages.
- Dev readiness means HTTP 200 **and the fixture marker** on all lazy routes,
  then stable declaration inventory. An open port or “ready” log is insufficient.
- Retain exact contributing declarations, audited compiler text/hashes and
  absent/neutralized former output. A fresh server per state is **not live
  hot-reload coverage**; inventory stability is observed quiescence, not a proof
  that every possible asynchronous task has completed.

The repaired helper dependency was verified from the owned fixture root with
non-executing module resolution. Its successful fix does not prove the original
loader-context root cause was fully isolated.

## 8. Launch and observe

Only after section 1's gate passes, use explicit arguments to the frozen runner.
Example **one-case five-condition shape** (all paths/IDs set in the campaign
record; this is not authorization to launch another experiment):

```sh
/usr/bin/caffeinate -i node "$EVAL_RUNNER" \
  --tasks "$EVAL_CASE_TASKS" \
  --repository "$EVAL_SOURCE_REPOSITORY" \
  --runs-root "$EVAL_ADMITTED_RUNS_ROOT" \
  --jscout "$EVAL_JSCOUT" \
  --prepared-root "$EVAL_PREPARED_ROOT" \
  --responses "$EVAL_CASE_OUTPUT/responses.jsonl" \
  --telemetry "$EVAL_CASE_OUTPUT/telemetry.jsonl" \
  --artifacts "$EVAL_CASE_OUTPUT/artifacts" \
  --work-root "$EVAL_SHORT_WORK_ROOT" \
  --keep-workspaces true \
  --profiles grep,memory-embed \
  --treatments skill,memory,efficiency,forced \
  --workflow single \
  --model gpt-6-astra --reasoning high \
  --run-timeout 3600 --trial "$EVAL_TRIAL"
```

Run under the approved outer native supervisor with the case environment,
stage deadlines and ownership records. `caffeinate` prevents idle sleep; it is
not a timeout or supervisor. A previous sleeping-host attempt spent two hours
with zero tokens/no patch. Use named, short, realpath-validated roots; never
reuse another campaign's mutable output directory.

`--keep-workspaces true` is intentional for this runbook: it lets the supervisor
verify survivors and retain evidence **before** deleting each completed tree.
The runner's default deletion has no such fail-closed gate. Budget disk for
retained trees and release completed, positively verified trees between cells;
use explicitly ordered one-cell invocations if the supervisor cannot do that
safely during a multi-cell invocation. Do not quietly revert to default deletion.

Important runner behavior:

- All options are `--name value` pairs, including booleans such as
  `--resume true` and `--keep-workspaces true`. Do not invent `--dry-run` or
  assume passing `--help` is harmless: inspect supported parser/source.
- `--run-timeout` is seconds and bounds the solver phase only. Setup/index/
  enrich/scout/embed and original grading use synchronous operations without a
  common stage deadline. Supervise those separately. A unit/e2e `timeout 900`
  does not bound the preceding build. September controls used bounded native
  setup/grade stages; choose and freeze suitable limits, not unbounded waits.
- An open stdin pipe can hang `codex exec`; supervisors use `stdin: ignore`.
- Setup output can be buffered until completion. Quiet logs do not alone prove
  a hang; inspect bounded process/stage status rather than restart speculatively.
- The generic runner alternates profile order by task index, not full condition
  order. September 6 reversed the second case explicitly via a separate
  invocation: `--profiles memory-embed,grep` and
  `--treatments forced,efficiency,memory,skill`.
- Prepared-index serving is **static**: no watcher/reindex of solver edits.
  Targeted reads for changed/new source are necessary, not a retrieval failure.
- Do not overlap preparation, unrelated inference jobs, graders and timed
  solvers when comparing elapsed time. Record unavoidable contention/cold starts.
- Two-phase `design-implement` is a different experiment. It requires its own
  read-only design preflight, budgets and phase accounting; do not pool with
  single-phase results or assume these instructions were tested in both phases.

Watch stage completion, terminal events, stderr, actual requests and owned process
health. Do not intervene with solution hints or hidden-test feedback. No-`rg`
violations or test failures are findings; they do not automatically authorize
discarding an arm or adding a replacement.

## 9. Recovery without corrupting the experiment

| Failure point | Allowed recovery discipline |
| --- | --- |
| Before model invocation | Preserve failed setup/logs, fix the infrastructure, revalidate affected gates, record amendment; no solver result has been replaced |
| After model starts | Preserve events, usage, patch and failure; any new invocation is another attempt, subject to declared retry policy/user approval |
| Solver finishes; original grade fails to execute | Preserve solver result; fix and regrade identically without another solver call; retain both grade records |
| Supplemental infrastructure fails | Stop as inconclusive, retain failed namespace; amend/freeze setup and regrade the full comparable set with the same suite |
| Metadata export/read-only audit fails | Resume reporting/audit only after verification; do not rerun indexing/scouting/embedding to regenerate a report |

`--resume true` is **not retry-failures**:

1. Any session in `responses.jsonl` is skipped, including failed/`runner_error`
   rows. Review rows before deciding what is incomplete.
2. Session identity includes task/profile/treatment/trial/workflow, **not model,
   prompt or binary hashes**. Independently compare the full frozen invocation.
3. Incomplete artifact directories are renamed `.interrupted-NNN`; the arm is
   restarted, not resumed inside the same model context. Preserve paid partial
   attempts even if the runner lacks a completed response row.
4. Hash completed cells before continuation; verify them unchanged afterwards.
   Keep the exact same inputs and only dispatch explicitly remaining cells.
5. Publish amended final readiness last. Never continue against superseded
   `readiness.json` merely because its old `ready` field is true.

September 6 kept the completed control, blocked indexed preparation before its
first model invocation, and resumed the remaining nine after correction. That
was an exceptional documented recovery, not a launch pattern to copy. The
control overlapped repair and therefore was not a contention-controlled baseline.

## 10. Cleanup is a completion gate

Use the supervisor that owns the child identities. Retain PID **and start/birth
time**, private group and descendants; command text can change through `exec`
and PIDs can be recycled. Test short-lived launchers as well as long-lived children.
Use TERM, a bounded grace period, then KILL only still-proven owned survivors.

Never use broad `pkill -f`, kill all Node/Chrome processes, or stop an unowned
inference service. If process observation fails, do not guess ownership or
declare the host clean. Escalate the inspection safely; unknown survivors block
workspace deletion. The repo's argv-prefix cleanup/preload is weaker than the
campaign's native PID/start-time supervisor.

Before deleting a disposable tree:

- Retain solver patches **before hidden-test overlay**, grades, prompts/events,
  requests/telemetry, command exits/signals, source/DB hashes and declaration
  evidence. Retain failed attempts too.
- Validate owner marker, exact realpath and approved temporary-root relationship;
  reject symlink/path escapes. Do not use a repository/workspace root as the
  target of a recursive deletion.
- Verify no owned process remains and original/frozen inputs are unchanged.
- Remove only the validated disposable tree; verify it is gone. Durable evidence
  and source originals remain. Record what was removed and retained.

Stop the owned inference service only after its last consumer/original grade.
An intentional SIGTERM can make a thin wrapper report exit 1; distinguish this
from a failed inference request using the recorded signal and timing.
Temporary worktrees hold branches open: remove only known-clean disposable
worktrees through Git before deleting their branches. `git worktree prune`
removes stale metadata, not active worktree contents. Do not force-remove a dirty
or unverified worktree merely to make branch cleanup succeed.

## 11. Report what happened, not what the labels imply

Minimum per cell: exact configuration; infrastructure status; original and
supplemental grade/counts; solver duration; fresh/cached/output tokens; shell and
jscout calls; provider failures/degradation; actual treatment use; compliance
qualifications; evidence references. Preserve all expected cells, including failures.

### Time and tokens

- Separate build/setup/index/checker/scout/embed, solver attempt, original grade,
  supplemental grade and total campaign wall time. Include failed prep/retries.
- Only final patches are externally graded here. Attempt duration is **not time
  to first correct solution**. Leave that metric unknown rather than infer it.
- Reconcile final/max cumulative usage once per phase; fresh = input − cached.
  Do not sum repeated cumulative events. Output includes reasoning; do not add
  reasoning again without a provider contract establishing that it is separate.
- Preparation ledger accounting is separate: logical calls vs attempts/retries,
  reused vs newly generated work, incomplete/failed subjects, missing retry usage.
  Do not sum inherited profile ledgers and gateway totals as independent costs.
- Host arrival timestamps are not backend execution times. Per-purpose tool
  intervals can overlap. Tokens/catalog estimates are not a Codex-plan cash bill.

### Actual treatment and delivery

- Memory available → relevant inquiry → returned artifact → body read → source
  verification → correct patch are **separate observations**. Empty supported
  memory may reflect an existing coverage gap; inspect original vs upgraded DB
  before blaming the prompt or upgrade. Do not scout from hidden solution clues.
- Audit commands, not just an `rg` regex. Separate authored-repo discovery,
  log filtering, generated-output inspection, editing, builds and tests. A shell
  tool item is a batch, not one subprocess.
- No-`rg` can avoid scans while still guessing direct paths. Record literal
  returned-path deviations separately from allowed targeted reads, test logs,
  changed source and context-dependent fixture navigation. Do not silently
  discard/rescore a trial or retroactively relax its contract.
- Count one canonical jscout body. Search's `canonical_rendered_bytes` can be
  absent/null on other tools; use serialized `result_bytes` there, not zero.
  Structured/text wire duplication is not proof of duplicate model context.
- Missing/empty captured aggregate is missing evidence, not zero actual output.
  Never reconstruct an intended read and label it observed delivery. Complete
  model-input traces were not retained by the ephemeral runs.
- Baseline source fingerprinting gives conservative repetition lower bounds,
  not total delivery or wasted tokens. Caller verification and definition reads
  can answer different questions; extra testing is not all retrieval overhead.
- Inspect broad-OR/path-shaped queries, truncation and failed navigation, but
  keep retrieval defects separate from fixture/browser failures. Product fixes
  need their own verification and an explicit request to implement them.

One attempt per condition is descriptive. Cross-date Sol/Terra/Astra comparisons
also changed product, guide, data, setup and ordering. A later binary returning
complete definitions proves that behavior, not a solver-level causal improvement.
Do not change product defaults, acceptance criteria or evaluation scope implicitly
in a results summary; propose such decisions to the user.

Publish a human report plus normalized JSON/evidence hashes in `eval/results/`,
link them from `eval/README.md`, and retain raw external artifacts. Run accounting,
hash, link, secret-pattern and `git diff --check` validation. New commits and
normal pushes only. Update this runbook when another operational gotcha is
verified; put run-specific deviations in the campaign record.

## 12. Incident index and evidence

The checklist above consolidates these recurring incidents, rather than treating
each as a new solver failure:

| Incident | Operational lesson | Section |
| --- | --- | --- |
| Wrong historical cohort/model/scouting effort | Freeze case and model identities explicitly; runner defaults are not conversation memory | 2, 8 |
| Chromium/Mach-port/SIGTRAP, loopback denial | Native outer services plus tested sandboxed client path | 3, 5 |
| CPU/float32 selected instead of MPS/float16 | Compare actual complete inference profile before vector writes | 6 |
| Browser exists but wrong revision/helper ignores endpoint | Pin per case and exercise actual helper/teardown | 4, 5 |
| Generator EPERM masked by outer exit 0 | Verify files/output, preflight installed-only route | 5 |
| Global pnpm 11 / missing temporary shim / TS7 fixture crash | Pin nested resolution and actual compiler, not just top-level install | 4 |
| EMFILE despite raised limit; route tests still fail under polling | Resource mitigation does not establish behavior | 5 |
| `afterAll`/destroy hang; host sleeps through attempt | Full lifecycle smoke, owned supervision, wakefulness and deadlines | 5, 8 |
| Separate `build_command` ignored; stale compiled output | Compose setup/build and rebuild patched source before grade | 4, 7 |
| Hidden override ignored / task command not actually in prompt | Verify precedence, safe execution notes and rendered prompt | 4, 7 |
| `@swc/helpers` resolves from Next but not app | Certify native fixture-root resolution, preserve inconclusive original | 4, 7, 9 |
| Original grade passes `any`-masked declarations | Real consumer typing, loaded strict assertions, lifecycle state | 7 |
| Archive without Git admits generated code | Initialize synthetic Git before setup/index; hash inventory | 4, 6 |
| Wrong endpoint only exported to outer shell | Freeze executable MCP launcher and smoke real MCP wiring | 6 |
| Reuse manifest accepted but profile/config stale | Independent closed-DB and full-input readiness gates | 6, 9 |
| Reporter/audit fails after successful expensive prep | Retry only reporting/read-only verification | 6, 9 |
| Partial scout publication mistaken for full knowledge | Coverage/freshness/use are explicit measurements | 6, 11 |
| Resume skips failures or silently mixes different model | Session rows and frozen invocation audited before continuation | 9 |
| Process listing denied means apparent empty cleanup | Native identity-based survivor verification | 3, 10 |
| Shared fixed `/tmp` logs, unowned cleanup, package contamination | Per-cell paths, environment hygiene and ownership records | 4, 7, 10 |
| Tool-only time, cumulative/wire double-counting, missing capture | Reconcile observed data and retain unknowns | 11 |
| Memory/no-rg label mistaken for actual treatment | Audit requests, bodies, source checks and commands | 11 |

The thread also found **product** defects while running these matrices:
incomplete split definitions, unstable ranking ties, missing/null revision
compatibility, lost identifier-form default-export edges and seed-order-dependent
workflow deduplication. Their fixes/rechecks are recorded in the linked results.
Keep them separate from harness failures; freeze a build containing the intended
fixes instead of compensating with undisclosed instructions or manual DB edits.

Repository evidence and entry points:

- [Replay runner](../scripts/eval-run-replay.mjs), [tests](../scripts/eval-run-replay.test.mjs),
  [Next teardown helper](../scripts/eval-next-teardown-preload.cjs).
- [Admission helper](../scripts/eval-pr-prepare.mjs),
  [original grader](../scripts/eval-pr-grade.mjs), [replay tests](../scripts/eval-pr-replay.test.mjs).
- [September 5 report](results/next-astra-full-scout-2026-09-05.md) and
  [September 6 report](results/next-astra-instructions-2026-09-06.md), with hashed machine evidence.
- [Fixed September 6 design](prereg/next-astra-instructions-2026-09-06.md).
- [Current stale-cache task](tasks/next-stale-dev-cache.json) and
  [headers calibration task](tasks/next-calibration.json): different tasks, not
  replacements for the later problem-solving cases.

Local historical evidence under `/Users/cristian/git/jscout-replay-runs/`:

- `next-main-snippets-2026-09-05/`: `protocol.md`, `compliance-notes.md`, runtime
  compatibility/definition/ranking audits; includes the older-service recovery.
- `next-astra-full-scout-2026-09-05/`: `harness-notes.md`, `prepare-audit.md`,
  `oracle-controls/README.md`, `analysis/shared-substrate-integrity.md`,
  `supplemental-repaired/README.md`, root/prefetch reviews and retained tasksets.
- `next-astra-instructions-2026-09-06.zIALfv/`: `RUN-NOTES.md`,
  `readiness-final.json`, `freeze.json`, launch/amendment/continuation records,
  tasksets, `supplemental/`, final input/typing/definition/treatment audits.

Older opening notes may say “not executed” or retain the first timeout; use the
final status/hash-bearing record and its amendments, not the first paragraph.
The September 6 `readiness-final.json` supersedes initial readiness. The external
scripts are retained forensic/reference implementations, not portable commands.

### Still manual or not universally solved

No claim is made that this documentation implements a generic supervisor,
full-input resume gate, historical-browser adaptation, reliable generator wrapper,
universal live-watch fix, hermetic host isolation or first-correct-patch timing.
These remain explicit preflight/reporting obligations. A run is ready only when
its actual path passes them; otherwise record the blocker instead of rediscovering
it repeatedly inside paid solver attempts.
