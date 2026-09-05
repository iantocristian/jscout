# jscout

A JavaScript/TypeScript structural, Rust lexical, and Markdown/MDX documentation
indexer for code search and agent retrieval. Built in Rust with [oxc](https://oxc.rs)
for JS/TS, with CLI and MCP interfaces.

Runtime value edges and type-only contracts stay separate. Code and documentation
share one SQLite database but have independent search corpora and content digests.
Lexical search works without a model, API key, Python, or GPU; vectors, reranking,
TypeScript checker enrichment, and model-generated semantic memory are optional.

## Getting started

With **Node.js ≥22.19.0**, install the prebuilt binary and prepare one repository:

```bash
npm install -g @jscout/cli
jscout setup /path/to/repo --client codex
jscout search /path/to/repo "checkout inventory" --lexical-only
```

Use `--client claude` for Claude Code. Setup creates minimal repository policy
if absent, indexes code and docs, installs the matching agent skill, registers
the project's MCP server, and verifies its handshake/tools. Restart/reload the
client and approve/trust the project configuration when prompted.

npm **already includes the MCP server**. Install has no compile step or install
script; setup is explicit and does not make model calls or download models.
It preserves existing repository settings and unrelated client entries.
To inspect registration without changing anything:

```bash
jscout setup /path/to/repo --client codex --print-config
```

For CLI-only use, skip setup:

```bash
jscout index /path/to/repo
jscout search /path/to/repo "checkout inventory" --lexical-only
jscout docs search /path/to/repo "deployment procedure" --lexical-only
```

Keep results current with a separate `jscout watch /path/to/repo` process,
or rerun `jscout index` after edits. Neither setup nor MCP starts a watcher.

npm supports macOS ARM64/x64 and GNU/Linux ARM64/x64 (glibc ≥2.31).
For source builds, release archives, Node/PATH requirements, authentication
and upgrades, see [installation](docs/installation.md).
For client-specific configuration and credentials, see [MCP setup](docs/mcp.md).

## Configuration

Repository policy lives in `<root>/.jscout.toml`; jscout does not search
parent directories. Defaults work without a config file, and setup creates
only `version = 1` when one is missing.

```bash
jscout config init /path/to/repo       # full annotated template; no overwrite
jscout config validate /path/to/repo
jscout config show /path/to/repo --json
```

The [configuration reference](docs/configuration.md) explains every setting,
default, bound, provider recipe, and precedence rule.
[.jscout.toml.example](.jscout.toml.example) is the annotated template.
jscout does **not** auto-load `.env`; export selected provider keys in the
launching process. [.env.example](.env.example) documents secret references
and invocation labels.

Documentation indexing is on by default, independently of vector generation.
Use `docs.enabled = false` to disable admission. `docs.search.vector`
controls vector participation, not whether documents get indexed or embedded.
[Documentation indexing](docs/documentation.md) covers Markdown/MDX membership,
front matter, vectors and opt-in Git freshness.

## Optional local vectors

npm and release archives include the Python service sources; Python/uv and
models are only needed when you opt in. Add to `.jscout.toml`:

```toml
[embedding]
provider = "local"
```

After [installing uv](https://docs.astral.sh/uv/getting-started/installation/):

```bash
cd /path/to/repo
jscout inference serve               # keep running; prepares locked Python environment
# In another terminal, from the same repository:
jscout inference doctor
jscout embed .                       # code vectors
jscout docs embed .                  # documentation vectors
```

The service uses Python 3.11/3.12 and downloads models on first use.
See [inference](docs/inference.md) for caches, hosted providers, reranking,
upgrades and troubleshooting. Code and docs embedding remain separate actions.

## Guides

- [Installation and troubleshooting](docs/installation.md): npm, source, archives,
  OAuth/API keys, environment, and upgrades.
- [MCP and agent setup](docs/mcp.md): registration, credentials, profiles, skills,
  response budgets, and transport.
- [Configuration reference](docs/configuration.md): all supported options and recipes.
- [Command reference](docs/commands.md): CLI workflows and anchor syntax.
- [Documentation indexing](docs/documentation.md): Markdown/MDX and freshness.
- [Embeddings and inference](docs/inference.md): optional vectors and reranking.
- [Advanced workflows and architecture](docs/advanced.md): checker enrichment,
  watcher lifecycle, reconnaissance, semantic memory, graph/search behavior,
  dependency indexing, storage and release packaging.

Run `jscout --help` or `jscout <command> --help` for complete flags.

## Project documents

[PLAN.md](PLAN.md) is the current architecture and roadmap.
[eval/](https://github.com/iantocristian/jscout/tree/main/eval) contains dated evaluation protocols and results.
[presentations/](https://github.com/iantocristian/jscout/tree/main/presentations) contains dated, non-normative explanatory
artifacts, not the current contract.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in jscout by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
