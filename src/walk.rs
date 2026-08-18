use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

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

/// Walk a repository root, honoring .gitignore, returning indexable source files.
pub fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !(e.file_type().is_some_and(|t| t.is_dir()) && SKIP_DIRS.contains(&name))
        })
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) && is_indexable(entry.path()) {
            files.push(entry.into_path());
        }
    }
    files.sort();
    files
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
        let generated_dist = repo.path().join("packages/app/dist");
        fs::create_dir_all(&source_build)?;
        fs::create_dir_all(&generated_dist)?;
        fs::write(source_build.join("plugin.ts"), "export const plugin = 1\n")?;
        fs::write(
            declarations.join("contracts.d.ts"),
            "export interface Contract { value: string }\n",
        )?;
        fs::write(
            generated_dist.join("generated.d.ts"),
            "export interface Generated {}\n",
        )?;

        let files = source_files(repo.path())
            .into_iter()
            .map(|path| {
                path.strip_prefix(repo.path())
                    .expect("inside repo")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        assert!(files.contains(&PathBuf::from("packages/app/src/build/plugin.ts")));
        assert!(files.contains(&PathBuf::from("packages/app/contracts.d.ts")));
        assert!(!files.contains(&PathBuf::from("packages/app/dist/generated.d.ts")));
        Ok(())
    }
}
