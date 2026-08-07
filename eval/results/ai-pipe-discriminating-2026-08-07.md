# Discriminating three-arm evaluation — ai-pipe

Date: 2026-08-07

## Decision

Do not broaden the structural graph or tune standalone `neighborhood` based on
this run. Structural jscout and repository-local grep both answered all four
tasks with exact gold file and symbol sets. Structural retrieval reduced direct
file inspection but required substantially more agent tokens and slightly more
wall time. That is not an outcome win over grep.

Baseline's two exact-set failures were final-answer discipline errors, not
localization failures: it inspected `server/scheduler.mjs` but omitted it from
one final file list, and it added `server/xPostsPoller.mjs` to another list even
though that file defines none of the requested path symbols. Therefore the
4/4 structural versus 2/4 baseline result is not evidence that graph expansion
caused a capability gain.

Freeze graph-surface scope. The next product experiment is bounded SC-2a
workflow memory/write-back. In parallel, retrieval work should target
production-path filtering and tool/prompt economy; expanded searches still
seeded on tests and build scripts when the question explicitly requested
production code.

## Corpus and protocol

- Repository: `ai-pipe`
- Frozen source commit: `ea13166c59cfc52574e96959413f5c54be20e8c8`
- Indexed JS/TS files: 690
- Chunks: 5,483
- References: 28,527
- jscout snapshot:
  `93fc29f5b7260c604d0e6d426420437921267ddda7f5eb5e8ebadbf8da2a4b86`
- Agent: Codex CLI `0.141.0`, `gpt-5.4`, low reasoning
- Tasks: registry dispatch, barrel resolution, receiver-disambiguated event
  wiring, and a cross-file durable retry workflow
- Profiles: repository-local grep/filesystem only; jscout baseline; jscout
  structural
- Trials: one run per task/profile; profile order counterbalanced by task
- Grading: exact set equality for claimed production files and symbols
- Isolation: read-only frozen checkout, isolated Codex configuration, no web,
  browser, apps, plugins, or unrelated MCP servers

The checkout contains its own agent skills and app map. The grep arm could read
those repository files, so this is the value of jscout versus a normally
configured repository-local agent, not versus a context-free `rg` script.

The previously used `gpt-5.6-terra` was unavailable in the installed CLI, so
these cost and latency numbers are not directly comparable to P0.

## Result

| Metric, mean per task | Grep | Baseline | Structural |
|---|---:|---:|---:|
| Exact correctness | 4/4 | 2/4 | 4/4 |
| File precision / recall | 1.000 / 1.000 | 0.958 / 0.875 | 1.000 / 1.000 |
| Symbol precision / recall | 1.000 / 1.000 | 1.000 / 1.000 | 1.000 / 1.000 |
| jscout calls | 0 | 7.75 | 8.50 |
| Failed jscout calls | 0 | 0 | 0 |
| Tool result bytes | 0 | 38,914.75 | 28,014.25 |
| Inspected files | 10.25 | 4.75 | 6.00 |
| Irrelevant inspected files | 6.00 | 0.50 | 1.75 |
| Agent tokens | 129,369.75 | 163,485.75 | 224,039.50 |
| Wall time | 37.603 s | 36.038 s | 40.238 s |

Against grep, structural inspected 4.25 fewer files and 4.25 fewer irrelevant
files per task, but used 94,669.75 more agent tokens (+73.2%) and took 2.635
seconds longer (+7.0%). Against baseline, structural returned 10,900.50 fewer
tool bytes (-28.0%) but used 60,553.75 more agent tokens (+37.0%). One trial per
cell is insufficient to treat these cost deltas as stable.

## Tool behavior

- Structural agents used expanded semantic search on the registry and durable
  retry tasks.
- No agent called standalone `neighborhood`.
- Structural made 34 calls total: 27 definitions, six semantic searches, and
  one file outline.
- Baseline made 31 calls total: 18 definitions, ten semantic searches, and
  three file outlines.
- Whole-response budgeting held individual expanded searches below the agent's
  requested 10–12 KB ceilings and made total structural tool bytes lower than
  baseline in this run.

Expanded search was useful as delivery plumbing, but its initial retrieval
still elevated tests and build scripts. The graph then expands the wrong seeds
honestly; ranking cannot repair a poor seed set after the fact.

## Limits

- One trial per task/profile does not separate product effects from agent
  variance.
- All four questions remained answerable by grep, so the suite created work
  headroom but not a structural-only capability boundary.
- Exact set grading penalizes extra or omitted final locations even when the
  explanation is substantively correct. That is intentional for localization
  precision, but it must not be misread as a graph-causality result.
- Repository-provided agent guidance helped every arm and may have reduced the
  standalone value of the index.

The missing product number remains a repeated-task result showing equal or
better outcomes at lower total agent cost. This run did not produce it.
