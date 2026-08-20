use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::{Error as IgnoreError, IncrementalIgnore, WalkBuilder};

use crate::io_policy;

const EXTENSIONS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"];

/// Directories that are almost never worth indexing even when not gitignored.
pub const SKIP_DIRS: &[&str] = &["node_modules", "dist", ".next", "coverage", "out"];

pub fn is_indexable(path: &Path) -> bool {
    // Authored declaration files are part of the contract plane. Generated
    // declarations under dependency/output directories are excluded by the
    // directory walker and origin policy rather than by their extension.
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkRejection {
    pub path: PathBuf,
    pub stage: &'static str,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct SourceInventory {
    pub files: Vec<PathBuf>,
    pub rejections: Vec<WalkRejection>,
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
    let mut files = Vec::new();
    let mut rejections = Vec::new();
    let walker = source_walk_builder(root).build();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = ignore_error_path(&error)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
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
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| entry.path().to_path_buf()),
                    stage: "ignore",
                    error: error.to_string(),
                });
            }
        }
        if entry.file_type().is_some_and(|t| t.is_file()) && is_indexable(entry.path()) {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(SourceInventory { files, rejections })
}

/// Read-only diagnostic callers need the file list but do not publish a
/// structural snapshot. Indexing uses [`source_inventory`] so rejections stay
/// visible in its outcome.
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
    use std::fs;

    use anyhow::Result;

    use super::*;

    #[test]
    fn authored_build_directories_and_declarations_are_indexable() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source_build = repo.path().join("packages/app/src/build");
        let declarations = repo.path().join("packages/app");
        let ignored_build = repo.path().join("build");
        let generated_dist = repo.path().join("packages/app/dist");
        fs::create_dir_all(repo.path().join(".git"))?;
        fs::create_dir_all(&source_build)?;
        fs::create_dir_all(&ignored_build)?;
        fs::create_dir_all(&generated_dist)?;
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
