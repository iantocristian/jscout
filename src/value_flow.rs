//! Bounded, syntax-and-binding receiver value flow.
//!
//! This is deliberately not a type inferrer. It records only closed lexical
//! shapes that projection can finish through the existing module/export graph:
//! `this`, direct or const-bound construction, imported/exported const values,
//! and synchronous factories whose every path returns another supported value
//! or factory call. Awaited values are deliberately excluded because JavaScript
//! thenable assimilation can change their runtime identity.

use std::collections::{HashMap, HashSet};

use oxc_ast::{AstKind, ast::*};
use oxc_semantic::Semantic;
use oxc_span::GetSpan;
use oxc_syntax::{node::NodeId, operator::UnaryOperator, symbol::SymbolId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowReference {
    /// `identifier` or syntactic `member`; projection accepts the latter only
    /// when the base resolves as a namespace import.
    pub kind: &'static str,
    pub name: String,
    pub start: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowValue {
    /// `construct`, `factory`, or `binding`.
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
    // Sloppy-mode `with` and `eval` can redirect or replace a binding
    // that the semantic model otherwise associates with a lexical root. A
    // file-wide bailout is intentionally conservative because the dynamic
    // environment can affect nested functions as well as the introducing site.
    if semantic.nodes().iter().any(|node| match node.kind() {
        AstKind::WithStatement(_) => true,
        AstKind::IdentifierReference(identifier) => identifier.name == "eval",
        _ => false,
    }) {
        return ValueFlows::default();
    }
    let mut flows = ValueFlows::default();
    let (binding_member_writes, this_member_writes) = collect_member_writes(semantic);
    let returns = collect_function_returns(semantic, &binding_member_writes);
    extract_classes(semantic, &returns, this_member_writes, &mut flows);
    extract_functions(semantic, &returns, &binding_member_writes, &mut flows);
    extract_bindings(semantic, &binding_member_writes, &mut flows);
    extract_receivers(semantic, &binding_member_writes, &mut flows);
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

fn extract_bindings(
    semantic: &Semantic<'_>,
    binding_member_writes: &BindingMemberWrites,
    flows: &mut ValueFlows,
) {
    let scoping = semantic.scoping();
    for (name, symbol_id) in scoping.get_bindings(scoping.root_scope_id()) {
        let AstKind::VariableDeclarator(declaration) =
            semantic.symbol_declaration(*symbol_id).kind()
        else {
            continue;
        };
        let BindingPattern::BindingIdentifier(identifier) = &declaration.id else {
            continue;
        };
        if identifier.symbol_id() != *symbol_id
            || !declaration.kind.is_const()
            || scoping.symbol_is_mutated(*symbol_id)
        {
            continue;
        }
        let Some(value) = declaration.init.as_ref().and_then(|init| {
            value_from_expression(init, semantic, binding_member_writes, &mut HashSet::new())
        }) else {
            continue;
        };
        flows.bindings.push(BindingFlow {
            name: name.to_string(),
            start: scoping.symbol_span(*symbol_id).start,
            value,
        });
    }
}

fn extract_classes(
    semantic: &Semantic<'_>,
    returns: &ReturnCatalog,
    mut written_members: ThisMemberWrites,
    flows: &mut ValueFlows,
) {
    for node in semantic.nodes() {
        let AstKind::Class(class) = node.kind() else {
            continue;
        };
        let Some(id) = &class.id else {
            continue;
        };
        let unsafe_constructor = class.body.body.iter().any(|element| {
            matches!(
                element,
                ClassElement::MethodDefinition(method)
                    if method.kind == MethodDefinitionKind::Constructor
                        && returns.contains_key(&method.value.node_id.get())
            )
        });
        if class.declare
            || class_has_runtime_decorators(class)
            || unsafe_constructor
            || semantic.scoping().symbol_is_mutated(id.symbol_id())
        {
            continue;
        }
        let super_class = match class.super_class.as_ref() {
            Some(super_class) => {
                let Some(super_class) = flow_reference(super_class, semantic) else {
                    continue;
                };
                Some(super_class)
            }
            None => None,
        };
        let mut instance_methods = Vec::new();
        let mut bodyless_instance_methods = Vec::new();
        let mut blocked_instance_members =
            written_members.remove(&id.span.start).unwrap_or_default();
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) if !method.r#static => {
                    if method.kind == MethodDefinitionKind::Constructor {
                        for parameter in &method.value.params.items {
                            if parameter.accessibility.is_none()
                                && !parameter.readonly
                                && !parameter.r#override
                            {
                                continue;
                            }
                            match &parameter.pattern {
                                BindingPattern::BindingIdentifier(identifier) => {
                                    blocked_instance_members.push(identifier.name.to_string());
                                }
                                _ => blocked_instance_members.push("*".to_string()),
                            }
                        }
                        continue;
                    }
                    let Some(name) = method.key.static_name().map(|name| name.to_string()) else {
                        if method.computed {
                            blocked_instance_members.push("*".to_string());
                        }
                        continue;
                    };
                    if method.r#type == MethodDefinitionType::MethodDefinition
                        && method.kind == MethodDefinitionKind::Method
                        && method.value.body.is_some()
                    {
                        instance_methods.push(ClassMethodFlow {
                            name,
                            start: method.span.start,
                        });
                    } else if method.kind == MethodDefinitionKind::Method {
                        bodyless_instance_methods.push(name);
                    } else {
                        blocked_instance_members.push(name);
                    }
                }
                ClassElement::PropertyDefinition(property) if !property.r#static => {
                    if let Some(name) = property
                        .key
                        .static_name()
                        .map(|name| name.to_string())
                        .or_else(|| property.computed.then(|| "*".to_string()))
                    {
                        blocked_instance_members.push(name);
                    }
                }
                ClassElement::AccessorProperty(property) if !property.r#static => {
                    if let Some(name) = property
                        .key
                        .static_name()
                        .map(|name| name.to_string())
                        .or_else(|| property.computed.then(|| "*".to_string()))
                    {
                        blocked_instance_members.push(name);
                    }
                }
                _ => {}
            }
        }
        instance_methods
            .sort_by(|left, right| (&left.name, left.start).cmp(&(&right.name, right.start)));
        instance_methods
            .dedup_by(|left, right| left.name == right.name && left.start == right.start);
        for name in bodyless_instance_methods {
            if !instance_methods.iter().any(|method| method.name == name) {
                blocked_instance_members.push(name);
            }
        }
        blocked_instance_members.sort();
        blocked_instance_members.dedup();
        flows.classes.push(ClassFlow {
            name: id.name.to_string(),
            start: id.span.start,
            super_class,
            instance_methods,
            blocked_instance_members,
        });
    }
}

fn method_has_runtime_decorators(method: &MethodDefinition<'_>) -> bool {
    !method.decorators.is_empty()
        || method
            .value
            .params
            .items
            .iter()
            .any(|parameter| !parameter.decorators.is_empty())
}

fn class_has_runtime_decorators(class: &Class<'_>) -> bool {
    !class.decorators.is_empty()
        || class.body.body.iter().any(|element| match element {
            ClassElement::MethodDefinition(method) => method_has_runtime_decorators(method),
            ClassElement::PropertyDefinition(property) => !property.decorators.is_empty(),
            ClassElement::AccessorProperty(property) => !property.decorators.is_empty(),
            _ => false,
        })
}

type BindingMemberWrites = HashMap<SymbolId, HashSet<String>>;
type ThisMemberWrites = HashMap<u32, Vec<String>>;

fn collect_member_writes(semantic: &Semantic<'_>) -> (BindingMemberWrites, ThisMemberWrites) {
    let mut binding_writes = BindingMemberWrites::new();
    let mut this_writes = ThisMemberWrites::new();
    for (node_id, node) in semantic.nodes().iter_enumerated() {
        for (object, property) in mutated_members(node.kind()) {
            match object.get_inner_expression() {
                Expression::ThisExpression(_) => {
                    if let Some((_, class_start)) = enclosing_instance_class(node_id, semantic) {
                        this_writes.entry(class_start).or_default().push(property);
                    }
                }
                Expression::Identifier(identifier) => {
                    let reference = semantic.scoping().get_reference(identifier.reference_id());
                    if let Some(symbol_id) = reference.symbol_id() {
                        binding_writes
                            .entry(symbol_id)
                            .or_default()
                            .insert(property);
                    }
                }
                _ => {}
            }
        }
    }
    (binding_writes, this_writes)
}

fn push_mutated_member<'a>(
    member: &'a MemberExpression<'a>,
    members: &mut Vec<(&'a Expression<'a>, String)>,
) {
    if matches!(member, MemberExpression::PrivateFieldExpression(_)) {
        return;
    }
    members.push((
        member.object(),
        member.static_property_name().unwrap_or("*").to_string(),
    ));
}

fn collect_assignment_target_members<'a>(
    target: &'a AssignmentTarget<'a>,
    members: &mut Vec<(&'a Expression<'a>, String)>,
) {
    if let Some(member) = target.as_member_expression() {
        push_mutated_member(member, members);
        return;
    }
    if let Some(expression) = target.get_expression()
        && let Some(member) = expression.get_inner_expression().as_member_expression()
    {
        push_mutated_member(member, members);
        return;
    }
    match target {
        AssignmentTarget::ArrayAssignmentTarget(array) => {
            for element in array.elements.iter().flatten() {
                collect_assignment_target_maybe_default_members(element, members);
            }
            if let Some(rest) = &array.rest {
                collect_assignment_target_members(&rest.target, members);
            }
        }
        AssignmentTarget::ObjectAssignmentTarget(object) => {
            for property in &object.properties {
                if let AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) =
                    property
                {
                    collect_assignment_target_maybe_default_members(&property.binding, members);
                }
            }
            if let Some(rest) = &object.rest {
                collect_assignment_target_members(&rest.target, members);
            }
        }
        _ => {}
    }
}

fn collect_assignment_target_maybe_default_members<'a>(
    target: &'a AssignmentTargetMaybeDefault<'a>,
    members: &mut Vec<(&'a Expression<'a>, String)>,
) {
    if let Some(target) = target.as_assignment_target() {
        collect_assignment_target_members(target, members);
    } else if let AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) = target {
        collect_assignment_target_members(&default.binding, members);
    }
}

fn mutated_members<'a>(kind: AstKind<'a>) -> Vec<(&'a Expression<'a>, String)> {
    let mut members = Vec::new();
    match kind {
        AstKind::AssignmentExpression(assignment) => {
            collect_assignment_target_members(&assignment.left, &mut members);
        }
        AstKind::UpdateExpression(update) => {
            if let Some(member) = update.argument.as_member_expression() {
                push_mutated_member(member, &mut members);
            }
        }
        AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => {
            if let Some(member) = unary.argument.get_inner_expression().as_member_expression() {
                push_mutated_member(member, &mut members);
            }
        }
        AstKind::ForInStatement(statement) => {
            if let Some(target) = statement.left.as_assignment_target() {
                collect_assignment_target_members(target, &mut members);
            }
        }
        AstKind::ForOfStatement(statement) => {
            if let Some(target) = statement.left.as_assignment_target() {
                collect_assignment_target_members(target, &mut members);
            }
        }
        _ => {}
    }
    members
}

type ReturnCatalog = HashMap<NodeId, Option<Vec<(u32, FlowValue)>>>;

fn collect_function_returns(
    semantic: &Semantic<'_>,
    binding_member_writes: &BindingMemberWrites,
) -> ReturnCatalog {
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
        let value = statement.argument.as_ref().and_then(|argument| {
            value_from_expression(
                argument,
                semantic,
                binding_member_writes,
                &mut HashSet::new(),
            )
        });
        let entry = returns.entry(owner).or_insert_with(|| Some(Vec::new()));
        match (entry.as_mut(), value) {
            (Some(values), Some(value)) => values.push((statement.span.start, value)),
            _ => *entry = None,
        }
    }
    returns
}

fn extract_functions(
    semantic: &Semantic<'_>,
    returns: &ReturnCatalog,
    binding_member_writes: &BindingMemberWrites,
    flows: &mut ValueFlows,
) {
    let scoping = semantic.scoping();
    let root = scoping.root_scope_id();
    let mut declaration_counts = HashMap::<String, usize>::new();
    for (node_id, node) in semantic.nodes().iter_enumerated() {
        match node.kind() {
            AstKind::Function(function)
                if function.r#type == FunctionType::FunctionDeclaration
                    && !semantic.nodes().ancestors(node_id).any(|ancestor| {
                        matches!(
                            ancestor.kind(),
                            AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
                        )
                    }) =>
            {
                if let Some(identifier) = &function.id {
                    *declaration_counts
                        .entry(identifier.name.to_string())
                        .or_default() += 1;
                }
            }
            AstKind::VariableDeclarator(declaration) => {
                for identifier in declaration.id.get_binding_identifiers() {
                    if scoping.symbol_scope_id(identifier.symbol_id()) == root {
                        *declaration_counts
                            .entry(identifier.name.to_string())
                            .or_default() += 1;
                    }
                }
            }
            _ => {}
        }
    }
    for (name, symbol_id) in scoping.get_bindings(root) {
        if scoping.symbol_is_mutated(*symbol_id)
            || declaration_counts
                .get(name.as_str())
                .is_some_and(|count| *count > 1)
        {
            continue;
        }
        let start = scoping.symbol_span(*symbol_id).start;
        let summary = match semantic.symbol_declaration(*symbol_id).kind() {
            AstKind::Function(function) if !function.generator => function
                .body
                .as_deref()
                .and_then(|body| {
                    function_returns(function.node_id.get(), &body.statements, returns)
                })
                .map(|returns| (function.r#async, returns)),
            AstKind::VariableDeclarator(declaration)
                if declaration.kind.is_const()
                    && matches!(
                        &declaration.id,
                        BindingPattern::BindingIdentifier(identifier)
                            if identifier.symbol_id() == *symbol_id
                    ) =>
            {
                declaration.init.as_ref().and_then(|init| {
                    function_expression_returns(init, semantic, returns, binding_member_writes)
                })
            }
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
    binding_member_writes: &BindingMemberWrites,
) -> Option<(bool, Vec<FlowValue>)> {
    match expression.get_inner_expression() {
        Expression::FunctionExpression(function) if !function.generator => function
            .body
            .as_deref()
            .and_then(|body| function_returns(function.node_id.get(), &body.statements, returns))
            .map(|returns| (function.r#async, returns)),
        Expression::ArrowFunctionExpression(arrow) => {
            if let Some(expression) = arrow.body.as_expression() {
                value_from_expression(
                    expression,
                    semantic,
                    binding_member_writes,
                    &mut HashSet::new(),
                )
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

fn extract_receivers(
    semantic: &Semantic<'_>,
    binding_member_writes: &BindingMemberWrites,
    flows: &mut ValueFlows,
) {
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
                if let Expression::Identifier(identifier) = member.object.get_inner_expression() {
                    let reference = semantic.scoping().get_reference(identifier.reference_id());
                    if reference.symbol_id().is_some_and(|symbol_id| {
                        binding_member_writes
                            .get(&symbol_id)
                            .is_some_and(|properties| {
                                properties.contains(member.property.name.as_str())
                                    || properties.contains("*")
                            })
                    }) {
                        continue;
                    }
                }
                if let Some(value) = value_from_expression(
                    &member.object,
                    semantic,
                    binding_member_writes,
                    &mut HashSet::new(),
                ) {
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
    let call_span = nodes.get_node(node_id).kind().span();
    for (ancestor_id, ancestor) in nodes.ancestors_enumerated(node_id) {
        match ancestor.kind() {
            AstKind::Function(_) => {
                let AstKind::MethodDefinition(method) = nodes.parent_kind(ancestor_id) else {
                    // A nested ordinary function has its own `this`; arrow
                    // functions do not enter this branch and retain lexical
                    // `this`.
                    return None;
                };
                if method.r#static || method_has_runtime_decorators(method) {
                    return None;
                }
                return enclosing_named_class(method.node_id.get(), nodes);
            }
            AstKind::PropertyDefinition(property) => {
                if property.r#static
                    || property.computed
                    || !property.decorators.is_empty()
                    || !property
                        .value
                        .as_ref()
                        .is_some_and(|value| value.span().contains_inclusive(call_span))
                {
                    return None;
                }
                return enclosing_named_class(ancestor_id, nodes);
            }
            AstKind::AccessorProperty(property) => {
                if property.r#static
                    || property.computed
                    || !property.decorators.is_empty()
                    || !property
                        .value
                        .as_ref()
                        .is_some_and(|value| value.span().contains_inclusive(call_span))
                {
                    return None;
                }
                return enclosing_named_class(ancestor_id, nodes);
            }
            AstKind::StaticBlock(_) | AstKind::Class(_) => return None,
            _ => {}
        }
    }
    None
}

fn enclosing_named_class(
    node_id: NodeId,
    nodes: &oxc_semantic::AstNodes<'_>,
) -> Option<(String, u32)> {
    for node in nodes.ancestors(node_id) {
        let AstKind::Class(class) = node.kind() else {
            continue;
        };
        if class_has_runtime_decorators(class) {
            return None;
        }
        return class
            .id
            .as_ref()
            .map(|id| (id.name.to_string(), id.span.start));
    }
    None
}

fn value_from_expression(
    expression: &Expression<'_>,
    semantic: &Semantic<'_>,
    binding_member_writes: &BindingMemberWrites,
    visited: &mut HashSet<SymbolId>,
) -> Option<FlowValue> {
    match expression.get_inner_expression() {
        Expression::NewExpression(expression) => Some(FlowValue {
            kind: "construct",
            target: flow_reference(&expression.callee, semantic)?,
        }),
        Expression::CallExpression(expression) if !expression.optional => Some(FlowValue {
            kind: "factory",
            target: flow_reference(&expression.callee, semantic)?,
        }),
        // `await` assimilates arbitrary thenables, so even `await new C()` does
        // not prove that the resulting receiver is a `C`.
        Expression::AwaitExpression(_) => None,
        Expression::Identifier(identifier) => {
            let reference = semantic.scoping().get_reference(identifier.reference_id());
            let symbol_id = reference.symbol_id()?;
            if !visited.insert(symbol_id)
                || semantic.scoping().symbol_is_mutated(symbol_id)
                || binding_member_writes.contains_key(&symbol_id)
            {
                return None;
            }
            let declaration = semantic.symbol_declaration(symbol_id);
            let value = match declaration.kind() {
                AstKind::VariableDeclarator(declaration)
                    if declaration.kind.is_const()
                        && matches!(
                            &declaration.id,
                            BindingPattern::BindingIdentifier(binding)
                                if binding.symbol_id() == symbol_id
                        ) =>
                {
                    value_from_expression(
                        declaration.init.as_ref()?,
                        semantic,
                        binding_member_writes,
                        visited,
                    )
                }
                AstKind::ImportSpecifier(_) | AstKind::ImportDefaultSpecifier(_) => {
                    Some(FlowValue {
                        kind: "binding",
                        target: FlowReference {
                            kind: "identifier",
                            name: identifier.name.to_string(),
                            start: identifier.span.start,
                        },
                    })
                }
                _ => None,
            };
            visited.remove(&symbol_id);
            value
        }
        _ => None,
    }
}

fn flow_reference(expression: &Expression<'_>, semantic: &Semantic<'_>) -> Option<FlowReference> {
    match expression.get_inner_expression() {
        Expression::Identifier(identifier) => {
            let reference = semantic.scoping().get_reference(identifier.reference_id());
            let symbol_id = reference.symbol_id()?;
            if semantic.scoping().symbol_scope_id(symbol_id) != semantic.scoping().root_scope_id()
                || semantic.scoping().symbol_is_mutated(symbol_id)
            {
                return None;
            }
            Some(FlowReference {
                kind: "identifier",
                name: identifier.name.to_string(),
                start: identifier.span.start,
            })
        }
        Expression::StaticMemberExpression(member) => {
            if member.optional {
                return None;
            }
            let Expression::Identifier(identifier) = member.object.get_inner_expression() else {
                return None;
            };
            let reference = semantic.scoping().get_reference(identifier.reference_id());
            let symbol_id = reference.symbol_id()?;
            if semantic.scoping().symbol_scope_id(symbol_id) != semantic.scoping().root_scope_id()
                || semantic.scoping().symbol_is_mutated(symbol_id)
            {
                return None;
            }
            Some(FlowReference {
                kind: "member",
                name: member.property.name.to_string(),
                start: identifier.span.start,
            })
        }
        _ => None,
    }
}
