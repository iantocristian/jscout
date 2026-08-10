//! Monorepo workspace discovery for module resolution.
//!
//! In pnpm/yarn/npm workspaces, cross-package imports use bare package names
//! (`import { X } from 'n8n-workflow'`). Without an installed `node_modules`
//! those specifiers don't resolve — and even installed, they resolve into
//! symlinked or `dist/` paths that are never indexed. This module maps each
//! workspace package name to its in-repo source so the resolver can land such
//! imports on indexed files.

use std::fs;
use std::path::{Path, PathBuf};

use oxc_resolver::{Alias, AliasValue};

use crate::walk;

/// Resolver aliases for every workspace package, three kinds per package:
///
/// - `name/sub$` (exact) for each non-wildcard subpath export, mapped from
///   its dist target back to the source file it was built from;
/// - `name/dist/*` (wildcard) so imports that name build output by path land
///   on the mirrored source tree;
/// - `name` (prefix) -> [source entry file, `src/` dir, package dir], keeping
///   whichever exist. Subpath-only packages (exports like `"./*"` with no
///   `"."`) get no entry file but still resolve subpaths through `src/`.
pub fn workspace_aliases(root: &Path) -> Alias {
    let globs = workspace_globs(root);
    if globs.is_empty() {
        return Vec::new();
    }
    let mut aliases: Alias = Vec::new();
    for dir in package_dirs(root, &globs) {
        let Ok(text) = fs::read_to_string(dir.join("package.json")) else { continue };
        let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(name) = pkg.get("name").and_then(|v| v.as_str()) else { continue };
        if name.is_empty() || name.starts_with('.') || name.starts_with('/') {
            continue;
        }
        subpath_export_aliases(name, &dir, &pkg, &mut aliases);

        let src = dir.join("src");
        let mut dist_values = Vec::new();
        let mut values = Vec::new();
        if let Some(entry) = package_entry(&dir, &pkg) {
            values.push(AliasValue::Path(entry.to_string_lossy().into_owned()));
        }
        if src.is_dir() {
            dist_values.push(AliasValue::Path(format!("{}/*", src.to_string_lossy())));
            values.push(AliasValue::Path(src.to_string_lossy().into_owned()));
        }
        dist_values.push(AliasValue::Path(format!("{}/*", dir.to_string_lossy())));
        values.push(AliasValue::Path(dir.to_string_lossy().into_owned()));
        aliases.push((format!("{name}/dist/*"), dist_values));
        aliases.push((name.to_string(), values));
    }
    // A matched-but-failing prefix entry stops resolution, so each package's
    // exact/wildcard subpath entries must be consulted before its bare-name
    // prefix entry: descending key order puts every "name/…" first.
    aliases.sort_by(|a, b| b.0.cmp(&a.0));
    aliases.dedup_by(|a, b| a.0 == b.0);
    aliases
}

/// Exact aliases for declared subpath exports (`"./tool": {...}`), pointing
/// each at the source file its dist target was built from.
fn subpath_export_aliases(name: &str, dir: &Path, pkg: &serde_json::Value, out: &mut Alias) {
    let Some(serde_json::Value::Object(map)) = pkg.get("exports") else { return };
    if !map.keys().any(|k| k.starts_with('.')) {
        return; // Condition object: describes "." only.
    }
    for (key, target) in map {
        let Some(sub) = key.strip_prefix("./") else { continue };
        if sub.is_empty() || sub == "package.json" || sub.contains('*') {
            continue;
        }
        let mut targets = Vec::new();
        collect_export_strings(target, &mut targets);
        if let Some(source) = subpath_source(dir, sub, &targets) {
            out.push((
                format!("{name}/{sub}$"),
                vec![AliasValue::Path(source.to_string_lossy().into_owned())],
            ));
        }
    }
}

/// Directories bundlers insert between `dist/` and the mirrored source tree.
const DIST_FLAVOR_DIRS: &[&str] = &["esm", "cjs", "es", "es6", "mjs", "umd", "lib", "types"];

/// Find the source file behind one subpath export. Dist targets usually
/// mirror the source tree (`dist[/flavor]/x.js` -> `src/x.ts` or `x.ts`);
/// when they don't, fall back to a unique source dir/file named after the
/// subpath (`"./define"` -> `src/sdk/define/index.ts`).
fn subpath_source(dir: &Path, sub: &str, targets: &[String]) -> Option<PathBuf> {
    for target in targets {
        let path = target.trim_start_matches("./");
        let mut segments = path.split('/');
        let Some(first) = segments.next() else { continue };
        let tail: Vec<&str> = segments.collect();
        let mut tails: Vec<String> = Vec::new();
        if walk::SKIP_DIRS.contains(&first) {
            if !tail.is_empty() {
                tails.push(tail.join("/"));
                if tail.len() > 1 && DIST_FLAVOR_DIRS.contains(&tail[0]) {
                    tails.push(tail[1..].join("/"));
                }
            }
        } else {
            tails.push(path.to_string());
        }
        for t in &tails {
            for base in ["src/", ""] {
                for candidate in entry_candidates(&format!("{base}{t}")) {
                    let p = dir.join(candidate);
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    unique_source_match(&dir.join("src"), sub)
}

/// The single dir (with an index file) or module file under `src/` whose
/// path ends with `sub` — None when absent or ambiguous.
fn unique_source_match(src: &Path, sub: &str) -> Option<PathBuf> {
    let mut matches = Vec::new();
    collect_source_matches(src, src, sub, 0, &mut matches);
    if matches.len() == 1 { matches.pop() } else { None }
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
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(base) else { continue };
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

/// Workspace globs from `pnpm-workspace.yaml` or the root `package.json`
/// `workspaces` field (array or `{ "packages": [...] }`). Only the indexing
/// root is consulted: when indexing a sub-package, cross-package targets live
/// outside the root and could never match indexed files anyway.
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
    let arr = if ws.is_array() { ws } else { ws.get("packages")? };
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
        dir.join("package.json").is_file()
            && !exclude.iter().any(|pat| segments_match(pat, &parts))
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
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
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
    let Some(mut rest) = name.strip_prefix(parts[0]) else { return false };
    let last = parts[parts.len() - 1];
    let Some(middle) = rest.strip_suffix(last) else { return false };
    rest = middle;
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

const ENTRY_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Pick the package's source entry file. package.json fields usually point at
/// build output (`dist/…`) that is gitignored — and never indexed even when
/// present (walk skips those dirs) — so candidates there are dropped and the
/// search falls back to source conventions (`src/index.*`, `index.*`).
fn package_entry(dir: &Path, pkg: &serde_json::Value) -> Option<PathBuf> {
    let mut fields: Vec<String> = Vec::new();
    if let Some(exports) = pkg.get("exports") {
        collect_export_strings(exports, &mut fields);
    }
    for field in ["source", "module", "main", "browser"] {
        if let Some(s) = pkg.get(field).and_then(|v| v.as_str()) {
            fields.push(s.to_string());
        }
    }
    fields.push("src/index".to_string());
    fields.push("index".to_string());
    for field in &fields {
        for candidate in entry_candidates(field) {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// String targets of the package's root export (`exports` itself, or its `"."`
/// subpath), walking condition objects. `types` conditions are skipped —
/// declaration files are never indexed.
fn collect_export_strings(exports: &serde_json::Value, out: &mut Vec<String>) {
    match exports {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_export_strings(v, out);
            }
        }
        serde_json::Value::Object(map) => {
            if map.keys().any(|k| k.starts_with('.')) {
                if let Some(dot) = map.get(".") {
                    collect_export_strings(dot, out);
                }
            } else {
                for (condition, v) in map {
                    if condition != "types" {
                        collect_export_strings(v, out);
                    }
                }
            }
        }
        _ => {}
    }
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
    ENTRY_EXTENSIONS.iter().map(|ext| format!("{path}.{ext}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{pnpm_workspace_globs, workspace_aliases};
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
        assert_eq!(pnpm_workspace_globs("packages: [a, 'b/c']\n"), vec!["a", "b/c"]);
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
        write(&root.join("packages/workflow/src/index.ts"), "export const w = 1;\n");
        // Module field pointing straight at source.
        write(
            &root.join("packages/@scope/api/package.json"),
            r#"{"name": "@scope/api", "main": "dist/index.js", "module": "src/index.ts"}"#,
        );
        write(&root.join("packages/@scope/api/src/index.ts"), "export const a = 1;\n");
        // Matched by the ** glob, one level down.
        write(
            &root.join("packages/nested/deep/ui/package.json"),
            r#"{"name": "acme-ui", "exports": {".": {"import": "./dist/index.mjs"}}}"#,
        );
        write(&root.join("packages/nested/deep/ui/src/index.ts"), "export const u = 1;\n");
        // Excluded by the negative glob.
        write(
            &root.join("packages/nested/skipme/pkg/package.json"),
            r#"{"name": "acme-skipped"}"#,
        );
        write(&root.join("packages/nested/skipme/pkg/src/index.ts"), "export const s = 1;\n");
        // No resolvable entry and no src/ -> alias falls back to the dir only.
        write(
            &root.join("packages/binary-only/package.json"),
            r#"{"name": "acme-binary", "main": "dist/index.js"}"#,
        );

        let aliases = workspace_aliases(root);
        // Descending key order: every "name/…" entry precedes its bare-name
        // prefix entry, so subpath/dist aliases win before the prefix matches.
        let names: Vec<&str> = aliases.iter().map(|(name, _)| name.as_str()).collect();
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
            alias_paths(&aliases, "acme-binary"),
            vec![root.join("packages/binary-only").to_string_lossy().to_string()]
        );

        let workflow = alias_paths(&aliases, "acme-workflow");
        assert_eq!(
            workflow,
            vec![
                root.join("packages/workflow/src/index.ts").to_string_lossy().to_string(),
                root.join("packages/workflow/src").to_string_lossy().to_string(),
                root.join("packages/workflow").to_string_lossy().to_string(),
            ]
        );
        let api = alias_paths(&aliases, "@scope/api");
        assert_eq!(api[0], root.join("packages/@scope/api/src/index.ts").to_string_lossy());
    }

    #[test]
    fn maps_subpath_exports_to_their_sources() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(&root.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n");
        write(
            &root.join("packages/sdk/package.json"),
            r#"{"name": "acme-sdk", "exports": {
                "./tool": {"types": "./dist/sdk/tool.d.ts", "default": "./dist/sdk/tool.js"},
                "./text-editor": {"import": "./dist/esm/utils/text-editor.js"},
                "./define": {"import": "./dist/define/index.mjs"},
                "./missing": {"import": "./dist/nowhere.js"}
            }}"#,
        );
        // "./tool": dist mirrors src -> src/sdk/tool.ts.
        write(&root.join("packages/sdk/src/sdk/tool.ts"), "export const t = 1;\n");
        // "./text-editor": build flavor dir (esm/) stripped from the mirror.
        write(&root.join("packages/sdk/src/utils/text-editor.ts"), "export const e = 1;\n");
        // "./define": dist does NOT mirror src; found as the unique dir named
        // "define" with an index file.
        write(&root.join("packages/sdk/src/sdk/define/index.ts"), "export const d = 1;\n");

        let aliases = workspace_aliases(root);
        assert_eq!(
            alias_paths(&aliases, "acme-sdk/tool$"),
            vec![root.join("packages/sdk/src/sdk/tool.ts").to_string_lossy().to_string()]
        );
        assert_eq!(
            alias_paths(&aliases, "acme-sdk/text-editor$"),
            vec![root.join("packages/sdk/src/utils/text-editor.ts").to_string_lossy().to_string()]
        );
        assert_eq!(
            alias_paths(&aliases, "acme-sdk/define$"),
            vec![root.join("packages/sdk/src/sdk/define/index.ts").to_string_lossy().to_string()]
        );
        // Unmappable subpath -> no exact alias for it.
        assert!(!aliases.iter().any(|(k, _)| k == "acme-sdk/missing$"));
    }

    #[test]
    fn maps_package_json_workspaces_field() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(
            &root.join("package.json"),
            r#"{"name": "root", "workspaces": {"packages": ["packages/one", "packages/star-*"]}}"#,
        );
        write(&root.join("packages/one/package.json"), r#"{"name": "one"}"#);
        write(&root.join("packages/one/index.ts"), "export const one = 1;\n");
        write(&root.join("packages/star-two/package.json"), r#"{"name": "two"}"#);
        write(&root.join("packages/star-two/src/index.tsx"), "export const two = 2;\n");

        let aliases = workspace_aliases(root);
        assert_eq!(alias_paths(&aliases, "one")[0], root.join("packages/one/index.ts").to_string_lossy());
        assert_eq!(
            alias_paths(&aliases, "two")[0],
            root.join("packages/star-two/src/index.tsx").to_string_lossy()
        );
    }

    #[test]
    fn no_workspace_manifest_yields_no_aliases() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo.path().join("package.json"), r#"{"name": "plain"}"#);
        assert!(workspace_aliases(repo.path()).is_empty());
    }
}
