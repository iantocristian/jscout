[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# Installation and troubleshooting

## npm

Install Node.js **22.19.0 or newer**, then:

```bash
npm install -g @jscout/cli
jscout --version
jscout setup /path/to/repo --client codex
jscout search /path/to/repo "checkout inventory" --lexical-only
```

Use `--client claude` for Claude Code. [MCP setup](mcp.md) explains the
files created and offers manual configuration. For CLI-only use, replace
`setup` with `jscout index /path/to/repo`; configuration is optional.

The npm package includes the Rust MCP server, gateway/checker Node sidecars,
and optional inference service sources. A platform-specific optional dependency
provides the binary; installation has no build step or lifecycle script.
Do not use `--omit=optional`: that removes the platform binary.
No Python environment, model download, credentials, client registration, or
repository indexing is performed by `npm install`.

Supported targets are `darwin-arm64`, `darwin-x64`, `linux-x64-gnu`, and
`linux-arm64-gnu`. GNU/Linux requires glibc 2.31 or newer. Other systems,
including musl-based Linux, need a compatible source build; no Windows npm
binary is published.

Node is required to launch **every npm-installed command**, even though core
indexing, lexical retrieval and MCP are implemented in Rust. A directly
launched release/source binary needs Node only for scouting or checker
enrichment. The local inference feature separately needs uv and Python;
see [inference](inference.md).

## Source checkout

From the jscout checkout, with a Rust toolchain installed:

```bash
cargo build --release --locked
./target/release/jscout --version
./target/release/jscout setup /path/to/repo --client codex
./target/release/jscout search /path/to/repo "checkout inventory" --lexical-only
```

`cargo build` does not add anything to `PATH`. Continue using
`./target/release/jscout` from this checkout, use its absolute path from
another directory, or put the installed release directory on `PATH`.
The setup command records the executable it is running, so moving or removing
that binary later requires inspecting `setup --print-config` and updating the existing registration
before running setup again.
For CLI-only use, replace the setup command with
`./target/release/jscout index /path/to/repo`.

For optional checker or scouting commands, also install Node ≥22.19.0 and
the pinned sidecar dependencies from the jscout checkout:

```bash
npm ci --prefix gateway
npm ci --prefix checker
./target/release/jscout checker doctor /path/to/repo
```

The binary discovers sidecars in the repository above
`target/{debug,release}`. `sidecars.gateway` and `sidecars.checker` can
override entry points. Scouting additionally needs the authentication below.

## Release archives

Extract the matching [release archive](https://github.com/iantocristian/jscout/releases)
and keep `gateway/`, `checker/`, and `inference/` adjacent to the binary.
Put that whole release directory on `PATH`, then use the npm quickstart's
`jscout` commands. Do not copy only the binary if you want bundled optional
features. See [release packaging](advanced.md#building-release-packages) to
build your own archive.

## Optional scouting authentication

Indexing, lexical search, and MCP setup need no model login. Only enable
scouting when you want generated repository/workflow/card/summary/concept
artifacts. Model calls require an explicit `--max-calls` budget.

The default model is `openai-codex:gpt-5.6-terra`, using pi-ai's
ChatGPT-plan OAuth credentials at `~/.pi-ai/auth.json`. This is a separate
credential store from a Codex client's login.

The gateway pins pi-ai 0.84.1. Its login CLI writes `auth.json` **in the
current directory**, so run it inside the directory jscout reads:

```bash
mkdir -p "$HOME/.pi-ai"
(
  cd "$HOME/.pi-ai"
  npx --yes @earendil-works/pi-ai@0.84.1 login openai-codex
)
cd /path/to/repo
jscout llm doctor
```

Follow the browser/login prompts. Use `llm.auth_file` if you deliberately
keep this store elsewhere. jscout does not initiate login; pi-ai's provider
handles credential refresh during use. The command and working-directory
behavior come from the pinned
[pi-ai CLI documentation](https://www.npmjs.com/package/@earendil-works/pi-ai/v/0.84.1).
`llm doctor` checks the gateway, model and auth configuration without making
a completion; it cannot establish account quota.

For an API-key provider, select the provider/model in `.jscout.toml` and
export the referenced key in the process that launches jscout:

```toml
[llm]
model = "openai:gpt-5.6-terra"
api_key_env = "OPENAI_API_KEY"
```

```bash
export OPENAI_API_KEY='your-key'
cd /path/to/repo
jscout llm doctor
```

An OpenAI Responses-compatible gateway can additionally set
`llm.openai_base_url = "https://gateway.example.com/v1"`; it must implement
streaming and tool calls. This endpoint is unrelated to the local
`sidecars.gateway` executable. For other providers and embedding keys, use
the separate [configuration recipes](configuration.md).

## Environment and MCP credentials

jscout **does not auto-load `.env`**.
[.env.example](../.env.example) lists secret references, invocation labels
and legacy migration examples. Keep durable, non-secret settings in
`.jscout.toml` and export only the secrets your selected provider needs.

A GUI-launched MCP client may not inherit terminal exports. Make the selected
key available through the client's environment configuration, or launch the
client from the configured shell. Codex supports the `env_vars` allowlist
for variables inherited by its MCP server; Claude Code supports an `env`
object. [Client-specific examples](mcp.md#credentials) show both. Setup does
not copy credentials into project files.

For a private certificate authority, export
`NODE_EXTRA_CA_CERTS=/absolute/path/to/private-ca.pem` before starting the
client or scouting command. Standard public HTTPS needs no TLS customization.
jscout does not install a global proxy agent; configure a compatible provider
endpoint when needed rather than assuming all adapters interpret proxy
environment variables alike.

## Updating

```bash
npm install -g @jscout/cli@latest
jscout --version
jscout config validate /path/to/repo
jscout setup /path/to/repo --client codex
```

Restart existing MCP clients and any local inference service after upgrading:
a running process still has the old code. Restart inference with the upgraded
`jscout inference serve` from the indexed repository; package installation
does not restart it. See [inference troubleshooting](inference.md#troubleshooting).
For CLI-only installations, run `jscout index /path/to/repo` instead of setup.

Writer commands rebuild disposable state when a supported older schema needs
it; read-only queries never migrate a database. Preserve a database before
replacing an unsupported durable format, because it can contain semantic
memory as well as disposable indexes. Run `jscout embed` and
`jscout docs embed` if their respective readiness diagnostics request it;
a changed embedding profile may require fresh vectors.

Re-running setup preserves existing repository policy and unrelated client
entries. A conflicting jscout registration is an error; inspect the file and merge
`--print-config` output before retrying. Customized skills are preserved with
a warning. See
[MCP setup](mcp.md#existing-configuration).

## Troubleshooting

| Symptom | Check / action |
| --- | --- |
| `jscout: command not found` after source build | Use `./target/release/jscout`; a build does not install onto `PATH`. |
| npm reports unsupported Node | Upgrade Node to ≥22.19.0 and reopen the shell/client. |
| npm platform binary is missing | Reinstall without `--omit=optional`; check the OS/CPU and glibc requirements above. |
| A query reports no published snapshot or an old schema | Run `jscout index <root>` against the same root, config and database as the reader. |
| MCP is missing or cannot start | Restart/reload the client, trust/approve the project configuration, and confirm its executable path still exists. |
| Code/docs results are stale | Run `jscout index <root>` or keep a separate `jscout watch <root>` process running. Setup and MCP do not start a watcher. |
| Docs are absent | Check `docs.enabled`, ignore/include/exclude rules, and `jscout docs status <root>`. |
| Vector results are absent | Check the selected provider and `jscout inference doctor` for local inference; run code and docs embedding separately. |
| Checker or model command cannot start Node | Check `node --version`, sidecar paths, then `jscout checker doctor <root>` or `jscout llm doctor`. |
| A key is exported but the agent cannot use it | Check the environment of the MCP client's server process, not only the terminal's. |

Use `jscout config show <root> --json` to see effective settings and their
sources. `jscout --help` and per-command `--help` expose the available
flags. For indexing rejections and retry behavior, see
[advanced workflows](advanced.md#gateway-execution-and-indexing-failures).
