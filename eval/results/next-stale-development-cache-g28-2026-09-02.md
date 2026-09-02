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
  Treatments: `skill` (naturalistic, three trials), `forced` (jscout mandated,
  shell search forbidden, one trial), `grep` control (one trial).
- Artifacts (outside the repository):
  `~/git/jscout-replay-runs/next-stale-dev-cache-g28-2026-09-02/trial-g28{a,b,c}/`.

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

## Reading

- **Naturalistic adoption on Next.js is 0/3 with the G28 full skill.** None of the
  three `skill` sessions opened `SKILL.md` or called the MCP server; each went
  straight to `rg` and `sed`, the flow the repository's own `AGENTS.md` prescribes
  ("Grep first to find relevant line numbers"), alongside its catalogue of 18
  repository skills. On ai-pipe the same skill was read in 16/16 sessions
  ([g28-live-ai-pipe-2026-09-02.md](g28-live-ai-pipe-2026-09-02.md)); the pre-G28
  skill was read in 4/5 Next.js sessions. The change that separates the two Next.js
  populations is the skill (frontmatter description and body) and the slimmer server
  instructions; the execution model, task, and sandbox are the same as on 2026-08-15.
  Three sessions cannot separate a description that no longer triggers from a
  repository whose instructions crowd out an extra skill, and this note does not try.
- **Outcome does not depend on jscout here.** grep and both passing skill arms fixed
  the task with the same three files in comparable time; the forced arm passed too.
  Trial c's failure came from an incomplete Turbopack change, not from localization.
- **When jscout is used, the G28 surface is much cheaper.** Forced arm: 20 calls and
  70,006 B against 45 calls and 327,763 B on the pre-G28 surface (2.3× fewer calls,
  4.7× fewer bytes, 3.5 KB per call against 7.3 KB), with the taught flow visible
  in the ledger: 13 exhaustive identifier searches, 2 ranked phrase searches,
  3 `definition`, 1 `who_uses`, 1 `documentation_search`, no shell search at all.
  Fresh input tokens were flat (163k against 153k–178k for the shell-only arms).
- **Ranked retrieval cost is the reranker.** The two vector searches spent 154–157 ms
  on the query embedding and 3.8–6.5 s in `bge-reranker-v2-m3` on MPS; exhaustive
  searches returned in 4–13 ms.

This is one task and one seed per arm. It records adoption and surface cost on a
repository with strong native agent instructions; it does not estimate the
correctness effect of the checker or the embeddings.
