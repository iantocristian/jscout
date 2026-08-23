use std::fs;

use anyhow::Result;

use super::cli_who_uses_for_target;
use crate::query;

#[test]
fn cli_who_uses_prefers_exact_edges_for_one_target() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export class Store { run(): void {} }\nconst store = new Store();\nstore.run();\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let origins = vec!["repository".to_string()];
    let targets = query::find_symbols_in_origins(&conn, "run", &origins)?;
    assert_eq!(targets.len(), 1);
    let graph = query::ModuleGraph::load(&conn)?;

    let exact = cli_who_uses_for_target(&conn, &graph, &targets[0], true, &origins)?;
    assert!(
        exact.iter().any(|usage| usage.confidence == "likely"),
        "the unique CLI target must surface its resolved receiver edge",
    );

    let legacy = cli_who_uses_for_target(&conn, &graph, &targets[0], false, &origins)?;
    assert!(legacy.iter().all(|usage| usage.confidence != "likely"));
    Ok(())
}
