//! Bounded, syntax-and-binding receiver value flow.
//!
//! This is deliberately not a type inferrer. It records only closed lexical
//! shapes that projection can finish through the existing module/export graph:
//! `this`, direct or const-bound construction, imported/exported const values,
//! and synchronous or awaited factories whose every path returns another
//! supported value or factory call.

use std::collections::{HashMap, HashSet};

use oxc_ast::{AstKind, ast::*};
use oxc_semantic::Semantic;
use oxc_syntax::{node::NodeId, symbol::SymbolId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowReference {
    pub name: String,
    pub start: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowValue {
    /// `construct`, `factory`, `await_factory`, or `binding`.
    pub kind: &'static str,
    pub target: FlowReference,
}

#[derive(Debug, Clone)]
pub struct ReceiverFlow {
    pub call_start: u32,
    pub call_end: u32,
    /// `this` or `value`.
    pub kind: &'static str,
    pub class_name: Option<String>,
    pub class_start: Option<u32>,
    pub value: Option<FlowValue>,
}

#[derive(Debug, Clone)]
pub struct FunctionFlow {
    pub name: String,
    pub start: u32,
    pub is_async: bool,
    pub returns: Vec<FlowValue>,
}

#[derive(Debug, Clone)]
pub struct BindingFlow {
    pub name: String,
    pub start: u32,
    pub value: FlowValue,
}

#[derive(Debug, Clone)]
pub struct ClassFlow {
    pub name: String,
    pub start: u32,
    pub super_class: Option<FlowReference>,
    pub instance_methods: Vec<ClassMethodFlow>,
    pub blocked_instance_members: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ClassMethodFlow {
    pub name: String,
    pub start: u32,
}

#[derive(Debug, Default)]
pub struct ValueFlows {
    pub receivers: Vec<ReceiverFlow>,
    pub functions: Vec<FunctionFlow>,
    pub bindings: Vec<BindingFlow>,
    pub classes: Vec<ClassFlow>,
}

pub fn extract(semantic: &Semantic<'_>) -> ValueFlows {
    let mut flows = ValueFlows::default();
    let returns = collect_function_returns(semantic);
    extract_classes(semantic, &mut flows);
    extract_functions(semantic, &returns, &mut flows);
    extract_bindings(semantic, &mut flows);
    extract_receivers(semantic, &mut flows);
    flows
        .receivers
        .sort_by_key(|flow| (flow.call_start, flow.call_end));
    flows
        .functions
        .sort_by(|left, right| (&left.name, left.start).cmp(&(&right.name, right.start)));
    flows
        .bindings
        .sort_by(|left, right| (&left.name, left.start).cmp(&(&right.name, right.start)));
    flows
        .classes
        .sort_by(|left, right| (&left.name, left.start).cmp(&(&right.name, right.start)));
    flows
}

fn extract_bindings(semantic: &Semantic<'_>, flows: &mut ValueFlows) {
    let scoping = semantic.scoping();
    for (name, symbol_id) in scoping.get_bindings(scoping.root_scope_id()) {
        let AstKind::VariableDeclarator(declaration) =
            semantic.symbol_declaration(*symbol_id).kind()
        else {
            continue;
        };
        if !declaration.kind.is_const() || scoping.symbol_is_mutated(*symbol_id) {
            continue;
        }
        let Some(value) = declaration
            .init
            .as_ref()
            .and_then(|init| value_from_expression(init, semantic, &mut HashSet::new()))
        else {
            continue;
        };
        flows.bindings.push(BindingFlow {
            name: name.to_string(),
            start: scoping.symbol_span(*symbol_id).start,
            value,
        });
    }
}

fn extract_classes(semantic: &Semantic<'_>, flows: &mut ValueFlows) {
    for node in semantic.nodes() {
        let AstKind::Class(class) = node.kind() else {
            continue;
        };
        let Some(id) = &class.id else {
            continue;
        };
        let mut instance_methods = Vec::new();
        let mut blocked_instance_members = Vec::new();
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) if !method.r#static => {
                    let Some(name) = method.key.static_name() else {
                        continue;
                    };
                    if method.r#type == MethodDefinitionType::MethodDefinition
                        && method.kind == MethodDefinitionKind::Method
                    {
                        instance_methods.push(ClassMethodFlow {
                            name: name.to_string(),
                            start: method.span.start,
                        });
                    } else {
                        blocked_instance_members.push(name.to_string());
                    }
                }
                ClassElement::PropertyDefinition(property) if !property.r#static => {
                    if let Some(name) = property.key.static_name() {
                        blocked_instance_members.push(name.to_string());
                    }
                }
                ClassElement::AccessorProperty(property) if !property.r#static => {
                    if let Some(name) = property.key.static_name() {
                        blocked_instance_members.push(name.to_string());
                    }
                }
                _ => {}
            }
        }
        instance_methods
            .sort_by(|left, right| (&left.name, left.start).cmp(&(&right.name, right.start)));
        instance_methods
            .dedup_by(|left, right| left.name == right.name && left.start == right.start);
        blocked_instance_members.sort();
        blocked_instance_members.dedup();
        flows.classes.push(ClassFlow {
            name: id.name.to_string(),
            start: id.span.start,
            super_class: class.super_class.as_ref().and_then(flow_reference),
            instance_methods,
            blocked_instance_members,
        });
    }
}

type ReturnCatalog = HashMap<NodeId, Option<Vec<(u32, FlowValue)>>>;

fn collect_function_returns(semantic: &Semantic<'_>) -> ReturnCatalog {
    let mut returns = ReturnCatalog::new();
    for (node_id, node) in semantic.nodes().iter_enumerated() {
        let AstKind::ReturnStatement(statement) = node.kind() else {
            continue;
        };
        let Some(owner) = semantic
            .nodes()
            .ancestors_enumerated(node_id)
            .find(|(_, ancestor)| {
                matches!(
                    ancestor.kind(),
                    AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
                )
            })
            .map(|(id, _)| id)
        else {
            continue;
        };
        let value = statement
            .argument
            .as_ref()
            .and_then(|argument| value_from_expression(argument, semantic, &mut HashSet::new()));
        let entry = returns.entry(owner).or_insert_with(|| Some(Vec::new()));
        match (entry.as_mut(), value) {
            (Some(values), Some(value)) => values.push((statement.span.start, value)),
            _ => *entry = None,
        }
    }
    returns
}

fn extract_functions(semantic: &Semantic<'_>, returns: &ReturnCatalog, flows: &mut ValueFlows) {
    let scoping = semantic.scoping();
    let root = scoping.root_scope_id();
    for (name, symbol_id) in scoping.get_bindings(root) {
        let start = scoping.symbol_span(*symbol_id).start;
        let summary = match semantic.symbol_declaration(*symbol_id).kind() {
            AstKind::Function(function) if !function.generator => function
                .body
                .as_deref()
                .and_then(|body| {
                    function_returns(function.node_id.get(), &body.statements, returns)
                })
                .map(|returns| (function.r#async, returns)),
            AstKind::VariableDeclarator(declaration) if declaration.kind.is_const() => declaration
                .init
                .as_ref()
                .and_then(|init| function_expression_returns(init, semantic, returns)),
            _ => None,
        };
        if let Some((is_async, returns)) = summary {
            flows.functions.push(FunctionFlow {
                name: name.to_string(),
                start,
                is_async,
                returns,
            });
        }
    }
}

fn function_expression_returns(
    expression: &Expression<'_>,
    semantic: &Semantic<'_>,
    returns: &ReturnCatalog,
) -> Option<(bool, Vec<FlowValue>)> {
    match expression.get_inner_expression() {
        Expression::FunctionExpression(function) if !function.generator => function
            .body
            .as_deref()
            .and_then(|body| function_returns(function.node_id.get(), &body.statements, returns))
            .map(|returns| (function.r#async, returns)),
        Expression::ArrowFunctionExpression(arrow) => {
            if let Some(expression) = arrow.body.as_expression() {
                value_from_expression(expression, semantic, &mut HashSet::new())
                    .map(|value| (arrow.r#async, vec![value]))
            } else {
                function_returns(
                    arrow.node_id.get(),
                    &arrow.body.as_function_body()?.statements,
                    returns,
                )
                .map(|returns| (arrow.r#async, returns))
            }
        }
        _ => None,
    }
}

/// A summary exists only when the function has at least one return and every
/// return has one supported value shape. Nested functions have separate owner
/// IDs in the catalog and cannot contaminate the parent summary.
fn function_returns(
    function_id: NodeId,
    statements: &[Statement<'_>],
    returns: &ReturnCatalog,
) -> Option<Vec<FlowValue>> {
    if !statements.last().is_some_and(statement_terminates) {
        return None;
    }
    let mut values = returns.get(&function_id)?.clone()?;
    if values.is_empty() {
        return None;
    }
    values.sort_by_key(|(start, _)| *start);
    Some(values.into_iter().map(|(_, value)| value).collect())
}

fn statement_terminates(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::ReturnStatement(_) | Statement::ThrowStatement(_) => true,
        Statement::BlockStatement(block) => block.body.last().is_some_and(statement_terminates),
        Statement::IfStatement(statement) => {
            statement_terminates(&statement.consequent)
                && statement
                    .alternate
                    .as_ref()
                    .is_some_and(|alternate| statement_terminates(alternate))
        }
        _ => false,
    }
}

fn extract_receivers(semantic: &Semantic<'_>, flows: &mut ValueFlows) {
    for (node_id, node) in semantic.nodes().iter_enumerated() {
        let AstKind::CallExpression(call) = node.kind() else {
            continue;
        };
        let Expression::StaticMemberExpression(member) = &call.callee else {
            continue;
        };
        match member.object.get_inner_expression() {
            Expression::ThisExpression(_) => {
                if let Some((class_name, class_start)) = enclosing_instance_class(node_id, semantic)
                {
                    flows.receivers.push(ReceiverFlow {
                        call_start: call.span.start,
                        call_end: call.span.end,
                        kind: "this",
                        class_name: Some(class_name),
                        class_start: Some(class_start),
                        value: None,
                    });
                }
            }
            _ => {
                if let Some(value) =
                    value_from_expression(&member.object, semantic, &mut HashSet::new())
                {
                    flows.receivers.push(ReceiverFlow {
                        call_start: call.span.start,
                        call_end: call.span.end,
                        kind: "value",
                        class_name: None,
                        class_start: None,
                        value: Some(value),
                    });
                }
            }
        }
    }
}

fn enclosing_instance_class(node_id: NodeId, semantic: &Semantic<'_>) -> Option<(String, u32)> {
    let nodes = semantic.nodes();
    for (ancestor_id, ancestor) in nodes.ancestors_enumerated(node_id) {
        let AstKind::Function(_) = ancestor.kind() else {
            continue;
        };
        let AstKind::MethodDefinition(method) = nodes.parent_kind(ancestor_id) else {
            // A nested ordinary function has its own `this`; do not walk past
            // it to an enclosing class method. Arrow functions never enter
            // this branch and correctly retain lexical `this`.
            return None;
        };
        if method.r#static {
            return None;
        }
        return nodes
            .ancestors(method.node_id.get())
            .find_map(|node| match node.kind() {
                AstKind::Class(class) => class
                    .id
                    .as_ref()
                    .map(|id| (id.name.to_string(), id.span.start)),
                _ => None,
            });
    }
    None
}

fn value_from_expression(
    expression: &Expression<'_>,
    semantic: &Semantic<'_>,
    visited: &mut HashSet<SymbolId>,
) -> Option<FlowValue> {
    match expression.get_inner_expression() {
        Expression::NewExpression(expression) => Some(FlowValue {
            kind: "construct",
            target: flow_reference(&expression.callee)?,
        }),
        Expression::CallExpression(expression) => Some(FlowValue {
            kind: "factory",
            target: flow_reference(&expression.callee)?,
        }),
        Expression::AwaitExpression(expression) => {
            let mut value = value_from_expression(&expression.argument, semantic, visited)?;
            if value.kind == "factory" {
                value.kind = "await_factory";
            }
            Some(value)
        }
        Expression::Identifier(identifier) => {
            let reference = semantic.scoping().get_reference(identifier.reference_id());
            let symbol_id = reference.symbol_id()?;
            if !visited.insert(symbol_id) || semantic.scoping().symbol_is_mutated(symbol_id) {
                return None;
            }
            let declaration = semantic.symbol_declaration(symbol_id);
            let value = match declaration.kind() {
                AstKind::VariableDeclarator(declaration) if declaration.kind.is_const() => {
                    value_from_expression(declaration.init.as_ref()?, semantic, visited)
                }
                _ => None,
            };
            visited.remove(&symbol_id);
            value.or_else(|| {
                Some(FlowValue {
                    kind: "binding",
                    target: FlowReference {
                        name: identifier.name.to_string(),
                        start: identifier.span.start,
                    },
                })
            })
        }
        _ => None,
    }
}

fn flow_reference(expression: &Expression<'_>) -> Option<FlowReference> {
    match expression.get_inner_expression() {
        Expression::Identifier(identifier) => Some(FlowReference {
            name: identifier.name.to_string(),
            start: identifier.span.start,
        }),
        Expression::StaticMemberExpression(member) => {
            let Expression::Identifier(identifier) = member.object.get_inner_expression() else {
                return None;
            };
            Some(FlowReference {
                name: member.property.name.to_string(),
                start: identifier.span.start,
            })
        }
        _ => None,
    }
}
