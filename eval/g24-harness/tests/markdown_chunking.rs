//! G24 corpus/chunking claims (M1-M11), measured against `g24_harness::md`.
//!
//! Methodology: every assertion below states what the PLAN claims, and where
//! reality disagreed the assertion was rewritten to state the OBSERVED
//! behavior with a comment naming the claim and the divergence. No assertion
//! was weakened to obtain a green run.
//!
//! Plan sources: docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md "Markdown corpus specification" and the PLAN.md
//! G24 entry, decision 2 (corpus) and decision 3 (embedding identity).

use std::collections::BTreeMap;

use g24_harness::md::{self, BlockKind, ChunkBounds, Document, FrontMatterState};

// ---------------------------------------------------------------------------
// Local helpers (the core must not be modified, so these live here)
// ---------------------------------------------------------------------------

/// Independently recomputed 1-based line number of a byte offset.
fn line_of(source: &str, byte: usize) -> usize {
    source.as_bytes()[..byte]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// Deterministic single-line prose filler of at least `approx` bytes. Contains
/// no Markdown structure, so it always parses as one paragraph.
fn filler(seed: &str, approx: usize) -> String {
    const WORDS: [&str; 10] = [
        "lorem",
        "ipsum",
        "dolor",
        "sit",
        "amet",
        "consectetur",
        "adipiscing",
        "elit",
        "sed",
        "quia",
    ];
    let mut out = String::with_capacity(approx + 32);
    out.push_str(seed);
    let mut index = 0usize;
    while out.len() < approx {
        out.push(' ');
        out.push_str(WORDS[index % WORDS.len()]);
        index += 1;
    }
    out
}

fn identities(doc: &Document) -> Vec<String> {
    doc.chunks
        .iter()
        .map(|chunk| chunk.embedding_identity.clone())
        .collect()
}

fn body_chunks(doc: &Document) -> Vec<&md::Chunk> {
    doc.chunks.iter().filter(|chunk| !chunk.is_stub).collect()
}

/// The multiset of block content hashes, so "untouched blocks kept identity"
/// can be checked even when order or grouping moves.
fn hash_multiset(doc: &Document) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for block in &doc.blocks {
        *out.entry(block.content_hash.clone()).or_insert(0) += 1;
    }
    out
}

/// Full-file assertion used by several tests: every block and every chunk span
/// slices back to the exact original bytes, spans are ordered, and line numbers
/// agree with an independent count.
fn assert_spans_round_trip(source: &str, doc: &Document, label: &str) {
    for block in &doc.blocks {
        assert!(
            source.is_char_boundary(block.byte_start),
            "{label}: block start off char boundary"
        );
        assert!(
            source.is_char_boundary(block.byte_end),
            "{label}: block end off char boundary"
        );
        assert_eq!(
            &source[block.byte_start..block.byte_end],
            block.raw,
            "{label}: block raw round-trip"
        );
        assert_eq!(
            block.content_hash,
            md::hash_hex(block.raw.as_bytes()),
            "{label}: content hash of raw"
        );
        assert_eq!(
            block.line_start,
            line_of(source, block.byte_start),
            "{label}: block line_start"
        );
        assert_eq!(
            block.line_end,
            line_of(source, block.byte_end - 1),
            "{label}: block line_end"
        );
    }
    for pair in doc.blocks.windows(2) {
        assert!(
            pair[0].byte_end <= pair[1].byte_start,
            "{label}: blocks overlap"
        );
    }
    for chunk in body_chunks(doc) {
        assert!(
            source.is_char_boundary(chunk.byte_start),
            "{label}: chunk start off char boundary"
        );
        assert!(
            source.is_char_boundary(chunk.byte_end),
            "{label}: chunk end off char boundary"
        );
        let slice = &source[chunk.byte_start..chunk.byte_end];
        assert_eq!(
            chunk.line_start,
            line_of(source, chunk.byte_start),
            "{label}: chunk line_start"
        );
        assert_eq!(
            chunk.line_end,
            line_of(source, chunk.byte_end - 1),
            "{label}: chunk line_end"
        );
        if chunk.blocks.len() == 1 && doc.blocks[chunk.blocks[0]].byte_start != chunk.byte_start {
            // A fragment of a split block: the span is a sub-slice of the block.
            let block = &doc.blocks[chunk.blocks[0]];
            assert!(chunk.byte_start >= block.byte_start && chunk.byte_end <= block.byte_end);
        }
        // Merged or whole chunks start and end on their contributing blocks.
        let first = &doc.blocks[*chunk.blocks.first().unwrap()];
        let last = &doc.blocks[*chunk.blocks.last().unwrap()];
        assert!(
            chunk.byte_start >= first.byte_start,
            "{label}: chunk starts before its first block"
        );
        assert!(
            chunk.byte_end <= last.byte_end,
            "{label}: chunk ends after its last block"
        );
        assert!(!slice.is_empty(), "{label}: empty chunk slice");
    }
}

// ---------------------------------------------------------------------------
// M1 - front matter is only a delimited top-level YAML MAPPING
// ---------------------------------------------------------------------------

#[test]
fn m1_front_matter_requires_a_top_level_mapping() {
    // Plan: "Recognized only when the file begins with `---`, has a valid
    // closing delimiter, parses as YAML, and produces a top-level mapping."
    let mapping = "---\ntitle: Real Title\ndescription: Real description.\ntags:\n  - alpha\n  - beta\n---\n\n\
                   # Heading\n\nBody paragraph.\n";
    let (fm, body_start) = md::parse_front_matter(mapping);
    assert_eq!(fm.state, Some(FrontMatterState::Parsed));
    assert_eq!(fm.title.as_deref(), Some("Real Title"));
    assert_eq!(fm.description.as_deref(), Some("Real description."));
    assert_eq!(fm.tags, vec!["alpha".to_string(), "beta".to_string()]);
    assert!(
        body_start > 0,
        "recognized front matter must move the body start past the closing delimiter"
    );
    assert_eq!(
        &mapping[body_start..body_start + 1],
        "\n",
        "body starts just past the closing `---` line"
    );

    // Plan: "Valid front matter is never emitted as a body chunk."
    let doc = md::index_document("docs/mapping.md", mapping, &ChunkBounds::default());
    for block in &doc.blocks {
        assert!(
            block.byte_start >= body_start,
            "front-matter bytes leaked into a body block: {:?}",
            block.raw
        );
    }
    for chunk in body_chunks(&doc) {
        assert!(
            !chunk.rendered_body.contains("Real description."),
            "front matter leaked into a chunk body"
        );
        assert!(
            !chunk.rendered_body.contains("tags:"),
            "front matter leaked into a chunk body"
        );
    }
    assert_eq!(
        doc.title.as_deref(),
        Some("Real Title"),
        "front-matter title wins the title fallback order"
    );

    // Plan: "A valid YAML scalar or sequence is not front matter and remains
    // ordinary body text." Scalar, kept off the setext path by a blank line.
    let scalar = "---\njust a scalar\n\n---\n\nBody after.\n";
    let (fm, body_start) = md::parse_front_matter(scalar);
    assert_eq!(
        fm.state,
        Some(FrontMatterState::MalformedAsBody),
        "a YAML scalar is not front matter"
    );
    assert_eq!(
        body_start, 0,
        "non-front-matter leaves the whole file as body"
    );
    assert_eq!(fm.title, None);
    assert!(fm.tags.is_empty());
    let doc = md::index_document("docs/scalar.md", scalar, &ChunkBounds::default());
    assert!(
        doc.chunks
            .iter()
            .any(|chunk| chunk.rendered_body.contains("just a scalar")),
        "scalar text must stay retrievable as body: {:?}",
        doc.chunks
            .iter()
            .map(|c| c.rendered_body.as_str())
            .collect::<Vec<_>>()
    );

    // Sequence.
    let sequence = "---\n- alpha\n- beta\n---\n\nBody after.\n";
    let (fm, body_start) = md::parse_front_matter(sequence);
    assert_eq!(
        fm.state,
        Some(FrontMatterState::MalformedAsBody),
        "a YAML sequence is not front matter"
    );
    assert_eq!(body_start, 0);
    let doc = md::index_document("docs/sequence.md", sequence, &ChunkBounds::default());
    assert!(
        doc.chunks
            .iter()
            .any(|chunk| chunk.rendered_body.contains("- alpha")),
        "sequence text must stay retrievable as body"
    );
    assert!(
        doc.blocks
            .iter()
            .any(|block| block.kind == BlockKind::List && block.raw.contains("- alpha")),
        "the sequence parses as an ordinary Markdown list in the body"
    );

    // OBSERVED, not a plan claim: when the closing `---` directly follows a
    // one-line scalar, CommonMark reads that `---` as a SETEXT underline, so
    // the scalar becomes an H2 heading rather than a paragraph. It is still
    // body text (the plan's claim), but it is body *structure*, and it silently
    // becomes a heading that scopes everything after it.
    let tight_scalar = "---\njust a scalar\n---\n\nBody after.\n";
    let (fm, _) = md::parse_front_matter(tight_scalar);
    assert_eq!(fm.state, Some(FrontMatterState::MalformedAsBody));
    let doc = md::index_document("docs/tight.md", tight_scalar, &ChunkBounds::default());
    let heading = doc
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Heading)
        .expect("setext heading produced from the closing delimiter");
    assert_eq!(heading.heading_level, Some(2));
    assert_eq!(heading.nearest_heading.as_deref(), Some("just a scalar"));
    assert_eq!(
        doc.chunks
            .iter()
            .filter(|c| !c.is_stub)
            .map(|c| c.nearest_heading.clone())
            .collect::<Vec<_>>(),
        vec![Some("just a scalar".to_string())],
        "the rejected front matter now supplies the nearest heading of the real body"
    );

    // A mapping whose values are the wrong types: recognized as front matter,
    // but only the admitted scalar shapes are used.
    let typed =
        "---\ntitle: 42\ndescription:\n  nested: value\ntags:\n  - ok\n  - 7\n---\n\nBody.\n";
    let (fm, body_start) = md::parse_front_matter(typed);
    assert_eq!(
        fm.state,
        Some(FrontMatterState::Parsed),
        "a mapping is front matter even with odd values"
    );
    assert!(body_start > 0);
    assert_eq!(fm.title, None, "a non-string title is ignored");
    assert_eq!(
        fm.description, None,
        "a mapping-valued description is ignored"
    );
    assert!(
        fm.tags.is_empty(),
        "a mixed sequence is not `a sequence of scalar strings`"
    );
    let doc = md::index_document("docs/typed.md", typed, &ChunkBounds::default());
    assert_eq!(
        doc.title.as_deref(),
        Some("typed"),
        "title falls through to the file stem"
    );
}

// ---------------------------------------------------------------------------
// M2 - malformed/unterminated front matter is body text, never a rejection
// ---------------------------------------------------------------------------

#[test]
fn m2_malformed_front_matter_degrades_to_body_text() {
    // Plan: "Malformed or unterminated front matter is ordinary Markdown body
    // text, reported by `docs status` as `front_matter=malformed_as_body`, not
    // a rejection."
    let cases: [(&str, &str); 4] = [
        (
            "unterminated",
            "---\ntitle: Never Closed\ndescription: dangling\n\nBody paragraph here.\n",
        ),
        (
            "invalid-yaml",
            "---\ntitle: [unclosed flow\n---\n\nBody paragraph here.\n",
        ),
        (
            "tab-indent-yaml",
            "---\ntitle: ok\n\tbad: \tindent\n---\n\nBody paragraph here.\n",
        ),
        ("only-open-delimiter", "---\n"),
    ];

    for (label, source) in cases {
        let (fm, body_start) = md::parse_front_matter(source);
        assert_eq!(
            fm.state,
            Some(FrontMatterState::MalformedAsBody),
            "{label}: malformed front matter must report malformed_as_body"
        );
        assert_eq!(
            body_start, 0,
            "{label}: malformed front matter stays in the body"
        );
        assert_eq!(
            fm.title, None,
            "{label}: no fields are harvested from malformed front matter"
        );
        assert_eq!(fm.description, None);
        assert!(fm.tags.is_empty());

        // Not a rejection: indexing still succeeds and still yields rows.
        let doc = md::index_document("docs/mal.md", source, &ChunkBounds::default());
        assert!(
            !doc.chunks.is_empty(),
            "{label}: a malformed document still produces searchable rows"
        );
        assert_spans_round_trip(source, &doc, label);
    }

    // The malformed text itself stays retrievable.
    let unterminated = "---\ntitle: Never Closed\ndescription: dangling\n\nBody paragraph here.\n";
    let doc = md::index_document("docs/mal.md", unterminated, &ChunkBounds::default());
    let bodies: Vec<&str> = doc
        .chunks
        .iter()
        .map(|chunk| chunk.rendered_body.as_str())
        .collect();
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("title: Never Closed")),
        "malformed YAML is body text: {bodies:?}"
    );
    assert!(bodies
        .iter()
        .any(|body| body.contains("Body paragraph here.")));
    assert_eq!(
        doc.title.as_deref(),
        Some("mal"),
        "no title is harvested; the stem is used"
    );

    // "---\n" alone: only an opening delimiter. It is malformed front matter
    // AND, as Markdown, a thematic break, which carries no retrieval text, so
    // the document is body-empty and takes the M8 stub path.
    let only_open = md::index_document("docs/open.md", "---\n", &ChunkBounds::default());
    assert_eq!(only_open.blocks.len(), 1);
    assert_eq!(only_open.blocks[0].kind, BlockKind::ThematicBreak);
    assert_eq!(only_open.chunks.len(), 1);
    assert!(
        only_open.chunks[0].is_stub,
        "a body-empty malformed document yields a stub, not a rejection"
    );
}

// ---------------------------------------------------------------------------
// M3 - chunks never cross heading boundaries
// ---------------------------------------------------------------------------

fn heading_boundary_fixture() -> String {
    let mut source = String::new();
    source.push_str("# Top Level\n\nIntro under H1.\n\n");
    source.push_str("## Alpha\n\nAlpha one.\n\nAlpha two.\n\n");
    source.push_str("### Alpha Deep\n\nDeep one.\n\n");
    source.push_str("#### Alpha Deeper\n\nDeeper one.\n\n");
    source.push_str("## Beta\n\nBeta one.\n");
    // An ATX heading interrupting a paragraph with no blank line.
    source.push_str("### Beta Deep\n\nBeta deep one.\n\n");
    source.push_str("Trailing paragraph.\n\n");
    // Setext headings also establish boundaries.
    source.push_str("Setext Heading One\n==================\n\nUnder setext one.\n\n");
    source.push_str("Setext Heading Two\n------------------\n\nUnder setext two.\n");
    source
}

#[test]
fn m3_chunks_never_cross_heading_boundaries() {
    // Plan: "Chunks never cross heading boundaries."
    let source = heading_boundary_fixture();
    let doc = md::index_document("docs/headings.md", &source, &ChunkBounds::default());
    assert_spans_round_trip(&source, &doc, "m3");

    let heading_starts: Vec<usize> = doc
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Heading)
        .map(|block| block.byte_start)
        .collect();
    assert!(
        heading_starts.len() >= 8,
        "fixture should have many headings: {}",
        heading_starts.len()
    );

    for chunk in body_chunks(&doc) {
        for start in &heading_starts {
            assert!(
                !(*start > chunk.byte_start && *start < chunk.byte_end),
                "a heading at byte {start} lies inside chunk {:?}",
                chunk.rendered_body
            );
        }
        // Every contributing block agrees on breadcrumb and nearest heading.
        for index in &chunk.blocks {
            let block = &doc.blocks[*index];
            assert_eq!(
                block.breadcrumb, chunk.breadcrumb,
                "chunk merged blocks from different sections"
            );
            assert_eq!(
                block.nearest_heading.as_deref(),
                chunk.nearest_heading.as_deref(),
                "chunk merged blocks with different nearest headings"
            );
        }
    }

    // The boundary really is tested: at default bounds these tiny paragraphs
    // WOULD merge if the heading did not stop them.
    let merged_pairs = body_chunks(&doc)
        .iter()
        .filter(|chunk| chunk.blocks.len() > 1)
        .count();
    let all_bodies: usize = doc
        .blocks
        .iter()
        .filter(|block| block.kind.is_retrieval_bearing())
        .count();
    println!(
        "M3: {} body blocks -> {} chunks ({merged_pairs} merged)",
        all_bodies,
        body_chunks(&doc).len()
    );
    assert!(
        merged_pairs >= 1,
        "fixture must contain at least one same-heading merge to prove merging is on"
    );
    assert!(
        body_chunks(&doc).len() > 1,
        "with merging on, headings must still split the document into several chunks"
    );

    // Setext headings scope the same way as ATX headings.
    let setext_chunk = body_chunks(&doc)
        .into_iter()
        .find(|chunk| chunk.rendered_body.contains("Under setext two."))
        .expect("setext section chunk");
    assert_eq!(
        setext_chunk.nearest_heading.as_deref(),
        Some("Setext Heading Two")
    );
    assert!(!setext_chunk.rendered_body.contains("Under setext one."));
}

// ---------------------------------------------------------------------------
// M4 - exact byte round trip on CRLF / tabs / multi-byte UTF-8
// ---------------------------------------------------------------------------

fn hostile_encoding_fixture() -> String {
    // CRLF everywhere, hard tabs inside a paragraph and as an indented code
    // block, emoji and CJK in headings, body, table and list.
    let mut source = String::new();
    source.push_str("---\r\ntitle: 概要 🚀\r\ntags:\r\n  - 日本語\r\n---\r\n\r\n");
    source.push_str("# 概要 🚀\r\n\r\n");
    source.push_str("こんにちは 世界 🎉 — a paragraph with\ttabs\tand emoji 🧪.\r\n\r\n");
    source.push_str("## 表 テーブル\r\n\r\n");
    source.push_str("| 名前 | 値 |\r\n| --- | --- |\r\n| 🚀 | 1 |\r\n| 世界 | 2 |\r\n\r\n");
    source.push_str("- リスト 🍎\r\n- リスト 🍊\r\n\r\n");
    source.push_str("```rust\r\nfn main() { println!(\"héllo 🌍\"); }\r\n```\r\n\r\n");
    source.push_str("\tindented\tcode with 漢字\r\n\r\n");
    source.push_str("> 引用 quote 🌱\r\n\r\n");
    source.push_str("Ünïcödé tail paragraph — ends without newline");
    source
}

#[test]
fn m4_every_span_slices_back_to_the_original_bytes() {
    // Plan (field composition): source spans are exact byte/line spans into the
    // indexed file; hit content resolves by slicing the same bytes.
    let source = hostile_encoding_fixture();
    let doc = md::index_document("docs/概要 🚀.md", &source, &ChunkBounds::default());
    assert_spans_round_trip(&source, &doc, "m4-default");

    assert!(source.contains('\r'), "fixture must really contain CRLF");
    assert!(source.contains('\t'), "fixture must really contain tabs");
    assert!(
        !source.is_ascii(),
        "fixture must really contain multi-byte UTF-8"
    );

    // Blocks cover every kind we care about and no span ends inside a
    // multi-byte character.
    let kinds: Vec<BlockKind> = doc.blocks.iter().map(|block| block.kind).collect();
    println!("M4: block kinds = {kinds:?}");
    for wanted in [
        BlockKind::Heading,
        BlockKind::Table,
        BlockKind::List,
        BlockKind::CodeFenced,
        BlockKind::BlockQuote,
    ] {
        assert!(kinds.contains(&wanted), "fixture should produce {wanted:?}");
    }

    // Trailing carriage returns are trimmed off spans, so a block never ends on
    // a bare `\r` and `raw` is still an exact slice.
    for block in &doc.blocks {
        assert!(
            !block.raw.ends_with('\r'),
            "block span kept a trailing CR: {:?}",
            block.raw
        );
        assert!(!block.raw.ends_with('\n'));
    }

    // Same guarantee with a tightened hard bound, i.e. once fragments exist.
    // The serialized heading prefix here is ~34 bytes, so hard_max 60 leaves a
    // ~26-byte body budget: every multi-byte block must split.
    let tight = ChunkBounds {
        target: 20,
        normal_max: 40,
        hard_max: 60,
        ..ChunkBounds::default()
    };
    let split_doc = md::index_document("docs/概要 🚀.md", &source, &tight);
    assert_spans_round_trip(&source, &split_doc, "m4-tight");
    let fragments: Vec<&md::Chunk> = split_doc
        .chunks
        .iter()
        .filter(|chunk| !chunk.is_stub && chunk.blocks.len() == 1)
        .filter(|chunk| {
            let block = &split_doc.blocks[chunk.blocks[0]];
            chunk.byte_start != block.byte_start || chunk.byte_end != block.byte_end
        })
        .collect();
    println!(
        "M4: {} fragments produced under a 120-byte hard bound",
        fragments.len()
    );
    assert!(
        !fragments.is_empty(),
        "the tight bound must actually force splitting"
    );
    for fragment in fragments {
        assert!(
            fragment
                .rendered_body
                .ends_with(&source[fragment.byte_start..fragment.byte_end]),
            "fragment rendered body must end with its exact source slice"
        );
    }
}

// ---------------------------------------------------------------------------
// M5 - THE KEY CLAIM: identity = hash(FORMAT_VERSION, nearest_heading, body)
// ---------------------------------------------------------------------------

const H1_OLD: &str = "Original Root Heading";
const H1_NEW: &str = "Renamed Root Heading";

/// ~30-chunk document: an H1 with its own body, then thirteen deeper sections
/// at levels 2-4, each with body that merges into two chunks at default bounds.
fn blast_radius_document(h1: &str, front_matter_title: Option<&str>) -> String {
    let mut source = String::new();
    if let Some(title) = front_matter_title {
        source.push_str(&format!("---\ntitle: {title}\n---\n\n"));
    }
    source.push_str(&format!("# {h1}\n\n"));
    for index in 0..6 {
        source.push_str(&filler(&format!("root-p{index}"), 1_500));
        source.push_str("\n\n");
    }
    let sections: [(usize, &str); 13] = [
        (2, "Section One"),
        (3, "One Deep"),
        (2, "Section Two"),
        (3, "Two Deep"),
        (4, "Two Deeper"),
        (2, "Section Three"),
        (3, "Three Deep"),
        (2, "Section Four"),
        (3, "Four Deep"),
        (2, "Section Five"),
        (3, "Five Deep"),
        (2, "Section Six"),
        (3, "Six Deep"),
    ];
    for (section, (level, name)) in sections.iter().enumerate() {
        source.push_str(&format!("{} {name}\n\n", "#".repeat(*level)));
        for index in 0..4 {
            source.push_str(&filler(&format!("s{section}-p{index}"), 1_500));
            source.push_str("\n\n");
        }
    }
    source
}

#[test]
fn m5a_a_file_rename_changes_no_embedding_identity() {
    // Plan: "a file rename reuses every vector".
    let source = blast_radius_document(H1_OLD, Some("Doc Title"));
    let before = md::index_document(
        "docs/adr/original-name.md",
        &source,
        &ChunkBounds::default(),
    );
    let after = md::index_document(
        "handbook/v2/renamed-file.md",
        &source,
        &ChunkBounds::default(),
    );

    assert_eq!(
        identities(&before),
        identities(&after),
        "a rename must not change any embedding identity"
    );
    assert_ne!(
        before.path, after.path,
        "the rename must actually have happened"
    );
    assert_ne!(
        before.chunks[0].path, after.chunks[0].path,
        "path is per-chunk metadata and does change"
    );
    println!(
        "M5a: {} chunks, 0 identities changed by rename",
        before.chunks.len()
    );

    // Also true when the path is the only source of the title (no front matter,
    // no H1): the title metadata changes, the identities do not.
    let untitled = format!("## Only An H2\n\n{}\n", filler("body", 200));
    let before = md::index_document("a/old-stem.md", &untitled, &ChunkBounds::default());
    let after = md::index_document("b/new-stem.md", &untitled, &ChunkBounds::default());
    assert_eq!(before.title.as_deref(), Some("old-stem"));
    assert_eq!(
        after.title.as_deref(),
        Some("new-stem"),
        "the title really did change with the rename"
    );
    assert_eq!(
        identities(&before),
        identities(&after),
        "a title-only change must not change identity"
    );
}

#[test]
fn m5b_an_h1_rename_only_re_embeds_chunks_whose_nearest_heading_is_that_h1() {
    // Plan: "an H1 rename re-embeds only chunks whose nearest heading is that
    // H1". Measured blast radius on a ~30-chunk multi-level document.
    let bounds = ChunkBounds::default();
    let old_source = blast_radius_document(H1_OLD, Some("Doc Title"));
    let new_source = blast_radius_document(H1_NEW, Some("Doc Title"));
    let old_doc = md::index_document("docs/blast.md", &old_source, &bounds);
    let new_doc = md::index_document("docs/blast.md", &new_source, &bounds);

    assert_eq!(
        old_doc.chunks.len(),
        29,
        "fixture shape: 3 chunks under the H1 + 13 sections x 2 chunks"
    );
    assert_eq!(
        new_doc.chunks.len(),
        old_doc.chunks.len(),
        "renaming a heading must not regroup chunks"
    );

    let under_h1 = old_doc
        .chunks
        .iter()
        .filter(|chunk| chunk.nearest_heading.as_deref() == Some(H1_OLD))
        .count();
    assert_eq!(
        under_h1, 3,
        "chunks whose NEAREST heading is the renamed H1"
    );

    let changed: Vec<usize> = old_doc
        .chunks
        .iter()
        .zip(new_doc.chunks.iter())
        .enumerate()
        .filter(|(_, (old, new))| old.embedding_identity != new.embedding_identity)
        .map(|(index, _)| index)
        .collect();
    println!(
        "M5b: {} chunks total, blast radius = {} changed identities (indices {changed:?})",
        old_doc.chunks.len(),
        changed.len()
    );
    assert_eq!(
        changed.len(),
        3,
        "exactly the chunks whose nearest heading is the renamed H1 re-embed"
    );
    assert_eq!(
        changed,
        vec![0, 1, 2],
        "and they are precisely the H1's own body chunks"
    );

    // Every changed chunk is one whose nearest heading was the H1; every other
    // chunk keeps its identity even though its BREADCRUMB changed.
    for (index, (old, new)) in old_doc.chunks.iter().zip(new_doc.chunks.iter()).enumerate() {
        let nearest_is_h1 = old.nearest_heading.as_deref() == Some(H1_OLD);
        assert_eq!(
            old.embedding_identity != new.embedding_identity,
            nearest_is_h1,
            "chunk {index}: identity change must track the NEAREST heading only"
        );
        assert_eq!(
            old.rendered_body, new.rendered_body,
            "chunk {index}: body text is untouched by a rename"
        );
        if !nearest_is_h1 {
            assert_eq!(old.breadcrumb[0], H1_OLD);
            assert_eq!(
                new.breadcrumb[0], H1_NEW,
                "chunk {index}: the ancestor breadcrumb DID change"
            );
            assert_ne!(
                old.breadcrumb, new.breadcrumb,
                "chunk {index}: metadata changed but identity did not"
            );
        }
    }

    // 26 of 29 chunks keep their vector: the plan's stated reuse property.
    let reused = old_doc.chunks.len() - changed.len();
    assert_eq!(reused, 26);
}

#[test]
fn m5c_title_and_distant_ancestor_edits_are_metadata_only() {
    // Plan: "an ancestor-heading or title edit is metadata-only".
    let bounds = ChunkBounds::default();
    let base = blast_radius_document(H1_OLD, Some("Doc Title"));
    let retitled = blast_radius_document(H1_OLD, Some("Completely Different Title"));
    let base_doc = md::index_document("docs/blast.md", &base, &bounds);
    let retitled_doc = md::index_document("docs/blast.md", &retitled, &bounds);

    assert_ne!(
        base_doc.title, retitled_doc.title,
        "the title really did change"
    );
    assert_eq!(
        identities(&base_doc),
        identities(&retitled_doc),
        "a title edit changes no embedding identity"
    );

    // A distant ancestor: rename the H2 above an H3 section and check that the
    // H3's chunks are untouched while the H2's own chunks re-embed.
    let renamed_ancestor = base.replace("## Section Two\n", "## Section Two Renamed\n");
    assert_ne!(
        renamed_ancestor, base,
        "the ancestor rename must actually apply"
    );
    let ancestor_doc = md::index_document("docs/blast.md", &renamed_ancestor, &bounds);

    let mut ancestor_changed = 0usize;
    for (index, (old, new)) in base_doc
        .chunks
        .iter()
        .zip(ancestor_doc.chunks.iter())
        .enumerate()
    {
        let nearest_is_renamed = old.nearest_heading.as_deref() == Some("Section Two");
        if old.embedding_identity != new.embedding_identity {
            ancestor_changed += 1;
        }
        assert_eq!(
            old.embedding_identity != new.embedding_identity,
            nearest_is_renamed,
            "chunk {index}: only nearest-heading chunks re-embed on an ancestor rename"
        );
    }
    println!(
        "M5c: renaming one H2 changed {ancestor_changed} of {} identities",
        base_doc.chunks.len()
    );
    assert_eq!(ancestor_changed, 2, "only the H2's own two body chunks");

    // Its descendants: same identity, changed breadcrumb.
    let deep_before: Vec<&md::Chunk> = base_doc
        .chunks
        .iter()
        .filter(|chunk| chunk.nearest_heading.as_deref() == Some("Two Deeper"))
        .collect();
    let deep_after: Vec<&md::Chunk> = ancestor_doc
        .chunks
        .iter()
        .filter(|chunk| chunk.nearest_heading.as_deref() == Some("Two Deeper"))
        .collect();
    assert_eq!(deep_before.len(), 2);
    for (before, after) in deep_before.iter().zip(deep_after.iter()) {
        assert_eq!(before.embedding_identity, after.embedding_identity);
        assert_ne!(
            before.breadcrumb, after.breadcrumb,
            "the breadcrumb metadata changed"
        );
        assert!(after
            .breadcrumb
            .contains(&"Section Two Renamed".to_string()));
    }
}

#[test]
fn m5d_identity_is_exactly_the_three_named_inputs_and_nothing_else() {
    // Plan: "Embedding identity is exactly hash(format_version,
    // nearest_heading, rendered_body). Nothing else enters that hash."
    let bounds = ChunkBounds::default();
    let source = blast_radius_document(H1_OLD, Some("Doc Title"));
    let doc = md::index_document("docs/blast.md", &source, &bounds);

    for chunk in body_chunks(&doc) {
        // Recomputed from ONLY the three named inputs.
        assert_eq!(
            chunk.embedding_identity,
            md::embedding_identity(chunk.nearest_heading.as_deref(), &chunk.rendered_body),
            "identity must be reproducible from the nearest heading and the rendered body alone"
        );
        let input = md::embedding_input(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
        assert!(
            input.starts_with(md::FORMAT_VERSION),
            "the format version participates"
        );
        assert_eq!(chunk.embedding_identity, md::hash_hex(input.as_bytes()));
        // Nothing else: the path, the title, the byte span and the ordinal are
        // absent from the hashed input.
        assert!(
            !input.contains("docs/blast.md"),
            "path must not enter the hash"
        );
        assert!(
            !input.contains("Doc Title"),
            "title must not enter the hash"
        );
    }

    // Everything the plan excludes, varied at once: path, document title,
    // ancestor breadcrumb, byte/line spans, and same-heading ordinal all
    // differ, while (nearest_heading, rendered_body) is held fixed. The
    // identity must be bit-identical.
    let body = filler("shared-section-body", 300);
    let left =
        format!("---\ntitle: Left Title\n---\n\n# Left Root\n\n## Shared Heading\n\n{body}\n");
    let right = format!(
        "# Right Root\n\n{}\n\n### Distractor\n\n{}\n\n## Shared Heading\n\n{body}\n",
        filler("filler-a", 200),
        filler("filler-b", 200)
    );
    let left_doc = md::index_document("one/left.md", &left, &bounds);
    let right_doc = md::index_document("two/deeper/right.md", &right, &bounds);
    let left_chunk = left_doc
        .chunks
        .iter()
        .find(|chunk| chunk.nearest_heading.as_deref() == Some("Shared Heading"))
        .expect("left shared chunk");
    let right_chunk = right_doc
        .chunks
        .iter()
        .find(|chunk| chunk.nearest_heading.as_deref() == Some("Shared Heading"))
        .expect("right shared chunk");

    assert_ne!(left_doc.path, right_doc.path, "paths differ");
    assert_ne!(left_doc.title, right_doc.title, "titles differ");
    assert_ne!(
        left_chunk.breadcrumb, right_chunk.breadcrumb,
        "ancestor breadcrumbs differ"
    );
    assert_ne!(
        left_chunk.byte_start, right_chunk.byte_start,
        "byte spans differ"
    );
    assert_ne!(
        left_chunk.line_start, right_chunk.line_start,
        "line spans differ"
    );
    assert_eq!(
        left_chunk.rendered_body, right_chunk.rendered_body,
        "bodies are held fixed"
    );
    assert_eq!(
        left_chunk.embedding_identity, right_chunk.embedding_identity,
        "identity is exactly (format_version, nearest_heading, rendered_body) and nothing else"
    );
    println!(
        "M5d: two documents differing in path/title/breadcrumb/spans share identity {}",
        &left_chunk.embedding_identity[..16]
    );

    // The format version really is load-bearing: two chunks that differ only in
    // heading, or only in body, get different identities; identical pairs
    // collide by design (content-addressed vectors).
    let a = md::embedding_identity(Some("H"), "body");
    let b = md::embedding_identity(Some("H2"), "body");
    let c = md::embedding_identity(Some("H"), "body2");
    let d = md::embedding_identity(Some("H"), "body");
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(a, d, "identical (heading, body) pairs share one vector");
    assert_ne!(
        md::embedding_identity(None, "body"),
        a,
        "absent heading is distinct from a heading"
    );
}

// ---------------------------------------------------------------------------
// M6 - deterministic block-native splitting of oversized blocks
// ---------------------------------------------------------------------------

/// Bounds tight enough to force splitting while keeping fixtures small.
fn split_bounds() -> ChunkBounds {
    ChunkBounds {
        target: 400,
        normal_max: 800,
        hard_max: 900,
        heading_ctx_max: 1_024,
        synthetic_max: 1_024,
    }
}

/// Fragments of one split block, in order.
fn fragments_of(doc: &Document, block_index: usize) -> Vec<&md::Chunk> {
    doc.chunks
        .iter()
        .filter(|chunk| !chunk.is_stub && chunk.blocks == vec![block_index])
        .collect()
}

fn assert_fragments_tile_block(source: &str, doc: &Document, block_index: usize, label: &str) {
    let block = &doc.blocks[block_index];
    let fragments = fragments_of(doc, block_index);
    assert!(
        fragments.len() > 1,
        "{label}: block must actually split (got {})",
        fragments.len()
    );
    assert_eq!(
        fragments[0].byte_start, block.byte_start,
        "{label}: first fragment starts at the block"
    );
    assert_eq!(
        fragments.last().unwrap().byte_end,
        block.byte_end,
        "{label}: last fragment ends at the block"
    );
    let mut rebuilt = String::new();
    let mut cursor = block.byte_start;
    for fragment in &fragments {
        assert_eq!(
            fragment.byte_start, cursor,
            "{label}: fragments must be contiguous with no gap or overlap"
        );
        rebuilt.push_str(&source[fragment.byte_start..fragment.byte_end]);
        cursor = fragment.byte_end;
    }
    assert_eq!(
        rebuilt, block.raw,
        "{label}: fragments must tile the block's exact source bytes"
    );
    // Ordinals establish order only, and they are dense and ascending.
    let ordinals: Vec<usize> = fragments
        .iter()
        .map(|fragment| fragment.same_heading_ordinal)
        .collect();
    let mut expected = ordinals.clone();
    expected.sort_unstable();
    assert_eq!(ordinals, expected, "{label}: fragment ordinals ascend");
}

#[test]
fn m6a_oversized_fenced_code_splits_at_newlines() {
    // Plan: "Oversized atomic blocks first split at the last block-native
    // boundary before that remaining bound: newline for code".
    let bounds = split_bounds();
    let mut code = String::from("# Code\n\n```rust\n");
    for index in 0..60 {
        code.push_str(&format!(
            "fn generated_function_number_{index:03}() -> usize {{ {index} }}\n"
        ));
    }
    code.push_str("```\n");

    let doc = md::index_document("docs/code.md", &code, &bounds);
    assert_spans_round_trip(&code, &doc, "m6a");
    let block_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::CodeFenced)
        .expect("fenced code block");
    assert_fragments_tile_block(&code, &doc, block_index, "m6a");

    let fragments = fragments_of(&doc, block_index);
    println!(
        "M6a: {} fenced-code fragments under a {}-byte hard bound",
        fragments.len(),
        bounds.hard_max
    );
    for fragment in fragments.iter().take(fragments.len() - 1) {
        let slice = &code[fragment.byte_start..fragment.byte_end];
        assert!(
            slice.ends_with('\n'),
            "code fragments must end on a newline boundary: {slice:?}"
        );
    }
    // Fragments after the first repeat the opening fence line as synthetic
    // context, without altering the exact source span.
    for fragment in fragments.iter().skip(1) {
        assert!(
            fragment.rendered_body.starts_with("```rust\n"),
            "fragment must repeat the fence info line"
        );
        assert!(
            fragment
                .rendered_body
                .ends_with(&code[fragment.byte_start..fragment.byte_end]),
            "synthetic context must not alter the source slice"
        );
    }
    assert_eq!(
        fragments[0].rendered_body,
        code[fragments[0].byte_start..fragments[0].byte_end],
        "the first fragment already carries the fence natively and gets no prefix"
    );

    // "the LAST block-native boundary before that remaining bound": each
    // fragment is maximal - one more source line would breach its own budget.
    let heading_prefix = md::embedding_input(fragments[0].nearest_heading.as_deref(), "").len();
    for (index, fragment) in fragments.iter().enumerate() {
        let span_len = fragment.byte_end - fragment.byte_start;
        let synthetic_len = fragment.rendered_body.len() - span_len;
        let budget = bounds.hard_max - heading_prefix - synthetic_len;
        assert!(
            span_len <= budget,
            "fragment {index} exceeds its body budget: {span_len} > {budget}"
        );
        if index + 1 < fragments.len() {
            let rest = &code[fragment.byte_end..];
            let next_line = rest
                .find('\n')
                .map(|offset| offset + 1)
                .unwrap_or(rest.len());
            assert!(
                span_len + next_line > budget,
                "fragment {index} is not maximal: {span_len} + {next_line} still fits in {budget}"
            );
        }
    }

    // Determinism: re-run and compare byte for byte.
    let again = md::index_document("docs/code.md", &String::from(code.as_str()), &bounds);
    assert_eq!(
        doc.chunks, again.chunks,
        "code splitting must be byte-for-byte deterministic"
    );
}

#[test]
fn m6b_oversized_tables_split_at_row_boundaries() {
    // Plan: "row for tables"; fragments repeat the bounded synthetic
    // table-header context.
    let bounds = split_bounds();
    let mut table = String::from("# Table\n\n| name | value | notes |\n| --- | --- | --- |\n");
    for index in 0..80 {
        table.push_str(&format!(
            "| row-{index:03} | {index} | note text for row {index:03} |\n"
        ));
    }

    let doc = md::index_document("docs/table.md", &table, &bounds);
    assert_spans_round_trip(&table, &doc, "m6b");
    let block_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::Table)
        .expect("table block");
    assert_fragments_tile_block(&table, &doc, block_index, "m6b");

    let header = "| name | value | notes |\n| --- | --- | --- |\n";
    let fragments = fragments_of(&doc, block_index);
    println!("M6b: {} table fragments", fragments.len());
    for fragment in fragments.iter().take(fragments.len() - 1) {
        assert!(
            table[fragment.byte_start..fragment.byte_end].ends_with('\n'),
            "table fragments must end on a row boundary"
        );
    }
    for fragment in fragments.iter().skip(1) {
        assert!(
            fragment.rendered_body.starts_with(header),
            "fragment must repeat header + delimiter rows"
        );
        assert!(
            table[fragment.byte_start..fragment.byte_end].starts_with('|'),
            "a fragment must start at the beginning of a row"
        );
        assert!(fragment
            .rendered_body
            .ends_with(&table[fragment.byte_start..fragment.byte_end]));
    }

    let again = md::index_document("docs/table.md", &String::from(table.as_str()), &bounds);
    assert_eq!(
        doc.chunks, again.chunks,
        "table splitting must be byte-for-byte deterministic"
    );
}

#[test]
fn m6c_oversized_lists_split_at_top_level_items() {
    // Plan: "top-level item for lists".
    let bounds = split_bounds();
    let mut list = String::from("# List\n\n");
    for index in 0..40 {
        list.push_str(&format!("- {}\n", filler(&format!("item-{index:02}"), 110)));
    }

    let doc = md::index_document("docs/list.md", &list, &bounds);
    assert_spans_round_trip(&list, &doc, "m6c");
    let block_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::List)
        .expect("list block");
    assert_fragments_tile_block(&list, &doc, block_index, "m6c");

    let fragments = fragments_of(&doc, block_index);
    println!("M6c: {} list fragments", fragments.len());
    for fragment in fragments.iter().skip(1) {
        let slice = &list[fragment.byte_start..fragment.byte_end];
        assert!(
            slice.starts_with("- item-"),
            "list fragments must start at a top-level item: {:?}",
            &slice[..12]
        );
        // Lists get no synthetic context.
        assert_eq!(
            fragment.rendered_body, slice,
            "list fragments carry no synthetic prefix"
        );
    }

    let again = md::index_document("docs/list.md", &String::from(list.as_str()), &bounds);
    assert_eq!(
        doc.chunks, again.chunks,
        "list splitting must be byte-for-byte deterministic"
    );
}

#[test]
fn m6d_a_single_oversized_line_falls_back_to_the_utf8_boundary() {
    // Plan: "If no non-empty native fragment fits, every block type falls back
    // to the last newline before the bound and then to the last UTF-8
    // boundary."
    let bounds = split_bounds();
    // One enormous line of multi-byte characters inside a fence: no interior
    // newline exists, so the newline fallback cannot fire either.
    let long_line: String = "αβγδε漢字🌍".repeat(200);
    let source = format!("# Long\n\n```txt\n{long_line}\n```\n");

    let doc = md::index_document("docs/long.md", &source, &bounds);
    assert_spans_round_trip(&source, &doc, "m6d");
    let block_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::CodeFenced)
        .expect("fenced code block");
    assert_fragments_tile_block(&source, &doc, block_index, "m6d");

    let fragments = fragments_of(&doc, block_index);
    println!(
        "M6d: {} fragments of one {}-byte line",
        fragments.len(),
        long_line.len()
    );
    let mut newline_free = 0usize;
    for fragment in &fragments {
        let slice = &source[fragment.byte_start..fragment.byte_end];
        assert!(
            source.is_char_boundary(fragment.byte_start),
            "fragment split inside a multi-byte character"
        );
        assert!(
            source.is_char_boundary(fragment.byte_end),
            "fragment split inside a multi-byte character"
        );
        if !slice.contains('\n') {
            newline_free += 1;
        }
        let input =
            md::embedding_input(fragment.nearest_heading.as_deref(), &fragment.rendered_body);
        assert!(
            input.len() <= bounds.hard_max,
            "fragment exceeded the hard bound: {} bytes",
            input.len()
        );
    }
    assert!(
        newline_free >= 2,
        "the UTF-8 fallback must actually be exercised (got {newline_free})"
    );

    // Fallback ordering is observable: the very first fragment is the fence
    // line alone, because the last newline before the bound is the one right
    // after "```txt".
    assert_eq!(
        &source[fragments[0].byte_start..fragments[0].byte_end],
        "```txt\n"
    );

    let again = md::index_document("docs/long.md", &String::from(source.as_str()), &bounds);
    assert_eq!(
        doc.chunks, again.chunks,
        "UTF-8 fallback splitting must be byte-for-byte deterministic"
    );
}

#[test]
fn m6e_whitespace_only_fragments_are_dropped_and_leave_a_coverage_hole() {
    // VIOLATION of the implied coverage property. The plan says oversized
    // blocks "split at the last block-native boundary" and that "Fragments
    // repeat the bounded synthetic context in their rendered body without
    // altering exact source spans" - i.e. the fragments ARE the block. In
    // practice a fragment whose rendered text is only whitespace is silently
    // discarded, so the block's source bytes are NOT fully covered by chunks
    // and a byte range of the file belongs to no retrievable row.
    //
    // At the plan's defaults the body budget is ~23,000 bytes, so this needs a
    // ~23 kB run of blank lines to bite; with a tight bound it is immediate.
    let bounds = split_bounds();
    let mut source = String::from("# Blanks\n\n```txt\n");
    source.push_str(&filler("alpha", 700).replace(' ', "\n"));
    source.push('\n');
    // A blank run several times longer than the ~871-byte body budget, so at
    // least one whole fragment consists of nothing but newlines.
    source.push_str(&"\n".repeat(2_600));
    source.push_str("omega\n```\n");

    let doc = md::index_document("docs/blanks.md", &source, &bounds);
    let block_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::CodeFenced)
        .expect("fenced code block");
    let block = &doc.blocks[block_index];
    let fragments = fragments_of(&doc, block_index);
    assert!(fragments.len() > 1, "the block must split");

    let covered: usize = fragments
        .iter()
        .map(|fragment| fragment.byte_end - fragment.byte_start)
        .sum();
    let block_len = block.byte_end - block.byte_start;
    println!(
        "M6e: fragments cover {covered} of {block_len} block bytes ({} fragments); hole = {} bytes",
        fragments.len(),
        block_len - covered
    );
    assert!(
        covered < block_len,
        "OBSERVED: whitespace-only fragments are dropped, leaving uncovered source bytes"
    );

    // The surviving fragments are still contiguous-with-gaps and still exact.
    let mut cursor = block.byte_start;
    for fragment in &fragments {
        assert!(fragment.byte_start >= cursor, "fragments never overlap");
        assert!(source.is_char_boundary(fragment.byte_start));
        assert!(source.is_char_boundary(fragment.byte_end));
        assert!(
            fragment
                .rendered_body
                .ends_with(&source[fragment.byte_start..fragment.byte_end]),
            "each surviving fragment still ends with its exact source slice"
        );
        cursor = fragment.byte_end;
    }
    // Both real payloads survive; only blank bytes were lost.
    let text: String = fragments
        .iter()
        .map(|fragment| fragment.rendered_body.as_str())
        .collect();
    assert!(text.contains("alpha"), "leading payload survives");
    assert!(text.contains("omega"), "trailing payload survives");

    let again = md::index_document("docs/blanks.md", &String::from(source.as_str()), &bounds);
    assert_eq!(
        doc.chunks, again.chunks,
        "the dropping behavior is at least deterministic"
    );
}

// ---------------------------------------------------------------------------
// M7 - the FINAL embedding input never exceeds hard_max
// ---------------------------------------------------------------------------

#[test]
fn m7_final_embedding_input_respects_the_hard_bound() {
    // Plan: "The hard bound applies after nearest-heading serialization and
    // synthetic context are added." Both truncation markers must appear.
    let bounds = ChunkBounds {
        target: 400,
        normal_max: 800,
        hard_max: 3_000,
        heading_ctx_max: 1_024, // the plan's default; embedding_input uses it too
        synthetic_max: 64,
    };

    let huge_heading = filler("Enormous Heading", 5_000).replace('\n', " ");
    let huge_info = filler("infostring", 400).replace(' ', "-");
    let mut source = format!("# {huge_heading}\n\n```{huge_info}\n");
    for index in 0..120 {
        source.push_str(&format!(
            "payload line {index:04} with some trailing content to make it long enough\n"
        ));
    }
    source.push_str("```\n\n");
    for index in 0..4 {
        source.push_str(&filler(&format!("tail-p{index}"), 900));
        source.push_str("\n\n");
    }

    let doc = md::index_document("docs/huge.md", &source, &bounds);
    assert_spans_round_trip(&source, &doc, "m7");

    // The nearest heading is truncated to exactly the context bound, marker
    // included.
    let heading = doc.chunks[0]
        .nearest_heading
        .clone()
        .expect("nearest heading");
    assert!(
        heading.ends_with(md::HEADING_TRUNCATED),
        "heading truncation marker must be present"
    );
    assert_eq!(
        heading.len(),
        bounds.heading_ctx_max,
        "the bounded heading is exactly heading_ctx_max bytes"
    );
    assert!(
        huge_heading.len() > bounds.heading_ctx_max,
        "the fixture heading must really be oversized"
    );

    // Synthetic fence context is truncated within the synthetic bound.
    let code_index = doc
        .blocks
        .iter()
        .position(|block| block.kind == BlockKind::CodeFenced)
        .expect("fenced code block");
    let fragments = fragments_of(&doc, code_index);
    assert!(fragments.len() > 1, "the code block must split");
    let prefix_len =
        fragments[1].rendered_body.len() - (fragments[1].byte_end - fragments[1].byte_start);
    assert_eq!(
        prefix_len, bounds.synthetic_max,
        "synthetic context is bounded to synthetic_max"
    );
    assert!(
        fragments[1].rendered_body[..prefix_len].ends_with(md::CONTEXT_TRUNCATED),
        "context truncation marker must be present: {:?}",
        &fragments[1].rendered_body[..prefix_len]
    );

    let mut max_input = 0usize;
    for chunk in body_chunks(&doc) {
        let input = md::embedding_input(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
        max_input = max_input.max(input.len());
        assert!(
            input.len() <= bounds.hard_max,
            "final embedding input {} bytes exceeds hard_max {}",
            input.len(),
            bounds.hard_max
        );
        // The bound is on the FINAL input, and the heading serialization is
        // really inside it.
        assert!(
            input.contains(md::HEADING_TRUNCATED),
            "the bounded heading is part of the measured input"
        );
    }
    println!(
        "M7: largest final embedding input = {max_input} of {} allowed",
        bounds.hard_max
    );
    assert!(
        max_input > bounds.hard_max / 2,
        "the fixture must approach the bound, not sit far below it"
    );

    // Same claim at the plan's own defaults.
    let defaults = ChunkBounds::default();
    let doc = md::index_document("docs/huge.md", &source, &defaults);
    for chunk in body_chunks(&doc) {
        let input = md::embedding_input(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
        assert!(
            input.len() <= defaults.hard_max,
            "default-bounds input exceeded hard_max: {}",
            input.len()
        );
    }
}

#[test]
fn m7b_hard_max_below_the_heading_serialization_breaks_the_bound() {
    // OBSERVED boundary condition the plan does not cover. The plan fixes
    // hard_max = 24,000 and heading context max = 1,024, so the heading
    // serialization always fits. When a configuration inverts that (hard_max
    // smaller than the serialized heading), the body budget saturates to zero
    // and the splitter still must make progress, so it emits one-character
    // fragments whose FINAL input necessarily exceeds hard_max.
    //
    // This is not a contradiction of the plan's numbers; it is a missing
    // constraint: the plan never states hard_max > heading_ctx_max + framing.
    let bounds = ChunkBounds {
        target: 100,
        normal_max: 200,
        hard_max: 1_000, // below the ~1,047-byte serialized heading prefix
        heading_ctx_max: 1_024,
        synthetic_max: 1_024,
    };
    let huge_heading = filler("Enormous Heading", 5_000).replace('\n', " ");
    let source = format!("# {huge_heading}\n\nshort body\n");
    let doc = md::index_document("docs/inverted.md", &source, &bounds);

    let bodies = body_chunks(&doc);
    println!(
        "M7b: hard_max={} produced {} fragments from a 10-byte body",
        bounds.hard_max,
        bodies.len()
    );
    // OBSERVED: 9, not 10. One character per fragment, except the single space
    // in "short body", whose fragment renders whitespace-only and is dropped
    // (see m6e: dropped fragments leave a hole in the source coverage).
    assert_eq!(
        bodies.len(),
        "short body".len() - 1,
        "one fragment per non-blank character"
    );
    for chunk in &bodies {
        let input = md::embedding_input(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
        assert!(
            input.len() > bounds.hard_max,
            "OBSERVED: with an inverted configuration the final input ({}) exceeds hard_max ({})",
            input.len(),
            bounds.hard_max
        );
    }
}

// ---------------------------------------------------------------------------
// M8 - body-empty documents yield exactly one lexical-only stub
// ---------------------------------------------------------------------------

#[test]
fn m8_body_empty_documents_yield_exactly_one_stub() {
    // Plan: "A document producing no body chunks emits exactly one lexical-only
    // document-stub row ... empty rendered body; no nearest heading; span
    // covering the file. Stubs are not embedded. There are no empty per-section
    // chunks."
    let cases: [(&str, &str, Vec<&str>); 4] = [
        (
            "front-matter-only",
            "---\ntitle: Only Meta\ndescription: d\ntags: [a]\n---\n",
            vec![],
        ),
        (
            "heading-only",
            "# Alpha\n\n## Beta\n\n### Gamma\n",
            vec!["Alpha", "Beta", "Gamma"],
        ),
        (
            "headings-and-breaks",
            "# One\n\n---\n\n## Two\n\n***\n",
            vec!["One", "Two"],
        ),
        (
            "comment-only-body",
            "---\ntitle: T\n---\n\n# Head\n\n<!-- an entirely invisible note -->\n",
            vec!["Head"],
        ),
    ];

    for (label, source, expected_headings) in cases {
        let doc = md::index_document("docs/empty.md", source, &ChunkBounds::default());
        assert_eq!(doc.chunks.len(), 1, "{label}: exactly one row");
        let stub = &doc.chunks[0];
        assert!(stub.is_stub, "{label}: the row is a stub");
        assert_eq!(stub.rendered_body, "", "{label}: empty rendered body");
        assert_eq!(stub.nearest_heading, None, "{label}: no nearest heading");
        assert!(
            stub.blocks.is_empty(),
            "{label}: a stub references no block"
        );
        assert_eq!(
            stub.embedding_identity, "",
            "{label}: stubs are not embedded"
        );
        assert_eq!(
            (stub.byte_start, stub.byte_end),
            (0, source.len()),
            "{label}: span covers the file"
        );
        assert_eq!(stub.line_start, 1, "{label}: stub starts at line 1");
        assert_eq!(stub.same_heading_ordinal, 0);

        // "all document headings are searchable, the headings carried in source
        // order by the stub's breadcrumb column".
        let expected: Vec<String> = expected_headings
            .iter()
            .map(|text| text.to_string())
            .collect();
        assert_eq!(
            stub.stub_headings, expected,
            "{label}: headings in source order"
        );
        assert_eq!(
            stub.breadcrumb, expected,
            "{label}: breadcrumb column carries them"
        );
        println!("M8: {label} -> stub with headings {:?}", stub.stub_headings);
    }

    // The stub path is only for body-empty documents: one visible character of
    // body is enough to suppress it.
    let with_body = md::index_document("docs/x.md", "# Alpha\n\n.\n", &ChunkBounds::default());
    assert_eq!(with_body.chunks.len(), 1);
    assert!(
        !with_body.chunks[0].is_stub,
        "a document with any body block emits no stub"
    );
    assert!(!with_body.chunks[0].embedding_identity.is_empty());

    // And there are no empty per-section chunks: three heading-only sections
    // produce zero section rows, not three.
    let sections = md::index_document(
        "docs/s.md",
        "# A\n\n## B\n\n## C\n",
        &ChunkBounds::default(),
    );
    assert_eq!(sections.chunks.len(), 1);
    assert!(sections.chunks[0].is_stub);
}

// ---------------------------------------------------------------------------
// M9 - determinism
// ---------------------------------------------------------------------------

#[test]
fn m9_indexing_is_deterministic_across_repeated_runs() {
    // Plan (validation): the corpus pipeline is "local, deterministic".
    let bounds = ChunkBounds::default();
    let tight = ChunkBounds {
        target: 200,
        normal_max: 400,
        hard_max: 500,
        ..ChunkBounds::default()
    };
    let mut fixtures: Vec<String> = vec![
        blast_radius_document(H1_OLD, Some("Doc Title")),
        hostile_encoding_fixture(),
        heading_boundary_fixture(),
    ];
    let mut code = String::from("# Code\n\n```rust\n");
    for index in 0..40 {
        code.push_str(&format!("let value_{index:03} = {index};\n"));
    }
    code.push_str("```\n");
    fixtures.push(code);

    for (index, source) in fixtures.iter().enumerate() {
        for current_bounds in [bounds, tight] {
            let first = md::index_document("docs/determinism.md", source, &current_bounds);
            for run in 0..4 {
                // A fresh allocation each time, so nothing can be shared.
                let copy: String = source.chars().collect();
                let again = md::index_document("docs/determinism.md", &copy, &current_bounds);
                assert_eq!(
                    first.blocks, again.blocks,
                    "fixture {index} run {run}: blocks differ"
                );
                assert_eq!(
                    first.chunks, again.chunks,
                    "fixture {index} run {run}: chunks differ"
                );
                assert_eq!(
                    first.title, again.title,
                    "fixture {index} run {run}: title differs"
                );
                assert_eq!(
                    first.front_matter, again.front_matter,
                    "fixture {index} run {run}: front matter differs"
                );
            }
        }
    }

    // Hash stability is explicit, not merely structural equality.
    let source = blast_radius_document(H1_OLD, Some("Doc Title"));
    let doc = md::index_document("docs/determinism.md", &source, &bounds);
    let block_hashes: Vec<&str> = doc
        .blocks
        .iter()
        .map(|block| block.content_hash.as_str())
        .collect();
    let chunk_ids: Vec<&str> = doc
        .chunks
        .iter()
        .map(|chunk| chunk.embedding_identity.as_str())
        .collect();
    let repeat = md::index_document("docs/determinism.md", &source, &bounds);
    assert_eq!(
        block_hashes,
        repeat
            .blocks
            .iter()
            .map(|b| b.content_hash.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        chunk_ids,
        repeat
            .chunks
            .iter()
            .map(|c| c.embedding_identity.as_str())
            .collect::<Vec<_>>()
    );
    println!(
        "M9: {} blocks / {} chunks stable across 5 runs at two bound settings",
        doc.blocks.len(),
        doc.chunks.len()
    );
}

// ---------------------------------------------------------------------------
// M10 - HTML comments leave retrieval text but never raw or spans
// ---------------------------------------------------------------------------

#[test]
fn m10_html_comments_are_excluded_from_rendered_text_only() {
    // Plan: "HTML comments are excluded from retrieval text."
    let source = concat!(
        "# Comments\n\n",
        "Visible start <!-- hidden inline note --> visible end.\n\n",
        "<!-- an entire block that is only a comment -->\n\n",
        "<div>real html stays</div>\n\n",
        "```txt\n<!-- literal comment inside code -->\n```\n\n",
        "Dangling <!-- never closed comment\n"
    );
    let doc = md::index_document("docs/comments.md", source, &ChunkBounds::default());
    assert_spans_round_trip(source, &doc, "m10");

    // 1. Inline comment: removed from rendered, present in raw, span unchanged.
    let paragraph = doc
        .blocks
        .iter()
        .find(|block| block.raw.starts_with("Visible start"))
        .expect("inline-comment paragraph");
    assert!(
        paragraph.raw.contains("<!-- hidden inline note -->"),
        "raw keeps the comment verbatim"
    );
    assert!(
        !paragraph.rendered.contains("<!--"),
        "rendered drops the comment"
    );
    assert!(!paragraph.rendered.contains("hidden inline note"));
    assert!(
        paragraph.rendered.contains("Visible start") && paragraph.rendered.contains("visible end.")
    );
    assert_eq!(
        &source[paragraph.byte_start..paragraph.byte_end],
        paragraph.raw,
        "span untouched by comment removal"
    );
    assert_eq!(
        paragraph.content_hash,
        md::hash_hex(paragraph.raw.as_bytes()),
        "identity hashes RAW, not rendered"
    );

    // 2. A block that is nothing but a comment renders empty and yields no chunk.
    let comment_block = doc
        .blocks
        .iter()
        .position(|block| block.raw.starts_with("<!-- an entire block"))
        .expect("comment-only block");
    assert_eq!(
        doc.blocks[comment_block].rendered, "",
        "a comment-only block renders empty"
    );
    assert!(
        !doc.blocks[comment_block].raw.is_empty(),
        "but its raw and span survive"
    );
    for chunk in body_chunks(&doc) {
        assert!(
            !chunk.blocks.contains(&comment_block),
            "a comment-only block must not reach a chunk"
        );
        let carries_code = chunk.blocks.iter().any(|index| {
            matches!(
                doc.blocks[*index].kind,
                BlockKind::CodeFenced | BlockKind::CodeIndented
            )
        });
        if !carries_code {
            assert!(
                !chunk.rendered_body.contains("<!--"),
                "no prose chunk body carries a comment"
            );
        }
    }

    // 3. Visible HTML is retained.
    assert!(
        body_chunks(&doc)
            .iter()
            .any(|chunk| chunk.rendered_body.contains("<div>real html stays</div>")),
        "visible HTML blocks stay in retrieval text"
    );

    // 4. OBSERVED, plan silent: code content is exempt from comment stripping.
    // The plan says "HTML comments are excluded from retrieval text" without
    // qualifying code. In CommonMark the sequence inside a fence is literal
    // text, not a comment, and the core keeps it. Documenting the actual
    // behavior: code fragments retain `<!-- ... -->`.
    let code_block = doc
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::CodeFenced)
        .expect("fenced code block");
    assert!(
        code_block
            .rendered
            .contains("<!-- literal comment inside code -->"),
        "OBSERVED: code blocks are not comment-stripped"
    );
    assert!(
        doc.chunks.iter().any(|chunk| chunk
            .rendered_body
            .contains("<!-- literal comment inside code -->")),
        "OBSERVED: the literal sequence therefore also reaches the chunk body"
    );

    // 5. OBSERVED, plan silent: an UNTERMINATED `<!--` is left in place, so a
    // stray opener cannot swallow the rest of the document.
    let dangling = doc
        .blocks
        .iter()
        .find(|block| block.raw.starts_with("Dangling"))
        .expect("dangling-comment paragraph");
    assert!(
        dangling.rendered.contains("<!-- never closed comment"),
        "OBSERVED: an unterminated opener is kept"
    );

    // 6. Comment removal never shifts a neighbour's span.
    let no_comments = source.replace("<!-- hidden inline note --> ", "");
    let stripped_doc =
        md::index_document("docs/comments.md", &no_comments, &ChunkBounds::default());
    assert_spans_round_trip(&no_comments, &stripped_doc, "m10-stripped");
    println!(
        "M10: {} blocks with comments, {} without; comment-only block index {}",
        doc.blocks.len(),
        stripped_doc.blocks.len(),
        comment_block
    );
}

// ---------------------------------------------------------------------------
// M11 - block identity is stable when chunk grouping changes
// ---------------------------------------------------------------------------

#[test]
fn m11_inserting_a_paragraph_does_not_disturb_untouched_block_hashes() {
    // Plan: "Retrieval chunks may regroup when blocks are inserted or removed;
    // that may rebuild vectors, but it does not fabricate history transitions
    // for unchanged blocks." This is the property the block-level ledger rests
    // on: block content identity must survive regrouping.
    let bounds = ChunkBounds::default();
    let paragraphs: Vec<String> = (0..4)
        .map(|index| filler(&format!("para-{index}"), 1_500))
        .collect();
    let mut before = String::from("# Ledger\n\n");
    for paragraph in &paragraphs {
        before.push_str(paragraph);
        before.push_str("\n\n");
    }

    let inserted = filler("INSERTED-UNIQUE-PARAGRAPH", 1_500);
    let mut after = String::from("# Ledger\n\n");
    for (index, paragraph) in paragraphs.iter().enumerate() {
        if index == 2 {
            after.push_str(&inserted);
            after.push_str("\n\n");
        }
        after.push_str(paragraph);
        after.push_str("\n\n");
    }

    let before_doc = md::index_document("docs/ledger.md", &before, &bounds);
    let after_doc = md::index_document("docs/ledger.md", &after, &bounds);
    assert_spans_round_trip(&before, &before_doc, "m11-before");
    assert_spans_round_trip(&after, &after_doc, "m11-after");

    // Chunk grouping really does change: the insert re-pairs the paragraphs.
    let before_groups: Vec<Vec<usize>> = before_doc
        .chunks
        .iter()
        .map(|chunk| chunk.blocks.clone())
        .collect();
    let after_groups: Vec<Vec<usize>> = after_doc
        .chunks
        .iter()
        .map(|chunk| chunk.blocks.clone())
        .collect();
    println!("M11: chunk grouping {before_groups:?} -> {after_groups:?}");
    assert_ne!(
        before_doc.chunks.len(),
        after_doc.chunks.len(),
        "the insert must regroup chunks"
    );
    let changed_identities = before_doc
        .chunks
        .iter()
        .filter(|chunk| {
            !after_doc
                .chunks
                .iter()
                .any(|other| other.embedding_identity == chunk.embedding_identity)
        })
        .count();
    assert!(
        changed_identities > 0,
        "regrouping does rebuild some vectors (the plan allows this)"
    );

    // ...but every untouched BLOCK keeps its content hash, with multiplicity.
    let before_hashes = hash_multiset(&before_doc);
    let after_hashes = hash_multiset(&after_doc);
    assert_eq!(
        before_doc.blocks.len() + 1,
        after_doc.blocks.len(),
        "exactly one new block"
    );
    let new_hashes: Vec<&String> = after_hashes
        .keys()
        .filter(|hash| !before_hashes.contains_key(*hash))
        .collect();
    let lost_hashes: Vec<&String> = before_hashes
        .keys()
        .filter(|hash| !after_hashes.contains_key(*hash))
        .collect();
    assert!(
        lost_hashes.is_empty(),
        "no untouched block may lose its identity: {lost_hashes:?}"
    );
    assert_eq!(new_hashes.len(), 1, "exactly one new block hash appears");
    let inserted_block = after_doc
        .blocks
        .iter()
        .find(|block| &block.content_hash == new_hashes[0])
        .expect("the inserted block");
    assert!(
        inserted_block.raw.starts_with("INSERTED-UNIQUE-PARAGRAPH"),
        "the one new hash is the inserted paragraph"
    );
    println!(
        "M11: {} of {} block hashes preserved across the insert",
        before_doc.blocks.len(),
        after_doc.blocks.len()
    );

    // Block hashes are of RAW, so byte spans move without changing identity.
    for paragraph in &paragraphs {
        let before_block = before_doc
            .blocks
            .iter()
            .find(|block| &block.raw == paragraph)
            .expect("paragraph before");
        let after_block = after_doc
            .blocks
            .iter()
            .find(|block| &block.raw == paragraph)
            .expect("paragraph after");
        assert_eq!(
            before_block.content_hash, after_block.content_hash,
            "identity survives the move"
        );
        if paragraph == &paragraphs[3] {
            assert_ne!(
                before_block.byte_start, after_block.byte_start,
                "its span really did shift"
            );
            assert_ne!(
                before_block.line_start, after_block.line_start,
                "and so did its line number"
            );
        }
    }

    // A heading rename likewise leaves body block hashes untouched (only chunk
    // context changes), which is what makes `context_changed` a metadata-only
    // event in the ledger.
    let renamed = before.replace("# Ledger\n", "# Ledger Renamed\n");
    let renamed_doc = md::index_document("docs/ledger.md", &renamed, &bounds);
    for paragraph in &paragraphs {
        let original = before_doc
            .blocks
            .iter()
            .find(|block| &block.raw == paragraph)
            .unwrap();
        let moved = renamed_doc
            .blocks
            .iter()
            .find(|block| &block.raw == paragraph)
            .unwrap();
        assert_eq!(
            original.content_hash, moved.content_hash,
            "a heading rename changes no body block hash"
        );
        assert_ne!(
            original.nearest_heading, moved.nearest_heading,
            "but the block's context did change"
        );
    }
}
