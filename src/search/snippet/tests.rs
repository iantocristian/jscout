use super::*;

fn select_source(content: &str, query: &str, identifiers: &[String]) -> Result<Snippet> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE chunks_fts USING fts5(
            content, name, symbols, path, tokenize=\"unicode61 tokenchars '_$'\"
        );",
    )?;
    conn.execute(
        "INSERT INTO chunks_fts(rowid, content, path) VALUES(1, ?1, 'pathOnlyNeedle.ts')",
        [content.replace('\0', " ")],
    )?;
    select(&conn, 1, content, query, identifiers)
}

#[test]
fn moves_to_a_late_match_with_context_and_an_exact_line_offset() -> Result<()> {
    let source = "function run() {\n  // setup\n  const first = 1;\n  const next = first;\n  prepare(next);\n  targetNeedle(next);\n  finish();\n}\n";
    let snippet = select_source(source, "targetNeedle", &[])?;
    assert_eq!(
        snippet.text,
        "  prepare(next);\n  targetNeedle(next);\n  finish();\n}"
    );
    assert_eq!(snippet.line_offset, 4);
    assert!(source.contains(&snippet.text));
    Ok(())
}

#[test]
fn exact_tier_identifiers_outrank_incidental_query_words() -> Result<()> {
    let source = "// return a value from code\n// return the value again\n// value\n// code\nfunction wrap() {\n  prepare();\n  targetNeedle();\n  finish();\n}\n";
    let snippet = select_source(
        source,
        "return a value from code with targetNeedle",
        &["targetNeedle".into()],
    )?;
    assert!(snippet.text.contains("targetNeedle()"));
    assert!(!snippet.text.contains("return a value"));
    assert_eq!(snippet.line_offset, 5);
    Ok(())
}

#[test]
fn window_prefers_distinct_matches_over_one_repeated_term() -> Result<()> {
    let source =
        "alpha alpha alpha\nalpha\nalpha\nalpha\n\n\nprepare\nalpha beta gamma\nfinish\nend";
    let snippet = select_source(source, "alpha beta gamma", &[])?;
    assert_eq!(snippet.text, "prepare\nalpha beta gamma\nfinish\nend");
    assert_eq!(snippet.line_offset, 6);
    Ok(())
}

#[test]
fn ties_choose_the_first_matching_window_deterministically() -> Result<()> {
    let source = "before\nneedle\nafter\nend\nspacer\nbefore2\nneedle\nafter2\nend2";
    for _ in 0..3 {
        let snippet = select_source(source, "needle", &[])?;
        assert_eq!(snippet.text, "before\nneedle\nafter\nend");
        assert_eq!(snippet.line_offset, 0);
    }
    Ok(())
}

#[test]
fn vector_only_path_only_and_empty_queries_keep_the_header_fallback() -> Result<()> {
    let source = "function useful() {\n  first();\n  second();\n  third();\n  fourth();\n}\n";
    for query in ["semantic paraphrase", "pathOnlyNeedle", "", "\" OR *"] {
        let snippet = select_source(source, query, &[])?;
        assert_eq!(
            snippet.text,
            "function useful() {\n  first();\n  second();\n  third();"
        );
        assert_eq!(snippet.line_offset, 0);
    }
    assert_eq!(select_source("", "needle", &[])?.text, "");
    assert_eq!(select_source("one line", "line", &[])?.text, "one line");
    Ok(())
}

#[test]
fn matches_use_fts_case_diacritic_and_identifier_boundaries() -> Result<()> {
    let source = "// setup\n// setup\n// setup\n// setup\nnot_targetNeedle_suffix();\nCAFÉ();\n$target_value();\nfinish();\n}";
    assert!(select_source(source, "cafe", &[])?.text.contains("CAFÉ()"));
    assert!(
        select_source(source, "$target_value", &[])?
            .text
            .contains("$target_value()")
    );
    // Do not invent a lexical match inside a different underscore identifier.
    assert_eq!(select_source(source, "targetNeedle", &[])?.line_offset, 0);
    Ok(())
}

#[test]
fn source_sentinels_nul_and_crlf_preserve_the_original_excerpt_and_location() -> Result<()> {
    let source = "// \u{1e}jscout-snippet\u{1f}\r\n// \u{1e}jscout-snippet-\u{1f}\r\n// three\r\n// four\r\n// before\0after\r\ncallNeedle();\r\nfinish();\r\n}";
    let snippet = select_source(source, "callNeedle", &[])?;
    assert_eq!(snippet.line_offset, 4);
    assert_eq!(
        snippet.text,
        "// before\0after\r\ncallNeedle();\r\nfinish();\r\n}"
    );
    assert!(source.contains(&snippet.text));
    Ok(())
}

#[test]
fn long_lines_are_byte_bounded_around_the_match_without_splitting_utf8() -> Result<()> {
    let source = format!("{} targetNeedle {}", "é".repeat(500), "界".repeat(500));
    let snippet = select_source(&source, "targetNeedle", &[])?;
    assert!(snippet.text.len() <= MAX_BYTES);
    assert!(snippet.text.contains("targetNeedle"));
    assert!(snippet.text.starts_with(ELLIPSIS));
    assert!(snippet.text.ends_with(ELLIPSIS));
    assert!(source.contains(snippet.text.trim_matches('…')));
    assert_eq!(snippet.line_offset, 0);
    let fallback = select_source(&source, "unmatched", &[])?;
    assert!(fallback.text.len() <= MAX_BYTES);
    assert!(!fallback.text.starts_with(ELLIPSIS));
    Ok(())
}

#[test]
fn byte_clipping_updates_the_start_line_when_it_removes_context_lines() -> Result<()> {
    let source = format!(
        "header\n{}\n{} needle\nnext\nlast",
        "a".repeat(600),
        "b".repeat(600)
    );
    let snippet = select_source(&source, "needle", &[])?;
    assert!(snippet.text.len() <= MAX_BYTES);
    assert!(snippet.text.contains("needle"));
    assert_eq!(snippet.line_offset, 2);
    Ok(())
}

#[test]
fn bounded_excerpts_keep_the_match_and_source_location_at_every_line_position() -> Result<()> {
    for target in 0..12 {
        for width in [0, 20, 600] {
            let source = (0..12)
                .map(|line| {
                    if line == target {
                        format!(
                            "{} targetNeedle {}",
                            "é ".repeat(width),
                            "界 ".repeat(width)
                        )
                    } else {
                        format!("// context {line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\r\n");
            let snippet = select_source(&source, "targetNeedle", &[])?;
            assert!(snippet.text.len() <= MAX_BYTES);
            assert!(snippet.text.lines().count() <= MAX_LINES);
            assert!(snippet.text.contains("targetNeedle"));
            let offset = source.find(snippet.text.trim_matches('…')).unwrap();
            assert_eq!(
                source[..offset]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count(),
                snippet.line_offset,
            );
        }
    }
    Ok(())
}
