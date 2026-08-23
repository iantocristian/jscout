//! Closed receiver-value projection over extraction facts and the module graph.

use super::*;

#[derive(Debug, Clone)]
struct StoredFlowValue {
    file_id: i64,
    kind: String,
    target_kind: String,
    target_name: String,
    target_start: i64,
}

#[derive(Debug, Clone)]
struct StoredClassFlow {
    file_id: i64,
    class_name: String,
    class_start: i64,
    super_name: Option<String>,
    super_start: Option<i64>,
    super_kind: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredFunctionFlow {
    is_async: bool,
    returns: Vec<StoredFlowValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ResolvedClass {
    file_id: i64,
    start: i64,
    name: String,
}

#[derive(Debug, Clone)]
struct ResolvedBinding {
    file_id: i64,
    start: i64,
    name: String,
    kind: String,
}

struct ValueFlowCatalog {
    functions: HashMap<(i64, i64), StoredFunctionFlow>,
    bindings: HashMap<(i64, i64), StoredFlowValue>,
    classes: HashMap<(i64, i64), StoredClassFlow>,
    instance_methods: HashMap<(i64, i64), i64>,
    member_blockers: HashMap<(i64, i64), HashSet<String>>,
}

impl ValueFlowCatalog {
    fn load(conn: &Connection) -> Result<Self> {
        let mut functions = HashMap::<(i64, i64), StoredFunctionFlow>::new();
        let mut function_statement = conn.prepare(
            "SELECT file_id, function_start, function_async,
                    value_kind, target_kind, target_name, target_start
             FROM function_return_flows
             ORDER BY file_id, function_start, return_index",
        )?;
        let function_rows = function_statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        for row in function_rows {
            let (file_id, function_start, is_async, kind, target_kind, target_name, target_start) =
                row?;
            let function = functions
                .entry((file_id, function_start))
                .or_insert_with(|| StoredFunctionFlow {
                    is_async,
                    returns: Vec::new(),
                });
            debug_assert_eq!(function.is_async, is_async);
            function.returns.push(StoredFlowValue {
                file_id,
                kind,
                target_kind,
                target_name,
                target_start,
            });
        }

        let mut bindings = HashMap::new();
        let mut binding_statement = conn.prepare(
            "SELECT file_id, binding_start, value_kind, target_kind, target_name, target_start
             FROM value_binding_flows ORDER BY file_id, binding_start",
        )?;
        let binding_rows = binding_statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in binding_rows {
            let (file_id, binding_start, kind, target_kind, target_name, target_start) = row?;
            bindings.insert(
                (file_id, binding_start),
                StoredFlowValue {
                    file_id,
                    kind,
                    target_kind,
                    target_name,
                    target_start,
                },
            );
        }

        let mut classes = HashMap::new();
        let mut class_statement = conn.prepare(
            "SELECT file_id, class_name, class_start, super_name, super_start, super_kind
             FROM class_value_flows ORDER BY file_id, class_start",
        )?;
        let class_rows = class_statement.query_map([], |row| {
            Ok(StoredClassFlow {
                file_id: row.get(0)?,
                class_name: row.get(1)?,
                class_start: row.get(2)?,
                super_name: row.get(3)?,
                super_start: row.get(4)?,
                super_kind: row.get(5)?,
            })
        })?;
        for row in class_rows {
            let class = row?;
            classes.insert((class.file_id, class.class_start), class);
        }
        let instance_methods = conn
            .prepare(
                "SELECT file_id, method_start, class_start
                 FROM instance_method_value_flows ORDER BY file_id, method_start",
            )?
            .query_map([], |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        let mut member_blockers = HashMap::<(i64, i64), HashSet<String>>::new();
        let mut blocker_statement = conn.prepare(
            "SELECT file_id, class_start, member_name
             FROM class_member_value_flow_blockers
             ORDER BY file_id, class_start, member_name",
        )?;
        let blocker_rows = blocker_statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in blocker_rows {
            let (file_id, class_start, member_name) = row?;
            member_blockers
                .entry((file_id, class_start))
                .or_default()
                .insert(member_name);
        }
        Ok(Self {
            functions,
            bindings,
            classes,
            instance_methods,
            member_blockers,
        })
    }
}

fn resolve_flow_bindings(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    value: &StoredFlowValue,
) -> Result<Option<Vec<ResolvedBinding>>> {
    let references = conn
        .prepare_cached(
            "SELECT target_request, target_name, local, detail
             FROM refs WHERE file_id=?1 AND start=?2
             ORDER BY id",
        )?
        .query_map(params![value.file_id, value.target_start], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let [(request, name, local, detail)] = references.as_slice() else {
        return Ok(None);
    };
    let namespace_reference = detail
        .as_deref()
        .is_some_and(|value| value.starts_with("via namespace "));
    match value.target_kind.as_str() {
        "identifier" if namespace_reference => return Ok(None),
        "member" if *local || name != &value.target_name || !namespace_reference => {
            return Ok(None);
        }
        "identifier" | "member" => {}
        _ => return Ok(None),
    }
    let targets = if *local {
        projected_symbols(root_symbol.get(&(value.file_id, name.clone())))
    } else {
        let Some(request) = request.as_deref() else {
            return Ok(None);
        };
        if graph.edge_inferred(value.file_id, request) {
            return Ok(None);
        }
        let Some(target_file) = graph.edge(value.file_id, request) else {
            return Ok(None);
        };
        let Some((resolved_file, resolved_name)) = graph.resolve_export_exact(target_file, name)
        else {
            return Ok(None);
        };
        projected_symbols(root_symbol.get(&(resolved_file, resolved_name)))
    };
    if targets.ambiguous || targets.keys.len() != 1 {
        return Ok(None);
    }
    let mut bindings = Vec::with_capacity(targets.keys.len());
    for key in targets.keys {
        let Some(symbol) = symbols_by_key.get(key.as_str()) else {
            return Ok(None);
        };
        bindings.push(ResolvedBinding {
            file_id: symbol.file_id,
            start: symbol.start,
            name: symbol.name.clone(),
            kind: symbol.kind.clone(),
        });
    }
    bindings.sort_by(|left, right| {
        (left.file_id, left.start, &left.name).cmp(&(right.file_id, right.start, &right.name))
    });
    bindings.dedup_by(|left, right| {
        left.file_id == right.file_id && left.start == right.start && left.name == right.name
    });
    Ok(Some(bindings))
}

fn resolve_constructed_classes(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    value: &StoredFlowValue,
) -> Result<Option<BTreeSet<ResolvedClass>>> {
    let Some(bindings) = resolve_flow_bindings(conn, graph, root_symbol, symbols_by_key, value)?
    else {
        return Ok(None);
    };
    let mut classes = BTreeSet::new();
    for binding in bindings {
        if binding.kind != "class" {
            return Ok(None);
        }
        classes.insert(ResolvedClass {
            file_id: binding.file_id,
            start: binding.start,
            name: binding.name,
        });
    }
    if classes.is_empty() || classes.len() > 3 {
        return Ok(None);
    }
    Ok(Some(classes))
}

#[expect(
    clippy::too_many_arguments,
    reason = "construction identity validation follows exact superclass bindings with explicit cache and cycle state"
)]
fn construction_identity_is_safe(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    catalog: &ValueFlowCatalog,
    class: &ResolvedClass,
    cache: &mut HashMap<ResolvedClass, bool>,
    visiting: &mut BTreeSet<ResolvedClass>,
) -> Result<bool> {
    if let Some(safe) = cache.get(class) {
        return Ok(*safe);
    }
    if !visiting.insert(class.clone()) {
        return Ok(false);
    }
    let safe = match catalog.classes.get(&(class.file_id, class.start)) {
        Some(flow) if flow.class_name == class.name => match (
            flow.super_name.as_ref(),
            flow.super_start,
            flow.super_kind.as_ref(),
        ) {
            (None, None, None) => true,
            (Some(super_name), Some(super_start), Some(super_kind)) => {
                let super_value = StoredFlowValue {
                    file_id: flow.file_id,
                    kind: "construct".into(),
                    target_kind: super_kind.clone(),
                    target_name: super_name.clone(),
                    target_start: super_start,
                };
                match resolve_constructed_classes(
                    conn,
                    graph,
                    root_symbol,
                    symbols_by_key,
                    &super_value,
                )? {
                    Some(base_classes) if base_classes.len() == 1 => {
                        let base = base_classes.iter().next().expect("one checked superclass");
                        construction_identity_is_safe(
                            conn,
                            graph,
                            root_symbol,
                            symbols_by_key,
                            catalog,
                            base,
                            cache,
                            visiting,
                        )?
                    }
                    _ => false,
                }
            }
            _ => false,
        },
        _ => false,
    };
    visiting.remove(class);
    cache.insert(class.clone(), safe);
    Ok(safe)
}

#[expect(
    clippy::too_many_arguments,
    reason = "property safety follows the exact superclass chain before accepting prototype methods"
)]
fn class_chain_allows_property(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    catalog: &ValueFlowCatalog,
    class: &ResolvedClass,
    property: &str,
    visiting: &mut BTreeSet<ResolvedClass>,
) -> Result<bool> {
    if !visiting.insert(class.clone()) {
        return Ok(false);
    }
    let allows = match catalog.classes.get(&(class.file_id, class.start)) {
        Some(flow) if flow.class_name == class.name => {
            if catalog
                .member_blockers
                .get(&(class.file_id, class.start))
                .is_some_and(|members| members.contains(property) || members.contains("*"))
            {
                false
            } else {
                match (
                    flow.super_name.as_ref(),
                    flow.super_start,
                    flow.super_kind.as_ref(),
                ) {
                    (None, None, None) => true,
                    (Some(super_name), Some(super_start), Some(super_kind)) => {
                        let super_value = StoredFlowValue {
                            file_id: flow.file_id,
                            kind: "construct".into(),
                            target_kind: super_kind.clone(),
                            target_name: super_name.clone(),
                            target_start: super_start,
                        };
                        match resolve_constructed_classes(
                            conn,
                            graph,
                            root_symbol,
                            symbols_by_key,
                            &super_value,
                        )? {
                            Some(base_classes) if base_classes.len() == 1 => {
                                let base = base_classes.iter().next().expect("one checked base");
                                class_chain_allows_property(
                                    conn,
                                    graph,
                                    root_symbol,
                                    symbols_by_key,
                                    catalog,
                                    base,
                                    property,
                                    visiting,
                                )?
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                }
            }
        }
        _ => false,
    };
    visiting.remove(class);
    Ok(allows)
}

#[expect(
    clippy::too_many_arguments,
    reason = "bounded flow resolution keeps immutable graph catalogs and its cycle/depth state explicit"
)]
fn resolve_factory_classes(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    catalog: &ValueFlowCatalog,
    value: &StoredFlowValue,
    depth: usize,
    visited: &mut BTreeSet<(u8, i64, i64)>,
) -> Result<Option<BTreeSet<ResolvedClass>>> {
    if depth > 2 {
        return Ok(None);
    }
    let Some(bindings) = resolve_flow_bindings(conn, graph, root_symbol, symbols_by_key, value)?
    else {
        return Ok(None);
    };
    let mut classes = BTreeSet::new();
    for binding in bindings {
        if !matches!(binding.kind.as_str(), "function" | "const") {
            return Ok(None);
        }
        let function_key = (binding.file_id, binding.start);
        let Some(function) = catalog.functions.get(&function_key) else {
            return Ok(None);
        };
        if function.is_async {
            return Ok(None);
        }
        let visit_key = (0, function_key.0, function_key.1);
        if !visited.insert(visit_key) {
            return Ok(None);
        }
        for returned in &function.returns {
            let resolved = match returned.kind.as_str() {
                "construct" => {
                    resolve_constructed_classes(conn, graph, root_symbol, symbols_by_key, returned)?
                }
                "factory" => resolve_factory_classes(
                    conn,
                    graph,
                    root_symbol,
                    symbols_by_key,
                    catalog,
                    returned,
                    depth + 1,
                    visited,
                )?,
                "binding" => resolve_binding_classes(
                    conn,
                    graph,
                    root_symbol,
                    symbols_by_key,
                    catalog,
                    returned,
                    depth,
                    visited,
                )?,
                _ => None,
            };
            let Some(resolved) = resolved else {
                visited.remove(&visit_key);
                return Ok(None);
            };
            classes.extend(resolved);
            if classes.len() > 3 {
                visited.remove(&visit_key);
                return Ok(None);
            }
        }
        visited.remove(&visit_key);
    }
    if classes.is_empty() {
        return Ok(None);
    }
    Ok(Some(classes))
}

#[expect(
    clippy::too_many_arguments,
    reason = "binding resolution uses the same closed graph catalogs and depth/cycle state as factories"
)]
fn resolve_binding_classes(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    catalog: &ValueFlowCatalog,
    value: &StoredFlowValue,
    depth: usize,
    visited: &mut BTreeSet<(u8, i64, i64)>,
) -> Result<Option<BTreeSet<ResolvedClass>>> {
    let Some(bindings) = resolve_flow_bindings(conn, graph, root_symbol, symbols_by_key, value)?
    else {
        return Ok(None);
    };
    let mut classes = BTreeSet::new();
    for binding in bindings {
        if binding.kind != "const" {
            return Ok(None);
        }
        let binding_key = (binding.file_id, binding.start);
        let Some(bound) = catalog.bindings.get(&binding_key) else {
            return Ok(None);
        };
        let visit_key = (1, binding_key.0, binding_key.1);
        if !visited.insert(visit_key) {
            return Ok(None);
        }
        let resolved = match bound.kind.as_str() {
            "construct" => {
                resolve_constructed_classes(conn, graph, root_symbol, symbols_by_key, bound)?
            }
            "factory" => resolve_factory_classes(
                conn,
                graph,
                root_symbol,
                symbols_by_key,
                catalog,
                bound,
                depth,
                visited,
            )?,
            "binding" => resolve_binding_classes(
                conn,
                graph,
                root_symbol,
                symbols_by_key,
                catalog,
                bound,
                depth,
                visited,
            )?,
            _ => None,
        };
        visited.remove(&visit_key);
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        classes.extend(resolved);
        if classes.len() > 3 {
            return Ok(None);
        }
    }
    if classes.is_empty() {
        return Ok(None);
    }
    Ok(Some(classes))
}

#[expect(
    clippy::too_many_arguments,
    reason = "receiver resolution composes the canonical module graph, symbol catalog, and bounded factory state"
)]
fn resolve_receiver_classes(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    catalog: &ValueFlowCatalog,
    receiver_kind: &str,
    file_id: i64,
    class_name: Option<String>,
    class_start: Option<i64>,
    value_kind: Option<String>,
    target_kind: Option<String>,
    target_name: Option<String>,
    target_start: Option<i64>,
) -> Result<Option<BTreeSet<ResolvedClass>>> {
    if receiver_kind == "this" {
        let (Some(name), Some(start)) = (class_name, class_start) else {
            return Ok(None);
        };
        return Ok(Some(BTreeSet::from([ResolvedClass {
            file_id,
            start,
            name,
        }])));
    }
    let (Some(kind), Some(target_kind), Some(target_name), Some(target_start)) =
        (value_kind, target_kind, target_name, target_start)
    else {
        return Ok(None);
    };
    let value = StoredFlowValue {
        file_id,
        kind: kind.clone(),
        target_kind,
        target_name,
        target_start,
    };
    match kind.as_str() {
        "construct" => {
            resolve_constructed_classes(conn, graph, root_symbol, symbols_by_key, &value)
        }
        "factory" => resolve_factory_classes(
            conn,
            graph,
            root_symbol,
            symbols_by_key,
            catalog,
            &value,
            0,
            &mut BTreeSet::new(),
        ),
        "binding" => resolve_binding_classes(
            conn,
            graph,
            root_symbol,
            symbols_by_key,
            catalog,
            &value,
            0,
            &mut BTreeSet::new(),
        ),
        _ => Ok(None),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "method lookup needs both extracted class inheritance and resolved symbol catalogs"
)]
fn resolve_flow_methods(
    conn: &Connection,
    graph: &ModuleGraph,
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    symbols_by_key: &HashMap<&str, &SymbolNode>,
    methods: &HashMap<(i64, i64, String), Vec<String>>,
    catalog: &ValueFlowCatalog,
    classes: &BTreeSet<ResolvedClass>,
    property: &str,
) -> Result<Option<BTreeSet<String>>> {
    let mut targets = BTreeSet::new();
    for class in classes {
        if !class_chain_allows_property(
            conn,
            graph,
            root_symbol,
            symbols_by_key,
            catalog,
            class,
            property,
            &mut BTreeSet::new(),
        )? {
            return Ok(None);
        }
        let own_key = (class.file_id, class.start, property.to_string());
        if let Some(own) = methods.get(&own_key) {
            if own.len() != 1 {
                return Ok(None);
            }
            targets.insert(own[0].clone());
            if targets.len() > 3 {
                return Ok(None);
            }
            continue;
        }
        let Some(class_flow) = catalog.classes.get(&(class.file_id, class.start)) else {
            return Ok(None);
        };
        if class_flow.class_name != class.name {
            return Ok(None);
        }
        let (Some(super_name), Some(super_start)) =
            (class_flow.super_name.as_ref(), class_flow.super_start)
        else {
            return Ok(None);
        };
        let super_value = StoredFlowValue {
            file_id: class_flow.file_id,
            kind: "construct".into(),
            target_kind: class_flow.super_kind.clone().unwrap_or_default(),
            target_name: super_name.clone(),
            target_start: super_start,
        };
        let Some(base_classes) =
            resolve_constructed_classes(conn, graph, root_symbol, symbols_by_key, &super_value)?
        else {
            return Ok(None);
        };
        for base in base_classes {
            if catalog
                .member_blockers
                .get(&(base.file_id, base.start))
                .is_some_and(|members| members.contains(property) || members.contains("*"))
            {
                return Ok(None);
            }
            let base_key = (base.file_id, base.start, property.to_string());
            let Some(inherited) = methods.get(&base_key) else {
                return Ok(None);
            };
            if inherited.len() != 1 {
                return Ok(None);
            }
            targets.insert(inherited[0].clone());
            if targets.len() > 3 {
                return Ok(None);
            }
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    Ok(Some(targets))
}

pub(super) fn project_receiver_value_flows(
    conn: &Connection,
    files: &HashMap<i64, String>,
    graph: &ModuleGraph,
    symbols: &[SymbolNode],
    root_symbol: &HashMap<(i64, String), Vec<&SymbolNode>>,
    insert_edge: &mut rusqlite::CachedStatement<'_>,
) -> Result<()> {
    let catalog = ValueFlowCatalog::load(conn)?;
    let symbols_by_key = symbols
        .iter()
        .map(|symbol| (symbol.key.as_str(), symbol))
        .collect::<HashMap<_, _>>();
    let mut symbols_by_file: HashMap<i64, Vec<&SymbolNode>> = HashMap::new();
    let mut methods = HashMap::<(i64, i64, String), Vec<String>>::new();
    for symbol in symbols {
        symbols_by_file
            .entry(symbol.file_id)
            .or_default()
            .push(symbol);
        if let Some(class_start) = catalog
            .instance_methods
            .get(&(symbol.file_id, symbol.start))
        {
            methods
                .entry((symbol.file_id, *class_start, symbol.name.clone()))
                .or_default()
                .push(symbol.key.clone());
        }
    }
    for targets in methods.values_mut() {
        targets.sort();
        targets.dedup();
    }
    let mut construction_cache = HashMap::<ResolvedClass, bool>::new();
    let mut method_cache = HashMap::<(Vec<ResolvedClass>, String), Option<BTreeSet<String>>>::new();

    let mut statement = conn.prepare(
        "SELECT flow.file_id, flow.receiver_kind, flow.class_name, flow.class_start,
                flow.value_kind, flow.target_kind, flow.target_name, flow.target_start,
                call.rowid, call.start, call.end, call.line,
                call.receiver_start, call.receiver_end,
                call.property_start, call.property_end, call.prop
         FROM receiver_value_flows flow
         JOIN member_calls call
           ON call.file_id=flow.file_id
          AND call.start=flow.call_start AND call.end=flow.call_end
         ORDER BY flow.file_id, flow.call_start, flow.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, i64>(13)?,
            row.get::<_, i64>(14)?,
            row.get::<_, i64>(15)?,
            row.get::<_, String>(16)?,
        ))
    })?;
    for row in rows {
        let (
            file_id,
            receiver_kind,
            class_name,
            class_start,
            value_kind,
            target_kind,
            target_name,
            target_start,
            member_call_id,
            call_start,
            call_end,
            line,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
            property,
        ) = row?;
        let Some(path) = files.get(&file_id) else {
            continue;
        };
        let Some(classes) = resolve_receiver_classes(
            conn,
            graph,
            root_symbol,
            &symbols_by_key,
            &catalog,
            &receiver_kind,
            file_id,
            class_name,
            class_start,
            value_kind.clone(),
            target_kind,
            target_name,
            target_start,
        )?
        else {
            continue;
        };
        if receiver_kind != "this" {
            let mut construction_is_safe = true;
            for class in &classes {
                if !construction_identity_is_safe(
                    conn,
                    graph,
                    root_symbol,
                    &symbols_by_key,
                    &catalog,
                    class,
                    &mut construction_cache,
                    &mut BTreeSet::new(),
                )? {
                    construction_is_safe = false;
                    break;
                }
            }
            if !construction_is_safe {
                continue;
            }
        }
        let method_key = (
            classes.iter().cloned().collect::<Vec<_>>(),
            property.clone(),
        );
        let targets = if let Some(cached) = method_cache.get(&method_key) {
            cached.clone()
        } else {
            let resolved = resolve_flow_methods(
                conn,
                graph,
                root_symbol,
                &symbols_by_key,
                &methods,
                &catalog,
                &classes,
                &property,
            )?;
            method_cache.insert(method_key, resolved.clone());
            resolved
        };
        let Some(targets) = targets else {
            continue;
        };
        let source = owner_at(symbols_by_file.get(&file_id), call_start)
            .map_or_else(|| file_key(path), |symbol| symbol.key.clone());
        let receiver_classes = classes
            .iter()
            .map(|class| {
                let class_path = files.get(&class.file_id).map_or("?", String::as_str);
                format!("{class_path}#{}", class.name)
            })
            .collect::<Vec<_>>();
        let candidate_count = targets.len();
        let flow_kind = if receiver_kind == "this" {
            "this"
        } else {
            value_kind.as_deref().unwrap_or("unknown")
        };
        let detail = json!({
            "memberCallId": member_call_id,
            "call": [call_start, call_end],
            "receiver": [receiver_start, receiver_end],
            "property": [property_start, property_end],
            "flow": flow_kind,
            "receiverClasses": receiver_classes,
            "candidateCount": candidate_count,
            "occurrenceSpecific": true,
        })
        .to_string();
        for target in targets {
            insert_edge.execute(params![
                source,
                target,
                "member_call",
                "likely",
                "receiver-value-flow",
                file_id,
                member_call_id,
                line,
                detail,
            ])?;
        }
    }
    Ok(())
}
