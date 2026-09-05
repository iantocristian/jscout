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

## Indexing and retrieval

```
jscout --version                 # installed binary/package version
jscout index <root>            # rebuild disposable structural state in .jscout.db
                               #   --database PATH isolates index/memory state
                               #   --deps pkg,@scope/pkg indexes named dependency internals
                               #   --no-deps disables configured dependencies for this pass
jscout search <root> "query"   # hybrid BM25 + embedding search (BM25-only without a provider)
                               #   --database PATH reads an isolated index
                               #   add --expand for a bounded structural context pack
                               #   --no-vector, --no-rerank, or --lexical-only control stages
                               #   --json is compact; --debug-json retains diagnostics
jscout docs search <root> Q    # Markdown/MDX BM25 plus ready shared-profile vectors
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
