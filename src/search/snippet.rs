//! Bounded source previews, selected after ranking using the indexed FTS tokenizer.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

const MAX_LINES: usize = 8;
const MAX_BYTES: usize = 1024;
const ELLIPSIS: &str = "…";

pub(super) struct Snippet {
    pub text: String,
    pub line_offset: usize,
}

pub(super) fn select(
    conn: &Connection,
    chunk_id: i64,
    content: &str,
    query: &str,
    identifiers: &[String],
) -> Result<Snippet> {
    // Exact-tier hits should show their identifier, not a generic prose term.
    let query = if identifiers.is_empty() {
        super::exhaustive_fts_query(query)
    } else {
        super::exhaustive_fts_query(&identifiers.join(" "))
    };
    // One collision-free delimiter can mark both ends: query terms are single
    // FTS tokens, so highlighted regions cannot span source lines.
    // Different start/end characters also prevent a marker from overlapping
    // itself across an inserted delimiter and an adjacent source character.
    let mut marker = "\u{1e}jscout-snippet\u{1f}".to_string();
    while content.contains(&marker) {
        marker.insert(marker.len() - 1, '-');
    }
    let highlighted = if query.is_empty() {
        None
    } else {
        conn.prepare_cached(
            "SELECT highlight(chunks_fts, 0, ?3, ?3) FROM chunks_fts
             WHERE rowid=?1 AND chunks_fts MATCH ?2",
        )?
        .query_row(rusqlite::params![chunk_id, query, marker], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
    };
    Ok(excerpt(content, highlighted.as_deref(), &marker))
}

fn excerpt(content: &str, highlighted: Option<&str>, marker: &str) -> Snippet {
    let Some(highlighted) = highlighted else {
        // A path-only or vector-only match has no literal source witness.
        // Preserve the header fallback without scanning the rest of the chunk.
        let end = content
            .match_indices('\n')
            .nth(MAX_LINES - 1)
            .map_or(content.len(), |(offset, _)| offset);
        return Snippet {
            text: clip(content[..end].trim_end_matches(['\r', '\n']), 0).0,
            line_offset: 0,
        };
    };
    let mut starts = vec![0];
    starts.extend(
        content
            .match_indices('\n')
            .map(|(offset, _)| offset + 1)
            .filter(|offset| *offset < content.len()),
    );
    let mut matches = vec![Vec::new(); starts.len()];
    let mut offset = 0;
    for (index, part) in highlighted.split(marker).enumerate() {
        if index % 2 == 1 {
            let line = starts.partition_point(|start| *start <= offset) - 1;
            matches[line].push((offset, part.to_lowercase()));
        }
        // FTS stores NUL as one space; source byte and line offsets agree.
        offset += part.len();
    }

    let mut best = (0, 0);
    let mut start_line = 0;
    let mut distinct = HashSet::new();
    for start in 0..starts.len() {
        let window = &matches[start..(start + MAX_LINES).min(starts.len())];
        distinct.clear();
        distinct.extend(window.iter().flatten().map(|(_, token)| token));
        // Prefer one context line before the first match, then source order.
        let context = window
            .iter()
            .position(|line| !line.is_empty())
            .map_or(0, |first| MAX_LINES - first.abs_diff(1));
        if (distinct.len(), context) > best {
            best = (distinct.len(), context);
            start_line = start;
        }
    }
    let start = starts[start_line];
    let end = starts
        .get(start_line + MAX_LINES)
        .copied()
        .unwrap_or(content.len());
    let source = content[start..end].trim_end_matches(['\r', '\n']);
    let focus = matches[start_line..(start_line + MAX_LINES).min(starts.len())]
        .iter()
        .flatten()
        .next()
        .map_or(0, |(offset, _)| offset - start);
    let (text, clipped_start) = clip(source, focus);
    Snippet {
        text,
        line_offset: start_line
            + source[..clipped_start]
                .bytes()
                .filter(|b| *b == b'\n')
                .count(),
    }
}

fn clip(source: &str, focus: usize) -> (String, usize) {
    if source.len() <= MAX_BYTES {
        return (source.to_owned(), 0);
    }
    let capacity = MAX_BYTES - 2 * ELLIPSIS.len();
    let mut start = focus
        .saturating_sub(capacity / 4)
        .min(source.len() - capacity);
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + capacity).min(source.len());
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = String::new();
    if start > 0 {
        text.push_str(ELLIPSIS);
    }
    text.push_str(&source[start..end]);
    if end < source.len() {
        text.push_str(ELLIPSIS);
    }
    (text, start)
}

#[cfg(test)]
mod tests;
