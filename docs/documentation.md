[← README](../README.md) · [Configuration](configuration.md) · [Commands](commands.md)

# Documentation indexing

## Markdown and MDX retrieval

Repository Markdown and MDX are admitted by the normal `jscout index` pass
into the same atomic database publication as code, with a separate
documentation digest. They rank in an isolated BM25/vector corpus, so
documentation never changes code-search term statistics or vector candidates.
MDX deliberately uses the same inert Markdown block parser: raw
JSX, props, expressions, and inner text remain searchable documentation and
never enter code graphs. Two narrow exclusions keep retrieval units useful: a
contiguous leading import/export-only preamble emits no chunk, and exact JSX
comments (`{/* ... */}`) are removed outside Markdown code ranges just as HTML
comments are.
The disposable `files` inventory records ranking membership in `corpus`
(`code` or `docs`) separately from parser identity in `format`; Markdown uses
`corpus='docs'` and `format='markdown'`, while MDX uses `corpus='docs'` and
`format='mdx'`. Chunk `kind` describes structure inside that file.
Documentation metadata is stored separately and is not used to infer corpus
membership.

Documentation admission is enabled by default and can be disabled
independently of vector search:

```toml
[docs]
enabled = false

[docs.search]
vector = true
freshness = false
max_rank_movement = 2
```

With `enabled = false`, the shared index admits no documentation rows,
`docs status` reports the feature as disabled, and the CLI/MCP documentation
retrieval surfaces are unavailable. The `docs.search.vector` setting controls
only vector participation during documentation search; it does not enable
corpus admission or generate vectors. `docs.search.freshness` defaults to
`false`; it controls both index-time Git attribution and the bounded
Git-authorship reorder. With the default, indexing performs no documentation
provenance Git commands, blame-cache work, or publication revalidation and
publishes disabled/unknown provenance. When enabled,
`max_rank_movement` selects the reorder's one-to-three position bound.

```bash
jscout index /path/to/repo
jscout docs search /path/to/repo "current deployment procedure" --lexical-only

# Optional: reuse the existing [embedding] provider and model.
jscout docs embed /path/to/repo
jscout docs search /path/to/repo "current deployment procedure"
```

After changing `docs.search.freshness`, run `jscout index`. A running
`jscout watch` reloads the documentation indexing policy and forces a full
generation automatically. Until an enabled, current-format provenance
projection has been published, an effectively freshness-enabled search fails
closed and asks for `jscout index`; freshness-disabled search, status, embed,
and code surfaces remain available.

Vector search joins BM25 only when the current documentation digest has a
complete vector generation for the configured embedding profile. Ordinary
search falls back to BM25 when vectors are absent; `--vector` requires vector
participation and fails instead. Index rebuilds rematerialize complete cached
documentation generations without provider calls, and documentation digest or
text-contract changes purge obsolete materialized occurrences; only new documentation
identities require `jscout docs embed`. Ordinary `jscout embed` and watched
code embedding never request documentation vectors. Retrieval returns title,
description, tags, heading context, path, and line range; file hashes and byte
offsets remain internal. The MCP `documentation_search` tool exposes the same
isolated ranking corpus.
Membership defaults to exact lowercase `**/*.md` and `**/*.mdx` and is
configured with `[docs]`. `docs.search.freshness` controls both indexed Git
provenance and the bounded temporal reorder. `--no-freshness` disables only the
query reorder and preserves relevance order for comparison; it does not rebuild
the indexed projection.

Documentation provenance publishes an internal
`meta.documentation_provenance_digest` in the same database transaction as the
code and documentation digests. A history-only attribution change rotates the
canonical `publication_snapshot` fold while leaving both content digests stable,
so code-bound checker, semantic, and cursor state does not become stale solely
because Git authorship metadata changed. A documentation source edit rotates
the documentation digest and publication fold but leaves the code digest
unchanged. Code and semantic responses expose the code digest as `snapshot`;
documentation responses expose the documentation digest. Successful atomic
query/read responses and `annotate` also carry `publication_snapshot` for
canonical indexed-publication correlation.
The first reindex after installing the split rebuilds disposable pre-split
state once through the schema gate.

Git provenance control-file events also still request a full watch generation.
The digest prevents cross-plane invalidation, but it does not yet provide a
provenance-only watcher fast path. Full refreshes rematerialize any complete
documentation-vector generation from the durable cache even when the rebuilt
documentation digest is unchanged.

## Membership

The shared walker applies deterministic skips and repository ignore files
before documentation globs. Only the root-level `.github`, `.claude`, and
`.agents` directories are added to the hidden-directory allowlist; hidden files
and further hidden components remain excluded.
`docs.exclude` wins over `docs.include`; include cannot resurrect ignored files.
Globs match paths relative to the indexed root. See [configuration](configuration.md)
for complete membership and glob rules.

Use `jscout docs status <root>` to inspect the corpus, rejection decisions,
and vector readiness. Normal `jscout watch <root>` tracks admitted Markdown/MDX
changes; `watch --embed` maintains code vectors, not documentation vectors.
Run `jscout docs embed <root>` explicitly after changes introduce new document
representations.

## Front matter, fields and chunks

A leading YAML mapping delimited by exact `---` lines supplies string `title`
and `description` fields and `tags` as a string or list of strings. Other keys
are ignored. Valid front matter never becomes a body chunk. Malformed front
matter is ordinary body text, with `malformed_as_body` visible in status; it
does not reject the document.

| Field | Source | Lexical weight | Embedded text |
| --- | --- | --- | --- |
| Title | Front-matter title → first H1 → filename stem | 4 | No |
| Description and tags | Front matter | 2 | No |
| Breadcrumb | Full enclosing heading path | 2 | No |
| Nearest heading | Closest enclosing heading | Through breadcrumb | Yes |
| Body | Markdown block text, after narrow removals | 1 | Yes |
| Path | Repository-relative path | 0.25 | No |

Title, description, tags, breadcrumb, path and source line range also accompany
retrieval results. Embedding identity hashes the text-format version, bounded
nearest heading and rendered body—not the filename, document title or full
breadcrumb. Renames and metadata-only changes therefore reuse compatible
vectors; changing an enclosing heading re-embeds chunks that use that nearest
heading. The weights above are ranking constants, not TOML settings.

Chunking stays within heading sections, targets about 2,400 body bytes and
merges small compatible blocks up to 4,000 bytes. Provider text has a hard
24,000-byte bound. Oversized blocks split first at native boundaries (fence
newlines, table rows, list items), then newlines or a UTF-8 boundary when one
item is itself too large. Fragments repeat bounded fence/table context, keep
sequential ordinals and retain source locators; synthetic context and removed
comments mean rendered body is not necessarily a literal source slice.

Heading-only, front-matter-only and other body-empty documents emit exactly one
searchable document stub. Stubs carry metadata and a file-wide span but no
embedding identity or provider call. Empty sections do not each create a chunk.
