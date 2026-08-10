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
use std::path::{Path, PathBuf};

use oxc_resolver::{Alias, AliasValue};

use crate::package_exports::collect_active_targets;
use crate::walk;

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

impl WorkspaceMap {
    /// Build the map for a repository root. Empty when the root declares no
    /// workspaces. Only the indexing root is consulted: when indexing a
    /// sub-package, cross-package targets live outside the root and could
    /// never match indexed files anyway.
    pub fn build(root: &Path) -> Self {
        let mut map = WorkspaceMap {
            aliases: Vec::new(),
            packages: Vec::new(),
            manifest_specifiers: HashSet::new(),
            package_names: HashSet::new(),
        };
        let globs = workspace_globs(root);
        if globs.is_empty() {
            return map;
        }
        for dir in package_dirs(root, &globs) {
            let Ok(text) = fs::read_to_string(dir.join("package.json")) else {
                continue;
            };
            let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(name) = pkg.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if name.is_empty() || name.starts_with('.') || name.starts_with('/') {
                continue;
            }
            let canonical_root = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            map.packages.push(WorkspacePackage {
                name: name.to_string(),
                version: pkg
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                canonical_root,
                manifest_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            });
            map.add_package(name, &dir, &pkg);
        }
        // A matched-but-failing prefix entry stops resolution, so each
        // package's exact/wildcard subpath entries must be consulted before
        // its bare-name prefix entry: descending key order puts every
        // "name/…" first.
        map.aliases.sort_by(|a, b| b.0.cmp(&a.0));
        map.aliases.dedup_by(|a, b| a.0 == b.0);
        map.packages
            .sort_by(|a, b| a.canonical_root.cmp(&b.canonical_root));
        map.packages
            .dedup_by(|a, b| a.canonical_root == b.canonical_root);
        map
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

    /// Alias entries for one package, three kinds:
    ///
    /// - `name/sub$` (exact) for each non-wildcard subpath export, mapped
    ///   from its dist target back to the source it was built from;
    /// - `name/…*…` (wildcard) for wildcard subpath exports and the implicit
    ///   `name/dist/*`, landing build-output paths on the source tree;
    /// - `name` (prefix) -> [source entry file, `src/` dir, package dir],
    ///   keeping whichever exist. Subpath-only packages (exports like `"./*"`
    ///   with no `"."`) get no entry file but still resolve through `src/`.
    fn add_package(&mut self, name: &str, dir: &Path, pkg: &serde_json::Value) {
        self.package_names.insert(name.to_string());
        self.subpath_export_aliases(name, dir, pkg);

        let src = dir.join("src");
        let mut dist_values = Vec::new();
        let mut values = Vec::new();
        if let Some((entry, origin)) = package_entry(dir, pkg) {
            if origin == Origin::Manifest {
                self.manifest_specifiers.insert(name.to_string());
            }
            values.push(AliasValue::Path(entry.to_string_lossy().into_owned()));
        }
        if src.is_dir() {
            dist_values.push(AliasValue::Path(format!("{}/*", src.to_string_lossy())));
            values.push(AliasValue::Path(src.to_string_lossy().into_owned()));
        }
        dist_values.push(AliasValue::Path(format!("{}/*", dir.to_string_lossy())));
        values.push(AliasValue::Path(dir.to_string_lossy().into_owned()));
        self.aliases.push((format!("{name}/dist/*"), dist_values));
        self.aliases.push((name.to_string(), values));
    }

    /// Aliases for declared subpath exports (`"./tool": {...}`), pointing
    /// each at the source its dist target was built from.
    fn subpath_export_aliases(&mut self, name: &str, dir: &Path, pkg: &serde_json::Value) {
        let Some(serde_json::Value::Object(map)) = pkg.get("exports") else {
            return;
        };
        if !map.keys().any(|k| k.starts_with('.')) {
            return; // Condition object: describes "." only.
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
                if let Some(entry) = wildcard_subpath_alias(name, dir, sub, &targets) {
                    self.aliases.push(entry);
                }
            } else if let Some((source, origin)) = subpath_source(dir, sub, &targets) {
                if origin == Origin::Manifest {
                    self.manifest_specifiers.insert(format!("{name}/{sub}"));
                }
                self.aliases.push((
                    format!("{name}/{sub}$"),
                    vec![AliasValue::Path(source.to_string_lossy().into_owned())],
                ));
            }
        }
    }
}

/// Workspace globs from `pnpm-workspace.yaml` or the root `package.json`
/// `workspaces` field (array or `{ "packages": [...] }`).
fn workspace_globs(root: &Path) -> Vec<String> {
    if let Ok(yaml) = fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let globs = pnpm_workspace_globs(&yaml);
        if !globs.is_empty() {
            return globs;
        }
    }
    package_json_workspace_globs(root).unwrap_or_default()
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

fn package_json_workspace_globs(root: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(root.join("package.json")).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&text).ok()?;
    let ws = pkg.get("workspaces")?;
    let arr = if ws.is_array() {
        ws
    } else {
        ws.get("packages")?
    };
    Some(
        arr.as_array()?
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
    )
}

/// Expand workspace globs to package directories (dirs containing a
/// package.json). Supports literal segments, `*` within a segment, `**`, and
/// leading-`!` exclusions — the shapes pnpm/yarn/npm accept in practice.
fn package_dirs(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for glob in globs {
        let glob = glob.trim().trim_start_matches("./").trim_end_matches('/');
        match glob.strip_prefix('!') {
            Some(neg) => exclude.push(segments(neg)),
            None if !glob.is_empty() => include.push(segments(glob)),
            None => {}
        }
    }
    let mut dirs = Vec::new();
    for pattern in &include {
        expand_segments(root, pattern, &mut dirs);
    }
    dirs.sort();
    dirs.dedup();
    dirs.retain(|dir| {
        let rel = dir.strip_prefix(root).unwrap_or(dir);
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        dir.join("package.json").is_file() && !exclude.iter().any(|pat| segments_match(pat, &parts))
    });
    dirs
}

fn segments(glob: &str) -> Vec<String> {
    glob.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

fn expand_segments(dir: &Path, pattern: &[String], out: &mut Vec<PathBuf>) {
    let Some(seg) = pattern.first() else {
        out.push(dir.to_path_buf());
        return;
    };
    if seg == "**" {
        // Zero or more directories.
        expand_segments(dir, &pattern[1..], out);
        for child in child_dirs(dir) {
            expand_segments(&child, pattern, out);
        }
    } else if seg.contains('*') {
        for child in child_dirs(dir) {
            if child
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| segment_match(seg, name))
            {
                expand_segments(&child, &pattern[1..], out);
            }
        }
    } else {
        let child = dir.join(seg);
        if child.is_dir() {
            expand_segments(&child, &pattern[1..], out);
        }
    }
}

/// Child directories eligible for wildcard expansion: skips hidden dirs,
/// build-output dirs the indexer never walks, and symlinks (cycle safety).
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !walk::SKIP_DIRS.contains(&name.as_ref())
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
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
    let mut manifest_fields = root_export_targets(pkg);
    for field in ["source", "module", "main"] {
        if let Some(s) = pkg.get(field).and_then(|v| v.as_str()) {
            manifest_fields.push(s.to_string());
        }
    }
    for field in &manifest_fields {
        if let Some(path) = first_existing(dir, field) {
            return Some((path, Origin::Manifest));
        }
    }
    let mut inferred_fields = Vec::new();
    if let Some(s) = pkg.get("browser").and_then(|v| v.as_str()) {
        inferred_fields.push(s.to_string());
    }
    inferred_fields.push("src/index".to_string());
    inferred_fields.push("index".to_string());
    for field in &inferred_fields {
        if let Some(path) = first_existing(dir, field) {
            return Some((path, Origin::Inferred));
        }
    }
    None
}

fn first_existing(dir: &Path, field: &str) -> Option<PathBuf> {
    entry_candidates(field)
        .into_iter()
        .map(|candidate| dir.join(candidate))
        .find(|path| path.is_file())
}

/// On-disk paths a package.json field value could denote, in preference
/// order. TS-first for `.js` values (mirrors the resolver's extension_alias);
/// every source extension for extensionless values.
fn entry_candidates(field: &str) -> Vec<String> {
    let path = field.trim_start_matches("./");
    if path.is_empty()
        || path.split('/').any(|seg| walk::SKIP_DIRS.contains(&seg))
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
fn subpath_source(dir: &Path, sub: &str, targets: &[String]) -> Option<(PathBuf, Origin)> {
    for target in targets {
        if let Some(path) = first_existing(dir, target) {
            return Some((path, Origin::Manifest));
        }
    }
    for target in targets {
        for tail in mirror_tails(target) {
            for base in ["src/", ""] {
                if let Some(path) = first_existing(dir, &format!("{base}{tail}")) {
                    return Some((path, Origin::Inferred));
                }
            }
        }
    }
    unique_source_match(&dir.join("src"), sub).map(|path| (path, Origin::Inferred))
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
) -> Option<(String, Vec<AliasValue>)> {
    if sub.matches('*').count() != 1 {
        return None;
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
    if dir.join("src").is_dir() {
        values.push(format!("{dir_str}/src/*"));
    }
    values.push(format!("{dir_str}/*"));
    let mut seen = HashSet::new();
    values.retain(|v| seen.insert(v.clone()));
    Some((
        format!("{name}/{sub}"),
        values.into_iter().map(AliasValue::Path).collect(),
    ))
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{WorkspaceMap, pnpm_workspace_globs};
    use oxc_resolver::AliasValue;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
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

        let map = WorkspaceMap::build(root);
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

        let map = WorkspaceMap::build(root);
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

        let map = WorkspaceMap::build(root);
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

        let map = WorkspaceMap::build(root);
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

        let map = WorkspaceMap::build(root);
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
        let map = WorkspaceMap::build(repo.path());
        assert!(map.aliases.is_empty());
        assert_eq!(map.classify("anything"), "resolver");
    }
}
