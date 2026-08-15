# Next.js PR replay: stale development cache

Execution: 2026-08-15

Task: `next-stale-development-cache`

Fix commit: `286862e35bbc4fa7c023077cf794d5852063463a`

Parent: `70f8b678877ba69f266e1522fcfacb95cfd3c76e`

Agent: `gpt-5.6-terra`, high reasoning

## Outcome

- The complete matrix contains 13 arms: grep control plus six jscout
  profiles, each under skill-only and forced-search treatments.
- Independent grading passed 10 of 13 arms. Grep passed. Five of six
  skill-only jscout arms passed, versus four of six forced arms.
- The installed skill produced natural jscout adoption in every skill arm:
  4-9 requests per arm. An explicit requirement to use only jscout for
  repository-wide localization was unnecessary and expensive here.
- Forced arms averaged 41.3 jscout requests and 257,488 MCP response bytes,
  versus 6.2 requests and 44,523 bytes for skill-only. Forced use consumed
  81% more cumulative tokens and 40% more agent time, with a lower pass rate.
- The production-order skill arm passed. The production-order forced arm
  passed both new curl/route-handler cases but regressed the existing browser
  HMR behavior, so the independent grade correctly failed it.
- These are one-task, one-seed descriptive results. They do not estimate the
  stable correctness effect of checker, scouting, or embeddings individually.

## Matrix

`Fresh in` excludes cached input. `Cached in` is shown separately because
Codex usage events are cumulative. `Tool bytes` is the sum of jscout MCP
response bytes. `Agent time` excludes profile preparation.

| Profile | Treatment | Grade | jscout calls | Tool bytes | Fresh in | Cached in | Output | Agent time |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| grep | control | pass | 0 | 0 | 205,532 | 7,166,976 | 24,822 | 831.9s |
| structural | skill | fail | 4 | 47,280 | 153,140 | 2,672,896 | 15,155 | 435.1s |
| structural | forced | pass | 48 | 315,342 | 225,952 | 8,464,896 | 17,419 | 594.0s |
| checker | skill | pass | 6 | 31,650 | 185,338 | 3,457,024 | 16,353 | 433.0s |
| checker | forced | pass | 44 | 288,816 | 282,116 | 4,796,672 | 15,085 | 514.5s |
| checker + embed | skill | pass | 4 | 17,052 | 157,014 | 3,687,168 | 16,323 | 585.8s |
| checker + embed | forced | pass | 45 | 327,763 | 213,389 | 6,837,504 | 19,840 | 806.6s |
| checker + scout | skill | pass | 9 | 94,490 | 149,114 | 3,490,816 | 16,412 | 572.5s |
| checker + scout | forced | fail | 27 | 176,569 | 288,226 | 3,433,984 | 14,735 | 420.9s |
| checker + scout + embed | skill | pass | 5 | 27,731 | 170,042 | 4,313,600 | 20,300 | 585.6s |
| checker + scout + embed | forced | pass | 48 | 230,474 | 197,074 | 7,771,392 | 19,352 | 909.9s |
| production order | skill | pass | 9 | 48,937 | 226,837 | 7,432,192 | 24,177 | 821.6s |
| production order | forced | fail | 36 | 205,961 | 265,088 | 14,515,712 | 35,037 | 1,572.1s |

Across the six jscout profiles:

| Mean metric | Skill-only | Forced | Change |
|---|---:|---:|---:|
| Independent passes | 5/6 | 4/6 | -1 arm |
| jscout calls | 6.2 | 41.3 | 6.70x |
| jscout result bytes | 44,523 | 257,488 | 5.78x |
| fresh input tokens | 173,581 | 245,308 | +41.3% |
| cumulative total tokens | 4,367,317 | 7,902,246 | +80.9% |
| agent time | 572.3s | 803.0s | +40.3% |

## Independent failures

- `structural/skill`: behavioral suite failed four of seven tests. The target
  direct-request values remained stale, and another HMR case also failed.
- `checker-scout/forced`: the submitted patch did not build. A cache-key tuple
  contained `string | undefined` where the declared tuple requires `string`.
  The agent had claimed a successful build; the independent grader caught the
  mismatch.
- `production-order/forced`: six of seven Turbopack tests passed, including
  both new route-handler and cookieless-page regressions. It failed the
  existing browser HMR test by returning the edited value where the current
  tab's HMR-key behavior required the original cached value.

## Preparation

Preparation was performed once per profile and copied byte-for-byte between
skill and forced treatments.

| Stage | Result |
|---|---|
| Structural index | 20,873 files; 11 extraction failures; 55,449 chunks; 87,154 references; 7.0s |
| Full checker embedding profile | 39,106 vectors; 783,781,888-byte database |
| Scout + product embedding profile | 12,346 vectors; 495,357,952-byte database |
| Production scout | 373/373 model calls; zero failed or budget-skipped subjects; about 28m37s |
| Production scout usage | 1,093,114 input + 54,684 output = 1,149,334 tokens |
| Production classifications | 240 runtime; 92 test; 40 tooling; 1 unknown |
| Production checker | 583 projects; 76,044 occurrences discovered; 14,808 selected; 1,764 facts; 114 occurrences avoided by the tooling filter; about 22m56s |
| Production product embedding | 12,326 vectors; 494,088,192-byte database; about 17m16s |

The production checker still used tooling fallback for 2,649 occurrences and
reported 1,502 unknown answers plus 17,116 unmapped declarations. Scouting
therefore reduced product embeddings by 68.5% relative to full embedding, but
did not make checker enrichment cheap.

## Interpretation

1. Keep the installed skill as the default integration. It was adopted in all
   six skill arms without a prompt mandate.
2. Do not use the jscout-only forced prompt as a normal product mode. On this
   task it multiplied retrieval and tokens without improving reliability.
3. Product-only embedding after scouting retained its preparation-size
   advantage: 12.3k vectors instead of 39.1k.
4. This run does not identify a winning retrieval stack. Correctness is
   non-monotonic across profiles because there is one task, one agent seed,
   and different generated implementations.
5. Independent builds and behavioral tests remain mandatory. Agent self-report
   was wrong in at least one arm.

## Artifacts

Durable raw artifacts are under:

`~/git/jscout-replay-runs/next-stale-dev-cache-2026-08-15/trial-001/`

They include 13 response rows, telemetry, Codex event streams, exact jscout
request logs, patches, independent grades, setup logs, and prepared databases.
The machine-readable aggregate is
`eval/results/next-stale-development-cache-2026-08-15.json`.
