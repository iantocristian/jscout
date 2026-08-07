use std::path::Path;

use anyhow::{Context, Result};
use oxc_allocator::Allocator;
use oxc_parser::{Parser, ParserReturn};
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::SourceType;

pub fn source_type_for(path: &Path) -> SourceType {
    SourceType::from_path(path).unwrap_or_default()
}

/// Parse and analyze one file, handing borrowed views to a closure.
/// The Program lives in the allocator on this stack frame while Semantic
/// borrows it, so all per-file analysis happens inside `f`; extract owned
/// data as the return value.
pub fn with_parsed<T>(
    source: &str,
    path: &Path,
    f: impl FnOnce(&ParserReturn<'_>, &Semantic<'_>) -> T,
) -> Result<T> {
    let allocator = Allocator::default();
    let source_type = source_type_for(path);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if ret.panicked {
        let first = ret
            .diagnostics
            .first()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "unknown parse error".into());
        return Err(anyhow::anyhow!("parser panicked: {first}"))
            .with_context(|| path.display().to_string());
    }
    // Node store enabled: reference classification walks node ancestors.
    let semantic_ret = SemanticBuilder::new().with_build_nodes(true).build(&ret.program);
    Ok(f(&ret, &semantic_ret.semantic))
}
