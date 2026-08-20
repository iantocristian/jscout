//! Monorepo workspace discovery for module resolution.
//!
//! In pnpm/yarn/npm workspaces, cross-package imports use bare package names
//! (`import { X } from 'n8n-workflow'`). Without an installed `node_modules`
//! those specifiers don't resolve — and even installed, they resolve into
//! symlinked or `dist/` paths that are never indexed. This module maps each
//! workspace package name to its in-repo source so the resolver can land such
//! imports on indexed files, and records which mappings came straight from
//! manifest data versus layout heuristics so edges carry honest provenance.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use oxc_resolver::{Alias, AliasValue};

use crate::io_policy;
use crate::package_exports::collect_active_targets;
use crate::walk;

fn read_workspace_file(path: &Path) -> io::Result<String> {
    #[cfg(test)]
    if let Some(error) = TEST_IO_FAILURES.with(|failures| failures.borrow_mut().remove(path)) {
        return Err(error);
    }
    fs::read_to_string(path)
}

fn workspace_metadata(path: &Path) -> io::Result<fs::Metadata> {
    #[cfg(test)]
    if let Some(error) = TEST_IO_FAILURES.with(|failures| failures.borrow_mut().remove(path)) {
        return Err(error);
    }
    fs::metadata(path)
}

fn read_workspace_dir(path: &Path) -> io::Result<fs::ReadDir> {
    #[cfg(test)]
    if let Some(error) = TEST_IO_FAILURES.with(|failures| failures.borrow_mut().remove(path)) {
        return Err(error);
    }
    fs::read_dir(path)
}

fn workspace_file_type(entry: &fs::DirEntry) -> io::Result<fs::FileType> {
    #[cfg(test)]
    if let Some(error) =
        TEST_IO_FAILURES.with(|failures| failures.borrow_mut().remove(&entry.path()))
    {
        return Err(error);
    }
    entry.file_type()
}

#[cfg(test)]
thread_local! {
    static TEST_IO_FAILURES: std::cell::RefCell<std::collections::HashMap<PathBuf, io::Error>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(test)]
fn inject_io_failure(path: PathBuf, error: io::Error) {
    TEST_IO_FAILURES.with(|failures| {
        failures.borrow_mut().insert(path, error);
    });
}

/// How a workspace mapping was established. `Manifest` mappings use a target
/// the package.json names directly (modulo TS extension aliasing, which the
/// resolver itself applies); `Inferred` mappings come from layout heuristics
/// (source conventions, dist-mirroring, unique-name search).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    Manifest,
    Inferred,
}

/// Workspace alias table plus the provenance needed to classify resolutions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePackage {
    pub name: String,
    pub version: Option<String>,
    pub canonical_root: PathBuf,
    pub manifest_hash: String,
}

pub struct WorkspaceMap {
    pub aliases: Alias,
    /// Declared first-party package roots. Canonical roots, rather than the
    /// path used to reach them through node_modules, establish ownership.
    pub packages: Vec<WorkspacePackage>,
    /// Exact specifiers (bare names, declared subpaths) whose mapping came
    /// straight from manifest data.
    manifest_specifiers: HashSet<String>,
    /// Every aliased workspace package name.
    package_names: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRejection {
    pub path: PathBuf,
    pub stage: &'static str,
    pub error: String,
}

pub struct WorkspaceDiscovery {
    pub map: WorkspaceMap,
    pub rejections: Vec<WorkspaceRejection>,
}

impl WorkspaceMap {
    /// Discover declared workspace members from filesystem-expanded globs and
    /// build one map for every consumer in the current operation. Indexed
    /// sources only influence alias-target preference; manifests establish
    /// package identity even when a member has no indexable source.
    pub fn discover(root: &Path, source_files: &[PathBuf]) -> Result<WorkspaceDiscovery> {
        let mut rejections = Vec::new();
        let indexed_sources = IndexedSources::new(source_files);
        let globs = checked_workspace_globs(root, &mut rejections)?;
        let dirs = checked_package_dirs(root, &globs, &mut rejections)?;
        let mut map = WorkspaceMap {
            aliases: Vec::new(),
            packages: Vec::new(),
            manifest_specifiers: HashSet::new(),
            package_names: HashSet::new(),
        };
        for dir in dirs {
            let manifest = dir.join("package.json");
            let Some(text) = classified_io(
                &manifest,
                "workspace-manifest",
                read_workspace_file(&manifest),
                &mut rejections,
            )?
            else {
                continue;
            };
            let pkg = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(pkg) => pkg,
                Err(error) => {
                    rejections.push(WorkspaceRejection {
                        path: manifest,
                        stage: "workspace-manifest",
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let Some(name) = pkg.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            if name.is_empty() || name.starts_with('.') || name.starts_with('/') {
                continue;
            }
            let Some(canonical_root) = classified_io(
                &dir,
                "workspace-canonicalize",
                dir.canonicalize(),
                &mut rejections,
            )?
            else {
                continue;
            };
            map.packages.push(WorkspacePackage {
                name: name.to_string(),
                version: pkg
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                canonical_root,
                manifest_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            });
            map.add_indexed_package(name, &dir, &pkg, &indexed_sources, &mut rejections)?;
        }
        map.aliases.sort_by(|left, right| right.0.cmp(&left.0));
        map.aliases.dedup_by(|left, right| left.0 == right.0);
        map.packages
            .sort_by(|left, right| left.canonical_root.cmp(&right.canonical_root));
        map.packages
            .dedup_by(|left, right| left.canonical_root == right.canonical_root);
        rejections.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.stage.cmp(right.stage))
                .then(left.error.cmp(&right.error))
        });
        rejections.dedup();
        Ok(WorkspaceDiscovery { map, rejections })
    }

    fn add_indexed_package(
        &mut self,
        name: &str,
        dir: &Path,
        package: &serde_json::Value,
        sources: &IndexedSources,
        rejections: &mut Vec<WorkspaceRejection>,
    ) -> Result<()> {
        self.package_names.insert(name.to_string());
        self.subpath_export_aliases(name, dir, package, sources, rejections)?;

        let src = dir.join("src");
        let mut dist_values = Vec::new();
        let mut values = Vec::new();
        if let Some((entry, origin)) = preferred_package_entry(dir, package, sources, rejections)? {
            if origin == Origin::Manifest {
                self.manifest_specifiers.insert(name.to_string());
            }
            values.push(AliasValue::Path(entry.to_string_lossy().into_owned()));
        }
        if sources.is_dir(&src) || classified_is_dir(&src, rejections)? {
            dist_values.push(AliasValue::Path(format!("{}/*", src.to_string_lossy())));
            values.push(AliasValue::Path(src.to_string_lossy().into_owned()));
        }
        dist_values.push(AliasValue::Path(format!("{}/*", dir.to_string_lossy())));
        values.push(AliasValue::Path(dir.to_string_lossy().into_owned()));
        self.aliases.push((format!("{name}/dist/*"), dist_values));
        self.aliases.push((name.to_string(), values));
        Ok(())
    }

    pub fn package_named(&self, name: &str) -> Option<&WorkspacePackage> {
        self.packages.iter().find(|package| package.name == name)
    }

    pub fn package_at_root(&self, root: &Path) -> Option<&WorkspacePackage> {
        self.packages
            .iter()
            .find(|package| package.canonical_root == root)
    }

    /// Provenance for a successfully resolved request: `resolver` when the
    /// workspace machinery wasn't involved, `workspace` for an exact
    /// manifest-backed mapping, `workspace-inferred` for heuristic mappings.
    pub fn classify(&self, request: &str) -> &'static str {
        if !self.is_workspace_request(request) {
            "resolver"
        } else if self.manifest_specifiers.contains(request) {
            "workspace"
        } else {
            "workspace-inferred"
        }
    }

    fn is_workspace_request(&self, request: &str) -> bool {
        if request.starts_with('.') || request.starts_with('/') {
            return false;
        }
        if self.package_names.contains(request) {
            return true;
        }
        request
            .match_indices('/')
            .any(|(i, _)| self.package_names.contains(&request[..i]))
    }

    /// Aliases for declared subpath exports (`"./tool": {...}`), pointing
    /// each at the source its dist target was built from.
    fn subpath_export_aliases(
        &mut self,
        name: &str,
        dir: &Path,
        pkg: &serde_json::Value,
        sources: &IndexedSources,
        rejections: &mut Vec<WorkspaceRejection>,
    ) -> Result<()> {
        let Some(serde_json::Value::Object(map)) = pkg.get("exports") else {
            return Ok(());
        };
        if !map.keys().any(|k| k.starts_with('.')) {
            return Ok(()); // Condition object: describes "." only.
        }
        for (key, value) in map {
            let Some(sub) = key.strip_prefix("./") else {
                continue;
            };
            if sub.is_empty() || sub == "package.json" {
                continue;
            }
            let mut targets = Vec::new();
            collect_active_targets(value, &mut targets);
            if sub.contains('*') {
                if let Some(entry) =
                    wildcard_subpath_alias(name, dir, sub, &targets, sources, rejections)?
                {
                    self.aliases.push(entry);
                }
            } else {
                let Some((source, origin)) =
                    preferred_subpath_source(dir, sub, &targets, sources, rejections)?
                else {
                    continue;
                };
                if origin == Origin::Manifest {
                    self.manifest_specifiers.insert(format!("{name}/{sub}"));
                }
                self.aliases.push((
                    format!("{name}/{sub}$"),
                    vec![AliasValue::Path(source.to_string_lossy().into_owned())],
                ));
            }
        }
        Ok(())
    }
}

/// Extract the `packages:` list from pnpm-workspace.yaml. A minimal parser:
/// handles the block-sequence form (with comments and quoting) and the inline
/// `packages: [a, b]` form, which is all this file uses in practice.
fn pnpm_workspace_globs(yaml: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let mut in_packages = false;
    for raw in yaml.lines() {
        let line = strip_yaml_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            // New top-level key.
            let Some((key, rest)) = line.split_once(':') else {
                in_packages = false;
                continue;
            };
            in_packages = key.trim() == "packages";
            if in_packages {
                let rest = rest.trim();
                if let Some(inline) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                    return inline
                        .split(',')
                        .map(|item| unquote(item.trim()).to_string())
                        .filter(|item| !item.is_empty())
                        .collect();
                }
            }
            continue;
        }
        if in_packages && let Some(item) = line.trim().strip_prefix('-') {
            let item = unquote(item.trim());
            if !item.is_empty() {
                globs.push(item.to_string());
            }
        }
    }
    globs
}

fn strip_yaml_comment(line: &str) -> &str {
    match line.find('#') {
        Some(0) => "",
        Some(i) if line[..i].ends_with([' ', '\t']) => &line[..i],
        _ => line,
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(s)
}

fn segments(glob: &str) -> Vec<String> {
    glob.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

fn classified_io<T>(
    path: &Path,
    stage: &'static str,
    result: io::Result<T>,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if io_policy::is_inventory_race(&error) => Ok(None),
        Err(error) if io_policy::is_retryable(&error) => {
            Err(anyhow::Error::new(error).context(format!("{stage} at {}", path.display())))
        }
        Err(error) => {
            rejections.push(WorkspaceRejection {
                path: path.to_path_buf(),
                stage,
                error: error.to_string(),
            });
            Ok(None)
        }
    }
}

fn checked_workspace_globs(
    root: &Path,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Vec<String>> {
    let pnpm = root.join("pnpm-workspace.yaml");
    if let Some(yaml) = classified_io(
        &pnpm,
        "workspace-manifest",
        read_workspace_file(&pnpm),
        rejections,
    )? {
        let globs = pnpm_workspace_globs(&yaml);
        if !globs.is_empty() {
            return Ok(globs);
        }
    }

    let manifest = root.join("package.json");
    let Some(text) = classified_io(
        &manifest,
        "workspace-manifest",
        read_workspace_file(&manifest),
        rejections,
    )?
    else {
        return Ok(Vec::new());
    };
    let package = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(package) => package,
        Err(error) => {
            rejections.push(WorkspaceRejection {
                path: manifest,
                stage: "workspace-manifest",
                error: error.to_string(),
            });
            return Ok(Vec::new());
        }
    };
    let Some(workspaces) = package.get("workspaces") else {
        return Ok(Vec::new());
    };
    let values = if workspaces.is_array() {
        workspaces
    } else if let Some(packages) = workspaces.get("packages") {
        packages
    } else {
        return Ok(Vec::new());
    };
    Ok(values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::to_string)
        .collect())
}

/// Expand workspace membership against the filesystem. Directory acquisition
/// follows the repository walk policy: checkout races disappear, systemic I/O
/// aborts the phase, and permanent boundary failures are reported and skipped.
fn checked_package_dirs(
    root: &Path,
    globs: &[String],
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Vec<PathBuf>> {
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for glob in globs {
        let glob = glob.trim().trim_start_matches("./").trim_end_matches('/');
        match glob.strip_prefix('!') {
            Some(negative) => exclude.push(segments(negative)),
            None if !glob.is_empty() => include.push(segments(glob)),
            None => {}
        }
    }
    let mut dirs = Vec::new();
    for pattern in &include {
        checked_expand_segments(root, pattern, &mut dirs, rejections)?;
    }
    dirs.sort();
    dirs.dedup();

    let mut packages = Vec::new();
    for dir in dirs {
        let relative = dir.strip_prefix(root).unwrap_or(&dir);
        let parts = relative
            .iter()
            .filter_map(|part| part.to_str())
            .collect::<Vec<_>>();
        if exclude
            .iter()
            .any(|pattern| segments_match(pattern, &parts))
        {
            continue;
        }
        let manifest = dir.join("package.json");
        let Some(metadata) = classified_io(
            &manifest,
            "workspace-manifest",
            workspace_metadata(&manifest),
            rejections,
        )?
        else {
            continue;
        };
        if metadata.is_file() {
            packages.push(dir);
        } else {
            rejections.push(WorkspaceRejection {
                path: manifest,
                stage: "workspace-manifest",
                error: "expected a regular file".to_string(),
            });
        }
    }
    Ok(packages)
}

fn checked_expand_segments(
    dir: &Path,
    pattern: &[String],
    out: &mut Vec<PathBuf>,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<()> {
    let Some(segment) = pattern.first() else {
        out.push(dir.to_path_buf());
        return Ok(());
    };
    if segment == "**" {
        checked_expand_segments(dir, &pattern[1..], out, rejections)?;
        for child in checked_child_dirs(dir, rejections)? {
            checked_expand_segments(&child, pattern, out, rejections)?;
        }
    } else if segment.contains('*') {
        for child in checked_child_dirs(dir, rejections)? {
            if child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| segment_match(segment, name))
            {
                checked_expand_segments(&child, &pattern[1..], out, rejections)?;
            }
        }
    } else {
        let child = dir.join(segment);
        let Some(metadata) = classified_io(
            &child,
            "workspace-walk",
            workspace_metadata(&child),
            rejections,
        )?
        else {
            return Ok(());
        };
        if metadata.is_dir() {
            checked_expand_segments(&child, &pattern[1..], out, rejections)?;
        }
    }
    Ok(())
}

fn checked_child_dirs(
    dir: &Path,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Vec<PathBuf>> {
    let Some(entries) = classified_io(dir, "workspace-walk", read_workspace_dir(dir), rejections)?
    else {
        return Ok(Vec::new());
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let Some(entry) = classified_io(dir, "workspace-walk", entry, rejections)? else {
            continue;
        };
        let path = entry.path();
        let Some(file_type) = classified_io(
            &path,
            "workspace-walk",
            workspace_file_type(&entry),
            rejections,
        )?
        else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') && !walk::SKIP_DIRS.contains(&name.as_ref()) {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// Match a full segment list against a `**`/`*` pattern (used for exclusions).
fn segments_match(pattern: &[String], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(seg) if seg == "**" => {
            segments_match(&pattern[1..], path)
                || (!path.is_empty() && segments_match(pattern, &path[1..]))
        }
        Some(seg) => {
            !path.is_empty()
                && segment_match(seg, path[0])
                && segments_match(&pattern[1..], &path[1..])
        }
    }
}

/// Match one path segment against a pattern where `*` matches any run of
/// characters (never crossing a `/`).
fn segment_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let Some(mut rest) = name.strip_prefix(parts[0]) else {
        return false;
    };
    let last = parts[parts.len() - 1];
    let Some(middle) = rest.strip_suffix(last) else {
        return false;
    };
    rest = middle;
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Targets of the package's root export: `exports` itself when it is a bare
/// target/condition object, or its `"."` entry in a subpath map.
fn root_export_targets(pkg: &serde_json::Value) -> Vec<String> {
    let Some(exports) = pkg.get("exports") else {
        return Vec::new();
    };
    let value = match exports {
        serde_json::Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => {
            match map.get(".") {
                Some(dot) => dot,
                None => return Vec::new(),
            }
        }
        other => other,
    };
    let mut out = Vec::new();
    collect_active_targets(value, &mut out);
    out
}

const ENTRY_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Pick the package's source entry file. Manifest targets (root export,
/// `source`/`module`/`main`) win when they name an existing source file;
/// package.json fields usually point at build output (`dist/…`) that is
/// gitignored — and never indexed even when present (walk skips those dirs) —
/// so such candidates are dropped and the search falls back to heuristics:
/// the `browser` field (not a resolver main field) and source conventions
/// (`src/index.*`, `index.*`).
fn package_entry(dir: &Path, pkg: &serde_json::Value) -> Option<(PathBuf, Origin)> {
    package_entry_with_sources(dir, pkg, &FilesystemSources)
}

fn manifest_entry_fields(pkg: &serde_json::Value) -> Vec<String> {
    let mut fields = root_export_targets(pkg);
    for field in ["source", "module", "main"] {
        if let Some(value) = pkg.get(field).and_then(|value| value.as_str()) {
            fields.push(value.to_string());
        }
    }
    fields
}

fn inferred_entry_fields(pkg: &serde_json::Value) -> Vec<String> {
    let mut fields = pkg
        .get("browser")
        .and_then(|value| value.as_str())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    fields.push("src/index".to_string());
    fields.push("index".to_string());
    fields
}

fn package_entry_with_sources<S: SourceLookup>(
    dir: &Path,
    pkg: &serde_json::Value,
    sources: &S,
) -> Option<(PathBuf, Origin)> {
    let manifest_fields = manifest_entry_fields(pkg);
    for field in &manifest_fields {
        if let Some(path) = first_existing(dir, field, sources) {
            return Some((path, Origin::Manifest));
        }
    }
    let inferred_fields = inferred_entry_fields(pkg);
    for field in &inferred_fields {
        if let Some(path) = first_existing(dir, field, sources) {
            return Some((path, Origin::Inferred));
        }
    }
    None
}

/// Preserve manifest-before-inferred entry semantics while preferring indexed
/// source over recognized build-output layouts. A manifest target outside a
/// skipped output directory keeps its historical precedence even when it is
/// not indexed. Build output itself is the final fallback for a genuinely
/// source-less package.
fn preferred_package_entry(
    dir: &Path,
    pkg: &serde_json::Value,
    sources: &IndexedSources,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<(PathBuf, Origin)>> {
    let manifest_fields = manifest_entry_fields(pkg);
    for field in &manifest_fields {
        if let Some(path) = first_existing(dir, field, sources) {
            return Ok(Some((path, Origin::Manifest)));
        }
    }
    for field in &manifest_fields {
        if let Some(path) = classified_first_existing(dir, field, false, rejections)? {
            return Ok(Some((path, Origin::Manifest)));
        }
    }
    let inferred_fields = inferred_entry_fields(pkg);
    for field in &inferred_fields {
        if let Some(path) = first_existing(dir, field, sources) {
            return Ok(Some((path, Origin::Inferred)));
        }
    }
    for field in &inferred_fields {
        if let Some(path) = classified_first_existing(dir, field, false, rejections)? {
            return Ok(Some((path, Origin::Inferred)));
        }
    }
    for field in &manifest_fields {
        if let Some(path) = classified_first_existing(dir, field, true, rejections)? {
            return Ok(Some((path, Origin::Manifest)));
        }
    }
    Ok(None)
}

/// Repository-relative source entry files named by the root/workspace package
/// manifests (with the same source-first fallback used by module resolution).
/// This is intentionally a path surface, not a second resolver.
pub fn package_entry_paths(root: &Path) -> Vec<String> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // Repository-overview scouting is diagnostic rather than a publication
    // boundary. If workspace discovery cannot complete, keep the root entry
    // surface instead of turning an overview request into an indexing error.
    let workspace_packages = WorkspaceMap::discover(root, &[])
        .map(|discovery| discovery.map.packages)
        .unwrap_or_default();
    let mut dirs = vec![canonical_root.clone()];
    dirs.extend(
        workspace_packages
            .into_iter()
            .map(|package| package.canonical_root),
    );
    dirs.sort();
    dirs.dedup();
    let mut entries = Vec::new();
    for dir in dirs {
        let Ok(text) = fs::read_to_string(dir.join("package.json")) else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some((entry, _)) = package_entry(&dir, &package) else {
            continue;
        };
        let entry = entry.canonicalize().unwrap_or(entry);
        let Ok(relative) = entry.strip_prefix(&canonical_root) else {
            continue;
        };
        entries.push(relative.to_string_lossy().replace('\\', "/"));
    }
    entries.sort();
    entries.dedup();
    entries
}

trait SourceLookup {
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn unique_source_match(&self, src: &Path, subpath: &str) -> Option<PathBuf>;
}

struct FilesystemSources;

impl SourceLookup for FilesystemSources {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn unique_source_match(&self, src: &Path, subpath: &str) -> Option<PathBuf> {
        unique_source_match(src, subpath)
    }
}

struct IndexedSources {
    files: HashSet<PathBuf>,
    directories: HashSet<PathBuf>,
}

impl IndexedSources {
    fn new(files: &[PathBuf]) -> Self {
        let files = files.iter().cloned().collect::<HashSet<_>>();
        let mut directories = HashSet::new();
        for file in &files {
            let mut parent = file.parent();
            while let Some(directory) = parent {
                if !directories.insert(directory.to_path_buf()) {
                    break;
                }
                parent = directory.parent();
            }
        }
        Self { files, directories }
    }
}

impl SourceLookup for IndexedSources {
    fn is_file(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.directories.contains(path)
    }

    fn unique_source_match(&self, src: &Path, subpath: &str) -> Option<PathBuf> {
        let mut matches = Vec::new();
        for file in &self.files {
            let Ok(relative) = file.strip_prefix(src) else {
                continue;
            };
            if relative.components().count() > 5 {
                continue;
            }
            let relative = relative.to_string_lossy().replace('\\', "/");
            let Some((stem, extension)) = relative.rsplit_once('.') else {
                continue;
            };
            if !ENTRY_EXTENSIONS.contains(&extension) {
                continue;
            }
            let parent = file
                .parent()
                .and_then(|parent| parent.strip_prefix(src).ok())
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let file_match = stem == subpath || stem.ends_with(&format!("/{subpath}"));
            let index_match = file
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "index")
                && (parent == subpath || parent.ends_with(&format!("/{subpath}")));
            if file_match || index_match {
                matches.push(file.clone());
            }
        }
        matches.sort();
        matches.dedup();
        (matches.len() == 1).then(|| matches.remove(0))
    }
}

fn first_existing<S: SourceLookup>(dir: &Path, field: &str, sources: &S) -> Option<PathBuf> {
    entry_candidates(field, false)
        .into_iter()
        .map(|candidate| dir.join(candidate))
        .find(|path| sources.is_file(path))
}

fn classified_first_existing(
    dir: &Path,
    field: &str,
    allow_build_output: bool,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<PathBuf>> {
    for candidate in entry_candidates(field, allow_build_output) {
        let path = dir.join(candidate);
        let Some(metadata) = classified_io(
            &path,
            "workspace-alias",
            workspace_metadata(&path),
            rejections,
        )?
        else {
            continue;
        };
        if metadata.is_file() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn classified_is_dir(path: &Path, rejections: &mut Vec<WorkspaceRejection>) -> Result<bool> {
    Ok(classified_io(
        path,
        "workspace-alias",
        workspace_metadata(path),
        rejections,
    )?
    .is_some_and(|metadata| metadata.is_dir()))
}

/// On-disk paths a package.json field value could denote, in preference
/// order. TS-first for `.js` values (mirrors the resolver's extension_alias);
/// every source extension for extensionless values.
fn entry_candidates(field: &str, allow_build_output: bool) -> Vec<String> {
    let path = field.trim_start_matches("./");
    if path.is_empty()
        || (!allow_build_output && path.split('/').any(|seg| walk::SKIP_DIRS.contains(&seg)))
        || path.ends_with(".d.ts")
        || path.ends_with(".d.mts")
        || path.ends_with(".d.cts")
    {
        return Vec::new();
    }
    if let Some((stem, ext)) = path.rsplit_once('.')
        && !ext.contains('/')
    {
        return match ext {
            "ts" | "tsx" | "mts" | "cts" | "jsx" => vec![path.to_string()],
            "js" => vec![
                format!("{stem}.ts"),
                format!("{stem}.tsx"),
                path.to_string(),
                format!("{stem}.jsx"),
            ],
            "mjs" => vec![format!("{stem}.mts"), path.to_string()],
            "cjs" => vec![format!("{stem}.cts"), path.to_string()],
            // Not a JS entry (e.g. "./style.css").
            _ => Vec::new(),
        };
    }
    ENTRY_EXTENSIONS
        .iter()
        .map(|ext| format!("{path}.{ext}"))
        .collect()
}

/// Directories bundlers insert between `dist/` and the mirrored source tree.
const DIST_FLAVOR_DIRS: &[&str] = &["esm", "cjs", "es", "es6", "mjs", "umd", "lib", "types"];

/// Find the source file behind one subpath export. A target that names an
/// existing source file is manifest truth. Otherwise dist targets usually
/// mirror the source tree (`dist[/flavor]/x.js` -> `src/x.ts` or `x.ts`);
/// when they don't, fall back to a unique source dir/file named after the
/// subpath (`"./define"` -> `src/sdk/define/index.ts`). Both fallbacks are
/// heuristic and reported as such.
fn preferred_subpath_source(
    dir: &Path,
    sub: &str,
    targets: &[String],
    sources: &IndexedSources,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<(PathBuf, Origin)>> {
    for target in targets {
        if let Some(path) = first_existing(dir, target, sources) {
            return Ok(Some((path, Origin::Manifest)));
        }
    }
    for target in targets {
        for tail in mirror_tails(target) {
            for base in ["src/", ""] {
                if let Some(path) = first_existing(dir, &format!("{base}{tail}"), sources) {
                    return Ok(Some((path, Origin::Inferred)));
                }
            }
        }
    }
    if let Some(path) = sources.unique_source_match(&dir.join("src"), sub) {
        return Ok(Some((path, Origin::Inferred)));
    }
    for target in targets {
        if let Some(path) = classified_first_existing(dir, target, true, rejections)? {
            return Ok(Some((path, Origin::Manifest)));
        }
    }
    for target in targets {
        for tail in mirror_tails(target) {
            for base in ["src/", ""] {
                if let Some(path) =
                    classified_first_existing(dir, &format!("{base}{tail}"), false, rejections)?
                {
                    return Ok(Some((path, Origin::Inferred)));
                }
            }
        }
    }
    Ok(
        classified_unique_source_match(&dir.join("src"), sub, rejections)?
            .map(|path| (path, Origin::Inferred)),
    )
}

/// Source-relative tails a build-output target may mirror:
/// `dist/esm/utils/x.js` -> [`esm/utils/x.js`, `utils/x.js`]. Empty for
/// targets outside build-output dirs.
fn mirror_tails(target: &str) -> Vec<String> {
    let path = target.trim_start_matches("./");
    let mut segments = path.split('/');
    let Some(first) = segments.next() else {
        return Vec::new();
    };
    let tail: Vec<&str> = segments.collect();
    if !walk::SKIP_DIRS.contains(&first) || tail.is_empty() {
        return Vec::new();
    }
    let mut tails = vec![tail.join("/")];
    if tail.len() > 1 && DIST_FLAVOR_DIRS.contains(&tail[0]) {
        tails.push(tail[1..].join("/"));
    }
    tails
}

/// Wildcard subpath export -> wildcard alias:
/// `"./*": "./dist/sdk/*.js"` becomes `name/*` -> [`<dir>/src/sdk/*`, …].
/// The target's prefix is translated the same way exact subpaths are, and
/// its suffix dropped so resolver extension/index handling picks the source
/// file. Trailing generic values keep specifiers outside the translated tree
/// resolvable (a matched-but-failing alias would otherwise block them).
fn wildcard_subpath_alias(
    name: &str,
    dir: &Path,
    sub: &str,
    targets: &[String],
    sources: &IndexedSources,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<(String, Vec<AliasValue>)>> {
    if sub.matches('*').count() != 1 {
        return Ok(None);
    }
    let dir_str = dir.to_string_lossy();
    let mut values: Vec<String> = Vec::new();
    for target in targets {
        let path = target.trim_start_matches("./");
        if path.matches('*').count() != 1 {
            continue;
        }
        let Some((prefix, _suffix)) = path.split_once('*') else {
            continue;
        };
        for translated in translate_wildcard_prefix(prefix) {
            values.push(format!("{dir_str}/{translated}*"));
        }
    }
    let src = dir.join("src");
    if sources.is_dir(&src) || classified_is_dir(&src, rejections)? {
        values.push(format!("{dir_str}/src/*"));
    }
    values.push(format!("{dir_str}/*"));
    let mut seen = HashSet::new();
    values.retain(|v| seen.insert(v.clone()));
    Ok(Some((
        format!("{name}/{sub}"),
        values.into_iter().map(AliasValue::Path).collect(),
    )))
}

/// Package-relative prefixes the wildcard target prefix may correspond to in
/// the source tree: `dist/sdk/` -> [`src/sdk/`, `sdk/`]; `src/foo/` stays
/// as-is. The prefix may end mid-segment (`dist/mod-`), which is preserved.
fn translate_wildcard_prefix(prefix: &str) -> Vec<String> {
    let (dirs, partial) = prefix
        .rsplit_once('/')
        .map_or(("", prefix), |(d, p)| (d, p));
    let segs: Vec<&str> = dirs.split('/').filter(|s| !s.is_empty()).collect();
    let dir_variants: Vec<String> = match segs.first() {
        Some(first) if walk::SKIP_DIRS.contains(first) => {
            let rest = &segs[1..];
            let mut tails: Vec<&[&str]> = vec![rest];
            if let Some(flavor) = rest.first()
                && DIST_FLAVOR_DIRS.contains(flavor)
            {
                tails.push(&rest[1..]);
            }
            tails
                .into_iter()
                .flat_map(|tail| {
                    ["src", ""].into_iter().map(move |base| {
                        let mut parts: Vec<&str> = Vec::new();
                        if !base.is_empty() {
                            parts.push(base);
                        }
                        parts.extend(tail);
                        parts.join("/")
                    })
                })
                .collect()
        }
        _ => vec![dirs.to_string()],
    };
    dir_variants
        .into_iter()
        .map(|d| {
            if d.is_empty() {
                partial.to_string()
            } else {
                format!("{d}/{partial}")
            }
        })
        .collect()
}

/// The single dir (with an index file) or module file under `src/` whose
/// path ends with `sub` — None when absent or ambiguous.
fn unique_source_match(src: &Path, sub: &str) -> Option<PathBuf> {
    let mut matches = Vec::new();
    collect_source_matches(src, src, sub, 0, &mut matches);
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

fn collect_source_matches(
    base: &Path,
    dir: &Path,
    sub: &str,
    depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if file_type.is_dir() {
            if walk::SKIP_DIRS.contains(&name) {
                continue;
            }
            if rel == sub || rel.ends_with(&format!("/{sub}")) {
                for ext in ENTRY_EXTENSIONS {
                    let index = path.join(format!("index.{ext}"));
                    if index.is_file() {
                        out.push(index);
                        break;
                    }
                }
            }
            collect_source_matches(base, &path, sub, depth + 1, out);
        } else if file_type.is_file()
            && let Some((stem, ext)) = rel.rsplit_once('.')
            && ENTRY_EXTENSIONS.contains(&ext)
            && (stem == sub || stem.ends_with(&format!("/{sub}")))
        {
            out.push(path);
        }
    }
}

fn classified_unique_source_match(
    src: &Path,
    sub: &str,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    collect_classified_source_matches(src, src, sub, 0, &mut matches, rejections)?;
    matches.sort();
    matches.dedup();
    Ok((matches.len() == 1).then(|| matches.remove(0)))
}

fn collect_classified_source_matches(
    base: &Path,
    dir: &Path,
    sub: &str,
    depth: usize,
    out: &mut Vec<PathBuf>,
    rejections: &mut Vec<WorkspaceRejection>,
) -> Result<()> {
    if depth > 4 {
        return Ok(());
    }
    let Some(entries) = classified_io(dir, "workspace-alias", read_workspace_dir(dir), rejections)?
    else {
        return Ok(());
    };
    for entry in entries {
        let Some(entry) = classified_io(dir, "workspace-alias", entry, rejections)? else {
            continue;
        };
        let path = entry.path();
        let Some(file_type) = classified_io(
            &path,
            "workspace-alias",
            workspace_file_type(&entry),
            rejections,
        )?
        else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if file_type.is_dir() {
            if walk::SKIP_DIRS.contains(&name) {
                continue;
            }
            if relative == sub || relative.ends_with(&format!("/{sub}")) {
                for extension in ENTRY_EXTENSIONS {
                    let index = path.join(format!("index.{extension}"));
                    let Some(metadata) = classified_io(
                        &index,
                        "workspace-alias",
                        workspace_metadata(&index),
                        rejections,
                    )?
                    else {
                        continue;
                    };
                    if metadata.is_file() {
                        out.push(index);
                        break;
                    }
                }
            }
            collect_classified_source_matches(base, &path, sub, depth + 1, out, rejections)?;
        } else if file_type.is_file()
            && let Some((stem, extension)) = relative.rsplit_once('.')
            && ENTRY_EXTENSIONS.contains(&extension)
            && (stem == sub || stem.ends_with(&format!("/{sub}")))
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        IndexedSources, Origin, WorkspaceMap, inject_io_failure, package_entry_paths,
        pnpm_workspace_globs, preferred_package_entry,
    };
    use oxc_resolver::AliasValue;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn discover_map(root: &Path) -> WorkspaceMap {
        let inventory = crate::walk::source_inventory(root).unwrap();
        WorkspaceMap::discover(root, &inventory.files).unwrap().map
    }

    fn alias_paths(aliases: &oxc_resolver::Alias, name: &str) -> Vec<String> {
        aliases
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, values)| {
                values
                    .iter()
                    .map(|v| match v {
                        AliasValue::Path(p) => p.clone(),
                        AliasValue::Ignore => "<ignore>".to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn checked_discovery_reports_permanently_unreadable_manifests() {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        );
        let manifest = root.join("packages/locked/package.json");
        write(&manifest, r#"{"name":"locked"}"#);
        write(
            &root.join("packages/locked/src/index.ts"),
            "export const value = 1;\n",
        );
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o000)).unwrap();

        let sources = vec![root.join("packages/locked/src/index.ts")];
        let result = WorkspaceMap::discover(root, &sources);
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
        let discovery = result.unwrap();

        assert!(discovery.map.packages.is_empty());
        assert_eq!(discovery.rejections.len(), 1);
        assert_eq!(discovery.rejections[0].path, manifest);
        assert_eq!(discovery.rejections[0].stage, "workspace-manifest");
    }

    #[cfg(unix)]
    #[test]
    fn checked_discovery_propagates_resource_exhaustion() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        );
        let manifest = root.join("packages/app/package.json");
        write(&manifest, r#"{"name":"app"}"#);
        let source = root.join("packages/app/src/index.ts");
        write(&source, "export const value = 1;\n");
        inject_io_failure(manifest, std::io::Error::from_raw_os_error(libc::EMFILE));

        let error = WorkspaceMap::discover(root, &[source])
            .err()
            .expect("resource exhaustion must abort workspace discovery");

        assert!(error.to_string().contains("workspace-manifest"));
    }

    #[test]
    fn parses_pnpm_workspace_package_lists() {
        let yaml = r#"
# workspace layout
packages:
  - packages/*
  - 'packages/@scope/*'
  - "packages/frontend/**"  # nested tree
  - '!**/fixtures/**'

catalog:
  'left-pad': ^1.0.0
"#;
        assert_eq!(
            pnpm_workspace_globs(yaml),
            vec![
                "packages/*",
                "packages/@scope/*",
                "packages/frontend/**",
                "!**/fixtures/**"
            ]
        );
        assert_eq!(
            pnpm_workspace_globs("packages: [a, 'b/c']\n"),
            vec!["a", "b/c"]
        );
    }

    #[test]
    fn source_less_members_keep_manifest_identity_aliases_and_specifiers() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        );
        write(&root.join(".gitignore"), "packages/ignored/src/\n");

        write(
            &root.join("packages/dist-only/package.json"),
            r#"{"name":"dist-only","main":"dist/index.js"}"#,
        );
        write(
            &root.join("packages/dist-only/dist/index.js"),
            "module.exports = 1;\n",
        );
        write(
            &root.join("packages/ignored/package.json"),
            r#"{"name":"ignored-source","main":"src/index.ts"}"#,
        );
        write(
            &root.join("packages/ignored/src/index.ts"),
            "export const ignored = true;\n",
        );

        let inventory = crate::walk::source_inventory(root).unwrap();
        assert!(
            inventory.files.is_empty(),
            "fixture sources must be excluded"
        );
        let discovery = WorkspaceMap::discover(root, &inventory.files).unwrap();
        let map = discovery.map;

        assert!(map.package_named("dist-only").is_some());
        assert!(map.package_named("ignored-source").is_some());
        assert_eq!(
            alias_paths(&map.aliases, "dist-only")[0],
            root.join("packages/dist-only/dist/index.js")
                .to_string_lossy()
        );
        assert_eq!(
            alias_paths(&map.aliases, "ignored-source")[0],
            root.join("packages/ignored/src/index.ts").to_string_lossy()
        );
        assert_eq!(map.classify("dist-only"), "workspace");
        assert_eq!(map.classify("ignored-source"), "workspace");
    }

    #[test]
    fn manifest_entry_precedes_an_indexed_inferred_entry() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        );
        write(&root.join(".gitignore"), "packages/lib/lib/\n");
        write(
            &root.join("packages/lib/package.json"),
            r#"{"name":"acme-lib","main":"lib/index.js"}"#,
        );
        write(
            &root.join("packages/lib/lib/index.js"),
            "module.exports = 1;\n",
        );
        write(
            &root.join("packages/lib/src/index.ts"),
            "export const source = 1;\n",
        );

        let inventory = crate::walk::source_inventory(root).unwrap();
        assert!(
            inventory
                .files
                .contains(&root.join("packages/lib/src/index.ts"))
        );
        assert!(
            !inventory
                .files
                .contains(&root.join("packages/lib/lib/index.js"))
        );
        let map = WorkspaceMap::discover(root, &inventory.files).unwrap().map;

        assert_eq!(
            alias_paths(&map.aliases, "acme-lib")[0],
            root.join("packages/lib/lib/index.js").to_string_lossy()
        );
        assert_eq!(map.classify("acme-lib"), "workspace");
    }

    #[test]
    fn package_entry_preference_matrix_preserves_source_and_manifest_semantics() {
        struct Case {
            name: &'static str,
            manifest: &'static str,
            files: &'static [&'static str],
            indexed: &'static [&'static str],
            expected: &'static str,
            origin: Origin,
        }

        let cases = [
            Case {
                name: "standard-src-and-dist",
                manifest: r#"{"main":"dist/index.js"}"#,
                files: &["dist/index.js", "src/index.ts"],
                indexed: &["src/index.ts"],
                expected: "src/index.ts",
                origin: Origin::Inferred,
            },
            Case {
                name: "unrecognized-lib-output",
                manifest: r#"{"main":"lib/index.js"}"#,
                files: &["lib/index.js", "src/index.ts"],
                indexed: &["src/index.ts"],
                expected: "lib/index.js",
                origin: Origin::Manifest,
            },
            Case {
                name: "dist-only",
                manifest: r#"{"main":"dist/index.js"}"#,
                files: &["dist/index.js"],
                indexed: &[],
                expected: "dist/index.js",
                origin: Origin::Manifest,
            },
            Case {
                name: "gitignored-source",
                manifest: r#"{"main":"src/index.ts"}"#,
                files: &["src/index.ts"],
                indexed: &[],
                expected: "src/index.ts",
                origin: Origin::Manifest,
            },
            Case {
                name: "exports-map",
                manifest: r#"{"exports":{".":{"import":"./source.ts"}}}"#,
                files: &["source.ts", "src/index.ts"],
                indexed: &["source.ts", "src/index.ts"],
                expected: "source.ts",
                origin: Origin::Manifest,
            },
        ];

        for case in cases {
            let package = tempfile::tempdir().unwrap();
            for file in case.files {
                write(&package.path().join(file), "export const value = 1;\n");
            }
            let sources = IndexedSources::new(
                &case
                    .indexed
                    .iter()
                    .map(|file| package.path().join(file))
                    .collect::<Vec<_>>(),
            );
            let manifest = serde_json::from_str(case.manifest).unwrap();
            let mut rejections = Vec::new();

            let (entry, origin) =
                preferred_package_entry(package.path(), &manifest, &sources, &mut rejections)
                    .unwrap()
                    .unwrap_or_else(|| panic!("{} produced no entry", case.name));

            assert_eq!(entry, package.path().join(case.expected), "{}", case.name);
            assert_eq!(origin, case.origin, "{}", case.name);
            assert!(rejections.is_empty(), "{}", case.name);
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_glob_expansion_applies_the_io_trichotomy() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(&root.join("packages/app/package.json"), r#"{"name":"app"}"#);
        let packages = root.join("packages");

        inject_io_failure(
            packages.clone(),
            std::io::Error::from_raw_os_error(libc::EMFILE),
        );
        let transient = WorkspaceMap::discover(root, &[])
            .err()
            .expect("resource exhaustion must abort glob expansion");
        assert!(transient.to_string().contains("workspace-walk"));

        inject_io_failure(
            packages.clone(),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let permanent = WorkspaceMap::discover(root, &[]).unwrap();
        assert!(permanent.map.packages.is_empty());
        assert!(permanent.rejections.iter().any(|rejection| {
            rejection.path == packages && rejection.stage == "workspace-walk"
        }));

        inject_io_failure(packages, std::io::Error::from(std::io::ErrorKind::NotFound));
        let race = WorkspaceMap::discover(root, &[]).unwrap();
        assert!(race.map.packages.is_empty());
        assert!(race.rejections.is_empty());
    }

    #[test]
    fn maps_pnpm_workspace_packages_to_source_entries() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n  - packages/@scope/*\n  - packages/nested/**\n  - '!**/skipme/**'\n",
        );
        // Entry hidden behind a dist-only main -> src/index.ts convention.
        write(
            &root.join("packages/workflow/package.json"),
            r#"{"name": "acme-workflow", "main": "dist/cjs/index.js"}"#,
        );
        write(
            &root.join("packages/workflow/src/index.ts"),
            "export const w = 1;\n",
        );
        // Module field pointing straight at source.
        write(
            &root.join("packages/@scope/api/package.json"),
            r#"{"name": "@scope/api", "main": "dist/index.js", "module": "src/index.ts"}"#,
        );
        write(
            &root.join("packages/@scope/api/src/index.ts"),
            "export const a = 1;\n",
        );
        // Matched by the ** glob, one level down.
        write(
            &root.join("packages/nested/deep/ui/package.json"),
            r#"{"name": "acme-ui", "exports": {".": {"import": "./dist/index.mjs"}}}"#,
        );
        write(
            &root.join("packages/nested/deep/ui/src/index.ts"),
            "export const u = 1;\n",
        );
        // Excluded by the negative glob.
        write(
            &root.join("packages/nested/skipme/pkg/package.json"),
            r#"{"name": "acme-skipped"}"#,
        );
        write(
            &root.join("packages/nested/skipme/pkg/src/index.ts"),
            "export const s = 1;\n",
        );
        // No resolvable entry and no src/ -> alias falls back to the dir only.
        write(
            &root.join("packages/binary-only/package.json"),
            r#"{"name": "acme-binary", "main": "dist/index.js"}"#,
        );

        let map = discover_map(root);
        assert_eq!(
            package_entry_paths(root),
            vec![
                "packages/@scope/api/src/index.ts",
                "packages/nested/deep/ui/src/index.ts",
                "packages/workflow/src/index.ts",
            ]
        );
        // Descending key order: every "name/…" entry precedes its bare-name
        // prefix entry, so subpath/dist aliases win before the prefix matches.
        let names: Vec<&str> = map.aliases.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "acme-workflow/dist/*",
                "acme-workflow",
                "acme-ui/dist/*",
                "acme-ui",
                "acme-binary/dist/*",
                "acme-binary",
                "@scope/api/dist/*",
                "@scope/api",
            ]
        );
        assert_eq!(
            alias_paths(&map.aliases, "acme-binary"),
            vec![
                root.join("packages/binary-only")
                    .to_string_lossy()
                    .to_string()
            ]
        );

        let workflow = alias_paths(&map.aliases, "acme-workflow");
        assert_eq!(
            workflow,
            vec![
                root.join("packages/workflow/src/index.ts")
                    .to_string_lossy()
                    .to_string(),
                root.join("packages/workflow/src")
                    .to_string_lossy()
                    .to_string(),
                root.join("packages/workflow").to_string_lossy().to_string(),
            ]
        );
        let api = alias_paths(&map.aliases, "@scope/api");
        assert_eq!(
            api[0],
            root.join("packages/@scope/api/src/index.ts")
                .to_string_lossy()
        );

        // Provenance: a field naming source directly is manifest truth; a
        // convention-recovered entry, any subpath, and non-workspace
        // requests classify accordingly.
        assert_eq!(map.classify("@scope/api"), "workspace");
        assert_eq!(map.classify("acme-workflow"), "workspace-inferred");
        assert_eq!(map.classify("acme-workflow/utils/x"), "workspace-inferred");
        assert_eq!(map.classify("lodash"), "resolver");
        assert_eq!(map.classify("./local"), "resolver");
    }

    #[test]
    fn maps_subpath_exports_to_their_sources() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        );
        write(
            &root.join("packages/sdk/package.json"),
            r#"{"name": "acme-sdk", "exports": {
                "./tool": {"types": "./dist/sdk/tool.d.ts", "default": "./dist/sdk/tool.js"},
                "./text-editor": {"import": "./dist/esm/utils/text-editor.js"},
                "./define": {"import": "./dist/define/index.mjs"},
                "./direct": {"import": "./src/direct.ts"},
                "./missing": {"import": "./dist/nowhere.js"}
            }}"#,
        );
        // "./tool": dist mirrors src -> src/sdk/tool.ts.
        write(
            &root.join("packages/sdk/src/sdk/tool.ts"),
            "export const t = 1;\n",
        );
        // "./text-editor": build flavor dir (esm/) stripped from the mirror.
        write(
            &root.join("packages/sdk/src/utils/text-editor.ts"),
            "export const e = 1;\n",
        );
        // "./define": dist does NOT mirror src; found as the unique dir named
        // "define" with an index file.
        write(
            &root.join("packages/sdk/src/sdk/define/index.ts"),
            "export const d = 1;\n",
        );
        // "./direct": target names the source file itself.
        write(
            &root.join("packages/sdk/src/direct.ts"),
            "export const x = 1;\n",
        );

        let map = discover_map(root);
        assert_eq!(
            alias_paths(&map.aliases, "acme-sdk/tool$"),
            vec![
                root.join("packages/sdk/src/sdk/tool.ts")
                    .to_string_lossy()
                    .to_string()
            ]
        );
        assert_eq!(
            alias_paths(&map.aliases, "acme-sdk/text-editor$"),
            vec![
                root.join("packages/sdk/src/utils/text-editor.ts")
                    .to_string_lossy()
                    .to_string()
            ]
        );
        assert_eq!(
            alias_paths(&map.aliases, "acme-sdk/define$"),
            vec![
                root.join("packages/sdk/src/sdk/define/index.ts")
                    .to_string_lossy()
                    .to_string()
            ]
        );
        // Unmappable subpath -> no exact alias for it.
        assert!(!map.aliases.iter().any(|(k, _)| k == "acme-sdk/missing$"));

        // Mirrored/searched mappings are inferred; direct targets are
        // manifest-backed.
        assert_eq!(map.classify("acme-sdk/tool"), "workspace-inferred");
        assert_eq!(map.classify("acme-sdk/define"), "workspace-inferred");
        assert_eq!(map.classify("acme-sdk/direct"), "workspace");
    }

    #[test]
    fn wildcard_exports_map_into_the_translated_source_tree() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        );
        write(
            &root.join("packages/lib/package.json"),
            r#"{"name": "acme-lib", "exports": {"./*": "./dist/sdk/*.js"}}"#,
        );
        // The decoy: without wildcard translation the generic src/ prefix
        // would pick src/foo.ts over the exported src/sdk/foo.ts.
        write(
            &root.join("packages/lib/src/foo.ts"),
            "export const wrong = 1;\n",
        );
        write(
            &root.join("packages/lib/src/sdk/foo.ts"),
            "export const right = 1;\n",
        );

        let map = discover_map(root);
        let dir = root.join("packages/lib");
        assert_eq!(
            alias_paths(&map.aliases, "acme-lib/*"),
            vec![
                format!("{}/src/sdk/*", dir.to_string_lossy()),
                format!("{}/sdk/*", dir.to_string_lossy()),
                format!("{}/src/*", dir.to_string_lossy()),
                format!("{}/*", dir.to_string_lossy()),
            ]
        );
    }

    #[test]
    fn conditional_exports_follow_resolver_conditions() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        );
        // browser is not an active resolver condition; node is. Declaration
        // order puts browser first — it must still lose.
        write(
            &root.join("packages/dual/package.json"),
            r#"{"name": "acme-dual", "exports": {
                ".": {"browser": "./src/browser.ts", "node": "./src/node.ts"},
                "./blocked": null
            }}"#,
        );
        write(
            &root.join("packages/dual/src/browser.ts"),
            "export const b = 1;\n",
        );
        write(
            &root.join("packages/dual/src/node.ts"),
            "export const n = 1;\n",
        );

        let map = discover_map(root);
        assert_eq!(
            alias_paths(&map.aliases, "acme-dual")[0],
            root.join("packages/dual/src/node.ts").to_string_lossy()
        );
        assert_eq!(map.classify("acme-dual"), "workspace");
        // A null target is explicitly not exported: no exact alias.
        assert!(!map.aliases.iter().any(|(k, _)| k == "acme-dual/blocked$"));
    }

    #[test]
    fn maps_package_json_workspaces_field() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("package.json"),
            r#"{"name": "root", "workspaces": {"packages": ["packages/one", "packages/star-*"]}}"#,
        );
        write(
            &root.join("packages/one/package.json"),
            r#"{"name": "one"}"#,
        );
        write(
            &root.join("packages/one/index.ts"),
            "export const one = 1;\n",
        );
        write(
            &root.join("packages/star-two/package.json"),
            r#"{"name": "two"}"#,
        );
        write(
            &root.join("packages/star-two/src/index.tsx"),
            "export const two = 2;\n",
        );

        let map = discover_map(root);
        assert_eq!(
            alias_paths(&map.aliases, "one")[0],
            root.join("packages/one/index.ts").to_string_lossy()
        );
        assert_eq!(
            alias_paths(&map.aliases, "two")[0],
            root.join("packages/star-two/src/index.tsx")
                .to_string_lossy()
        );
    }

    #[test]
    fn no_workspace_manifest_yields_no_aliases() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo.path().join("package.json"), r#"{"name": "plain"}"#);
        let map = discover_map(repo.path());
        assert!(map.aliases.is_empty());
        assert_eq!(map.classify("anything"), "resolver");
    }
}
