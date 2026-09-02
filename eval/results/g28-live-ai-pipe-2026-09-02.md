# G28 live validation on ai-pipe — 2026-09-02

- Binary: `jscout` built from PR #116 head (G28 phases 0–2), debug profile
- Agent: Codex CLI 0.147.0, `gpt-5.6-sol`, reasoning `high`, `--ignore-user-config`, read-only sandbox, unrelated tools disabled (the recorded `scripts/eval-run-codex.mjs` protocol)
- Repository: ai-pipe at `ea13166c` (the commit pinned by `eval/tasks/ai-pipe-p0.json` and `ai-pipe-discriminating.json`), scratch local clone; 690 indexed code files, 5,483 chunks, 793 Markdown files
- Adoption mode: naturalistic — no `--require-jscout`; the agent decides whether to use jscout
- Skill: `.agents/skills/jscout/SKILL.md` installed per arm (`core` with the baseline profile, `full` with structural), disabled by Codex config in the no-skill arm; ai-pipe's own project skills present in every arm

## Live matrix, p0 task set (8 gold lookup tasks)

| Arm | Correct | Sessions using jscout | Skill read | jscout calls | jscout bytes | Median task wall |
|---|---|---|---|---|---|---|
| core profile + core skill | 8/8 | 8/8 | 8/8 | 48 (median 4) | 102.7 KB | 66.6 s |
| full profile + full skill | 8/8 | 7/8 | 8/8 | 37 (median 4.5) | 122.4 KB | 60.2 s |
| core profile, skill disabled | 8/8 | **0/8** | 0/8 | 0 | 0 | 46.4 s |
| grep (no MCP server) | 8/8 | — | — | 0 | 0 | 67.6 s |

Tokens (sum over 8 tasks): core+skill 2,319k input (1,908k cached; 411k uncached) / 23.0k output; full+skill 2,493k (2,074k cached; 418k uncached) / 20.6k; no-skill 1,201k (823k cached; 378k uncached) / 15.9k; grep 1,490k (1,098k cached; 391k uncached) / 26.2k.

Reading the p0 matrix:

- **The skill is the adoption mechanism.** Same MCP server, same seven tools, same slim instructions: with the core skill present every session used jscout and read the skill; with the skill disabled, no session called jscout at all. Registration alone is availability, not use — exactly the available/taught/activated split G28 assumes.
- **The taught flow was followed.** Core arm: 17 exhaustive searches versus 6 ranked, the canonical `search → exhaustive → definition → who_uses` shape on every task, zero `broad_or_query` warnings, no overview (unavailable). Full arm: with memory, overview, neighborhood, entities, paths, and annotate all registered, the agent used none of them — search, definition, who_uses, and one outline.
- **Correctness does not discriminate on p0**, and jscout costs more than plain repository search here: the no-skill arm answered 8/8 from shell reads in the least uncached input and least wall time. This reproduces the 2026-08-07 finding on the same repository ("not an outcome win over grep" for lookup tasks). The value question is the discriminating set below.

## Local embeddings (bge-m3 sidecar on MPS)

- `jscout inference serve`: healthy, `BAAI/bge-m3` 1024-d float16 + `bge-reranker-v2-m3`, device `mps`.
- Code: 5,065 unique chunks embedded, 5,483 occurrences synced, 424 s (debug binary). Documentation: 6,400 chunks, 8,297 occurrences, ready, 578 s.
- Vocabulary sensitivity, ranked search for the broker-risk gold: the abstract paraphrase ("deterministic decision that blocks a broker order on safety constraints") misses `riskPolicy.mjs` in the top 10 under both lexical-only and hybrid; the in-domain phrasing ("risk policy evaluate broker order", "broker order risk policy gate") ranks it **#1 under hybrid** and misses it in the top 5 lexical-only. Vectors lift domain-phrased questions, not abstract ones; the exact identifier ranks #1 either way.
- Documentation hybrid search on "how does the trading subsystem block unsafe orders" returns the trading-subsystem doc with the "Risk policy (the deterministic gate)" section at rank 2, vector and reranker active.

## Scouting (openai-codex `gpt-5.6-terra`, reasoning low)

- Gateway doctor: pi-ai 0.84.1, OAuth plan billing, model context 272k.
- `jscout scout repository --max-calls 6`: 6 calls, 6 area classifications published, 2.2–4.1k tokens per call, 29.8 s total; 9 boundaries skipped by the call budget.
- Overview with `--reconnaissance-limit 12` now carries 6 classifications (6,688 B); the default overview stays 4,278 B with no reconnaissance key. Under budgets of 6,000 / 5,000 / 3,500 B the response sheds 2, 5, then all 6 classifications before touching relations or areas — the phase-2 eviction order on real data.

## Discriminating task set (4 gold tasks: registry dispatch, deep barrel indirection, event-receiver disambiguation, cross-file workflow)

Same four tasks and pinned commit as `results/ai-pipe-discriminating-2026-08-07.md` (then `gpt-5.4` low). Three sessions failed on the provider side ("Selected model is at capacity") and were rerun once with `--resume`; no jscout or runner error occurred in any session. Local embeddings were ready for these arms (vector and reranker active when a ranked search asked for them).

| Arm | Correct | Sessions using jscout | Skill read | jscout calls | jscout bytes | Uncached input tokens | Median task wall |
|---|---|---|---|---|---|---|---|
| core profile + core skill (vectors ready) | 3/4 | 4/4 | 4/4 | 42 (18 exhaustive, 2 ranked-vector) | 108.5 KB | 198k | 105 s |
| full profile + full skill (vectors ready) | 3/4 | 4/4 | 4/4 | 41 (13 exhaustive, 2 ranked-vector, 10 ranked-lexical) | 133.0 KB | 230k | 103 s |
| core profile, skill disabled | **4/4** | 0/4 | 0/4 | 0 | 0 | 171k | 75 s |
| grep (no MCP server) | 3/4 | — | — | 0 | 0 | 189k | 119 s |

The single miss in the jscout arms and in the grep arm is the same task and the same error: `http-close-scheduler-receiver`, where the answer text correctly names `startScheduler` from `server/scheduler.mjs` but the final `files` list omits that file — a final-answer discipline error, the class the 2026-08-07 report already identified, not a localization failure. Every jscout arm session used jscout, read the skill, and followed the taught shape (exhaustive first, definitions after, one `events` call for the receiver task); none used memory, overview, neighborhood, entities, paths, or annotate in the full arm.

Reading the discriminating matrix: on this 690-file repository, `gpt-5.6-sol` at high reasoning localizes every gold set from plain shell reads, and the jscout arms cost more uncached input and more wall time for the same or slightly worse exact-set outcome. That repeats the 2026-08-07 conclusion for ai-pipe and does not contradict the 2026-08-24 production result, where the skill-guided arm won on a 7,000-file monorepo: the value of indexed retrieval scales with the cost of shell search, and ai-pipe is small enough that shell search is cheap. G28's claims — the skill drives adoption, the taught flow is followed, the surface is small — are validated here; the value claim is not testable on a repository this size and moves to the larger real-problem tests.

## Claim boundary

Single repository, single model, one trial per task and arm, naturalistic adoption; p0 gold sets are exact file+symbol matches graded automatically; the discriminating set is the same set the 2026-08-07 report used with `gpt-5.4` low. Vector-enabled arms ran only on the discriminating set (embeddings finished after the p0 matrix).
