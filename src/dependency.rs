//! Resolver-driven discovery of explicitly selected installed packages.
//!
//! Discovery never walks node_modules. It resolves requests from real
//! importer locations, finds the owning package manifest, canonicalizes that
//! package root, and then compares it with declared workspace roots.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oxc_resolver::{Resolver, TsconfigDiscovery};
use rusqlite::Connection;
use serde::Serialize;

use crate::indexer::{package_name, resolver_options};
use crate::workspace::{WorkspaceMap, WorkspacePackage};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscoveredPackage {
    pub origin: String,
    pub name: String,
    pub version: Option<String>,
    #[serde(skip)]
    pub canonical_root: PathBuf,
    pub locator: String,
    pub manifest_hash: String,
}

/// Find every installed instance of the selected package names that is used
/// by indexed first-party importers. An unused root installation is also
/// admitted through its logical node_modules path. Transitive packages are
/// deliberately not discovered.
pub fn discover(
    root: &Path,
    conn: &Connection,
    selectors: &[String],
    workspace: &WorkspaceMap,
) -> Result<Vec<DiscoveredPackage>> {
    let selectors = normalized_selectors(selectors)?;
    if selectors.is_empty() {
        return Ok(Vec::new());
    }
    if !root.join("node_modules").is_dir()
        && (root.join(".pnp.cjs").is_file() || root.join(".pnp.loader.mjs").is_file())
    {
        bail!("Yarn Plug'n'Play dependency indexing is not supported (no node_modules directory)");
    }

    let requests = importer_requests(root, conn)?;
    let resolver = Resolver::new(resolver_options(Vec::new(), Some(TsconfigDiscovery::Auto)));
    let fallback = Resolver::new(resolver_options(Vec::new(), None));
    let mut found: BTreeMap<PathBuf, DiscoveredPackage> = BTreeMap::new();

    for selector in selectors {
        if let Some(package) = workspace.package_named(&selector) {
            let discovered = workspace_instance(root, package);
            found.insert(discovered.canonical_root.clone(), discovered);
            continue;
        }

        for (importer, request) in requests
            .iter()
            .filter(|(_, request)| package_name(request) == selector)
        {
            let Ok(resolution) = resolver
                .resolve_file(importer, request)
                .or_else(|_| fallback.resolve_file(importer, request))
            else {
                continue;
            };
            if let Some(package_root) = owning_package_root(resolution.path(), &selector)? {
                let discovered = read_instance(root, &package_root, workspace)?;
                found.insert(discovered.canonical_root.clone(), discovered);
            }
        }

        // A requested but currently unused package may still be installed at
        // the repository root. This path is only probed for the named package;
        // it is not a node_modules traversal.
        if !found.values().any(|package| package.name == selector) {
            let logical_root = root.join("node_modules").join(&selector);
            if logical_root.join("package.json").is_file() {
                let discovered = read_instance(root, &logical_root, workspace)?;
                found.insert(discovered.canonical_root.clone(), discovered);
            }
        }

        if !found.values().any(|package| package.name == selector) {
            bail!("selected dependency `{selector}` is not installed or resolvable from an indexed importer");
        }
    }

    Ok(found.into_values().collect())
}

fn normalized_selectors(selectors: &[String]) -> Result<BTreeSet<String>> {
    let mut normalized = BTreeSet::new();
    for selector in selectors {
        let selector = selector.trim();
        let valid = if let Some(scoped) = selector.strip_prefix('@') {
            let mut parts = scoped.split('/');
            parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_none()
        } else {
            !selector.is_empty()
                && !selector.starts_with('.')
                && !selector.contains('/')
                && !selector.contains('@')
        };
        if !valid {
            bail!("dependency selector must be one exact package name; got `{selector}`");
        }
        normalized.insert(selector.to_string());
    }
    Ok(normalized)
}

fn importer_requests(root: &Path, conn: &Connection) -> Result<Vec<(PathBuf, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.path, requests.request
         FROM files f
         JOIN (
           SELECT file_id, request FROM imports
           UNION SELECT file_id, from_request FROM exports WHERE from_request IS NOT NULL
           UNION SELECT file_id, target_request FROM refs WHERE target_request IS NOT NULL
         ) requests ON requests.file_id = f.id
         WHERE f.origin IN ('repository', 'workspace')
         ORDER BY f.path, requests.request",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((root.join(row.get::<_, String>(0)?), row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn owning_package_root(resolved: &Path, expected_name: &str) -> Result<Option<PathBuf>> {
    let resolved = resolved.canonicalize().unwrap_or_else(|_| resolved.to_path_buf());
    let mut current = if resolved.is_dir() {
        resolved
    } else {
        resolved.parent().map(Path::to_path_buf).unwrap_or(resolved)
    };
    loop {
        let manifest = current.join("package.json");
        if manifest.is_file() {
            let text = fs::read_to_string(&manifest)
                .with_context(|| format!("read dependency manifest {}", manifest.display()))?;
            let package: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("parse dependency manifest {}", manifest.display()))?;
            if package.get("name").and_then(|value| value.as_str()) == Some(expected_name) {
                return Ok(Some(current));
            }
        }
        if !current.pop() {
            return Ok(None);
        }
    }
}

fn read_instance(root: &Path, package_root: &Path, workspace: &WorkspaceMap) -> Result<DiscoveredPackage> {
    let canonical_root = package_root
        .canonicalize()
        .with_context(|| format!("canonicalize package root {}", package_root.display()))?;
    let manifest = canonical_root.join("package.json");
    let text = fs::read_to_string(&manifest)
        .with_context(|| format!("read dependency manifest {}", manifest.display()))?;
    let package: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse dependency manifest {}", manifest.display()))?;
    let name = package
        .get("name")
        .and_then(|value| value.as_str())
        .context("selected package manifest has no name")?;
    let origin = if workspace.package_at_root(&canonical_root).is_some() {
        "workspace"
    } else {
        "dependency"
    };
    Ok(DiscoveredPackage {
        origin: origin.into(),
        name: name.into(),
        version: package.get("version").and_then(|value| value.as_str()).map(str::to_string),
        locator: display_locator(root, &canonical_root),
        canonical_root,
        manifest_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
    })
}

fn workspace_instance(root: &Path, package: &WorkspacePackage) -> DiscoveredPackage {
    DiscoveredPackage {
        origin: "workspace".into(),
        name: package.name.clone(),
        version: package.version.clone(),
        canonical_root: package.canonical_root.clone(),
        locator: display_locator(root, &package.canonical_root),
        manifest_hash: package.manifest_hash.clone(),
    }
}

fn display_locator(root: &Path, package_root: &Path) -> String {
    package_root
        .strip_prefix(root)
        .unwrap_or(package_root)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use anyhow::Result;

    use super::discover;
    use crate::{store, workspace::WorkspaceMap};

    fn write(path: &Path, content: &str) -> Result<()> {
        fs::create_dir_all(path.parent().expect("parent"))?;
        fs::write(path, content)?;
        Ok(())
    }

    fn importer(conn: &rusqlite::Connection, root: &Path, path: &str, request: &str) -> Result<()> {
        write(&root.join(path), &format!("import value from '{request}';\n"))?;
        conn.execute(
            "INSERT INTO files(path, hash, role) VALUES(?1, 'hash', 'production')",
            [path],
        )?;
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO imports(file_id, local_name, imported_name, request)
             VALUES(?1, 'value', 'default', ?2)",
            rusqlite::params![file_id, request],
        )?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_pnpm_links_and_keeps_workspace_links_first_party() -> Result<()> {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir()?;
        let root = repo.path();
        write(&root.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")?;
        write(
            &root.join("packages/work/package.json"),
            r#"{"name":"work","version":"0.1.0","main":"src/index.ts"}"#,
        )?;
        write(&root.join("packages/work/src/index.ts"), "export default 1;\n")?;

        let external = root.join("node_modules/.pnpm/left-pad@1.3.0/node_modules/left-pad");
        write(
            &external.join("package.json"),
            r#"{"name":"left-pad","version":"1.3.0","main":"index.js"}"#,
        )?;
        write(&external.join("index.js"), "module.exports = 1;\n")?;
        fs::create_dir_all(root.join("node_modules"))?;
        symlink(&external, root.join("node_modules/left-pad"))?;
        symlink(root.join("packages/work"), root.join("node_modules/work"))?;

        let conn = store::open(root)?;
        importer(&conn, root, "src/main.ts", "left-pad")?;
        let workspace = WorkspaceMap::build(root);
        let packages = discover(
            root,
            &conn,
            &["left-pad".into(), "work".into()],
            &workspace,
        )?;

        assert_eq!(packages.len(), 2);
        let dependency = packages.iter().find(|package| package.name == "left-pad").unwrap();
        assert_eq!(dependency.origin, "dependency");
        assert_eq!(dependency.version.as_deref(), Some("1.3.0"));
        assert_eq!(dependency.canonical_root, external.canonicalize()?);
        let work = packages.iter().find(|package| package.name == "work").unwrap();
        assert_eq!(work.origin, "workspace");
        assert_eq!(work.canonical_root, root.join("packages/work").canonicalize()?);
        Ok(())
    }

    #[test]
    fn discovers_distinct_versions_from_each_importer_location() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path();
        for (base, version) in [
            (root.to_path_buf(), "1.0.0"),
            (root.join("packages/app"), "2.0.0"),
        ] {
            write(
                &base.join("node_modules/dep/package.json"),
                &format!(r#"{{"name":"dep","version":"{version}","main":"index.js"}}"#),
            )?;
            write(&base.join("node_modules/dep/index.js"), "module.exports = 1;\n")?;
        }
        let conn = store::open(root)?;
        importer(&conn, root, "src/root.ts", "dep")?;
        importer(&conn, root, "packages/app/src/app.ts", "dep")?;

        let packages = discover(root, &conn, &["dep".into()], &WorkspaceMap::build(root))?;
        let versions: BTreeSet<_> =
            packages.iter().filter_map(|package| package.version.as_deref()).collect();
        assert_eq!(versions, BTreeSet::from(["1.0.0", "2.0.0"]));
        Ok(())
    }

    #[test]
    fn rejects_pnp_without_pretending_the_package_is_missing() -> Result<()> {
        let repo = tempfile::tempdir()?;
        write(&repo.path().join(".pnp.cjs"), "module.exports = {};\n")?;
        let conn = store::open(repo.path())?;
        let error = discover(
            repo.path(),
            &conn,
            &["left-pad".into()],
            &WorkspaceMap::build(repo.path()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Plug'n'Play"));
        Ok(())
    }
}
