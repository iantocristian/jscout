//! Smoke tests for the harness CORE, not for the G24 plan.
//!
//! These assertions exist to prove the instrument is trustworthy before any
//! plan claim is measured with it: spans slice back to the original bytes,
//! embedding identity is stable and carries nothing but what the plan names,
//! oversized blocks tile their source exactly, and the git laboratory really
//! produces a shallow clone, really diverges author from committer time, and
//! really cannot see ambient git configuration.
//!
//! Where real behavior contradicted the naive reading of the plan, the
//! assertion records the OBSERVED behavior and the comment names the claim.

use std::path::Path;

use g24_harness::git;
use g24_harness::md::{self, BlockKind, ChunkBounds, FrontMatterState};
use g24_harness::proc;

const RICH: &str = "---\ntitle: Doc Title\ndescription: A doc.\ntags:\n  - alpha\n  - beta\n---\n\n\
# Top\n\nIntro paragraph.\n\n## Sub\n\n- one\n- two\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n\
```rust\nfn main() {}\n```\n\n> quoted\n\n<!-- hidden -->\n\n<div>visible</div>\n\n---\n\nAfter break.\n";

/// Independently recomputed 1-based line number of a byte offset. Counts over
/// bytes so it stays valid at offsets inside a multi-byte character.
fn line_of(source: &str, byte: usize) -> usize {
    source.as_bytes()[..byte]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

// ---------------------------------------------------------------------------
// 1. Spans
// ---------------------------------------------------------------------------

#[test]
fn block_spans_slice_back_to_the_original_bytes_exactly() {
    let blocks = md::parse_blocks(RICH);
    assert!(
        blocks.len() >= 10,
        "rich fixture should produce every block kind: {}",
        blocks.len()
    );

    for block in &blocks {
        assert_eq!(
            &RICH[block.byte_start..block.byte_end],
            block.raw,
            "raw must be the exact source slice for {:?}",
            block.kind
        );
        assert_eq!(block.content_hash, md::hash_hex(block.raw.as_bytes()));
        assert_eq!(
            block.line_start,
            line_of(RICH, block.byte_start),
            "line_start for {:?}",
            block.kind
        );
        assert_eq!(
            block.line_end,
            line_of(RICH, block.byte_end - 1),
            "line_end for {:?} raw={:?}",
            block.kind,
            block.raw
        );
        assert!(block.byte_start < block.byte_end);
    }

    // Blocks are strictly ordered and never overlap.
    for pair in blocks.windows(2) {
        assert!(
            pair[0].byte_end <= pair[1].byte_start,
            "blocks overlap: {:?} / {:?}",
            pair[0],
            pair[1]
        );
    }

    // Front matter is never emitted as a body block: the first block starts
    // after the closing delimiter.
    let (_, body_start) = md::parse_front_matter(RICH);
    assert!(body_start > 0);
    assert!(blocks[0].byte_start >= body_start);

    let kinds: Vec<BlockKind> = blocks.iter().map(|b| b.kind).collect();
    for expected in [
        BlockKind::Heading,
        BlockKind::Paragraph,
        BlockKind::List,
        BlockKind::Table,
        BlockKind::CodeFenced,
        BlockKind::BlockQuote,
        BlockKind::HtmlBlock,
        BlockKind::ThematicBreak,
    ] {
        assert!(
            kinds.contains(&expected),
            "missing {expected:?} in {kinds:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Front matter recognition
// ---------------------------------------------------------------------------

#[test]
fn front_matter_requires_delimiters_valid_yaml_and_a_top_level_mapping() {
    let cases: &[(&str, FrontMatterState, bool)] = &[
        // (source, expected state, expected body_start > 0)
        ("# No front matter\n", FrontMatterState::Absent, false),
        ("---\ntitle: t\n---\nbody\n", FrontMatterState::Parsed, true),
        // A valid YAML *scalar* is not front matter.
        (
            "---\njust a scalar\n---\nbody\n",
            FrontMatterState::MalformedAsBody,
            false,
        ),
        // A valid YAML *sequence* is not front matter.
        (
            "---\n- a\n- b\n---\nbody\n",
            FrontMatterState::MalformedAsBody,
            false,
        ),
        // Unterminated.
        (
            "---\ntitle: t\nbody\n",
            FrontMatterState::MalformedAsBody,
            false,
        ),
        // Invalid YAML.
        (
            "---\ntitle: [unclosed\n---\nbody\n",
            FrontMatterState::MalformedAsBody,
            false,
        ),
        // OBSERVED: `---\n---\n` yields YAML `null`, which is not a mapping, so
        // the plan's "produces a top-level mapping" rule classifies empty front
        // matter as malformed_as_body rather than as empty-but-valid.
        ("---\n---\nbody\n", FrontMatterState::MalformedAsBody, false),
    ];

    for (source, expected, has_body_offset) in cases {
        let (fm, body_start) = md::parse_front_matter(source);
        assert_eq!(fm.state, Some(*expected), "state for {source:?}");
        assert_eq!(
            body_start > 0,
            *has_body_offset,
            "body_start for {source:?}"
        );
    }

    // Malformed front matter stays in the body: its text is parsed as blocks.
    let malformed = "---\njust a scalar\n---\nbody\n";
    let blocks = md::parse_blocks(malformed);
    assert_eq!(
        blocks[0].byte_start, 0,
        "malformed front matter must remain body text"
    );
    assert!(
        blocks.iter().any(|b| b.raw.contains("just a scalar")),
        "malformed front-matter text must be retrievable: {blocks:#?}"
    );
}

#[test]
fn front_matter_uses_only_scalar_strings_and_string_sequences() {
    let (fm, _) = md::parse_front_matter(RICH);
    assert_eq!(fm.title.as_deref(), Some("Doc Title"));
    assert_eq!(fm.description.as_deref(), Some("A doc."));
    assert_eq!(fm.tags, vec!["alpha".to_string(), "beta".to_string()]);

    // Non-string scalars and wrong value shapes are ignored, not coerced.
    let (fm, _) = md::parse_front_matter("---\ntitle: 42\ndescription: true\ntags: solo\n---\nx\n");
    assert_eq!(fm.title, None, "a YAML number is not a scalar string title");
    assert_eq!(
        fm.description, None,
        "a YAML bool is not a scalar-string description"
    );
    assert_eq!(
        fm.tags,
        vec!["solo".to_string()],
        "a scalar string tag becomes a one-element list"
    );

    let (fm, _) = md::parse_front_matter("---\ntags:\n  - ok\n  - 7\n---\nx\n");
    assert!(
        fm.tags.is_empty(),
        "a mixed sequence is not a sequence of scalar strings"
    );

    // Title fallback order: front matter -> first H1 -> file stem.
    let bounds = ChunkBounds::default();
    assert_eq!(
        md::index_document("d/x.md", RICH, &bounds).title.as_deref(),
        Some("Doc Title")
    );
    assert_eq!(
        md::index_document("d/x.md", "# From H1\n\nbody\n", &bounds)
            .title
            .as_deref(),
        Some("From H1")
    );
    assert_eq!(
        md::index_document("d/x.md", "body only\n", &bounds)
            .title
            .as_deref(),
        Some("x")
    );
}

// ---------------------------------------------------------------------------
// 3. Embedding identity
// ---------------------------------------------------------------------------

#[test]
fn embedding_identity_is_stable_and_independent_of_path() {
    let bounds = ChunkBounds::default();
    let a = md::index_document("docs/guide.md", RICH, &bounds);
    let b = md::index_document("docs/guide.md", RICH, &bounds);
    let renamed = md::index_document("archive/v2/other-name.md", RICH, &bounds);

    let ids = |doc: &md::Document| -> Vec<String> {
        doc.chunks
            .iter()
            .map(|c| c.embedding_identity.clone())
            .collect()
    };
    assert_eq!(ids(&a), ids(&b), "identity must be stable across runs");
    assert_eq!(ids(&a), ids(&renamed), "a rename must reuse every vector");
    assert!(a.chunks.iter().all(|c| !c.embedding_identity.is_empty()));

    // Nothing but (FORMAT_VERSION, nearest_heading, rendered_body) enters it.
    for chunk in &a.chunks {
        assert_eq!(
            chunk.embedding_identity,
            md::embedding_identity(chunk.nearest_heading.as_deref(), &chunk.rendered_body)
        );
    }

    // Same body under a different nearest heading is a different identity;
    // same body and heading under a different ancestor breadcrumb is not.
    let body = "Intro paragraph.";
    assert_ne!(
        md::embedding_identity(Some("Sub"), body),
        md::embedding_identity(Some("Other"), body)
    );
    let deep = md::index_document("x.md", "# Renamed Top\n\n## Sub\n\nshared body\n", &bounds);
    let shallow_doc = md::index_document(
        "x.md",
        "# Different Top\n\n## Sub\n\nshared body\n",
        &bounds,
    );
    let pick = |doc: &md::Document| {
        doc.chunks
            .iter()
            .find(|c| c.rendered_body == "shared body")
            .unwrap()
            .embedding_identity
            .clone()
    };
    assert_eq!(
        pick(&deep),
        pick(&shallow_doc),
        "an ancestor-heading edit must be metadata-only"
    );

    // The identity is exactly blake3 of the serialized embedder input, and the
    // format version is inside it.
    assert!(md::embedding_input(Some("Sub"), body).starts_with(md::FORMAT_VERSION));
    assert_eq!(
        md::embedding_identity(Some("Sub"), body),
        md::hash_hex(md::embedding_input(Some("Sub"), body).as_bytes())
    );
}

#[test]
fn oversized_heading_is_truncated_with_the_exact_literal() {
    let bounds = ChunkBounds::default();
    let heading = "H".repeat(5_000);
    let source = format!("# {heading}\n\nbody text\n");
    let doc = md::index_document("x.md", &source, &bounds);
    let chunk = doc
        .chunks
        .iter()
        .find(|c| !c.is_stub)
        .expect("one body chunk");
    let nearest = chunk.nearest_heading.as_deref().expect("nearest heading");

    assert!(
        nearest.ends_with(md::HEADING_TRUNCATED),
        "must use the exact literal: {:?}",
        &nearest[nearest.len() - 40..]
    );
    assert!(
        nearest.len() <= bounds.heading_ctx_max,
        "bounded heading is {} bytes",
        nearest.len()
    );
    assert_eq!(
        nearest.len(),
        bounds.heading_ctx_max,
        "the largest prefix that leaves room is retained"
    );
    // The BOUNDED value is what the identity uses.
    assert_eq!(
        chunk.embedding_identity,
        md::embedding_identity(Some(nearest), &chunk.rendered_body)
    );
    // The untruncated heading survives on the block for metadata purposes.
    let block = doc
        .blocks
        .iter()
        .find(|b| b.kind == BlockKind::Heading)
        .unwrap();
    assert_eq!(block.nearest_heading.as_deref(), Some(heading.as_str()));
}

// ---------------------------------------------------------------------------
// 4. HTML comments
// ---------------------------------------------------------------------------

#[test]
fn html_comments_leave_raw_and_spans_untouched() {
    let source =
        "# H\n\nVisible <!-- secret --> text.\n\n<!-- whole block -->\n\n<div>kept</div>\n";
    let doc = md::index_document("x.md", source, &ChunkBounds::default());

    let paragraph = doc
        .blocks
        .iter()
        .find(|b| b.kind == BlockKind::Paragraph)
        .unwrap();
    assert!(
        paragraph.raw.contains("<!-- secret -->"),
        "raw keeps the comment"
    );
    assert_eq!(
        &source[paragraph.byte_start..paragraph.byte_end],
        paragraph.raw,
        "spans unaffected"
    );
    assert!(
        !paragraph.rendered.contains("secret"),
        "rendered drops the comment: {:?}",
        paragraph.rendered
    );

    let comment_only = doc
        .blocks
        .iter()
        .find(|b| b.kind == BlockKind::HtmlBlock && b.raw.contains("whole block"))
        .expect("comment-only html block");
    assert_eq!(
        comment_only.rendered, "",
        "a comment-only block renders empty"
    );
    assert_eq!(
        &source[comment_only.byte_start..comment_only.byte_end],
        comment_only.raw
    );

    // No chunk anywhere leaks comment text, and the empty block produced none.
    for chunk in &doc.chunks {
        assert!(
            !chunk.rendered_body.contains("secret"),
            "{:?}",
            chunk.rendered_body
        );
        assert!(
            !chunk.rendered_body.contains("whole block"),
            "{:?}",
            chunk.rendered_body
        );
    }
    assert!(doc
        .chunks
        .iter()
        .any(|c| c.rendered_body.contains("<div>kept</div>")));
}

// ---------------------------------------------------------------------------
// 5. Chunking, splitting, and the hard bound
// ---------------------------------------------------------------------------

#[test]
fn chunks_never_cross_heading_boundaries_and_respect_normal_max() {
    let bounds = ChunkBounds::default();
    let doc = md::index_document("x.md", RICH, &bounds);
    for chunk in &doc.chunks {
        let crumbs: Vec<&Vec<String>> = chunk
            .blocks
            .iter()
            .map(|i| &doc.blocks[*i].breadcrumb)
            .collect();
        assert!(
            crumbs.windows(2).all(|w| w[0] == w[1]),
            "chunk spans two headings: {crumbs:?}"
        );
        assert!(chunk
            .blocks
            .iter()
            .all(|i| doc.blocks[*i].kind.is_retrieval_bearing()));
        assert!(
            chunk.rendered_body.len() <= bounds.normal_max,
            "merged chunk exceeds normal_max: {}",
            chunk.rendered_body.len()
        );
    }
    // Ordinals are dense per breadcrumb.
    let sub: Vec<usize> = doc
        .chunks
        .iter()
        .filter(|c| c.breadcrumb == vec!["Top".to_string(), "Sub".to_string()])
        .map(|c| c.same_heading_ordinal)
        .collect();
    assert_eq!(sub, (0..sub.len()).collect::<Vec<_>>());
}

#[test]
fn oversized_blocks_split_at_native_boundaries_tile_their_source_and_respect_the_hard_bound() {
    // A tight hard bound forces many fragments per block type.
    let bounds = ChunkBounds {
        hard_max: 400,
        ..Default::default()
    };

    let mut code = String::from("# Code\n\n```rust\n");
    for i in 0..80 {
        code.push_str(&format!("let x{i} = {i};\n"));
    }
    code.push_str("```\n");

    let mut list = String::from("# List\n\n");
    for i in 0..40 {
        list.push_str(&format!("- item {i} words\n  continuation line\n"));
    }

    let mut table = String::from("# Table\n\n| col a | col b |\n| --- | --- |\n");
    for i in 0..200 {
        table.push_str(&format!("| r{i}a | r{i}b |\n"));
    }

    for (label, source, kind) in [
        ("code", code.as_str(), BlockKind::CodeFenced),
        ("list", list.as_str(), BlockKind::List),
        ("table", table.as_str(), BlockKind::Table),
    ] {
        let doc = md::index_document("x.md", source, &bounds);
        let block_index = doc.blocks.iter().position(|b| b.kind == kind).expect(label);
        let block = &doc.blocks[block_index];
        let fragments: Vec<&md::Chunk> = doc
            .chunks
            .iter()
            .filter(|c| c.blocks == vec![block_index])
            .collect();
        assert!(
            fragments.len() > 3,
            "{label}: expected several fragments, got {}",
            fragments.len()
        );

        // Fragments tile the block's source exactly, in order, with no gaps.
        assert_eq!(
            fragments[0].byte_start, block.byte_start,
            "{label}: first fragment start"
        );
        assert_eq!(
            fragments.last().unwrap().byte_end,
            block.byte_end,
            "{label}: last fragment end"
        );
        for pair in fragments.windows(2) {
            assert_eq!(
                pair[0].byte_end, pair[1].byte_start,
                "{label}: fragments must tile"
            );
        }

        for (ordinal, fragment) in fragments.iter().enumerate() {
            // Spans stay exact into the ORIGINAL bytes.
            let slice = &source[fragment.byte_start..fragment.byte_end];
            assert!(
                fragment.rendered_body.ends_with(slice),
                "{label}: fragment body must end with its exact source slice"
            );
            assert_eq!(
                fragment.line_start,
                line_of(source, fragment.byte_start),
                "{label}: line_start"
            );
            assert_eq!(
                fragment.line_end,
                line_of(source, fragment.byte_end - 1),
                "{label}: line_end"
            );

            // The hard bound applies to the FINAL embedding input.
            let input =
                md::embedding_input(fragment.nearest_heading.as_deref(), &fragment.rendered_body);
            assert!(
                input.len() <= bounds.hard_max,
                "{label}: input is {} bytes",
                input.len()
            );

            // Fragments after the first repeat the bounded synthetic context.
            if ordinal > 0 {
                match kind {
                    BlockKind::CodeFenced => assert!(
                        fragment.rendered_body.starts_with("```rust\n"),
                        "{label}: fragment {ordinal} must repeat the fence info string"
                    ),
                    BlockKind::Table => assert!(
                        fragment
                            .rendered_body
                            .starts_with("| col a | col b |\n| --- | --- |\n"),
                        "{label}: fragment {ordinal} must repeat the table header"
                    ),
                    // A list has no synthetic context to repeat.
                    _ => assert_eq!(fragment.rendered_body, slice),
                }
            }
            // Native boundary: every fragment but the last ends on a line
            // break, and list fragments start at a top-level item.
            if ordinal + 1 < fragments.len() {
                assert!(
                    slice.ends_with('\n'),
                    "{label}: fragment {ordinal} must end at a native boundary"
                );
            }
            if kind == BlockKind::List {
                assert!(
                    slice.starts_with("- item"),
                    "{label}: fragment {ordinal} must start at an item"
                );
            }
        }
    }
}

#[test]
fn the_hard_bound_holds_at_the_plan_defaults_for_a_very_large_block() {
    let bounds = ChunkBounds::default();
    let heading = "H".repeat(4_000); // forces heading truncation too
    let mut source = format!("# {heading}\n\n```text\n");
    for i in 0..8_000 {
        source.push_str(&format!("row {i} of a very long generated code block\n"));
    }
    source.push_str("```\n");

    let doc = md::index_document("x.md", &source, &bounds);
    assert!(
        doc.chunks.len() > 10,
        "a ~350 KB block must split, got {}",
        doc.chunks.len()
    );
    for chunk in &doc.chunks {
        let input = md::embedding_input(chunk.nearest_heading.as_deref(), &chunk.rendered_body);
        assert!(
            input.len() <= bounds.hard_max,
            "final embedding input is {} bytes",
            input.len()
        );
        assert_eq!(
            &source[chunk.byte_start..chunk.byte_end].len(),
            &(chunk.byte_end - chunk.byte_start)
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Body-empty documents
// ---------------------------------------------------------------------------

#[test]
fn a_body_empty_document_emits_exactly_one_lexical_only_stub() {
    let bounds = ChunkBounds::default();
    let source = "---\ntitle: Only Headings\n---\n\n# Alpha\n\n## Beta\n\n### Gamma\n";
    let doc = md::index_document("docs/empty.md", source, &bounds);

    assert_eq!(
        doc.chunks.len(),
        1,
        "exactly one stub row: {:#?}",
        doc.chunks
    );
    let stub = &doc.chunks[0];
    assert!(stub.is_stub);
    assert_eq!(stub.rendered_body, "", "empty rendered body");
    assert_eq!(stub.nearest_heading, None, "no nearest heading");
    assert_eq!(
        stub.stub_headings,
        vec!["Alpha", "Beta", "Gamma"],
        "all headings in source order"
    );
    assert_eq!(
        stub.breadcrumb, stub.stub_headings,
        "carried by the breadcrumb column"
    );
    assert_eq!(
        (stub.byte_start, stub.byte_end),
        (0, source.len()),
        "span covers the file"
    );
    assert_eq!(stub.embedding_identity, "", "stubs are never embedded");

    // Other body-empty shapes reach the same single-stub outcome.
    for empty in ["", "---\ntitle: t\n---\n", "<!-- only a comment -->\n"] {
        let doc = md::index_document("docs/x.md", empty, &bounds);
        assert_eq!(doc.chunks.len(), 1, "{empty:?}");
        assert!(doc.chunks[0].is_stub, "{empty:?}");
        assert!(doc.chunks[0].embedding_identity.is_empty(), "{empty:?}");
    }

    // A document WITH a body emits no stub.
    let doc = md::index_document("docs/x.md", "# A\n\nbody\n", &bounds);
    assert!(doc.chunks.iter().all(|c| !c.is_stub));
}

// ---------------------------------------------------------------------------
// 7. Git laboratory
// ---------------------------------------------------------------------------

fn seeded_lab() -> anyhow::Result<git::GitLab> {
    let lab = git::GitLab::init()?;
    lab.write("docs/a.md", "line one\nline two\nline three\n")?;
    lab.commit_at("first", 1_000_000_000, 1_700_000_000)?;
    lab.write("docs/a.md", "line one\nline two CHANGED\nline three\n")?;
    lab.commit_at("second", 1_100_000_000, 1_700_000_100)?;
    lab.write("docs/b.md", "unrelated\n")?;
    lab.commit_at("third", 1_200_000_000, 1_700_000_200)?;
    Ok(lab)
}

#[test]
fn the_lab_is_hermetic_and_deterministic() -> anyhow::Result<()> {
    let lab = git::GitLab::init()?;
    lab.write("a.md", "x\n")?;
    lab.commit("only")?;

    assert_eq!(
        lab.git(&["rev-parse", "--abbrev-ref", "HEAD"])?,
        "main",
        "deterministic default branch"
    );
    assert_eq!(
        lab.git(&["config", "--local", "--get", "commit.gpgsign"])?,
        "false"
    );
    assert_eq!(lab.git(&["log", "-1", "--format=%an"])?, "Harness Author");
    assert_eq!(
        lab.git(&["log", "-1", "--format=%cn"])?,
        "Harness Committer"
    );

    // Prove ambient configuration really is neutralized, rather than merely
    // absent on this machine: a hostile global config is visible WITHOUT
    // hermetic_env and invisible WITH it.
    let hostile_dir = tempfile::tempdir()?;
    let hostile = hostile_dir.path().join("gitconfig");
    std::fs::write(&hostile, "[blame]\n\tignoreRevsFile = /nonexistent/revs\n")?;
    let hostile_path = hostile.to_string_lossy().into_owned();

    let leaked = proc::run(
        Path::new("git"),
        &["config", "--get", "blame.ignoreRevsFile"],
        lab.path(),
        &[
            ("GIT_CONFIG_GLOBAL", hostile_path.as_str()),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ],
    );
    assert!(
        leaked.ok && leaked.stdout.trim() == "/nonexistent/revs",
        "the hostile config must be real: {leaked:?}"
    );

    let sealed = lab.git_raw(&["config", "--get", "blame.ignoreRevsFile"]);
    assert!(
        !sealed.ok,
        "hermetic_env must hide global config: {sealed:?}"
    );
    assert_eq!(sealed.stdout.trim(), "");

    // And blame still works under it (a leaked ignore-revs file would abort).
    let blame = git::blame_porcelain(
        lab.path(),
        "a.md",
        &["--no-replace-objects", "-c", "blame.ignoreRevsFile="],
    )?;
    assert_eq!(blame.len(), 1);
    Ok(())
}

#[test]
fn commit_at_diverges_author_and_committer_time() -> anyhow::Result<()> {
    let lab = seeded_lab()?;
    assert_eq!(
        lab.git(&["log", "-1", "--format=%at"])?,
        "1200000000",
        "author epoch"
    );
    assert_eq!(
        lab.git(&["log", "-1", "--format=%ct"])?,
        "1700000200",
        "committer epoch"
    );

    let blame = git::blame_porcelain(lab.path(), "docs/a.md", &[])?;
    let changed = blame
        .iter()
        .find(|l| l.content == "line two CHANGED")
        .expect("changed line");
    assert_eq!(changed.author_time, 1_100_000_000);
    assert_eq!(changed.committer_time, 1_700_000_100);
    assert_ne!(
        changed.author_time, changed.committer_time,
        "the two clocks must be independently settable"
    );
    // Author time is the one that survives history rewriting; blame reports both.
    let untouched = blame
        .iter()
        .find(|l| l.content == "line one")
        .expect("untouched line");
    assert_eq!(untouched.author_time, 1_000_000_000);
    assert_eq!(untouched.committer_time, 1_700_000_000);
    Ok(())
}

#[test]
fn blame_porcelain_round_trips_the_file_and_parses_every_documented_field() -> anyhow::Result<()> {
    let lab = seeded_lab()?;
    let blame = git::blame_porcelain(
        lab.path(),
        "docs/a.md",
        &["--no-replace-objects", "-c", "blame.ignoreRevsFile="],
    )?;

    let on_disk = std::fs::read_to_string(lab.path().join("docs/a.md"))?;
    let expected: Vec<&str> = on_disk.lines().collect();
    assert_eq!(blame.len(), expected.len(), "one blame entry per line");
    for (index, line) in blame.iter().enumerate() {
        assert_eq!(
            line.final_line,
            index + 1,
            "final_line is 1-based and ordered"
        );
        assert_eq!(line.content, expected[index], "content round-trips");
        assert_eq!(line.filename.as_deref(), Some("docs/a.md"));
        assert_eq!(line.author, "Harness Author");
        assert!(line.sha.len() >= 40 && line.sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!line.not_committed_yet);
    }
    // `previous <sha> <path>` is parsed for a line whose commit has a parent.
    let changed = blame
        .iter()
        .find(|l| l.content == "line two CHANGED")
        .unwrap();
    assert_eq!(changed.previous_filename.as_deref(), Some("docs/a.md"));

    // The all-zero "not committed yet" SHA appears for a dirty worktree line.
    lab.write("docs/a.md", "line one\nDIRTY EDIT\nline three\n")?;
    let dirty = git::blame_porcelain(lab.path(), "docs/a.md", &[])?;
    let uncommitted = dirty
        .iter()
        .find(|l| l.content == "DIRTY EDIT")
        .expect("dirty line");
    assert!(
        uncommitted.not_committed_yet,
        "all-zero sha must be recognized: {}",
        uncommitted.sha
    );
    assert!(uncommitted.sha.chars().all(|c| c == '0'));
    assert_eq!(uncommitted.author, "Not Committed Yet");
    assert!(
        dirty.iter().filter(|l| !l.not_committed_yet).count() == 2,
        "only the edited line is uncommitted"
    );
    Ok(())
}

/// The SHAs listed in `.git/shallow` - the authoritative shallow-graft set.
fn shallow_set(repo: &Path) -> anyhow::Result<Vec<String>> {
    let path = git::git_dir(repo)?.join("shallow");
    Ok(match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    })
}

#[test]
fn the_blame_boundary_flag_does_not_mean_shallow() -> anyhow::Result<()> {
    // OBSERVED, and a hazard for the plan. The plan says "shallow-clone boundary
    // commits contribute no timestamp" and "a chunk whose contributing lines all
    // blame to a boundary commit has unknown git age". Git 2.49 marks ROOT
    // commits as boundaries by default in an ordinary, complete repository, so
    // keying "unknown git age" off the boundary flag would erase the age of
    // every line still attributed to a repository's first commit.
    let lab = seeded_lab()?;
    assert!(
        git::shallow_boundary_fingerprint(lab.path())?.is_none(),
        "this repo is not shallow"
    );
    assert!(shallow_set(lab.path())?.is_empty());

    let default_blame = git::blame_porcelain(lab.path(), "docs/a.md", &[])?;
    let from_root = default_blame
        .iter()
        .find(|l| l.content == "line one")
        .unwrap();
    assert!(
        from_root.boundary,
        "git marks root-commit lines as boundary by default"
    );
    assert_eq!(
        from_root.author_time, 1_000_000_000,
        "yet its author time is genuine, not unknown"
    );

    // `--root` clears the flag here - but see the shallow test: it clears it on
    // a shallow graft too, so it cannot be used to tell the two cases apart.
    let rooted = git::blame_porcelain(lab.path(), "docs/a.md", &["--root"])?;
    assert!(
        rooted.iter().all(|l| !l.boundary),
        "--root removes the marker in a complete repo"
    );
    Ok(())
}

#[test]
fn clone_shallow_produces_a_genuinely_shallow_clone() -> anyhow::Result<()> {
    let lab = seeded_lab()?;
    let clone = lab.clone_shallow(1)?;

    // `.git/shallow` exists: a plain local-path clone would silently ignore
    // --depth and this would be absent.
    let shallow_file = git::git_dir(clone.path())?.join("shallow");
    assert!(
        shallow_file.is_file(),
        "{} must exist",
        shallow_file.display()
    );
    assert_eq!(
        clone.git(&["rev-list", "--count", "HEAD"])?,
        "1",
        "only one commit was fetched"
    );
    assert_eq!(clone.git(&["log", "--format=%s"])?, "third");

    let before = git::shallow_boundary_fingerprint(clone.path())?.expect("clone is shallow");
    let grafts = shallow_set(clone.path())?;
    assert_eq!(
        grafts,
        vec![clone.head()?],
        ".git/shallow lists the grafted boundary commit"
    );

    // Every line blames to the boundary commit, whose author time (1.2e9) is
    // NOT when those lines were written (1.0e9 / 1.1e9). This is exactly the
    // condition the plan calls "unknown git age", and the plan is right about it.
    let blame = git::blame_porcelain(clone.path(), "docs/a.md", &[])?;
    assert!(
        blame.iter().all(|l| l.boundary),
        "all lines blame to the shallow boundary"
    );
    assert!(
        blame.iter().all(|l| l.author_time == 1_200_000_000),
        "the boundary commit's own time is reported for lines it did not author"
    );
    assert!(
        blame.iter().all(|l| grafts.contains(&l.sha)),
        "each blamed sha is a graft"
    );

    // OBSERVED: `--root` also suppresses the marker on a SHALLOW graft, because
    // a grafted commit has no parents. So neither the bare boundary flag nor
    // `--root` distinguishes "repository's first commit" from "shallow graft";
    // only membership in `.git/shallow` does. An implementation that follows
    // the plan's wording literally must use the shallow set, not the flag.
    let rooted = git::blame_porcelain(clone.path(), "docs/a.md", &["--root"])?;
    assert!(
        rooted.iter().all(|l| !l.boundary),
        "--root hides the shallow boundary too, so the flag alone is ambiguous"
    );
    assert!(
        rooted.iter().all(|l| grafts.contains(&l.sha)),
        "the shallow set still identifies them"
    );

    // Deepening changes the boundary fingerprint and restores real attribution.
    clone.deepen(2)?;
    let after = git::shallow_boundary_fingerprint(clone.path())?.expect("still shallow-capable");
    assert_ne!(
        before, after,
        "the fingerprint must invalidate a blame cache on deepening"
    );
    let deep = git::blame_porcelain(clone.path(), "docs/a.md", &[])?;
    assert_eq!(
        deep.iter()
            .find(|l| l.content == "line one")
            .unwrap()
            .author_time,
        1_000_000_000,
        "after deepening the true author time is recovered"
    );
    assert_eq!(
        deep.iter()
            .find(|l| l.content == "line two CHANGED")
            .unwrap()
            .author_time,
        1_100_000_000
    );
    Ok(())
}

#[test]
fn path_tip_commit_tracks_only_its_own_path() -> anyhow::Result<()> {
    let empty = git::GitLab::init()?;
    assert_eq!(
        git::path_tip_commit(empty.path(), "docs/a.md")?,
        None,
        "unborn branch has no tip"
    );

    let lab = seeded_lab()?;
    let tip_a = git::path_tip_commit(lab.path(), "docs/a.md")?.expect("a has a tip");
    let tip_b = git::path_tip_commit(lab.path(), "docs/b.md")?.expect("b has a tip");
    assert_ne!(tip_a, tip_b);
    assert_eq!(tip_b, lab.head()?, "b was touched by the newest commit");
    assert_eq!(git::path_tip_commit(lab.path(), "docs/missing.md")?, None);

    // An unrelated commit must not move a path's tip (the plan's cache-key claim).
    lab.write("docs/b.md", "unrelated edit\n")?;
    lab.commit_at("fourth", 1_300_000_000, 1_700_000_300)?;
    assert_eq!(
        git::path_tip_commit(lab.path(), "docs/a.md")?.as_deref(),
        Some(tip_a.as_str())
    );

    // Staging an unchanged worktree file must not move it either.
    lab.add_all()?;
    assert_eq!(
        git::path_tip_commit(lab.path(), "docs/a.md")?.as_deref(),
        Some(tip_a.as_str())
    );

    // Touching the path does move it.
    lab.write(
        "docs/a.md",
        "line one\nline two CHANGED AGAIN\nline three\n",
    )?;
    lab.commit_at("fifth", 1_400_000_000, 1_700_000_400)?;
    assert_eq!(
        git::path_tip_commit(lab.path(), "docs/a.md")?.as_deref(),
        Some(lab.head()?.as_str())
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. Awkward source shapes must not break span exactness
// ---------------------------------------------------------------------------

#[test]
fn awkward_markdown_shapes_keep_spans_and_lines_exact() {
    let cases: &[(&str, &str)] = &[
        ("setext", "Setext Title\n============\n\nbody\n\nSub Title\n---------\n\nmore\n"),
        ("indented code", "# H\n\n    indented code\n    second line\n\npara\n"),
        ("nested containers", "# H\n\n> quote\n> - a\n> - b\n\n1. one\n   - nested\n2. two\n"),
        ("crlf", "# H\r\n\r\npara one\r\n\r\npara two\r\n"),
        ("multibyte", "# \u{dc}n\u{ef}c\u{f6}d\u{e9} \u{2603}\n\npros\u{e9} with \u{1f389} and \u{6f22}\u{5b57}\n"),
        ("no trailing newline", "# H\n\nlast para"),
        ("thematic vs setext", "para\n---\nnext\n"),
    ];

    for (label, source) in cases {
        let blocks = md::parse_blocks(source);
        assert!(!blocks.is_empty(), "{label}: produced no blocks");
        for block in &blocks {
            assert_eq!(
                &source[block.byte_start..block.byte_end],
                block.raw,
                "{label}: exact slice"
            );
            assert_eq!(
                block.line_start,
                line_of(source, block.byte_start),
                "{label}: line_start"
            );
            assert_eq!(
                block.line_end,
                line_of(source, block.byte_end - 1),
                "{label}: line_end"
            );
        }
        for chunk in md::index_document("f.md", source, &ChunkBounds::default()).chunks {
            if !chunk.is_stub {
                assert!(
                    source.get(chunk.byte_start..chunk.byte_end).is_some(),
                    "{label}: chunk span"
                );
            }
        }
    }

    // Setext headings are real headings, with the level CommonMark assigns:
    // `===` is H1 (so it feeds title derivation) and `---` under a paragraph is
    // H2, NOT a thematic break.
    let setext = md::parse_blocks("Setext Title\n============\n\nbody\n");
    assert_eq!(setext[0].kind, BlockKind::Heading);
    assert_eq!(setext[0].heading_level, Some(1));
    assert_eq!(setext[0].nearest_heading.as_deref(), Some("Setext Title"));
    let ambiguous = md::parse_blocks("para\n---\nnext\n");
    assert_eq!(
        ambiguous[0].kind,
        BlockKind::Heading,
        "`---` after a paragraph is a setext H2"
    );
    assert_eq!(ambiguous[0].heading_level, Some(2));

    // OBSERVED corpus hazard: a UTF-8 BOM is not stripped, so `\u{feff}# H` is a
    // PARAGRAPH, not an H1. The plan's title fallback "front-matter title ->
    // first H1 -> file stem" therefore silently skips to the file stem for any
    // BOM-prefixed document, and a leading `\u{feff}---` is not front matter either.
    let bom = "\u{feff}# H\n\nbody\n";
    let blocks = md::parse_blocks(bom);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Paragraph,
        "a BOM defeats heading recognition"
    );
    assert_eq!(
        md::index_document("d/stem.md", bom, &ChunkBounds::default())
            .title
            .as_deref(),
        Some("stem")
    );
    let (front, offset) = md::parse_front_matter("\u{feff}---\ntitle: t\n---\nbody\n");
    assert_eq!(
        front.state,
        Some(FrontMatterState::Absent),
        "a BOM defeats front-matter recognition"
    );
    assert_eq!(offset, 0);
}
