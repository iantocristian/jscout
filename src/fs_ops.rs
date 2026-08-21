use std::fs;
use std::io;
use std::path::Path;

/// Filesystem operations injected across source publication, classified
/// workspace discovery, and selected-dependency discovery and planning.
///
/// The production implementation delegates directly to `std::fs`. Keeping
/// this boundary explicit lets tests supply operation-local failures without
/// installing thread-local state in production modules.
///
/// This boundary deliberately excludes path canonicalization and existence
/// probes, diagnostic `package_entry_paths` traversal, resolver internals, and
/// repository walking through `ignore`; those operations retain their
/// existing owners and error policies.
pub(crate) trait FileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata>;
    fn read_dir(&self, path: &Path) -> io::Result<fs::ReadDir>;
    fn file_type(&self, entry: &fs::DirEntry) -> io::Result<fs::FileType>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OsFileSystem;

impl FileSystem for OsFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::metadata(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<fs::ReadDir> {
        fs::read_dir(path)
    }

    fn file_type(&self, entry: &fs::DirEntry) -> io::Result<fs::FileType> {
        entry.file_type()
    }
}
