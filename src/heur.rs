//! Tier-2/3 heuristic extraction: constructs the checker-less runtime graph
//! can't bind precisely — CommonJS require/exports, dynamic imports, and
//! string-keyed event wiring. Confidence is labeled accordingly.

use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

#[derive(Debug, Clone)]
pub struct RequireBinding {
    pub local: String,
    /// "*" for whole-module, otherwise the destructured member name.
    pub imported: String,
    pub request: String,
}

#[derive(Debug, Clone)]
pub struct CjsExport {
    pub export_name: String,
    pub local_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventSite {
    pub role: &'static str, // emit | listen
    pub name: String,
    pub method: String,
    pub span_start: u32,
}

#[derive(Debug, Clone)]
pub struct MemberCall {
    pub prop: String,
    pub object: Option<String>,
    /// True when the receiver base is a bare identifier with no binding in
    /// the file's scope tree — a genuine global reference (`console`, a
    /// CommonJS `module`), never a parameter, import, or local that happens
    /// to share the name.
    pub receiver_unbound: bool,
    /// Full static receiver chain (`dbs.wave.card`, `this.db`); None when any
    /// link is computed or a call result.
    pub receiver: Option<String>,
    pub span_start: u32,
    /// End of the complete CallExpression: a multiline call's evidence lines
    /// all fall inside [span_start, span_end].
    pub span_end: u32,
    /// Exact UTF-8 byte span of the receiver expression (`dbs.wave.card`).
    pub receiver_start: u32,
    pub receiver_end: u32,
    /// Exact UTF-8 byte span of the statically named property (`insert`).
    pub property_start: u32,
    pub property_end: u32,
}

#[derive(Debug, Clone)]
pub struct DynamicImport {
    pub request: String,
    pub span_start: u32,
}

#[derive(Debug, Clone)]
pub struct MethodDef {
    pub class: String,
    pub name: String,
    pub span_start: u32,
    pub span_end: u32,
}

#[derive(Debug, Default)]
pub struct Heuristics {
    pub requires: Vec<RequireBinding>,
    pub cjs_exports: Vec<CjsExport>,
    pub events: Vec<EventSite>,
    pub member_calls: Vec<MemberCall>,
    pub dynamic_imports: Vec<DynamicImport>,
    pub methods: Vec<MethodDef>,
}

const EMIT_METHODS: &[&str] = &[
    "emit",
    "publish",
    "dispatch",
    "dispatchEvent",
    "trigger",
    "send",
];
const LISTEN_METHODS: &[&str] = &[
    "on",
    "once",
    "off",
    "addListener",
    "addEventListener",
    "subscribe",
    "handle",
];

struct HeurVisitor<'s> {
    out: Heuristics,
    scoping: &'s oxc_semantic::Scoping,
}

fn string_arg(args: &[Argument<'_>]) -> Option<String> {
    match args.first() {
        Some(Argument::StringLiteral(s)) => Some(s.value.to_string()),
        Some(Argument::TemplateLiteral(t)) if t.expressions.is_empty() => {
            t.quasis.first().map(|q| q.value.raw.to_string())
        }
        _ => None,
    }
}

fn require_request(expr: &Expression<'_>) -> Option<String> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    let Expression::Identifier(callee) = &call.callee else {
        return None;
    };
    if callee.name != "require" {
        return None;
    }
    string_arg(&call.arguments)
}

impl<'a> Visit<'a> for HeurVisitor<'_> {
    fn visit_variable_declarator(&mut self, decl: &VariableDeclarator<'a>) {
        if let Some(init) = &decl.init
            && let Some(request) = require_request(init)
        {
            match &decl.id {
                BindingPattern::BindingIdentifier(id) => {
                    self.out.requires.push(RequireBinding {
                        local: id.name.to_string(),
                        imported: "*".into(),
                        request,
                    });
                }
                BindingPattern::ObjectPattern(obj) => {
                    for prop in &obj.properties {
                        if let (Some(key), BindingPattern::BindingIdentifier(local)) =
                            (prop.key.static_name(), &prop.value)
                        {
                            self.out.requires.push(RequireBinding {
                                local: local.name.to_string(),
                                imported: key.to_string(),
                                request: request.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, decl);
    }

    fn visit_assignment_expression(&mut self, expr: &AssignmentExpression<'a>) {
        // module.exports.X = ... | exports.X = ... | module.exports = { X, Y }
        if let AssignmentTarget::StaticMemberExpression(target) = &expr.left {
            let obj_text = member_path(&target.object);
            let prop = target.property.name.to_string();
            match obj_text.as_deref() {
                Some("module.exports") | Some("exports") => {
                    let local = match &expr.right {
                        Expression::Identifier(id) => Some(id.name.to_string()),
                        _ => None,
                    };
                    self.out.cjs_exports.push(CjsExport {
                        export_name: prop,
                        local_name: local,
                    });
                }
                Some("module") if prop == "exports" => {
                    if let Expression::ObjectExpression(obj) = &expr.right {
                        for p in &obj.properties {
                            if let ObjectPropertyKind::ObjectProperty(op) = p
                                && let Some(name) = op.key.static_name()
                            {
                                let local = match &op.value {
                                    Expression::Identifier(id) => Some(id.name.to_string()),
                                    _ if op.shorthand => Some(name.to_string()),
                                    _ => None,
                                };
                                self.out.cjs_exports.push(CjsExport {
                                    export_name: name.to_string(),
                                    local_name: local,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        oxc_ast_visit::walk::walk_assignment_expression(self, expr);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::StaticMemberExpression(m) = &call.callee {
            let prop = m.property.name.to_string();
            let mut receiver_unbound = false;
            let object = match &m.object {
                Expression::Identifier(id) => {
                    receiver_unbound = self
                        .scoping
                        .get_reference(id.reference_id())
                        .symbol_id()
                        .is_none();
                    Some(id.name.to_string())
                }
                Expression::ThisExpression(_) => Some("this".into()),
                _ => None,
            };
            // Event wiring by string key.
            if let Some(event) = string_arg(&call.arguments) {
                let role = if EMIT_METHODS.contains(&prop.as_str()) {
                    Some("emit")
                } else if LISTEN_METHODS.contains(&prop.as_str()) {
                    Some("listen")
                } else {
                    None
                };
                if let Some(role) = role {
                    self.out.events.push(EventSite {
                        role,
                        name: event,
                        method: prop.clone(),
                        span_start: call.span.start,
                    });
                }
            }
            self.out.member_calls.push(MemberCall {
                prop,
                object,
                receiver_unbound,
                receiver: member_path(&m.object),
                span_start: call.span.start,
                span_end: call.span.end,
                receiver_start: m.object.span().start,
                receiver_end: m.object.span().end,
                property_start: m.property.span.start,
                property_end: m.property.span.end,
            });
        }
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        if let Some(id) = &class.id {
            let class_name = id.name.to_string();
            for member in &class.body.body {
                if let ClassElement::MethodDefinition(m) = member
                    && let Some(name) = m.key.static_name()
                {
                    self.out.methods.push(MethodDef {
                        class: class_name.clone(),
                        name: name.to_string(),
                        span_start: m.span().start,
                        span_end: m.span().end,
                    });
                }
            }
        }
        oxc_ast_visit::walk::walk_class(self, class);
    }

    fn visit_import_expression(&mut self, expr: &ImportExpression<'a>) {
        let request = match &expr.source {
            Expression::StringLiteral(s) => Some(s.value.to_string()),
            Expression::TemplateLiteral(t) if t.expressions.is_empty() => {
                t.quasis.first().map(|q| q.value.raw.to_string())
            }
            _ => None,
        };
        if let Some(request) = request {
            self.out.dynamic_imports.push(DynamicImport {
                request,
                span_start: expr.span.start,
            });
        }
        oxc_ast_visit::walk::walk_import_expression(self, expr);
    }
}

pub fn extract(program: &Program<'_>, semantic: &oxc_semantic::Semantic<'_>) -> Heuristics {
    let mut v = HeurVisitor {
        out: Heuristics::default(),
        scoping: semantic.scoping(),
    };
    v.visit_program(program);
    v.out
}

/// Render `a.b.c` member paths for small depths; None for anything complex.
pub(crate) fn member_path(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::ThisExpression(_) => Some("this".into()),
        Expression::StaticMemberExpression(m) => {
            let base = member_path(&m.object)?;
            Some(format!("{base}.{}", m.property.name))
        }
        _ => None,
    }
}

#[allow(unused)]
fn span_of(expr: &Expression<'_>) -> Span {
    expr.span()
}
