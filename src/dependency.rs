//! Resolver-driven discovery of explicitly selected installed packages.
//!
//! Discovery never walks `node_modules`. It resolves requests from real
//! importer locations, finds the owning package manifest, canonicalizes that
//! package root, and then compares it with declared workspace roots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oxc_resolver::{Resolver, TsconfigDiscovery};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::fs_ops::FileSystem;
use crate::indexer::{package_name, resolver_options};
use crate::package_exports::collect_active_targets;
use crate::store;
use crate::walk;
use crate::workspace::{WorkspaceMap, WorkspacePackage};

pub const DEFAULT_MAX_FILES: usize = 10_000;
pub const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

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

#[derive(Clone, Copy, Debug)]
pub struct DependencyLimits {
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_file_bytes: u64,
}

impl Default for DependencyLimits {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_FILES,
            max_bytes: DEFAULT_MAX_BYTES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlannedFile {
    pub source_path: PathBuf,
    pub package_path: String,
    pub bytes: u64,
    pub forced_entry: bool,
}

#[derive(Clone, Debug)]
pub struct PackagePlan {
    pub package: DiscoveredPackage,
    pub files: Vec<PlannedFile>,
    pub source_basis: String,
    pub skipped_files: usize,
    pub skipped_bytes: u64,
    pub status: String,
}

/// Find every installed instance of the selected package names that is used
/// by indexed first-party importers. An unused root installation is also
/// admitted through its logical `node_modules` path. Transitive packages are
/// deliberately not discovered.
pub fn discover(
    root: &Path,
    conn: &Connection,
    selectors: &[String],
    workspace: &WorkspaceMap,
    fs: &impl FileSystem,
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
            if let Some(package_root) = owning_package_root(resolution.path(), &selector, fs)? {
                let discovered = read_instance(root, &package_root, workspace, fs)?;
                found.insert(discovered.canonical_root.clone(), discovered);
            }
        }

        // A requested but currently unused package may still be installed at
        // the repository root. This path is only probed for the named package;
        // it is not a node_modules traversal.
        if !found.values().any(|package| package.name == selector) {
            let logical_root = root.join("node_modules").join(&selector);
            if logical_root.join("package.json").is_file() {
                let discovered = read_instance(root, &logical_root, workspace, fs)?;
                found.insert(discovered.canonical_root.clone(), discovered);
            }
        }

        if !found.values().any(|package| package.name == selector) {
            bail!(
                "selected dependency `{selector}` is not installed or resolvable from an indexed importer"
            );
        }
    }

    Ok(found.into_values().collect())
}

/// Select a bounded, deterministic set of source files for each discovered
/// third-party package. Workspace instances are identity-only here because
/// their files are already part of the repository corpus.
pub fn plan_packages(
    packages: &[DiscoveredPackage],
    limits: DependencyLimits,
    fs: &impl FileSystem,
) -> Result<Vec<PackagePlan>> {
    if limits.max_files == 0 || limits.max_bytes == 0 || limits.max_file_bytes == 0 {
        bail!("dependency file and byte limits must be greater than zero");
    }
    packages
        .iter()
        .filter(|package| package.origin == "dependency")
        .map(|package| plan_package(package, limits, fs))
        .collect()
}

/// Reconcile persistent package instances with the current workspace and the
/// explicitly selected dependency plans. The selector list is authoritative:
/// dependency instances omitted from this run are removed with their files.
pub fn synchronize_instances(
    root: &Path,
    conn: &Connection,
    workspace: &WorkspaceMap,
    plans: &[PackagePlan],
) -> Result<BTreeMap<PathBuf, i64>> {
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        conn.execute(
            "UPDATE files
             SET origin='repository', package_instance_id=NULL, package_path=NULL
             WHERE origin IN ('repository', 'workspace')",
            [],
        )?;

        let mut instances = BTreeMap::new();
        let mut desired = BTreeSet::new();
        for package in &workspace.packages {
            let discovered = workspace_instance(root, package);
            let id = upsert_instance(conn, &discovered, "complete")?;
            desired.insert(discovered.canonical_root.clone());
            instances.insert(discovered.canonical_root, id);
        }
        for plan in plans {
            let id = upsert_instance(conn, &plan.package, &plan.status)?;
            desired.insert(plan.package.canonical_root.clone());
            instances.insert(plan.package.canonical_root.clone(), id);
        }

        let stale: Vec<(i64, PathBuf)> = {
            let mut stmt = conn.prepare(
                "SELECT id, canonical_root FROM package_instances
                 WHERE origin IN ('workspace', 'dependency')",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (id, canonical_root) in stale {
            if !desired.contains(&canonical_root) {
                // FTS5 does not participate in SQLite foreign-key cascades.
                // Remove instance-owned files through the canonical deletion
                // path before deleting the package instance itself.
                let file_ids: Vec<i64> = {
                    let mut stmt =
                        conn.prepare("SELECT id FROM files WHERE package_instance_id=?1")?;
                    let rows = stmt.query_map([id], |row| row.get(0))?;
                    rows.collect::<std::result::Result<_, _>>()?
                };
                for file_id in file_ids {
                    store::delete_file(conn, file_id)?;
                }
                conn.execute("DELETE FROM package_instances WHERE id=?1", [id])?;
            }
        }

        // Shallow packages are tagged first so a nested declared workspace
        // package, if present, owns its more-specific subtree.
        let mut workspace_roots: Vec<_> = workspace.packages.iter().collect();
        workspace_roots.sort_by_key(|package| package.canonical_root.components().count());
        for package in workspace_roots {
            let Some(id) = instances.get(&package.canonical_root) else {
                continue;
            };
            let Ok(relative) = package.canonical_root.strip_prefix(root) else {
                continue;
            };
            let prefix = relative.to_string_lossy().replace('\\', "/");
            let subtree_prefix = format!("{prefix}/");
            conn.execute(
                "UPDATE files
                 SET origin='workspace', package_instance_id=?1,
                     package_path=CASE
                       WHEN path=?2 THEN ''
                       ELSE substr(path, ?3)
                     END
                 WHERE origin!='dependency'
                   AND (path=?2 OR substr(path, 1, ?5)=?4)",
                params![
                    id,
                    prefix,
                    prefix.len() as i64 + 2,
                    subtree_prefix,
                    subtree_prefix.len() as i64,
                ],
            )?;
        }
        Ok(instances)
    })();
    match result {
        Ok(instances) => {
            conn.execute_batch("COMMIT")?;
            Ok(instances)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn upsert_instance(conn: &Connection, package: &DiscoveredPackage, status: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO package_instances(
           origin, name, version, canonical_root, locator, manifest_hash, status
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(canonical_root) DO UPDATE SET
           origin=excluded.origin,
           name=excluded.name,
           version=excluded.version,
           locator=excluded.locator,
           manifest_hash=excluded.manifest_hash,
           status=excluded.status",
        params![
            package.origin,
            package.name,
            package.version,
            package.canonical_root.to_string_lossy(),
            package.locator,
            package.manifest_hash,
            status,
        ],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM package_instances WHERE canonical_root=?1",
        [package.canonical_root.to_string_lossy()],
        |row| row.get(0),
    )?)
}

pub fn should_skip_minified(path: &Path, source: &str, forced_entry: bool) -> bool {
    if forced_entry {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.contains(".min.") {
        return true;
    }
    let mut lines = source.lines();
    let first = lines.next().unwrap_or_default();
    first.len() > 4_000 && lines.take(4).all(|line| line.len() > 1_000)
}

fn plan_package(
    package: &DiscoveredPackage,
    limits: DependencyLimits,
    fs: &impl FileSystem,
) -> Result<PackagePlan> {
    let manifest_path = package.canonical_root.join("package.json");
    let text = fs
        .read_to_string(&manifest_path)
        .with_context(|| format!("read dependency manifest {}", manifest_path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse dependency manifest {}", manifest_path.display()))?;
    let (roots, forced_entries, source_basis) = analysis_roots(package, &manifest);
    let mut candidates = Vec::new();
    for root in roots {
        collect_indexable_files(&root, &mut candidates, fs)?;
    }
    candidates.sort();
    candidates.dedup();
    // Boundary entries consume the bounded budget first. Without this
    // ordering a lexically late entry could disappear behind unrelated files
    // even though it is the package target first-party imports resolve to.
    candidates.sort_by(|left, right| {
        let is_forced = |path: &Path| {
            path.strip_prefix(&package.canonical_root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .is_some_and(|path| forced_entries.contains(&path))
        };
        is_forced(right)
            .cmp(&is_forced(left))
            .then_with(|| left.cmp(right))
    });

    let mut files = Vec::new();
    let mut selected_bytes = 0_u64;
    let mut skipped_files = 0_usize;
    let mut skipped_bytes = 0_u64;
    for source_path in candidates {
        let Ok(package_path) = source_path.strip_prefix(&package.canonical_root) else {
            continue;
        };
        let package_path = package_path.to_string_lossy().replace('\\', "/");
        let bytes = fs.metadata(&source_path).map_or(0, |meta| meta.len());
        if bytes > limits.max_file_bytes
            || files.len() >= limits.max_files
            || selected_bytes.saturating_add(bytes) > limits.max_bytes
        {
            skipped_files += 1;
            skipped_bytes = skipped_bytes.saturating_add(bytes);
            continue;
        }
        selected_bytes += bytes;
        files.push(PlannedFile {
            source_path,
            forced_entry: forced_entries.contains(&package_path),
            package_path,
            bytes,
        });
    }
    let status = if skipped_files == 0 {
        "complete"
    } else {
        "truncated"
    };
    Ok(PackagePlan {
        package: package.clone(),
        files,
        source_basis,
        skipped_files,
        skipped_bytes,
        status: status.into(),
    })
}

fn analysis_roots(
    package: &DiscoveredPackage,
    manifest: &serde_json::Value,
) -> (Vec<PathBuf>, BTreeSet<String>, String) {
    let mut source_targets = Vec::new();
    if let Some(source) = manifest.get("source").and_then(|value| value.as_str()) {
        source_targets.push(source.to_string());
    }
    collect_named_condition(manifest.get("exports"), "source", &mut source_targets);
    let source_paths = existing_targets(&package.canonical_root, &source_targets);
    if !source_paths.is_empty() {
        return (
            roots_for_targets(&package.canonical_root, &source_paths),
            relative_files(&package.canonical_root, &source_paths),
            "manifest-source".into(),
        );
    }

    let mut runtime_targets = Vec::new();
    if let Some(exports) = manifest.get("exports") {
        collect_runtime_targets(exports, &mut runtime_targets);
    }
    for field in ["module", "main"] {
        if let Some(target) = manifest.get(field).and_then(|value| value.as_str()) {
            runtime_targets.push(target.to_string());
        }
    }
    let runtime_paths = existing_targets(&package.canonical_root, &runtime_targets);
    if !runtime_paths.is_empty() {
        return (
            roots_for_targets(&package.canonical_root, &runtime_paths),
            relative_files(&package.canonical_root, &runtime_paths),
            "runtime".into(),
        );
    }
    (
        vec![package.canonical_root.clone()],
        BTreeSet::new(),
        "package-root".into(),
    )
}

fn collect_named_condition(value: Option<&serde_json::Value>, name: &str, out: &mut Vec<String>) {
    let Some(value) = value else { return };
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == name {
                    collect_strings(value, out);
                } else {
                    collect_named_condition(Some(value), name, out);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_named_condition(Some(value), name, out);
            }
        }
        _ => {}
    }
}

fn collect_runtime_targets(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(value) => out.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_runtime_targets(value, out);
            }
        }
        serde_json::Value::Object(map) if map.keys().any(|key| key.starts_with('.')) => {
            for value in map.values() {
                collect_runtime_targets(value, out);
            }
        }
        serde_json::Value::Object(_) => collect_active_targets(value, out),
        _ => {}
    }
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(value) => out.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_strings(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn existing_targets(package_root: &Path, targets: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for target in targets {
        if target.contains('*') {
            let prefix = target
                .split('*')
                .next()
                .unwrap_or_default()
                .trim_start_matches("./")
                .trim_end_matches('/');
            let path = package_root.join(prefix);
            if path.is_dir() {
                paths.push(path);
            }
            continue;
        }
        let path = package_root.join(target.trim_start_matches("./"));
        if path.is_file() || path.is_dir() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn roots_for_targets(package_root: &Path, targets: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for target in targets {
        let relative = target.strip_prefix(package_root).unwrap_or(target);
        let first = relative
            .components()
            .next()
            .map(|component| component.as_os_str());
        let root = match first {
            Some(first) if relative.components().count() > 1 => package_root.join(first),
            _ if target.is_dir() => target.clone(),
            _ => package_root.to_path_buf(),
        };
        roots.push(root);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn relative_files(package_root: &Path, targets: &[PathBuf]) -> BTreeSet<String> {
    targets
        .iter()
        .filter(|path| path.is_file())
        .filter_map(|path| path.strip_prefix(package_root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn collect_indexable_files(
    root: &Path,
    out: &mut Vec<PathBuf>,
    fs: &impl FileSystem,
) -> Result<()> {
    if root.is_file() {
        if walk::is_indexable(root) {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    let entries = fs
        .read_dir(root)
        .with_context(|| format!("read dependency directory {}", root.display()))?;
    for entry in entries {
        let entry = entry
            .with_context(|| format!("read dependency directory entry in {}", root.display()))?;
        let file_type = fs
            .file_type(&entry)
            .with_context(|| format!("read dependency file type {}", entry.path().display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_indexable_files(&path, out, fs)?;
        } else if file_type.is_file() && walk::is_indexable(&path) {
            out.push(path);
        }
    }
    Ok(())
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
        Ok((
            root.join(row.get::<_, String>(0)?),
            row.get::<_, String>(1)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn owning_package_root(
    resolved: &Path,
    expected_name: &str,
    fs: &impl FileSystem,
) -> Result<Option<PathBuf>> {
    let resolved = resolved
        .canonicalize()
        .unwrap_or_else(|_| resolved.to_path_buf());
    let mut current = if resolved.is_dir() {
        resolved
    } else {
        resolved.parent().map(Path::to_path_buf).unwrap_or(resolved)
    };
    loop {
        let manifest = current.join("package.json");
        if manifest.is_file() {
            let text = fs
                .read_to_string(&manifest)
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

fn read_instance(
    root: &Path,
    package_root: &Path,
    workspace: &WorkspaceMap,
    fs: &impl FileSystem,
) -> Result<DiscoveredPackage> {
    let canonical_root = package_root
        .canonicalize()
        .with_context(|| format!("canonicalize package root {}", package_root.display()))?;
    let manifest = canonical_root.join("package.json");
    let text = fs
        .read_to_string(&manifest)
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
        version: package
            .get("version")
            .and_then(|value| value.as_str())
            .map(str::to_string),
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
    use std::io::ErrorKind;
    use std::path::Path;

    use anyhow::Result;

    use super::{DependencyLimits, DiscoveredPackage, PackagePlan, should_skip_minified};
    use crate::fs_ops::OsFileSystem;
    use crate::test_fs::{FaultFileSystem, FileOperation};
    use crate::{store, workspace::WorkspaceMap};

    fn write(path: &Path, content: &str) -> Result<()> {
        fs::create_dir_all(path.parent().expect("parent"))?;
        fs::write(path, content)?;
        Ok(())
    }

    fn workspace(root: &Path) -> Result<WorkspaceMap> {
        let inventory = crate::walk::source_inventory(root)?;
        Ok(WorkspaceMap::discover(root, &inventory.files)?.map)
    }

    fn discover(
        root: &Path,
        conn: &rusqlite::Connection,
        selectors: &[String],
        workspace: &WorkspaceMap,
    ) -> Result<Vec<DiscoveredPackage>> {
        super::discover(root, conn, selectors, workspace, &OsFileSystem)
    }

    fn plan_packages(
        packages: &[DiscoveredPackage],
        limits: DependencyLimits,
    ) -> Result<Vec<PackagePlan>> {
        super::plan_packages(packages, limits, &OsFileSystem)
    }

    fn discovered_dependency(root: &Path) -> Result<DiscoveredPackage> {
        Ok(DiscoveredPackage {
            origin: "dependency".into(),
            name: "dep".into(),
            version: Some("1.0.0".into()),
            canonical_root: root.canonicalize()?,
            locator: "node_modules/dep".into(),
            manifest_hash: "hash".into(),
        })
    }

    #[test]
    fn dependency_traversal_errors_are_not_silently_dropped() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        let manifest = root.join("package.json");
        let source_root = root.join("src");
        write(
            &manifest,
            r#"{"name":"dep","version":"1.0.0","source":"src/index.ts"}"#,
        )?;
        write(&source_root.join("index.ts"), "export default 1;\n")?;
        let package = discovered_dependency(&root)?;
        let fault_fs = FaultFileSystem::default();
        fault_fs.fail_operation(
            FileOperation::ReadDir,
            source_root.clone(),
            std::io::Error::new(ErrorKind::PermissionDenied, "injected traversal failure"),
        );

        let error =
            super::plan_packages(&[package], DependencyLimits::default(), &fault_fs).unwrap_err();

        assert!(error.to_string().contains(&format!(
            "read dependency directory {}",
            source_root.display()
        )));
        assert!(format!("{error:#}").contains("injected traversal failure"));
        Ok(())
    }

    #[test]
    fn dependency_planning_manifest_errors_are_not_silently_dropped() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        let manifest = root.join("package.json");
        write(
            &manifest,
            r#"{"name":"dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        write(&root.join("index.js"), "module.exports = 1;\n")?;
        let package = discovered_dependency(&root)?;
        let fault_fs = FaultFileSystem::default();
        fault_fs.fail_operation(
            FileOperation::ReadToString,
            manifest.clone(),
            std::io::Error::new(
                ErrorKind::PermissionDenied,
                "injected planning manifest failure",
            ),
        );

        let error =
            super::plan_packages(&[package], DependencyLimits::default(), &fault_fs).unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&format!("read dependency manifest {}", manifest.display()))
        );
        assert!(format!("{error:#}").contains("injected planning manifest failure"));
        Ok(())
    }

    #[test]
    fn dependency_metadata_errors_keep_the_existing_zero_byte_fallback() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        write(
            &root.join("package.json"),
            r#"{"name":"dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        let entry = root.join("index.js");
        write(&entry, "module.exports = 1;\n")?;
        let package = discovered_dependency(&root)?;
        let fault_fs = FaultFileSystem::default();
        fault_fs.fail_operation(
            FileOperation::Metadata,
            entry,
            std::io::Error::new(ErrorKind::PermissionDenied, "injected metadata failure"),
        );

        let plans = super::plan_packages(&[package], DependencyLimits::default(), &fault_fs)?;

        assert_eq!(plans[0].files.len(), 1);
        assert_eq!(plans[0].files[0].bytes, 0);
        assert_eq!(plans[0].status, "complete");
        Ok(())
    }

    #[test]
    fn dependency_manifest_read_errors_use_the_operation_filesystem() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path();
        let manifest = root.join("node_modules/dep/package.json");
        write(
            &manifest,
            r#"{"name":"dep","version":"1.0.0","main":"index.js"}"#,
        )?;
        write(
            &root.join("node_modules/dep/index.js"),
            "module.exports = 1;\n",
        )?;
        let conn = store::open(root)?;
        importer(&conn, root, "src/main.ts", "dep")?;
        let workspace = workspace(root)?;
        let fault_fs = FaultFileSystem::default();
        fault_fs.fail_operation(
            FileOperation::ReadToString,
            manifest.canonicalize()?,
            std::io::Error::new(ErrorKind::PermissionDenied, "injected manifest failure"),
        );

        let error =
            super::discover(root, &conn, &["dep".into()], &workspace, &fault_fs).unwrap_err();

        assert!(error.to_string().contains("read dependency manifest"));
        assert!(format!("{error:#}").contains("injected manifest failure"));
        Ok(())
    }

    fn importer(conn: &rusqlite::Connection, root: &Path, path: &str, request: &str) -> Result<()> {
        write(
            &root.join(path),
            &format!("import value from '{request}';\n"),
        )?;
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
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )?;
        write(
            &root.join("packages/work/package.json"),
            r#"{"name":"work","version":"0.1.0","main":"src/index.ts"}"#,
        )?;
        write(
            &root.join("packages/work/src/index.ts"),
            "export default 1;\n",
        )?;

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
        let workspace = workspace(root)?;
        let packages = discover(root, &conn, &["left-pad".into(), "work".into()], &workspace)?;

        assert_eq!(packages.len(), 2);
        let dependency = packages
            .iter()
            .find(|package| package.name == "left-pad")
            .unwrap();
        assert_eq!(dependency.origin, "dependency");
        assert_eq!(dependency.version.as_deref(), Some("1.3.0"));
        assert_eq!(dependency.canonical_root, external.canonicalize()?);
        let work = packages
            .iter()
            .find(|package| package.name == "work")
            .unwrap();
        assert_eq!(work.origin, "workspace");
        assert_eq!(
            work.canonical_root,
            root.join("packages/work").canonicalize()?
        );
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
            write(
                &base.join("node_modules/dep/index.js"),
                "module.exports = 1;\n",
            )?;
        }
        let conn = store::open(root)?;
        importer(&conn, root, "src/root.ts", "dep")?;
        importer(&conn, root, "packages/app/src/app.ts", "dep")?;

        let packages = discover(root, &conn, &["dep".into()], &workspace(root)?)?;
        let versions: BTreeSet<_> = packages
            .iter()
            .filter_map(|package| package.version.as_deref())
            .collect();
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
            &workspace(repo.path())?,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Plug'n'Play"));
        Ok(())
    }

    #[test]
    fn planning_prefers_manifest_source_and_enforces_deterministic_limits() -> Result<()> {
        let package_root = tempfile::tempdir()?;
        write(
            &package_root.path().join("package.json"),
            r#"{"name":"dep","version":"1.0.0","source":"src/z-entry.ts","main":"dist/index.js"}"#,
        )?;
        write(
            &package_root.path().join("src/a.ts"),
            "export const a = 1;\n",
        )?;
        write(
            &package_root.path().join("src/z-entry.ts"),
            "export const entry = 1;\n",
        )?;
        write(
            &package_root.path().join("dist/index.js"),
            "exports.entry = 1;\n",
        )?;
        let package = discovered_dependency(package_root.path())?;

        let plans = plan_packages(
            &[package],
            DependencyLimits {
                max_files: 1,
                max_bytes: 1024,
                max_file_bytes: 1024,
            },
        )?;
        let plan = &plans[0];
        assert_eq!(plan.source_basis, "manifest-source");
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].package_path, "src/z-entry.ts");
        assert!(plan.files[0].forced_entry);
        assert_eq!(plan.skipped_files, 1);
        assert_eq!(plan.status, "truncated");
        assert!(
            !plan
                .files
                .iter()
                .any(|file| file.package_path.starts_with("dist/"))
        );
        Ok(())
    }

    #[test]
    fn planning_uses_runtime_wildcard_tree_without_entering_nested_node_modules() -> Result<()> {
        let package_root = tempfile::tempdir()?;
        write(
            &package_root.path().join("package.json"),
            r#"{"name":"dep","version":"1.0.0","exports":{"./*":"./dist/*.js"}}"#,
        )?;
        write(&package_root.path().join("dist/a.js"), "exports.a = 1;\n")?;
        write(
            &package_root.path().join("src/a.ts"),
            "export const a = 1;\n",
        )?;
        write(
            &package_root
                .path()
                .join("dist/node_modules/nested/index.js"),
            "module.exports = 1;\n",
        )?;
        let package = discovered_dependency(package_root.path())?;

        let plans = plan_packages(&[package], DependencyLimits::default())?;
        let paths: Vec<&str> = plans[0]
            .files
            .iter()
            .map(|file| file.package_path.as_str())
            .collect();
        assert_eq!(plans[0].source_basis, "runtime");
        assert_eq!(paths, vec!["dist/a.js"]);
        assert!(should_skip_minified(Path::new("bundle.min.js"), "x", false));
        assert!(!should_skip_minified(Path::new("bundle.min.js"), "x", true));
        Ok(())
    }

    #[test]
    fn planning_uses_declaration_order_for_active_export_conditions() -> Result<()> {
        let package_root = tempfile::tempdir()?;
        write(
            &package_root.path().join("package.json"),
            r#"{"name":"dep","version":"1.0.0","exports":{".":{"default":"./fallback/index.js","import":"./esm/index.js"}}}"#,
        )?;
        write(
            &package_root.path().join("fallback/index.js"),
            "export const selected = true;\n",
        )?;
        write(
            &package_root.path().join("esm/index.js"),
            "export const skipped = true;\n",
        )?;
        let package = discovered_dependency(package_root.path())?;

        let plans = plan_packages(&[package], DependencyLimits::default())?;
        let paths: Vec<&str> = plans[0]
            .files
            .iter()
            .map(|file| file.package_path.as_str())
            .collect();
        assert_eq!(paths, ["fallback/index.js"]);
        Ok(())
    }
}
