//! Deterministic non-symbol facts that join runtime workflows across dynamic
//! boundaries. Extraction records source-local sites first; resolution later
//! groups those sites under snapshot-canonical entities.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};
use serde_json::json;

/// Bump whenever deterministic extraction semantics change in a way that
/// requires unchanged files to be parsed again.
pub const EXTRACTION_VERSION: &str = "6";

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

struct GeneralSiteSpec {
    entity_type: &'static str,
    role: &'static str,
    identity_kind: &'static str,
    identity_name: String,
    identity_start: u32,
    span: Span,
    target: Option<(String, u32)>,
    extractor: &'static str,
    provenance: &'static str,
    detail: serde_json::Value,
}

struct EntityVisitor {
    sites: Vec<EntitySite>,
    // Deliberately file-global and single-pass: this is a cheap static-string
    // approximation, not scope-aware constant evaluation or declaration
    // hoisting. Confidence remains `likely` where it contributes identity.
    static_strings: HashMap<String, String>,
    exported: HashSet<String>,
}

#[derive(Debug)]
struct TypeReference {
    name: String,
    start: u32,
    end: u32,
}

#[derive(Default)]
struct TypeReferenceVisitor {
    references: Vec<TypeReference>,
    bound_names: Vec<HashSet<String>>,
}

impl<'a> Visit<'a> for TypeReferenceVisitor {
    fn visit_ts_type_reference(&mut self, reference: &TSTypeReference<'a>) {
        let name = reference.type_name.to_string();
        if !is_builtin_contract_wrapper(&name) && !self.is_bound(&name) {
            self.references.push(TypeReference {
                name,
                start: reference.type_name.span().start,
                end: reference.type_name.span().end,
            });
        }
        oxc_ast_visit::walk::walk_ts_type_reference(self, reference);
    }

    fn visit_ts_function_type(&mut self, function: &TSFunctionType<'a>) {
        self.with_type_parameters(function.type_parameters.as_deref(), |visitor| {
            oxc_ast_visit::walk::walk_ts_function_type(visitor, function);
        });
    }

    fn visit_ts_constructor_type(&mut self, constructor: &TSConstructorType<'a>) {
        self.with_type_parameters(constructor.type_parameters.as_deref(), |visitor| {
            oxc_ast_visit::walk::walk_ts_constructor_type(visitor, constructor);
        });
    }

    fn visit_ts_mapped_type(&mut self, mapped: &TSMappedType<'a>) {
        self.with_bound_names([mapped.key.name.as_str()], |visitor| {
            oxc_ast_visit::walk::walk_ts_mapped_type(visitor, mapped);
        });
    }
}

impl TypeReferenceVisitor {
    fn with_type_parameters<'a>(
        &mut self,
        parameters: Option<&TSTypeParameterDeclaration<'a>>,
        visit: impl FnOnce(&mut Self),
    ) {
        self.with_bound_names(
            parameters
                .into_iter()
                .flat_map(|declaration| declaration.params.iter())
                .map(|parameter| parameter.name.name.as_str()),
            visit,
        );
    }

    fn with_bound_names<'a>(
        &mut self,
        names: impl IntoIterator<Item = &'a str>,
        visit: impl FnOnce(&mut Self),
    ) {
        let names: HashSet<String> = names.into_iter().map(str::to_string).collect();
        self.bound_names.push(names);
        visit(self);
        self.bound_names.pop();
    }

    fn is_bound(&self, name: &str) -> bool {
        self.bound_names
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
    }
}

impl EntityVisitor {
    fn push_general(&mut self, site: GeneralSiteSpec) {
        self.sites.push(EntitySite {
            plane: "general",
            entity_type: site.entity_type,
            role: site.role,
            identity_kind: site.identity_kind,
            identity_name: site.identity_name,
            identity_start: site.identity_start,
            target_name: site.target.as_ref().map(|(name, _)| name.clone()),
            target_start: site.target.map(|(_, start)| start),
            span_start: site.span.start,
            span_end: site.span.end,
            extractor: site.extractor,
            provenance: site.provenance,
            confidence: "likely",
            detail: site.detail,
        });
    }

    fn push_contract_declaration(
        &mut self,
        entity_type: &'static str,
        name: &str,
        name_span: Span,
        declaration_span: Span,
        trust: (&'static str, &'static str),
        detail: serde_json::Value,
    ) {
        self.sites.push(EntitySite {
            plane: "contract",
            entity_type,
            role: "contract_declaration",
            identity_kind: "reference",
            identity_name: name.to_string(),
            identity_start: name_span.start,
            target_name: None,
            target_start: None,
            span_start: declaration_span.start,
            span_end: declaration_span.end,
            extractor: "contract-declaration",
            provenance: trust.0,
            confidence: trust.1,
            detail,
        });
    }

    fn push_contract_references(
        &mut self,
        references: Vec<TypeReference>,
        role: &'static str,
        owner: &str,
        owner_kind: &'static str,
    ) {
        for reference in references {
            self.sites.push(EntitySite {
                plane: "contract",
                entity_type: "reference",
                role,
                identity_kind: "reference",
                identity_name: reference.name,
                identity_start: reference.start,
                target_name: None,
                target_start: None,
                span_start: reference.start,
                span_end: reference.end,
                extractor: "typescript-contract-reference",
                provenance: "type-syntax",
                confidence: "certain",
                detail: json!({ "owner": owner, "ownerKind": owner_kind }),
            });
        }
    }

    fn extract_function_contracts(
        &mut self,
        owner: &str,
        owner_kind: &'static str,
        function: &Function<'_>,
        outer_type_parameters: Option<&TSTypeParameterDeclaration<'_>>,
    ) {
        let bound_names = type_parameter_names(outer_type_parameters)
            .chain(type_parameter_names(function.type_parameters.as_deref()))
            .collect::<HashSet<_>>();
        for (index, parameter) in function.params.items.iter().enumerate() {
            if let Some(annotation) = &parameter.type_annotation {
                let before = self.sites.len();
                self.push_contract_references(
                    collect_type_references(annotation, &bound_names),
                    "parameter_contract",
                    owner,
                    owner_kind,
                );
                for site in &mut self.sites[before..] {
                    site.detail["parameterIndex"] = json!(index);
                }
            }
        }
        if let Some(rest) = &function.params.rest
            && let Some(annotation) = &rest.type_annotation
        {
            self.push_contract_references(
                collect_type_references(annotation, &bound_names),
                "parameter_contract",
                owner,
                owner_kind,
            );
        }
        if let Some(annotation) = &function.return_type {
            self.push_contract_references(
                collect_type_references(annotation, &bound_names),
                "return_contract",
                owner,
                owner_kind,
            );
        }
    }

    fn extract_arrow_contracts(&mut self, owner: &str, arrow: &ArrowFunctionExpression<'_>) {
        let bound_names = type_parameter_names(arrow.type_parameters.as_deref()).collect();
        for (index, parameter) in arrow.params.items.iter().enumerate() {
            if let Some(annotation) = &parameter.type_annotation {
                let before = self.sites.len();
                self.push_contract_references(
                    collect_type_references(annotation, &bound_names),
                    "parameter_contract",
                    owner,
                    "exported_arrow",
                );
                for site in &mut self.sites[before..] {
                    site.detail["parameterIndex"] = json!(index);
                }
            }
        }
        if let Some(annotation) = &arrow.return_type {
            self.push_contract_references(
                collect_type_references(annotation, &bound_names),
                "return_contract",
                owner,
                "exported_arrow",
            );
        }
    }

    fn identity(&self, expression: &Expression<'_>) -> Option<(&'static str, String, u32)> {
        match expression {
            Expression::Identifier(identifier) => Some((
                "reference",
                identifier.name.to_string(),
                identifier.span.start,
            )),
            Expression::StaticMemberExpression(member)
                if member.property.name == "name"
                    && matches!(&member.object, Expression::Identifier(_)) =>
            {
                let Expression::Identifier(identifier) = &member.object else {
                    unreachable!("guard requires an identifier")
                };
                Some((
                    "reference",
                    identifier.name.to_string(),
                    identifier.span.start,
                ))
            }
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
        let Some(config) = first_object_argument(call) else {
            return;
        };
        let Some(identifier) = object_value(config, "universalIdentifier") else {
            return;
        };
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
            target_start: target.as_ref().map(|(_, start)| *start),
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
        let Some(event_name_expression) = object_value(settings, "eventName") else {
            return;
        };
        let Some(event_name) = static_string(event_name_expression, &self.static_strings) else {
            return;
        };
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type: "data_lifecycle",
            role: "lifecycle_listener",
            identity_kind: "literal",
            identity_name: event_name,
            identity_start: event_name_expression.span().start,
            target_name: target.as_ref().map(|(name, _)| name.clone()),
            target_start: target.as_ref().map(|(_, start)| *start),
            span_start: settings.span.start,
            span_end: settings.span.end,
            extractor: "twenty-database-event-trigger",
            provenance: "framework-pattern",
            confidence: "likely",
            detail: json!({ "source": "databaseEventTriggerSettings.eventName" }),
        });
    }

    fn extract_mutation(&mut self, call: &CallExpression<'_>) {
        let Expression::StaticMemberExpression(member) = &call.callee else {
            return;
        };
        if member.property.name != "mutation" {
            return;
        }
        let Some(mutation) = first_object_argument(call) else {
            return;
        };
        for property in &mutation.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                continue;
            };
            let Some(operation) = property.key.static_name() else {
                continue;
            };
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

    fn extract_job_call(&mut self, call: &CallExpression<'_>) {
        let Expression::StaticMemberExpression(member) = &call.callee else {
            return;
        };
        let method = member.property.name.as_str();
        if !matches!(
            method,
            "add" | "addBulk" | "addCron" | "enqueue" | "publish" | "schedule"
        ) {
            return;
        }
        let Some(object) = member_path(&member.object) else {
            return;
        };
        let object_lower = object.to_ascii_lowercase();
        if !["queue", "job", "worker", "cron", "schedul", "producer"]
            .iter()
            .any(|part| object_lower.contains(part))
        {
            return;
        }
        let identity_expression = if method == "addCron" {
            first_object_argument(call).and_then(|options| object_value(options, "jobName"))
        } else {
            call.arguments.first().and_then(Argument::as_expression)
        };
        let Some(identity_expression) = identity_expression else {
            return;
        };
        let Some((identity_kind, identity_name, identity_start)) =
            self.identity(identity_expression)
        else {
            return;
        };
        let scheduled_handler = (method == "schedule" || object_lower.contains("cron"))
            .then(|| call.arguments.get(1).and_then(Argument::as_expression))
            .flatten()
            .and_then(Self::target);
        let role = if scheduled_handler.is_some() {
            "job_handler"
        } else {
            "job_producer"
        };
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type: "job",
            role,
            identity_kind,
            identity_name,
            identity_start,
            target_name: scheduled_handler.as_ref().map(|(name, _)| name.clone()),
            target_start: scheduled_handler.map(|(_, start)| start),
            span_start: call.span.start,
            span_end: call.span.end,
            extractor: "queue-cron-call",
            provenance: "runtime-api-pattern",
            confidence: "likely",
            detail: json!({ "object": object, "method": method }),
        });
    }

    fn extract_general_call(&mut self, call: &CallExpression<'_>) {
        let callee_name = match &call.callee {
            Expression::Identifier(identifier) => identifier.name.as_str(),
            Expression::StaticMemberExpression(member) => member.property.name.as_str(),
            _ => return,
        };
        if !is_general_callee(callee_name) {
            return;
        }
        let callee_path = member_path(&call.callee);

        if let Some(path) = callee_path.as_deref()
            && let Some(method) = http_method(callee_name)
            && path.split('.').any(is_router_holder)
            && let Some(path_expression) = call.arguments.first().and_then(Argument::as_expression)
            && let Some(route_path) = static_string(path_expression, &self.static_strings)
        {
            let target = call
                .arguments
                .iter()
                .rev()
                .find_map(Argument::as_expression)
                .and_then(Self::target);
            self.push_general(GeneralSiteSpec {
                entity_type: "route",
                role: "route_handler",
                identity_kind: "literal",
                identity_name: format!("{method} {}", normalize_route_path(&route_path)),
                identity_start: path_expression.span().start,
                span: call.span,
                target,
                extractor: "http-router-call",
                provenance: "routing-api-pattern",
                detail: json!({ "callee": path }),
            });
        }

        if matches!(callee_name, "query" | "mutation" | "subscription")
            && let Some(path) = callee_path.as_deref()
            && path.to_ascii_lowercase().contains("client")
            && let Some(config) = first_object_argument(call)
            && !is_graphql_options_object(config)
        {
            for property in &config.properties {
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    continue;
                };
                let Some(operation) = property.key.static_name() else {
                    continue;
                };
                self.push_general(GeneralSiteSpec {
                    entity_type: "graphql_operation",
                    role: "graphql_operation",
                    identity_kind: "literal",
                    identity_name: format!("{callee_name}:{operation}"),
                    identity_start: property.key.span().start,
                    span: property.span,
                    target: None,
                    extractor: "graphql-client-operation",
                    provenance: "graphql-api-pattern",
                    detail: json!({ "operationType": callee_name, "operation": operation }),
                });
            }
        }

        if matches!(callee_name, "get" | "require" | "getEnv" | "requireEnv")
            && callee_path.as_deref().is_some_and(|path| {
                matches!(path, "Deno.env.get" | "env.get" | "getEnv" | "requireEnv")
            })
            && let Some(expression) = call.arguments.first().and_then(Argument::as_expression)
            && let Some(name) = static_string(expression, &self.static_strings)
        {
            self.push_general(GeneralSiteSpec {
                entity_type: "environment_variable",
                role: "environment_read",
                identity_kind: "literal",
                identity_name: name,
                identity_start: expression.span().start,
                span: call.span,
                target: None,
                extractor: "environment-api-call",
                provenance: "environment-api-pattern",
                detail: json!({ "callee": callee_path }),
            });
        }

        if matches!(callee_name, "get" | "require")
            && callee_path.as_deref().is_some_and(is_config_api_path)
            && let Some(expression) = call.arguments.first().and_then(Argument::as_expression)
            && let Some(name) = static_string(expression, &self.static_strings)
        {
            self.push_general(GeneralSiteSpec {
                entity_type: "config_key",
                role: "config_read",
                identity_kind: "literal",
                identity_name: name,
                identity_start: expression.span().start,
                span: call.span,
                target: None,
                extractor: "configuration-api-call",
                provenance: "configuration-api-pattern",
                detail: json!({ "callee": callee_path }),
            });
        }

        if is_feature_flag_callee(callee_name)
            && let Some(expression) = call.arguments.first().and_then(Argument::as_expression)
            && let Some((kind, name, start)) = self.identity(expression).or_else(|| {
                member_path(expression).map(|name| ("literal", name, expression.span().start))
            })
        {
            self.push_general(GeneralSiteSpec {
                entity_type: "feature_flag",
                role: "feature_flag_check",
                identity_kind: kind,
                identity_name: name,
                identity_start: start,
                span: call.span,
                target: None,
                extractor: "feature-flag-call",
                provenance: "feature-flag-api-pattern",
                detail: json!({ "callee": callee_path }),
            });
        }

        if let Some((resource, access)) = database_call(call, &self.static_strings) {
            self.push_general(GeneralSiteSpec {
                entity_type: "database_resource",
                role: match access {
                    "write" => "database_write",
                    "read" => "database_read",
                    _ => "database_acquire",
                },
                identity_kind: "literal",
                identity_name: resource,
                identity_start: call.callee.span().start,
                span: call.span,
                target: None,
                extractor: "database-api-call",
                provenance: "database-api-pattern",
                detail: json!({ "callee": callee_path, "access": access }),
            });
        }

        if is_external_call(callee_path.as_deref(), callee_name)
            && let Some(expression) = call.arguments.first().and_then(Argument::as_expression)
            && let Some(url) = static_string(expression, &self.static_strings)
            && let Some(host) = url_host(&url)
        {
            self.push_general(GeneralSiteSpec {
                entity_type: "external_host",
                role: "external_host_call",
                identity_kind: "literal",
                identity_name: host,
                identity_start: expression.span().start,
                span: call.span,
                target: None,
                extractor: "static-url-call",
                provenance: "network-api-pattern",
                detail: json!({ "callee": callee_path, "url": url }),
            });
        }
    }

    fn extract_class_endpoints(&mut self, class: &Class<'_>) {
        let controller_prefix = class
            .decorators
            .iter()
            .find_map(|decorator| {
                decorator_static_argument(decorator, "Controller", &self.static_strings)
            })
            .unwrap_or_default();
        for element in &class.body.body {
            let ClassElement::MethodDefinition(method) = element else {
                continue;
            };
            let Some(method_name) = method.key.static_name() else {
                continue;
            };
            for decorator in &method.decorators {
                let Some((decorator_name, call)) = decorator_call(decorator) else {
                    continue;
                };
                let terminal = decorator_name.rsplit('.').next().unwrap_or(&decorator_name);
                if let Some(http_method) = http_method(terminal) {
                    let method_path = call
                        .arguments
                        .first()
                        .and_then(Argument::as_expression)
                        .and_then(|expression| static_string(expression, &self.static_strings))
                        .unwrap_or_default();
                    let route_path = join_route_path(&controller_prefix, &method_path);
                    self.push_general(GeneralSiteSpec {
                        entity_type: "route",
                        role: "route_handler",
                        identity_kind: "literal",
                        identity_name: format!("{http_method} {route_path}"),
                        identity_start: decorator.span.start,
                        span: decorator.span,
                        target: None,
                        extractor: "http-route-decorator",
                        provenance: "routing-decorator-pattern",
                        detail: json!({
                            "class": class.id.as_ref().map(|identifier| identifier.name.as_str()),
                            "method": method_name,
                            "decorator": terminal,
                        }),
                    });
                }
                if matches!(terminal, "Query" | "Mutation" | "Subscription") {
                    let operation_name = call
                        .arguments
                        .first()
                        .and_then(Argument::as_expression)
                        .and_then(|expression| static_string(expression, &self.static_strings))
                        .unwrap_or_else(|| method_name.to_string());
                    self.push_general(GeneralSiteSpec {
                        entity_type: "graphql_operation",
                        role: "graphql_handler",
                        identity_kind: "literal",
                        identity_name: format!(
                            "{}:{operation_name}",
                            terminal.to_ascii_lowercase()
                        ),
                        identity_start: decorator.span.start,
                        span: decorator.span,
                        target: None,
                        extractor: "graphql-operation-decorator",
                        provenance: "graphql-decorator-pattern",
                        detail: json!({ "method": method_name, "decorator": terminal }),
                    });
                }
            }
        }
    }

    fn extract_provider(&mut self, object: &ObjectExpression<'_>) {
        let Some(token) = object_value(object, "provide") else {
            return;
        };
        let implementation = ["useClass", "useFactory", "useExisting"]
            .into_iter()
            .find_map(|field| object_value(object, field).map(|value| (field, value)));
        let Some((binding, implementation)) = implementation else {
            return;
        };
        let Some((identity_kind, identity_name, identity_start)) = self.identity(token) else {
            return;
        };
        let target = Self::target(implementation);
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type: "di_token",
            role: "provider",
            identity_kind,
            identity_name,
            identity_start,
            target_name: target.as_ref().map(|(name, _)| name.clone()),
            target_start: target.map(|(_, start)| start),
            span_start: object.span.start,
            span_end: object.span.end,
            extractor: "di-provider-object",
            provenance: "provider-object-pattern",
            confidence: "likely",
            detail: json!({ "binding": binding }),
        });
    }

    fn extract_decorator(&mut self, decorator: &Decorator<'_>) {
        let Expression::CallExpression(call) = &decorator.expression else {
            return;
        };
        let Expression::Identifier(callee) = &call.callee else {
            return;
        };
        let Some(identity_expression) = call.arguments.first().and_then(Argument::as_expression)
        else {
            return;
        };
        let Some((identity_kind, identity_name, identity_start)) =
            self.identity(identity_expression)
        else {
            return;
        };
        let (entity_type, role, extractor) = match callee.name.as_str() {
            "Inject" => ("di_token", "injection_site", "di-inject-decorator"),
            "Cron" | "Interval" | "Timeout" | "Process" | "Processor" | "Job" => {
                ("job", "job_handler", "job-handler-decorator")
            }
            _ => return,
        };
        self.sites.push(EntitySite {
            plane: "runtime",
            entity_type,
            role,
            identity_kind,
            identity_name,
            identity_start,
            target_name: None,
            target_start: None,
            span_start: decorator.span.start,
            span_end: decorator.span.end,
            extractor,
            provenance: "decorator-pattern",
            confidence: "likely",
            detail: json!({ "decorator": callee.name.as_str() }),
        });
    }
}

impl<'a> Visit<'a> for EntityVisitor {
    fn visit_variable_declarator(&mut self, declaration: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(identifier) = &declaration.id
            && let Some(initializer) = &declaration.init
        {
            if let Some(value) = static_string(initializer, &self.static_strings) {
                self.static_strings
                    .insert(identifier.name.to_string(), value);
            }
            if self.exported.contains(identifier.name.as_str())
                && let Expression::ArrowFunctionExpression(arrow) = initializer
            {
                self.extract_arrow_contracts(identifier.name.as_str(), arrow);
            }
            if let Some(callee) = validation_schema_callee(initializer, identifier.name.as_str()) {
                self.push_contract_declaration(
                    "schema",
                    identifier.name.as_str(),
                    identifier.span,
                    declaration.span,
                    ("validation-schema-pattern", "likely"),
                    json!({
                        "declarationKind": "validation_schema",
                        "callee": callee,
                        "exported": self.exported.contains(identifier.name.as_str()),
                    }),
                );
            }
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, declaration);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
        if let Some(identifier) = &function.id
            && self.exported.contains(identifier.name.as_str())
        {
            self.extract_function_contracts(
                identifier.name.as_str(),
                "exported_function",
                function,
                None,
            );
        }
        oxc_ast_visit::walk::walk_function(self, function, flags);
    }

    fn visit_ts_interface_declaration(&mut self, declaration: &TSInterfaceDeclaration<'a>) {
        let name = declaration.id.name.as_str();
        self.push_contract_declaration(
            "interface",
            name,
            declaration.id.span,
            declaration.span,
            ("type-declaration", "certain"),
            json!({
                "declarationKind": "interface",
                "exported": self.exported.contains(name),
            }),
        );
        let mut collector = type_reference_visitor(declaration.type_parameters.as_deref());
        oxc_ast_visit::walk::walk_ts_interface_declaration(&mut collector, declaration);
        self.push_contract_references(
            collector.references,
            "contract_reference",
            name,
            "interface",
        );
        oxc_ast_visit::walk::walk_ts_interface_declaration(self, declaration);
    }

    fn visit_ts_type_alias_declaration(&mut self, declaration: &TSTypeAliasDeclaration<'a>) {
        let name = declaration.id.name.as_str();
        self.push_contract_declaration(
            "type_alias",
            name,
            declaration.id.span,
            declaration.span,
            ("type-declaration", "certain"),
            json!({
                "declarationKind": "type_alias",
                "exported": self.exported.contains(name),
            }),
        );
        let mut collector = type_reference_visitor(declaration.type_parameters.as_deref());
        oxc_ast_visit::walk::walk_ts_type_alias_declaration(&mut collector, declaration);
        self.push_contract_references(
            collector.references,
            "contract_reference",
            name,
            "type_alias",
        );
        oxc_ast_visit::walk::walk_ts_type_alias_declaration(self, declaration);
    }

    fn visit_ts_enum_declaration(&mut self, declaration: &TSEnumDeclaration<'a>) {
        let name = declaration.id.name.as_str();
        self.push_contract_declaration(
            "enum",
            name,
            declaration.id.span,
            declaration.span,
            ("type-declaration", "certain"),
            json!({
                "declarationKind": "enum",
                "exported": self.exported.contains(name),
                "const": declaration.r#const,
            }),
        );
        oxc_ast_visit::walk::walk_ts_enum_declaration(self, declaration);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        self.extract_class_endpoints(class);
        if let Some(identifier) = &class.id {
            let name = identifier.name.as_str();
            if class_is_contract_schema(class) {
                self.push_contract_declaration(
                    "schema",
                    name,
                    identifier.span,
                    class.span,
                    ("dto-schema-pattern", "likely"),
                    json!({
                        "declarationKind": "dto_schema",
                        "exported": self.exported.contains(name),
                    }),
                );
            }
            if self.exported.contains(name) {
                for element in &class.body.body {
                    if let ClassElement::MethodDefinition(method) = element
                        && method.accessibility != Some(TSAccessibility::Private)
                        && let Some(method_name) = method.key.static_name()
                    {
                        self.extract_function_contracts(
                            &format!("{name}.{method_name}"),
                            "exported_method",
                            &method.value,
                            class.type_parameters.as_deref(),
                        );
                    }
                }
            }
        }
        oxc_ast_visit::walk::walk_class(self, class);
    }

    fn visit_object_expression(&mut self, object: &ObjectExpression<'a>) {
        self.extract_provider(object);
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
        self.extract_job_call(call);
        self.extract_general_call(call);
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }

    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if is_process_env(&member.object) {
            self.push_general(GeneralSiteSpec {
                entity_type: "environment_variable",
                role: "environment_read",
                identity_kind: "literal",
                identity_name: member.property.name.to_string(),
                identity_start: member.property.span.start,
                span: member.span,
                target: None,
                extractor: "process-env-member",
                provenance: "environment-syntax",
                detail: json!({ "object": "process.env" }),
            });
        }
        oxc_ast_visit::walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        if is_process_env(&member.object)
            && let Some(name) = static_string(&member.expression, &self.static_strings)
        {
            self.push_general(GeneralSiteSpec {
                entity_type: "environment_variable",
                role: "environment_read",
                identity_kind: "literal",
                identity_name: name,
                identity_start: member.expression.span().start,
                span: member.span,
                target: None,
                extractor: "process-env-computed-member",
                provenance: "environment-syntax",
                detail: json!({ "object": "process.env" }),
            });
        }
        oxc_ast_visit::walk::walk_computed_member_expression(self, member);
    }

    fn visit_new_expression(&mut self, expression: &NewExpression<'a>) {
        if matches!(
            &expression.callee,
            Expression::Identifier(identifier) if matches!(identifier.name.as_str(), "Worker" | "QueueWorker")
        ) && let Some(identity_expression) = expression
            .arguments
            .first()
            .and_then(Argument::as_expression)
            && let Some((identity_kind, identity_name, identity_start)) =
                self.identity(identity_expression)
        {
            let target = expression
                .arguments
                .get(1)
                .and_then(Argument::as_expression)
                .and_then(Self::target);
            self.sites.push(EntitySite {
                plane: "runtime",
                entity_type: "job",
                role: "job_handler",
                identity_kind,
                identity_name,
                identity_start,
                target_name: target.as_ref().map(|(name, _)| name.clone()),
                target_start: target.map(|(_, start)| start),
                span_start: expression.span.start,
                span_end: expression.span.end,
                extractor: "queue-worker-constructor",
                provenance: "runtime-api-pattern",
                confidence: "likely",
                detail: json!({ "constructor": "Worker" }),
            });
        }
        oxc_ast_visit::walk::walk_new_expression(self, expression);
    }

    fn visit_decorator(&mut self, decorator: &Decorator<'a>) {
        if let Some((name, start)) = decorator_reference(&decorator.expression) {
            self.sites.push(EntitySite {
                plane: "contract",
                entity_type: "decorator",
                role: "decorator_use",
                identity_kind: "reference",
                identity_name: name,
                identity_start: start,
                target_name: None,
                target_start: None,
                span_start: decorator.span.start,
                span_end: decorator.span.end,
                extractor: "decorator-contract",
                provenance: "decorator-syntax",
                confidence: "certain",
                detail: json!({ "syntax": "decorator" }),
            });
        }
        self.extract_decorator(decorator);
        oxc_ast_visit::walk::walk_decorator(self, decorator);
    }
}

/// Extract source-local entity evidence. Recognizers are deliberately narrow:
/// each emitted site names its framework/convention and remains below
/// `certain` unless syntax alone proves the relationship.
pub fn extract(program: &Program<'_>, exported: &HashSet<String>) -> Vec<EntitySite> {
    let mut visitor = EntityVisitor {
        sites: Vec::new(),
        static_strings: HashMap::new(),
        exported: exported.clone(),
    };
    visitor.visit_program(program);
    visitor.sites
}

fn collect_type_references<'a>(
    annotation: &'a TSTypeAnnotation<'a>,
    bound_names: &HashSet<String>,
) -> Vec<TypeReference> {
    let mut visitor = TypeReferenceVisitor {
        references: Vec::new(),
        bound_names: vec![bound_names.clone()],
    };
    visitor.visit_ts_type_annotation(annotation);
    visitor.references
}

fn type_reference_visitor(
    parameters: Option<&TSTypeParameterDeclaration<'_>>,
) -> TypeReferenceVisitor {
    TypeReferenceVisitor {
        references: Vec::new(),
        bound_names: vec![type_parameter_names(parameters).collect()],
    }
}

fn type_parameter_names<'a>(
    parameters: Option<&'a TSTypeParameterDeclaration<'a>>,
) -> impl Iterator<Item = String> + 'a {
    parameters
        .into_iter()
        .flat_map(|declaration| declaration.params.iter())
        .map(|parameter| parameter.name.name.to_string())
}

fn is_builtin_contract_wrapper(name: &str) -> bool {
    matches!(
        name,
        "Array"
            | "ReadonlyArray"
            | "Promise"
            | "PromiseLike"
            | "Record"
            | "Partial"
            | "Required"
            | "Readonly"
            | "Pick"
            | "Omit"
            | "Exclude"
            | "Extract"
            | "NonNullable"
            | "Parameters"
            | "ConstructorParameters"
            | "ReturnType"
            | "InstanceType"
            | "Awaited"
            | "Map"
            | "ReadonlyMap"
            | "WeakMap"
            | "Set"
            | "ReadonlySet"
            | "WeakSet"
            | "Date"
            | "Error"
            | "RegExp"
    )
}

fn decorator_reference(expression: &Expression<'_>) -> Option<(String, u32)> {
    match expression {
        Expression::Identifier(identifier) => {
            Some((identifier.name.to_string(), identifier.span.start))
        }
        Expression::CallExpression(call) => match &call.callee {
            Expression::Identifier(identifier) => {
                Some((identifier.name.to_string(), identifier.span.start))
            }
            expression => Some((member_path(expression)?, expression.span().start)),
        },
        expression => Some((member_path(expression)?, expression.span().start)),
    }
}

fn validation_schema_callee(expression: &Expression<'_>, binding: &str) -> Option<String> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };
    let path = member_path(&call.callee)?;
    let recognized = matches!(
        path.as_str(),
        "z.object"
            | "z.strictObject"
            | "yup.object"
            | "Joi.object"
            | "Type.Object"
            | "v.object"
            | "valibot.object"
    ) || (binding.to_ascii_lowercase().ends_with("schema")
        && matches!(
            path.rsplit('.').next(),
            Some("object" | "strictObject" | "createSchema" | "defineSchema" | "schema")
        ));
    recognized.then_some(path)
}

fn class_is_contract_schema(class: &Class<'_>) -> bool {
    let Some(identifier) = &class.id else {
        return false;
    };
    let lower = identifier.name.to_ascii_lowercase();
    if lower.ends_with("dto") {
        return true;
    }
    class.decorators.iter().any(|decorator| {
        decorator_reference(&decorator.expression).is_some_and(|(name, _)| {
            matches!(
                name.rsplit('.').next(),
                Some("InputType" | "ObjectType" | "ArgsType" | "Schema")
            )
        })
    }) || class.body.body.iter().any(|element| match element {
        ClassElement::PropertyDefinition(property) => property.decorators.iter().any(|decorator| {
            decorator_reference(&decorator.expression).is_some_and(|(name, _)| {
                name.rsplit('.').next().is_some_and(|name| {
                    name.starts_with("Is")
                        || name.starts_with("Validate")
                        || matches!(name, "ApiProperty" | "Field")
                })
            })
        }),
        _ => false,
    })
}

fn first_object_argument<'a>(call: &'a CallExpression<'a>) -> Option<&'a ObjectExpression<'a>> {
    match call.arguments.first()? {
        Argument::ObjectExpression(object) => Some(object),
        _ => None,
    }
}

fn object_value<'a>(object: &'a ObjectExpression<'a>, name: &str) -> Option<&'a Expression<'a>> {
    object.properties.iter().find_map(|property| {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            return None;
        };
        (property.key.static_name().as_deref() == Some(name)).then_some(&property.value)
    })
}

fn http_method(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "patch" => Some("PATCH"),
        "delete" => Some("DELETE"),
        "options" => Some("OPTIONS"),
        "head" => Some("HEAD"),
        "all" => Some("ALL"),
        _ => None,
    }
}

fn is_router_holder(segment: &str) -> bool {
    let segment = segment.to_ascii_lowercase();
    matches!(segment.as_str(), "app" | "router" | "server" | "route") || segment.ends_with("router")
}

fn is_graphql_options_object(object: &ObjectExpression<'_>) -> bool {
    object.properties.iter().any(|property| {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            return false;
        };
        property.key.static_name().is_some_and(|name| {
            matches!(
                name.as_ref(),
                "query"
                    | "mutation"
                    | "subscription"
                    | "variables"
                    | "fetchPolicy"
                    | "errorPolicy"
                    | "context"
                    | "refetchQueries"
                    | "awaitRefetchQueries"
                    | "optimisticResponse"
                    | "update"
                    | "onCompleted"
                    | "onError"
                    | "pollInterval"
                    | "notifyOnNetworkStatusChange"
                    | "returnPartialData"
                    | "skip"
                    | "client"
                    | "ssr"
            )
        })
    })
}

fn is_config_api_path(path: &str) -> bool {
    let mut segments = path.rsplit('.');
    let Some(method) = segments.next() else {
        return false;
    };
    if !matches!(method, "get" | "require") {
        return false;
    }
    segments.next().is_some_and(|receiver| {
        let receiver = receiver.to_ascii_lowercase();
        receiver == "config" || receiver.ends_with("configservice")
    })
}

fn normalize_route_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".to_string()
    } else {
        format!("/{}", trimmed.trim_matches('/'))
    }
}

fn join_route_path(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let path = path.trim_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => "/".to_string(),
        (false, true) => format!("/{prefix}"),
        (true, false) => format!("/{path}"),
        (false, false) => format!("/{prefix}/{path}"),
    }
}

fn decorator_call<'a>(decorator: &'a Decorator<'a>) -> Option<(String, &'a CallExpression<'a>)> {
    let Expression::CallExpression(call) = &decorator.expression else {
        return None;
    };
    let name = member_path(&call.callee)?;
    Some((name, call))
}

fn decorator_static_argument(
    decorator: &Decorator<'_>,
    expected: &str,
    constants: &HashMap<String, String>,
) -> Option<String> {
    let (name, call) = decorator_call(decorator)?;
    if name.rsplit('.').next() != Some(expected) {
        return None;
    }
    match call.arguments.first() {
        None => Some(String::new()),
        Some(argument) => static_string(argument.as_expression()?, constants),
    }
}

fn is_feature_flag_callee(name: &str) -> bool {
    matches!(
        name,
        "isEnabled"
            | "isFeatureEnabled"
            | "isFeatureFlagEnabled"
            | "hasFeature"
            | "useFeatureFlag"
            | "featureFlag"
    )
}

fn is_general_callee(name: &str) -> bool {
    http_method(name).is_some()
        || matches!(
            name,
            "query"
                | "mutation"
                | "subscription"
                | "require"
                | "getEnv"
                | "requireEnv"
                | "getRepository"
                | "getModel"
                | "create"
                | "createMany"
                | "insert"
                | "insertMany"
                | "save"
                | "update"
                | "updateMany"
                | "upsert"
                | "deleteMany"
                | "remove"
                | "find"
                | "findOne"
                | "findFirst"
                | "findMany"
                | "findUnique"
                | "select"
                | "count"
                | "aggregate"
                | "exists"
                | "fetch"
                | "axios"
                | "got"
                | "request"
        )
        || is_feature_flag_callee(name)
}

fn is_process_env(expression: &Expression<'_>) -> bool {
    matches!(
        expression,
        Expression::StaticMemberExpression(member)
            if member.property.name == "env"
                && matches!(
                    &member.object,
                    Expression::Identifier(identifier) if identifier.name == "process"
                )
    )
}

fn database_call(
    call: &CallExpression<'_>,
    constants: &HashMap<String, String>,
) -> Option<(String, &'static str)> {
    if let Expression::Identifier(callee) = &call.callee
        && matches!(callee.name.as_str(), "getRepository" | "getModel")
    {
        let resource = call.arguments.first()?.as_expression()?;
        let name = match resource {
            Expression::Identifier(identifier) => identifier.name.to_string(),
            _ => static_string(resource, constants)?,
        };
        return Some((name, "acquire"));
    }
    let Expression::StaticMemberExpression(member) = &call.callee else {
        return None;
    };
    let method = member.property.name.as_str();
    let access = if matches!(
        method,
        "create"
            | "createMany"
            | "insert"
            | "insertMany"
            | "save"
            | "update"
            | "updateMany"
            | "upsert"
            | "delete"
            | "deleteMany"
            | "remove"
    ) {
        "write"
    } else if matches!(
        method,
        "find"
            | "findOne"
            | "findFirst"
            | "findMany"
            | "findUnique"
            | "select"
            | "count"
            | "aggregate"
            | "exists"
    ) {
        "read"
    } else {
        return None;
    };
    let object = member_path(&member.object)?;
    let segments: Vec<&str> = object.split('.').collect();
    if let Some(resource) = segments
        .iter()
        .rev()
        .find_map(|segment| database_holder_resource(segment))
    {
        return Some((resource, access));
    }
    let holder_index = segments
        .iter()
        .position(|segment| is_database_api_segment(segment))?;
    if let Some(resource) = segments
        .get(holder_index + 1..)
        .and_then(|tail| tail.last())
        .filter(|resource| !is_database_api_segment(resource))
    {
        return Some(((*resource).to_string(), access));
    }
    let holder = segments[holder_index].to_ascii_lowercase();
    if !matches!(holder.as_str(), "db" | "database" | "prisma") {
        return None;
    }
    let expression = call.arguments.first()?.as_expression()?;
    let resource = match expression {
        Expression::Identifier(identifier) => identifier.name.to_string(),
        _ => static_string(expression, constants)?,
    };
    (!is_database_api_segment(&resource)).then_some((resource, access))
}

fn database_holder_resource(holder: &str) -> Option<String> {
    ["repository", "repo", "model"]
        .into_iter()
        .find_map(|suffix| {
            let prefix_len = holder.len().checked_sub(suffix.len())?;
            let (prefix, candidate_suffix) = holder.split_at(prefix_len);
            (!prefix.is_empty() && candidate_suffix.eq_ignore_ascii_case(suffix))
                .then(|| lower_first(prefix))
        })
}

fn lower_first(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_lowercase().chain(chars).collect()
}

fn is_database_api_segment(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "db" | "database" | "prisma" | "repository" | "repo" | "model" | "entitymanager"
    )
}

fn is_external_call(path: Option<&str>, name: &str) -> bool {
    name == "fetch"
        || matches!(path, Some("axios" | "got" | "request"))
        || path.is_some_and(|path| {
            let root = path.split('.').next().unwrap_or_default();
            matches!(root, "axios" | "got" | "request")
                && matches!(
                    name,
                    "get" | "post" | "put" | "patch" | "delete" | "request"
                )
        })
}

fn url_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = if authority.starts_with('[') {
        authority.split(']').next()?.trim_start_matches('[')
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
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

fn member_path(expression: &Expression<'_>) -> Option<String> {
    match expression {
        Expression::ThisExpression(_) => Some("this".into()),
        Expression::Identifier(identifier) => Some(identifier.name.to_string()),
        Expression::StaticMemberExpression(member) => Some(format!(
            "{}.{}",
            member_path(&member.object)?,
            member.property.name
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
