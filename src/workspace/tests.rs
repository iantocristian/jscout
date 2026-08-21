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
    assert!(
        permanent
            .rejections
            .iter()
            .any(|rejection| { rejection.path == packages && rejection.stage == "workspace-walk" })
    );

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
