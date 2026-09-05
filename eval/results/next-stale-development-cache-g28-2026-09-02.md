# G28 surface on the Next.js `next-stale-development-cache` replay — 2026-09-02

- Binary: `jscout` built from PR #116 head (G28 phases 0–2), release profile;
  checker sidecar and inference project supplied through
  `JSCOUT_CHECKER_SIDECAR` / `JSCOUT_INFERENCE_PROJECT`.
- Rig: `scripts/eval-run-replay.mjs` from the same branch (full-tier skill and
  `--profile structural` for every non-baseline profile).
- Task: `eval/tasks/next-stale-dev-cache.json` (`next-stale-development-cache`,
  parent `70f8b678`, reference `286862e3`; task file sha `4d28475a…`, byte-identical
  to the 2026-08-24 campaign). Gold bundle: the 2026-08-15 certification bundle.
- Posture: `checker-embed` = index + checker enrich + full code embedding from the
  local bge-m3 sidecar (MPS). No scouting, no semantic memory, no `docs embed`
  (the docs plane is on by default, so `documentation_search` was registered and
  answered lexically; its vector generation reports `not_ready`).
- Agent: Codex CLI 0.147.0, `gpt-5.6-terra`, reasoning `high`, run timeout 3,600 s.
  Treatments: `skill` (naturalistic, three trials plus one with the pre-G28
  frontmatter description swapped in), `forced` (jscout mandated, shell search
  forbidden, one trial), `grep` control (one trial).
- Artifacts (outside the repository):
  `~/git/jscout-replay-runs/next-stale-dev-cache-g28-2026-09-02/trial-g28{a,b,c,d-olddesc,e-1,e-2}/`.

## Preparation (once; the profile database was reused byte-identically by trials b and c)

| Stage | Result | Wall time |
|---|---|---:|
| index | 23,488 files, 67,944 chunks, 93,001 refs | 8 s |
| checker enrich | 237 projects | ~7 min |
| embed (bge-m3, MPS) | 40,816 code chunks, 57,245 occurrences synced | ~58 min |

Database: 843 MB. Per-arm workspace setup (archive + `pnpm install --frozen-lockfile`
+ `pnpm build`) takes about 70 s.

## Matrix

`Tool bytes` is the sum of jscout MCP `result_bytes`. `Skill read` counts references to
`.agents/skills/jscout/SKILL.md` in the Codex event stream. `Fresh in` excludes cached
input. `Agent time` excludes preparation and grading.

| Trial | Profile | Treatment | Grade | jscout calls | Tool bytes | Skill read | Shell cmds | Fresh in | Cached in | Output | Agent time |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| a | grep | control | pass | 0 | 0 | 0 | 33 | 177,163 | 5,149,440 | 21,772 | 733.5 s |
| a | checker-embed | skill | pass | 0 | 0 | 0 | 32 | 153,148 | 4,040,448 | 23,117 | 640.0 s |
| b | checker-embed | skill | pass | 0 | 0 | 0 | 26 | 177,581 | 4,134,656 | 20,824 | 586.6 s |
| c | checker-embed | skill | fail | 0 | 0 | 0 | 67 | 238,541 | 6,756,096 | 36,594 | 921.1 s |
| d | checker-embed | skill, pre-G28 description on the G28 body | pass | 0 | 0 | 0 | 19 | 130,322 | 2,284,544 | 16,240 | 421.1 s |
| b | checker-embed | forced | pass | 20 | 70,006 | 2 | 31 | 163,236 | 5,091,584 | 20,354 | 625.9 s |

Every arm patched the same three gold files (`work-unit-async-storage.external.ts`,
`hot-reloader-webpack.ts`, `hot-reloader-turbopack.ts`); the remaining 18 gold code
files stay pending adjudication, as in the earlier campaigns. Trial c's failure is the
Turbopack route-handler case (`should update cached data used by a route handler after
editing a file`, expected `bar`, received `foo`) with the webpack suite at 7/7.

## Prior campaigns on the same task

| Campaign | Model | Arm | Grade | jscout calls | Tool bytes | Skill read | Agent time |
|---|---|---|---:|---:|---:|---:|---:|
| 2026-08-15 | terra/high | grep control | pass | 0 | 0 | – | 831.9 s |
| 2026-08-15 | terra/high | checker + embed / skill | pass | 4 | 17,052 | yes | 585.8 s |
| 2026-08-15 | terra/high | checker + embed / forced | pass | 45 | 327,763 | yes | 806.6 s |
| 2026-08-24 | sol/high | grep control ×2 | pass, pass | 0 | 0 | – | 1,147 s, 1,216 s |
| 2026-08-24 | sol/high | checker + scout + embed / skill ×2 | pass, pass | 3, 0 | 12,634, 0 | yes, no | 1,497 s, 1,907 s |
| 2026-08-24 | sol/high | memory + embed / skill ×2 | pass, fail | 3, 3 | 20,091, 15,568 | yes, yes | 1,448 s, 1,417 s |

Those campaigns installed the pre-G28 single skill (12.2 KB) with the pre-G28 tool
surface; four of their five naturalistic skill sessions opened the skill and called
jscout.

## Reading the replay arms

- **Naturalistic adoption was 0/3 on the shipped G28 surface** and stayed 0/1
  with the pre-G28 frontmatter description swapped onto the G28 skill body
  (trial d). No session opened `SKILL.md` or called the server; each went
  straight to `rg` and `sed`, the flow the repository's own `AGENTS.md`
  prescribes ("Grep first to find relevant line numbers") next to its
  catalogue of 18 native skills. The pre-G28 campaigns read the skill in 4/5
  sessions, always as the first command. The populations differ in more than
  the skill (binary, instructions, tool descriptions, run date), so the arms
  alone do not isolate a cause; the diagnosis below does.
- **Outcome does not depend on jscout here.** grep and both passing skill arms
  fixed the task with the same three files in comparable time; the forced arm
  passed too. Trial c's failure came from an incomplete Turbopack change, not
  from localization.
- **When jscout is used, the G28 surface is much cheaper.** Forced arm: 20
  calls and 70,006 B against 45 calls and 327,763 B on the pre-G28 surface
  (2.3× fewer calls, 4.7× fewer bytes, 3.5 KB per call against 7.3 KB), with the
  taught flow visible in the ledger: 13 exhaustive identifier searches,
  2 ranked phrase searches, 3 `definition`, 1 `who_uses`,
  1 `documentation_search`, no shell search at all.
- **Ranked retrieval cost is the reranker.** The two vector searches spent
  154–157 ms on the query embedding and 3.8–6.5 s in `bge-reranker-v2-m3` on
  MPS; exhaustive searches returned in 4–13 ms.

## Diagnosis: what the model sees at its first turn

Measured on the kept trial-d workspace with the same Codex flags the rig uses.

- **The skill is in the catalog.** Asked to echo its skills list, Codex 0.147.0
  returned 26 entries including `jscout` with the installed description and path,
  next to the 18 Next.js skills. Codex's own rule (from its embedded prompt) is
  that a listed skill must be used when "the task clearly matches an available
  skill's description".
- **The MCP tools are present at the first turn.** `jscout mcp` answers
  `initialize` and `tools/list` 20 ms after spawn on the 843 MB database. With a
  wrapper that delays the server by 2 s, a first-action probe still called
  `mcp__jscout__semantic_search` before any shell command; with an 8 s delay
  Codex dropped the server without ever sending `initialize`, with or without
  `startup_timeout_sec`. jscout is far inside that budget.
- **So the decision was the model's, at turn 1, with the tools listed.** What
  the shipped G28 surface offered at that moment: a catalog line "Localize and
  prove code in a jscout-indexed repository…", server instructions whose
  pointer read like an install command (``usage guidance lives in the installed
  jscout skill (`jscout agent-guide --tier full`)``), and one-line tool
  descriptions. Pre-G28, the server instructions were a directive investigation
  loop and the tool descriptions were long and imperative. Nothing in the G28
  surface said "read this file before you search".

## Fix and adoption measurement

Changes on the PR branch after this diagnosis:

- Skill descriptions state the trigger situations: "Use the jscout repository
  index to search code before grep or rg whenever you fix a bug, implement a
  change, or answer a question in this JavaScript or TypeScript project…"
  (pinned by a test; the core tier stays under its byte gate).
- Server instructions carry a directive pointer: "Before the first repository
  search, read the installed jscout skill at `.agents/skills/jscout/SKILL.md`
  … and follow its flows; it is the usage contract for these tools." The
  documentation-routing sentence moved out (it lives in the skill only).
- `semantic_search`'s description says to use it before grep or rg
  (185 B, still a one-liner).
- Both skills open with "Tools here are MCP tools on the jscout server, not
  shell commands" after one session read the skill and ran
  `jscout semantic_search --help` in the shell.

Adoption was measured with short naturalistic sessions: the rig's exact prompt
and Codex flags, `gpt-5.6-terra` high, read-only sandbox, 240 s cap, the same
prepared database, five seeds per variant, on the trial-d workspace. Adoption is
decided within the first commands, so the cap does not truncate it.

| Variant | Skill read | Read as first command | Used jscout | MCP calls (5 sessions) |
|---|---:|---:|---:|---:|
| V0: shipped 7d6ca67 surface | 3/5 | 2/5 | 3/5 | 8 |
| V1: fixed surface | 5/5 | 5/5 | 4/5 | 23 |

V0's 3/5 says the replay arms' 0/3 was partly sampling; adoption on the shipped
surface is a coin flip on this repository. V1 reads the skill first every time;
the one V1 session without MCP calls read the skill, tried `jscout` as a shell
command, and fell back to `rg` within the cap, which the new opening line
addresses.

## Confirmation: full replay arms on the fixed surface

Two naturalistic `checker-embed / skill` arms with the 3b5ae8f binary, same
rig, same reused database, terra/high, graded end to end:

| Trial | Grade | jscout calls | Tool bytes | Skill reads | Shell cmds | Fresh in | Agent time |
|---|---:|---:|---:|---:|---:|---:|---:|
| e-1 | pass | 11 | 60,556 | 3 | 46 | 241,225 | 851.5 s |
| e-2 | pass | 8 | 33,129 | 7 | 36 | 326,865 | 1,462.2 s |

Both sessions read the skill and worked through jscout (`semantic_search`,
`file_outline`, `definition`, `who_uses`) before editing; both passed the
hidden tests. Adoption on this repository went from 0/3 (shipped surface)
to 2/2 (fixed surface) in full replay, matching the short-session
measurement. The local inference sidecar was not running during these two
arms, so their ranked searches report `retrieval_vector: degraded` and were
served lexically; adoption and grade are unaffected, but their ranked-search
quality is not the hybrid path measured in the forced arm.

This is one task and one model. It records adoption and surface cost on a
repository with strong native agent instructions; it does not estimate the
correctness effect of the checker or the embeddings.
