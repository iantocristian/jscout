//! Definition coverage is a symbol property, not a retrieval-chunk boundary.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::{query::SymbolTarget, scout, store};

pub(super) fn render(
    root: &Path,
    conn: &Connection,
    target: &SymbolTarget,
    view: scout::SourceView,
    byte_limit: usize,
) -> Result<Option<scout::RenderedSource>> {
    let declaration = target.declaration.as_ref();
    if let Some(span) = declaration {
        let indexed_hash: String = conn.query_row(
            "SELECT hash FROM files WHERE id=?1",
            [target.file_id],
            |row| row.get(0),
        )?;
        let source = store::file_source_path(conn, root, target.file_id)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok());
        if let Some(source) = source
            && blake3::hash(source.as_bytes()).to_hex().as_str() == indexed_hash
        {
            return scout::render_source(
                Path::new(&target.file),
                &source,
                span.start as usize,
                span.end as usize,
                view,
                byte_limit,
            )
            .map(Some);
        }
    }

    // Do not splice chunks together: their spans can omit whitespace, comments
    // and delimiters. A stale/missing file may only offer an indexed fragment.
    let chunk: Option<(String, u32, u32)> = conn
        .query_row(
            "SELECT content, start, end FROM chunks
         WHERE file_id=?1
           AND ((?4 IS NOT NULL AND start <= ?4 AND end > ?4)
             OR (?4 IS NULL AND start_line <= ?2 AND end_line >= ?2))
         ORDER BY name=?3 DESC, (end-start), start LIMIT 1",
            rusqlite::params![
                target.file_id,
                target.line,
                target.name,
                declaration.map(|span| span.start)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((content, start, end)) = chunk else {
        return Ok(None);
    };
    let available = declaration.map_or(start..end, |span| span.start..span.end.min(end));
    let mut rendered = scout::render_source(
        Path::new(&target.file),
        &content,
        (available.start - start) as usize,
        (available.end - start) as usize,
        view,
        byte_limit,
    )?;
    rendered.partial = declaration.is_none_or(|span| *span != available);
    Ok(Some(rendered))
}
