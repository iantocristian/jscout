use oxc_ast::AstKind;
use oxc_parser::ParserReturn;
use oxc_semantic::Semantic;
use oxc_span::{GetSpan, Span};
use oxc_syntax::module_record::{
    ExportExportName, ExportImportName, ExportLocalName, ImportImportName,
};
use oxc_syntax::symbol::SymbolFlags;

#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub name: String,
    pub kind: String,
    pub start: u32,
    pub end: u32,
    pub decl_start: u32,
    pub decl_end: u32,
    pub scope_chain: String,
    pub exported: bool,
}

#[derive(Debug, Clone)]
pub struct ImportRow {
    pub local: String,
    /// "default" | "*" | exported name at the source module
    pub imported: String,
    pub request: String,
}

#[derive(Debug, Clone)]
pub struct ExportRow {
    /// "*" for star re-exports without alias
    pub export_name: String,
    pub local_name: Option<String>,
    pub from_request: Option<String>,
    pub from_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RefRow {
    pub span_start: u32,
    pub kind: &'static str,       // call | render | extend | use | reexport
    pub confidence: &'static str, // certain (all tier-1 here)
    pub target_request: Option<String>,
    pub target_name: String,
    pub local: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Default)]
pub struct FileGraph {
    pub symbols: Vec<SymbolRow>,
    pub imports: Vec<ImportRow>,
    pub exports: Vec<ExportRow>,
    /// Type/documentary bindings, including declarations imported through a
    /// runtime import. Kept separate so they cannot affect call projection.
    pub contract_imports: Vec<ImportRow>,
    pub contract_exports: Vec<ExportRow>,
    pub refs: Vec<RefRow>,
    /// All module requests (for resolution), including re-export sources.
    pub requests: Vec<String>,
    pub events: Vec<crate::heur::EventSite>,
    pub member_calls: Vec<crate::heur::MemberCall>,
    pub entity_sites: Vec<crate::entity::EntitySite>,
}

pub fn extract(ret: &ParserReturn<'_>, semantic: &Semantic<'_>) -> FileGraph {
    let mut g = FileGraph::default();
    let record = &ret.module_record;
    let heur = crate::heur::extract(&ret.program, semantic);

    // ---- imports ----
    for entry in &record.import_entries {
        let imported = match &entry.import_name {
            ImportImportName::Name(n) => n.name.to_string(),
            ImportImportName::NamespaceObject => "*".to_string(),
            ImportImportName::Default(_) => "default".to_string(),
        };
        let row = ImportRow {
            local: entry.local_name.name.to_string(),
            imported,
            request: entry.module_request.name.to_string(),
        };
        g.contract_imports.push(row.clone());
        if !entry.is_type {
            g.imports.push(row);
        }
    }

    // ---- exports ----
    let mut exported_locals: std::collections::HashSet<String> = Default::default();
    let mut exported_contract_locals: std::collections::HashSet<String> = Default::default();
    for entry in &record.local_export_entries {
        let export_name = match &entry.export_name {
            ExportExportName::Name(n) => n.name.to_string(),
            ExportExportName::Default(_) => "default".to_string(),
            ExportExportName::Null => continue,
        };
        let local_name = match &entry.local_name {
            ExportLocalName::Name(n) => Some(n.name.to_string()),
            ExportLocalName::Default(_) => None,
            ExportLocalName::Null => None,
        };
        if let Some(l) = &local_name {
            exported_contract_locals.insert(l.clone());
        }
        let row = ExportRow {
            export_name,
            local_name,
            from_request: None,
            from_name: None,
        };
        g.contract_exports.push(row.clone());
        if entry.is_type {
            continue;
        }
        if let Some(l) = &row.local_name {
            exported_locals.insert(l.clone());
        }
        g.exports.push(row);
    }
    for entry in &record.indirect_export_entries {
        let Some(request) = &entry.module_request else {
            continue;
        };
        let export_name = match &entry.export_name {
            ExportExportName::Name(n) => n.name.to_string(),
            ExportExportName::Default(_) => "default".to_string(),
            ExportExportName::Null => continue,
        };
        let from_name = match &entry.import_name {
            ExportImportName::Name(n) => n.name.to_string(),
            ExportImportName::All | ExportImportName::AllButDefault => "*".to_string(),
            ExportImportName::Null => continue,
        };
        let row = ExportRow {
            export_name,
            local_name: None,
            from_request: Some(request.name.to_string()),
            from_name: Some(from_name.clone()),
        };
        g.contract_exports.push(row.clone());
        if entry.is_type {
            continue;
        }
        g.exports.push(row);
        g.refs.push(RefRow {
            span_start: entry.span.start,
            kind: "reexport",
            confidence: "certain",
            target_request: Some(request.name.to_string()),
            target_name: from_name,
            local: false,
            detail: None,
        });
    }
    for entry in &record.star_export_entries {
        let Some(request) = &entry.module_request else {
            continue;
        };
        let row = ExportRow {
            export_name: "*".to_string(),
            local_name: None,
            from_request: Some(request.name.to_string()),
            from_name: Some("*".to_string()),
        };
        g.contract_exports.push(row.clone());
        if !entry.is_type {
            g.exports.push(row);
        }
    }
    g.entity_sites = crate::entity::extract(&ret.program, &exported_contract_locals);
    g.requests = record
        .requested_modules
        .keys()
        .map(|s| s.to_string())
        .collect();

    // ---- CommonJS + dynamic-import heuristics ----
    for r in &heur.requires {
        g.imports.push(ImportRow {
            local: r.local.clone(),
            imported: r.imported.clone(),
            request: r.request.clone(),
        });
        g.requests.push(r.request.clone());
    }
    for e in &heur.cjs_exports {
        exported_locals.extend(e.local_name.clone());
        g.exports.push(ExportRow {
            export_name: e.export_name.clone(),
            local_name: e.local_name.clone().or(Some(e.export_name.clone())),
            from_request: None,
            from_name: None,
        });
    }
    for d in &heur.dynamic_imports {
        g.refs.push(RefRow {
            span_start: d.span_start,
            kind: "use",
            confidence: "certain",
            target_request: Some(d.request.clone()),
            target_name: "*".to_string(),
            local: false,
            detail: Some("dynamic import".into()),
        });
        g.requests.push(d.request.clone());
    }
    g.events = heur.events;
    g.member_calls = heur.member_calls;
    for m in &heur.methods {
        g.symbols.push(SymbolRow {
            name: m.name.clone(),
            kind: format!("method:{}", m.class),
            start: m.span_start,
            end: m.span_end,
            decl_start: m.span_start,
            decl_end: m.span_end,
            scope_chain: m.class.clone(),
            exported: false,
        });
    }

    // ---- symbols & references ----
    let scoping = semantic.scoping();
    let nodes = semantic.nodes();
    let import_by_local: std::collections::HashMap<&str, &ImportRow> =
        g.imports.iter().map(|i| (i.local.as_str(), i)).collect();

    let root = scoping.root_scope_id();
    let bindings: Vec<(String, oxc_syntax::symbol::SymbolId)> = scoping
        .get_bindings(root)
        .iter()
        .map(|(name, id)| (name.to_string(), *id))
        .collect();

    let mut refs: Vec<RefRow> = Vec::new();
    for (name, symbol_id) in &bindings {
        let flags = scoping.symbol_flags(*symbol_id);
        let is_import = flags.contains(SymbolFlags::Import);
        let import_row = import_by_local.get(name.as_str());

        if !is_import {
            // type-only symbols (interfaces, type aliases) are erased
            if flags.intersects(SymbolFlags::TypeAlias | SymbolFlags::Interface) {
                continue;
            }
            let span = scoping.symbol_span(*symbol_id);
            let declaration = semantic.symbol_declaration(*symbol_id).kind().span();
            g.symbols.push(SymbolRow {
                name: name.clone(),
                kind: symbol_kind(flags).to_string(),
                start: span.start,
                end: span.end,
                decl_start: declaration.start,
                decl_end: declaration.end,
                scope_chain: String::new(),
                exported: exported_locals.contains(name),
            });
        }

        for ref_id in scoping.get_resolved_reference_ids(*symbol_id) {
            let r = scoping.get_reference(*ref_id);
            let ref_node = nodes.get_node(r.node_id());
            let ref_span = ref_node.kind().span();
            // Skip pure type positions (e.g. `foo: typeof x` in erased land is fine,
            // but `let a: Foo` type refs are noise for the runtime graph).
            if r.flags().is_type_only() {
                continue;
            }
            let (kind, member_prop) = classify_reference(nodes, r.node_id(), ref_span);

            match import_row {
                Some(imp) => {
                    // Through-namespace member access refines the target name.
                    let (target_name, detail) = if imp.imported == "*" {
                        match &member_prop {
                            Some(p) => (p.clone(), Some(format!("via namespace {}", imp.local))),
                            None => ("*".to_string(), Some(format!("namespace {}", imp.local))),
                        }
                    } else {
                        (imp.imported.clone(), None)
                    };
                    refs.push(RefRow {
                        span_start: ref_span.start,
                        kind,
                        confidence: "certain",
                        target_request: Some(imp.request.clone()),
                        target_name,
                        local: false,
                        detail,
                    });
                }
                None if is_import => {} // import binding without record entry (type import)
                None => {
                    refs.push(RefRow {
                        span_start: ref_span.start,
                        kind,
                        confidence: "certain",
                        target_request: None,
                        target_name: name.clone(),
                        local: true,
                        detail: None,
                    });
                }
            }
        }
    }
    g.refs.extend(refs);
    g
}

fn symbol_kind(flags: SymbolFlags) -> &'static str {
    if flags.contains(SymbolFlags::Class) {
        "class"
    } else if flags.contains(SymbolFlags::Function) {
        "function"
    } else if flags.contains(SymbolFlags::ConstVariable) {
        "const"
    } else {
        "var"
    }
}

/// Classify how a reference is used by walking a few ancestors.
/// Returns (kind, member_property_if_namespace_access).
fn classify_reference<'a>(
    nodes: &oxc_semantic::AstNodes<'a>,
    node_id: oxc_syntax::node::NodeId,
    ref_span: Span,
) -> (&'static str, Option<String>) {
    let mut member_prop: Option<String> = None;
    for (depth, anc) in nodes.ancestors(node_id).enumerate() {
        if depth > 4 {
            break;
        }
        match anc.kind() {
            AstKind::StaticMemberExpression(m) if depth == 0 => {
                if m.object.span() == ref_span {
                    member_prop = Some(m.property.name.to_string());
                }
            }
            AstKind::JSXOpeningElement(_) | AstKind::JSXClosingElement(_) => {
                return ("render", member_prop);
            }
            AstKind::CallExpression(c) => {
                if c.callee.span().contains_inclusive(ref_span) {
                    return ("call", member_prop);
                }
                return ("use", member_prop);
            }
            AstKind::NewExpression(n) => {
                if n.callee.span().contains_inclusive(ref_span) {
                    return ("call", member_prop);
                }
                return ("use", member_prop);
            }
            AstKind::Class(c)
                if c.super_class
                    .as_ref()
                    .is_some_and(|s| s.span().contains_inclusive(ref_span)) =>
            {
                return ("extend", member_prop);
            }
            _ => {}
        }
    }
    ("use", member_prop)
}
