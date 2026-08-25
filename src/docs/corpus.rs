use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{IncrementalIgnore, WalkBuilder};
use oxc_allocator::Allocator;
use oxc_parser::Parser as OxcParser;
use oxc_span::SourceType;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as YamlValue;

const DEFAULT_MAX_FILE_BYTES: u64 = 4_194_304;
const TARGET_BYTES: usize = 2_400;
const MERGE_MAX_BYTES: usize = 4_000;
const HARD_MAX_BYTES: usize = 24_000;
const HEADING_CONTEXT_MAX_BYTES: usize = 1_024;
const SYNTHETIC_CONTEXT_MAX_BYTES: usize = 1_024;
const EMBEDDING_DOMAIN: &[u8] = b"jscout-doc-embedding-v1\0";
#[cfg(test)]
const CORPUS_DOMAIN: &[u8] = b"jscout-doc-corpus-v1\0";
const HIDDEN_ROOT_ALLOWLIST: &[&str] = &[".github", ".claude", ".agents"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: u64,
}

impl Default for CorpusOptions {
    fn default() -> Self {
        Self {
            include: super::default_include_globs(),
            exclude: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

/// Path-only form of the documentation admission policy used by the watcher.
/// It deliberately shares the corpus glob, ignore, and hidden-path semantics,
/// but does not read or parse the file. A matching event therefore schedules
/// the complete inventory pass that remains the authority for membership.
pub struct DocumentationPathPolicy {
    root: PathBuf,
    include: GlobSet,
    exclude: GlobSet,
    ignore: IncrementalIgnore,
    active: bool,
}

impl DocumentationPathPolicy {
    pub fn new(root: &Path, options: &CorpusOptions) -> Result<Self> {
        let include = build_glob_set(&options.include, "include")?;
        let exclude = build_glob_set(&options.exclude, "exclude")?;
        let ignore = documentation_ignore(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            include,
            exclude,
            ignore,
            active: !options.include.is_empty(),
        })
    }

    /// Rebuild ignore state after a successful refresh publishes any edits to
    /// `.gitignore`, `.ignore`, or repository exclude files.
    pub fn reload_ignore(&mut self) -> Result<()> {
        self.ignore = documentation_ignore(&self.root)?;
        Ok(())
    }

    /// Whether this path could belong to the configured documentation corpus.
    /// Ignore loading errors conservatively return true so the authoritative
    /// inventory pass can classify and report them.
    pub fn is_admitted(&mut self, path: &Path, is_dir: bool) -> bool {
        if !self.active || is_dir {
            return false;
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if !is_document_path(relative) || hidden_path_is_excluded(relative) {
            return false;
        }
        let Some(normalized) = normalized_utf8_path(relative) else {
            return false;
        };
        if !self.include.is_match(&normalized) || self.exclude.is_match(&normalized) {
            return false;
        }
        let (matched, error) = self.ignore.matched_with_errors(relative, false);
        error.is_some() || !matched.is_ignore()
    }

    /// Whether a directory-shaped path could contain visible documentation.
    /// This is deliberately independent of include/exclude file globs: the
    /// authoritative inventory descends every visible directory and applies
    /// arbitrary globs to files. The watcher uses this only after the source
    /// plane ignored an existing directory or a missing path, so it closes the
    /// allowlisted-hidden-root gap without stealing source-file affinity.
    pub fn may_contain_document(&mut self, path: &Path) -> bool {
        if !self.active {
            return false;
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.as_os_str().is_empty() || hidden_path_is_excluded(relative) {
            return false;
        }
        let (matched, error) = self.ignore.matched_with_errors(relative, true);
        error.is_some() || !matched.is_ignore()
    }
}

fn documentation_ignore(root: &Path) -> Result<IncrementalIgnore> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    builder
        .build_matchers()
        .pop()
        .ok_or_else(|| anyhow!("documentation ignore matcher was not built"))
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Corpus {
    pub canonical_root: PathBuf,
    pub fingerprint: String,
    pub files: Vec<DocFile>,
    pub decisions: Vec<Decision>,
}

/// One captured Markdown document from the shared repository inventory.
/// Parsing and later shared-index insertion both consume this exact immutable
/// byte buffer, so source hashes and chunk slices cannot observe different
/// filesystem states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDocument {
    pub bytes: Vec<u8>,
    pub file: DocFile,
}

/// Results of the one repository traversal used by shared indexing. Code
/// paths remain a code-only input to workspace discovery; Markdown carries
/// its captured bytes and its visible membership decisions beside them.
#[derive(Debug)]
pub(crate) struct RepositoryCorpus {
    pub files: Vec<PathBuf>,
    pub rejections: Vec<crate::walk::WalkRejection>,
    pub documents: Vec<CapturedDocument>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// Slash-normalized relative path, or a non-authoritative escaped display
    /// path when `path_base64` is present.
    pub path: String,
    pub subject: String,
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_encoding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocFile {
    pub path: String,
    pub content_hash: String,
    pub byte_len: u64,
    pub line_count: u64,
    pub title: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub front_matter_state: String,
    pub headings: Vec<DocHeading>,
    pub blocks: Vec<DocBlock>,
    pub chunks: Vec<DocChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocHeading {
    pub level: u8,
    pub text: String,
    pub breadcrumb: String,
    pub source_start: u64,
    pub source_end: u64,
    pub line_start: u64,
    pub line_end: u64,
}

/// One inclusive, 1-based Git/LF-logical source-line range whose retained text
/// contributes to a documentation block or chunk. Exact source spans and the
/// display line fields retain the parser's broader line-ending semantics;
/// these ranges match `git blame` numbering and deliberately omit blank lines
/// and text removed from retrieval, such as Markdown and MDX comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocLineRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocBlock {
    pub ordinal: u64,
    pub kind: String,
    pub source_start: u64,
    pub source_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    /// Retained body lines inside this exact block span.
    pub contributing_lines: Vec<DocLineRange>,
    pub content_hash: String,
    /// Exact source text for this block, including original line endings.
    pub body: String,
    pub rendered_body: String,
    pub breadcrumb: String,
    pub nearest_heading: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChunk {
    /// Deterministic publication order across every chunk in the document.
    pub ordinal: u64,
    /// Deterministic order among chunks owned by the same heading occurrence.
    pub same_heading_ordinal: u64,
    pub source_start: u64,
    pub source_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    /// Retained body lines used by this retrieval chunk. An oversized table
    /// or fence fragment also names the earlier line that supplies its repeated
    /// synthetic context, even when that line is outside the fragment span.
    pub contributing_lines: Vec<DocLineRange>,
    pub breadcrumb: String,
    pub nearest_heading: Option<String>,
    pub rendered_body: String,
    pub embedding_text: Option<String>,
    pub embedding_identity: Option<String>,
    pub block_ordinals: Vec<u64>,
    pub is_stub: bool,
}

pub fn validate_patterns(include: &[String], exclude: &[String]) -> Result<()> {
    build_glob_set(include, "include")?;
    build_glob_set(exclude, "exclude")?;
    Ok(())
}

pub(crate) fn repository_inventory(
    root: &Path,
    options: &CorpusOptions,
) -> Result<RepositoryCorpus> {
    repository_inventory_with_consumer(root, options, &capture_file)
}

#[cfg(test)]
fn scan_repository_with_capture(
    root: &Path,
    options: &CorpusOptions,
    capture: &dyn Fn(&Path, u64) -> std::io::Result<CapturedFile>,
) -> Result<RepositoryCorpus> {
    repository_inventory_with_consumer(root, options, capture)
}

fn repository_inventory_with_consumer(
    root: &Path,
    options: &CorpusOptions,
    capture: &dyn Fn(&Path, u64) -> std::io::Result<CapturedFile>,
) -> Result<RepositoryCorpus> {
    let documentation = DocumentationCollector::new(options, capture)?;
    let inventory = crate::walk::repository_inventory(root, documentation)?;
    let DocumentationCollection {
        documents,
        decisions,
    } = inventory.consumer;
    Ok(RepositoryCorpus {
        files: inventory.files,
        rejections: inventory.rejections,
        documents,
        decisions,
    })
}

#[cfg(test)]
fn scan_repository(root: &Path, options: &CorpusOptions) -> Result<RepositoryCorpus> {
    repository_inventory(root, options)
}

/// Parser-focused compatibility helper. Production indexing uses the shared
/// repository inventory so Markdown is never published through an independent
/// lifecycle or snapshot.
#[cfg(test)]
pub fn scan(root: &Path, options: &CorpusOptions) -> Result<Corpus> {
    let repository = scan_repository(root, options)?;
    let files = repository
        .documents
        .into_iter()
        .map(|document| document.file)
        .collect::<Vec<_>>();
    Ok(Corpus {
        canonical_root: root.canonicalize()?,
        fingerprint: corpus_fingerprint(&files),
        files,
        decisions: repository.decisions,
    })
}

pub fn provider_text(nearest_heading: Option<&str>, rendered_body: &str) -> String {
    match nearest_heading {
        Some(heading) => {
            let mut text = String::with_capacity(heading.len() + 2 + rendered_body.len());
            text.push_str(heading);
            text.push_str("\n\n");
            text.push_str(rendered_body);
            text
        }
        None => rendered_body.to_owned(),
    }
}

pub fn embedding_identity(nearest_heading: Option<&str>, rendered_body: &str) -> String {
    let heading = nearest_heading.unwrap_or_default().as_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMBEDDING_DOMAIN);
    hasher.update(&[u8::from(nearest_heading.is_some())]);
    hasher.update(&(heading.len() as u64).to_be_bytes());
    hasher.update(heading);
    hasher.update(&(rendered_body.len() as u64).to_be_bytes());
    hasher.update(rendered_body.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Documentation-plane consumer for the shared repository traversal. It owns
/// Markdown/MDX membership, capture, and parsing; `walk::inventory` owns
/// directory traversal and code-file selection.
pub(crate) struct DocumentationCollector<'a> {
    options: &'a CorpusOptions,
    include: GlobSet,
    exclude: GlobSet,
    candidates: Vec<InventoryCandidate>,
    documents: Vec<CapturedDocument>,
    decisions: Vec<Decision>,
    capture: &'a dyn Fn(&Path, u64) -> std::io::Result<CapturedFile>,
}

pub(crate) struct DocumentationCollection {
    pub documents: Vec<CapturedDocument>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug)]
struct InventoryCandidate {
    relative: PathBuf,
    normalized: String,
}

impl<'a> DocumentationCollector<'a> {
    pub(crate) fn new(
        options: &'a CorpusOptions,
        capture: &'a dyn Fn(&Path, u64) -> std::io::Result<CapturedFile>,
    ) -> Result<Self> {
        validate_patterns(&options.include, &options.exclude)?;
        Ok(Self {
            options,
            include: build_glob_set(&options.include, "include")?,
            exclude: build_glob_set(&options.exclude, "exclude")?,
            candidates: Vec::new(),
            documents: Vec::new(),
            decisions: Vec::new(),
            capture,
        })
    }

    fn is_active(&self) -> bool {
        !self.options.include.is_empty()
    }

    fn path_relevant(&self, relative: &Path) -> bool {
        is_document_path(relative)
            || normalized_utf8_path(relative).is_some_and(|path| self.include.is_match(path))
    }

    fn hidden_path_is_excluded(&self, relative: &Path) -> bool {
        hidden_path_is_excluded(relative)
    }

    fn record_decision(&mut self, path: &Path, subject: &str, rule: &str, detail: Option<String>) {
        self.decisions.push(decision(path, subject, rule, detail));
    }

    fn inspect_special_file(
        &self,
        relative: &Path,
        absolute: &Path,
        file_type: fs::FileType,
    ) -> Result<()> {
        if is_document_path(relative) {
            ensure_regular_inventory_file(absolute, file_type)?;
        }
        Ok(())
    }

    fn inspect_regular_file(&mut self, relative: PathBuf) {
        let Some(path) = normalized_utf8_path(&relative) else {
            if is_document_path(&relative) {
                self.record_decision(&relative, "file", "non-utf8-path", None);
            }
            return;
        };
        if !is_document_path(&relative) {
            if self.include.is_match(&path) {
                self.record_decision(&relative, "file", "unsupported-extension", None);
            }
            return;
        }
        if self.exclude.is_match(&path) {
            self.record_decision(&relative, "file", "excluded", None);
            return;
        }
        if !self.include.is_match(&path) {
            self.record_decision(&relative, "file", "not-included", None);
            return;
        }
        self.candidates.push(InventoryCandidate {
            relative,
            normalized: path,
        });
    }

    fn finish(mut self, root: &Path) -> Result<DocumentationCollection> {
        self.acquire_candidates(root)?;
        self.documents
            .sort_by(|left, right| left.file.path.cmp(&right.file.path));
        self.decisions.sort_by(|left, right| {
            left.path
                .as_bytes()
                .cmp(right.path.as_bytes())
                .then_with(|| left.subject.cmp(&right.subject))
                .then_with(|| left.rule.cmp(&right.rule))
        });
        Ok(DocumentationCollection {
            documents: self.documents,
            decisions: self.decisions,
        })
    }

    fn acquire_candidates(&mut self, root: &Path) -> Result<()> {
        self.candidates
            .sort_by(|left, right| left.normalized.as_bytes().cmp(right.normalized.as_bytes()));
        for candidate in std::mem::take(&mut self.candidates) {
            let absolute = root.join(&candidate.relative);
            match (self.capture)(&absolute, self.options.max_file_bytes) {
                Ok(CapturedFile::Oversized) => {
                    self.decisions
                        .push(decision(&candidate.relative, "file", "oversized", None));
                }
                Ok(CapturedFile::NotRegular) => {
                    bail!(
                        "documentation inventory changed file type while capturing {}",
                        candidate.normalized
                    );
                }
                Ok(CapturedFile::Bytes(bytes)) => match std::str::from_utf8(&bytes) {
                    Err(_) => {
                        self.decisions.push(decision(
                            &candidate.relative,
                            "file",
                            "non-utf8",
                            None,
                        ));
                    }
                    Ok(_) => {
                        let file =
                            parse_document(&candidate.normalized, &bytes).with_context(|| {
                                format!("parse captured Markdown document {}", candidate.normalized)
                            })?;
                        self.documents.push(CapturedDocument { bytes, file });
                        self.decisions
                            .push(decision(&candidate.relative, "file", "indexed", None));
                    }
                },
                Err(error) if is_no_follow_violation(&error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "documentation inventory changed file type while capturing {}",
                            candidate.normalized
                        )
                    });
                }
                Err(error) if crate::io_policy::is_retryable(&error) => {
                    return Err(error).with_context(|| {
                        format!("capture documentation file {}", candidate.normalized)
                    });
                }
                Err(error) if crate::io_policy::is_inventory_race(&error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "documentation inventory changed while capturing {}",
                            candidate.normalized
                        )
                    });
                }
                Err(error) => self.decisions.push(decision(
                    &candidate.relative,
                    "file",
                    "read-error",
                    Some(error.to_string()),
                )),
            }
        }
        Ok(())
    }
}

impl crate::walk::RepositoryInventoryConsumer for DocumentationCollector<'_> {
    type Output = DocumentationCollection;

    fn is_active(&self) -> bool {
        DocumentationCollector::is_active(self)
    }

    fn path_relevant(&self, relative: &Path) -> bool {
        DocumentationCollector::path_relevant(self, relative)
    }

    fn hidden_path_is_excluded(&self, relative: &Path) -> bool {
        DocumentationCollector::hidden_path_is_excluded(self, relative)
    }

    fn record_decision(&mut self, path: &Path, subject: &str, rule: &str, detail: Option<String>) {
        DocumentationCollector::record_decision(self, path, subject, rule, detail);
    }

    fn inspect_special_file(
        &self,
        relative: &Path,
        absolute: &Path,
        file_type: fs::FileType,
    ) -> Result<()> {
        DocumentationCollector::inspect_special_file(self, relative, absolute, file_type)
    }

    fn inspect_regular_file(&mut self, relative: PathBuf) {
        DocumentationCollector::inspect_regular_file(self, relative);
    }

    fn finish(self, root: &Path) -> Result<Self::Output> {
        DocumentationCollector::finish(self, root)
    }
}

fn ensure_regular_inventory_file(path: &Path, file_type: fs::FileType) -> Result<()> {
    if !file_type.is_file() {
        bail!(
            "unsupported file type during documentation inventory: {}",
            path.display()
        );
    }
    Ok(())
}

pub(crate) enum CapturedFile {
    Bytes(Vec<u8>),
    Oversized,
    NotRegular,
}

pub(crate) fn capture_file(path: &Path, max_bytes: u64) -> std::io::Result<CapturedFile> {
    let file = open_no_follow(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Ok(CapturedFile::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        Ok(CapturedFile::Oversized)
    } else {
        Ok(CapturedFile::Bytes(bytes))
    }
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to follow a symlink",
        ));
    }
    OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn is_no_follow_violation(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_no_follow_violation(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::InvalidInput
}

fn build_glob_set(patterns: &[String], label: &str) -> Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        validate_pattern_shape(pattern, label)?;
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .case_insensitive(false)
            .backslash_escape(true)
            .build()
            .with_context(|| format!("invalid documentation {label} glob {pattern:?}"))?;
        set.add(glob);
    }
    set.build()
        .with_context(|| format!("compile documentation {label} globs"))
}

fn validate_pattern_shape(pattern: &str, label: &str) -> Result<()> {
    if pattern.starts_with('!') {
        bail!("documentation {label} glob must not start with '!': {pattern:?}");
    }
    if pattern.ends_with('/') {
        bail!("documentation {label} glob must match files, not end in '/': {pattern:?}");
    }
    if contains_unescaped(pattern, b'{') || contains_unescaped(pattern, b'}') {
        bail!("documentation {label} glob must not use brace alternation: {pattern:?}");
    }
    Ok(())
}

fn contains_unescaped(value: &str, needle: u8) -> bool {
    let mut escaped = false;
    for byte in value.bytes() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == needle {
            return true;
        }
    }
    false
}

fn hidden_path_is_excluded(path: &Path) -> bool {
    for (index, component) in path.components().enumerate() {
        let name = component.as_os_str();
        if os_str_starts_with_dot(name) {
            if index == 0
                && name
                    .to_str()
                    .is_some_and(|name| HIDDEN_ROOT_ALLOWLIST.contains(&name))
            {
                continue;
            }
            return true;
        }
    }
    false
}

fn is_document_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension == std::ffi::OsStr::new("md") || extension == std::ffi::OsStr::new("mdx")
    })
}

fn is_mdx_path(path: &Path) -> bool {
    path.extension() == Some(std::ffi::OsStr::new("mdx"))
}

#[cfg(unix)]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().first() == Some(&b'.')
}

#[cfg(windows)]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().next() == Some(u16::from(b'.'))
}

#[cfg(not(any(unix, windows)))]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    value.to_string_lossy().as_bytes().first() == Some(&b'.')
}

fn normalized_utf8_path(path: &Path) -> Option<String> {
    let mut normalized = String::new();
    for component in path.components() {
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component.as_os_str().to_str()?);
    }
    Some(normalized)
}

fn decision(path: &Path, subject: &str, rule: &str, detail: Option<String>) -> Decision {
    if let Some(path) = normalized_utf8_path(path) {
        return Decision {
            path,
            subject: subject.to_owned(),
            rule: rule.to_owned(),
            detail,
            path_base64: None,
            path_encoding: None,
        };
    }
    let (encoded, encoding) = encode_native_path(path);
    Decision {
        path: path.to_string_lossy().replace('\\', "/"),
        subject: subject.to_owned(),
        rule: rule.to_owned(),
        detail,
        path_base64: Some(encoded),
        path_encoding: Some(encoding.to_owned()),
    }
}

#[cfg(unix)]
fn encode_native_path(path: &Path) -> (String, &'static str) {
    use std::os::unix::ffi::OsStrExt as _;
    (
        base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_bytes()),
        "unix-bytes",
    )
}

#[cfg(windows)]
fn encode_native_path(path: &Path) -> (String, &'static str) {
    use std::os::windows::ffi::OsStrExt as _;
    let bytes = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    (
        base64::engine::general_purpose::STANDARD.encode(bytes),
        "windows-wtf16le",
    )
}

#[cfg(not(any(unix, windows)))]
fn encode_native_path(path: &Path) -> (String, &'static str) {
    (
        base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes()),
        "platform-bytes",
    )
}

#[cfg(test)]
fn corpus_fingerprint(files: &[DocFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CORPUS_DOMAIN);
    hasher.update(&(files.len() as u64).to_be_bytes());
    for file in files {
        hasher.update(&(file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        let hash = decode_hash(&file.content_hash);
        hasher.update(&hash);
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
fn decode_hash(hash: &str) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for (index, pair) in hash.as_bytes().chunks_exact(2).enumerate().take(32) {
        bytes[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    bytes
}

#[cfg(test)]
fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("BLAKE3 emits lowercase hexadecimal"),
    }
}

#[derive(Debug)]
struct FrontMatter {
    state: &'static str,
    body_start: usize,
    title: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
}

fn parse_front_matter(markdown: &str) -> FrontMatter {
    let lines = logical_lines(markdown.as_bytes());
    if lines.first().map(|line| line.content(markdown.as_bytes())) != Some(b"---") {
        return FrontMatter {
            state: "absent",
            body_start: 0,
            title: None,
            description: None,
            tags: Vec::new(),
        };
    }
    let Some(closing) = lines
        .iter()
        .skip(1)
        .find(|line| line.content(markdown.as_bytes()) == b"---")
    else {
        return malformed_front_matter();
    };
    let yaml_start = lines[0].full_end;
    let yaml_end = closing.start;
    let Ok(YamlValue::Mapping(mapping)) = serde_yaml_ng::from_str(&markdown[yaml_start..yaml_end])
    else {
        return malformed_front_matter();
    };

    let title = yaml_string(&mapping, "title");
    let description = yaml_string(&mapping, "description");
    let tags = yaml_tags(&mapping);
    FrontMatter {
        state: "valid",
        body_start: closing.full_end,
        title,
        description,
        tags,
    }
}

fn malformed_front_matter() -> FrontMatter {
    FrontMatter {
        state: "malformed_as_body",
        body_start: 0,
        title: None,
        description: None,
        tags: Vec::new(),
    }
}

fn yaml_string(mapping: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    mapping
        .get(YamlValue::String(key.to_owned()))
        .and_then(|value| match value {
            YamlValue::String(value) => Some(value.clone()),
            _ => None,
        })
}

fn yaml_tags(mapping: &serde_yaml_ng::Mapping) -> Vec<String> {
    let Some(value) = mapping.get(YamlValue::String("tags".to_owned())) else {
        return Vec::new();
    };
    match value {
        YamlValue::String(value) => vec![value.clone()],
        YamlValue::Sequence(values)
            if values
                .iter()
                .all(|value| matches!(value, YamlValue::String(_))) =>
        {
            values
                .iter()
                .filter_map(YamlValue::as_str)
                .map(str::to_owned)
                .collect()
        }
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
struct LogicalLine {
    start: usize,
    content_end: usize,
    full_end: usize,
}

impl LogicalLine {
    fn content<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.start..self.content_end]
    }
}

fn logical_lines(bytes: &[u8]) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let newline = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| start + offset);
        let (line_end, full_end) = newline.map_or((bytes.len(), bytes.len()), |end| (end, end + 1));
        let content_end = if line_end > start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        lines.push(LogicalLine {
            start,
            content_end,
            full_end,
        });
        if full_end == bytes.len() {
            break;
        }
        start = full_end;
    }
    lines
}

#[derive(Debug, Clone)]
enum BlockKind {
    Paragraph,
    List,
    Table { header: Vec<String> },
    Quote,
    FencedCode { info: String },
    IndentedCode,
    Html,
}

impl BlockKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::List => "list",
            Self::Table { .. } => "table",
            Self::Quote => "block-quote",
            Self::FencedCode { .. } => "fenced-code",
            Self::IndentedCode => "indented-code",
            Self::Html => "html",
        }
    }

    fn synthetic_context(&self) -> Option<String> {
        let context = match self {
            Self::FencedCode { info } if info.is_empty() => "[fence]\n".to_owned(),
            Self::FencedCode { info } => format!("[fence {info}]\n"),
            Self::Table { header } if header.is_empty() => "[table]\n".to_owned(),
            Self::Table { header } => format!("[table {}]\n", header.join(" | ")),
            _ => return None,
        };
        Some(truncate_with_suffix(
            &context,
            SYNTHETIC_CONTEXT_MAX_BYTES,
            "\n[context truncated]",
        ))
    }
}

#[derive(Debug, Clone)]
struct ParsedBlock {
    public: DocBlock,
    kind: BlockKind,
    section: u64,
    heading_instance: u64,
}

#[derive(Debug)]
enum DocumentItem {
    Heading {
        level: u8,
        text: String,
        range: Range<usize>,
    },
    Boundary,
    Body {
        kind: BlockKind,
        range: Range<usize>,
    },
}

fn parse_document(path: &str, bytes: &[u8]) -> Result<DocFile> {
    let content_hash = blake3::hash(bytes).to_hex().to_string();
    let (bom_bytes, markdown) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (3_usize, std::str::from_utf8(&bytes[3..])?)
    } else {
        (0_usize, std::str::from_utf8(bytes)?)
    };
    let front_matter = parse_front_matter(markdown);
    let body_base = bom_bytes + front_matter.body_start;
    let body = &markdown[front_matter.body_start..];
    let options = markdown_options();
    let is_mdx = is_mdx_path(Path::new(path));
    let protected = protected_code_ranges(body, body_base, options);
    let removals = comment_removals(bytes, body_base..bytes.len(), &protected, is_mdx);
    let items = document_items(body, body_base, options, bytes, &removals);
    let lines = LineIndex::new(bytes);
    let git_lines = GitLineIndex::new(bytes);

    let mut heading_levels = vec![None::<String>; 6];
    let mut headings = Vec::new();
    let mut blocks = Vec::new();
    let mut section = 0_u64;
    // Zero is the document preamble. Every parsed heading occurrence gets a
    // distinct identity, even when its rendered text repeats an earlier one.
    let mut heading_instance = 0_u64;
    let mut first_h1 = None;
    let mut mdx_preamble_open = is_mdx;

    for item in items {
        match item {
            DocumentItem::Heading { level, text, range } => {
                mdx_preamble_open = false;
                section += 1;
                heading_instance += 1;
                let slot = usize::from(level.saturating_sub(1)).min(5);
                heading_levels[slot] = Some(text.clone());
                for value in &mut heading_levels[slot + 1..] {
                    *value = None;
                }
                let breadcrumb = heading_levels
                    .iter()
                    .filter_map(Option::as_deref)
                    .collect::<Vec<_>>()
                    .join(" > ");
                if level == 1 && first_h1.is_none() {
                    first_h1 = Some(text.clone());
                }
                let (line_start, line_end) = lines.span(range.clone());
                headings.push(DocHeading {
                    level,
                    text,
                    breadcrumb,
                    source_start: range.start as u64,
                    source_end: range.end as u64,
                    line_start,
                    line_end,
                });
            }
            DocumentItem::Boundary => {
                mdx_preamble_open = false;
                section += 1;
            }
            DocumentItem::Body { kind, range } => {
                let rendered_body = render_source_range(bytes, range.clone(), &removals);
                let contributing_lines =
                    contributing_line_ranges(bytes, range.clone(), &removals, &git_lines);
                if rendered_body.is_empty() {
                    continue;
                }
                if mdx_preamble_open
                    && matches!(kind, BlockKind::Paragraph)
                    && is_esm_only(&rendered_body)
                {
                    continue;
                }
                mdx_preamble_open = false;
                let raw = std::str::from_utf8(&bytes[range.clone()])?.to_owned();
                let nearest_heading = heading_levels.iter().rev().find_map(Clone::clone);
                let breadcrumb = heading_levels
                    .iter()
                    .filter_map(Option::as_deref)
                    .collect::<Vec<_>>()
                    .join(" > ");
                let (line_start, line_end) = lines.span(range.clone());
                let ordinal = blocks.len() as u64;
                blocks.push(ParsedBlock {
                    public: DocBlock {
                        ordinal,
                        kind: kind.label().to_owned(),
                        source_start: range.start as u64,
                        source_end: range.end as u64,
                        line_start,
                        line_end,
                        contributing_lines,
                        content_hash: blake3::hash(&bytes[range.clone()]).to_hex().to_string(),
                        body: raw,
                        rendered_body,
                        breadcrumb,
                        nearest_heading,
                    },
                    kind,
                    section,
                    heading_instance,
                });
            }
        }
    }

    let title = front_matter.title.clone().or(first_h1).unwrap_or_else(|| {
        Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(path)
            .to_owned()
    });
    let public_blocks = blocks.iter().map(|block| block.public.clone()).collect();
    let mut chunks = build_chunks(bytes, &blocks, &removals, &lines, &git_lines);
    if chunks.is_empty() {
        let breadcrumb = headings
            .iter()
            .map(|heading| heading.text.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        let (line_start, line_end) = lines.span(0..bytes.len());
        chunks.push(DocChunk {
            ordinal: 0,
            same_heading_ordinal: 0,
            source_start: 0,
            source_end: bytes.len() as u64,
            line_start,
            line_end,
            contributing_lines: Vec::new(),
            breadcrumb,
            nearest_heading: None,
            rendered_body: String::new(),
            embedding_text: None,
            embedding_identity: None,
            block_ordinals: Vec::new(),
            is_stub: true,
        });
    }

    Ok(DocFile {
        path: path.to_owned(),
        content_hash,
        byte_len: bytes.len() as u64,
        line_count: line_count(bytes),
        title,
        description: front_matter.description,
        tags: front_matter.tags,
        front_matter_state: front_matter.state.to_owned(),
        headings,
        blocks: public_blocks,
        chunks,
    })
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
}

fn protected_code_ranges(body: &str, base: usize, options: Options) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) | Event::Code(_) => {
                ranges.push(base + range.start..base + range.end);
            }
            _ => {}
        }
    }
    ranges.sort_by_key(|range| range.start);
    merge_ranges(ranges)
}

fn merge_ranges(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

fn comment_removals(
    bytes: &[u8],
    body_range: Range<usize>,
    protected: &[Range<usize>],
    strip_jsx_comments: bool,
) -> Vec<Range<usize>> {
    let mut removals = Vec::new();
    let mut protected_index = 0;
    let mut cursor = body_range.start;
    while cursor < body_range.end {
        while protected_index < protected.len() && protected[protected_index].end <= cursor {
            protected_index += 1;
        }
        if protected
            .get(protected_index)
            .is_some_and(|range| range.contains(&cursor))
        {
            cursor = protected[protected_index].end;
            continue;
        }
        let delimiter = if bytes[cursor..body_range.end].starts_with(b"<!--") {
            Some((4_usize, b"-->".as_slice()))
        } else if strip_jsx_comments && bytes[cursor..body_range.end].starts_with(b"{/*") {
            Some((3_usize, b"*/}".as_slice()))
        } else {
            None
        };
        let Some((opener_len, closer)) = delimiter else {
            cursor += 1;
            continue;
        };
        let search_start = cursor + opener_len;
        let search_end = protected
            .get(protected_index)
            .map_or(body_range.end, |range| range.start.min(body_range.end));
        if search_start > search_end {
            cursor += opener_len;
            continue;
        }
        let Some(close_offset) = find_bytes(&bytes[search_start..search_end], closer) else {
            cursor += opener_len;
            continue;
        };
        let end = cursor + opener_len + close_offset + closer.len();
        removals.push(cursor..end);
        cursor = end;
    }
    removals
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn document_items(
    body: &str,
    base: usize,
    options: Options,
    bytes: &[u8],
    removals: &[Range<usize>],
) -> Vec<DocumentItem> {
    let mut items = Vec::new();
    let mut consumed_until = 0;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        if range.start < consumed_until {
            continue;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let absolute = base + range.start..base + range.end;
                let rendered = render_source_range(bytes, absolute.clone(), removals);
                items.push(DocumentItem::Heading {
                    level: heading_level(level),
                    text: render_heading(&rendered, options),
                    range: absolute,
                });
                consumed_until = range.end;
            }
            Event::Start(tag) => {
                let absolute = base + range.start..base + range.end;
                let rendered = render_source_range(bytes, absolute.clone(), removals);
                if let Some(kind) = body_block_kind(&tag, &rendered, options) {
                    items.push(DocumentItem::Body {
                        kind,
                        range: absolute,
                    });
                    consumed_until = range.end;
                }
            }
            Event::Rule => {
                items.push(DocumentItem::Boundary);
                consumed_until = range.end;
            }
            Event::Html(_) => {
                items.push(DocumentItem::Body {
                    kind: BlockKind::Html,
                    range: base + range.start..base + range.end,
                });
                consumed_until = range.end;
            }
            _ => {}
        }
    }
    items
}

fn is_esm_only(source: &str) -> bool {
    let allocator = Allocator::default();
    let parsed = OxcParser::new(&allocator, source, SourceType::jsx()).parse();
    !parsed.panicked
        && parsed.diagnostics.is_empty()
        && parsed.program.hashbang.is_none()
        && parsed.program.directives.is_empty()
        && !parsed.program.body.is_empty()
        && parsed
            .program
            .body
            .iter()
            .all(|statement| statement.is_module_declaration())
}

fn body_block_kind(tag: &Tag<'_>, source: &str, options: Options) -> Option<BlockKind> {
    match tag {
        Tag::Paragraph => Some(BlockKind::Paragraph),
        Tag::List(_) => Some(BlockKind::List),
        Tag::Table(_) => Some(BlockKind::Table {
            header: table_header(source, options),
        }),
        Tag::BlockQuote(_) => Some(BlockKind::Quote),
        Tag::CodeBlock(CodeBlockKind::Fenced(info)) => Some(BlockKind::FencedCode {
            info: info.trim_matches([' ', '\t']).to_owned(),
        }),
        Tag::CodeBlock(CodeBlockKind::Indented) => Some(BlockKind::IndentedCode),
        Tag::HtmlBlock => Some(BlockKind::Html),
        _ => None,
    }
}

fn render_heading(source: &str, options: Options) -> String {
    let mut rendered = String::new();
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Text(text) | Event::Code(text) => rendered.push_str(&text),
            Event::SoftBreak | Event::HardBreak => rendered.push(' '),
            _ => {}
        }
    }
    rendered.trim_matches([' ', '\t']).to_owned()
}

fn table_header(source: &str, options: Options) -> Vec<String> {
    let mut in_head = false;
    let mut cell = None::<String>;
    let mut cells = Vec::new();
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(Tag::TableHead) => in_head = true,
            Event::End(TagEnd::TableHead) => in_head = false,
            Event::Start(Tag::TableCell) if in_head => cell = Some(String::new()),
            Event::End(TagEnd::TableCell) if in_head => {
                if let Some(value) = cell.take() {
                    cells.push(value.trim_matches([' ', '\t']).to_owned());
                }
            }
            Event::Text(text) | Event::Code(text) if cell.is_some() => {
                cell.as_mut().expect("cell exists").push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if cell.is_some() => {
                cell.as_mut().expect("cell exists").push(' ');
            }
            _ => {}
        }
    }
    cells
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn render_source_range(bytes: &[u8], range: Range<usize>, removals: &[Range<usize>]) -> String {
    let mut filtered = Vec::with_capacity(range.len());
    let mut cursor = range.start;
    for removal in removals {
        if removal.end <= cursor {
            continue;
        }
        if removal.start >= range.end {
            break;
        }
        let keep_end = removal.start.max(cursor).min(range.end);
        filtered.extend_from_slice(&bytes[cursor..keep_end]);
        cursor = removal.end.max(cursor).min(range.end);
    }
    filtered.extend_from_slice(&bytes[cursor..range.end]);

    let mut normalized = Vec::with_capacity(filtered.len());
    let mut index = 0;
    while index < filtered.len() {
        if filtered[index] == b'\r' {
            normalized.push(b'\n');
            index += usize::from(filtered.get(index + 1) == Some(&b'\n')) + 1;
        } else {
            normalized.push(filtered[index]);
            index += 1;
        }
    }
    let start = normalized
        .iter()
        .position(|byte| *byte != b'\n')
        .unwrap_or(normalized.len());
    let end = normalized
        .iter()
        .rposition(|byte| *byte != b'\n')
        .map_or(start, |index| index + 1);
    String::from_utf8(normalized[start..end].to_vec())
        .expect("a source slice with ASCII removal remains UTF-8")
}

fn contributing_line_ranges(
    bytes: &[u8],
    range: Range<usize>,
    removals: &[Range<usize>],
    lines: &GitLineIndex,
) -> Vec<DocLineRange> {
    let mut contributing = Vec::new();
    for (index, &line_start) in lines.starts.iter().enumerate() {
        if line_start >= range.end {
            break;
        }
        let full_end = lines.starts.get(index + 1).copied().unwrap_or(bytes.len());
        if full_end <= range.start {
            continue;
        }
        let mut content_end = full_end;
        if content_end > line_start && bytes[content_end - 1] == b'\n' {
            content_end -= 1;
            if content_end > line_start && bytes[content_end - 1] == b'\r' {
                content_end -= 1;
            }
        } else if content_end > line_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        let retained = line_start.max(range.start)..content_end.min(range.end);
        if retained.start < retained.end && has_retained_non_whitespace(bytes, retained, removals) {
            let line = index as u64 + 1;
            append_line_range(
                &mut contributing,
                DocLineRange {
                    start: line,
                    end: line,
                },
            );
        }
    }
    contributing
}

fn has_retained_non_whitespace(
    bytes: &[u8],
    range: Range<usize>,
    removals: &[Range<usize>],
) -> bool {
    let mut cursor = range.start;
    for removal in removals {
        if removal.end <= cursor {
            continue;
        }
        if removal.start >= range.end {
            break;
        }
        let keep_end = removal.start.max(cursor).min(range.end);
        if bytes[cursor..keep_end]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace())
        {
            return true;
        }
        cursor = removal.end.max(cursor).min(range.end);
    }
    bytes[cursor..range.end]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
}

fn append_line_ranges(target: &mut Vec<DocLineRange>, ranges: Vec<DocLineRange>) {
    for range in ranges {
        append_line_range(target, range);
    }
}

fn append_line_range(target: &mut Vec<DocLineRange>, range: DocLineRange) {
    if let Some(previous) = target.last_mut()
        && range.start <= previous.end.saturating_add(1)
    {
        previous.end = previous.end.max(range.end);
    } else {
        target.push(range);
    }
}

#[derive(Debug)]
struct ChunkDraft {
    source_start: usize,
    source_end: usize,
    contributing_lines: Vec<DocLineRange>,
    heading_instance: u64,
    breadcrumb: String,
    nearest_heading: Option<String>,
    rendered_body: String,
    block_ordinals: Vec<u64>,
}

fn build_chunks(
    bytes: &[u8],
    blocks: &[ParsedBlock],
    removals: &[Range<usize>],
    lines: &LineIndex,
    git_lines: &GitLineIndex,
) -> Vec<DocChunk> {
    let mut drafts = Vec::<ChunkDraft>::new();
    let mut current = None::<(u64, ChunkDraft)>;

    for block in blocks {
        let nearest = block.public.nearest_heading.as_deref().map(|heading| {
            truncate_with_suffix(heading, HEADING_CONTEXT_MAX_BYTES, "\n[heading truncated]")
        });
        let provider = provider_text(nearest.as_deref(), &block.public.rendered_body);
        if provider.len() > HARD_MAX_BYTES {
            flush_draft(&mut current, &mut drafts);
            drafts.extend(split_block(bytes, block, removals, nearest, git_lines));
            continue;
        }

        let candidate = ChunkDraft {
            source_start: block.public.source_start as usize,
            source_end: block.public.source_end as usize,
            contributing_lines: block.public.contributing_lines.clone(),
            heading_instance: block.heading_instance,
            breadcrumb: block.public.breadcrumb.clone(),
            nearest_heading: nearest,
            rendered_body: block.public.rendered_body.clone(),
            block_ordinals: vec![block.public.ordinal],
        };
        match current.as_mut() {
            Some((section, draft))
                if *section == block.section
                    && draft.rendered_body.len() < TARGET_BYTES
                    && draft.rendered_body.len() + 2 + candidate.rendered_body.len()
                        <= MERGE_MAX_BYTES =>
            {
                draft.rendered_body.push_str("\n\n");
                draft.rendered_body.push_str(&candidate.rendered_body);
                draft.source_end = candidate.source_end;
                append_line_ranges(&mut draft.contributing_lines, candidate.contributing_lines);
                draft.block_ordinals.extend(candidate.block_ordinals);
            }
            Some(_) => {
                flush_draft(&mut current, &mut drafts);
                current = Some((block.section, candidate));
            }
            None => current = Some((block.section, candidate)),
        }
    }
    flush_draft(&mut current, &mut drafts);

    let mut previous_heading_instance = None;
    let mut same_heading_ordinal = 0_u64;
    drafts
        .into_iter()
        .enumerate()
        .map(|(global_ordinal, draft)| {
            if previous_heading_instance == Some(draft.heading_instance) {
                same_heading_ordinal += 1;
            } else {
                previous_heading_instance = Some(draft.heading_instance);
                same_heading_ordinal = 0;
            }
            let embedding_text =
                provider_text(draft.nearest_heading.as_deref(), &draft.rendered_body);
            debug_assert!(embedding_text.len() <= HARD_MAX_BYTES);
            let identity =
                embedding_identity(draft.nearest_heading.as_deref(), &draft.rendered_body);
            let (line_start, line_end) = lines.span(draft.source_start..draft.source_end);
            DocChunk {
                ordinal: global_ordinal as u64,
                same_heading_ordinal,
                source_start: draft.source_start as u64,
                source_end: draft.source_end as u64,
                line_start,
                line_end,
                contributing_lines: draft.contributing_lines,
                breadcrumb: draft.breadcrumb,
                nearest_heading: draft.nearest_heading,
                rendered_body: draft.rendered_body,
                embedding_text: Some(embedding_text),
                embedding_identity: Some(identity),
                block_ordinals: draft.block_ordinals,
                is_stub: false,
            }
        })
        .collect()
}

fn flush_draft(current: &mut Option<(u64, ChunkDraft)>, drafts: &mut Vec<ChunkDraft>) {
    if let Some((_, draft)) = current.take() {
        drafts.push(draft);
    }
}

fn split_block(
    bytes: &[u8],
    block: &ParsedBlock,
    removals: &[Range<usize>],
    nearest_heading: Option<String>,
    git_lines: &GitLineIndex,
) -> Vec<ChunkDraft> {
    let block_range = block.public.source_start as usize..block.public.source_end as usize;
    let synthetic = block.kind.synthetic_context().unwrap_or_default();
    let synthetic_context_line = (!synthetic.is_empty()).then(|| {
        let line = git_lines.line(block_range.start);
        DocLineRange {
            start: line,
            end: line,
        }
    });
    let fragment_context = FragmentContext {
        bytes,
        removals,
        synthetic: &synthetic,
        synthetic_context_line,
        git_lines,
    };
    let heading_overhead = nearest_heading
        .as_ref()
        .map_or(0, |heading| heading.len() + 2);
    let body_budget = HARD_MAX_BYTES
        .checked_sub(heading_overhead)
        .expect("bounded heading leaves a body budget");
    let native = native_boundaries(bytes, block, &block_range);
    let newlines = newline_boundaries(bytes, block_range.clone());
    let mut drafts = Vec::new();
    let mut start = block_range.start;

    while start < block_range.end {
        if fragment_fits(
            bytes,
            start..block_range.end,
            removals,
            &synthetic,
            body_budget,
        ) {
            drafts.push(fragment_context.draft(start..block_range.end, block, nearest_heading));
            break;
        }
        let end = last_fitting_boundary(
            bytes,
            start,
            block_range.end,
            &native,
            removals,
            &synthetic,
            body_budget,
        )
        .or_else(|| {
            last_fitting_boundary(
                bytes,
                start,
                block_range.end,
                &newlines,
                removals,
                &synthetic,
                body_budget,
            )
        })
        .unwrap_or_else(|| {
            last_fitting_utf8_boundary(
                bytes,
                start,
                block_range.end,
                removals,
                &synthetic,
                body_budget,
            )
        });
        assert!(end > start, "hard-bound splitting must make progress");
        drafts.push(fragment_context.draft(start..end, block, nearest_heading.clone()));
        start = end;
    }
    drafts
}

struct FragmentContext<'a> {
    bytes: &'a [u8],
    removals: &'a [Range<usize>],
    synthetic: &'a str,
    synthetic_context_line: Option<DocLineRange>,
    git_lines: &'a GitLineIndex,
}

impl FragmentContext<'_> {
    fn draft(
        &self,
        range: Range<usize>,
        block: &ParsedBlock,
        nearest_heading: Option<String>,
    ) -> ChunkDraft {
        let mut rendered = String::with_capacity(self.synthetic.len() + range.len());
        rendered.push_str(self.synthetic);
        rendered.push_str(&render_source_range(
            self.bytes,
            range.clone(),
            self.removals,
        ));
        let mut contributing_lines = self.synthetic_context_line.into_iter().collect::<Vec<_>>();
        append_line_ranges(
            &mut contributing_lines,
            contributing_line_ranges(self.bytes, range.clone(), self.removals, self.git_lines),
        );
        ChunkDraft {
            source_start: range.start,
            source_end: range.end,
            contributing_lines,
            heading_instance: block.heading_instance,
            breadcrumb: block.public.breadcrumb.clone(),
            nearest_heading,
            rendered_body: rendered,
            block_ordinals: vec![block.public.ordinal],
        }
    }
}

fn fragment_fits(
    bytes: &[u8],
    range: Range<usize>,
    removals: &[Range<usize>],
    synthetic: &str,
    budget: usize,
) -> bool {
    synthetic.len() + render_source_range(bytes, range, removals).len() <= budget
}

fn last_fitting_boundary(
    bytes: &[u8],
    start: usize,
    block_end: usize,
    boundaries: &[usize],
    removals: &[Range<usize>],
    synthetic: &str,
    budget: usize,
) -> Option<usize> {
    let candidates = boundaries.partition_point(|boundary| *boundary <= start)
        ..boundaries.partition_point(|boundary| *boundary < block_end);
    if candidates.is_empty() {
        return None;
    }
    let slice = &boundaries[candidates];
    let count =
        slice.partition_point(|end| fragment_fits(bytes, start..*end, removals, synthetic, budget));
    count.checked_sub(1).map(|index| slice[index])
}

fn last_fitting_utf8_boundary(
    bytes: &[u8],
    start: usize,
    end: usize,
    removals: &[Range<usize>],
    synthetic: &str,
    budget: usize,
) -> usize {
    let source = std::str::from_utf8(&bytes[start..end]).expect("captured file is UTF-8");
    let boundaries = source
        .char_indices()
        .skip(1)
        .map(|(offset, _)| start + offset)
        .filter(|boundary| !splits_crlf(bytes, *boundary))
        .chain(std::iter::once(end))
        .collect::<Vec<_>>();
    let count = boundaries.partition_point(|boundary| {
        fragment_fits(bytes, start..*boundary, removals, synthetic, budget)
    });
    count.checked_sub(1).map_or_else(
        || next_utf8_boundary(bytes, start, end),
        |index| boundaries[index],
    )
}

fn next_utf8_boundary(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut boundary = (start + 1).min(end);
    while boundary < end && std::str::from_utf8(&bytes[start..boundary]).is_err() {
        boundary += 1;
    }
    if splits_crlf(bytes, boundary) {
        boundary += 1;
    }
    boundary
}

fn splits_crlf(bytes: &[u8], boundary: usize) -> bool {
    boundary > 0
        && boundary < bytes.len()
        && bytes[boundary - 1] == b'\r'
        && bytes[boundary] == b'\n'
}

fn native_boundaries(bytes: &[u8], block: &ParsedBlock, range: &Range<usize>) -> Vec<usize> {
    match block.kind {
        BlockKind::FencedCode { .. } | BlockKind::IndentedCode => {
            newline_boundaries(bytes, range.clone())
        }
        BlockKind::Table { .. } => parser_boundaries(bytes, range, NativeBoundary::TableRow),
        BlockKind::List => parser_boundaries(bytes, range, NativeBoundary::TopLevelItem),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
enum NativeBoundary {
    TableRow,
    TopLevelItem,
}

fn parser_boundaries(bytes: &[u8], range: &Range<usize>, kind: NativeBoundary) -> Vec<usize> {
    let source = std::str::from_utf8(&bytes[range.clone()]).expect("captured file is UTF-8");
    let mut boundaries = Vec::new();
    let mut list_depth = 0_u32;
    for (event, event_range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item)
                if matches!(kind, NativeBoundary::TopLevelItem) && list_depth == 1 =>
            {
                boundaries.push(include_following_line_ending(
                    bytes,
                    range.start + event_range.end,
                    range.end,
                ));
            }
            Event::Start(Tag::TableRow) if matches!(kind, NativeBoundary::TableRow) => {
                boundaries.push(include_following_line_ending(
                    bytes,
                    range.start + event_range.end,
                    range.end,
                ));
            }
            _ => {}
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.retain(|boundary| *boundary > range.start && *boundary < range.end);
    boundaries
}

fn include_following_line_ending(bytes: &[u8], mut end: usize, limit: usize) -> usize {
    if end < limit && bytes[end] == b'\r' {
        end += 1;
    }
    if end < limit && bytes[end] == b'\n' {
        end += 1;
    }
    end
}

fn newline_boundaries(bytes: &[u8], range: Range<usize>) -> Vec<usize> {
    bytes[range.clone()]
        .iter()
        .enumerate()
        .filter_map(|(offset, byte)| (*byte == b'\n').then_some(range.start + offset + 1))
        .filter(|boundary| *boundary < range.end)
        .collect()
}

fn truncate_with_suffix(value: &str, max_bytes: usize, suffix: &str) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let prefix_budget = max_bytes.saturating_sub(suffix.len());
    let mut prefix_end = prefix_budget.min(value.len());
    while !value.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }
    let mut result = String::with_capacity(prefix_end + suffix.len());
    result.push_str(&value[..prefix_end]);
    result.push_str(suffix);
    result
}

#[derive(Debug)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                    starts.push(index + 2);
                    index += 2;
                }
                b'\r' | b'\n' => {
                    starts.push(index + 1);
                    index += 1;
                }
                _ => index += 1,
            }
        }
        Self { starts }
    }

    fn span(&self, range: Range<usize>) -> (u64, u64) {
        let start = self.line(range.start);
        let last_byte = range.end.saturating_sub(1).max(range.start);
        (start, self.line(last_byte))
    }

    fn line(&self, offset: usize) -> u64 {
        self.starts.partition_point(|start| *start <= offset) as u64
    }
}

/// Line index matching Git's LF-only logical-line model. Markdown display
/// spans continue to use `LineIndex`, which also recognizes bare CR; freshness
/// attribution must instead address the exact line numbers emitted by blame.
#[derive(Debug)]
struct GitLineIndex {
    starts: Vec<usize>,
}

impl GitLineIndex {
    fn new(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                starts.push(index + 1);
            }
        }
        Self { starts }
    }

    fn line(&self, offset: usize) -> u64 {
        self.starts.partition_point(|start| *start <= offset) as u64
    }
}

fn line_count(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut lines = 0_u64;
    let mut index = 0;
    let mut ended_with_break = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                lines += 1;
                index += 2;
                ended_with_break = true;
            }
            b'\r' | b'\n' => {
                lines += 1;
                index += 1;
                ended_with_break = true;
            }
            _ => {
                index += 1;
                ended_with_break = false;
            }
        }
    }
    lines + u64::from(!ended_with_break)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn glob_contract_is_pinned() -> Result<()> {
        validate_patterns(
            &["**/*.md".to_owned(), "**/*.mdx".to_owned()],
            &["drafts/**".to_owned()],
        )?;
        assert!(validate_patterns(&["{a,b}.md".to_owned()], &[]).is_err());
        assert!(validate_patterns(&["!README.md".to_owned()], &[]).is_err());
        assert!(validate_patterns(&[], &["drafts/".to_owned()]).is_err());
        let set = build_glob_set(&["**/*.md".to_owned()], "include")?;
        assert!(set.is_match("README.md"));
        assert!(set.is_match("guides/setup.md"));
        assert!(!set.is_match("README.MD"));
        let mdx = build_glob_set(&["**/*.mdx".to_owned()], "include")?;
        assert!(mdx.is_match("Guide.mdx"));
        assert!(!mdx.is_match("Guide.MDX"));
        Ok(())
    }

    #[test]
    fn embedding_serialization_matches_the_normative_preimage() {
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"jscout-doc-embedding-v1\0");
        preimage.push(0x01);
        preimage.extend_from_slice(&6_u64.to_be_bytes());
        preimage.extend_from_slice(b"API v2");
        preimage.extend_from_slice(&7_u64.to_be_bytes());
        preimage.extend_from_slice(b"Use it.");

        assert_eq!(
            provider_text(Some("API v2"), "Use it."),
            "API v2\n\nUse it."
        );
        assert_eq!(
            embedding_identity(Some("API v2"), "Use it."),
            blake3::hash(&preimage).to_hex().to_string()
        );

        let mut absent = Vec::new();
        absent.extend_from_slice(b"jscout-doc-embedding-v1\0");
        absent.push(0x00);
        absent.extend_from_slice(&0_u64.to_be_bytes());
        absent.extend_from_slice(&7_u64.to_be_bytes());
        absent.extend_from_slice(b"Use it.");
        assert_eq!(
            embedding_identity(None, "Use it."),
            blake3::hash(&absent).to_hex().to_string()
        );
    }

    #[test]
    fn membership_precedence_and_hidden_allowlist_are_visible() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join(".github/.private"))?;
        fs::create_dir_all(repo.path().join(".github/workflows"))?;
        fs::create_dir_all(repo.path().join("packages/app/.github"))?;
        fs::create_dir_all(repo.path().join("drafts"))?;
        fs::create_dir_all(repo.path().join(".git"))?;
        fs::write(repo.path().join(".gitignore"), "ignored.md\n")?;
        fs::write(repo.path().join("ignored.md"), "ignored")?;
        fs::write(repo.path().join("drafts/hidden.md"), "excluded")?;
        fs::write(repo.path().join("README.md"), "root")?;
        fs::write(repo.path().join("COMPONENT.mdx"), "# Component\n")?;
        fs::write(repo.path().join("WRONG.MD"), "wrong case")?;
        fs::write(repo.path().join(".github/workflows/help.md"), "allowed")?;
        fs::write(repo.path().join(".github/.private/no.md"), "hidden")?;
        fs::write(repo.path().join("packages/app/.github/no.md"), "hidden")?;

        let corpus = scan(
            repo.path(),
            &CorpusOptions {
                exclude: vec!["drafts/**".to_owned()],
                ..CorpusOptions::default()
            },
        )?;
        assert_eq!(
            corpus
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![".github/workflows/help.md", "COMPONENT.mdx", "README.md"]
        );
        assert_rule(&corpus, "ignored.md", "ignored");
        assert_rule(&corpus, "drafts/hidden.md", "excluded");
        assert!(
            corpus
                .decisions
                .iter()
                .all(|decision| decision.path != "WRONG.MD"),
            "ordinary non-document files do not amplify the decision log"
        );
        assert_rule(&corpus, ".github/.private", "hidden-not-allowlisted");
        assert_rule(&corpus, "packages/app/.github", "hidden-not-allowlisted");
        Ok(())
    }

    #[test]
    fn non_document_files_are_silent_unless_an_include_explicitly_matches() -> Result<()> {
        let repo = tempfile::tempdir()?;
        for (path, contents) in [
            ("README.md", "# Readme\n"),
            ("component.mdx", "# Component\n"),
            ("main.ts", "export const main = 1;\n"),
            ("package.json", "{}\n"),
            ("WRONG.MD", "wrong case\n"),
        ] {
            fs::write(repo.path().join(path), contents)?;
        }

        let ordinary = scan_repository(repo.path(), &CorpusOptions::default())?;
        assert_eq!(ordinary.documents.len(), 2);
        assert_eq!(ordinary.decisions.len(), 2);
        assert!(
            ordinary
                .decisions
                .iter()
                .all(|decision| decision.rule == "indexed")
        );

        let broad = scan_repository(
            repo.path(),
            &CorpusOptions {
                include: vec!["**/*".to_owned()],
                ..CorpusOptions::default()
            },
        )?;
        assert_eq!(broad.documents.len(), 2);
        let unsupported = broad
            .decisions
            .iter()
            .filter(|decision| decision.rule == "unsupported-extension")
            .map(|decision| decision.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(unsupported, ["WRONG.MD", "main.ts", "package.json"]);
        Ok(())
    }

    #[test]
    fn empty_include_disables_docs_without_narrowing_code_inventory() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const main = 1;\n")?;
        fs::write(repo.path().join("README.md"), "# Readme\n")?;
        fs::write(repo.path().join("component.mdx"), "# Component\n")?;

        let root = repo.path().canonicalize()?;
        let inventory = scan_repository(
            &root,
            &CorpusOptions {
                include: Vec::new(),
                ..CorpusOptions::default()
            },
        )?;
        assert_eq!(inventory.files, [root.join("main.ts")]);
        assert!(inventory.documents.is_empty());
        assert!(inventory.decisions.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_reported_and_not_followed() -> Result<()> {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("outside.md"), "outside")?;
        symlink(outside.path(), repo.path().join("linked"))?;
        symlink(
            outside.path().join("outside.md"),
            repo.path().join("linked-file.md"),
        )?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        assert!(corpus.files.is_empty());
        assert_rule(&corpus, "linked", "symlink-not-followed");
        assert_rule(&corpus, "linked-file.md", "symlink-not-followed");
        Ok(())
    }

    #[test]
    fn special_file_type_is_a_corpus_level_inventory_failure() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let file_type = fs::metadata(repo.path())?.file_type();
        let error =
            ensure_regular_inventory_file(&repo.path().join("special.md"), file_type).unwrap_err();
        assert!(error.to_string().contains("unsupported file type"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn non_markdown_special_files_do_not_widen_code_inventory_failures() -> Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let repo = tempfile::tempdir()?;
        let unrelated = repo.path().join("runtime.ts");
        let unrelated_native = CString::new(unrelated.as_os_str().as_bytes())?;
        let result = unsafe { libc::mkfifo(unrelated_native.as_ptr(), 0o600) };
        assert_eq!(result, 0);

        let inventory = scan_repository(repo.path(), &CorpusOptions::default())?;
        assert!(inventory.files.is_empty());
        assert!(inventory.documents.is_empty());

        let markdown_repo = tempfile::tempdir()?;
        let markdown = markdown_repo.path().join("special.md");
        let markdown_native = CString::new(markdown.as_os_str().as_bytes())?;
        let result = unsafe { libc::mkfifo(markdown_native.as_ptr(), 0o600) };
        assert_eq!(result, 0);
        assert!(scan_repository(markdown_repo.path(), &CorpusOptions::default()).is_err());
        Ok(())
    }

    #[test]
    fn permanent_capture_failure_is_visible_without_losing_other_inputs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const main = 1;\n")?;
        fs::write(repo.path().join("good.md"), "# Good\n\nCurrent.\n")?;
        fs::write(repo.path().join("denied.md"), "# Denied\n")?;

        let inventory = scan_repository_with_capture(
            repo.path(),
            &CorpusOptions::default(),
            &|path, max_bytes| {
                if path.file_name() == Some(std::ffi::OsStr::new("denied.md")) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected capture denial",
                    ));
                }
                capture_file(path, max_bytes)
            },
        )?;

        assert_eq!(
            inventory
                .files
                .iter()
                .map(|path| path.file_name().unwrap())
                .collect::<Vec<_>>(),
            [std::ffi::OsStr::new("main.ts")]
        );
        assert_eq!(
            inventory
                .documents
                .iter()
                .map(|document| document.file.path.as_str())
                .collect::<Vec<_>>(),
            ["good.md"]
        );
        let rejection = inventory
            .decisions
            .iter()
            .find(|decision| decision.path == "denied.md")
            .expect("capture rejection");
        assert_eq!(rejection.rule, "read-error");
        assert_eq!(rejection.detail.as_deref(), Some("injected capture denial"));
        Ok(())
    }

    #[test]
    fn retryable_capture_failure_aborts_the_corpus() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("guide.md"), "# Guide\n")?;

        let error =
            scan_repository_with_capture(repo.path(), &CorpusOptions::default(), &|_, _| {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("capture documentation file guide.md")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_have_a_lossless_rejection() -> Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let native = b"bad-\xFF.md".to_vec();
        let path = PathBuf::from(OsString::from_vec(native.clone()));
        assert!(normalized_utf8_path(&path).is_none());
        assert!(is_document_path(&path));
        let rejection = decision(&path, "file", "non-utf8-path", None);
        assert_eq!(rejection.path_encoding.as_deref(), Some("unix-bytes"));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(rejection.path_base64.as_deref().expect("base64 path"))?,
            native
        );

        Ok(())
    }

    #[test]
    fn explicit_stack_walks_deep_trees_without_a_depth_cap() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        let mut directory = root.clone();
        for _ in 0..384 {
            directory.push("d");
        }
        fs::create_dir_all(&directory)?;
        fs::write(directory.join("deep.ts"), "export const deep = 1;\n")?;
        fs::write(directory.join("deep.mdx"), "# Deep\n\nContent.\n")?;

        let inventory = scan_repository(&root, &CorpusOptions::default())?;
        assert_eq!(inventory.files, [directory.join("deep.ts")]);
        assert_eq!(inventory.documents.len(), 1);
        assert!(inventory.documents[0].file.path.ends_with("deep.mdx"));
        Ok(())
    }

    #[test]
    fn bom_front_matter_heading_and_identity_are_exact() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let bytes = b"\xEF\xBB\xBF---\r\ntitle: Manual\r\ndescription: yes\r\ntags: [api, v2]\r\n---\r\n# **API** `v2`\r\n\r\nUse it.\r\n";
        fs::write(repo.path().join("guide.md"), bytes)?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert_eq!(file.content_hash, blake3::hash(bytes).to_hex().to_string());
        assert_eq!(file.title, "Manual");
        assert_eq!(file.description.as_deref(), Some("yes"));
        assert_eq!(file.tags, ["api", "v2"]);
        assert_eq!(file.front_matter_state, "valid");
        assert_eq!(file.headings[0].text, "API v2");
        assert_eq!(file.headings[0].source_start, 63);
        let chunk = &file.chunks[0];
        assert_eq!(chunk.rendered_body, "Use it.");
        assert_eq!(chunk.nearest_heading.as_deref(), Some("API v2"));
        assert_eq!(chunk.embedding_text.as_deref(), Some("API v2\n\nUse it."));
        assert_eq!(
            chunk.embedding_identity.as_deref(),
            Some(embedding_identity(Some("API v2"), "Use it.").as_str())
        );
        Ok(())
    }

    #[test]
    fn malformed_front_matter_is_body_and_heading_only_document_is_stubbed() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("malformed.md"), "---\ntitle: [\n\n---\n")?;
        fs::write(repo.path().join("heading.md"), "# Heading\n## Child\n")?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let malformed = corpus
            .files
            .iter()
            .find(|file| file.path == "malformed.md")
            .expect("malformed file");
        assert_eq!(malformed.front_matter_state, "malformed_as_body");
        assert!(!malformed.blocks.is_empty());
        let heading = corpus
            .files
            .iter()
            .find(|file| file.path == "heading.md")
            .expect("heading file");
        assert_eq!(heading.chunks.len(), 1);
        assert!(heading.chunks[0].is_stub);
        assert_eq!(heading.chunks[0].same_heading_ordinal, 0);
        assert_eq!(heading.chunks[0].breadcrumb, "Heading > Child");
        assert!(heading.chunks[0].embedding_identity.is_none());
        Ok(())
    }

    #[test]
    fn front_matter_requires_exact_delimiters_mapping_and_unique_keys() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("duplicate.md"),
            "---\ntitle: one\ntitle: two\n---\nBody.\n",
        )?;
        fs::write(
            repo.path().join("scalar.md"),
            "---\njust a scalar\n---\nBody.\n",
        )?;
        fs::write(
            repo.path().join("spaced.md"),
            "--- \ntitle: not front matter\n---\nBody.\n",
        )?;
        fs::write(
            repo.path().join("types.md"),
            "---\ntitle: true\ndescription: false\ntags: [ok, true]\n---\nBody.\n",
        )?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        for path in ["duplicate.md", "scalar.md"] {
            let file = corpus
                .files
                .iter()
                .find(|file| file.path == path)
                .expect("fixture");
            assert_eq!(file.front_matter_state, "malformed_as_body", "{path}");
        }
        let spaced = corpus
            .files
            .iter()
            .find(|file| file.path == "spaced.md")
            .expect("spaced fixture");
        assert_eq!(spaced.front_matter_state, "absent");
        let types = corpus
            .files
            .iter()
            .find(|file| file.path == "types.md")
            .expect("typed fixture");
        assert_eq!(types.title, "types");
        assert!(types.description.is_none());
        assert!(types.tags.is_empty());
        Ok(())
    }

    #[test]
    fn adversarial_yaml_depth_and_aliases_fall_back_to_body() -> Result<()> {
        let nested = format!(
            "---\nvalue: {}null{}\n---\nBody.\n",
            "[".repeat(256),
            "]".repeat(256)
        );
        let nested = parse_document("nested.md", nested.as_bytes())?;
        assert_eq!(nested.front_matter_state, "malformed_as_body");

        let aliases = concat!(
            "---\n",
            "a: &a null\n",
            "b: &b [*a,*a,*a,*a,*a,*a,*a,*a,*a]\n",
            "c: &c [*b,*b,*b,*b,*b,*b,*b,*b,*b]\n",
            "d: &d [*c,*c,*c,*c,*c,*c,*c,*c,*c]\n",
            "e: &e [*d,*d,*d,*d,*d,*d,*d,*d,*d]\n",
            "f: &f [*e,*e,*e,*e,*e,*e,*e,*e,*e]\n",
            "g: &g [*f,*f,*f,*f,*f,*f,*f,*f,*f]\n",
            "h: &h [*g,*g,*g,*g,*g,*g,*g,*g,*g]\n",
            "i: &i [*h,*h,*h,*h,*h,*h,*h,*h,*h]\n",
            "---\n",
            "Body.\n",
        );
        let first = parse_document("aliases.md", aliases.as_bytes())?;
        let second = parse_document("aliases.md", aliases.as_bytes())?;
        assert_eq!(first.front_matter_state, "malformed_as_body");
        assert_eq!(second.front_matter_state, "malformed_as_body");
        assert_eq!(first.content_hash, second.content_hash);
        assert_eq!(first.chunks, second.chunks);
        Ok(())
    }

    #[test]
    fn rendering_removes_comments_except_code_and_merges_with_two_lfs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("render.md"),
            "# API\r\n\r\nFirst <!--gone--> paragraph.\r\n\r\nSecond `<!--kept-->`.\r\n\r\n```html\r\n<!--also kept-->\r\n```\r\n",
        )?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert_eq!(file.blocks.len(), 3);
        assert_eq!(file.chunks.len(), 1);
        assert_eq!(
            file.chunks[0].rendered_body,
            "First  paragraph.\n\nSecond `<!--kept-->`.\n\n```html\n<!--also kept-->\n```"
        );
        Ok(())
    }

    #[test]
    fn mdx_drops_only_leading_esm_and_unprotected_jsx_comments() -> Result<()> {
        let source = concat!(
            "\u{feff}---\n",
            "tags: [component]\n",
            "---\n",
            "import { Badge } from './badge'\n",
            "export const preambleOnlyNeedle = { title: 'Guide' }\n",
            "\n",
            "# Public {/* headingOnlyNeedle */}\n",
            "\n",
            "<Badge label=\"Deprecated\" since={version}>\n",
            "ActualInnerNeedle remains searchable.\n",
            "</Badge>\n",
            "\n",
            "Visible before {/* commentOnlyNeedle */} visible after.\n",
            "\n",
            "export const postHeadingNeedle = {version};\n",
            "\n",
            "`{/* inlineCodeNeedle */}`\n",
            "\n",
            "```mdx\n",
            "{/* fencedCodeNeedle */}\n",
            "```\n",
            "\n",
            "    {/* indentedCodeNeedle */}\n",
        );
        let file = parse_document("guide.mdx", source.as_bytes())?;
        let rendered = file
            .chunks
            .iter()
            .map(|chunk| chunk.rendered_body.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let embedded = file
            .chunks
            .iter()
            .filter_map(|chunk| chunk.embedding_text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n");

        assert_eq!(file.front_matter_state, "valid");
        assert_eq!(file.title, "Public");
        assert_eq!(file.tags, ["component"]);
        for removed in [
            "preambleOnlyNeedle",
            "headingOnlyNeedle",
            "commentOnlyNeedle",
        ] {
            assert!(!rendered.contains(removed), "rendered {removed}");
            assert!(!embedded.contains(removed), "embedded {removed}");
        }
        for retained in [
            "<Badge label=\"Deprecated\" since={version}>",
            "ActualInnerNeedle",
            "postHeadingNeedle",
            "{/* inlineCodeNeedle */}",
            "{/* fencedCodeNeedle */}",
            "{/* indentedCodeNeedle */}",
        ] {
            assert!(rendered.contains(retained), "missing {retained}");
            assert!(embedded.contains(retained), "missing embedded {retained}");
        }
        assert!(
            file.blocks
                .iter()
                .all(|block| !block.body.contains("preambleOnlyNeedle"))
        );
        assert!(
            file.blocks
                .iter()
                .any(|block| block.body.contains("commentOnlyNeedle")),
            "raw source block must retain the removed JSX comment"
        );

        let esm_only = parse_document(
            "module.mdx",
            b"import Thing from './thing'\nexport const metadata = { title: 'Only ESM' }\n",
        )?;
        assert!(esm_only.blocks.is_empty());
        assert_eq!(esm_only.chunks.len(), 1);
        assert!(esm_only.chunks[0].is_stub);
        assert!(esm_only.chunks[0].embedding_identity.is_none());
        assert!(esm_only.chunks[0].contributing_lines.is_empty());
        Ok(())
    }

    #[test]
    fn contributing_lines_exclude_removed_comments_and_mdx_preamble() -> Result<()> {
        let source = concat!(
            "import Widget from './widget'\n",
            "export const metadata = { privateNeedle: true }\n",
            "\n",
            "# Guide\n",
            "\n",
            "Before {/* inlineHidden */} after.\n",
            "\n",
            "Start {/*\n",
            "jsxCommentOnly\n",
            "*/} finish.\n",
            "\n",
            "{/*\n",
            "wholeJsxCommentOnly\n",
            "*/}\n",
            "\n",
            "After <!-- inlineHtmlHidden --> text.\n",
            "\n",
            "<!--\n",
            "wholeHtmlCommentOnly\n",
            "-->\n",
            "\n",
            "Last.\n",
        );
        let file = parse_document("guide.mdx", source.as_bytes())?;

        assert_eq!(
            file.blocks
                .iter()
                .map(|block| block.contributing_lines.clone())
                .collect::<Vec<_>>(),
            [
                vec![DocLineRange { start: 6, end: 6 }],
                vec![
                    DocLineRange { start: 8, end: 8 },
                    DocLineRange { start: 10, end: 10 },
                ],
                vec![DocLineRange { start: 16, end: 16 }],
                vec![DocLineRange { start: 22, end: 22 }],
            ]
        );
        assert_eq!(file.chunks.len(), 1);
        assert_eq!(
            file.chunks[0].contributing_lines,
            [
                DocLineRange { start: 6, end: 6 },
                DocLineRange { start: 8, end: 8 },
                DocLineRange { start: 10, end: 10 },
                DocLineRange { start: 16, end: 16 },
                DocLineRange { start: 22, end: 22 },
            ]
        );
        assert!(file.chunks[0].source_start < file.chunks[0].source_end);
        assert!(file.blocks[0].body.contains("inlineHidden"));
        assert!(file.blocks[1].body.contains("jsxCommentOnly"));
        assert!(
            file.blocks
                .iter()
                .all(|block| !block.body.contains("privateNeedle"))
        );
        Ok(())
    }

    #[test]
    fn contributing_lines_follow_git_lf_numbering_for_bare_cr() -> Result<()> {
        let file = parse_document(
            "bare-cr.mdx",
            b"Before.\r{/* commentOnlyNeedle */}\rAfter.\r",
        )?;
        assert_eq!(file.chunks.len(), 1);
        assert_eq!(
            file.chunks[0].contributing_lines,
            [DocLineRange { start: 1, end: 1 }]
        );
        assert!(file.chunks[0].line_end > 1);
        assert!(!file.chunks[0].rendered_body.contains("commentOnlyNeedle"));

        let comments_only = parse_document("comments-only.mdx", b"{/* hidden */}\r")?;
        assert!(comments_only.chunks[0].is_stub);
        assert!(comments_only.chunks[0].contributing_lines.is_empty());
        Ok(())
    }

    #[test]
    fn rendered_whitespace_without_contributing_lines_keeps_phase2_chunk_shape() -> Result<()> {
        let file = parse_document("whitespace.md", b"<!-- hidden -->   \n")?;
        assert_eq!(file.blocks.len(), 1);
        assert_eq!(file.blocks[0].rendered_body, "   ");
        assert!(file.blocks[0].contributing_lines.is_empty());
        assert_eq!(file.chunks.len(), 1);
        assert!(!file.chunks[0].is_stub);
        assert_eq!(file.chunks[0].rendered_body, "   ");
        assert!(file.chunks[0].contributing_lines.is_empty());
        Ok(())
    }

    #[test]
    fn mdx_esm_filter_is_contiguous_leading_and_paragraph_only() -> Result<()> {
        let split = parse_document(
            "split.mdx",
            concat!(
                "import FirstNeedle from './first'\n",
                "\n",
                "export const SecondNeedle = { enabled: true }\n",
                "\n",
                "Visible prose closes the preamble.\n",
                "\n",
                "export const LaterNeedle = 'retained';\n",
            )
            .as_bytes(),
        )?;
        let rendered = split
            .chunks
            .iter()
            .map(|chunk| chunk.rendered_body.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(!rendered.contains("FirstNeedle"));
        assert!(!rendered.contains("SecondNeedle"));
        assert!(rendered.contains("Visible prose"));
        assert!(rendered.contains("LaterNeedle"));

        for source in [
            "import this package before continuing.\n",
            "import('./dynamicNeedle')\n",
            "\"use client\";\n",
            "```js\nimport FencedNeedle from './fenced'\n```\n",
            "> import QuotedNeedle from './quoted'\n",
        ] {
            let file = parse_document("retained.mdx", source.as_bytes())?;
            assert!(!file.chunks[0].is_stub, "unexpected stub for {source:?}");
            assert_eq!(file.chunks[0].rendered_body, source.trim_end());
        }
        Ok(())
    }

    #[test]
    fn mdx_table_context_does_not_reintroduce_removed_comments() -> Result<()> {
        use std::fmt::Write as _;

        let mut source =
            String::from("| Name {/* tableCommentNeedle */} | Value |\n| --- | --- |\n");
        for index in 0..1_000 {
            writeln!(source, "| item-{index:04} | {} |", "x".repeat(24))?;
        }
        let file = parse_document("table.mdx", source.as_bytes())?;
        assert!(file.chunks.len() > 1);
        for chunk in &file.chunks {
            assert!(chunk.rendered_body.starts_with("[table Name | Value]\n"));
            assert_eq!(
                chunk.contributing_lines.first().map(|range| range.start),
                Some(1)
            );
            assert!(!chunk.rendered_body.contains("tableCommentNeedle"));
            assert!(
                !chunk
                    .embedding_text
                    .as_deref()
                    .expect("retrieval chunk")
                    .contains("tableCommentNeedle")
            );
        }
        Ok(())
    }

    #[test]
    fn jsx_comment_syntax_is_format_gated_and_unclosed_text_is_retained() -> Result<()> {
        let markdown = parse_document(
            "literal.md",
            b"Before {/* markdownLiteralNeedle */} after.\n",
        )?;
        assert!(
            markdown.chunks[0]
                .rendered_body
                .contains("{/* markdownLiteralNeedle */}")
        );
        let markdown_esm = parse_document(
            "module.md",
            b"import MarkdownEsmNeedle from './still-authored'\n",
        )?;
        assert!(
            markdown_esm.chunks[0]
                .rendered_body
                .contains("MarkdownEsmNeedle")
        );

        let mdx = parse_document("unclosed.mdx", b"Before {/* unclosedNeedle after.\n")?;
        assert!(mdx.chunks[0].rendered_body.contains("{/* unclosedNeedle"));

        let protected_closer = parse_document(
            "protected-closer.mdx",
            b"Before {/* unclosed `*/}` protectedCloserNeedle.\n",
        )?;
        assert_eq!(
            protected_closer.chunks[0].rendered_body,
            "Before {/* unclosed `*/}` protectedCloserNeedle."
        );
        Ok(())
    }

    #[test]
    fn thematic_separator_flushes_without_becoming_a_block() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("rule.md"), "Before.\n\n---\n\nAfter.\n")?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert_eq!(file.blocks.len(), 2);
        assert_eq!(file.chunks.len(), 2);
        assert_eq!(file.chunks[0].rendered_body, "Before.");
        assert_eq!(file.chunks[1].rendered_body, "After.");
        Ok(())
    }

    #[test]
    fn same_heading_ordinals_use_heading_instances_and_ignore_thematic_boundaries() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("ordinals.md"),
            "# Repeated\n\nFirst.\n\n---\n\nSecond.\n\n# Repeated\n\nThird.\n",
        )?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];

        assert_eq!(
            file.chunks
                .iter()
                .map(|chunk| chunk.ordinal)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(
            file.chunks
                .iter()
                .map(|chunk| chunk.same_heading_ordinal)
                .collect::<Vec<_>>(),
            [0, 1, 0]
        );
        assert!(
            file.chunks
                .iter()
                .all(|chunk| chunk.nearest_heading.as_deref() == Some("Repeated"))
        );
        Ok(())
    }

    #[test]
    fn five_thousand_byte_atomic_paragraph_stays_whole() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("large.md"), "a".repeat(5_000))?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert_eq!(file.blocks.len(), 1);
        assert_eq!(file.chunks.len(), 1);
        assert_eq!(file.chunks[0].rendered_body.len(), 5_000);
        Ok(())
    }

    #[test]
    fn oversized_fence_fragments_partition_source_and_repeat_context() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = format!(
            "# Code\n\n```rust extra\n{}\n```\n",
            "let x = 1;\n".repeat(3_000)
        );
        fs::write(repo.path().join("code.md"), &source)?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert!(file.chunks.len() > 1);
        for (ordinal, chunk) in file.chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, ordinal as u64);
            assert_eq!(chunk.same_heading_ordinal, ordinal as u64);
            assert!(chunk.rendered_body.starts_with("[fence rust extra]\n"));
            assert!(chunk.embedding_text.as_ref().unwrap().len() <= HARD_MAX_BYTES);
        }
        for pair in file.chunks.windows(2) {
            assert_eq!(pair[0].source_end, pair[1].source_start);
        }
        assert_eq!(
            file.chunks.first().unwrap().source_start,
            file.blocks[0].source_start
        );
        assert_eq!(
            file.chunks.last().unwrap().source_end,
            file.blocks[0].source_end
        );
        Ok(())
    }

    #[test]
    fn oversized_table_repeats_rendered_header_context() -> Result<()> {
        use std::fmt::Write as _;

        let repo = tempfile::tempdir()?;
        let mut source = String::from("# Data\n\n| **Name** | `Value` |\n| --- | --- |\n");
        for index in 0..2_000 {
            writeln!(source, "| item-{index:04} | {} |", "x".repeat(20))?;
        }
        fs::write(repo.path().join("table.md"), &source)?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        let file = &corpus.files[0];
        assert_eq!(file.blocks.len(), 1);
        assert_eq!(file.blocks[0].kind, "table");
        assert!(file.chunks.len() > 1);
        for chunk in &file.chunks {
            assert!(chunk.rendered_body.starts_with("[table Name | Value]\n"));
            assert!(chunk.embedding_text.as_ref().unwrap().len() <= HARD_MAX_BYTES);
        }
        for pair in file.chunks.windows(2) {
            assert_eq!(pair[0].source_end, pair[1].source_start);
        }
        assert_eq!(
            file.chunks.first().unwrap().source_start,
            file.blocks[0].source_start
        );
        assert_eq!(
            file.chunks.last().unwrap().source_end,
            file.blocks[0].source_end
        );
        Ok(())
    }

    #[test]
    fn fingerprint_is_exact_and_path_sorted() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("z.md"), "z")?;
        fs::write(repo.path().join("a.md"), "a")?;
        let corpus = scan(repo.path(), &CorpusOptions::default())?;
        assert_eq!(
            corpus
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["a.md", "z.md"]
        );
        let mut hasher = blake3::Hasher::new();
        hasher.update(CORPUS_DOMAIN);
        hasher.update(&2_u64.to_be_bytes());
        for (path, body) in [("a.md", b"a".as_slice()), ("z.md", b"z".as_slice())] {
            hasher.update(&(path.len() as u64).to_be_bytes());
            hasher.update(path.as_bytes());
            hasher.update(blake3::hash(body).as_bytes());
        }
        assert_eq!(corpus.fingerprint, hasher.finalize().to_hex().to_string());
        Ok(())
    }

    fn assert_rule(corpus: &Corpus, path: &str, rule: &str) {
        assert_eq!(
            corpus
                .decisions
                .iter()
                .find(|decision| decision.path == path)
                .map(|decision| decision.rule.as_str()),
            Some(rule),
            "decision for {path}"
        );
    }
}
