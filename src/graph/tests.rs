use std::path::Path;

use anyhow::Result;

use crate::parse::with_parsed;

#[test]
fn default_identifier_exports_preserve_local_bindings_and_exported_metadata() -> Result<()> {
    for source in [
        "const Local = () => null; export default Local;",
        "function Local() {} export default Local;",
        "class Local {} export default Local;",
        "export default function Local() {}",
        "export default class Local {}",
        "const Local = () => null; export { Local as default };",
        "export function Local() {}",
    ] {
        let graph = with_parsed(source, Path::new("entry.tsx"), super::extract)?;
        for exports in [&graph.exports, &graph.contract_exports] {
            assert_eq!(exports.len(), 1, "{source}");
            assert_eq!(exports[0].local_name.as_deref(), Some("Local"), "{source}");
            assert_eq!(
                exports[0].export_name,
                if source.starts_with("export function") {
                    "Local"
                } else {
                    "default"
                },
                "{source}"
            );
        }
        let symbol = graph
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Local")
            .unwrap();
        assert!(symbol.exported, "{source}");
    }
    Ok(())
}

#[test]
fn default_expressions_do_not_acquire_a_local_binding() -> Result<()> {
    for expression in [
        "Local()",
        "{ Local }",
        "Local.member",
        "() => Local",
        "function() {}",
    ] {
        let source = format!("const Local = () => null; export default {expression};");
        let graph = with_parsed(&source, Path::new("entry.tsx"), super::extract)?;
        for exports in [&graph.exports, &graph.contract_exports] {
            assert_eq!(exports.len(), 1, "{source}");
            assert_eq!(exports[0].local_name, None, "{source}");
        }
        assert!(
            graph.symbols.iter().all(|symbol| !symbol.exported),
            "{source}"
        );
    }
    Ok(())
}

#[test]
fn type_only_default_export_stays_out_of_the_runtime_graph() -> Result<()> {
    let graph = with_parsed(
        "interface Local {} export type { Local as default };",
        Path::new("entry.ts"),
        super::extract,
    )?;
    assert!(graph.exports.is_empty());
    assert!(graph.symbols.is_empty());
    assert_eq!(graph.contract_exports.len(), 1);
    assert_eq!(graph.contract_exports[0].export_name, "default");
    assert_eq!(
        graph.contract_exports[0].local_name.as_deref(),
        Some("Local")
    );
    Ok(())
}
