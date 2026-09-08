[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# Command reference

Use `jscout --help` and `jscout <command> --help` for the complete flags.
The following commands share [repository configuration](configuration.md).

```bash
jscout setup <root> --client codex     # index, install skill, register and verify MCP
jscout setup <root> --client claude    # same flow for Claude Code
jscout setup <root> --client codex --replace # refresh this root's local registration
jscout setup <root> --client codex --print-config # print only; no changes
jscout config init <root>             # full annotated config template; no overwrite
jscout config validate <root>
jscout config show <root> --json       # effective values and their sources
```

Setup's `--replace` preserves other client settings and refuses remote,
disabled, unrecognized, or different-root registrations. It cannot be combined
with `--print-config`; see [existing configuration](mcp.md#existing-configuration).

## Progress and output

Long-running CLI commands—including indexing, embedding code/docs/semantic memory,
all `scout` commands, and checker enrichment—report their current phase, elapsed
time, and available work counts on **stderr**. Query and diagnostic commands only
show progress if they take at least two seconds. Results stay on stdout, so JSON
and JSONL output remain usable in pipes and files:

```bash
jscout docs embed /path/to/repo --json > embeddings.json
jscout --no-progress docs embed /path/to/repo --json > embeddings.json
```

`--no-progress` is global and suppresses progress, including checker batch and
resource updates. Warnings, errors, explicit diagnostics, and watch lifecycle
logs remain visible. Progress is plain text, with
waiting updates about once per second on a terminal and every five seconds
otherwise. Phase changes and completion can be reported sooner.

Counts apply to the named phase, not an estimated percentage of the whole
command. Embedding counts distinguish missing representations from cached reuse
and published occurrences. Scouting counts include processed subjects, including
reuse and skips; repository subdivision can increase the total. Finished model
requests are not necessarily successful or published artifacts. Final reports
and exit status remain authoritative.

Watch reports active work and pauses progress while idle. The MCP stdio server
and `inference serve` do not use this CLI reporter; their existing protocol and
service logging are unchanged.

## Indexing and retrieval

```
jscout --version                 # installed binary/package version
jscout index <root>            # rebuild disposable structural state in .jscout.db
                               #   --database PATH isolates index/memory state
                               #   --deps pkg,@scope/pkg indexes named dependency internals
                               #   --no-deps disables configured dependencies for this pass
jscout search <root> [query]   # hybrid BM25 + embedding search (BM25-only without a provider)
                               #   --database PATH reads an isolated index
                               #   --path FILE / --path-prefix DIR narrow indexed candidates
                               #   omit query only with a path filter
                               #   --exhaustive defaults to --match-mode all (AND)
                               #   --match-mode any selects OR; --allow-broad confirms large OR sets
                               #   add --expand for a bounded structural context pack
                               #   --no-vector, --no-rerank, or --lexical-only control stages
                               #   --json is compact; --debug-json retains diagnostics
jscout docs search <root> [Q]  # Markdown/MDX BM25 plus ready shared-profile vectors
                               #   --path FILE / --path-prefix DIR; omit Q for path-only lookup
                               #   --lexical-only needs no embedding provider
                               #   --no-freshness preserves pure relevance order
jscout docs embed <root>       # embed missing Markdown/MDX representations
jscout docs status <root>      # corpus decisions and vector readiness
jscout who-uses <root> SPEC    # all usage sites of a symbol, grouped by confidence
jscout neighborhood <root> A   # bounded structural traversal around an anchor
                               #   compact JSON by default; --debug-json for diagnostics
jscout workflow-candidates R S # experimental fingerprinted candidate-set diagnostic
jscout events <root> [name]    # string-keyed event wiring (emit/listen sites)
jscout calls <root> METHOD     # exact member-call sites matched on the AST
                               #   --arg merge=replace --receiver wave.card --json
jscout checker doctor <root>   # checker version, projects, config problems, readiness
jscout scout repository <root> # classify repository/package/project purpose from evidence
  --max-calls N                #   explicit model budget; --dry-run makes no model calls
jscout enrich <root>           # explicit occurrence-scoped TypeScript checker pass
                               #   --dry-run plans ownership without building Programs
                               #   --file/--package/--member/--role narrow eligibility
                               #   --max-occurrences N explicitly requests partial coverage
                               #   --all includes other resolved calls, excluded roles, every orphan;
                               #   receiver value-flow answers remain excluded
jscout watch <root> [--embed [--product]] [--enrich]
                               # full startup/boundaries; complete-inventory incremental reconciliation
                               # optional code-vector/checker/semantic-vector phases
                               #   --product keeps embedding to the effective product corpus
                               #   uses watch.dependencies, independently of index.dependencies
                               #   --deps overrides it; --no-deps disables it for this session
                               #   --database PATH isolates index/memory state
                               #   --debounce-ms 2000 waits for a trailing quiet point
                               #   --reconcile-seconds 600 recovers missed notifications
jscout embed <root>            # embed code chunks missing embeddings (cached by content hash)
                               #   --database PATH writes an isolated index
  --product                    #   fresh runtime recon + neutral production fallback only
  --semantic                   #   also embed current generated/agent semantic artifacts
  --semantic-only              #   update only the semantic-artifact vector index
  --repair                     #   force a full code-vector consistency audit
jscout inference serve         # run the optional local embedding/reranking service
jscout inference doctor        # verify its endpoint, device, models, and dimensions
jscout entities <root> [query] # runtime, contract, route, config, data, flag, host entities
jscout paths <root> A B        # bounded ranked paths between exact boundaries
jscout overview <root>         # deterministic cold-start inventory
  --semantic                   #   optional current/fresh untrusted memory overlay
jscout mcp <root>              # MCP stdio server; the core profile (default) serves
                               #   search, definition, who_uses, calls, file_outline, events,
                               #   documentation_search; --profile full adds graph, entity,
                               #   overview, semantic_memory, and annotate tools
                               #   --result-transport auto|text|structured overrides config
jscout memory <root> [query]   # compact semantic handles and freshness
  --anchor EXACT_ANCHOR        #   hard direct-support join; also --file/--reconnaissance-subject
jscout memory <root> --artifact ID
                               #   compact meaning/freshness; --view body gets the body + one locator
  --view full                  #   diagnostic relations/supports/provenance/hashes
  --source                     #   optional hash-verified source evidence (one row by default)
jscout annotate <root> in.json # write a validated semantic artifact
jscout llm doctor              # verify Node, pi-ai, plan auth, and default model capabilities
jscout scout workflows R       # auto-select deterministic workflow entry surfaces
  --max-calls N                #   default: openai-codex:gpt-5.6-terra via ChatGPT plan
jscout scout workflows R       # classify one agent-supplied workflow boundary
  --seed ANCHOR                #   repeat --seed to define one multi-seed boundary
jscout scout cards R           # evidence-backed cards for selected symbols
  --max-calls N                #   --anchor/--file/--subject target exact surfaces
jscout scout summaries R       # bottom-up file/module/repository summaries over artifacts
  --max-calls N                #   --level file|module|repository, --scope KEY (repeatable)
jscout scout concepts R        # concepts from exact workflow-name/card-domain-term vocabulary
  --max-calls N                #   --term TEXT selects normalized groups explicitly (repeatable)
jscout scout refresh R         # replace stale/degraded workflows, cards, summaries, and concepts
  --max-calls N                #   reuses each artifact's recorded model/configuration
jscout stats <root>            # parse stats
jscout chunks <root>           # dump AST-aware chunks as JSONL
jscout agent-guide             # print the core skill (--tier full for the full one)
jscout agent-guide --install R # install a project-local jscout skill
  --tier core|full             #   core teaches the default tool surface; full adds memory/graph
  --dest agents|claude|codex   #   .agents/, .claude/, or .codex/skills/jscout/SKILL.md
jscout agent-guide --update R --tier core --dest agents  # replace exactly that installed skill
```

## Search scope and exhaustive results

Both code and documentation search accept `--path` for an exact
repository-relative file and `--path-prefix` for a directory subtree.
These are literal paths, not globs: `--path-prefix src/app/` includes
`src/app/page.ts` but not `src/apple.ts`. The prefix is normalized to one
trailing slash. Both filters intersect when supplied together; code's
origin, format, and role filters still apply. Candidate scope is applied
before ranking limits, including vector and exact-identifier candidates.
Filtering does not traverse the filesystem, change admission, or require reindexing.

```bash
jscout search /path/to/repo "cache" --path-prefix packages/server --exhaustive --json
jscout search /path/to/repo --path src/cache.ts --json
jscout docs search /path/to/repo --path README.md --json
jscout docs search /path/to/repo "deployment" --path-prefix docs/operations
```

Omitting the query requires a path filter and returns bounded indexed chunks
in deterministic path/start/id order. Path-only lookup skips vectors,
reranking, freshness ordering, memory, and expansion. Membership comes from
the index; documentation still verifies selected source-file hashes before
delivery, with indexed content as the mismatch fallback. For a text query,
ranked behavior is unchanged;
explicit expansion can supply separately labeled related context outside the
primary path scope.

`--exhaustive` defaults to `--match-mode all`: every query token must occur
in the same indexed source-content chunk, without requiring adjacency or
one matching line. Use `--match-mode any` explicitly for OR. Neither mode
interprets input as regex or FTS syntax, and zero AND matches do not trigger
an OR retry. The match operator and `--allow-broad` are exhaustive-only.

A multi-token OR request matching at least 200 chunks initially returns
`confirmation_required: true`, the count and a `broad_or_query` warning,
with no hits or cursor. This is not an empty completed search. Refine it or
explicitly confirm the counted set:

```bash
jscout search /path/to/repo "cache route" --exhaustive --match-mode any --json
# Only if that OR evidence set is intended:
jscout search /path/to/repo "cache route" --exhaustive --match-mode any --allow-broad --json
```

For an intended traversal, copy `next_cursor` into `--cursor` until
`truncated: false`. Preserve the query, match mode, and path/role/origin/format
filters; confirmation need not repeat on continuation. On
`response_budget_too_small ... minimum_bytes=N`, retry the same page with
`--response-bytes N`. Documentation search does not have exhaustive matching
or this broad-query guard; its textual lexical/hybrid ranking is unchanged.

## Anchor arguments and resolution boundaries

`SPEC` is `NAME` or `path-substring:NAME`, e.g. `getUser` or `services/user:getUser`.

Workflow-candidate seeds must each resolve uniquely to a symbol.
File anchors are rejected because a file can contain multiple unrelated
operations; choose an exported symbol or pass its exact returned `sym:` anchor.

`A` accepts a returned node key, a repo-relative file path, a symbol name, or
`path-substring:NAME`. Every neighborhood includes the current repository
snapshot. When reusing an anchor after edits, pass that value with `--snapshot`;
jscout re-resolves stale symbol anchors by path, scope, and name, and returns an
error with candidates instead of guessing when the identity is ambiguous.
Traversal defaults to `certain`/`likely` edges. Use
`--min-confidence possible` to include unresolved string-event hubs and other
explicit candidates. Unknown-receiver member calls are projected through
property hubs; use depth two to traverse from a candidate symbol to possible
callers without materializing every call-site × symbol pair.

Indexing also performs a bounded receiver value-flow pass. It resolves
`this.m()` inside instance methods and supported instance initializers to the
enclosing class, direct or const-bound `new C()` receivers to `C.m`, and
module-scope immutable factory receivers through closed returns at depth two.
Imported/exported const values retain their value semantics. Awaited values and
async factories are left to the checker because thenable assimilation can
change their receiver identity. Every constructor, factory, or imported-value
reference must resolve to one exact module root or imported binding; local
immutable aliases are followed, while heuristic workspace edges and ambiguous
re-exports are rejected. Every factory branch must be a construct, a const
binding to one, or another bounded factory call, and a block body must not fall
through. Parameters, destructuring, conditional expressions, mutable
declarations, optional factory results, async/await values, decorators,
constructors with explicit returns, `eval` references, dynamic `with` scope,
unresolved or dynamically computed base/member shapes, TypeScript parameter
properties, and an accessor, field, or direct binding-member write anywhere in
the exact superclass chain that can shadow a method give up to the property
hub. Optional member invocation is accepted because it changes whether a call
runs, not the target when it runs. These occurrence-specific edges are
`likely`, never `certain`, and capped at three targets. Alias-mediated writes,
global-object rebinding, `Object.assign`/`defineProperty`, and prototype
mutation remain outside the bounded proof.

For admission and freshness rules, see [documentation indexing](documentation.md).
For checker, watch, scouting, and graph behavior, see [advanced workflows](advanced.md).
