# G17 exact-identifier dominance: implementation plan

Date: 2026-08-18  
Status: implementation-ready  
Normative parent: `PLAN.md`, “Planned G17 — exact-identifier dominance”

## Problem statement

`semantic_search` currently sends lexical and vector candidates through reciprocal-rank fusion, an optional cross-encoder, and repository-role policy. RRF deliberately discards score magnitude. A chunk containing the exact requested definition can therefore enter the fused list with no durable priority over partial-name examples, then be demoted by the reranker or role penalty.

That is an intent error, not a tuning problem. A case-sensitive code identifier is a deterministic lookup key. Learned ranking should operate only after exact definitions and whole-token occurrences have been satisfied.

G17 adds a separate deterministic intent lane. It does not change indexing, embeddings, the vector profile, BM25 weights, or the response-byte budget.

## Required behavior

Search results are partitioned into these tiers:

1. `exact_definition`: a chunk whose own name equals the identifier, or the most specific chunk containing a symbol declaration whose name equals it;
2. `exact_occurrence`: a non-definition chunk whose indexed symbol inventory contains the identifier as a case-sensitive whole token;
3. `hybrid`: the existing BM25/vector/RRF/reranker/repository-policy result.

Tier order is absolute. Ranking may reorder candidates inside one tier but cannot move a lower-tier candidate above a higher-tier candidate. Existing origin filters and explicit role filters apply to every tier.

For multiple identifiers, selection is coverage-first: one candidate for each resolvable identifier is emitted before a second candidate for any identifier, subject to the caller’s result limit. Same-named definitions remain separate candidates.

## Query-intent parsing

Add a small parser in `src/search.rs` that returns ordered, deduplicated, case-preserving identifier tokens.

An identifier matches JavaScript/TypeScript’s common ASCII spelling subset:

```text
[A-Za-z_$][A-Za-z0-9_$]*
```

The exact lane activates under either condition:

- the trimmed query is one identifier token, including lowercase names such as `insert`; or
- a token inside a larger query is visibly code-shaped: it contains `_` or `$`, starts uppercase, or contains an uppercase character after its first character.

Backticks and ordinary punctuation may delimit a token. Multiple plain lowercase prose words do not activate the exact lane. This deliberately avoids interpreting a query such as `development cache behavior` as three symbol lookups while still recognizing `find createRouteTypesManifest callers`.

The parser does not consult the index to decide whether prose is code. Doing so would make intent vary with corpus contents and could promote common English words merely because a matching symbol exists.

## Exact candidate retrieval

### Definitions

For each parsed token:

1. query `symbols.name = ?` using SQLite’s case-sensitive binary comparison;
2. retain symbols whose file passes the active origin and explicit role filters;
3. map each declaration to the most specific containing chunk in the same file;
4. prefer a containing chunk with `chunks.name = symbols.name`, then the smallest source span, then stable IDs;
5. also query exact `chunks.name = ?` rows so named chunks without a projected symbol are not lost;
6. deduplicate by chunk ID while preserving distinct same-name definitions in different scopes/files.

The exact query is independent of the BM25 and vector candidate limits. A definition cannot disappear because the hybrid pool was truncated first.

### Occurrences

For each parsed token, retrieve chunks whose indexed `chunks.symbols` inventory contains the same case-sensitive whole token. Candidate generation may use FTS to narrow the scan, but the final classification must verify token boundaries and case from the stored inventory. Definition chunks already selected for that token are excluded from its occurrence tier.

This is intentionally based on indexed symbol evidence, not a raw substring scan of source. Comments, string literals, and partial identifiers such as `NextTypesPluginExample` do not become exact occurrences.

### Filters

Exact retrieval shares one filter helper with hybrid retrieval:

- default origin policy remains unchanged;
- explicit origin requests apply normally;
- explicit file-role filters are hard filters;
- repository-role penalties are not hard filters and therefore cannot demote exact candidates into the hybrid tier.

## Coverage and ordering

Build deterministic per-token queues for definitions and occurrences. Merge them in this order:

1. round-robin over definition queues;
2. round-robin over occurrence queues;
3. append hybrid candidates not already emitted.

Within a token’s definition queue, preserve a deterministic ordering by exact chunk-name preference, exported declaration preference, path, source position, and chunk ID. Within occurrences, use the existing hybrid order when the chunk is present there, followed by path/source order for exact-only candidates.

Round-robin is applied before the result limit. With a limit of two and two resolvable identifiers, each identifier receives one slot. If the limit is smaller than the number of identifiers, earlier query order wins and truncation remains visible through the existing response budget/result count metadata.

## Interaction with learned ranking

The current hybrid pipeline remains intact:

1. BM25 and optional vector retrieval;
2. role prefiltering;
3. RRF;
4. optional reranking;
5. repository-role policy;
6. hybrid truncation.

Exact chunk IDs are removed from the hybrid list before the final tier merge. The reranker may still order exact peers if they already have reranker scores, but a hostile or malformed score cannot cross a tier boundary. The first implementation may use deterministic peer order only; cross-tier invariance matters more than learned ordering among aliases.

## Output contract

Add a compact `match` reason to every hit:

- `exact_definition`;
- `exact_occurrence`;
- `hybrid`.

For an exact hit, also expose the identifier responsible for the tier. If one chunk matches multiple requested identifiers, retain all matched identifiers in query order while emitting the chunk once.

The compact renderer must include the match reason without restoring redundant internal IDs. Debug output remains fact-equivalent. Existing `score` and retrieval diagnostics remain available but are explicitly non-calibrated and do not encode tier precedence.

## Code changes

Primary changes:

- `src/search.rs`
  - identifier-intent parser;
  - exact definition/occurrence SQL helpers;
  - candidate tier and matched-identifier metadata;
  - coverage-first deterministic tier merger;
  - integration before final hit loading;
- `src/compact.rs`
  - compact match reason and matched identifiers;
- MCP/search serialization tests
  - ensure compact and debug surfaces agree.

No schema migration is required because `symbols`, `chunks`, and `chunks_fts` already contain the required evidence.

## Test plan

### Unit tests

- one lowercase identifier activates exact lookup;
- camelCase, PascalCase, snake_case, and `$` tokens are recognized inside prose;
- multiword lowercase prose stays on the hybrid-only path;
- case mismatches do not enter an exact tier;
- whole-token verification rejects prefixes and suffixes;
- same-name definitions remain distinct;
- a chunk matching several tokens is emitted once with all reasons;
- round-robin covers each token before repeated candidates.

### Search integration tests

Create a deterministic fixture containing:

- exact definitions for `createRouteTypesManifest`, `getRootParamsFromLayouts`, `collectedRootParams`, and `NextTypesPlugin`;
- misleading Sitecore/example chunks with high lexical overlap;
- duplicate same-name definitions in separate files;
- generated/test roles to exercise explicit filters.

Assert that exact tiers precede hybrid candidates with vector search disabled and enabled.

### Hostile reranker test

Use the existing local fake-provider pattern to return the worst possible cross-encoder order: unrelated chunks receive the highest scores and exact definitions the lowest. Assert that peer order may change but no hybrid candidate crosses the exact-definition boundary.

### Regression tests

- prose queries produce the pre-G17 hybrid ordering;
- origin and explicit role filters apply to exact candidates;
- complete response-byte budgeting and truncation still hold;
- compact/debug results carry equivalent identity, location, tier, and follow-up facts.

## Delivery sequence

1. Add intent parsing and isolated tests.
2. Add exact definition and occurrence retrieval.
3. Add the coverage-first tier merger and result metadata.
4. Integrate with hybrid search after reranking/policy.
5. Update compact output and MCP fixtures.
6. Run formatting, Clippy, the full Rust suite, and script tests relevant to MCP response shape.

## Failure handling and rollback

If exact lookup fails, search fails rather than silently claiming hybrid-only completeness. The feature has no durable state and can be reverted without rebuilding an index. Performance regressions are bounded by parsed identifier count, filtered indexed SQL lookups, and the existing final result limit; raw repository source is never scanned.

## Completion gate

G17 is complete only when exact definitions survive a hostile reranker, multi-identifier coverage is deterministic, prose queries are unchanged, match reasons appear on both compact and debug surfaces, and the full local test suite passes without relying on CI.
