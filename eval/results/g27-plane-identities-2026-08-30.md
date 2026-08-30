# G27 plane identities — implementation validation

Date: 2026-08-30

## Outcome

G27 passed its code/docs isolation checks on the test suite and on two real
repositories. A separate matched single-host observation against the pre-G27
binary also reproduced the coupling G27 is intended to remove.

On ai-pipe, a documentation-only manual index under the treatment kept the
code digest, active checker batch, 387 checker facts, and 1,025 value-flow
edges unchanged. The next real TypeScript enrichment reused the batch with
zero checker queries instead of repeating 4,601 queries in 42 batches. A
deterministic delayed scouting request also published after documentation
rotated, because the code digest it validated had not changed.

The machine-readable record is
[`g27-plane-identities-2026-08-30.json`](g27-plane-identities-2026-08-30.json),
SHA-256 `32ccad854092d9e0e05bb2a2b882d4a2a06e4d763b8b2f04a032f6b9684754f6`.
Raw databases, archived source trees, command logs, and copied checker files
were retained only as local scratch evidence and are not committed. The
measurement was not run by a checked-in end-to-end paired harness.

## Implementation under test

| Item | Pre-G27 control | G27 treatment |
| --- | --- | --- |
| Source commit | `e434a26353d0b51d5a7208328596fc2184e128b1` | `552c8c690a58954b0761dd534eea9f1894ca9d69` |
| Source tree | `a6d068840efe8140e703357c795f7b921d9013ae` | `c526e1936e53165cecceff156efbe8a2e56477bd` |
| Release binary SHA-256 | `d7ae10580cd25907455a3851c250556e95740853726efc894c2e742ea9ae3c7b` | `846fd8607d5a9abccc89851bf40739c034a57b08ef2ca45bddc5bbb1761acaa9` |

Both arms used `git archive` of ai-pipe commit
`ea13166c59cfc52574e96959413f5c54be20e8c8`, indexed with `--no-deps`.
Both used the real local TypeScript checker against the archived source with
an externally linked dependency installation; those dependency bytes were not
frozen by `git archive`. Scouting used a local deterministic gateway that
accepted one request, waited 30 seconds, and returned one valid card; no remote
provider was contacted.

The G27 release binary was rebuilt from a clean committed checkpoint before
the run. Later branch changes updated documentation and corrected failed-call
MCP telemetry plane attribution; they did not change indexing, identity gates,
query responses, or the evaluated command paths.

## Real-repository smoke tests

Both corpora were tracked-file-only archives, leaving the original checkouts
untouched. Counts below are from the resulting schema-v34 databases.

| Corpus | Indexed files | Chunks | References | Documentation | Integrity |
| --- | ---: | ---: | ---: | ---: | --- |
| ai-pipe `ea13166` | 856 | 13,736 | 28,527 | 166 files / 8,253 chunks / 1 rejection | `ok` |
| n8n `9d9e9bf` | 19,816 | 97,609 | 404,990 | 580 files / 5,380 chunks / 31 rejections | `ok` |

The initial release-mode index observations were 1.21 seconds and about
106 MiB peak RSS for ai-pipe, and 17.1 seconds and about 398 MiB peak RSS for
n8n. These are descriptive single runs on an uncontrolled host, not
performance thresholds.

On both repositories, a documentation-only edit rotated the documentation
digest and publication fold while leaving the code digest byte-identical. A
complete code-query response before and after the edit was byte-identical
after normalizing only its top-level `snapshot` and `publication_snapshot`.
On ai-pipe, the reverse test changed code and left the documentation digest
stable; the complete normalized documentation response was byte-identical.

## Checker retention after a documentation-only manual index

Each arm began with a fresh database and one real `enrich --all --full` run:
4,601 selected checker occurrences, 42 request batches, and 387 published
checker facts. One Markdown file was then added and indexed manually before
running `enrich --all` again.

| Observation | Pre-G27 control | G27 treatment |
| --- | ---: | ---: |
| Initial full enrichment wall | 25.39 s | 25.61 s |
| Active batch after docs index | 0 | 1 |
| Stored checker facts after docs index | 0 | 387 |
| Projected checker edges after docs index | 0 | 387 |
| Value-flow edges after docs index | 1,025 | 1,025 |
| Follow-up checker occurrences queried | 4,601 | 0 |
| Follow-up request batches | 42 | 0 |
| Exact batch reused | false | true |
| Follow-up command wall | 24.49 s recovery | 0.29 s validation |

In the treatment, `documentation_digest` changed from `295730a9…a1092` to
`da84a9a0…4c59e` and the publication fold changed from `04294d76…570d` to
`e115168e…b004`. `code_digest` remained
`caf8a36e…ffaf`. The treatment arm therefore avoided 4,601 redundant checker
queries and the observed 24.49-second recovery cycle in this run.

## Scouting publication while documentation rotates

The local gateway wrote an accepted-request marker and delayed its fixed
result for 30 seconds. After observing that marker, the runner added a second
Markdown file and completed another manual index before the gateway returned.

| Observation | Pre-G27 control | G27 treatment |
| --- | ---: | ---: |
| Documentation index began after marker | 20 ms | 23 ms |
| Observed documentation-index wall | 1,259 ms | 1,438 ms |
| Scout finished after marker | 30,014 ms | 30,035 ms |
| Scout exit | 1 | 0 |
| Terminal state | `incomplete / inputs_changed` | `completed` |
| New card artifacts | 0 | 1 |

The treatment's documentation digest and publication fold rotated during the
request, while its code digest remained `caf8a36e…ffaf`. The completed run and
card recorded `code-v1:caf8a36e…ffaf`. The final treatment database contained
one completed run, one card, one active checker batch, 387 stored checker
facts, 387 projected checker edges, and passed `PRAGMA integrity_check`.

## Limits

This is one matched observation per arm on one uncontrolled Apple Silicon
host. Filesystem cache, host load, and thermals were not controlled. Timings
include process startup and are not CI limits or cross-machine estimates. The
gateway's reported one-input/one-output-token usage is fixture data, not billed
model traffic.

The source-checkout validation compared the bytes of `git status --short`
before and after; it proves that status output was unchanged, not that every
untracked or ignored byte in the source checkout was inspected.

The existing broad `bench/perf/ai-pipe.mjs` was not used for this result. Its
pre-G24 fixture still asserts 690 total indexed files, while the current archive
publishes 690 code rows plus 166 documentation rows. This measurement used a
narrow command sequence rather than weakening that older benchmark's frozen
invariant.
