# @jscout/cli

JavaScript/TypeScript structural, Rust lexical, and Markdown/MDX documentation
indexing for code search and agent retrieval. Includes the Rust CLI and MCP
server, Node sidecars, and optional Python inference service sources.

## Install and use

Requires **Node.js ≥22.19.0**:

```bash
npm install -g @jscout/cli
jscout setup /path/to/repo --client codex
jscout search /path/to/repo "checkout inventory" --lexical-only
```

Use `--client claude` for Claude Code. Setup builds the initial index,
installs the matching project skill, registers and verifies MCP, and preserves
existing policy and unrelated client configuration. Restart/reload the client
and approve/trust project configuration when prompted.

The matching binary arrives as an optional dependency; there is no compilation
or install script. Do not install with `--omit=optional`. npm already installs
the MCP server—there is no separate server package or automatic client mutation.

For CLI-only use:

```bash
jscout index /path/to/repo
jscout search /path/to/repo "checkout inventory" --lexical-only
jscout docs search /path/to/repo "deployment procedure" --lexical-only
```

Print client-specific configuration without changing files or indexing:

```bash
jscout setup /path/to/repo --client codex --print-config
```

Keep a separate `jscout watch /path/to/repo` running, or rerun index after edits.
MCP and setup do not start a watcher.

## Optional features

Lexical indexing/search and MCP need no API key, Python, models or GPU.
Node is required for this npm launcher even for Rust-only operations.
The bundled Node sidecars support optional checker enrichment and model-backed
scouting; scouting needs separate authentication.

Local embeddings/reranking use bundled Python service sources, with uv and
Python 3.11/3.12. No source checkout is needed. Configure
`embedding.provider = "local"`, then explicitly start
`jscout inference serve` from the indexed repository to prepare its locked
environment. Package install and setup download no Python dependencies or models.
Run code `jscout embed` and documentation `jscout docs embed` separately.

## Supported platforms

`darwin-arm64`, `darwin-x64`, `linux-x64-gnu`, `linux-arm64-gnu`.
GNU/Linux requires glibc 2.31 or newer. Other systems need a compatible source
build; no Windows or musl npm binary is published.

## Documentation

- [Quickstart](https://github.com/iantocristian/jscout#readme)
- [Installation, authentication and troubleshooting](https://github.com/iantocristian/jscout/blob/main/docs/installation.md)
- [MCP setup and generated configuration](https://github.com/iantocristian/jscout/blob/main/docs/mcp.md)
- [Configuration reference](https://github.com/iantocristian/jscout/blob/main/docs/configuration.md)
- [Local inference and vectors](https://github.com/iantocristian/jscout/blob/main/docs/inference.md)

Use `jscout --help` for the command surface.

## License

MIT OR Apache-2.0
