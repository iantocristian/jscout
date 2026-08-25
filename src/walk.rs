use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::{Error as IgnoreError, IncrementalIgnore, WalkBuilder};

use crate::{formats, io_policy};

mod inventory;

/// Directories that are almost never worth indexing even when not gitignored.
pub const SKIP_DIRS: &[&str] = &["node_modules", "dist", ".next", "coverage", "out"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkRejection {
    pub path: PathBuf,
    pub stage: &'static str,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct SourceInventory {
    pub files: Vec<PathBuf>,
    // Keep one production/test result shape so callers that publish an
    // inventory can retain failures; file-list-only diagnostics ignore them.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "file-list-only production diagnostics intentionally ignore rejections"
        )
    )]
    pub rejections: Vec<WalkRejection>,
}

/// The shared index pass returns the code-file inventory beside one
/// plane-specific consumer output without teaching the walk layer its shape.
#[derive(Debug)]
pub(crate) struct RepositoryInventory<T> {
    pub files: Vec<PathBuf>,
    pub rejections: Vec<WalkRejection>,
    pub consumer: T,
}

/// One plane that consumes entries from the shared repository traversal.
/// Implementations own their membership, hidden-path, and extraction policy;
/// the inventory engine owns filesystem traversal and ignore handling.
pub(crate) trait RepositoryInventoryConsumer: Sized {
    type Output;

    fn is_active(&self) -> bool;
    fn path_relevant(&self, relative: &Path) -> bool;
    fn hidden_path_is_excluded(&self, relative: &Path) -> bool;
    fn record_decision(&mut self, path: &Path, subject: &str, rule: &str, detail: Option<String>);
    fn inspect_special_file(
        &self,
        relative: &Path,
        absolute: &Path,
        file_type: std::fs::FileType,
    ) -> Result<()>;
    fn inspect_regular_file(&mut self, relative: PathBuf);
    fn finish(self, root: &Path) -> Result<Self::Output>;
}

/// Path matcher configured from the same ignore policy as the source walker.
/// The watcher rebuilds this after every successful refresh so edits to ignore
/// files take effect at the same publication boundary as the new inventory.
pub struct SourcePathPolicy {
    root: PathBuf,
    matcher: IncrementalIgnore,
}

impl SourcePathPolicy {
    pub fn new(root: &Path) -> Self {
        let mut matchers = source_walk_builder(root).build_matchers();
        let matcher = matchers
            .pop()
            .expect("one ignore matcher for the source root");
        Self {
            root: root.to_path_buf(),
            matcher,
        }
    }

    /// Whether ignore files or hidden-file policy exclude this path. An ignore
    /// loading error conservatively returns false so the watcher schedules a
    /// refresh, whose inventory pass will classify and report the error.
    pub fn is_ignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let (matched, error) = self.matcher.matched_with_errors(relative, is_dir);
        error.is_none() && matched.is_ignore()
    }

    /// Apply a format's directory policy to a watch path using current source
    /// visibility. Cargo membership requires a regular, non-symlink manifest
    /// that is admitted by the same ignore/hidden policy as the source walk.
    pub fn directory_admitted(&mut self, path: &Path, format: &formats::FormatSpec) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return true;
        };
        if format.directory != formats::DirectoryPolicy::CargoTarget {
            return true;
        }
        let mut cargo_roots = BTreeSet::new();
        let mut parent = PathBuf::new();
        for component in relative.components() {
            if component.as_os_str() == "target" {
                let manifest = self.root.join(&parent).join("Cargo.toml");
                let regular = std::fs::symlink_metadata(&manifest)
                    .is_ok_and(|metadata| metadata.file_type().is_file());
                if regular
                    && !is_in_skipped_directory(&self.root, &manifest)
                    && !self.is_ignored(&manifest, false)
                {
                    cargo_roots.insert(parent.clone());
                }
            }
            parent.push(component.as_os_str());
        }
        formats::repository_directory_admitted(format, relative, &cargo_roots)
    }
}

/// Whether a path is under a directory the source walker excludes
/// deterministically. Keep event filtering on this function instead of
/// copying directory names into the watcher.
pub fn is_in_skipped_directory(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
    })
}

fn source_walk_builder(root: &Path) -> WalkBuilder {
    let filter_root = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| !is_in_skipped_directory(&filter_root, entry.path()));
    builder
}

/// Walk a repository root, honoring ignore files. Retryable traversal and
/// ignore-file I/O abort the inventory; permanent subtree failures are
/// reported and excluded so one inaccessible directory cannot wedge the
/// entire repository.
pub fn source_inventory(root: &Path) -> Result<SourceInventory> {
    let mut candidates = Vec::new();
    let mut cargo_roots = BTreeSet::new();
    let mut rejections = Vec::new();
    let walker = source_walk_builder(root).build();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path =
                    ignore_error_path(&error).map_or_else(|| root.to_path_buf(), Path::to_path_buf);
                if retryable_ignore_error(&error) || error.depth() == Some(0) {
                    return Err(error)
                        .with_context(|| format!("walk source inventory at {}", path.display()));
                }
                if !only_inventory_races(&error) {
                    rejections.push(WalkRejection {
                        path,
                        stage: "walk",
                        error: error.to_string(),
                    });
                }
                continue;
            }
        };
        if let Some(error) = entry.error() {
            if retryable_ignore_error(error) {
                return Err(anyhow::anyhow!(error.to_string())).with_context(|| {
                    format!("read ignore rules under {}", entry.path().display())
                });
            }
            if !only_inventory_races(error) {
                rejections.push(WalkRejection {
                    path: ignore_error_path(error)
                        .map_or_else(|| entry.path().to_path_buf(), Path::to_path_buf),
                    stage: "ignore",
                    error: error.to_string(),
                });
            }
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            let path = entry.into_path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if relative
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
            {
                cargo_roots.insert(relative.parent().unwrap_or(Path::new("")).to_path_buf());
            }
            if let Some(format) = formats::repository_code_for_path(&path) {
                candidates.push((path, format));
            }
        }
    }
    let mut files = candidates
        .into_iter()
        .filter_map(|(path, format)| {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            formats::repository_directory_admitted(format, relative, &cargo_roots).then_some(path)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(SourceInventory { files, rejections })
}

/// Run one deterministic repository traversal for code paths and another
/// inventory consumer. The consumer owns its output and extraction policy.
pub(crate) fn repository_inventory<C: RepositoryInventoryConsumer>(
    root: &Path,
    consumer: C,
) -> Result<RepositoryInventory<C::Output>> {
    inventory::repository_inventory(root, consumer)
}

/// Read-only diagnostic callers need the file list but do not publish a
/// structural snapshot. These callers use [`source_inventory`] so traversal
/// rejections remain visible instead of being reduced to a bare file list.
pub fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(source_inventory(root)?.files)
}

fn retryable_ignore_error(error: &IgnoreError) -> bool {
    match error {
        IgnoreError::Partial(errors) => errors.iter().any(retryable_ignore_error),
        IgnoreError::WithLineNumber { err, .. }
        | IgnoreError::WithPath { err, .. }
        | IgnoreError::WithDepth { err, .. } => retryable_ignore_error(err),
        IgnoreError::Io(error) => io_policy::is_retryable(error),
        _ => false,
    }
}

fn only_inventory_races(error: &IgnoreError) -> bool {
    match error {
        IgnoreError::Partial(errors) => {
            !errors.is_empty() && errors.iter().all(only_inventory_races)
        }
        IgnoreError::WithLineNumber { err, .. }
        | IgnoreError::WithPath { err, .. }
        | IgnoreError::WithDepth { err, .. } => only_inventory_races(err),
        IgnoreError::Io(error) => io_policy::is_inventory_race(error),
        _ => false,
    }
}

fn ignore_error_path(error: &IgnoreError) -> Option<&Path> {
    match error {
        IgnoreError::Partial(errors) => errors.iter().find_map(ignore_error_path),
        IgnoreError::WithPath { path, .. } => Some(path),
        IgnoreError::WithLineNumber { err, .. } | IgnoreError::WithDepth { err, .. } => {
            ignore_error_path(err)
        }
        IgnoreError::Loop { child, .. } => Some(child),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use anyhow::Result;

    use super::*;
    use crate::docs::corpus::{self, CorpusOptions};

    fn relative_code_inventory(
        root: &Path,
    ) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>, BTreeSet<String>)> {
        let root = root.canonicalize()?;
        let source = source_inventory(&root)?
            .files
            .into_iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect::<BTreeSet<_>>();
        let shared = corpus::repository_inventory(&root, &CorpusOptions::default())?;
        let shared_code = shared
            .files
            .into_iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect::<BTreeSet<_>>();
        let shared_docs = shared
            .documents
            .into_iter()
            .map(|document| document.file.path)
            .collect::<BTreeSet<_>>();
        Ok((source, shared_code, shared_docs))
    }

    #[test]
    fn authored_build_directories_and_declarations_are_indexable() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source_build = repo.path().join("packages/app/src/build");
        let declarations = repo.path().join("packages/app");
        let ignored_build = repo.path().join("build");
        let generated_dist = repo.path().join("packages/app/dist");
        let cargo_target = repo.path().join("target/debug");
        let authored_target = repo.path().join("src/target");
        fs::create_dir_all(repo.path().join(".git"))?;
        fs::create_dir_all(&source_build)?;
        fs::create_dir_all(&ignored_build)?;
        fs::create_dir_all(&generated_dist)?;
        fs::create_dir_all(&cargo_target)?;
        fs::create_dir_all(&authored_target)?;
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname='fixture'\n",
        )?;
        fs::write(repo.path().join(".gitignore"), "/build/\n")?;
        fs::write(source_build.join("plugin.ts"), "export const plugin = 1\n")?;
        fs::write(
            declarations.join("contracts.d.ts"),
            "export interface Contract { value: string }\n",
        )?;
        fs::write(
            ignored_build.join("generated.d.ts"),
            "export interface IgnoredGenerated {}\n",
        )?;
        fs::write(
            generated_dist.join("generated.d.ts"),
            "export interface Generated {}\n",
        )?;
        fs::write(cargo_target.join("generated.rs"), "pub fn generated() {}\n")?;
        fs::write(
            cargo_target.join("authored.ts"),
            "export const authored = 1;\n",
        )?;
        fs::write(authored_target.join("module.rs"), "pub fn authored() {}\n")?;

        let files = source_files(repo.path())?
            .into_iter()
            .map(|path| {
                path.strip_prefix(repo.path())
                    .expect("inside repo")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        assert!(files.contains(&PathBuf::from("packages/app/src/build/plugin.ts")));
        assert!(files.contains(&PathBuf::from("packages/app/contracts.d.ts")));
        assert!(!files.contains(&PathBuf::from("build/generated.d.ts")));
        assert!(!files.contains(&PathBuf::from("packages/app/dist/generated.d.ts")));
        assert!(!files.contains(&PathBuf::from("target/debug/generated.rs")));
        assert!(files.contains(&PathBuf::from("target/debug/authored.ts")));
        assert!(files.contains(&PathBuf::from("src/target/module.rs")));
        Ok(())
    }

    #[test]
    fn cargo_targets_require_a_visible_regular_manifest_in_both_inventories() -> Result<()> {
        let repo = tempfile::tempdir()?;
        for directory in [
            "target",
            "src/target",
            "packages/member/target",
            "packages/member/src/target",
            "packages/no-manifest/target",
        ] {
            fs::create_dir_all(repo.path().join(directory))?;
        }
        fs::write(repo.path().join("Cargo.toml"), "[package]\nname='root'\n")?;
        fs::write(
            repo.path().join("packages/member/Cargo.toml"),
            "[package]\nname='member'\n",
        )?;
        for path in ["target/generated.rs", "packages/member/target/generated.rs"] {
            fs::write(repo.path().join(path), "pub fn generated() {}\n")?;
        }
        for path in [
            "src/target/authored.rs",
            "packages/member/src/target/authored.rs",
            "packages/no-manifest/target/authored.rs",
        ] {
            fs::write(repo.path().join(path), "pub fn authored() {}\n")?;
        }
        fs::write(
            repo.path().join("target/authored.ts"),
            "export const authored = true;\n",
        )?;
        fs::write(
            repo.path().join("target/guide.md"),
            "# Authored documentation\n",
        )?;

        let (source, shared, docs) = relative_code_inventory(repo.path())?;
        let expected = BTreeSet::from([
            PathBuf::from("packages/member/src/target/authored.rs"),
            PathBuf::from("packages/no-manifest/target/authored.rs"),
            PathBuf::from("src/target/authored.rs"),
            PathBuf::from("target/authored.ts"),
        ]);
        assert_eq!(source, expected);
        assert_eq!(shared, expected);
        assert!(docs.contains("target/guide.md"));
        Ok(())
    }

    #[test]
    fn ignored_cargo_manifest_does_not_turn_target_into_an_output_root() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir(repo.path().join(".git"))?;
        fs::create_dir(repo.path().join("target"))?;
        fs::write(repo.path().join(".gitignore"), "/Cargo.toml\n")?;
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname='ignored'\n",
        )?;
        fs::write(
            repo.path().join("target/authored.rs"),
            "pub fn authored() {}\n",
        )?;

        let (source, shared, _) = relative_code_inventory(repo.path())?;
        let expected = BTreeSet::from([PathBuf::from("target/authored.rs")]);
        assert_eq!(source, expected);
        assert_eq!(shared, expected);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_cargo_manifest_does_not_turn_target_into_an_output_root() -> Result<()> {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir()?;
        fs::create_dir(repo.path().join("target"))?;
        fs::write(
            repo.path().join("manifest-source.toml"),
            "[package]\nname='linked'\n",
        )?;
        symlink(
            repo.path().join("manifest-source.toml"),
            repo.path().join("Cargo.toml"),
        )?;
        fs::write(
            repo.path().join("target/authored.rs"),
            "pub fn authored() {}\n",
        )?;

        let (source, shared, _) = relative_code_inventory(repo.path())?;
        let expected = BTreeSet::from([PathBuf::from("target/authored.rs")]);
        assert_eq!(source, expected);
        assert_eq!(shared, expected);
        Ok(())
    }

    #[test]
    fn shared_inventory_admits_docs_without_widening_code_inputs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join(".github/workflows"))?;
        fs::write(repo.path().join("main.ts"), "export const main = 1;\n")?;
        fs::write(repo.path().join("lib.rs"), "pub fn main() {}\n")?;
        fs::write(repo.path().join("README.md"), "# Guide\n\nCurrent text.\n")?;
        fs::write(
            repo.path().join("component.mdx"),
            "# Component\n\n<Example />\n",
        )?;
        fs::write(
            repo.path().join(".github/workflows/help.md"),
            "# Automation\n\nCurrent workflow.\n",
        )?;
        fs::write(
            repo.path().join(".github/workflows/hidden.ts"),
            "export const hidden = true;\n",
        )?;

        let root = repo.path().canonicalize()?;
        let inventory = corpus::repository_inventory(&root, &CorpusOptions::default())?;
        let code = inventory
            .files
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect::<Vec<_>>();
        let docs = inventory
            .documents
            .iter()
            .map(|document| document.file.path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(code, [PathBuf::from("lib.rs"), PathBuf::from("main.ts")]);
        assert_eq!(
            docs,
            [".github/workflows/help.md", "README.md", "component.mdx"]
        );
        assert!(
            inventory
                .decisions
                .iter()
                .all(|decision| decision.path != ".github/workflows/hidden.ts")
        );
        let source_names = source_files(repo.path())?
            .into_iter()
            .map(|path| path.file_name().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            source_names,
            [
                std::ffi::OsString::from("lib.rs"),
                std::ffi::OsString::from("main.ts")
            ],
            "the read-only code inventory remains code-only"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn shared_inventory_preserves_the_source_walker_contract() -> Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join(".git"))?;
        for directory in [
            "nested/kept",
            "nested/ignored",
            "nested/pruned/reincluded",
            ".github/workflows",
            ".hidden",
            ".source-visible",
            "packages/app/.github",
            "node_modules/pkg",
            "packages/app/dist",
        ] {
            fs::create_dir_all(repo.path().join(directory))?;
        }
        fs::write(
            repo.path().join(".gitignore"),
            concat!(
                "root-ignored.ts\n",
                "nested/ignored/\n",
                "nested/pruned/\n",
                "!nested/pruned/reincluded/\n",
                "!.source-visible/\n",
            ),
        )?;
        fs::write(
            repo.path().join("nested/.gitignore"),
            "*.ts\n!kept/reincluded.ts\n",
        )?;
        fs::write(repo.path().join(".ignore"), "ignored-by-dot-ignore.ts\n")?;

        for (path, source) in [
            ("z.ts", "export const z = 1;\n"),
            ("a.js", "exports.a = 1;\n"),
            ("root-ignored.ts", "export const ignored = 1;\n"),
            ("ignored-by-dot-ignore.ts", "export const ignored = 1;\n"),
            ("nested/hidden-by-local.ts", "export const ignored = 1;\n"),
            ("nested/kept/reincluded.ts", "export const included = 1;\n"),
            ("nested/ignored/no.ts", "export const ignored = 1;\n"),
            (
                "nested/pruned/reincluded/no.ts",
                "export const ignored = 1;\n",
            ),
            (".github/workflows/hidden.ts", "export const hidden = 1;\n"),
            (".hidden/no.ts", "export const hidden = 1;\n"),
            (
                ".source-visible/reincluded.ts",
                "export const visible = 1;\n",
            ),
            (".source-visible/private.md", "# Hidden documentation\n"),
            ("packages/app/.github/no.ts", "export const hidden = 1;\n"),
            ("node_modules/pkg/no.ts", "export const generated = 1;\n"),
            ("packages/app/dist/no.ts", "export const generated = 1;\n"),
        ] {
            fs::write(repo.path().join(path), source)?;
        }
        fs::write(repo.path().join("README.md"), "# Docs\n")?;
        symlink(repo.path().join("z.ts"), repo.path().join("linked.ts"))?;
        let fifo = repo.path().join("special.ts");
        let fifo_native = CString::new(fifo.as_os_str().as_bytes())?;
        assert_eq!(unsafe { libc::mkfifo(fifo_native.as_ptr(), 0o600) }, 0);

        let canonical_root = repo.path().canonicalize()?;
        let legacy = source_inventory(&canonical_root)?;
        let shared = corpus::repository_inventory(&canonical_root, &CorpusOptions::default())?;
        assert_eq!(shared.files, legacy.files);
        assert!(shared.files.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(shared.decisions.iter().any(|decision| {
            decision.path == ".source-visible" && decision.rule == "hidden-not-allowlisted"
        }));
        assert!(
            shared
                .decisions
                .iter()
                .all(|decision| !decision.path.starts_with(".source-visible/")),
            "documentation remains pruned even when the code plane must descend"
        );
        assert_eq!(
            shared
                .files
                .iter()
                .map(|path| path.strip_prefix(&canonical_root).unwrap())
                .collect::<Vec<_>>(),
            [
                Path::new(".source-visible/reincluded.ts"),
                Path::new("a.js"),
                Path::new("nested/kept/reincluded.ts"),
                Path::new("z.ts"),
            ]
        );
        Ok(())
    }

    #[test]
    fn shared_inventory_preserves_gitignore_repository_detection() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join(".gitignore"), "ignored-by-git.ts\n")?;
        fs::write(repo.path().join(".ignore"), "ignored-by-ignore.ts\n")?;
        fs::write(
            repo.path().join("ignored-by-git.ts"),
            "export const keptOutsideGit = true;\n",
        )?;
        fs::write(
            repo.path().join("ignored-by-ignore.ts"),
            "export const ignored = true;\n",
        )?;
        fs::write(
            repo.path().join("included.ts"),
            "export const included = true;\n",
        )?;

        let canonical_root = repo.path().canonicalize()?;
        let legacy = source_inventory(&canonical_root)?;
        let shared = corpus::repository_inventory(&canonical_root, &CorpusOptions::default())?;
        assert_eq!(shared.files, legacy.files);
        assert_eq!(
            shared
                .files
                .iter()
                .map(|path| path.strip_prefix(&canonical_root).unwrap())
                .collect::<Vec<_>>(),
            [Path::new("ignored-by-git.ts"), Path::new("included.ts")]
        );
        Ok(())
    }

    #[test]
    fn traversal_errors_are_not_silently_dropped() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let missing = repo.path().join("missing");

        assert!(source_files(&missing).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_subtree_is_reported_without_losing_accessible_files() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir()?;
        let locked = repo.path().join("locked");
        fs::create_dir_all(&locked)?;
        fs::write(repo.path().join("good.ts"), "export const good = 1;\n")?;
        fs::write(locked.join("hidden.ts"), "export const hidden = 1;\n")?;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;

        let result = source_inventory(repo.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700))?;
        let inventory = result?;

        assert_eq!(inventory.files, vec![repo.path().join("good.ts")]);
        assert_eq!(inventory.rejections.len(), 1);
        assert_eq!(inventory.rejections[0].path, locked);
        assert_eq!(inventory.rejections[0].stage, "walk");
        Ok(())
    }
}
