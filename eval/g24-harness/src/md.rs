//! Prototype of the G24 Markdown corpus specification: front matter, block
//! parsing, chunking, and embedding identity.
//!
//! Every rule here is transcribed from the proposal so the tests can check the
//! *specified* behavior rather than a convenient approximation. Section
//! references below are to `docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md`.
//!
//! # Rules the plan left underspecified
//!
//! Where the plan does not decide something the implementation must decide,
//! the choice is marked `INVENTED:` in a comment at the point of use. The
//! complete list is also reported by the harness run.

use std::collections::HashMap;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};

/// Versioned chunk/representation format tag; participates in embedding identity.
pub const FORMAT_VERSION: &str = "markdown-v1";

/// Literal appended when the nearest heading exceeds its context bound.
pub const HEADING_TRUNCATED: &str = "\n[heading truncated]";
/// Literal appended when synthetic fence/table context exceeds its bound.
pub const CONTEXT_TRUNCATED: &str = "\n[context truncated]";

/// Deterministic size bounds. Defaults are the plan's stated hypotheses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkBounds {
    /// Target merged size, in rendered UTF-8 bytes (~600 tokens).
    pub target: usize,
    /// Normal maximum merged size, in rendered UTF-8 bytes (~1,000 tokens).
    pub normal_max: usize,
    /// Hard maximum of the FINAL embedding input, in UTF-8 bytes.
    pub hard_max: usize,
    /// Maximum rendered bytes of serialized nearest-heading context.
    pub heading_ctx_max: usize,
    /// Maximum rendered bytes of synthetic fence/table context.
    pub synthetic_max: usize,
}

impl Default for ChunkBounds {
    fn default() -> Self {
        Self {
            target: 2_400,
            normal_max: 4_000,
            hard_max: 24_000,
            heading_ctx_max: 1_024,
            synthetic_max: 1_024,
        }
    }
}

/// Outcome of front-matter recognition, mirrored by `docs status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontMatterState {
    /// No leading `---` delimiter at all.
    Absent,
    /// Recognized: delimited, valid YAML, and a top-level mapping.
    Parsed,
    /// Delimited but unterminated, invalid YAML, or not a top-level mapping.
    /// Reported as `front_matter=malformed_as_body`; the text stays in the body.
    MalformedAsBody,
}

/// Front matter fields the corpus admits. Only scalar-string `title` and
/// `description`, and a scalar string or sequence of scalar strings for `tags`,
/// are used; all other keys and value shapes are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub state: Option<FrontMatterState>,
}

/// Source-backed block kinds the chunker distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Paragraph,
    Heading,
    List,
    Table,
    BlockQuote,
    CodeFenced,
    CodeIndented,
    HtmlBlock,
    ThematicBreak,
}

impl BlockKind {
    /// Headings establish structure and metadata but are not independent
    /// history occurrences; the ledger tracks retrieval-bearing body blocks.
    /// Thematic breaks carry no retrieval text either.
    pub fn is_retrieval_bearing(self) -> bool {
        !matches!(self, BlockKind::Heading | BlockKind::ThematicBreak)
    }

    /// True when block content is literal text where `<!-- -->` is not an HTML
    /// comment. INVENTED: the plan says "HTML comments are excluded from
    /// retrieval text" without saying whether code content is scanned. Scanning
    /// code would mutilate source samples that legitimately contain the
    /// sequence, so code blocks are exempt.
    fn is_literal_text(self) -> bool {
        matches!(self, BlockKind::CodeFenced | BlockKind::CodeIndented)
    }
}

/// One source-backed Markdown block. Spans are absolute byte/line offsets into
/// the ORIGINAL file, including any front matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// Heading level 1-6 for `BlockKind::Heading`, otherwise `None`.
    pub heading_level: Option<u8>,
    pub byte_start: usize,
    pub byte_end: usize,
    /// 1-based inclusive line span.
    pub line_start: usize,
    pub line_end: usize,
    /// Full heading path enclosing this block, outermost first.
    pub breadcrumb: Vec<String>,
    /// Closest enclosing heading text, before any context-bound truncation.
    pub nearest_heading: Option<String>,
    /// Exact source slice `source[byte_start..byte_end]`.
    pub raw: String,
    /// Retrieval rendering of `raw`: HTML comments removed. Synthetic
    /// fence/table context is NOT included here; it is added per chunk.
    pub rendered: String,
    /// blake3 hex of `raw`, the block's content identity for history matching.
    pub content_hash: String,
}

/// A retrieval chunk: one or more adjacent retrieval-bearing blocks under the
/// same heading, merged toward `target` and never crossing a heading boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub path: String,
    /// Indices into [`Document::blocks`] contributing to this chunk.
    pub blocks: Vec<usize>,
    pub breadcrumb: Vec<String>,
    /// Nearest heading AFTER context-bound truncation; this exact value enters
    /// the embedding identity.
    pub nearest_heading: Option<String>,
    /// Final deterministic body string sent to FTS and the embedder: rendered
    /// block text after comment removal, prefixed by any bounded synthetic
    /// fence/table context.
    pub rendered_body: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_end: usize,
    /// Ordinal among chunks sharing the same heading breadcrumb, from 0.
    /// Establishes order only; never historical succession.
    pub same_heading_ordinal: usize,
    /// `hash(FORMAT_VERSION, nearest_heading, rendered_body)`.
    pub embedding_identity: String,
    /// True for the single lexical-only row emitted by a body-empty document.
    /// Stubs are never embedded.
    pub is_stub: bool,
    /// For stub rows: every document heading in source order, carried by the
    /// breadcrumb column. Empty for ordinary chunks.
    pub stub_headings: Vec<String>,
}

/// A fully indexed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub path: String,
    pub front_matter: FrontMatter,
    /// front-matter `title` -> first H1 -> file stem.
    pub title: Option<String>,
    pub blocks: Vec<Block>,
    pub chunks: Vec<Chunk>,
}

// ---------------------------------------------------------------------------
// Front matter
// ---------------------------------------------------------------------------

/// Recognize front matter per the plan: only when the file begins with `---`,
/// has a valid closing delimiter, parses as YAML, AND produces a top-level
/// mapping. A valid YAML scalar or sequence is NOT front matter.
///
/// Returns the parsed fields and the byte offset at which the document body
/// begins (0 when front matter is absent or malformed-as-body).
pub fn parse_front_matter(source: &str) -> (FrontMatter, usize) {
    let mut fm = FrontMatter::default();

    // "begins with `---`". INVENTED: the delimiter must be its own line whose
    // trailing whitespace-trimmed content is exactly `---`. `----`, `--- x`,
    // and an indented `  ---` are therefore not opening delimiters. Without
    // this the rule cannot be applied at all.
    let first_end = line_end_of(source, 0);
    if !is_delimiter_line(&source[..first_end]) {
        fm.state = Some(FrontMatterState::Absent);
        return (fm, 0);
    }

    // "valid closing delimiter". INVENTED: only `---` closes; YAML's `...`
    // document terminator does not. The plan names one delimiter.
    let mut cursor = first_end;
    let mut close: Option<(usize, usize)> = None;
    while cursor < source.len() {
        let end = line_end_of(source, cursor);
        if is_delimiter_line(&source[cursor..end]) {
            close = Some((cursor, end));
            break;
        }
        cursor = end;
    }

    let Some((yaml_end, body_start)) = close else {
        // Unterminated: ordinary Markdown body text.
        fm.state = Some(FrontMatterState::MalformedAsBody);
        return (fm, 0);
    };

    let yaml_src = &source[first_end..yaml_end];
    let value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(yaml_src) {
        Ok(value) => value,
        Err(_) => {
            fm.state = Some(FrontMatterState::MalformedAsBody);
            return (fm, 0);
        }
    };

    // A valid YAML scalar or sequence is NOT front matter.
    let Some(mapping) = value.as_mapping() else {
        fm.state = Some(FrontMatterState::MalformedAsBody);
        return (fm, 0);
    };

    fm.state = Some(FrontMatterState::Parsed);
    fm.title = scalar_string(mapping.get("title"));
    fm.description = scalar_string(mapping.get("description"));
    fm.tags = tag_list(mapping.get("tags"));
    (fm, body_start)
}

fn scalar_string(value: Option<&serde_yaml_ng::Value>) -> Option<String> {
    match value {
        Some(serde_yaml_ng::Value::String(text)) => Some(text.clone()),
        // Numbers, bools, nulls, sequences, and mappings are "other value
        // types" and are ignored.
        _ => None,
    }
}

fn tag_list(value: Option<&serde_yaml_ng::Value>) -> Vec<String> {
    match value {
        Some(serde_yaml_ng::Value::String(text)) => vec![text.clone()],
        Some(serde_yaml_ng::Value::Sequence(items)) => {
            // INVENTED: the plan admits "a sequence of scalar strings". A mixed
            // sequence is not that value type, so the whole value is ignored
            // rather than silently filtered.
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_yaml_ng::Value::String(text) => out.push(text.clone()),
                    _ => return Vec::new(),
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn is_delimiter_line(line: &str) -> bool {
    line.trim_end_matches(['\n', '\r']).trim_end() == "---"
}

fn line_end_of(source: &str, from: usize) -> usize {
    match source[from..].find('\n') {
        Some(offset) => from + offset + 1,
        None => source.len(),
    }
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

/// Parse the full file into source-backed blocks. Valid front matter is skipped
/// (never emitted as a body block); malformed front matter stays as body text.
/// Spans are absolute into `source`. HTML comments are excluded from
/// `Block::rendered` but never alter `Block::raw` or the spans.
pub fn parse_blocks(source: &str) -> Vec<Block> {
    let (_, body_start) = parse_front_matter(source);
    let body = &source[body_start..];

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);

    let mut blocks: Vec<Block> = Vec::new();
    // (level, text) stack of open headings.
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let mut depth: usize = 0;
    let mut pending: Option<(BlockKind, Option<u8>, std::ops::Range<usize>)> = None;
    let mut text_acc = String::new();

    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let (kind, level) = classify(&tag);
                    pending = Some((kind, level, range.clone()));
                    text_acc.clear();
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some((kind, level, range)) = pending.take() {
                        push_block(
                            &mut blocks,
                            &mut heading_stack,
                            source,
                            body_start + range.start,
                            body_start + range.end,
                            kind,
                            level,
                            &text_acc,
                        );
                    }
                }
            }
            Event::Rule => {
                if depth == 0 {
                    push_block(
                        &mut blocks,
                        &mut heading_stack,
                        source,
                        body_start + range.start,
                        body_start + range.end,
                        BlockKind::ThematicBreak,
                        None,
                        "",
                    );
                }
            }
            Event::Text(text) | Event::Code(text) if depth > 0 => {
                text_acc.push_str(&text);
            }
            _ => {}
        }
    }

    blocks
}

fn classify(tag: &Tag<'_>) -> (BlockKind, Option<u8>) {
    match tag {
        Tag::Paragraph => (BlockKind::Paragraph, None),
        Tag::Heading { level, .. } => (BlockKind::Heading, Some(heading_level_number(*level))),
        Tag::BlockQuote(_) => (BlockKind::BlockQuote, None),
        Tag::CodeBlock(CodeBlockKind::Fenced(_)) => (BlockKind::CodeFenced, None),
        Tag::CodeBlock(CodeBlockKind::Indented) => (BlockKind::CodeIndented, None),
        Tag::HtmlBlock => (BlockKind::HtmlBlock, None),
        Tag::List(_) => (BlockKind::List, None),
        Tag::Table(_) => (BlockKind::Table, None),
        // Any other top-level container (footnote definitions, definition
        // lists) is treated as a paragraph-like prose block. Those extensions
        // are not enabled here, so this arm is unreachable in practice.
        _ => (BlockKind::Paragraph, None),
    }
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_block(
    blocks: &mut Vec<Block>,
    heading_stack: &mut Vec<(u8, String)>,
    source: &str,
    byte_start: usize,
    byte_end_raw: usize,
    kind: BlockKind,
    heading_level: Option<u8>,
    text: &str,
) {
    // pulldown's element range includes the block's terminating newline (and,
    // for some containers, trailing blank lines). Trim trailing line breaks so
    // the span is the block's own text; `raw` is still an exact source slice.
    let mut byte_end = byte_end_raw.min(source.len());
    while byte_end > byte_start && matches!(source.as_bytes()[byte_end - 1], b'\n' | b'\r') {
        byte_end -= 1;
    }
    if byte_end <= byte_start {
        return;
    }

    let raw = source[byte_start..byte_end].to_string();

    let (breadcrumb, nearest_heading) = if kind == BlockKind::Heading {
        let level = heading_level.unwrap_or(1);
        while heading_stack.last().is_some_and(|(open, _)| *open >= level) {
            heading_stack.pop();
        }
        let own = text.trim().to_string();
        let mut crumb: Vec<String> = heading_stack.iter().map(|(_, text)| text.clone()).collect();
        // INVENTED: a heading's own breadcrumb includes itself and its nearest
        // heading is itself. The plan defines "nearest enclosing heading" only
        // for body blocks. Making the heading agree with the section it opens
        // keeps a heading and its first paragraph in one context.
        crumb.push(own.clone());
        heading_stack.push((level, own.clone()));
        (crumb, Some(own))
    } else {
        let crumb: Vec<String> = heading_stack.iter().map(|(_, text)| text.clone()).collect();
        (crumb, heading_stack.last().map(|(_, text)| text.clone()))
    };

    let rendered = render_for_retrieval(&raw, kind);

    blocks.push(Block {
        kind,
        heading_level,
        byte_start,
        byte_end,
        line_start: line_at(source, byte_start),
        line_end: line_at(source, byte_end.saturating_sub(1).max(byte_start)),
        breadcrumb,
        nearest_heading,
        raw,
        rendered,
        content_hash: hash_hex(&source.as_bytes()[byte_start..byte_end]),
    });
}

/// Retrieval rendering: HTML comments removed, nothing else changed.
fn render_for_retrieval(raw: &str, kind: BlockKind) -> String {
    if kind.is_literal_text() {
        return raw.to_string();
    }
    let stripped = strip_html_comments(raw);
    if stripped == raw {
        return stripped;
    }
    // Removing a comment can leave dangling trailing whitespace; a block that
    // was nothing but a comment renders empty and produces no chunk.
    let trimmed = stripped.trim_end();
    if trimmed.trim().is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Remove every terminated `<!-- ... -->` span. INVENTED: an unterminated
/// `<!--` is left alone (CommonMark does not treat it as a comment either), so
/// a stray `<!--` cannot silently delete the rest of a document.
fn strip_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(open) = rest.find("<!--") else {
            out.push_str(rest);
            return out;
        };
        let Some(close) = rest[open + 4..].find("-->") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + 4 + close + 3..];
    }
}

fn line_at(source: &str, byte: usize) -> usize {
    let byte = byte.min(source.len());
    source.as_bytes()[..byte]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

// ---------------------------------------------------------------------------
// Embedding identity
// ---------------------------------------------------------------------------

/// The exact versioned string sent to the embedder. Serialization is stable and
/// participates in identity. The nearest heading is truncated to
/// `heading_ctx_max` (appending [`HEADING_TRUNCATED`]) before inclusion, and the
/// total MUST respect `hard_max`.
pub fn embedding_input(nearest_heading: Option<&str>, rendered_body: &str) -> String {
    let bounds = ChunkBounds::default();
    let prefix = embedding_prefix(nearest_heading, &bounds);
    let room = bounds.hard_max.saturating_sub(prefix.len());
    // INVENTED: when the final input still exceeds `hard_max` the body is cut
    // at the last UTF-8 boundary that fits, with no marker. The plan defines
    // markers only for heading and synthetic context and says "the remaining
    // total byte budget is used for source text"; chunking normally keeps this
    // path unreached, so it is a safety net, not a formatting rule.
    let body = if rendered_body.len() <= room {
        rendered_body
    } else {
        &rendered_body[..floor_char_boundary(rendered_body, room)]
    };
    let mut out = String::with_capacity(prefix.len() + body.len());
    out.push_str(&prefix);
    out.push_str(body);
    out
}

/// Stable serialization header. INVENTED: the plan never fixes the wire format
/// of "the same versioned serialization sent to the embedder", only its inputs.
/// The header is `<format>\nheading: <bounded heading>\n\n`, with an empty
/// value when there is no nearest heading.
fn embedding_prefix(nearest_heading: Option<&str>, bounds: &ChunkBounds) -> String {
    let heading = nearest_heading
        .map(|h| bound_text(h, bounds.heading_ctx_max, HEADING_TRUNCATED))
        .unwrap_or_default();
    format!("{FORMAT_VERSION}\nheading: {heading}\n\n")
}

/// Largest UTF-8 prefix that leaves room for `marker`, with `marker` appended.
fn bound_text(text: &str, max: usize, marker: &str) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(marker.len());
    let cut = floor_char_boundary(text, room);
    let mut out = String::with_capacity(cut + marker.len());
    out.push_str(&text[..cut]);
    out.push_str(marker);
    out
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// blake3 hex of [`embedding_input`]. Nothing else enters this hash: not the
/// path, not the full breadcrumb, not the document title, not any timestamp.
pub fn embedding_identity(nearest_heading: Option<&str>, rendered_body: &str) -> String {
    hash_hex(embedding_input(nearest_heading, rendered_body).as_bytes())
}

// ---------------------------------------------------------------------------
// Document indexing
// ---------------------------------------------------------------------------

/// Index one document end to end: front matter, blocks, chunks, identities.
///
/// Chunking rules:
/// - chunks never cross heading boundaries;
/// - adjacent retrieval-bearing blocks under the same heading merge toward
///   `bounds.target`, never exceeding `bounds.normal_max`;
/// - an oversized atomic block splits at the last block-native boundary before
///   the remaining budget (newline for code, row for tables, top-level item for
///   lists), falling back to the last newline and then the last UTF-8 boundary;
/// - fragments repeat bounded synthetic context (fence info string, table
///   header) in `rendered_body` WITHOUT altering exact source spans;
/// - the hard bound applies to the FINAL embedding input, after nearest-heading
///   serialization and synthetic context are added;
/// - a document producing no body chunks emits exactly one lexical-only stub.
pub fn index_document(path: &str, source: &str, bounds: &ChunkBounds) -> Document {
    let (front_matter, _body_start) = parse_front_matter(source);
    let blocks = parse_blocks(source);

    let title = front_matter
        .title
        .clone()
        .or_else(|| {
            blocks
                .iter()
                .find(|block| block.kind == BlockKind::Heading && block.heading_level == Some(1))
                .and_then(|block| block.nearest_heading.clone())
                .filter(|text| !text.is_empty())
        })
        .or_else(|| file_stem(path));

    let chunks = build_chunks(path, source, &blocks, bounds);

    Document {
        path: path.to_string(),
        front_matter,
        title,
        blocks,
        chunks,
    }
}

fn file_stem(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    if name.is_empty() {
        return None;
    }
    Some(match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    })
}

struct ChunkBuilder<'a> {
    path: &'a str,
    source: &'a str,
    bounds: &'a ChunkBounds,
    chunks: Vec<Chunk>,
    ordinals: HashMap<Vec<String>, usize>,
    group: Vec<usize>,
    group_len: usize,
    crumb: Vec<String>,
    heading: Option<String>,
}

impl<'a> ChunkBuilder<'a> {
    fn next_ordinal(&mut self, crumb: &[String]) -> usize {
        let slot = self.ordinals.entry(crumb.to_vec()).or_insert(0);
        let value = *slot;
        *slot += 1;
        value
    }

    fn flush(&mut self, blocks: &[Block]) {
        if self.group.is_empty() {
            return;
        }
        let indices = std::mem::take(&mut self.group);
        self.group_len = 0;
        let rendered_body = indices
            .iter()
            .map(|i| blocks[*i].rendered.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let byte_start = blocks[indices[0]].byte_start;
        let byte_end = blocks[*indices.last().unwrap()].byte_end;
        let crumb = self.crumb.clone();
        let ordinal = self.next_ordinal(&crumb);
        let nearest = self.bounded_heading();
        self.chunks.push(Chunk {
            path: self.path.to_string(),
            blocks: indices,
            breadcrumb: crumb,
            embedding_identity: embedding_identity(nearest.as_deref(), &rendered_body),
            nearest_heading: nearest,
            rendered_body,
            byte_start,
            byte_end,
            line_start: line_at(self.source, byte_start),
            line_end: line_at(self.source, byte_end.saturating_sub(1).max(byte_start)),
            same_heading_ordinal: ordinal,
            is_stub: false,
            stub_headings: Vec::new(),
        });
    }

    fn bounded_heading(&self) -> Option<String> {
        self.heading
            .as_ref()
            .map(|h| bound_text(h, self.bounds.heading_ctx_max, HEADING_TRUNCATED))
    }

    /// Body budget left for source text once the heading serialization is
    /// accounted for. This is the plan's "remaining total byte budget".
    fn body_budget(&self) -> usize {
        let prefix = embedding_prefix(self.bounded_heading().as_deref(), self.bounds);
        self.bounds.hard_max.saturating_sub(prefix.len())
    }
}

fn build_chunks(path: &str, source: &str, blocks: &[Block], bounds: &ChunkBounds) -> Vec<Chunk> {
    let mut builder = ChunkBuilder {
        path,
        source,
        bounds,
        chunks: Vec::new(),
        ordinals: HashMap::new(),
        group: Vec::new(),
        group_len: 0,
        crumb: Vec::new(),
        heading: None,
    };

    for (index, block) in blocks.iter().enumerate() {
        if block.kind == BlockKind::Heading {
            // Chunks never cross heading boundaries.
            builder.flush(blocks);
            builder.crumb = block.breadcrumb.clone();
            builder.heading = block.nearest_heading.clone();
            continue;
        }
        if !block.kind.is_retrieval_bearing() {
            // INVENTED: a thematic break carries no retrieval text and does not
            // itself end a chunk; the plan speaks of "adjacent retrieval-bearing
            // blocks", so adjacency is measured among those blocks only.
            continue;
        }
        if block.rendered.trim().is_empty() {
            // A block that renders empty (e.g. an HTML block that was only a
            // comment) produces no chunk: "There are no empty per-section
            // chunks."
            continue;
        }

        let budget = builder.body_budget();
        if block.rendered.len() > budget {
            builder.flush(blocks);
            split_oversized(&mut builder, blocks, index);
            continue;
        }

        if builder.group.is_empty() {
            builder.group.push(index);
            builder.group_len = block.rendered.len();
            continue;
        }

        // "merge toward target, never exceeding normal_max": keep appending
        // while the group is still under target and the merged size stays
        // within normal_max. A single block larger than normal_max is left
        // whole - normal_max bounds merging, and only the hard bound splits.
        // The hard-bound body budget also caps merging; at the plan's defaults
        // normal_max (4,000) is far below it, so this only bites when a test
        // tightens `hard_max`.
        let merged = builder.group_len + 2 + block.rendered.len();
        if builder.group_len < bounds.target && merged <= bounds.normal_max.min(budget) {
            builder.group.push(index);
            builder.group_len = merged;
        } else {
            builder.flush(blocks);
            builder.group.push(index);
            builder.group_len = block.rendered.len();
        }
    }
    builder.flush(blocks);

    let mut chunks = builder.chunks;
    if chunks.is_empty() {
        chunks.push(stub_chunk(path, source, blocks));
    }
    chunks
}

/// A document producing no body chunks emits exactly one lexical-only stub row.
fn stub_chunk(path: &str, source: &str, blocks: &[Block]) -> Chunk {
    let headings: Vec<String> = blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Heading)
        .filter_map(|block| block.nearest_heading.clone())
        .filter(|text| !text.is_empty())
        .collect();
    Chunk {
        path: path.to_string(),
        blocks: Vec::new(),
        // "all document headings are searchable, the headings carried in source
        // order by the stub's breadcrumb column".
        breadcrumb: headings.clone(),
        nearest_heading: None,
        rendered_body: String::new(),
        byte_start: 0,
        byte_end: source.len(),
        line_start: 1,
        line_end: line_at(source, source.len().saturating_sub(1)),
        same_heading_ordinal: 0,
        // INVENTED: stubs are never embedded, so they carry no embedding
        // identity at all. The plan says only "Stubs are not embedded"; an
        // empty string makes that checkable instead of minting an identity for
        // a row that must never reach the embedder.
        embedding_identity: String::new(),
        is_stub: true,
        stub_headings: headings,
    }
}

// ---------------------------------------------------------------------------
// Oversized atomic blocks
// ---------------------------------------------------------------------------

/// Bounded synthetic context repeated by fragments of an oversized block.
/// Fence info string for code, header rows for tables, nothing otherwise.
fn synthetic_context(block: &Block, bounds: &ChunkBounds) -> String {
    let raw = block.raw.as_str();
    let context = match block.kind {
        BlockKind::CodeFenced => {
            // The opening fence line carries the info string.
            let end = raw.find('\n').map(|i| i + 1).unwrap_or(0);
            &raw[..end]
        }
        BlockKind::Table => {
            // Header row plus the alignment/delimiter row.
            let first = raw.find('\n').map(|i| i + 1).unwrap_or(raw.len());
            let second = raw[first..]
                .find('\n')
                .map(|i| first + i + 1)
                .unwrap_or(raw.len());
            &raw[..second]
        }
        _ => "",
    };
    if context.is_empty() {
        String::new()
    } else {
        bound_text(context, bounds.synthetic_max, CONTEXT_TRUNCATED)
    }
}

fn split_oversized(builder: &mut ChunkBuilder<'_>, blocks: &[Block], index: usize) {
    let block = &blocks[index];
    let synthetic = synthetic_context(block, builder.bounds);
    let heading_budget = builder.body_budget();
    let raw = block.raw.as_str();

    let mut position = 0usize;
    let mut first = true;
    while position < raw.len() {
        // INVENTED: the first fragment already contains the fence/table header
        // in its own source text, so only fragments after it repeat the
        // synthetic context. The plan says fragments "repeat" it, which is
        // vacuous for the fragment that has it natively.
        let prefix = if first { "" } else { synthetic.as_str() };
        let budget = heading_budget.saturating_sub(prefix.len());
        let remaining = &raw[position..];
        let take = if remaining.len() <= budget {
            remaining.len()
        } else {
            split_point(remaining, budget, block.kind)
        };
        debug_assert!(take > 0, "split must make progress");
        let fragment_raw = &raw[position..position + take];
        let byte_start = block.byte_start + position;
        let byte_end = byte_start + take;

        let rendered_fragment = if block.kind.is_literal_text() {
            fragment_raw.to_string()
        } else {
            strip_html_comments(fragment_raw)
        };
        if !rendered_fragment.trim().is_empty() {
            let mut rendered_body = String::with_capacity(prefix.len() + rendered_fragment.len());
            rendered_body.push_str(prefix);
            rendered_body.push_str(&rendered_fragment);
            let crumb = builder.crumb.clone();
            let ordinal = builder.next_ordinal(&crumb);
            let nearest = builder.bounded_heading();
            builder.chunks.push(Chunk {
                path: builder.path.to_string(),
                blocks: vec![index],
                breadcrumb: crumb,
                embedding_identity: embedding_identity(nearest.as_deref(), &rendered_body),
                nearest_heading: nearest,
                rendered_body,
                byte_start,
                byte_end,
                line_start: line_at(builder.source, byte_start),
                line_end: line_at(builder.source, byte_end.saturating_sub(1).max(byte_start)),
                same_heading_ordinal: ordinal,
                is_stub: false,
                stub_headings: Vec::new(),
            });
        }

        position += take;
        first = false;
    }
}

/// Last block-native boundary at or before `budget`, then the last newline,
/// then the last UTF-8 boundary. Always returns a non-zero length.
fn split_point(text: &str, budget: usize, kind: BlockKind) -> usize {
    let cap = floor_char_boundary(text, budget.min(text.len()));
    if cap == 0 {
        // Not even one character fits; take one character so progress is made.
        return next_char_boundary(text, 1);
    }
    let window = &text[..cap];

    let native = match kind {
        // Code splits at a newline: each source line is a native unit.
        BlockKind::CodeFenced | BlockKind::CodeIndented => last_line_break(window),
        // A table row is a line.
        BlockKind::Table => last_line_break(window),
        // A list splits only at a top-level item boundary.
        BlockKind::List => last_top_level_item(window),
        _ => None,
    };
    if let Some(offset) = native.filter(|offset| *offset > 0) {
        return offset;
    }
    // Fallback 1: the last newline before the bound.
    if let Some(offset) = last_line_break(window).filter(|offset| *offset > 0) {
        return offset;
    }
    // Fallback 2: the last UTF-8 boundary.
    cap
}

fn last_line_break(window: &str) -> Option<usize> {
    window.rfind('\n').map(|index| index + 1)
}

/// Byte offset of the start of the last line in `window` that begins a
/// top-level list item (`-`, `*`, `+`, `N.`, `N)` at column 0), excluding
/// offset 0.
fn last_top_level_item(window: &str) -> Option<usize> {
    let mut best = None;
    let mut line_start = 0usize;
    while line_start < window.len() {
        if line_start > 0 && is_top_level_item_line(&window[line_start..]) {
            best = Some(line_start);
        }
        match window[line_start..].find('\n') {
            Some(offset) => line_start += offset + 1,
            None => break,
        }
    }
    best
}

fn is_top_level_item_line(line: &str) -> bool {
    let line = line.split('\n').next().unwrap_or(line);
    let mut chars = line.chars();
    match chars.next() {
        Some('-') | Some('*') | Some('+') => matches!(chars.next(), Some(' ') | Some('\t')),
        Some(first) if first.is_ascii_digit() => {
            let mut seen_digit = true;
            for character in chars.by_ref() {
                if character.is_ascii_digit() {
                    continue;
                }
                if character == '.' || character == ')' {
                    seen_digit = true;
                    break;
                }
                return false;
            }
            seen_digit && matches!(chars.next(), Some(' ') | Some('\t'))
        }
        _ => false,
    }
}

fn next_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// blake3 hex helper used for block content identity.
pub fn hash_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
