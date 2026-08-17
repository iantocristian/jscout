# @jscout/cli

A fast JavaScript/TypeScript codebase indexer for RAG and agent retrieval,
written in Rust on [oxc](https://oxc.rs).

The runtime value graph is primary: functions, classes, components, calls,
renders, and executable module edges work the same way for JavaScript and
TypeScript. Type-only bindings never become runtime edges. A separate
documentary contract plane indexes interfaces, aliases, enums, decorators,
schemas, and exported API types without claiming they execute.

## Install

```bash
npm install -g @jscout/cli
```

The matching prebuilt binary is pulled in as an optional dependency; there is
no compile step and no install script.

## Use

```bash
jscout index /path/to/repo       # rebuild the structural snapshot
jscout search /path/to/repo "checkout inventory"
jscout overview /path/to/repo    # deterministic cold-start inventory
```

As an MCP server, with no absolute paths to maintain:

```json
{
  "mcpServers": {
    "jscout": {
      "command": "npx",
      "args": ["-y", "@jscout/cli", "mcp", "/path/to/repo"]
    }
  }
}
```

Run `jscout --help` for the full command surface.

## What needs what

Indexing, search, graph traversal, and the MCP server use the Rust binary
alone. Generative scouting and the optional TypeScript-checker enrichment pass
also use the Node sidecars bundled here — verify them with:

```bash
jscout llm doctor
jscout checker doctor /path/to/repo
```

Optional local semantic retrieval needs a separate Python service that is not
distributed on npm; see the [repository
README](https://github.com/iantocristian/jscout#readme). BM25-only installs
need no Python, no model downloads, and no GPU.

## Supported platforms

`darwin-arm64`, `darwin-x64`, `linux-x64-gnu`, `linux-arm64-gnu`.

The GNU/Linux packages require glibc 2.31 or newer. They are built and
smoke-tested in that userspace rather than against the current runner image.

Elsewhere, build from source with a Rust toolchain — the binary itself has no
Node dependency.

## License

MIT OR Apache-2.0
