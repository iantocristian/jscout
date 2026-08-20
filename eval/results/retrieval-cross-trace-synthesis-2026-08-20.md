# Cross-trace synthesis — production retrieval sessions

- Date: 2026-08-20
- Sources: the two retained production call traces in this review round
  (`workflow-architecture-inquiry-2026-08-19.md`, 42 calls;
  `targets-queue-problem-investigation-2026-08-20.md`, 19 calls)
- Status: observational synthesis across both traces; independent read of the
  same inventories, no new session data

## Claim boundary

Everything below derives from the two traces' own call inventories and byte
tables. The workloads remain distinct (architecture inquiry versus
problem-solving investigation) and are compared descriptively, never pooled.
Nothing here is implementation-outcome evidence.

## What the two traces show together

**Tool selection is inverted relative to byte spend.** Across all 61 calls:
`who_uses` was selected zero times, the graph/entity tools (`paths`,
`neighborhood`, `events`, `calls`, `entities`, `file_outline`) zero times,
and `definition` four times — all four in the investigation session, where
they consumed ~5% of session bytes and carried what that agent rated the most
decisive evidence. Expanded search produced ~70% of investigation bytes with
its value concentrated in the first hits of the first pass. The investigation
agent converged unprompted on the efficient loop in its session tail: narrow
unexpanded searches plus `definition` to prove mechanism.

**The two workloads have opposite tool profiles.** Inquiry: memory-first
(24 of 42 calls on memory surfaces), `definition` never, `rg`/`sed` for
verification. Investigation: search-plus-definition, memory once, decisive
evidence from exact reads. One skill posture currently serves both modes.

**Parallel batching is the largest single waste class, and it is
interaction waste, not content waste.** Six identical `source_limit: 0`
schema failures launched in one parallel batch (14% of the inquiry session);
parallel expansions overflowing the client's outer message budget with later
results omitted; interleaved parallel artifact fetches forcing four repeat
reads. Roughly a quarter of the traced calls were retries or re-reads caused
by the batching interaction rather than by response content. The structural
mismatch: jscout budgets per response, the client budgets per combined
message, and agents batch aggressively — per-response budgets do not know
they have siblings.

**The memory plane's demonstrated production niche is orientation, and it is
entirely scout-fed.** `annotate` appears zero times in both traces; every
artifact came from scout batches. Where the plane covered the domain, it
structured the entire inquiry session and produced the source-verified
publish-flow skeleton; in the investigation it contributed one weak preview.
Set beside the evaluation record (corpus blind on fix surfaces, delivery
events rare and ambiguous), the split is consistent from both directions:
memory earns its keep for architecture inquiry and onboarding on a
well-scouted repository, not (yet) for implementation acceleration.

**Freshness surfacing earned its keep concretely**: the investigation agent
detected a mid-session rebase purely from response-level snapshot signals and
correctly re-established its evidence boundary.

## Roadmap consequences proposed

1. **G14 skill guidance: two postures, not one.** Inquiry mode — memory
   discovery first, at most one orientation expansion, `include_memory` off
   after useful memory is known. Investigation mode — narrow unexpanded
   search → `definition` chains, one expansion after localization. The traces
   show agents partially discovering this split themselves; the skill should
   hand it to them instead.
2. **G20 addition: sibling-aware response budgets.** Per-response budgets
   should assume parallel siblings share one client message budget — a lower
   effective default under batching, or explicit skill guidance that large
   reads are sequential-only. This targets the ~25% interaction-waste class,
   which no serializer change reaches.
3. **Expansion demoted from workhorse to orientation tool** in defaults and
   skill text. The first expanded search per investigation earns its bytes;
   later expansions mostly repeat known graph context. G20's path-shaped
   expansion fixes the shape; the posture change removes the repetition.
4. **`who_uses`: cap first, then decide.** The two traces here show zero
   selection, but the wider telemetry window
   (`mcp-telemetry-first-window-2026-08-20.md`) shows occasional use — and an
   unguarded worst case: three byte-identical 1.86MB responses on a
   high-fanout symbol, with no truncation ever firing. A response cap is
   immediate; fold-vs-keep waits for delivered-vs-selected data once call
   arguments are recorded.

## Concrete skill amendment proposed (G14)

Five lines, replacing posture-neutral guidance:

1. Two postures. Inquiry: memory discovery first, at most one orientation
   expansion, `include_memory` off once useful memory is known.
   Investigation: narrow unexpanded search, then `definition` on the exact
   anchor to prove mechanism.
2. Expansion once per investigation, after localization — never in the first
   parallel sweep.
3. Big reads are sequential: expanded searches and artifact details one at a
   time; parallel is fine only for small unexpanded searches.
4. Copy anchors verbatim from responses — never retype or shorten them (the
   invented-anchor failure class observed in evaluation).
5. If the response snapshot changes mid-session, re-verify the evidence
   boundary before continuing. The investigation agent discovered this by
   accident; it should be taught.

## Telemetry gap the traces expose

Per-call MCP telemetry already exists (`JSCOUT_TELEMETRY_FILE` plus
session/task/profile labels: tool, ok, elapsed, result bytes,
source/expansion/semantic metrics, retrieval vector/reranker status,
snapshot) and is what every evaluation used — but both production traces in
this PR are agent-self-reported because the variable was not set. Two fixes:

1. Make enablement one flag (or default-on for MCP sessions with a standard
   output path), and document setting a session label so pid-based session
   ids do not collide across weeks.
2. Add the three fields the follow-up-evidence list needs that rows lack:
   call arguments, a same-batch marker, and an outer/client truncation
   indicator. Delivered-versus-selected follow-up measurement then comes
   from the file instead of agent memory.
