# G26 Rust code indexing — design detail

- Date: 2026-08-25
- Status: subordinate, non-normative detail for the Proposed G26 entry in
  [PLAN.md](../../PLAN.md); that entry wins any explicit disagreement.
- Motivation: dogfood. jscout is a Rust program that cannot index itself.
  Admitting Rust makes this repository its own standing evaluation corpus and
  makes jscout useful for its own development. It is also the first exercise
  of G25's registry claims for a second code-corpus format — the honest test
  of "adding a format never reopens the architecture".

## Decision summary

Rust is admitted as `files.format='rust'`, `files.corpus='code'`, through the
same shared index pass, snapshot, and publication as everything else. It
climbs the G25 tier ladder in phases: text chunks first (no names, no
exact-tier interaction — safely measurable), named item chunks second (the
paid integration), module edges third. The parser is `ra_ap_syntax`. No
tree-sitter, no rust-analyzer semantics in the index path, no entity or
checker participation in this goal.

Unlike Markdown, Rust deliberately joins the code ranking economy: its chunks
enter `chunks_fts` and, once named, compete in the exact tiers. There is no
byte-identity gate — new corpus content changes code search *by design* — so
acceptance is evaluation-based instead: JS/TS-query behavior must not regress
materially, and the exact-tier interaction is measured before names ship.

## The seams (all exist after G24)

1. `code_format()` in `src/indexer.rs` gains `Some("rs") => Ok(RUST_FORMAT)`.
2. `walk::is_indexable` admits `.rs`; `walk::SKIP_DIRS` gains `"target"` —
   today only gitignore excludes Cargo build output, and a `target/` tree can
   be gigabytes; deterministic skips are how `node_modules` is handled and
   Rust deserves the same floor.
3. `extract_file()` gains a `rust::extract(rel, source)` arm returning the
   standard `FileData { chunks, graph, lines }`; `FileGraph::default()` is
   the phase 1 and 2 graph.
4. A new `src/rust_lang/` extractor module — the actual work.

The dependency walker (`dependency::collect_indexable_files`) does not widen:
crate sources under `~/.cargo` and vendored trees are out of scope for this
goal, exactly as G24 kept dependency Markdown out.

## Parser: `ra_ap_syntax`, pinned

rust-analyzer's syntax crate, published standalone. Chosen on three
requirements the chunk contract already imposes:

- **lossless spans** — the CST covers every source byte, comments included,
  so chunk spans slice back to the original file exactly, as the G24 harness
  pinned for Markdown and the code plane requires everywhere;
- **error tolerance** — watch mode indexes mid-edit working trees; a parser
  that fails a whole file on one syntax error makes in-progress code vanish
  from search. `ra_ap_syntax` parses broken files and marks error nodes;
- **pure Rust** — no C toolchain enters the build.

Why not `syn`: built for proc-macro token streams — not error-tolerant (one
bad token fails the file), drops non-doc comments, and byte spans are an
afterthought behind a feature flag. Why not `tree-sitter-rust`: G25 rejected
tree-sitter with a named revisit trigger, and this goal explicitly does not
fire it — tree-sitter's advantage is many grammars under one framework, and
for a single language a pure-Rust lossless alternative dominates. The trigger
remains what G25 says it is.

`ra_ap_syntax` tracks rust-analyzer releases and its API churns; the version
is pinned and bumped deliberately. Macro *expansion* is out of scope — items
inside `macro_rules!` bodies and attribute-macro output are not chunked;
`extractor_version`-style contract hashing covers the chunker like Markdown's
chunk format hash does.

## Phases

### Phase 1 — text tier, dogfood measurement

`.rs` files admitted as `corpus='code'`, chunked by the existing byte-budget
splitter with **no chunk names** (`chunks.kind='rust_text'`). Zero exact-tier
interaction, no symbols, `FileGraph::default()`. Rust content becomes
lexically searchable beside JS/TS immediately.

Measured on this repository before phase 2: index time delta, corpus and
database size delta, and a fixed JS/TS query set run before and after
admission — same queries, ranked output compared, regressions explained or
fixed. `--timing` already prints the projection cost.

### Phase 2 — named item chunks

`ra_ap_syntax` item walk: `fn`, `struct`, `enum`, `trait`, `impl` (chunked
per associated item with the impl header as context), `mod`, `const`,
`static`, `macro_rules!` (as one named chunk, body unexpanded). Doc comments
attach to their item's chunk like leading comments do for JS. `chunks.kind`
carries the item kind (`rust_fn`, `rust_struct`, …); `chunks.name` carries
the item name; `scope_chain` carries the module path (`store::open`).

This is the paid integration. Known hazard to measure before accepting:
Rust's name distribution is exact-tier-hostile — every type has `new`,
`default`, `from`, `len`. The per-identifier limits and path ordering of
`exact_definition_chunks` are the existing mitigations; the phase 2
evaluation measures collision behavior on mixed JS+Rust corpora (this
repository plus one JS monorepo) and phase 2 is accepted only on those
numbers. Inline `#[cfg(test)]` modules are a known role-granularity gap:
`file_role` classifies files, so test modules inside production files index
as production — recorded as a limitation, not solved here.

### Phase 3 — module edges

Deterministic, hand-rolled resolution at the fidelity the JS plane had before
the checker existed: the `mod` tree (`mod foo;` → `foo.rs` / `foo/mod.rs`),
`use crate::…` paths resolved against it, workspace/package layout from
`cargo metadata` (via the `cargo_metadata` crate). Produces module edges and
file-granularity `who-uses`/`neighborhood` for Rust. No trait or method
resolution — full semantics belong to a future rust-analyzer enrichment
sidecar following the tsserver pattern, explicitly out of this goal.

## Out of scope

Entity extraction for Rust (heur/value_flow are JS-idiom scanners), events,
member-call facts, checker enrichment, macro expansion, dependency-crate
indexing, a rust-analyzer sidecar, and any docs-corpus or freshness
interaction — Rust is `corpus='code'` and current-only like all code.

## Validation

- registry admission: `.rs` under `target/` is never indexed even without a
  gitignore; the dependency walker is unchanged byte-for-byte;
- spans slice back exactly on files with multibyte UTF-8, raw strings
  (`r#"…"#`), and CRLF;
- a file with syntax errors still yields chunks for its parseable items and
  is visibly counted, never silently dropped;
- phase 1 admission leaves a fixed JS/TS query set's ranked output within the
  agreed tolerance, and exact tiers byte-identical (no named Rust chunks
  exist yet);
- phase 2: exact-tier collision measurements on `new`/`from`/`default`
  recorded before names are accepted;
- self-index: this repository indexes end to end with timing and size
  recorded in `eval/results/` as the dogfood baseline.
