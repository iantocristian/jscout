use std::path::Path;

use anyhow::Result;
use oxc_allocator::Allocator;
use oxc_parser::{Parser, ParserReturn};
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::SourceType;

pub fn source_type_for(path: &Path) -> SourceType {
    match SourceType::from_path(path) {
        // JSX in `.js` is common in Babel- and framework-owned sources. Oxc
        // 0.143 derives the standard (non-JSX) variant for `.js`, `.mjs`, and
        // `.cjs`, so opt every JavaScript source type into its additive JSX
        // grammar while preserving its module kind. TypeScript remains
        // extension-strict: only `.tsx` enables TSX.
        Ok(source_type) if source_type.is_javascript() => source_type.with_jsx(true),
        Ok(source_type) => source_type,
        Err(_) => SourceType::default(),
    }
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
            .map_or_else(|| "unknown parse error".into(), |d| d.to_string());
        // Callers already report the file path. Keep the parser diagnostic as
        // the outer error so Display does not collapse it to a path-only
        // anyhow context.
        return Err(anyhow::anyhow!("parser aborted: {first}"));
    }
    // Node store enabled: reference classification walks node ancestors.
    let semantic_ret = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&ret.program);
    Ok(f(&ret, &semantic_ret.semantic))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::{source_type_for, with_parsed};

    #[test]
    fn javascript_extensions_enable_jsx_but_typescript_remains_extension_strict() {
        for path in ["page.js", "page.jsx", "page.mjs", "page.cjs"] {
            let source_type = source_type_for(Path::new(path));
            assert!(source_type.is_javascript(), "{path}");
            assert!(source_type.is_jsx(), "{path}");
        }

        assert!(!source_type_for(Path::new("page.ts")).is_jsx());
        assert!(!source_type_for(Path::new("page.mts")).is_jsx());
        assert!(!source_type_for(Path::new("page.cts")).is_jsx());
        assert!(source_type_for(Path::new("page.tsx")).is_jsx());
    }

    #[test]
    fn parses_jsx_and_ordinary_javascript_in_js_files() -> Result<()> {
        let jsx_statements = with_parsed(
            "export default function Page() { return <main>Hello</main>; }",
            Path::new("page.js"),
            |ret, _| {
                assert!(ret.diagnostics.is_empty());
                assert!(ret.program.source_type.is_jsx());
                ret.program.body.len()
            },
        )?;
        assert_eq!(jsx_statements, 1);

        let comparison_statements = with_parsed(
            "export const ordered = left < middle && middle > right;",
            Path::new("comparison.js"),
            |ret, _| {
                assert!(ret.diagnostics.is_empty());
                ret.program.body.len()
            },
        )?;
        assert_eq!(comparison_statements, 1);
        Ok(())
    }

    #[test]
    fn fatal_parse_errors_surface_the_parser_diagnostic() {
        let error = with_parsed(
            "export default function Page() { return <main>",
            Path::new("broken.js"),
            |_, _| (),
        )
        .expect_err("unterminated JSX should abort parsing");

        let rendered = error.to_string();
        assert!(rendered.starts_with("parser aborted: "), "{rendered}");
        assert!(!rendered.ends_with("unknown parse error"), "{rendered}");
    }
}
