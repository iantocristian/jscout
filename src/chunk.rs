use std::path::Path;

use oxc_ast::ast::*;
use oxc_parser::ParserReturn;
use oxc_span::{GetSpan, Span};
use serde::{Deserialize, Serialize};

/// Target/limit sizes in estimated tokens (bytes / 4).
const TARGET_TOKENS: usize = 1200;
const MAX_TOKENS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Function,
    Component,
    Class,
    ClassHeader,
    Method,
    Imports,
    Module, // merged top-level statements / misc
    RustText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub file: String,
    pub kind: ChunkKind,
    /// Primary symbol name, if the chunk is a named declaration.
    pub name: Option<String>,
    /// Enclosing names, outermost first (e.g. `["UserService"]` for a method).
    pub scope_chain: Vec<String>,
    /// All symbol names declared at the top level of this chunk.
    pub symbols: Vec<String>,
    pub start: u32,
    pub end: u32,
    pub start_line: u32,
    pub end_line: u32,
    /// blake3 hex of content.
    pub hash: String,
    pub content: String,
    /// Modules imported by the file (context for embedding, not per-chunk).
    pub file_imports: Vec<String>,
}

pub struct LineIndex {
    offsets: Vec<u32>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut offsets = vec![0u32];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                offsets.push(i as u32 + 1);
            }
        }
        Self { offsets }
    }
    pub fn line(&self, offset: u32) -> u32 {
        (self.offsets.partition_point(|&o| o <= offset)) as u32
    }
}

fn est_tokens(span: Span) -> usize {
    ((span.end - span.start) as usize) / 4
}

/// A pre-chunk unit: a top-level syntactic item with metadata.
struct Unit {
    span: Span,
    kind: ChunkKind,
    name: Option<String>,
    scope_chain: Vec<String>,
    symbols: Vec<String>,
    /// May not be merged with neighbors (already a split product).
    atomic: bool,
}

pub struct Chunker<'s> {
    source: &'s str,
    file: String,
    is_jsx: bool,
    lines: LineIndex,
    file_imports: Vec<String>,
}

impl<'s> Chunker<'s> {
    pub fn new(path_rel: &Path, source: &'s str, ret: &ParserReturn<'_>) -> Self {
        let mut file_imports: Vec<String> = ret
            .module_record
            .requested_modules
            .keys()
            .map(|s| s.to_string())
            .collect();
        file_imports.sort();
        Self {
            source,
            file: path_rel.to_string_lossy().into_owned(),
            is_jsx: ret.program.source_type.is_jsx(),
            lines: LineIndex::new(source),
            file_imports,
        }
    }

    pub fn chunk_program(&self, program: &Program<'_>, comments: &[Comment]) -> Vec<Chunk> {
        let mut units: Vec<Unit> = Vec::new();
        for stmt in &program.body {
            self.units_for_statement(stmt, &mut units);
        }
        // Extend each unit's span backward over an immediately-preceding JSDoc.
        for u in &mut units {
            u.span = self.with_leading_comment(u.span, comments);
        }
        self.merge_units(units)
    }

    /// Type-only statements return no units (erasure).
    fn units_for_statement(&self, stmt: &Statement<'_>, out: &mut Vec<Unit>) {
        match stmt {
            // ---- erased: type-only constructs ----
            Statement::TSInterfaceDeclaration(_)
            | Statement::TSTypeAliasDeclaration(_)
            | Statement::TSImportEqualsDeclaration(_) => {}
            Statement::TSModuleDeclaration(m) if m.declare => {}
            Statement::ImportDeclaration(imp) if imp.import_kind.is_type() => {}
            Statement::ExportNamedDeclaration(exp) if exp.export_kind.is_type() => {}

            // ---- imports get bucketed together by merge (kind Imports) ----
            Statement::ImportDeclaration(imp) => out.push(Unit {
                span: imp.span,
                kind: ChunkKind::Imports,
                name: None,
                scope_chain: vec![],
                symbols: vec![],
                atomic: false,
            }),

            Statement::FunctionDeclaration(f) => {
                self.units_for_function(f, stmt.span(), &[], out);
            }
            Statement::ClassDeclaration(c) => {
                self.units_for_class(c, stmt.span(), out);
            }
            Statement::VariableDeclaration(v) => {
                self.units_for_var(v, stmt.span(), out);
            }
            // `export <declaration>` — unwrap to the inner declaration.
            Statement::ExportDeclaration(exp) => match &exp.declaration {
                Declaration::FunctionDeclaration(f) => {
                    self.units_for_function(f, stmt.span(), &[], out);
                }
                Declaration::ClassDeclaration(c) => {
                    self.units_for_class(c, stmt.span(), out);
                }
                Declaration::VariableDeclaration(v) => {
                    self.units_for_var(v, stmt.span(), out);
                }
                // erased type-only exports
                Declaration::TSInterfaceDeclaration(_)
                | Declaration::TSTypeAliasDeclaration(_)
                | Declaration::TSImportEqualsDeclaration(_) => {}
                Declaration::TSModuleDeclaration(m) if m.declare => {}
                _ => out.push(self.misc_unit(stmt.span())),
            },
            Statement::ExportDefaultDeclaration(exp) => match &exp.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    self.units_for_function(f, stmt.span(), &[], out);
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    self.units_for_class(c, stmt.span(), out);
                }
                _ => out.push(self.misc_unit(stmt.span())),
            },
            _ => out.push(self.misc_unit(stmt.span())),
        }
    }

    fn misc_unit(&self, span: Span) -> Unit {
        Unit {
            span,
            kind: ChunkKind::Module,
            name: None,
            scope_chain: vec![],
            symbols: vec![],
            atomic: false,
        }
    }

    fn component_or_function(&self, name: &str) -> ChunkKind {
        if self.is_jsx && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            ChunkKind::Component
        } else {
            ChunkKind::Function
        }
    }

    fn units_for_function(
        &self,
        f: &Function<'_>,
        full_span: Span,
        scope: &[String],
        out: &mut Vec<Unit>,
    ) {
        if f.declare {
            return; // ambient `declare function` — type-only, erased
        }
        let name = f.id.as_ref().map(|id| id.name.to_string());
        let kind = name
            .as_deref()
            .map_or(ChunkKind::Function, |n| self.component_or_function(n));
        if est_tokens(full_span) <= MAX_TOKENS {
            out.push(Unit {
                span: full_span,
                kind,
                symbols: name.clone().into_iter().collect(),
                name,
                scope_chain: scope.to_vec(),
                atomic: false,
            });
            return;
        }
        // Oversized function: split body statements into atomic parts, each
        // carrying the function name in its scope chain.
        let mut inner_scope = scope.to_vec();
        if let Some(n) = &name {
            inner_scope.push(n.clone());
        }
        if let Some(body) = &f.body {
            if body.statements.is_empty() {
                out.push(Unit {
                    span: full_span,
                    kind,
                    symbols: name.clone().into_iter().collect(),
                    name,
                    scope_chain: scope.to_vec(),
                    atomic: true,
                });
                return;
            }
            // Header: from function start to first body statement.
            let first = body.statements[0].span().start;
            out.push(Unit {
                span: Span::new(full_span.start, first),
                kind,
                name: name.clone(),
                scope_chain: scope.to_vec(),
                symbols: name.into_iter().collect(),
                atomic: true,
            });
            for s in &body.statements {
                out.push(Unit {
                    span: s.span(),
                    kind: ChunkKind::Module,
                    name: None,
                    scope_chain: inner_scope.clone(),
                    symbols: vec![],
                    atomic: false,
                });
            }
        } else {
            out.push(Unit {
                span: full_span,
                kind,
                symbols: name.clone().into_iter().collect(),
                name,
                scope_chain: scope.to_vec(),
                atomic: true,
            });
        }
    }

    fn units_for_class(&self, c: &Class<'_>, full_span: Span, out: &mut Vec<Unit>) {
        let name = c.id.as_ref().map(|id| id.name.to_string());
        if est_tokens(full_span) <= MAX_TOKENS {
            out.push(Unit {
                span: full_span,
                kind: ChunkKind::Class,
                symbols: name.clone().into_iter().collect(),
                name,
                scope_chain: vec![],
                atomic: false,
            });
            return;
        }
        // Oversized class: header chunk + one unit per member.
        let class_name = name.clone().unwrap_or_else(|| "<anonymous class>".into());
        let body_start = c
            .body
            .body
            .first()
            .map_or(full_span.end, |m| m.span().start);
        out.push(Unit {
            span: Span::new(full_span.start, body_start),
            kind: ChunkKind::ClassHeader,
            name: name.clone(),
            scope_chain: vec![],
            symbols: name.into_iter().collect(),
            atomic: true,
        });
        for member in &c.body.body {
            let mname = match member {
                ClassElement::MethodDefinition(m) => prop_key_name(&m.key),
                ClassElement::PropertyDefinition(p) => prop_key_name(&p.key),
                _ => None,
            };
            out.push(Unit {
                span: member.span(),
                kind: ChunkKind::Method,
                name: mname.clone(),
                scope_chain: vec![class_name.clone()],
                symbols: mname.into_iter().collect(),
                atomic: false,
            });
        }
    }

    fn units_for_var(&self, v: &VariableDeclaration<'_>, full_span: Span, out: &mut Vec<Unit>) {
        // `const Foo = () => ...` — treat single-declarator function values as
        // named function/component units; everything else is module misc.
        let mut symbols: Vec<String> = Vec::new();
        for d in &v.declarations {
            if let BindingPattern::BindingIdentifier(id) = &d.id {
                symbols.push(id.name.to_string());
            }
        }
        if v.declarations.len() == 1 {
            let d = &v.declarations[0];
            if let BindingPattern::BindingIdentifier(id) = &d.id {
                let is_fn_value = matches!(
                    d.init,
                    Some(
                        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
                    )
                );
                if is_fn_value {
                    let name = id.name.to_string();
                    let kind = self.component_or_function(&name);
                    if est_tokens(full_span) <= MAX_TOKENS {
                        out.push(Unit {
                            span: full_span,
                            kind,
                            name: Some(name),
                            scope_chain: vec![],
                            symbols,
                            atomic: false,
                        });
                        return;
                    }
                    // Oversized arrow function: split its body if it's a block.
                    if let Some(Expression::ArrowFunctionExpression(arrow)) = &d.init
                        && let ArrowFunctionBody::FunctionBody(body) = &arrow.body
                        && let Some(first) = body.statements.first().map(|s| s.span().start)
                    {
                        out.push(Unit {
                            span: Span::new(full_span.start, first),
                            kind,
                            name: Some(name.clone()),
                            scope_chain: vec![],
                            symbols,
                            atomic: true,
                        });
                        for s in &body.statements {
                            out.push(Unit {
                                span: s.span(),
                                kind: ChunkKind::Module,
                                name: None,
                                scope_chain: vec![name.clone()],
                                symbols: vec![],
                                atomic: false,
                            });
                        }
                        return;
                    }
                }
            }
        }
        out.push(Unit {
            span: full_span,
            kind: ChunkKind::Module,
            name: symbols.first().cloned(),
            scope_chain: vec![],
            symbols,
            atomic: false,
        });
    }

    fn with_leading_comment(&self, span: Span, comments: &[Comment]) -> Span {
        // Attach a JSDoc/leading comment whose end is separated from the node
        // start only by whitespace.
        let mut start = span.start;
        for c in comments.iter().rev() {
            if c.span.end <= start {
                let gap = &self.source[c.span.end as usize..start as usize];
                if gap.chars().all(char::is_whitespace) && gap.matches('\n').count() <= 1 {
                    start = c.span.start;
                }
                break;
            }
        }
        Span::new(start, span.end)
    }

    fn merge_units(&self, units: Vec<Unit>) -> Vec<Chunk> {
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut acc: Option<Unit> = None;

        let flush = |acc: &mut Option<Unit>, chunks: &mut Vec<Chunk>, this: &Self| {
            if let Some(u) = acc.take() {
                chunks.extend(this.unit_to_chunks(u));
            }
        };

        for u in units {
            match &mut acc {
                None => acc = Some(u),
                Some(a) => {
                    let same_scope = a.scope_chain == u.scope_chain;
                    let both_imports = a.kind == ChunkKind::Imports && u.kind == ChunkKind::Imports;
                    // Named declarations (functions/components/classes) always
                    // stand alone — symbol-aligned chunks beat packing density
                    // for retrieval. Only anonymous module statements and
                    // adjacent imports merge.
                    let both_anonymous = a.kind == ChunkKind::Module
                        && u.kind == ChunkKind::Module
                        && a.name.is_none()
                        && u.name.is_none();
                    let mergeable = !a.atomic
                        && !u.atomic
                        && same_scope
                        && (both_imports
                            || (both_anonymous
                                && est_tokens(a.span) + est_tokens(u.span) <= TARGET_TOKENS));
                    if mergeable {
                        a.span =
                            Span::new(a.span.start.min(u.span.start), a.span.end.max(u.span.end));
                        if a.kind != u.kind {
                            a.kind = if both_imports {
                                ChunkKind::Imports
                            } else {
                                ChunkKind::Module
                            };
                        }
                        if a.name.is_none() {
                            a.name = u.name;
                        }
                        a.symbols.extend(u.symbols);
                    } else {
                        let prev = std::mem::replace(a, u);
                        chunks.extend(self.unit_to_chunks(prev));
                    }
                }
            }
        }
        flush(&mut acc, &mut chunks, self);
        chunks
    }

    /// Convert one unit to chunk(s); oversized leaves fall back to line splits.
    fn unit_to_chunks(&self, u: Unit) -> Vec<Chunk> {
        let mut out = Vec::new();
        let spans: Vec<Span> = if est_tokens(u.span) > MAX_TOKENS {
            self.split_by_lines(u.span)
        } else {
            vec![u.span]
        };
        let n = spans.len();
        for (i, span) in spans.into_iter().enumerate() {
            let content = &self.source[span.start as usize..span.end as usize];
            let name = if n > 1 {
                u.name.as_ref().map(|s| format!("{s}#part{}", i + 1))
            } else {
                u.name.clone()
            };
            out.push(Chunk {
                file: self.file.clone(),
                kind: u.kind,
                name,
                scope_chain: u.scope_chain.clone(),
                symbols: if i == 0 { u.symbols.clone() } else { vec![] },
                start: span.start,
                end: span.end,
                start_line: self.lines.line(span.start),
                end_line: self.lines.line(span.end.saturating_sub(1).max(span.start)),
                hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
                content: content.to_string(),
                file_imports: self.file_imports.clone(),
            });
        }
        out
    }

    fn split_by_lines(&self, span: Span) -> Vec<Span> {
        let budget_bytes = (TARGET_TOKENS * 4) as u32;
        let mut spans = Vec::new();
        let mut start = span.start;
        while start < span.end {
            let mut end = (start + budget_bytes).min(span.end);
            if end < span.end {
                // The byte budget can land inside a multibyte code point. Make
                // the provisional end sliceable before looking for a newline.
                while end > start && !self.source.is_char_boundary(end as usize) {
                    end -= 1;
                }
                // back up to a line boundary
                let slice = &self.source[start as usize..end as usize];
                if let Some(pos) = slice.rfind('\n') {
                    let cand = start + pos as u32 + 1;
                    if cand > start {
                        end = cand;
                    }
                }
                // don't split a UTF-8 char
                while !self.source.is_char_boundary(end as usize) {
                    end -= 1;
                }
            }
            spans.push(Span::new(start, end));
            start = end;
        }
        spans
    }
}

fn prop_key_name(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::PrivateIdentifier(id) => Some(format!("#{}", id.name)),
        PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::path::Path;

    fn chunks_of(name: &str, source: &str) -> Vec<Chunk> {
        crate::parse::with_parsed(source, Path::new(name), |ret, _| {
            Chunker::new(Path::new(name), source, ret)
                .chunk_program(&ret.program, &ret.program.comments)
        })
        .unwrap()
    }

    #[test]
    fn erases_type_only_constructs() {
        let src = r"
interface User { id: string }
type Alias = User | null;
declare function ambient(): void;
import type { Foo } from './foo';
export type { Alias };
export function getUser(id: string): User { return { id }; }
";
        let chunks = chunks_of("a.ts", src);
        let all: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(!all.contains("interface User"));
        assert!(!all.contains("type Alias"));
        assert!(!all.contains("declare function"));
        assert!(!all.contains("import type"));
        assert!(all.contains("export function getUser"));
        let names: Vec<_> = chunks.iter().filter_map(|c| c.name.as_deref()).collect();
        assert!(names.contains(&"getUser"));
    }

    #[test]
    fn unwraps_exports_and_names_arrow_components() {
        let src = r#"
import React from 'react';
export const UserCard = ({ id }) => { return <div>{id}</div>; };
export default function App() { return <UserCard id="1" />; }
"#;
        let chunks = chunks_of("a.tsx", src);
        let comps: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Component)
            .filter_map(|c| c.name.as_deref())
            .collect();
        assert!(comps.contains(&"UserCard"), "components: {comps:?}");
        assert!(comps.contains(&"App"));
    }

    #[test]
    fn splits_oversized_class_into_methods() {
        let filler = "    x = 1 + 1; // padding line to inflate method body\n".repeat(60);
        let mut src = String::from("export class Big {\n");
        for i in 0..8 {
            let _ = write!(src, "  method{i}() {{\n{filler}  }}\n");
        }
        src.push_str("}\n");
        let chunks = chunks_of("big.ts", &src);
        assert!(chunks.iter().any(|c| c.kind == ChunkKind::ClassHeader));
        let methods: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Method)
            .collect();
        assert!(!methods.is_empty());
        assert!(
            methods
                .iter()
                .all(|c| c.scope_chain == vec!["Big".to_string()])
        );
    }

    #[test]
    fn line_fallback_never_splits_multibyte_characters() {
        let mut src = String::from("export const text = `");
        while src.len() < 4_799 {
            src.push('a');
        }
        src.push('—');
        while src.len() < 9_598 {
            src.push('b');
        }
        src.push('ل');
        while src.len() < 11_000 {
            src.push('c');
        }
        src.push_str("`;\n");

        let chunks = chunks_of("unicode.ts", &src);
        assert!(chunks.len() >= 3);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<String>(),
            src.strip_suffix('\n').unwrap()
        );
        assert!(chunks.iter().all(|chunk| {
            src.is_char_boundary(chunk.start as usize) && src.is_char_boundary(chunk.end as usize)
        }));
    }

    #[test]
    fn jsdoc_attaches_to_declaration() {
        let src =
            "/** Fetches a user by id. */\nexport function getUser(id) { return db.get(id); }\n";
        let chunks = chunks_of("a.js", src);
        let c = chunks
            .iter()
            .find(|c| c.name.as_deref() == Some("getUser"))
            .unwrap();
        assert!(
            c.content.starts_with("/** Fetches a user"),
            "content: {}",
            &c.content[..40.min(c.content.len())]
        );
    }
}
