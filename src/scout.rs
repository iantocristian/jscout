use std::path::Path;

use anyhow::{Result, bail};
use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};
use oxc_syntax::scope::ScopeFlags;
use serde::Serialize;

use crate::{chunk::LineIndex, parse};

pub const DEFAULT_SOURCE_BYTE_LIMIT: usize = 12_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceView {
    Full,
    Elided,
}

impl SourceView {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "full" => Ok(Self::Full),
            "elided" => Ok(Self::Elided),
            _ => bail!("source view must be one of: full, elided"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Elided => "elided",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ElidedRange {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug)]
pub struct RenderedSource {
    pub representation: &'static str,
    pub text: String,
    pub original_bytes: usize,
    pub rendered_bytes: usize,
    pub elisions: Vec<ElidedRange>,
    pub budget_truncated: bool,
}

pub fn render_source(
    path: &Path,
    full_source: &str,
    start: usize,
    end: usize,
    view: SourceView,
    byte_limit: usize,
) -> Result<RenderedSource> {
    if byte_limit < 64 {
        bail!("source byte limit must be at least 64 bytes");
    }
    if start > end
        || end > full_source.len()
        || !full_source.is_char_boundary(start)
        || !full_source.is_char_boundary(end)
    {
        bail!("source span {start}..{end} is outside the indexed file");
    }
    let original = &full_source[start..end];
    let (rendered, elisions) = match view {
        SourceView::Full => (original.to_string(), Vec::new()),
        SourceView::Elided => render_elided(path, full_source, start, end)
            .unwrap_or_else(|_| (original.to_string(), Vec::new())),
    };
    let (text, budget_truncated) = fit_text_budget(rendered, byte_limit);
    let rendered_bytes = text.len();
    Ok(RenderedSource {
        representation: view.as_str(),
        text,
        original_bytes: original.len(),
        rendered_bytes,
        elisions,
        budget_truncated,
    })
}

fn render_elided(
    path: &Path,
    full_source: &str,
    start: usize,
    end: usize,
) -> Result<(String, Vec<ElidedRange>)> {
    let line_index = LineIndex::new(full_source);
    let line_count = full_source.lines().count() + usize::from(full_source.ends_with('\n'));
    let mut visitor = ElisionVisitor {
        line_index: &line_index,
        keep: vec![false; line_count.max(1)],
    };
    let clean_parse = parse::with_parsed(full_source, path, |ret, _| {
        if !ret.diagnostics.is_empty() {
            return false;
        }
        visitor.visit_program(&ret.program);
        true
    })?;
    if !clean_parse {
        return Ok((full_source[start..end].to_string(), Vec::new()));
    }

    let first_global_line = line_index.line(start as u32).saturating_sub(1) as usize;
    let selected = &full_source[start..end];
    let lines: Vec<&str> = selected.split_inclusive('\n').collect();
    if lines.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let mut selected_keep = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let structural = trimmed.starts_with('}')
            || trimmed == "{"
            || trimmed.starts_with("else")
            || trimmed.starts_with("catch")
            || trimmed.starts_with("finally");
        selected_keep.push(
            visitor
                .keep
                .get(first_global_line + index)
                .copied()
                .unwrap_or(false)
                || structural,
        );
    }
    if let Some(first) = selected_keep.iter_mut().position(|_| true) {
        selected_keep[first] = true;
    }
    selected_keep[0] = true;
    selected_keep[lines.len() - 1] = true;

    let mut output = String::new();
    let mut elisions = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if selected_keep[index] {
            output.push_str(lines[index]);
            index += 1;
            continue;
        }
        let range_start = index;
        while index < lines.len() && !selected_keep[index] {
            index += 1;
        }
        let original: String = lines[range_start..index].concat();
        let indent: String = lines[range_start]
            .chars()
            .take_while(|character| character.is_whitespace() && *character != '\n')
            .collect();
        let start_line = first_global_line as u32 + range_start as u32 + 1;
        let end_line = first_global_line as u32 + index as u32;
        let marker =
            format!("{indent}/* … jscout elided source lines {start_line}-{end_line} … */\n");
        if marker.len() < original.len() {
            output.push_str(&marker);
            elisions.push(ElidedRange {
                start_line,
                end_line,
            });
        } else {
            output.push_str(&original);
        }
    }
    Ok((output, elisions))
}

fn fit_text_budget(mut text: String, byte_limit: usize) -> (String, bool) {
    if text.len() <= byte_limit {
        return (text, false);
    }
    const MARKER: &str = "\n/* … jscout source response truncated to byte budget … */";
    let mut cut = byte_limit.saturating_sub(MARKER.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(MARKER);
    (text, true)
}

struct ElisionVisitor<'a> {
    line_index: &'a LineIndex,
    keep: Vec<bool>,
}

impl ElisionVisitor<'_> {
    fn keep_span(&mut self, span: Span) {
        if span.end <= span.start {
            return;
        }
        let start = self.line_index.line(span.start).saturating_sub(1) as usize;
        let end = self
            .line_index
            .line(span.end.saturating_sub(1))
            .saturating_sub(1) as usize;
        for line in start..=end.min(self.keep.len().saturating_sub(1)) {
            self.keep[line] = true;
        }
    }

    fn keep_header(&mut self, start: u32, body_start: u32) {
        self.keep_span(Span::new(start, body_start.max(start + 1)));
    }

    fn keep_line(&mut self, offset: u32) {
        let line = self.line_index.line(offset).saturating_sub(1) as usize;
        if let Some(keep) = self.keep.get_mut(line) {
            *keep = true;
        }
    }
}

impl<'a> Visit<'a> for ElisionVisitor<'_> {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        self.keep_span(declaration.span);
        oxc_ast_visit::walk::walk_import_declaration(self, declaration);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if let Some(body) = &function.body {
            self.keep_header(function.span.start, body.span.start + 1);
        } else {
            self.keep_span(function.span);
        }
        oxc_ast_visit::walk::walk_function(self, function, flags);
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        match &arrow.body {
            ArrowFunctionBody::FunctionBody(body) => {
                self.keep_header(arrow.span.start, body.span.start + 1);
            }
            _ => self.keep_span(arrow.span),
        }
        oxc_ast_visit::walk::walk_arrow_function_expression(self, arrow);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        self.keep_header(class.span.start, class.body.span.start + 1);
        oxc_ast_visit::walk::walk_class(self, class);
    }

    fn visit_method_definition(&mut self, method: &MethodDefinition<'a>) {
        if let Some(body) = &method.value.body {
            self.keep_header(method.span.start, body.span.start + 1);
        }
        oxc_ast_visit::walk::walk_method_definition(self, method);
    }

    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        self.keep_header(statement.span.start, statement.consequent.span().start);
        oxc_ast_visit::walk::walk_if_statement(self, statement);
    }

    fn visit_for_statement(&mut self, statement: &ForStatement<'a>) {
        self.keep_header(statement.span.start, statement.body.span().start);
        oxc_ast_visit::walk::walk_for_statement(self, statement);
    }

    fn visit_for_in_statement(&mut self, statement: &ForInStatement<'a>) {
        self.keep_header(statement.span.start, statement.body.span().start);
        oxc_ast_visit::walk::walk_for_in_statement(self, statement);
    }

    fn visit_for_of_statement(&mut self, statement: &ForOfStatement<'a>) {
        self.keep_header(statement.span.start, statement.body.span().start);
        oxc_ast_visit::walk::walk_for_of_statement(self, statement);
    }

    fn visit_while_statement(&mut self, statement: &WhileStatement<'a>) {
        self.keep_header(statement.span.start, statement.body.span().start);
        oxc_ast_visit::walk::walk_while_statement(self, statement);
    }

    fn visit_do_while_statement(&mut self, statement: &DoWhileStatement<'a>) {
        self.keep_line(statement.span.start);
        self.keep_span(statement.test.span());
        oxc_ast_visit::walk::walk_do_while_statement(self, statement);
    }

    fn visit_switch_statement(&mut self, statement: &SwitchStatement<'a>) {
        self.keep_header(statement.span.start, statement.discriminant.span().end);
        oxc_ast_visit::walk::walk_switch_statement(self, statement);
    }

    fn visit_switch_case(&mut self, case: &SwitchCase<'a>) {
        self.keep_line(case.span.start);
        oxc_ast_visit::walk::walk_switch_case(self, case);
    }

    fn visit_try_statement(&mut self, statement: &TryStatement<'a>) {
        self.keep_line(statement.span.start);
        if let Some(handler) = &statement.handler {
            self.keep_header(handler.span.start, handler.body.span.start + 1);
        }
        if let Some(finalizer) = &statement.finalizer {
            self.keep_line(finalizer.span.start);
        }
        oxc_ast_visit::walk::walk_try_statement(self, statement);
    }

    fn visit_call_expression(&mut self, expression: &CallExpression<'a>) {
        self.keep_span(expression.span);
        oxc_ast_visit::walk::walk_call_expression(self, expression);
    }

    fn visit_new_expression(&mut self, expression: &NewExpression<'a>) {
        self.keep_span(expression.span);
        oxc_ast_visit::walk::walk_new_expression(self, expression);
    }

    fn visit_assignment_expression(&mut self, expression: &AssignmentExpression<'a>) {
        self.keep_span(expression.span);
        oxc_ast_visit::walk::walk_assignment_expression(self, expression);
    }

    fn visit_update_expression(&mut self, expression: &UpdateExpression<'a>) {
        self.keep_span(expression.span);
        oxc_ast_visit::walk::walk_update_expression(self, expression);
    }

    fn visit_return_statement(&mut self, statement: &ReturnStatement<'a>) {
        self.keep_span(statement.span);
        oxc_ast_visit::walk::walk_return_statement(self, statement);
    }

    fn visit_throw_statement(&mut self, statement: &ThrowStatement<'a>) {
        self.keep_span(statement.span);
        oxc_ast_visit::walk::walk_throw_statement(self, statement);
    }

    fn visit_break_statement(&mut self, statement: &BreakStatement<'a>) {
        self.keep_span(statement.span);
        oxc_ast_visit::walk::walk_break_statement(self, statement);
    }

    fn visit_continue_statement(&mut self, statement: &ContinueStatement<'a>) {
        self.keep_span(statement.span);
        oxc_ast_visit::walk::walk_continue_statement(self, statement);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::{SourceView, render_source};

    #[test]
    fn elided_source_keeps_control_flow_calls_and_types() -> Result<()> {
        let source = r"export async function checkout(
  cart: Cart,
  inventory: Inventory,
): Promise<Order> {
  const localPlumbingWithAnIntentionallyLongName = cart.items.length + cart.version + cart.revision;
  if (cart.items.length === 0) {
    throw new EmptyCartError();
  }
  for (const item of cart.items) {
    await inventory.reserve(item);
  }
  try {
    await payments.authorize(cart.total);
  } catch (error) {
    logger.warn(error);
    throw error;
  }
  return order;
}
";
        let rendered = render_source(
            Path::new("checkout.ts"),
            source,
            0,
            source.len(),
            SourceView::Elided,
            12_000,
        )?;
        assert!(rendered.text.contains("cart: Cart"));
        assert!(rendered.text.contains("if (cart.items.length === 0)"));
        assert!(rendered.text.contains("throw new EmptyCartError"));
        assert!(rendered.text.contains("for (const item"));
        assert!(rendered.text.contains("inventory.reserve"));
        assert!(rendered.text.contains("catch (error)"));
        assert!(rendered.text.contains("return order"));
        assert!(
            !rendered
                .text
                .contains("localPlumbingWithAnIntentionallyLongName")
        );
        assert!(!rendered.elisions.is_empty());
        assert!(rendered.rendered_bytes < rendered.original_bytes);
        Ok(())
    }

    #[test]
    fn both_views_obey_the_same_source_byte_ceiling() -> Result<()> {
        let source = format!("export const value = '{}';\n", "x".repeat(2_000));
        for view in [SourceView::Full, SourceView::Elided] {
            let rendered =
                render_source(Path::new("large.ts"), &source, 0, source.len(), view, 256)?;
            assert!(rendered.rendered_bytes <= 256);
            assert!(rendered.budget_truncated);
        }
        Ok(())
    }
}
