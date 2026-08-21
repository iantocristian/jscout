use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs_ops::{FileSystem, OsFileSystem};

/// One-shot, path-addressed failures layered over the real filesystem.
#[derive(Default)]
pub(crate) struct FaultFileSystem {
    failures: RefCell<HashMap<PathBuf, io::Error>>,
}

impl FaultFileSystem {
    pub(crate) fn fail(&self, path: PathBuf, error: io::Error) {
        self.failures.borrow_mut().insert(path, error);
    }

    fn take_failure(&self, path: &Path) -> Option<io::Error> {
        self.failures.borrow_mut().remove(path)
    }
}

impl FileSystem for FaultFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.take_failure(path)
            .map_or_else(|| OsFileSystem.read_to_string(path), Err)
    }

    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        self.take_failure(path)
            .map_or_else(|| OsFileSystem.metadata(path), Err)
    }

    fn read_dir(&self, path: &Path) -> io::Result<fs::ReadDir> {
        self.take_failure(path)
            .map_or_else(|| OsFileSystem.read_dir(path), Err)
    }

    fn file_type(&self, entry: &fs::DirEntry) -> io::Result<fs::FileType> {
        self.take_failure(&entry.path())
            .map_or_else(|| OsFileSystem.file_type(entry), Err)
    }
}
