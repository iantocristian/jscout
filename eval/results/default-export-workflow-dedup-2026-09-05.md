# Default-export and workflow-dedup correctness checks

The first three replay findings (definition coverage, RRF ties, and optional
inference revision identity) were already merged in #120. This change fixes the
remaining default-export binding and automatic workflow-boundary deduplication
defects on top of main `97d80cd`.

## Regression tests

Both defects were reproduced by their new tests before applying the production
fixes. The extraction-version refresh test also failed before the version bump:
all four unchanged files were skipped instead of reparsing the two JS/TS files.

- Preserve Oxc's existing binding for default exports of locally declared
  identifiers (`export default Local`), including runtime/contract export metadata.
  Cover const/function/class identifiers,
  direct/named exports, arbitrary-expression exclusions, and type-only exports.
- Exercise external call/render projection, name/anchor `who_uses`, and workflow
  candidates through a five-file index fixture.
- Hash sorted boundary membership, without changing candidate presentation or
  execution fingerprints. Exercise actual alternate seeds, all six permutations
  of a three-member boundary, changed membership/snapshot, and automatic planning
  of one call instead of two.
- Bump ECMAScript extraction version 7 to 8. Simulated old runtime/contract export
  rows and missing render edges are repaired without source edits; Rust/Markdown
  file and chunk rows stay intact, documentation/provenance identities stay
  unchanged, and the next incremental pass is a no-op. The shared code identity
  and graph projection still change/rebuild, as required.

Verification: **797 tests passed**, `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, release build, and
`git diff --check` passed. The historical G26 golden excludes the intentionally
changed extraction-version marker; its canonical rows and public results still
compare unchanged. The frozen baseline file itself was not rewritten.

Imported-binding forwarding (`import X from './x'; export default X`) remains
unsupported for downstream consumers. That is a pre-existing resolution gap,
not a regression fixed by this PR; it is left for a separate follow-up. The
external-edge and usage claims here apply to locally declared identifiers, not
to forwarding an imported binding.

## Native repository checks

Sources were exported to isolated directories from ai-pipe
`ea13166c59cfc52574e96959413f5c54be20e8c8` and the same Next.js root-params parent
used by the replay, `1d8e326d1b360da4a439cf440316fe76a359bfd3`.
The baseline is the retained #120 release; fixed indexing used a copy of each
baseline database. Docs and dependency indexing were disabled for these
code-graph checks. No model, embedding, checker, or solver calls were made.

| Measurement | ai-pipe | Next.js |
| --- | ---: | ---: |
| Indexed files, before and after | 690 | 20,721 |
| Chunks, before and after | 5,483 | 56,332 |
| Symbols, before and after | 4,538 | 39,409 |
| Newly bound default exports | 0 | 1,365 |
| Net additional call / render edges | 0 / 0 | 100 / 369 |
| Planned calls on identical old DB, old → fixed planner | 16 → 15 | 226 → 212 |
| Duplicate boundaries suppressed on identical old DB | 1 | 14 |

The dedup-only comparisons use `scout workflows --dry-run --max-calls 256` on
the exact unchanged baseline databases: their bytes and snapshots are verified
unchanged. These are fewer planned calls, not measured provider-cost savings.
Automatic planning remains capped at the existing 256-seed frontier; this is not
an exhaustive Next.js workflow census.

The targeted Next.js regression is independently visible through SQL and CLI:
RichText goes from **zero to 11 certain render usages**. CallToActionBlock's
candidate set grows from 18 to 20, adding exactly RichText and its `serialize`
dependency, with no removed candidates or traversal/output truncation.
Ai-pipe's resolved-edge content is unchanged. In Next.js, 39 old documentary
contract edges have same-source/kind/line replacements; for example, an unresolved
author-type placeholder resolves to the exported `AuthorType` declaration.

With both fixes and a rebuilt Next.js graph, the plan contains 208 proposed calls
and 15 suppressed duplicate boundaries. Graph repair also changes discovered
seeds (366 → 447), so this combined result must not be reported as 18 calls saved
by deduplication. The isolated dedup result is 14.

Both Next.js index runs retain the same 11 parser/input rejections. Two errors in
the ad-hoc inspection script (a nonexistent SQL column and a wrong symbol name)
were corrected with the failed logs retained and successful indexes reused.
These are not jscout failures. Native results were independently reviewed; they
establish structural/planning behavior, not improved solver success.

Local evidence is retained at `/private/tmp/jscout-graph-real.MMNinL/results/`
(`baseline-final.json`, `fixed.json`, command outputs, and four databases).
Baseline binary SHA-256: `254fb58d6a1beade0bfa4a8b96627ba81e64eeca38f4771ce994f326e182be8a`.
Fixed binary SHA-256: `87a91b465ca7807a685d0cce6e896a1ad071ec57935955f95f8152806064278c`.

## After updating

After updating jscout, run `jscout index <root>` or let a watch refresh publish the
new extraction contract. No schema migration or manual database deletion is
needed. Restart an already-running watcher to use the updated binary.

Manual indexing normally validates whether checker enrichment can be retained;
it does not unconditionally drop it. This upgrade's extraction-version change
prevents retaining the old checker publication, so the manual upgrade index
clears that batch. If the repository was previously enriched, run
`jscout enrich <root>` afterward to restore checker-backed edges.

Alternatively, `jscout watch <root> --enrich` can perform enrichment automatically.
Watch retains previous batches as hidden carry sources for its enrichment phase,
not as checker edges that remain visible under the new code identity. This PR
does not change the retention policy. The no-op check above exercises incremental
refresh; manual `jscout index` always performs a full refresh.
