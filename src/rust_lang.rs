use std::ops::Range;
use std::path::Path;

use anyhow::{Result, ensure};
use ra_ap_syntax::{Edition, SourceFile};

use crate::chunk::{Chunk, ChunkKind, LineIndex};

const TARGET_BYTES: usize = 4_800;
const HARD_MAX_BYTES: usize = 8_000;

pub struct RustExtraction {
    pub chunks: Vec<Chunk>,
    pub parse_error_count: usize,
}

pub fn extract(path: &Path, source: &str) -> Result<RustExtraction> {
    let parsed = SourceFile::parse(source, Edition::CURRENT);
    let syntax = parsed.syntax_node();
    let source_len = source.len();
    let syntax_end = u32::from(syntax.text_range().end()) as usize;
    ensure!(
        syntax_end == source_len,
        "Rust parser lossless span ended at {syntax_end}, expected {source_len}"
    );

    let mut units = Vec::new();
    let mut cursor = 0;
    for child in syntax.children() {
        let range = child.text_range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        ensure!(
            start >= cursor && end >= start && end <= source_len,
            "Rust parser emitted an invalid or overlapping top-level range"
        );
        if end > cursor {
            // Include the lossless tokens before the node so comments,
            // whitespace, shebangs, and malformed residual text cannot vanish.
            units.push(cursor..end);
            cursor = end;
        }
    }
    if cursor < source_len {
        units.push(cursor..source_len);
    }
    if units.is_empty() && !source.is_empty() {
        units.push(0..source_len);
    }

    let ranges = coalesce_and_split(source, units);
    validate_partition(source, &ranges)?;
    let lines = LineIndex::new(source);
    let file = path.to_string_lossy().into_owned();
    let chunks = ranges
        .into_iter()
        .map(|range| {
            let content = &source[range.clone()];
            let start = range.start as u32;
            let end = range.end as u32;
            Chunk {
                file: file.clone(),
                kind: ChunkKind::RustText,
                name: None,
                scope_chain: Vec::new(),
                symbols: Vec::new(),
                start,
                end,
                start_line: lines.line(start),
                end_line: lines.line(end.saturating_sub(1).max(start)),
                hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
                content: content.to_string(),
                file_imports: Vec::new(),
            }
        })
        .collect();

    Ok(RustExtraction {
        chunks,
        parse_error_count: parsed.errors().len(),
    })
}

fn coalesce_and_split(source: &str, units: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut pending: Option<Range<usize>> = None;
    for unit in units {
        if unit.is_empty() {
            continue;
        }
        if unit.len() > HARD_MAX_BYTES {
            if let Some(range) = pending.take() {
                chunks.push(range);
            }
            chunks.extend(split_oversized(source, unit));
            continue;
        }
        match &mut pending {
            Some(range) if unit.end - range.start <= TARGET_BYTES => range.end = unit.end,
            Some(range) => {
                let previous = std::mem::replace(range, unit);
                chunks.push(previous);
            }
            None => pending = Some(unit),
        }
    }
    if let Some(range) = pending {
        chunks.push(range);
    }
    chunks
}

fn split_oversized(source: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = range.start;
    while range.end - start > HARD_MAX_BYTES {
        let hard_end = start + HARD_MAX_BYTES;
        let safe_end = if source.as_bytes()[hard_end - 1] == b'\r'
            && source.as_bytes().get(hard_end) == Some(&b'\n')
        {
            hard_end - 1
        } else {
            hard_end
        };
        let newline = source.as_bytes()[start..safe_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|position| start + position + 1)
            .filter(|boundary| *boundary > start);
        let mut end = newline.unwrap_or(safe_end);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        debug_assert!(end > start);
        chunks.push(start..end);
        start = end;
    }
    if start < range.end {
        chunks.push(start..range.end);
    }
    chunks
}

fn validate_partition(source: &str, ranges: &[Range<usize>]) -> Result<()> {
    if source.is_empty() {
        ensure!(ranges.is_empty(), "empty Rust source emitted chunks");
        return Ok(());
    }
    let mut cursor = 0;
    for range in ranges {
        ensure!(
            range.start == cursor,
            "Rust chunks are not a gap-free source partition"
        );
        ensure!(
            range.end > range.start
                && range.end <= source.len()
                && source.is_char_boundary(range.start)
                && source.is_char_boundary(range.end),
            "Rust chunk range is empty, out of bounds, or not UTF-8 aligned"
        );
        ensure!(
            range.len() <= HARD_MAX_BYTES,
            "Rust chunk exceeds the hard byte bound"
        );
        ensure!(
            !(range.end < source.len()
                && source.as_bytes()[range.end - 1] == b'\r'
                && source.as_bytes()[range.end] == b'\n'),
            "Rust chunk boundary split CRLF"
        );
        cursor = range.end;
    }
    ensure!(
        cursor == source.len(),
        "Rust chunks do not cover the complete source"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn assert_contract(source: &str, extraction: &RustExtraction) {
        let mut cursor = 0_usize;
        let mut rebuilt = String::new();
        for chunk in &extraction.chunks {
            let start = chunk.start as usize;
            let end = chunk.end as usize;
            assert_eq!(start, cursor);
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
            assert_eq!(chunk.content.as_bytes(), &source.as_bytes()[start..end]);
            assert!(end - start <= HARD_MAX_BYTES);
            assert!(
                !(end < source.len()
                    && source.as_bytes()[end - 1] == b'\r'
                    && source.as_bytes()[end] == b'\n')
            );
            assert_eq!(chunk.kind, ChunkKind::RustText);
            assert!(chunk.name.is_none());
            assert!(chunk.scope_chain.is_empty());
            assert!(chunk.symbols.is_empty());
            assert!(chunk.file_imports.is_empty());
            rebuilt.push_str(&chunk.content);
            cursor = end;
        }
        assert_eq!(cursor, source.len());
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn chunks_are_exact_for_rust_lexical_edge_cases() -> Result<()> {
        let mut source = String::from(
            "#![allow(dead_code)]\r\n/* outer /* nested */ done */\r\n\
             pub fn lifetime<'a>(value: &'a str) -> &'a str { value }\r\n\
             const RAW: &str = r###\"quote \" and ## lookalike — λ\"###;\r\n\
             const BYTE: &[u8] = br#\"bytes \\xFF\"#;\r\n",
        );
        for index in 0..700 {
            let _ = writeln!(
                source,
                "pub const ITEM_{index}: &str = \"padding-{index}-界\";\r"
            );
        }
        let extraction = extract(Path::new("src/lib.rs"), &source)?;
        assert!(extraction.chunks.len() > 3);
        assert_eq!(extraction.parse_error_count, 0);
        assert_contract(&source, &extraction);
        Ok(())
    }

    #[test]
    fn malformed_mid_edit_remains_lossless_and_counted() -> Result<()> {
        let source = "pub fn before() {}\nfn broken() { let value = ;\npub fn after() {}\n";
        let extraction = extract(Path::new("broken.rs"), source)?;
        assert!(extraction.parse_error_count > 0);
        assert_contract(source, &extraction);
        assert!(
            extraction
                .chunks
                .iter()
                .any(|chunk| chunk.content.contains("pub fn after"))
        );
        Ok(())
    }

    #[test]
    fn empty_source_emits_no_chunks() -> Result<()> {
        let extraction = extract(Path::new("empty.rs"), "")?;
        assert!(extraction.chunks.is_empty());
        assert_eq!(extraction.parse_error_count, 0);
        Ok(())
    }

    #[test]
    fn hard_bound_never_splits_crlf_or_utf8() -> Result<()> {
        let mut source = " ".repeat(HARD_MAX_BYTES - 1);
        source.push_str("\r\n");
        source.push_str(&" ".repeat(HARD_MAX_BYTES - 2));
        source.push('界');
        source.push_str(&" ".repeat(HARD_MAX_BYTES));

        let extraction = extract(Path::new("boundaries.rs"), &source)?;

        assert!(extraction.chunks.len() >= 3);
        assert_contract(&source, &extraction);
        assert_eq!(extraction.chunks[0].end as usize, HARD_MAX_BYTES - 1);
        Ok(())
    }
}
