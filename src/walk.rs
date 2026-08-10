use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

const EXTENSIONS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"];

/// Directories that are almost never worth indexing even when not gitignored.
pub const SKIP_DIRS: &[&str] = &["node_modules", "dist", "build", ".next", "coverage", "out"];

pub fn is_indexable(path: &Path) -> bool {
    // .d.ts files are pure type declarations — nothing at runtime, skip entirely.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
        return false;
    }
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
