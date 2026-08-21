use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs_ops::{FileSystem, OsFileSystem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FileOperation {
    ReadToString,
    Metadata,
    ReadDir,
    FileType,
}

/// One-shot, path-addressed failures layered over the real filesystem. A
/// failure may target either the next operation on a path or one exact kind of
/// operation, which prevents an earlier metadata probe from consuming a fault
/// intended for a later content read.
#[derive(Default)]
pub(crate) struct FaultFileSystem {
    failures: RefCell<HashMap<PathBuf, io::Error>>,
    operation_failures: RefCell<HashMap<(FileOperation, PathBuf), io::Error>>,
}

impl FaultFileSystem {
    pub(crate) fn fail(&self, path: PathBuf, error: io::Error) {
        self.failures.borrow_mut().insert(path, error);
    }

    pub(crate) fn fail_operation(&self, operation: FileOperation, path: PathBuf, error: io::Error) {
        self.operation_failures
            .borrow_mut()
            .insert((operation, path), error);
    }

    fn take_failure(&self, operation: FileOperation, path: &Path) -> Option<io::Error> {
        self.operation_failures
            .borrow_mut()
            .remove(&(operation, path.to_path_buf()))
            .or_else(|| self.failures.borrow_mut().remove(path))
    }
}

impl FileSystem for FaultFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.take_failure(FileOperation::ReadToString, path)
            .map_or_else(|| OsFileSystem.read_to_string(path), Err)
    }

    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        self.take_failure(FileOperation::Metadata, path)
            .map_or_else(|| OsFileSystem.metadata(path), Err)
    }

    fn read_dir(&self, path: &Path) -> io::Result<fs::ReadDir> {
        self.take_failure(FileOperation::ReadDir, path)
            .map_or_else(|| OsFileSystem.read_dir(path), Err)
    }

    fn file_type(&self, entry: &fs::DirEntry) -> io::Result<fs::FileType> {
        self.take_failure(FileOperation::FileType, &entry.path())
            .map_or_else(|| OsFileSystem.file_type(entry), Err)
    }
}
