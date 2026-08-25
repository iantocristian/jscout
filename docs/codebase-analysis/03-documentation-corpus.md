# The documentation corpus: discovery, admission, and prose chunking

`src/docs/corpus.rs` is 2,548 lines that do four things in sequence: ride one repository traversal it does not own, decide which files are documents and record why every rejected path was rejected, capture each admitted document's exact bytes under `O_NOFOLLOW`, and turn those bytes into heading-scoped, byte-budgeted chunks carrying a breadcrumb and a path-independent embedding identity. The structurally interesting part is the first step. The walk that carries this module is not a documentation walk that runs alongside the code walk — it *is* the code walk. `src/walk/inventory.rs` owns the filesystem traversal and the code-file list; `DocumentationCollector` (`src/docs/corpus.rs:243`) is one implementor of `walk::RepositoryInventoryConsumer` riding along, and it never sees a directory it did not get handed.

## One traversal, two planes

`src/walk/inventory.rs` (263 lines) is the traversal engine, and `pub(crate) trait RepositoryInventoryConsumer` (`src/walk.rs:58`) is the seam a second plane rides it through. Production documentation indexing enters at `corpus::repository_inventory` (`src/docs/corpus.rs:156`), which builds a `DocumentationCollector` and hands it to the generic `walk::repository_inventory` (`src/walk.rs:187`). The doc comments on both sides of the seam state the reason (`src/walk.rs:185-186`, `src/docs/corpus.rs:196-198`, `:240-242`): Markdown membership, capture, and parse happen inside the same deterministic traversal that selects code paths, so there is no independent documentation snapshot and no later filesystem scan that could disagree with the code inventory. The alternative — a second `ignore::WalkBuilder` pass for Markdown — would produce two file lists taken at two different instants, and any mismatch would show up as documentation referring to code that the same index pass never saw.

The naming is the one confusing residue. `corpus::repository_inventory` and `walk::repository_inventory` are different functions. The second is the engine entry, generic over the consumer and returning `RepositoryInventory<C::Output>` (`src/walk.rs:49`); the first is the documentation-facing wrapper whose entire body destructures `inventory.consumer` and flattens it into `RepositoryCorpus { files, rejections, documents, decisions }` (`src/docs/corpus.rs:67-72`, `:172-189`). The flattening is a re-shape, not a re-classification — `RepositoryCorpus.rejections` still holds `crate::walk::WalkRejection` values the walk layer produced.

The old walker survives. `walk::source_inventory` (`src/walk.rs:138`) still drives `ignore::WalkBuilder` directly, but its only remaining production callers are the read-only `source_files` diagnostic (`src/walk.rs:197`) and the watcher's `SourcePathPolicy` (`src/walk.rs:78`), which needs a standalone path matcher rather than a file list. So "which files count" is implemented twice, in two structurally different walkers that must stay in agreement — `shared_inventory_preserves_the_source_walker_contract` asserts `shared.files == legacy.files` on a tree containing symlinks, FIFOs, and a whitelist-reopened hidden directory (`src/walk.rs:414`). The divergence risk is paid in the watcher: `walk::is_indexable` accepts only the eight code extensions (`src/walk.rs:10`, `:15`), so a `.md` edit on its own never triggers a watch generation (`16-incremental-and-watch.md`).

The engine does something unusual with `ignore`: it builds a `WalkBuilder` with `hidden(false).git_ignore(true).git_global(true).git_exclude(true).follow_links(false)`, then throws the walker away and keeps only the popped `IncrementalIgnore` matcher (`src/walk/inventory.rs:42-52`). The crate is used as an ignore-rule engine, not as a traversal. Traversal is an explicit heap stack of `WalkTask` (`src/walk/inventory.rs:11-23`): directory entries are read, sorted by file name, and pushed in reverse so popping reproduces sorted depth-first order — without recursion, and therefore without a depth cap (comment at `src/walk/inventory.rs:61-62`, test `explicit_stack_walks_deep_trees_without_a_depth_cap` at `src/docs/corpus.rs:2039`).

Each task carries **two independent descent bits**, `source_active` and `consumer_active` (`src/walk/inventory.rs:11-23`), because the two planes' hidden-path policies genuinely differ and a docs-only prune must not narrow the code inventory (comment at `src/walk/inventory.rs:156-157`). Code keeps the legacy `hidden(true)` semantics, including the ignore-file whitelist that can reopen a hidden entry (`src/walk/inventory.rs:158-160`). The consumer's own rule is asked for separately, through `hidden_path_is_excluded` (`src/walk/inventory.rs:169`); for documentation that is the free function at `src/docs/corpus.rs:564`, where a dot-prefixed component is fatal *unless* it is at component index 0 and appears in `HIDDEN_ROOT_ALLOWLIST = [".github", ".claude", ".agents"]` (`src/docs/corpus.rs:25`). `.github/workflows/notes.md` is admitted; `packages/app/.github/notes.md` is `hidden-not-allowlisted`; `docs/.drafts/x.md` likewise. The allowlist is not configurable — it names the three places where agent-facing authored guidance actually lives, and widening it would multiply the admission surface. A repo that keeps prose under `.docs/` cannot admit it without a code change.

The whole documentation plane is switched off by an empty include list, and the walk reads that fact exactly once. `DocumentationCollector::is_active` returns `!self.options.include.is_empty()` (`src/docs/corpus.rs:281-283`) and seeds the root task's `consumer_active` bit (`src/walk/inventory.rs:55-59`); `DocsSettings::indexing_include` returns `&[]` when `[docs] enabled = false` (`src/config/model.rs:58-60`). Disabling docs costs nothing in the code plane, which is pinned by `empty_include_disables_docs_without_narrowing_code_inventory` (`src/docs/corpus.rs:1877`).

What to look for in the walk diagram: the single loop in `walk/inventory.rs` feeding two outputs across the trait boundary, and the fact that the ignore matcher is an input to the loop rather than a driver of it.

```mermaid
flowchart TB
  IDX["jscout index"] --> CRI["corpus::repository_inventory<br/>corpus.rs:156 builds the collector"]
  CRI --> RI["walk::repository_inventory<br/>walk.rs:187"]
  RI --> IGN["WalkBuilder built,<br/>discarded, IncrementalIgnore kept"]
  IGN --> STACK["Explicit WalkTask stack<br/>sorted DFS, no depth cap"]
  STACK --> ENTRY["Per entry:<br/>symlink_metadata"]
  ENTRY --> HARD{"dir in .git<br/>or SKIP_DIRS?"}
  HARD -->|yes| DEC["record_decision on the consumer"]
  HARD -->|no| IGM{"ignore match?"}
  IGM -->|yes| DEC
  IGM -->|no| SYM{"symlink?"}
  SYM -->|yes| DEC
  SYM -->|no| HID{"consumer says<br/>hidden_path_is_excluded?"}
  HID -->|yes| DEC
  HID -->|no| SPLIT["Two descent bits:<br/>source_active, consumer_active"]
  SPLIT --> CODE["is_indexable -> files"]
  SPLIT --> DOCF["inspect_regular_file:<br/>utf8 -> .md/.mdx -><br/>exclude -> include"]
  DOCF --> CAND["InventoryCandidate"]
  CAND --> FIN["finish -> acquire_candidates<br/>corpus.rs:354"]
  FIN --> DOCS["CapturedDocument + parsed DocFile"]
  CODE --> OUT["RepositoryInventory,<br/>flattened to RepositoryCorpus"]
  DOCS --> OUT
  DEC --> OUT
```

Note that `SPLIT` is the only node with two outgoing edges into different planes, and that `HID` prunes only the consumer bit — the code bit continues down a hidden directory that an ignore-file whitelist reopened. `shared_inventory_preserves_the_source_walker_contract` asserts exactly that pair: a `hidden-not-allowlisted` decision for `.source-visible`, no decision at all for anything beneath it, and `.source-visible/reincluded.ts` still present in `files` (`src/walk.rs:416-437`).

## Admission order and the visible decisions

Membership is evaluated in a fixed order, but it is now evaluated on both sides of the trait boundary. The walk decides the four exclusions that are properties of the filesystem and reports them by calling `record_decision` with its own vocabulary (`src/walk/inventory.rs:129-177`); the collector decides the four that are properties of documentation policy, inside `inspect_regular_file` (`src/docs/corpus.rs:310-335`), which does no I/O; the last four are decided during capture. Every rejection emits a `Decision` (`src/docs/corpus.rs:75`) carrying `{path, subject, rule, detail, path_base64, path_encoding}`, where `subject` is `directory`, `entry`, or `file`. The complete rule vocabulary:

| Rule | Decided by | Emitted when |
| --- | --- | --- |
| `hard-skip` | walk | directory named `.git` or in `walk::SKIP_DIRS` (`src/walk/inventory.rs:131-136`, `:242-246`) |
| `ignored` | walk | gitignore/global/exclude matched (directories always; files only when path-relevant) (`src/walk/inventory.rs:144-154`) |
| `symlink-not-followed` | walk | any symlink, of any name (`src/walk/inventory.rs:162-167`) |
| `hidden-not-allowlisted` | walk | consumer's `hidden_path_is_excluded` returned true (`src/walk/inventory.rs:169-177`) |
| `non-utf8-path` | collector | `.md`/`.mdx` path that is not valid UTF-8 (`src/docs/corpus.rs:311-315`) |
| `unsupported-extension` | collector | non-document file that an `include` glob explicitly matched (`src/docs/corpus.rs:317-321`) |
| `excluded` | collector | an `exclude` glob matched (`src/docs/corpus.rs:323-326`) |
| `not-included` | collector | no `include` glob matched (`src/docs/corpus.rs:327-330`) |
| `oversized` | capture | file exceeds `max_file_bytes` (4 MiB) |
| `non-utf8` | capture | bytes are not valid UTF-8 |
| `read-error` | capture | permanent I/O failure, with `detail` |
| `indexed` | capture | captured and parsed |

The split matters for what the vocabulary can express: the walk layer chooses the subject and rule strings for the exclusions it decides and passes `None` for `detail` at all four sites, so a consumer can log those rows or not, but cannot rename them or annotate them. A pruned path reaches the collector only as a decision, never as a candidate. Decision rows are deliberately narrow for *files*: `unsupported-extension` fires only when an include glob matched a non-document path (`src/docs/corpus.rs:317-321`), so ordinary `.ts` files never appear in `jscout docs status`. The symlink branch is the exception and is not narrowed this way — a symlink named `foo.ts`, or a bare directory symlink, produces a `symlink-not-followed` row whenever the consumer bit is live, which is exactly what `symlinks_are_reported_and_not_followed` (`src/docs/corpus.rs:1899`) asserts for a link named `linked`. Non-UTF-8 paths carry a lossless base64 encoding beside a lossy display path (`encode_native_path`, `src/docs/corpus.rs:642`).

Globs are compiled with `literal_separator(true)`, `case_insensitive(false)`, `backslash_escape(true)` (`src/docs/corpus.rs:521-535`), and patterns starting with `!`, ending with `/`, or containing unescaped braces are rejected outright (`src/docs/corpus.rs:537-548`), at config load (`src/config/load.rs:419`) and again at collector construction (`src/docs/corpus.rs:269`). Admission is therefore one deterministic pass with no ordering semantics between patterns — and consequently no way to write "everything under `docs/` except drafts" as a single pattern. `is_document_path` compares the raw `OsStr` extension against `md` and `mdx` (`src/docs/corpus.rs:581`), so `README.MD` is never a document even on a case-insensitive filesystem; `glob_contract_is_pinned` (`src/docs/corpus.rs:1739`) exists to keep that from drifting.

## Two-phase capture

The walk collects candidates; it does not read anything. That deferral is what `finish(self, root)` (`src/walk.rs:72`) exists for — it is the one consumer callback that runs after the stack has drained, and `DocumentationCollector::finish` (`src/docs/corpus.rs:337`) spends it on `acquire_candidates` (`src/docs/corpus.rs:354`), which sorts candidates by normalized path bytes and then calls `capture_file` (`src/docs/corpus.rs:475`) per file. Capture opens with `O_NOFOLLOW | O_NONBLOCK` on unix (`src/docs/corpus.rs:490-498`), re-checks the file type *through the descriptor*, and reads `max_bytes + 1` bytes so an oversized file is classified without reading all of it. The `capture` closure is a `DocumentationCollector` field (`src/docs/corpus.rs:250`) reached through the test-only `scan_repository_with_capture` (`src/docs/corpus.rs:164`), purely so tests can simulate read failures and type changes without a filesystem — and keeping it here rather than in `walk` is why the walk layer needs no test hook of its own.

TOCTOU is classified rather than swallowed, and the classification has four distinct outcomes:

- **Decision** — oversized, non-UTF-8, or a permanent read error. The rest of the corpus proceeds.
- **`walk::WalkRejection`** — a permanent walk-stage failure on a directory or entry, recorded with `stage: "walk"` (`src/walk.rs:24-28`, `src/walk/inventory.rs:119-123`, `:232-238`). The type belongs to `walk`, not to `docs`; `RepositoryCorpus` carries the same values through without reclassifying them.
- **Hard abort of the whole index** — `CapturedFile::NotRegular` (the type changed between walk and open), an `ELOOP` no-follow violation, a retryable error, or an inventory race detected during capture (`src/docs/corpus.rs:359-416`). A file that was a regular file during the walk and is a symlink during capture cannot be reported as "skipped", because the snapshot would then be a mix of two filesystem states. The inventory-race case inverts across the two phases and that inversion is the point: during traversal a race means "absent, reconcile later" (`src/walk/inventory.rs:112`), but after a path has been admitted to the candidate list it means the snapshot no longer matches the filesystem it was derived from.
- **A fourth, easily missed abort**: `inspect_special_file` (`src/docs/corpus.rs:298-308`) is the only consumer callback that returns `Result`, and the walk propagates it with `?` (`src/walk/inventory.rs:190-196`). It calls `ensure_regular_inventory_file` (`src/docs/corpus.rs:459`), which `bail!`s when a non-file, non-directory, non-symlink entry has a `.md`/`.mdx` extension. A FIFO named `notes.md` inside an active docs subtree wedges the entire index; the same FIFO named `notes.txt` is silently skipped. The asymmetry is deliberate and tested both ways (`src/docs/corpus.rs:1918, :1929`), but it is a real foot-gun.

The captured `Vec<u8>` is retained in `CapturedDocument` alongside the parsed `DocFile` (`src/docs/corpus.rs:57-61`), and parsing runs against that same buffer. The content hash, every block hash, and every chunk byte range therefore observe one filesystem state. The indexer re-derives the byte length, re-hashes with BLAKE3, and re-derives every non-stub chunk's embedding identity, `ensure!`ing each matches the parser's metadata (`src/indexer.rs:923-931, :1010-1026`) — a trust-but-verify seam between parse and insert. The cost is that the entire repository's Markdown is held in memory for the duration of the index pass, bounded only by the per-file 4 MiB cap. That cap is a `CorpusOptions` field but the indexer always takes the default via `..CorpusOptions::default()` (`src/indexer.rs:385-392`), so it is effectively hardcoded with no `[docs]` key.

## Parsing prose

`parse_document` (`src/docs/corpus.rs:892`) hashes the raw bytes first, then strips a UTF-8 BOM for text purposes only — offsets stay relative to the raw buffer via `body_base` (`src/docs/corpus.rs:900`). Front matter is recognized only when the *first* logical line is exactly `---` and some later logical line is exactly `---`, **and** the enclosed YAML deserializes to a `Mapping`; anything else yields `malformed_as_body` with `body_start = 0`, re-reading the delimiters as ordinary Markdown (`src/docs/corpus.rs:713-758`). `logical_lines` (`src/docs/corpus.rs:803`) tracks content end and full end separately so LF and CRLF compare identically without per-line allocation. Only a string `title`, a string `description`, and a string-or-all-string-sequence `tags` are extracted. A leading blank line defeats recognition entirely: byte 0 is not `---`, so the state is `absent`, the delimiters become a thematic break, and the YAML becomes visible body text.

Comment removal runs before structure. `protected_code_ranges` (`src/docs/corpus.rs:1048`) collects fenced/indented code blocks and inline code spans; `comment_removals` (`src/docs/corpus.rs:1076`) then scans raw bytes for `<!-- -->` always and `{/* */}` only for `.mdx`, skipping protected ranges and retaining unclosed openers. This is a byte scan with a skip list, not a parse — a `<!--` inside an HTML attribute value is still removed. Blocks keep both representations: `body` is the exact source slice with original line endings, `rendered_body` is comment-stripped and LF-normalized by `render_source_range` (`src/docs/corpus.rs:1265`), which also trims leading and trailing newlines. Retrieval and embedding see the rendered form; the raw form remains available as evidence.

Structure comes from `pulldown-cmark` with `Options::ENABLE_TABLES` and nothing else (`src/docs/corpus.rs:1044`). Footnotes, strikethrough, and task lists are parsed as plain CommonMark and land inside paragraph blocks as literal text. `document_items` (`src/docs/corpus.rs:1132`) iterates `into_offset_iter` and uses a `consumed_until` cursor to suppress nested events, so a list's inner paragraphs do not become sibling blocks. `Event::Rule` becomes a `Boundary`; `Event::Html` bypasses `body_block_kind` and emits a `BlockKind::Html` body item directly (`src/docs/corpus.rs:1171-1177`).

MDX gets no JSX-aware parser. JSX elements, props, and expressions stay as authored text so a component name remains searchable in BM25. Only two things are dropped: unprotected `{/* */}` comments, and the contiguous *leading* run of paragraph blocks that `is_esm_only` (`src/docs/corpus.rs:1184`) confirms oxc parses as pure module declarations — no hashbang, no directives, non-empty body, every statement a module declaration. `mdx_preamble_open` opens only for `.mdx` and closes permanently at the first heading, boundary, or non-ESM block (`src/docs/corpus.rs:917, :922, :950, :964`). A `"use client";` directive, a fence, or one line of prose closes it, after which `export const metadata = {…}` is retained verbatim as a paragraph. Running the full oxc parser on leading MDX paragraphs also means prose parsing is not purely lexical.

Heading state is a six-slot array (`src/docs/corpus.rs:909`). Setting level *n* writes slot *n−1* and clears every slot below it; `breadcrumb` joins the live slots with `" > "`; `nearest_heading` is the deepest live slot (`src/docs/corpus.rs:925-934, :966-971`). There are no slugs and no HTML anchors anywhere in `src/docs/` — a chunk's address is its byte span plus its line span plus its breadcrumb. The document `title` falls back front matter → first H1 → file stem (`src/docs/corpus.rs:996-1002`).

Two counters advance separately and this is load-bearing. `section` increments at every heading **and** every thematic break (`src/docs/corpus.rs:923, :951`); it gates chunk merging. `heading_instance` increments only at headings, with 0 meaning preamble (`src/docs/corpus.rs:924`); it gates `same_heading_ordinal`. Because they diverge, a `---` flushes the current chunk without renumbering that heading's chunks — pinned by `same_heading_ordinals_use_heading_instances_and_ignore_thematic_boundaries` (`src/docs/corpus.rs:2404`).

## Chunking

What to look for in the pipeline diagram: the oversize test happens *before* merging, not after, and the split path never rejoins the merge path.

```mermaid
flowchart TB
  BLK["ParsedBlock<br/>kind, section, heading_instance"] --> TRUNC["truncate heading at<br/>HEADING_CONTEXT_MAX_BYTES 1024"]
  TRUNC --> PT["provider_text =<br/>heading + blank line + rendered_body"]
  PT --> BIG{"len > HARD_MAX_BYTES<br/>24000?"}
  BIG -->|yes| FLUSH["flush current draft"]
  FLUSH --> SPL["split_block<br/>corpus.rs:1408"]
  SPL --> NAT{"native boundaries?"}
  NAT -->|code| NL1["newline ends"]
  NAT -->|table| TR["table row ends"]
  NAT -->|list| LI["depth-1 item ends"]
  NAT -->|none| ANY["any newline"]
  ANY --> UTF["UTF-8 char boundary,<br/>never splitting CRLF"]
  NL1 --> FRAG["fragment + synthetic context"]
  TR --> FRAG
  LI --> FRAG
  UTF --> FRAG
  BIG -->|no| MERGE{"same section AND<br/>draft < 2400 AND<br/>merged + 2 <= 4000?"}
  MERGE -->|yes| APPEND["append with two LFs"]
  MERGE -->|no| NEWD["flush, start new draft"]
  APPEND --> DRAFTS["ChunkDraft list"]
  NEWD --> DRAFTS
  FRAG --> DRAFTS
  DRAFTS --> FIN["assign ordinal,<br/>same_heading_ordinal,<br/>embedding_text, identity"]
```

`build_chunks` (`src/docs/corpus.rs:1315`) walks blocks in order. Before anything else it truncates the block's nearest heading to `HEADING_CONTEXT_MAX_BYTES` = 1,024 with the suffix `"\n[heading truncated]"` (`src/docs/corpus.rs:1325-1327`) — the truncated string is what lands in `DocChunk::nearest_heading` and what feeds both `provider_text` and `embedding_identity`. A draft absorbs the next block only while all three conditions hold: identical `section`, accumulated rendered body under `TARGET_BYTES` = 2,400, and merged length plus the two-byte `"\n\n"` joiner within `MERGE_MAX_BYTES` = 4,000 (`src/docs/corpus.rs:1345-1349`). A chunk boundary is therefore a section change or a size threshold, never a heading crossing. `MERGE_MAX_BYTES` sitting well above `TARGET_BYTES` is what lets an atomic 5 KB paragraph stay whole rather than being cut mid-sentence (`src/docs/corpus.rs:2436`).

A block whose provider text already exceeds `HARD_MAX_BYTES` = 24,000 bypasses merging entirely and goes to `split_block` (`src/docs/corpus.rs:1408`) with a body budget of `HARD_MAX_BYTES` minus the heading overhead. Splitting tries the whole remainder first, then the last fitting *native* boundary, then any newline, then a UTF-8 character boundary. Native boundaries are format-specific and are computed as **ends**, not starts: newline ends for fenced and indented code, `Tag::TableRow` start-event offset ends for tables, depth-1 `Tag::Item` offset ends for lists, each extended over a following CR and LF (`parser_boundaries`, `src/docs/corpus.rs:1601-1632`). Under `into_offset_iter` a `Start` event's range spans the whole row or item, so this cuts between rows, not before them. Every fragment re-prepends the block's synthetic context — `[fence lang]` or `[table col | col]`, capped at 1,024 bytes (`src/docs/corpus.rs:854-867`) — so the reader of fragment 7 of a 40 KB table still sees the column names. Synthetic context exists for exactly two block kinds; everything else gets an empty string.

Two assertions keep splitting honest. `assert!(end > start, "hard-bound splitting must make progress")` (`src/docs/corpus.rs:1475`) is a real runtime assert, backed by `next_utf8_boundary` guaranteeing at least one character advances (`src/docs/corpus.rs:1566-1575`), and `splits_crlf` (`src/docs/corpus.rs:1577`) prevents a CR from being separated from its LF. Fragment byte ranges partition the original block exactly — each fragment's `source_end` is the next fragment's `source_start` (`src/docs/corpus.rs:2448`). A `debug_assert!` at `src/docs/corpus.rs:1379` checks the resulting embedding text against the hard cap.

A document that produced no blocks at all — heading-only, or entirely front matter — gets one stub chunk spanning the whole file with `is_stub = true`, no embedding text, and no identity (`src/docs/corpus.rs:1005-1026`). Its breadcrumb is every heading's text joined with `" > "` in document order, which for five sibling H2s reads like a five-level nesting that does not exist.

## Identity, and why the code chunker could not be reused

`embedding_identity` (`src/docs/corpus.rs:228`) is BLAKE3 over `b"jscout-doc-embedding-v1\0"`, a one-byte heading-presence flag, then big-endian length-prefixed heading and rendered body. Path, breadcrumb, and byte offsets are all excluded, so a file rename or an edit to an ancestor heading reuses cached vectors — only text that actually reaches the provider is in the preimage. The consequence is that two identical passages under identically-named headings in different files share one cached vector; that is intended reuse, but the cache cannot tell them apart. The preimage is frozen byte-for-byte by `embedding_serialization_matches_the_normative_preimage` (`src/docs/corpus.rs:1758`), so extending it requires bumping `docs::CHUNK_FORMAT_VERSION` (currently `"documentation-v1"`, `src/docs/mod.rs:11`), which is checked independently of the code extractor version (`src/indexer.rs:822`) and reprocesses documentation without invalidating code rows.

The code chunker (`src/chunk.rs`) could not be reused because almost every input it depends on is absent from prose. It walks an oxc AST and splits at declaration boundaries; Markdown has no declarations, and `pulldown-cmark` yields an event stream, not a tree with spans it can subdivide. It budgets in estimated tokens (`TARGET_TOKENS` 1200, `MAX_TOKENS` 2000, `src/chunk.rs:9-10`) against a `&str`; the docs chunker budgets in bytes against the exact `provider_text` string, because bytes are the only measure it can compute exactly over the raw captured buffer. It emits `name`, `scope_chain`, `symbols`, and `file_imports`, all of which are NULL or empty for a documentation chunk (`src/indexer.rs:958-962`). And it feeds `chunks_fts` and the exact-identifier tiers, while prose feeds `docs_fts` only. Even the line-number helper is a separate implementation — `docs::corpus::LineIndex` (`src/docs/corpus.rs:1669`) indexes raw bytes and counts CR, LF, and CRLF as one break each, because chunk line spans must address the file on disk rather than a rendered string.

Byte budgets are a proxy for token budgets, and an imperfect one: CJK or heavily-escaped prose produces fewer tokens per chunk than ASCII at the same byte size. Two further limits are worth naming. `doc_chunk_meta.ordinal` stores `same_heading_ordinal`, not the global `ordinal` (`src/indexer.rs:1033-1043`) — the global ordinal is contiguous from 0 and `ensure!`d at insert (`src/indexer.rs:976-980`) but is never persisted, surviving only as `chunks` row order. And for a stub chunk, `rendered_body` is empty, so `docs_fts.body` is empty while `chunks.content` holds the whole raw file (`src/indexer.rs:1030, :1044-1052`); such a document is findable by title, path, and breadcrumb, but not by its own text.
