# G20b MCP structured-content compatibility

- Date: 2026-08-21
- Status: compatibility experiment complete; fixed-call transport replay still pending
- Probe: `scripts/eval-mcp-structured-content-probe.mjs`
- Payload: 40 deterministic repository-shaped records, one tool call per arm

## Decision

Ship a profiled transport, not a global wire-shape flip:

- `mcp.result_transport = "auto"` is the default;
- auto mode emits `structuredContent` only for the verified Codex MCP client at
  version 0.147.0 or newer;
- Claude Code and unknown clients retain text-only results;
- `text` forces universal text-only compatibility;
- `structured` explicitly requests both native structured content and the
  required JSON-text fallback for client testing; and
- errors and any non-JSON success fail back to text only.

MCP exposes no structured-result capability bit during initialization, so this
is client profiling rather than protocol negotiation. Every structured success
keeps a fact-equivalent JSON-text fallback. Telemetry records client identity,
requested and applied transport, canonical/fallback/structured bytes, complete
tool-result bytes, complete JSON-RPC response bytes, and parse fallback.

## Canonical payload

The canonical compact JSON payload was 4,650 bytes. The structured and text
arms contained the same marker and all 40 records. Raw MCP response size grows
because the compatibility fallback and native value are both present; whether
that is worthwhile depends on what each client places in model context.

## Codex CLI

- Client: `codex-mcp-client` 0.147.0.
- Protocol requested: 2025-06-18.
- Both arms returned `jscout-structured-content-probe-v1 40`.
- Text-only raw call-result response: 5,255 bytes.
- Structured plus fallback raw call-result response: 9,926 bytes (+88.9%).
- Inspection of the matching official Codex source shows that a non-null
  `structured_content` is serialized directly as the model tool-result body;
  otherwise Codex serializes the complete `content` array.
- For this payload, the deterministic model-body representation was therefore
  5,243 bytes in the text arm and 4,650 bytes in the structured arm: 593 bytes,
  or 11.31%, smaller.

The reported aggregate model-token counters were cache-confounded and did not
show a stable token reduction. The claim is limited to the deterministic client
mapping and this payload's bytes. It does not claim an 11.31% session-token or
latency improvement.

## Claude Code

- Client: `claude-code` 2.1.238.
- Protocol requested: 2025-11-25.
- Both arms returned `jscout-structured-content-probe-v1 40`.
- Text-only raw call-result response: 5,289 bytes.
- Structured plus fallback raw call-result response: 9,960 bytes (+88.3%).
- The tool-follow-up cache-creation counters were 1,733 tokens for text and
  1,738 for structured. This rules out two complete payload copies reaching the
  model in this probe, but provides no evidence of a context reduction.

Because structured output adds wire bytes without a measured client-visible
benefit, auto mode deliberately keeps Claude Code on text. The explicit
`structured` policy remains available for future client-version probes.

## pi / pi-ai boundary

The installed `pi` 0.79.4 coding agent exposes built-in and extension tools but
no MCP client configuration or transport. The pi-ai process used by jscout is
an LLM gateway, not the consumer of jscout's MCP results. There is therefore no
applicable pi MCP arm in the currently supported local stack. If an MCP client
extension is adopted later, it starts as an unknown text-only client and must
pass this probe before being added to auto mode.

## Real-server smoke

The actual G20b binary was run against an indexed TypeScript fixture, not only
the synthetic server. With identical 988-byte canonical search results:

| Initialize identity | Policy | Applied | Tool-result wire bytes | JSON-RPC response bytes |
|---|---|---|---:|---:|
| Codex 0.147.0 | auto | structured | 2,144 | 2,178 |
| Claude Code 2.1.238 | auto | text | 1,135 | 1,169 |

The Codex response contained a native object equal to parsing the fallback
text. The Claude response contained only the fallback. Both choices and every
byte counter were retained in normal jscout telemetry.

## Claim boundary

This experiment establishes fact preservation, client selection, absence of a
duplicated full model payload in the two tested clients, and exact wire/model-
mapping bytes for one deterministic payload. It does not establish aggregate
G20 savings. The 42-call architecture inquiry and 19-call problem investigation
must still be replayed separately before the plan's 60% fixed-call target can
be evaluated.

