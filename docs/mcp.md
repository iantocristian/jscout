[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# MCP and agent setup

The MCP server is part of the jscout binary. npm already installs it; there is
no second server package. One MCP process serves one indexed repository and
database over stdio.

## Setup

```bash
jscout setup /path/to/repo --client codex
# Or:
jscout setup /path/to/repo --client claude
```

Setup creates minimal `version = 1` repository policy only when absent,
indexes the repository, installs the skill matching the effective MCP profile,
registers the server for the selected client, and checks MCP initialization
and tool discovery. It makes no embedding or model calls. It does not launch
a persistent watcher or restart the client.

| Client | Project registration | Project skill |
| --- | --- | --- |
| Codex | `.codex/config.toml` | `.agents/skills/jscout/SKILL.md` |
| Claude Code | `.mcp.json` | `.claude/skills/jscout/SKILL.md` |

Restart/reload the client after setup. Codex loads project MCP settings only
for trusted projects. Claude Code may request approval of project MCP
servers. Setup does not change client trust or approval preferences.
[Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp),
[Claude Code project scope](https://code.claude.com/docs/en/mcp#project-scope).

The generated registration uses the current executable and canonical
repository/config paths. npm installs register the actual Node executable plus
the package wrapper so bundled sidecar discovery is retained. It is specific to that installation and checkout; after
moving either, regenerate the entry. Review machine-specific paths before
sharing project configuration in Git.

## Existing configuration

Setup preserves existing `.jscout.toml` settings and unrelated client
configuration. Matching command/argument entries retain extra client settings.
A conflicting launcher/argument list or disabled jscout server stops setup and
must be resolved manually. A customized existing skill is preserved with a
warning, not replaced. It does not change global client configuration.

Mutating setup rejects active legacy `JSCOUT_*` non-secret settings: migrate the
reported values into `.jscout.toml` first so a client with a different launch
environment gets the same policy. Referenced API-key variables are unaffected.
Existing local-stdio `env`/`cwd` overrides are preserved and exercised by the
handshake; disabled, remote/URL, or conflicting registrations are not replaced.

Configuration/skill creation and indexing happen before the handshake check;
client registration is written only after verification succeeds. If indexing
or verification fails, the minimal policy, installed skill, or completed index
may remain. Fix the reported problem and rerun setup; it does not delete a
working index to roll back onboarding.

Verification checks the indexed publication, MCP identity and exposed tools.
It does not claim optional provider health, model authentication, or vector
readiness; use the feature-specific commands before relying on those stages.

To inspect only the client configuration snippet:

```bash
jscout setup /path/to/repo --client codex --print-config
jscout setup /path/to/repo --client claude --print-config
```

Print-config writes to stdout and makes no changes: no config file, index,
skill, registration, or subprocess verification. Merge the printed snippet
into the appropriate client file yourself; do not redirect it over a file
containing other settings. For a fuller editable repository template, use
`jscout config init <root>`, which also refuses to overwrite existing policy.

## Manual registration

Build an initial index first:

```bash
jscout index /path/to/repo
```

Codex project `.codex/config.toml`:

```toml
[mcp_servers.jscout]
command = "/absolute/path/to/jscout"
args = ["mcp", "/absolute/path/to/repo"]
```

Claude Code project `.mcp.json`:

```json
{
  "mcpServers": {
    "jscout": {
      "command": "/absolute/path/to/jscout",
      "args": ["mcp", "/absolute/path/to/repo"]
    }
  }
}
```

Use the actual installed executable path. Other MCP clients need their own
configuration format; generic `mcpServers` JSON is not a Codex configuration.

For a launcher that resolves through npm, use `command = "npx"` and
`args = ["--yes", "@jscout/cli@0.5.0", "mcp", "/absolute/path/to/repo"]`
in the corresponding client format. This pins a version; change it explicitly
when upgrading. The client still needs Node ≥22.19.0 and a suitable `PATH`,
and the first launch may need network access. An installed executable avoids
that startup dependency.

## Credentials

Lexical retrieval requires no credentials. For a selected provider key, make
it available in the environment that starts the MCP client. Shell exports
are not automatically inherited by an already-running GUI app, and jscout
does not read `.env`.

For Codex, merge the required names into the existing server table's list
without removing other entries. Setup already adds configured embedding-key
names and the Node CA variable:

```toml
env_vars = ["NODE_EXTRA_CA_CERTS", "OPENAI_API_KEY"]
```

For Claude Code, add this field to the existing jscout server object:

```json
{ "env": { "OPENAI_API_KEY": "${OPENAI_API_KEY}" } }
```

Use only the variables your configured provider requires.
Codex forwards allowlisted environment values; Claude Code expands references
in `.mcp.json`. No literal key belongs in a shared project file.
[Codex environment forwarding](https://learn.chatgpt.com/docs/extend/mcp),
[Claude Code variable expansion](https://code.claude.com/docs/en/mcp#environment-variable-expansion-in-mcpjson).

## Keeping results current

Setup builds the first index. Thereafter run a separate process:

```bash
jscout watch /path/to/repo
```

Or rerun `jscout index <root>` after changes. MCP does not manage watch.
Use the same effective root/config/database for reader and writer. Keep
`index.dependencies` and `watch.dependencies` consistent: watch has its own
list and does not inherit the index setting. Alternatively pass the same
explicit `--deps` override to both commands.
[Watch lifecycle](advanced.md#watcher-lifecycle) explains atomic
publication, retries and optional checker/code-vector phases.

## Profiles, skills and transport

The skill is the teaching surface: server instructions carry only the
server's identity, a pointer to the skill, and two mechanical contracts, and
tool descriptions are one line each. Install the project-local skill so the
agent learns the flows without reading the schema:

```bash
jscout agent-guide --install /path/to/repo                     # core tier, .agents/skills
jscout agent-guide --install /path/to/repo --tier full --dest claude   # pair with mcp.profile = "full"
```

`--tier core` (the default) teaches the production-selected surface —
`semantic_search`, `definition`, `who_uses`, `calls`, `file_outline`,
`events`, and `documentation_search` — with two flows and the recorded
anti-patterns, in under 3 KB. `--tier full` adds `semantic_memory`,
`repository_overview`, `neighborhood`, `entities`, `paths`, and `annotate`
with the inquiry and write-back flows. `--dest` selects
`.agents/skills/jscout/SKILL.md` (default), `.claude/skills/jscout/SKILL.md`,
or `.codex/skills/jscout/SKILL.md`. The install refuses to overwrite an
existing guide; `jscout agent-guide --update` replaces one exact destination
in a single rename and creates it when missing, and requires explicit
`--tier` and `--dest` so it can never silently target another destination or
downgrade an installed tier. `jscout agent-guide` alone prints the text for
clients that consume `AGENTS.md`.

The skill tier and the MCP profile are independent settings that should be
paired: `jscout mcp` serves the `core` profile by default (alias `baseline`),
the seven tools the core skill teaches; `--profile full` (alias `structural`,
or `mcp.profile = "full"` in `.jscout.toml`) registers the six additional
tools the full skill teaches. Installing the full skill without the full
profile teaches tools the server does not expose. A `[mcp].tools` allowlist
narrows either profile per project — it must name at least one tool, and an
allowlist that leaves nothing registered under the effective profile
(documentation included) is refused at server start; omit it to register
everything the profile allows — and a call to a tool that is not registered
is refused at the boundary before any connection or lock is taken. The
server instructions name the repository, tell the agent to read the
installed skill before its first repository search, and carry the two
mechanical contracts; every routing rule, documentation included, lives in
the skill. `--database PATH` separates the index
and semantic-memory state from the source root for isolated warm/cold runs.
See [eval/README.md](https://github.com/iantocristian/jscout/blob/main/eval/README.md) for the paired-run protocol and grader.

`repository_overview` returns corpus totals and a bounded area table by
default; per-project reconnaissance prose appears only for an explicit
`reconnaissance_subject` or a non-zero `reconnaissance_limit`, and budget
eviction sheds that prose before structural counts.

`definition` returns full source by default. `jscout mcp --source-view elided`
enables the experimental deterministic renderer, and each call can override it
with `view: "full"` or `view: "elided"`. Both representations obey the same
per-definition `source_bytes` ceiling and report original/rendered byte counts.
MCP `definition` and `who_uses` use compact agent transport with a complete
`response_bytes` ceiling; set `debug: true` for their full diagnostic JSON.
Compact definitions serialize source once, while compact usages group sites by
confidence and file without dropping enclosing-symbol or candidate-detail
evidence.
The first SC-1 agent run found no compression on the artifacts selected by the
elided arm, so elision remains experimental rather than becoming the default.
The first discriminating three-arm run found no outcome gain over grep: both
grep and structural answered 4/4 exactly, while structural inspected fewer
files at substantially higher agent-token cost. See
[eval/results/ai-pipe-discriminating-2026-08-07.md](https://github.com/iantocristian/jscout/blob/main/eval/results/ai-pipe-discriminating-2026-08-07.md).

For opt-in agent-behavior measurement, set `telemetry.file` in `.jscout.toml`
or start MCP with `--telemetry .jscout-telemetry.jsonl`. The JSONL records tool
name, total and retrieval-stage latency, success, response size, session,
snapshot, binary fingerprint, configuration fingerprint, and requested
retrieval posture. It does not record queries, arguments, source, or results.
Set
`JSCOUT_SESSION_ID` to correlate calls from one evaluation run and
`JSCOUT_TASK_ID` to join it to an evaluation task. Profile and task labels are
included in each record; `JSCOUT_PROFILE_LABEL` overrides the recorded
profile label. Expanded searches also record aggregate node totals
and `expansion_role_counts`, plus projection and candidate/selected/omitted path
counts; no path bodies or source are added to telemetry.
Semantic calls add aggregate candidate/selected/returned/written counts and
fresh/degraded/stale totals. Search calls also record the canonical compact
`hits_bytes`, `graph_bytes`, `memory_bytes`, `envelope_bytes`, and total; these
sections sum to the canonical response and stay out of normal agent payloads.

For controlled evaluations that require a complete audit trail, additionally
pass `--request-log PATH`. This separate JSONL records every incoming MCP
method in order and includes exact `tools/call` arguments. It can therefore
contain repository queries, anchors, annotation text, and other sensitive
inputs; keep it with restricted raw eval artifacts, not in the repository.

## Configuration reloads

MCP remains one process for one root and one database. `jscout mcp
/path/to/repo` loads `/path/to/repo/.jscout.toml` once at startup.
Initialization metadata reports the exact database, config path, binary/config
fingerprints, and effective retrieval defaults. MCP has no hot reload. Watch
reloads only the documentation indexing policy—`docs.enabled`, `docs.include`,
`docs.exclude`, and `docs.search.freshness`—and forces a full generation when
that effective policy changes. Every other watch setting remains bound to
startup and requires restart. There is no multi-repository routing.

`mcp.result_transport = "auto"` emits native MCP `structuredContent` only for
verified Codex client versions and retains the fact-equivalent JSON-text
fallback. Unknown clients, including Claude Code in the current compatibility
profile, remain text-only because structured results increased raw wire bytes
without reducing measured client context. Set `text` for universal text-only
behavior or `structured` for an explicit compatibility probe; errors always
remain text-only. Transport selection and byte counts are recorded in MCP
telemetry.

## Response budgets

Code search defaults to **30,000 response bytes**. Documentation search and
non-search MCP tools default to **24,000 bytes**; these are not one global
budget. Per-call `response_bytes` overrides the applicable default. Code and
docs search defaults are independently configurable under `search` and
`docs.search`.

Code hit snippets target up to **eight lines / 1 KiB per hit**, within the
whole-response budget. Those are ceilings, not guaranteed padding. Under
pressure, tail shedding can remove a late match while retaining
`snippet_line`; `snippet_truncated` reports the loss. A definition has a
separate default `source_bytes = 12000` per definition, still inside its
whole-response ceiling. Source bytes, serialized response bytes, model tokens
and scouting `--max-calls` are different limits.
