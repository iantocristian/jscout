//! Deterministic non-symbol facts that join runtime workflows across dynamic
//! boundaries. Extraction records source-local sites first; resolution later
//! groups those sites under snapshot-canonical entities.

use std::collections::HashMap;

use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct EntitySite {
    pub plane: &'static str,
    pub entity_type: &'static str,
    pub role: &'static str,
    pub identity_kind: &'static str,
    pub identity_name: String,
    pub identity_start: u32,
    pub target_name: Option<String>,
    pub target_start: Option<u32>,
    pub span_start: u32,
    pub span_end: u32,
    pub extractor: &'static str,
    pub provenance: &'static str,
    pub confidence: &'static str,
    pub detail: serde_json::Value,
}

struct EntityVisitor {
    sites: Vec<EntitySite>,
    static_strings: HashMap<String, String>,
}

impl EntityVisitor {
    fn identity(&self, expression: &Expression<'_>) -> Option<(&'static str, String, u32)> {
        match expression {
            Expression::Identifier(identifier) => Some((
                "reference",
                identifier.name.to_string(),
                identifier.span.start,
            )),
            _ => static_string(expression, &self.static_strings)
                .map(|value| ("literal", value, expression.span().start)),
        }
    }

    fn target(expression: &Expression<'_>) -> Option<(String, u32)> {
        match expression {
            Expression::Identifier(identifier) => {
                Some((identifier.name.to_string(), identifier.span.start))
            }
            _ => None,
        }
    }

    fn extract_logic_function(&mut self, call: &CallExpression<'_>) {
        let Some(config) = first_object_argument(call) else { return };
        let Some(identifier) = object_value(config, "universalIdentifier") else { return };
        let Some((identity_kind, identity_name, identity_start)) = self.identity(identifier) else {
            return;
        };
        let target = object_value(config, "handler").and_then(Self::target);
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type: "registry",
            role: "registered_handler",
            identity_kind,
            identity_name,
            identity_start,
            target_name: target.as_ref().map(|(name, _)| name.clone()),
            target_start: target.map(|(_, start)| start),
            span_start: call.span.start,
            span_end: call.span.end,
            extractor: "twenty-define-logic-function",
            provenance: "framework-pattern",
            confidence: "likely",
            detail: json!({ "callee": "defineLogicFunction" }),
        });

        let Some(Expression::ObjectExpression(settings)) =
            object_value(config, "databaseEventTriggerSettings")
        else {
            return;
        };
        let Some(event_name) = object_value(settings, "eventName") else { return };
        let Some(event_name) = static_string(event_name, &self.static_strings) else { return };
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type: "data_lifecycle",
            role: "lifecycle_listener",
            identity_kind: "literal",
            identity_name: event_name,
            identity_start: event_name_span(settings).unwrap_or(call.span.start),
            target_name: object_value(config, "handler")
                .and_then(Self::target)
                .map(|(name, _)| name),
            target_start: object_value(config, "handler")
                .and_then(Self::target)
                .map(|(_, start)| start),
            span_start: settings.span.start,
            span_end: settings.span.end,
            extractor: "twenty-database-event-trigger",
            provenance: "framework-pattern",
            confidence: "likely",
            detail: json!({ "source": "databaseEventTriggerSettings.eventName" }),
        });
    }

    fn extract_mutation(&mut self, call: &CallExpression<'_>) {
        let Expression::StaticMemberExpression(member) = &call.callee else { return };
        if member.property.name != "mutation" {
            return;
        }
        let Some(mutation) = first_object_argument(call) else { return };
        for property in &mutation.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else { continue };
            let Some(operation) = property.key.static_name() else { continue };
            let Some((action, resource)) = mutation_resource(operation.as_ref()) else {
                continue;
            };
            let event = format!("{resource}.{action}");
            self.sites.push(EntitySite {
                plane: "runtime",
                entity_type: "data_lifecycle",
                role: "lifecycle_producer",
                identity_kind: "literal",
                identity_name: event,
                identity_start: property.key.span().start,
                target_name: None,
                target_start: None,
                span_start: property.span.start,
                span_end: property.span.end,
                extractor: "graphql-mutation-lifecycle",
                provenance: "naming-convention",
                confidence: "likely",
                detail: json!({
                    "operation": operation,
                    "resource": resource,
                    "action": action,
                }),
            });
        }
    }
}

impl<'a> Visit<'a> for EntityVisitor {
    fn visit_variable_declarator(&mut self, declaration: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(identifier) = &declaration.id
            && let Some(initializer) = &declaration.init
            && let Some(value) = static_string(initializer, &self.static_strings)
        {
            self.static_strings.insert(identifier.name.to_string(), value);
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, declaration);
    }

    fn visit_object_expression(&mut self, object: &ObjectExpression<'a>) {
        if let Some(identifier) = object_value(object, "targetLogicFunctionUniversalIdentifier")
            && let Some((identity_kind, identity_name, identity_start)) = self.identity(identifier)
        {
            self.sites.push(EntitySite {
                plane: "runtime",
                entity_type: "registry",
                role: "dispatch_site",
                identity_kind,
                identity_name,
                identity_start,
                target_name: None,
                target_start: None,
                span_start: object.span.start,
                span_end: object.span.end,
                extractor: "twenty-logic-function-dispatch",
                provenance: "framework-field",
                confidence: "likely",
                detail: json!({
                    "field": "targetLogicFunctionUniversalIdentifier"
                }),
            });
        }
        oxc_ast_visit::walk::walk_object_expression(self, object);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if matches!(
            &call.callee,
            Expression::Identifier(identifier) if identifier.name == "defineLogicFunction"
        ) {
            self.extract_logic_function(call);
        }
        self.extract_mutation(call);
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }
}

/// Extract source-local entity evidence. Recognizers are deliberately narrow:
/// each emitted site names its framework/convention and remains below
/// `certain` unless syntax alone proves the relationship.
pub fn extract(program: &Program<'_>) -> Vec<EntitySite> {
    let mut visitor = EntityVisitor {
        sites: Vec::new(),
        static_strings: HashMap::new(),
    };
    visitor.visit_program(program);
    visitor.sites
}

fn first_object_argument<'a>(call: &'a CallExpression<'a>) -> Option<&'a ObjectExpression<'a>> {
    match call.arguments.first()? {
        Argument::ObjectExpression(object) => Some(object),
        _ => None,
    }
}

fn object_value<'a>(object: &'a ObjectExpression<'a>, name: &str) -> Option<&'a Expression<'a>> {
    object.properties.iter().find_map(|property| {
        let ObjectPropertyKind::ObjectProperty(property) = property else { return None };
        (property.key.static_name().as_deref() == Some(name)).then_some(&property.value)
    })
}

fn event_name_span(settings: &ObjectExpression<'_>) -> Option<u32> {
    object_value(settings, "eventName").map(|expression| expression.span().start)
}

fn static_string(
    expression: &Expression<'_>,
    constants: &HashMap<String, String>,
) -> Option<String> {
    match expression {
        Expression::StringLiteral(literal) => Some(literal.value.to_string()),
        Expression::TemplateLiteral(template) => {
            let mut value = String::new();
            for (index, quasi) in template.quasis.iter().enumerate() {
                value.push_str(quasi.value.raw.as_str());
                if let Some(expression) = template.expressions.get(index) {
                    value.push_str(&static_string(expression, constants)?);
                }
            }
            Some(value)
        }
        Expression::Identifier(identifier) => constants.get(identifier.name.as_str()).cloned(),
        _ => None,
    }
}

fn mutation_resource(operation: &str) -> Option<(&'static str, String)> {
    let (action, mut resource) = [
        ("create", "created"),
        ("update", "updated"),
        ("delete", "deleted"),
    ]
    .into_iter()
    .find_map(|(prefix, action)| operation.strip_prefix(prefix).map(|rest| (action, rest)))?;
    for qualifier in ["One", "Many"] {
        if let Some(unqualified) = resource.strip_prefix(qualifier) {
            resource = unqualified;
            break;
        }
    }
    if resource.is_empty() {
        return None;
    }
    let mut chars = resource.chars();
    let first = chars.next()?.to_lowercase().collect::<String>();
    Some((action, format!("{first}{}", chars.as_str())))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::extract;

    fn sites(source: &str) -> Result<Vec<super::EntitySite>> {
        crate::parse::with_parsed(source, Path::new("fixture.ts"), |ret, _| {
            extract(&ret.program)
        })
    }

    #[test]
    fn extracts_registry_registration_and_dispatch() -> Result<()> {
        let extracted = sites(
            "const TARGET = 'logic-id';\n\
             export const handler = () => {};\n\
             export default defineLogicFunction({ universalIdentifier: TARGET, handler });\n\
             export const route = () => ({ targetLogicFunctionUniversalIdentifier: TARGET });\n",
        )?;
        assert!(extracted.iter().any(|site| {
            site.entity_type == "registry"
                && site.role == "registered_handler"
                && site.identity_kind == "reference"
                && site.identity_name == "TARGET"
                && site.target_name.as_deref() == Some("handler")
        }));
        assert!(extracted.iter().any(|site| {
            site.entity_type == "registry"
                && site.role == "dispatch_site"
                && site.identity_name == "TARGET"
        }));
        Ok(())
    }

    #[test]
    fn joins_database_event_listener_and_graphql_mutation_by_resource() -> Result<()> {
        let extracted = sites(
            "const OBJECT = 'slackAssistantRequest';\n\
             const worker = () => {};\n\
             defineLogicFunction({\n\
               universalIdentifier: 'worker-id',\n\
               handler: worker,\n\
               databaseEventTriggerSettings: { eventName: `${OBJECT}.created` },\n\
             });\n\
             client.mutation({ createSlackAssistantRequest: { id: true } });\n",
        )?;
        let roles: Vec<(&str, &str)> = extracted
            .iter()
            .filter(|site| site.entity_type == "data_lifecycle")
            .map(|site| (site.role, site.identity_name.as_str()))
            .collect();
        assert_eq!(
            roles,
            [
                ("lifecycle_listener", "slackAssistantRequest.created"),
                ("lifecycle_producer", "slackAssistantRequest.created"),
            ]
        );
        Ok(())
    }
}
