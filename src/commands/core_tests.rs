use std::fs;

use anyhow::Result;

use super::cli_who_uses_for_target;
use crate::query;

#[test]
fn cli_who_uses_resolves_each_matching_target_exactly() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export class First { run(): void {} }\n\
         export class Second { run(): void {} }\n\
         const first = new First();\n\
         const second = new Second();\n\
         declare const dynamic: any;\n\
         first.run();\n\
         second.run();\n\
         dynamic.run();\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let origins = vec!["repository".to_string()];
    let targets = query::find_symbols_in_origins(&conn, "run", &origins)?;
    assert_eq!(targets.len(), 2);
    let graph = query::ModuleGraph::load(&conn)?;

    for target in &targets {
        let exact = cli_who_uses_for_target(&conn, &graph, target, &origins)?;
        assert_eq!(
            exact
                .iter()
                .filter(|usage| usage.confidence == "likely")
                .count(),
            1,
            "each exact CLI target must surface its resolved receiver edge: {target:?}",
        );
        let possible = exact
            .iter()
            .filter(|usage| usage.confidence == "possible")
            .collect::<Vec<_>>();
        assert_eq!(
            possible.len(),
            1,
            "resolved calls must not remain candidates of other targets: {target:?}",
        );
        assert_eq!(possible[0].detail.as_deref(), Some("dynamic.run()"));
    }
    Ok(())
}
