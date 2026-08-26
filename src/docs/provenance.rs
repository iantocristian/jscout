use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

#[cfg(test)]
const CACHE_KEY_DOMAIN: &[u8] = b"jscout-doc-blame-cache-v2\0";
const INDEX_MEMBERSHIP_DOMAIN: &[u8] = b"jscout-doc-git-index-membership-v1\0";
const PATH_SCOPE_DOMAIN: &[u8] = b"jscout-doc-git-path-scope-v1\0";
const SHALLOW_SET_DOMAIN: &[u8] = b"jscout-doc-shallow-set-v1\0";
pub(crate) const MAX_BLAME_LOGICAL_LINES: usize = 65_536;
const MAX_BLAME_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;
const MAX_HASH_OBJECT_STDOUT_BYTES: usize = 128;

/// Git state recorded before a documentation corpus is captured and parsed.
/// Every per-file blame uses this same `HEAD` and shallow set.
#[derive(Debug, Clone)]
pub(crate) struct GitRepository {
    root: PathBuf,
    path_scope: String,
    head: String,
    index: IndexState,
    shallow: ShallowState,
}

/// Git is optional. A missing binary, non-Git root, unborn `HEAD`, or
/// unreadable Git metadata produces visible unknown provenance instead of
/// failing documentation indexing.
#[derive(Debug)]
pub(crate) enum RepositoryCapture {
    Git(GitRepository),
    Unknown(ProvenanceDiagnostic),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProvenanceDiagnostic {
    pub path: Option<String>,
    pub operation: String,
    pub detail: String,
}

/// Complete semantic key for a reusable line-blame mapping. `HEAD` itself is
/// deliberately absent: unrelated commits must not invalidate the mapping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct BlameCacheKey {
    pub path_scope: String,
    pub path: String,
    pub bytes_hash: String,
    /// Object ID after Git applies the path's current clean-conversion rules
    /// to the exact captured bytes. Raw bytes alone cannot distinguish an
    /// attributes or conversion-config change that alters blame semantics.
    pub converted_blob_oid: String,
    pub path_tip: String,
    pub shallow_fingerprint: String,
}

impl BlameCacheKey {
    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(CACHE_KEY_DOMAIN);
        hash_field(&mut hasher, self.path_scope.as_bytes());
        hash_field(&mut hasher, self.path.as_bytes());
        hash_field(&mut hasher, self.bytes_hash.as_bytes());
        hash_field(&mut hasher, self.converted_blob_oid.as_bytes());
        hash_field(&mut hasher, self.path_tip.as_bytes());
        hash_field(&mut hasher, self.shallow_fingerprint.as_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

/// A tracked document ready either for a cache lookup or for blame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlameRequest {
    pub cache_key: BlameCacheKey,
    recorded_head: String,
    recorded_index_fingerprint: String,
}

/// An untracked or newly staged path is an expected unknown result and does
/// not carry a Git-error diagnostic.
#[derive(Debug)]
pub(crate) enum DocumentPreparation {
    Tracked(BlameRequest),
    Unknown,
    Failed(ProvenanceDiagnostic),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub(crate) enum LineProvenance {
    Git {
        oid: String,
        author_time: i64,
        committer_time: i64,
    },
    WorkingTree,
    /// This OID is literally in the resolved shallow file. Porcelain's
    /// `boundary` marker is intentionally ignored.
    Shallow {
        oid: String,
    },
}

/// Attribution for every one-based Git-logical line in the captured bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlameMapping {
    pub cache_key: BlameCacheKey,
    pub lines: Vec<LineProvenance>,
}

#[derive(Debug)]
pub(crate) enum DocumentBlame {
    Attributed(BlameMapping),
    Unknown(ProvenanceDiagnostic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitChunkBasis {
    Git,
    WorkingTree,
    Unknown,
}

/// Git-derived chunk provenance. Observation provenance is a separate,
/// deferred plane and is intentionally absent here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChunkGitProvenance {
    pub chunk_ordinal: u64,
    pub basis: GitChunkBasis,
    /// Latest usable author time among contributing committed body lines. For
    /// working-tree chunks this is secondary metadata, not the rank key.
    pub author_time: Option<i64>,
    pub committer_time: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublicationValidation {
    Stable,
    Drift(RepositoryDrift),
    ConversionDrift(ConvertedBlobDrift),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvertedBlobDrift {
    pub path: String,
    pub recorded_oid: String,
    pub current_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryDrift {
    pub recorded_head: String,
    pub current_head: String,
    pub recorded_index_fingerprint: String,
    pub current_index_fingerprint: String,
    pub recorded_shallow_fingerprint: String,
    pub current_shallow_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ShallowState {
    oids: BTreeSet<String>,
    fingerprint: String,
}

#[derive(Debug, Clone)]
struct IndexState {
    paths: BTreeSet<Vec<u8>>,
    fingerprint: String,
}

impl RepositoryCapture {
    pub(crate) fn capture(root: &Path) -> Self {
        match GitRepository::try_capture(root) {
            Ok(repository) => Self::Git(repository),
            Err(error) => Self::Unknown(ProvenanceDiagnostic {
                path: None,
                operation: "capture Git documentation provenance".to_owned(),
                detail: format!("{error:#}"),
            }),
        }
    }
}

impl GitRepository {
    fn try_capture(root: &Path) -> Result<Self> {
        ensure!(
            root.is_dir(),
            "indexed root is not a directory: {}",
            root.display()
        );
        ensure_work_tree(root).context("verify indexed root is inside a Git worktree")?;
        let path_scope = read_path_scope(root).context("record Git path scope")?;
        let head = read_head(root).context("record checked-out HEAD")?;
        let index = read_index_state(root).context("record current index membership")?;
        let shallow = read_shallow_state(root).context("record shallow set")?;
        Ok(Self {
            root: root.to_path_buf(),
            path_scope,
            head,
            index,
            shallow,
        })
    }

    #[cfg(test)]
    pub(crate) fn head(&self) -> &str {
        &self.head
    }

    /// Resolve the newest commit touching `path` at the recorded head and
    /// construct the cache key before doing expensive blame work.
    pub(crate) fn prepare_document(&self, path: &str, bytes: &[u8]) -> DocumentPreparation {
        match self.try_prepare_document(path, bytes) {
            Ok(Some(request)) => DocumentPreparation::Tracked(request),
            Ok(None) => DocumentPreparation::Unknown,
            Err(error) => DocumentPreparation::Failed(file_diagnostic(
                path,
                "prepare Git documentation provenance",
                error,
            )),
        }
    }

    fn try_prepare_document(&self, path: &str, bytes: &[u8]) -> Result<Option<BlameRequest>> {
        validate_repository_relative_path(path)?;
        if !self.index.paths.contains(path.as_bytes()) {
            // `git rm --cached` leaves a physical file that is untracked even
            // though the recorded HEAD still contains a blob at the path.
            return Ok(None);
        }
        if !path_exists_at_head(&self.root, &self.head, path)
            .with_context(|| format!("check path membership at recorded HEAD for {path}"))?
        {
            // A deleted path can still have log history. If it is recreated in
            // the worktree without entering HEAD, it is untracked and has no
            // Git authorship time just like a never-committed path.
            return Ok(None);
        }
        let Some(path_tip) = read_path_tip(&self.root, &self.head, path)
            .with_context(|| format!("resolve path-tip commit for {path}"))?
        else {
            // No version of the path exists at the recorded head: untracked
            // and newly staged files have no Git authorship time.
            return Ok(None);
        };
        Ok(Some(BlameRequest {
            cache_key: BlameCacheKey {
                path_scope: self.path_scope.clone(),
                path: path.to_owned(),
                bytes_hash: blake3::hash(bytes).to_hex().to_string(),
                converted_blob_oid: read_converted_blob_oid(&self.root, path, bytes)
                    .with_context(|| format!("fingerprint Git conversion state for {path}"))?,
                path_tip,
                shallow_fingerprint: self.shallow.fingerprint.clone(),
            },
            recorded_head: self.head.clone(),
            recorded_index_fingerprint: self.index.fingerprint.clone(),
        }))
    }

    /// Attribute the exact immutable bytes supplied to corpus parsing. A
    /// command or porcelain failure becomes visible per-file unknown
    /// provenance and does not fail the repository scan.
    pub(crate) fn blame_document(&self, request: &BlameRequest, bytes: &[u8]) -> DocumentBlame {
        match self.try_blame_document(request, bytes) {
            Ok(mapping) => DocumentBlame::Attributed(mapping),
            Err(error) => DocumentBlame::Unknown(file_diagnostic(
                &request.cache_key.path,
                "blame captured documentation bytes",
                error,
            )),
        }
    }

    fn try_blame_document(&self, request: &BlameRequest, bytes: &[u8]) -> Result<BlameMapping> {
        ensure!(
            request.recorded_head == self.head,
            "blame request was prepared for a different recorded HEAD"
        );
        ensure!(
            request.recorded_index_fingerprint == self.index.fingerprint,
            "blame request was prepared for a different index membership"
        );
        ensure!(
            request.cache_key.shallow_fingerprint == self.shallow.fingerprint,
            "blame request was prepared for a different shallow set"
        );
        let bytes_hash = blake3::hash(bytes).to_hex().to_string();
        ensure!(
            request.cache_key.bytes_hash == bytes_hash,
            "captured bytes do not match the prepared blame cache key"
        );

        let expected_lines = validate_blame_input(bytes)?;
        let args = blame_arguments(&self.head, &request.cache_key.path);
        let lines = run_git_blame(&self.root, &args, bytes, expected_lines, &self.shallow.oids)?;
        Ok(BlameMapping {
            cache_key: request.cache_key.clone(),
            lines,
        })
    }

    /// Re-read all state inputs immediately before publication. Callers must
    /// abort and retry the whole immutable capture when this reports drift.
    pub(crate) fn validate_before_publication<'a, I>(
        &self,
        conversion_checks: I,
    ) -> Result<PublicationValidation>
    where
        I: IntoIterator<Item = (&'a BlameCacheKey, &'a [u8])>,
    {
        let current_head = read_head(&self.root).context("re-read checked-out HEAD")?;
        let current_index =
            read_index_state(&self.root).context("re-read current index membership")?;
        let current_shallow =
            read_shallow_state(&self.root).context("re-read resolved shallow file")?;
        if current_head != self.head
            || current_index.fingerprint != self.index.fingerprint
            || current_shallow.fingerprint != self.shallow.fingerprint
        {
            return Ok(PublicationValidation::Drift(RepositoryDrift {
                recorded_head: self.head.clone(),
                current_head,
                recorded_index_fingerprint: self.index.fingerprint.clone(),
                current_index_fingerprint: current_index.fingerprint,
                recorded_shallow_fingerprint: self.shallow.fingerprint.clone(),
                current_shallow_fingerprint: current_shallow.fingerprint,
            }));
        }
        for (key, bytes) in conversion_checks {
            ensure!(
                key.bytes_hash == blake3::hash(bytes).to_hex().as_str(),
                "captured bytes do not match the conversion revalidation key for {}",
                key.path
            );
            let current_oid = read_converted_blob_oid(&self.root, &key.path, bytes)
                .with_context(|| format!("revalidate Git conversion state for {}", key.path))?;
            if current_oid != key.converted_blob_oid {
                return Ok(PublicationValidation::ConversionDrift(ConvertedBlobDrift {
                    path: key.path.clone(),
                    recorded_oid: key.converted_blob_oid.clone(),
                    current_oid,
                }));
            }
        }
        Ok(PublicationValidation::Stable)
    }
}

impl BlameMapping {
    /// Aggregate explicitly contributing body lines. Input line numbers use
    /// Git's one-based LF model. Suppressed comments, headings, front matter,
    /// and MDX ESM preambles must not be passed.
    pub(crate) fn aggregate_chunk<I>(
        &self,
        chunk_ordinal: u64,
        contributing_lines: I,
    ) -> Result<ChunkGitProvenance>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut working_tree = false;
        let mut author_time = None;
        let mut committer_time = None;

        for line_number in contributing_lines {
            ensure!(
                line_number > 0,
                "contributing Git line numbers are one-based"
            );
            let index = usize::try_from(line_number - 1)
                .context("contributing Git line number exceeds addressable memory")?;
            let line = self.lines.get(index).with_context(|| {
                format!(
                    "contributing Git line {line_number} exceeds blamed line count {}",
                    self.lines.len()
                )
            })?;
            match line {
                LineProvenance::Git {
                    author_time: line_author,
                    committer_time: line_committer,
                    ..
                } => {
                    author_time =
                        Some(author_time.map_or(*line_author, |time: i64| time.max(*line_author)));
                    committer_time = Some(
                        committer_time
                            .map_or(*line_committer, |time: i64| time.max(*line_committer)),
                    );
                }
                LineProvenance::WorkingTree => working_tree = true,
                LineProvenance::Shallow { .. } => {}
            }
        }

        let basis = if working_tree {
            GitChunkBasis::WorkingTree
        } else if author_time.is_some() {
            GitChunkBasis::Git
        } else {
            GitChunkBasis::Unknown
        };
        Ok(ChunkGitProvenance {
            chunk_ordinal,
            basis,
            author_time,
            committer_time,
        })
    }

    pub(crate) fn aggregate_chunk_ranges<I>(
        &self,
        chunk_ordinal: u64,
        contributing_ranges: I,
    ) -> Result<ChunkGitProvenance>
    where
        I: IntoIterator<Item = (u64, u64)>,
    {
        let mut lines = Vec::new();
        for (start, end) in contributing_ranges {
            ensure!(
                start > 0 && start <= end,
                "invalid contributing Git line range"
            );
            lines.extend(start..=end);
        }
        self.aggregate_chunk(chunk_ordinal, lines)
    }
}

fn read_head(root: &Path) -> Result<String> {
    let args = [
        OsString::from("--no-replace-objects"),
        OsString::from("rev-parse"),
        OsString::from("--verify"),
        OsString::from("HEAD"),
    ];
    let output = run_git(root, &args, None)?;
    parse_single_oid(&output, "HEAD")
}

fn ensure_work_tree(root: &Path) -> Result<()> {
    let args = [
        OsString::from("--no-replace-objects"),
        OsString::from("rev-parse"),
        OsString::from("--is-inside-work-tree"),
    ];
    let output = run_git(root, &args, None)?;
    let value = output.strip_suffix(b"\n").unwrap_or(&output);
    let value = value.strip_suffix(b"\r").unwrap_or(value);
    ensure!(
        value == b"true",
        "indexed root is not inside a Git worktree"
    );
    Ok(())
}

fn read_path_scope(root: &Path) -> Result<String> {
    let args = [
        OsString::from("--no-replace-objects"),
        OsString::from("rev-parse"),
        OsString::from("--show-prefix"),
    ];
    let output = run_git(root, &args, None)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(PATH_SCOPE_DOMAIN);
    // Hash Git's raw output, including its record terminator. This keeps the
    // scope byte-stable without assuming that the worktree-relative prefix is
    // UTF-8 or contains no newline bytes.
    hash_field(&mut hasher, &output);
    Ok(hasher.finalize().to_hex().to_string())
}

fn read_path_tip(root: &Path, head: &str, path: &str) -> Result<Option<String>> {
    let args = vec![
        OsString::from("--no-replace-objects"),
        OsString::from("log"),
        OsString::from("-1"),
        OsString::from("--format=%H"),
        OsString::from(head),
        OsString::from("--"),
        OsString::from(path),
    ];
    let output = run_git(root, &args, None)?;
    if output.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    parse_single_oid(&output, "path-tip commit").map(Some)
}

fn path_exists_at_head(root: &Path, head: &str, path: &str) -> Result<bool> {
    let args = vec![
        OsString::from("--no-replace-objects"),
        OsString::from("ls-tree"),
        OsString::from("-z"),
        OsString::from(head),
        OsString::from("--"),
        OsString::from(path),
    ];
    let output = run_git(root, &args, None)?;
    if output.is_empty() {
        return Ok(false);
    }
    let mut records = output
        .split(|byte| *byte == b'\0')
        .filter(|row| !row.is_empty());
    let record = records
        .next()
        .ok_or_else(|| anyhow!("Git returned an empty ls-tree record"))?;
    ensure!(
        records.next().is_none(),
        "Git returned multiple ls-tree records for one documentation path"
    );
    let metadata_end = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| anyhow!("Git returned an invalid ls-tree record"))?;
    let fields = record[..metadata_end]
        .split(|byte| *byte == b' ')
        .collect::<Vec<_>>();
    ensure!(fields.len() == 3, "Git returned invalid ls-tree metadata");
    Ok(fields[1] == b"blob")
}

fn read_index_state(root: &Path) -> Result<IndexState> {
    let args = [
        OsString::from("--no-replace-objects"),
        OsString::from("ls-files"),
        OsString::from("--stage"),
        OsString::from("-z"),
    ];
    let output = run_git(root, &args, None)?;
    let mut paths = BTreeSet::new();
    for record in output.split(|byte| *byte == b'\0') {
        if record.is_empty() {
            continue;
        }
        let metadata_end = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| anyhow!("Git returned an invalid ls-files record"))?;
        let fields = record[..metadata_end]
            .split(|byte| *byte == b' ')
            .collect::<Vec<_>>();
        ensure!(fields.len() == 3, "Git returned invalid ls-files metadata");
        paths.insert(record[metadata_end + 1..].to_vec());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(INDEX_MEMBERSHIP_DOMAIN);
    for path in &paths {
        hash_field(&mut hasher, path);
    }
    Ok(IndexState {
        paths,
        fingerprint: hasher.finalize().to_hex().to_string(),
    })
}

fn read_shallow_state(root: &Path) -> Result<ShallowState> {
    let args = [
        OsString::from("--no-replace-objects"),
        OsString::from("rev-parse"),
        OsString::from("--git-path"),
        OsString::from("shallow"),
    ];
    let output = run_git(root, &args, None)?;
    let raw_path = one_output_path(&output, "resolved shallow path")?;
    let path = output_path(raw_path)?;
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read resolved shallow file {}", path.display()));
        }
    };
    let oids = parse_shallow_set(&bytes)
        .with_context(|| format!("parse resolved shallow file {}", path.display()))?;
    let fingerprint = shallow_fingerprint(&oids);
    Ok(ShallowState { oids, fingerprint })
}

fn blame_arguments(head: &str, path: &str) -> Vec<OsString> {
    vec![
        OsString::from("--no-replace-objects"),
        OsString::from("blame"),
        OsString::from("--line-porcelain"),
        OsString::from("--no-ignore-revs-file"),
        OsString::from("--contents"),
        OsString::from("-"),
        OsString::from(head),
        OsString::from("--"),
        OsString::from(path),
    ]
}

fn read_converted_blob_oid(root: &Path, path: &str, bytes: &[u8]) -> Result<String> {
    let args = vec![
        OsString::from("--no-replace-objects"),
        OsString::from("hash-object"),
        OsString::from("--stdin"),
        OsString::from("--path"),
        OsString::from(path),
    ];
    let output = run_git(root, &args, Some(bytes))?;
    parse_single_oid(&output, "converted blob")
}

fn run_git(root: &Path, args: &[OsString], stdin_bytes: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(bytes) = stdin_bytes {
        return run_git_with_bounded_input(root, args, bytes, MAX_HASH_OBJECT_STDOUT_BYTES);
    }
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        // `--` does not disable pathspec magic; every repository path here is
        // intended literally.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let child = command.spawn().with_context(|| {
        format!(
            "run Git command `{}`",
            display_git_command(args.iter().map(OsString::as_os_str))
        )
    })?;

    let output = child.wait_with_output().context("wait for Git command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Git command `{}` exited with {}: {}",
            display_git_command(args.iter().map(OsString::as_os_str)),
            output.status,
            stderr.trim()
        );
    }
    Ok(output.stdout)
}

fn run_git_with_bounded_input(
    root: &Path,
    args: &[OsString],
    stdin_bytes: &[u8],
    max_stdout_bytes: usize,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| {
        format!(
            "run Git command `{}`",
            display_git_command(args.iter().map(OsString::as_os_str))
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stderr"))?;

    std::thread::scope(|scope| -> Result<Vec<u8>> {
        let writer = scope.spawn(move || stdin.write_all(stdin_bytes));
        let stderr_reader = scope.spawn(move || read_capped_stderr(stderr));
        let stdout = read_strictly_capped(stdout, max_stdout_bytes);
        if stdout.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().context("wait for bounded Git command")?;
        let writer_result = writer
            .join()
            .map_err(|_| anyhow!("Git stdin writer panicked"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| anyhow!("Git stderr reader panicked"))?
            .context("read bounded Git command stderr")?;
        let stderr_text = String::from_utf8_lossy(&stderr.bytes);
        let truncation = if stderr.truncated {
            " [stderr truncated]"
        } else {
            ""
        };
        let stdout = stdout.with_context(|| {
            format!(
                "read bounded output from Git command `{}` (status {}): {}{}",
                display_git_command(args.iter().map(OsString::as_os_str)),
                status,
                stderr_text.trim(),
                truncation,
            )
        })?;
        // A failing filter commonly closes stdin early. Preserve Git's actual
        // stderr instead of replacing it with a secondary BrokenPipe.
        if !status.success() {
            bail!(
                "Git command `{}` exited with {}: {}{}",
                display_git_command(args.iter().map(OsString::as_os_str)),
                status,
                stderr_text.trim(),
                truncation,
            );
        }
        writer_result.context("write command input to Git")?;
        Ok(stdout)
    })
}

fn read_strictly_capped(mut reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).context("read Git stdout")?;
        if read == 0 {
            break;
        }
        ensure!(
            retained.len().saturating_add(read) <= limit,
            "Git stdout exceeds the {limit}-byte limit"
        );
        retained.extend_from_slice(&buffer[..read]);
    }
    Ok(retained)
}

struct CappedStderr {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_capped_stderr(mut reader: impl Read) -> std::io::Result<CappedStderr> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let available = MAX_GIT_STDERR_BYTES.saturating_sub(retained.len());
        let keep = available.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(CappedStderr {
        bytes: retained,
        truncated,
    })
}

fn run_git_blame(
    root: &Path,
    args: &[OsString],
    stdin_bytes: &[u8],
    expected_lines: usize,
    shallow_oids: &BTreeSet<String>,
) -> Result<Vec<LineProvenance>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| {
        format!(
            "run Git command `{}`",
            display_git_command(args.iter().map(OsString::as_os_str))
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Git child did not expose piped stderr"))?;

    std::thread::scope(|scope| -> Result<Vec<LineProvenance>> {
        let writer = scope.spawn(move || stdin.write_all(stdin_bytes));
        // Always drain stderr to EOF so a verbose Git/filter failure cannot
        // block the child, but retain only a diagnostic-sized prefix.
        let stderr_reader = scope.spawn(move || read_capped_stderr(stderr));

        let parsed = parse_line_porcelain_reader(
            BufReader::new(stdout),
            expected_lines,
            shallow_oids,
            MAX_BLAME_STDOUT_BYTES,
        );
        if parsed.is_err() {
            // Closing stdout is normally enough to stop Git, but an explicit
            // kill also handles producers that ignore a broken pipe. `wait`
            // below always reaps the process.
            let _ = child.kill();
        }
        let status = child.wait().context("wait for Git blame command")?;
        let writer_result = writer
            .join()
            .map_err(|_| anyhow!("Git stdin writer panicked"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| anyhow!("Git stderr reader panicked"))?
            .context("read Git blame stderr")?;
        let stderr_text = String::from_utf8_lossy(&stderr.bytes);
        let truncation = if stderr.truncated {
            " [stderr truncated]"
        } else {
            ""
        };

        let lines = parsed.with_context(|| {
            format!(
                "parse bounded output from Git command `{}` (status {}): {}{}",
                display_git_command(args.iter().map(OsString::as_os_str)),
                status,
                stderr_text.trim(),
                truncation,
            )
        })?;
        // Preserve Git's stderr when an early command failure also causes the
        // stdin writer to observe BrokenPipe.
        if !status.success() {
            bail!(
                "Git command `{}` exited with {}: {}{}",
                display_git_command(args.iter().map(OsString::as_os_str)),
                status,
                stderr_text.trim(),
                truncation,
            );
        }
        writer_result.context("write captured documentation bytes to Git blame")?;
        Ok(lines)
    })
}

fn display_git_command<'a>(args: impl IntoIterator<Item = &'a OsStr>) -> String {
    let mut display = String::from("git");
    for arg in args {
        display.push(' ');
        display.push_str(&arg.to_string_lossy());
    }
    display
}

#[cfg(test)]
fn parse_line_porcelain(
    output: &[u8],
    expected_lines: usize,
    shallow_oids: &BTreeSet<String>,
) -> Result<Vec<LineProvenance>> {
    parse_line_porcelain_reader(
        std::io::Cursor::new(output),
        expected_lines,
        shallow_oids,
        MAX_BLAME_STDOUT_BYTES,
    )
}

fn parse_line_porcelain_reader(
    mut reader: impl BufRead,
    expected_lines: usize,
    shallow_oids: &BTreeSet<String>,
    max_output_bytes: usize,
) -> Result<Vec<LineProvenance>> {
    let mut consumed = 0_usize;
    let mut record = Vec::new();
    if expected_lines == 0 {
        ensure!(
            !read_capped_line(&mut reader, &mut record, &mut consumed, max_output_bytes)?,
            "Git blame returned output for an empty document"
        );
        return Ok(Vec::new());
    }

    let mut mapped = vec![None; expected_lines];
    while read_capped_line(&mut reader, &mut record, &mut consumed, max_output_bytes)? {
        let header = ascii(&record, "blame header")?;
        let mut fields = header.split_ascii_whitespace();
        let oid = fields
            .next()
            .ok_or_else(|| anyhow!("malformed line-porcelain header `{header}`"))?;
        let original_line = fields
            .next()
            .ok_or_else(|| anyhow!("malformed line-porcelain header `{header}`"))?;
        let final_line = fields
            .next()
            .ok_or_else(|| anyhow!("malformed line-porcelain header `{header}`"))?;
        let group_size = fields.next();
        ensure!(
            fields.next().is_none(),
            "malformed line-porcelain header `{header}`"
        );
        let oid = parse_oid(oid, "blamed commit")?;
        let _original_line = parse_positive_line(original_line, "original line")?;
        let final_line = parse_positive_line(final_line, "final line")?;
        if let Some(group_size) = group_size {
            let _ = parse_positive_line(group_size, "group size")?;
        }

        let mut author_time = None;
        let mut committer_time = None;
        let mut found_content = false;
        while read_capped_line(&mut reader, &mut record, &mut consumed, max_output_bytes)? {
            if record.first() == Some(&b'\t') {
                found_content = true;
                break;
            }
            if let Some(value) = record.strip_prefix(b"author-time ") {
                author_time = Some(parse_timestamp(value, "author time")?);
            } else if let Some(value) = record.strip_prefix(b"committer-time ") {
                committer_time = Some(parse_timestamp(value, "committer time")?);
            }
        }
        ensure!(found_content, "line-porcelain record has no content line");

        let attribution = if oid.bytes().all(|byte| byte == b'0') {
            LineProvenance::WorkingTree
        } else if shallow_oids.contains(&oid) {
            LineProvenance::Shallow { oid }
        } else {
            LineProvenance::Git {
                oid,
                author_time: author_time
                    .ok_or_else(|| anyhow!("blame record has no author time"))?,
                committer_time: committer_time
                    .ok_or_else(|| anyhow!("blame record has no committer time"))?,
            }
        };
        let index = final_line - 1;
        let slot = mapped.get_mut(index).with_context(|| {
            format!("blamed final line {final_line} exceeds captured line count {expected_lines}")
        })?;
        ensure!(slot.is_none(), "duplicate blamed final line {final_line}");
        *slot = Some(attribution);
    }

    mapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            line.ok_or_else(|| anyhow!("Git blame omitted captured final line {}", index + 1))
        })
        .collect()
}

fn read_capped_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    consumed: &mut usize,
    max_output_bytes: usize,
) -> Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf().context("read Git blame stdout")?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let (take, terminated) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        let next_consumed = consumed
            .checked_add(take)
            .ok_or_else(|| anyhow!("Git blame stdout byte count overflowed"))?;
        ensure!(
            next_consumed <= max_output_bytes,
            "Git blame output exceeds the {max_output_bytes}-byte provenance limit"
        );
        let content_end = take - usize::from(terminated);
        line.extend_from_slice(&available[..content_end]);
        reader.consume(take);
        *consumed = next_consumed;
        if terminated {
            return Ok(true);
        }
    }
}

fn parse_shallow_set(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let mut oids = BTreeSet::new();
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if raw_line.is_empty() {
            continue;
        }
        let value = ascii(raw_line, "shallow commit")?;
        oids.insert(parse_oid(value, "shallow commit")?);
    }
    Ok(oids)
}

fn shallow_fingerprint(oids: &BTreeSet<String>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SHALLOW_SET_DOMAIN);
    for oid in oids {
        hash_field(&mut hasher, oid.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn parse_single_oid(output: &[u8], label: &str) -> Result<String> {
    let line = one_output_line(output, label)?;
    parse_oid(ascii(line, label)?, label)
}

fn one_output_line<'a>(output: &'a [u8], label: &str) -> Result<&'a [u8]> {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    let output = output.strip_suffix(b"\r").unwrap_or(output);
    ensure!(!output.is_empty(), "Git returned an empty {label}");
    ensure!(
        !output.contains(&b'\n') && !output.contains(&b'\r'),
        "Git returned multiple lines for {label}"
    );
    Ok(output)
}

fn one_output_path<'a>(output: &'a [u8], label: &str) -> Result<&'a [u8]> {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    let output = output.strip_suffix(b"\r").unwrap_or(output);
    ensure!(!output.is_empty(), "Git returned an empty {label}");
    Ok(output)
}

fn parse_oid(value: &str, label: &str) -> Result<String> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Git returned an invalid {label} `{value}`"
    );
    Ok(value.to_ascii_lowercase())
}

fn parse_positive_line(value: &str, label: &str) -> Result<usize> {
    let value = value
        .parse::<usize>()
        .with_context(|| format!("parse blame {label} `{value}`"))?;
    ensure!(value > 0, "blame {label} must be positive");
    Ok(value)
}

fn parse_timestamp(value: &[u8], label: &str) -> Result<i64> {
    let value = ascii(value, label)?;
    value
        .parse::<i64>()
        .with_context(|| format!("parse blame {label} `{value}`"))
}

fn ascii<'a>(value: &'a [u8], label: &str) -> Result<&'a str> {
    ensure!(value.is_ascii(), "Git returned non-ASCII {label}");
    std::str::from_utf8(value).context("ASCII Git output is not UTF-8")
}

pub(crate) fn validate_blame_input(bytes: &[u8]) -> Result<usize> {
    let logical_lines = git_logical_line_count(bytes);
    ensure!(
        logical_lines <= MAX_BLAME_LOGICAL_LINES,
        "captured document has {logical_lines} Git logical lines; provenance is limited to {MAX_BLAME_LOGICAL_LINES}"
    );
    Ok(logical_lines)
}

fn git_logical_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes.split(|byte| *byte == b'\n').count() - usize::from(bytes.last() == Some(&b'\n'))
}

fn validate_repository_relative_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "documentation path is empty");
    let path = Path::new(path);
    ensure!(!path.is_absolute(), "documentation path is absolute");
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "documentation path is not a normalized repository-relative path"
    );
    Ok(())
}

fn file_diagnostic(path: &str, operation: &str, error: anyhow::Error) -> ProvenanceDiagnostic {
    ProvenanceDiagnostic {
        path: Some(path.to_owned()),
        operation: operation.to_owned(),
        detail: format!("{error:#}"),
    }
}

#[cfg(unix)]
fn output_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn output_path(bytes: &[u8]) -> Result<PathBuf> {
    Ok(PathBuf::from(
        String::from_utf8(bytes.to_vec()).context("resolved shallow path is not UTF-8")?,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use anyhow::{Context, Result, bail};
    use tempfile::TempDir;

    use super::*;

    struct TestRepository {
        directory: TempDir,
    }

    impl TestRepository {
        fn new() -> Result<Option<Self>> {
            if Command::new("git").arg("--version").output().is_err() {
                return Ok(None);
            }
            let directory = tempfile::tempdir()?;
            let repository = Self { directory };
            repository.git(&["init", "--quiet"])?;
            repository.git(&["config", "user.name", "Documentation Test"])?;
            repository.git(&["config", "user.email", "docs@example.invalid"])?;
            Ok(Some(repository))
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn write(&self, path: &str, contents: &[u8]) -> Result<()> {
            let absolute = self.path().join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(absolute, contents)?;
            Ok(())
        }

        fn commit(&self, message: &str, author_date: &str, committer_date: &str) -> Result<()> {
            self.git(&["add", "--all"])?;
            let output = Command::new("git")
                .args(["commit", "--quiet", "-m", message])
                .current_dir(self.path())
                .env("GIT_AUTHOR_DATE", author_date)
                .env("GIT_COMMITTER_DATE", committer_date)
                .output()?;
            if !output.status.success() {
                bail!(
                    "git commit failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        }

        fn git(&self, args: &[&str]) -> Result<String> {
            let output = Command::new("git")
                .args(args)
                .current_dir(self.path())
                .output()?;
            if !output.status.success() {
                bail!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            String::from_utf8(output.stdout).context("Git test output is not UTF-8")
        }

        fn head(&self) -> Result<String> {
            Ok(self.git(&["rev-parse", "HEAD"])?.trim().to_owned())
        }

        fn shallow_path(&self) -> PathBuf {
            self.path().join(".git/shallow")
        }
    }

    fn captured_git(repository: &TestRepository) -> Result<GitRepository> {
        match RepositoryCapture::capture(repository.path()) {
            RepositoryCapture::Git(git) => Ok(git),
            RepositoryCapture::Unknown(diagnostic) => {
                bail!("expected Git provenance: {}", diagnostic.detail)
            }
        }
    }

    fn tracked_request(git: &GitRepository, path: &str, bytes: &[u8]) -> Result<BlameRequest> {
        match git.prepare_document(path, bytes) {
            DocumentPreparation::Tracked(request) => Ok(request),
            DocumentPreparation::Unknown => bail!("expected tracked document"),
            DocumentPreparation::Failed(diagnostic) => bail!(diagnostic.detail),
        }
    }

    fn attributed(
        git: &GitRepository,
        request: &BlameRequest,
        bytes: &[u8],
    ) -> Result<BlameMapping> {
        match git.blame_document(request, bytes) {
            DocumentBlame::Attributed(mapping) => Ok(mapping),
            DocumentBlame::Unknown(diagnostic) => bail!(diagnostic.detail),
        }
    }

    #[test]
    fn blame_argument_order_is_the_normative_contract() {
        let arguments = blame_arguments("0123456789012345678901234567890123456789", "guide.md")
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "--no-replace-objects",
                "blame",
                "--line-porcelain",
                "--no-ignore-revs-file",
                "--contents",
                "-",
                "0123456789012345678901234567890123456789",
                "--",
                "guide.md",
            ]
        );
    }

    #[test]
    fn parser_ignores_boundary_and_classifies_zero_oid() -> Result<()> {
        let oid = "1111111111111111111111111111111111111111";
        let zero = "0000000000000000000000000000000000000000";
        let output = format!(
            "{oid} 1 1 1\nauthor A\nauthor-time 100\ncommitter C\ncommitter-time 200\nboundary\nfilename guide.md\n\told\n{zero} 2 2 1\nauthor External\nauthor-time 999\ncommitter External\ncommitter-time 999\nfilename guide.md\n\tnew\n"
        );
        assert_eq!(
            parse_line_porcelain(output.as_bytes(), 2, &BTreeSet::new())?,
            [
                LineProvenance::Git {
                    oid: oid.to_owned(),
                    author_time: 100,
                    committer_time: 200,
                },
                LineProvenance::WorkingTree,
            ]
        );
        Ok(())
    }

    #[test]
    fn streaming_parser_enforces_its_stdout_byte_limit() {
        let oid = "1111111111111111111111111111111111111111";
        let output = format!(
            "{oid} 1 1 1\nauthor A\nauthor-time 100\ncommitter C\ncommitter-time 200\nfilename guide.md\n\tbody\n"
        );
        assert_eq!(
            parse_line_porcelain_reader(
                std::io::Cursor::new(output.as_bytes()),
                1,
                &BTreeSet::new(),
                output.len(),
            )
            .expect("the exact output cap must be accepted")
            .len(),
            1
        );
        let error = parse_line_porcelain_reader(
            std::io::Cursor::new(output.as_bytes()),
            1,
            &BTreeSet::new(),
            output.len() - 1,
        )
        .expect_err("one byte beyond the configured cap must fail");
        assert!(error.to_string().contains("provenance limit"));
    }

    #[test]
    fn blame_logical_line_limit_is_exact() {
        let accepted = vec![b'\n'; MAX_BLAME_LOGICAL_LINES];
        assert_eq!(
            validate_blame_input(&accepted).expect("exact limit must be accepted"),
            MAX_BLAME_LOGICAL_LINES
        );

        let rejected = vec![b'\n'; MAX_BLAME_LOGICAL_LINES + 1];
        let error = validate_blame_input(&rejected).expect_err("line bomb must be rejected");
        assert!(error.to_string().contains("65537 Git logical lines"));
        assert!(error.to_string().contains("limited to 65536"));
    }

    #[test]
    fn stderr_capture_drains_but_retains_only_its_bound() -> Result<()> {
        let stderr = vec![b'x'; MAX_GIT_STDERR_BYTES + 17];
        let captured = read_capped_stderr(std::io::Cursor::new(stderr))?;
        assert_eq!(captured.bytes.len(), MAX_GIT_STDERR_BYTES);
        assert!(captured.truncated);
        Ok(())
    }

    #[test]
    fn resolved_git_path_preserves_embedded_line_bytes() -> Result<()> {
        assert_eq!(
            one_output_path(
                b"/tmp/repository\nwith\rcontrols/.git/shallow\n",
                "resolved shallow path"
            )?,
            b"/tmp/repository\nwith\rcontrols/.git/shallow"
        );
        Ok(())
    }

    #[test]
    fn only_literal_shallow_oids_lose_git_age() -> Result<()> {
        let shallow_oid = "1111111111111111111111111111111111111111";
        let root_oid = "2222222222222222222222222222222222222222";
        let output = format!(
            "{shallow_oid} 1 1 1\nauthor A\nauthor-time 100\ncommitter C\ncommitter-time 200\nfilename guide.md\n\tshallow\n{root_oid} 2 2 1\nauthor A\nauthor-time 300\ncommitter C\ncommitter-time 400\nboundary\nfilename guide.md\n\troot\n"
        );
        let shallow = BTreeSet::from([shallow_oid.to_owned()]);
        assert_eq!(
            parse_line_porcelain(output.as_bytes(), 2, &shallow)?,
            [
                LineProvenance::Shallow {
                    oid: shallow_oid.to_owned(),
                },
                LineProvenance::Git {
                    oid: root_oid.to_owned(),
                    author_time: 300,
                    committer_time: 400,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn chunk_aggregation_obeys_worktree_git_unknown_precedence() -> Result<()> {
        let mapping = BlameMapping {
            cache_key: test_cache_key(),
            lines: vec![
                LineProvenance::Shallow {
                    oid: "1111111111111111111111111111111111111111".to_owned(),
                },
                LineProvenance::Git {
                    oid: "2222222222222222222222222222222222222222".to_owned(),
                    author_time: 100,
                    committer_time: 200,
                },
                LineProvenance::Git {
                    oid: "3333333333333333333333333333333333333333".to_owned(),
                    author_time: 300,
                    committer_time: 250,
                },
                LineProvenance::WorkingTree,
            ],
        };
        assert_eq!(
            mapping.aggregate_chunk(0, [1])?,
            ChunkGitProvenance {
                chunk_ordinal: 0,
                basis: GitChunkBasis::Unknown,
                author_time: None,
                committer_time: None,
            }
        );
        assert_eq!(
            mapping.aggregate_chunk_ranges(1, [(1, 3)])?,
            ChunkGitProvenance {
                chunk_ordinal: 1,
                basis: GitChunkBasis::Git,
                author_time: Some(300),
                committer_time: Some(250),
            }
        );
        assert_eq!(
            mapping.aggregate_chunk(2, [2, 4])?,
            ChunkGitProvenance {
                chunk_ordinal: 2,
                basis: GitChunkBasis::WorkingTree,
                author_time: Some(100),
                committer_time: Some(200),
            }
        );
        Ok(())
    }

    #[test]
    fn real_blame_uses_captured_bytes_and_preserves_root_author_time() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"old line\nstable line\n")?;
        repository.commit(
            "initial documentation",
            "2001-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;

        let git = captured_git(&repository)?;
        let bytes = b"changed line\nstable line\n";
        let request = tracked_request(&git, "guide.md", bytes)?;
        let mapping = attributed(&git, &request, bytes)?;
        assert!(matches!(mapping.lines[0], LineProvenance::WorkingTree));
        assert!(matches!(
            mapping.lines[1],
            LineProvenance::Git {
                author_time: 978_307_200,
                committer_time: 1_009_843_200,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn real_streaming_blame_maps_a_multi_record_document() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let mut bytes = Vec::new();
        for line in 0..2_048 {
            writeln!(&mut bytes, "stable line {line}")?;
        }
        repository.write("guide.md", &bytes)?;
        repository.commit(
            "multi-line documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;

        let git = captured_git(&repository)?;
        let request = tracked_request(&git, "guide.md", &bytes)?;
        let mapping = attributed(&git, &request, &bytes)?;
        assert_eq!(mapping.lines.len(), 2_048);
        assert!(
            mapping
                .lines
                .iter()
                .all(|line| matches!(line, LineProvenance::Git { .. }))
        );
        Ok(())
    }

    #[test]
    fn staged_and_unstaged_changes_in_tracked_file_are_working_tree() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"first old\nsecond old\nstable\n")?;
        repository.commit(
            "initial documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        repository.write("guide.md", b"first staged\nsecond old\nstable\n")?;
        repository.git(&["add", "guide.md"])?;
        let bytes = b"first staged\nsecond unstaged\nstable\n";
        repository.write("guide.md", bytes)?;

        let git = captured_git(&repository)?;
        let request = tracked_request(&git, "guide.md", bytes)?;
        let mapping = attributed(&git, &request, bytes)?;
        assert!(matches!(mapping.lines[0], LineProvenance::WorkingTree));
        assert!(matches!(mapping.lines[1], LineProvenance::WorkingTree));
        assert!(matches!(mapping.lines[2], LineProvenance::Git { .. }));
        Ok(())
    }

    #[test]
    fn ignore_revs_configuration_and_replace_refs_do_not_change_attribution() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"old instruction\n")?;
        repository.commit(
            "old instruction",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        let old_oid = repository.head()?;
        let bytes = b"current instruction\n";
        repository.write("guide.md", bytes)?;
        repository.commit(
            "current instruction",
            "2002-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;
        let current_oid = repository.head()?;

        repository.write(
            ".git-blame-ignore-revs",
            format!("{current_oid}\n").as_bytes(),
        )?;
        repository.git(&["config", "blame.ignoreRevsFile", ".git-blame-ignore-revs"])?;
        repository.git(&["replace", &current_oid, &old_oid])?;

        let git = captured_git(&repository)?;
        assert_eq!(git.head(), current_oid);
        let request = tracked_request(&git, "guide.md", bytes)?;
        let mapping = attributed(&git, &request, bytes)?;
        assert!(matches!(
            &mapping.lines[0],
            LineProvenance::Git { oid, author_time: 1_009_843_200, .. }
                if oid == &current_oid
        ));
        Ok(())
    }

    #[test]
    fn staged_new_and_untracked_paths_are_unknown_without_blame() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("tracked.md", b"tracked\n")?;
        repository.commit(
            "initial",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        repository.write("untracked.md", b"untracked\n")?;
        repository.write("staged.md", b"staged\n")?;
        repository.git(&["add", "staged.md"])?;

        let git = captured_git(&repository)?;
        assert!(matches!(
            git.prepare_document("untracked.md", b"untracked\n"),
            DocumentPreparation::Unknown
        ));
        assert!(matches!(
            git.prepare_document("staged.md", b"staged\n"),
            DocumentPreparation::Unknown
        ));
        Ok(())
    }

    #[test]
    fn path_removed_from_the_index_is_untracked_despite_the_head_blob() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let bytes = b"tracked documentation\n";
        repository.write("README.md", bytes)?;
        repository.commit(
            "documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        repository.git(&["rm", "--cached", "--quiet", "README.md"])?;

        let git = captured_git(&repository)?;
        assert!(matches!(
            git.prepare_document("README.md", bytes),
            DocumentPreparation::Unknown
        ));
        Ok(())
    }

    #[test]
    fn deleted_then_recreated_untracked_path_is_unknown_without_blame() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"tracked\n")?;
        repository.commit(
            "add guide",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        fs::remove_file(repository.path().join("guide.md"))?;
        repository.commit(
            "delete guide",
            "2002-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;
        repository.write("guide.md", b"untracked replacement\n")?;

        let git = captured_git(&repository)?;
        assert!(matches!(
            git.prepare_document("guide.md", b"untracked replacement\n"),
            DocumentPreparation::Unknown
        ));
        Ok(())
    }

    #[test]
    fn head_directory_replaced_by_untracked_file_is_unknown_without_blame() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md/child.txt", b"tracked child\n")?;
        repository.commit(
            "add directory",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        fs::remove_dir_all(repository.path().join("guide.md"))?;
        repository.write("guide.md", b"untracked replacement\n")?;

        let git = captured_git(&repository)?;
        assert!(matches!(
            git.prepare_document("guide.md", b"untracked replacement\n"),
            DocumentPreparation::Unknown
        ));
        Ok(())
    }

    #[test]
    fn head_membership_is_relative_to_a_nested_indexed_root() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"root\n")?;
        repository.write("docs/guide.md", b"nested\n")?;
        repository.commit(
            "add guides",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;

        let nested_root = repository.path().join("docs");
        let git = match RepositoryCapture::capture(&nested_root) {
            RepositoryCapture::Git(git) => git,
            RepositoryCapture::Unknown(diagnostic) => {
                bail!("expected Git provenance: {}", diagnostic.detail)
            }
        };
        assert!(matches!(
            git.prepare_document("guide.md", b"nested\n"),
            DocumentPreparation::Tracked(_)
        ));

        fs::remove_file(nested_root.join("guide.md"))?;
        repository.commit(
            "delete nested guide",
            "2002-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;
        repository.write("docs/guide.md", b"untracked nested replacement\n")?;
        let git = match RepositoryCapture::capture(&nested_root) {
            RepositoryCapture::Git(git) => git,
            RepositoryCapture::Unknown(diagnostic) => {
                bail!("expected Git provenance: {}", diagnostic.detail)
            }
        };
        assert!(matches!(
            git.prepare_document("guide.md", b"untracked nested replacement\n"),
            DocumentPreparation::Unknown
        ));
        Ok(())
    }

    #[test]
    fn cache_key_ignores_unrelated_head_changes_and_tracks_other_inputs() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let bytes = b"same\n";
        repository.write("guide.md", bytes)?;
        repository.write("copy.md", bytes)?;
        repository.commit(
            "docs",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;

        let first_git = captured_git(&repository)?;
        let first = tracked_request(&first_git, "guide.md", bytes)?.cache_key;
        let copy = tracked_request(&first_git, "copy.md", bytes)?.cache_key;
        assert_ne!(first.fingerprint(), copy.fingerprint());

        repository.write("unrelated.txt", b"unrelated\n")?;
        repository.commit(
            "unrelated",
            "2002-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;
        let second_git = captured_git(&repository)?;
        let after_unrelated = tracked_request(&second_git, "guide.md", bytes)?.cache_key;
        assert_eq!(first, after_unrelated);

        let edited = tracked_request(&second_git, "guide.md", b"edited\n")?.cache_key;
        assert_ne!(first.fingerprint(), edited.fingerprint());
        Ok(())
    }

    #[test]
    fn cache_key_and_publication_validation_track_git_conversion_state() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.git(&["config", "core.autocrlf", "true"])?;
        let bytes = b"first\r\nsecond\r\n";
        repository.write("guide.md", bytes)?;
        repository.commit(
            "documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        assert_eq!(fs::read(repository.path().join("guide.md"))?, bytes);

        let before_attributes = captured_git(&repository)?;
        let before = tracked_request(&before_attributes, "guide.md", bytes)?;
        repository.write(".gitattributes", b"guide.md -text\n")?;

        assert!(matches!(
            before_attributes
                .validate_before_publication([(&before.cache_key, bytes.as_slice())])?,
            PublicationValidation::ConversionDrift(_)
        ));

        repository.git(&["add", ".gitattributes"])?;
        repository.git(&["commit", "--quiet", "-m", "disable conversion"])?;
        let after_attributes = captured_git(&repository)?;
        let after = tracked_request(&after_attributes, "guide.md", bytes)?;
        assert_eq!(before.cache_key.bytes_hash, after.cache_key.bytes_hash);
        assert_eq!(before.cache_key.path_tip, after.cache_key.path_tip);
        assert_eq!(
            before.cache_key.shallow_fingerprint,
            after.cache_key.shallow_fingerprint
        );
        assert_ne!(
            before.cache_key.converted_blob_oid,
            after.cache_key.converted_blob_oid
        );
        assert_ne!(
            before.cache_key.fingerprint(),
            after.cache_key.fingerprint()
        );
        Ok(())
    }

    #[test]
    fn shallow_file_controls_mapping_and_cache_key() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let bytes = b"root line\n";
        repository.write("guide.md", bytes)?;
        repository.commit(
            "root",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        let root_oid = repository.head()?;

        let full = captured_git(&repository)?;
        let full_request = tracked_request(&full, "guide.md", bytes)?;
        let full_mapping = attributed(&full, &full_request, bytes)?;
        assert!(matches!(full_mapping.lines[0], LineProvenance::Git { .. }));

        fs::write(repository.shallow_path(), format!("{root_oid}\n"))?;
        let shallow = captured_git(&repository)?;
        let shallow_request = tracked_request(&shallow, "guide.md", bytes)?;
        let shallow_mapping = attributed(&shallow, &shallow_request, bytes)?;
        assert!(matches!(
            shallow_mapping.lines[0],
            LineProvenance::Shallow { .. }
        ));
        assert_ne!(
            full_request.cache_key.fingerprint(),
            shallow_request.cache_key.fingerprint()
        );
        Ok(())
    }

    #[test]
    fn prepublication_validation_detects_head_index_and_shallow_drift() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("guide.md", b"guide\n")?;
        repository.commit(
            "root",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        let root_oid = repository.head()?;

        let before_head_change = captured_git(&repository)?;
        repository.write("other.txt", b"other\n")?;
        repository.commit(
            "other",
            "2002-01-01T00:00:00 +0000",
            "2002-01-01T00:00:00 +0000",
        )?;
        assert!(matches!(
            before_head_change.validate_before_publication(std::iter::empty())?,
            PublicationValidation::Drift(_)
        ));

        let before_index_change = captured_git(&repository)?;
        repository.git(&["rm", "--cached", "--quiet", "guide.md"])?;
        assert!(matches!(
            before_index_change.validate_before_publication(std::iter::empty())?,
            PublicationValidation::Drift(_)
        ));
        repository.git(&["add", "guide.md"])?;

        let before_shallow_change = captured_git(&repository)?;
        fs::write(repository.shallow_path(), format!("{root_oid}\n"))?;
        assert!(matches!(
            before_shallow_change.validate_before_publication(std::iter::empty())?,
            PublicationValidation::Drift(_)
        ));
        Ok(())
    }

    #[test]
    fn non_git_root_degrades_to_visible_unknown() -> Result<()> {
        let root = tempfile::tempdir()?;
        match RepositoryCapture::capture(root.path()) {
            RepositoryCapture::Git(_) => bail!("plain directory unexpectedly detected as Git"),
            RepositoryCapture::Unknown(diagnostic) => {
                assert_eq!(diagnostic.operation, "capture Git documentation provenance");
                assert!(diagnostic.detail.contains("worktree"));
            }
        }
        Ok(())
    }

    #[test]
    fn bare_repository_root_degrades_to_visible_unknown() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.write("README.md", b"tracked documentation\n")?;
        repository.commit(
            "documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;

        let clone_parent = tempfile::tempdir()?;
        let bare = clone_parent.path().join("bare.git");
        let output = Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(repository.path())
            .arg(&bare)
            .output()?;
        if !output.status.success() {
            bail!(
                "git clone --bare failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(bare.join("README.md"), b"unrelated filesystem file\n")?;

        match RepositoryCapture::capture(&bare) {
            RepositoryCapture::Git(_) => bail!("bare repository unexpectedly treated as worktree"),
            RepositoryCapture::Unknown(diagnostic) => {
                assert_eq!(diagnostic.operation, "capture Git documentation provenance");
                assert!(diagnostic.detail.contains("not inside a Git worktree"));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn linked_worktree_under_a_control_character_path_keeps_git_provenance() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let bytes = b"tracked documentation\n";
        repository.write("README.md", bytes)?;
        repository.commit(
            "documentation",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;

        let worktrees = tempfile::tempdir()?;
        let common = worktrees.path().join("common\nwith\rcontrols");
        let linked = worktrees.path().join("linked");
        let clone = Command::new("git")
            .args(["clone", "--quiet"])
            .arg(repository.path())
            .arg(&common)
            .output()?;
        if !clone.status.success() {
            bail!(
                "git clone failed: {}",
                String::from_utf8_lossy(&clone.stderr)
            );
        }
        let add = Command::new("git")
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(&linked)
            .arg("HEAD")
            .current_dir(&common)
            .output()?;
        if !add.status.success() {
            bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&add.stderr)
            );
        }

        let git = match RepositoryCapture::capture(&linked) {
            RepositoryCapture::Git(git) => git,
            RepositoryCapture::Unknown(diagnostic) => {
                bail!("expected Git provenance: {}", diagnostic.detail)
            }
        };
        let request = tracked_request(&git, "README.md", bytes)?;
        let mapping = attributed(&git, &request, bytes)?;
        assert!(matches!(mapping.lines[0], LineProvenance::Git { .. }));
        Ok(())
    }

    fn test_cache_key() -> BlameCacheKey {
        BlameCacheKey {
            path_scope: "scope".to_owned(),
            path: "guide.md".to_owned(),
            bytes_hash: "bytes".to_owned(),
            converted_blob_oid: "converted".to_owned(),
            path_tip: "tip".to_owned(),
            shallow_fingerprint: "shallow".to_owned(),
        }
    }
}
