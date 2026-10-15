//! Retain each filesystem answer actually used by one compiler project. The
//! manifest hashes those answers, not a second read made after compilation.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tsr_jsstring::JsString;
use tsr_vfs::{Entries, Error, FileContent, FileInfo, FileSystem, SnapshotId};

use super::{Request, error, path_text};
use crate::checker::process::CheckerError;
use crate::checker::protocol::CheckerInputFile;

pub const INPUT_PREFIX: &str = "tsr-observed:v2:";

/// File names relevant to compiler directory enumeration. Explicit reads and
/// failed lookups are still recorded regardless of extension. Include JSON for
/// package/config discovery and resolveJsonModule; keep every directory below.
pub fn compiler_file_name(name: &[u8]) -> bool {
    [
        b".ts".as_slice(),
        b".tsx",
        b".mts",
        b".cts",
        b".js",
        b".jsx",
        b".mjs",
        b".cjs",
        b".json",
    ]
    .iter()
    .any(|extension| {
        name.len() >= extension.len()
            && name[name.len() - extension.len()..].eq_ignore_ascii_case(extension)
    })
}

pub fn observes_directory_entries(signature: &str) -> bool {
    signature
        .strip_prefix(INPUT_PREFIX)
        .and_then(|s| s.split_once(':'))
        .and_then(|(mask, _)| mask.parse::<u8>().ok())
        .is_some_and(|mask| mask & 4 != 0)
}

// The persistence layer treats trailing separators and `.` components as one
// path. Use that same spelling for host calls and observations, merging their
// operation masks before hashing. Preserve `..` and symbolic links.
fn input_path(path: &[u8]) -> Result<Vec<u8>, Error> {
    if tsr_bundled::is_bundled(path) {
        return Ok(path.to_vec());
    }
    let path = std::str::from_utf8(path).map_err(|_| Error::Unsupported("non-UTF-8 input path"))?;
    let path: PathBuf = Path::new(path).components().collect();
    path_text(&path)
        .map(String::into_bytes)
        .map_err(|_| Error::Unsupported("non-UTF-8 input path"))
}

#[derive(Clone, Default)]
struct Observation {
    read: Option<Result<Option<FileContent>, Error>>,
    stat: Option<Result<Option<FileInfo>, Error>>,
    entries: Option<Result<Entries, Error>>,
    realpath: Option<Result<JsString, Error>>,
}

fn part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn record<T>(
    hasher: &mut blake3::Hasher,
    result: &Result<T, Error>,
    value: impl FnOnce(&mut blake3::Hasher, &T),
) {
    match result {
        Ok(item) => {
            part(hasher, b"ok");
            value(hasher, item);
        }
        Err(error) => {
            part(hasher, b"error");
            part(hasher, error.to_string().as_bytes());
        }
    }
}

impl Observation {
    fn signature(&self) -> String {
        let mut mask = 0_u8;
        let mut hasher = blake3::Hasher::new();
        if let Some(value) = &self.read {
            mask |= 1;
            record(&mut hasher, value, |hash, file| match file {
                Some(file) => {
                    part(hash, b"file");
                    part(hash, blake3::hash(&file.raw).as_bytes());
                }
                None => part(hash, b"absent"),
            });
        }
        if let Some(value) = &self.stat {
            mask |= 2;
            record(&mut hasher, value, |hash, info| {
                part(
                    hash,
                    match info {
                        None => b"absent",
                        Some(info) if info.directory => b"directory",
                        Some(_) => b"file",
                    },
                );
            });
        }
        if let Some(value) = &self.entries {
            mask |= 4;
            record(&mut hasher, value, |hash, entries| {
                let mut relevant = BTreeSet::new();
                for (names, files) in [(&entries.files, true), (&entries.directories, false)] {
                    let mut names = names
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(JsString::as_bytes)
                        .filter(|name| !files || compiler_file_name(name))
                        .collect::<Vec<_>>();
                    names.sort_unstable();
                    part(hash, &(names.len() as u64).to_le_bytes());
                    for name in names {
                        relevant.insert(name);
                        part(hash, name);
                    }
                }
                let mut symlinks = entries
                    .symlinks
                    .iter()
                    .flatten()
                    .map(JsString::as_bytes)
                    .filter(|name| relevant.contains(name))
                    .collect::<Vec<_>>();
                symlinks.sort_unstable();
                for name in symlinks {
                    part(hash, name);
                }
            });
        }
        if let Some(value) = &self.realpath {
            mask |= 8;
            record(&mut hasher, value, |hash, path| part(hash, path.as_bytes()));
        }
        format!("{INPUT_PREFIX}{mask}:{}", hasher.finalize().to_hex())
    }
}

pub struct InputFs {
    inner: Arc<dyn FileSystem>,
    observations: Mutex<BTreeMap<Vec<u8>, Observation>>,
    request: Mutex<Arc<Request>>,
}

impl InputFs {
    pub fn new(inner: Arc<dyn FileSystem>, request: Arc<Request>) -> Self {
        Self {
            inner,
            observations: Mutex::new(BTreeMap::new()),
            request: Mutex::new(request),
        }
    }

    pub fn os(request: Arc<Request>) -> Self {
        Self::new(
            Arc::new(tsr_bundled::BundledFs::new(tsr_vfs::os::shared_fs())),
            request,
        )
    }

    /// Reuse the answers behind a parsed configuration, not its live host or
    /// request. New observations must bypass discovery's shared metadata cache.
    pub fn fork(&self, request: &Arc<Request>) -> Result<Self, CheckerError> {
        request.check()?;
        let observations = self
            .observations
            .lock()
            .map_err(|_| error("checker_failure", "input lock poisoned"))?
            .clone();
        for (path, observation) in &observations {
            request.check()?;
            if tsr_bundled::is_bundled(path) {
                continue;
            }
            let path = std::str::from_utf8(path)
                .map_err(|_| error("unsupported_path", "compiler input is not UTF-8"))?;
            let expected = observation.signature();
            if fresh_signature(Path::new(path), &expected).as_deref() != Some(expected.as_str()) {
                return Err(error(
                    "checker_input_changed",
                    format!("configuration input changed since project discovery: {path}"),
                ));
            }
        }
        request.check()?;
        Ok(Self {
            observations: Mutex::new(observations),
            ..Self::os(request.clone())
        })
    }

    pub fn set_request(&self, request: Arc<Request>) {
        *self.request.lock().expect("request lock") = request;
    }

    fn check(&self) -> Result<(), Error> {
        self.request
            .lock()
            .map_err(|_| Error::Unsupported("request lock poisoned"))?
            .check()
            .map_err(|_| Error::Unsupported("embedded checker deadline or cancellation"))
    }

    pub fn inputs(&self) -> Result<Vec<CheckerInputFile>, CheckerError> {
        self.observations
            .lock()
            .map_err(|_| error("checker_failure", "input lock poisoned"))?
            .iter()
            .filter(|(path, _)| !tsr_bundled::is_bundled(path))
            .map(|(path, observation)| {
                Ok(CheckerInputFile {
                    path: std::str::from_utf8(path)
                        .map_err(|_| error("unsupported_path", "compiler input is not UTF-8"))?
                        .into(),
                    source_hash: observation.signature(),
                })
            })
            .collect()
    }

    pub fn raw(&self, path: &Path) -> Result<Arc<[u8]>, CheckerError> {
        self.read_file(path_text(path)?.as_bytes())
            .map_err(|e| error("query_file_missing", e.to_string()))?
            .map(|file| file.raw)
            .ok_or_else(|| {
                error(
                    "query_file_missing",
                    format!("missing source: {}", path.display()),
                )
            })
    }
}

impl FileSystem for InputFs {
    fn snapshot_id(&self) -> Option<SnapshotId> {
        None
    }
    fn use_case_sensitive_file_names(&self) -> bool {
        self.inner.use_case_sensitive_file_names()
    }
    fn read_file(&self, path: &[u8]) -> Result<Option<FileContent>, Error> {
        self.check()?;
        let path = input_path(path)?;
        let mut entries = self
            .observations
            .lock()
            .map_err(|_| Error::Unsupported("input lock poisoned"))?;
        entries
            .entry(path.clone())
            .or_default()
            .read
            .get_or_insert_with(|| self.inner.read_file(&path))
            .clone()
    }
    fn stat(&self, path: &[u8]) -> Result<Option<FileInfo>, Error> {
        self.check()?;
        let path = input_path(path)?;
        let mut entries = self
            .observations
            .lock()
            .map_err(|_| Error::Unsupported("input lock poisoned"))?;
        entries
            .entry(path.clone())
            .or_default()
            .stat
            .get_or_insert_with(|| self.inner.stat(&path))
            .clone()
    }
    fn entries(&self, path: &[u8]) -> Result<Entries, Error> {
        self.check()?;
        let path = input_path(path)?;
        let mut entries = self
            .observations
            .lock()
            .map_err(|_| Error::Unsupported("input lock poisoned"))?;
        entries
            .entry(path.clone())
            .or_default()
            .entries
            .get_or_insert_with(|| self.inner.entries(&path))
            .clone()
    }
    fn realpath(&self, path: &[u8]) -> Result<JsString, Error> {
        self.check()?;
        let path = input_path(path)?;
        let mut entries = self
            .observations
            .lock()
            .map_err(|_| Error::Unsupported("input lock poisoned"))?;
        entries
            .entry(path.clone())
            .or_default()
            .realpath
            .get_or_insert_with(|| self.inner.realpath(&path))
            .clone()
    }
}

/// Re-observe only the operations named in the persisted manifest. Stat-only
/// misses and directory listings must not be mistaken for file-content hashes.
pub fn fresh_signature(path: &Path, expected: &str) -> Option<String> {
    let (mask, digest) = expected.strip_prefix(INPUT_PREFIX)?.split_once(':')?;
    let mask = mask
        .parse::<u8>()
        .ok()
        .filter(|mask| (1..=15).contains(mask))?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let path = path_text(path).ok()?;
    let host = tsr_vfs::os::fs();
    let observation = Observation {
        read: (mask & 1 != 0).then(|| host.read_file(path.as_bytes())),
        stat: (mask & 2 != 0).then(|| host.stat(path.as_bytes())),
        entries: (mask & 4 != 0).then(|| host.entries(path.as_bytes())),
        realpath: (mask & 8 != 0).then(|| host.realpath(path.as_bytes())),
    };
    Some(observation.signature())
}

pub fn inputs_fingerprint(inputs: &[CheckerInputFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    for input in inputs {
        part(&mut hasher, input.path.as_bytes());
        part(&mut hasher, input.source_hash.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Duration};

    fn request() -> Arc<Request> {
        Request::new(Duration::from_secs(30), false).unwrap()
    }

    #[test]
    fn fork_isolates_observations_and_request_lifetimes() {
        let repo = tempfile::tempdir().unwrap();
        let config = path_text(&repo.path().join("tsconfig.json")).unwrap();
        let extra = path_text(&repo.path().join("extra.ts")).unwrap();
        fs::write(&config, "{}").unwrap();
        fs::write(&extra, "export {};").unwrap();
        let original = InputFs::os(request());
        original.read_file(config.as_bytes()).unwrap();
        original.set_request(Request::new(Duration::ZERO, false).unwrap());

        let first = original.fork(&request()).unwrap();
        let second = original.fork(&request()).unwrap();
        assert_eq!(
            inputs_fingerprint(&first.inputs().unwrap()),
            inputs_fingerprint(&original.inputs().unwrap())
        );
        assert!(original.read_file(config.as_bytes()).is_err());
        assert!(first.read_file(config.as_bytes()).unwrap().is_some());
        first.read_file(extra.as_bytes()).unwrap();
        assert_eq!(first.inputs().unwrap().len(), 2);
        assert_eq!(second.inputs().unwrap().len(), 1);
        assert_eq!(original.inputs().unwrap().len(), 1);
        first.set_request(Request::new(Duration::ZERO, false).unwrap());
        assert!(first.read_file(config.as_bytes()).is_err());
        assert!(second.read_file(config.as_bytes()).unwrap().is_some());
    }

    #[test]
    fn fork_rejects_changed_configuration_directory_and_missing_inputs() {
        for changed in ["configuration", "directory", "missing"] {
            let repo = tempfile::tempdir().unwrap();
            let config = repo.path().join("tsconfig.json");
            let directory = repo.path().join("src");
            let missing = repo.path().join("missing.ts");
            fs::write(&config, "{}").unwrap();
            fs::create_dir(&directory).unwrap();
            let original = InputFs::os(request());
            original
                .read_file(path_text(&config).unwrap().as_bytes())
                .unwrap();
            original
                .entries(path_text(&directory).unwrap().as_bytes())
                .unwrap();
            original
                .stat(path_text(&missing).unwrap().as_bytes())
                .unwrap();
            match changed {
                "configuration" => fs::write(config, r#"{"compilerOptions":{"strict":true}}"#),
                "directory" => fs::write(directory.join("new.ts"), "export {};"),
                "missing" => fs::write(missing, "export {};"),
                _ => unreachable!(),
            }
            .unwrap();
            assert!(
                matches!(original.fork(&request()),
                    Err(CheckerError::Remote { code, .. }) if code == "checker_input_changed"),
                "{changed} must invalidate the captured configuration"
            );
        }
    }

    #[test]
    fn fork_does_not_inherit_unobserved_cached_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let late = path_text(&repo.path().join("late.ts")).unwrap();
        let cached = Arc::new(tsr_vfs::cached::CachedFs::new(Arc::new(
            tsr_bundled::BundledFs::new(tsr_vfs::os::shared_fs()),
        )));
        assert!(cached.stat(late.as_bytes()).unwrap().is_none());
        let original = InputFs::new(cached.clone(), request());
        let fork = original.fork(&request()).unwrap();
        fs::write(&late, "export {};").unwrap();
        assert!(cached.stat(late.as_bytes()).unwrap().is_none());
        assert!(fork.stat(late.as_bytes()).unwrap().is_some());
        assert!(fork.read_file(late.as_bytes()).unwrap().is_some());
        assert!(original.inputs().unwrap().is_empty());
    }

    #[test]
    fn fork_honors_its_own_deadline_even_without_captured_inputs() {
        let original = InputFs::os(request());
        let expired = Request::new(Duration::ZERO, false).unwrap();
        assert!(matches!(
            original.fork(&expired),
            Err(CheckerError::Timeout(_))
        ));
    }
}
