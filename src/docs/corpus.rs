use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
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
            include: vec!["**/*.md".to_owned()],
            exclude: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
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
pub struct RepositoryCorpus {
    pub source_files: Vec<PathBuf>,
    pub documents: Vec<CapturedDocument>,
    pub decisions: Vec<Decision>,
    pub rejections: Vec<InventoryRejection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRejection {
    pub path: PathBuf,
    pub stage: &'static str,
    pub error: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocBlock {
    pub ordinal: u64,
    pub kind: String,
    pub source_start: u64,
    pub source_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub content_hash: String,
    /// Exact source text for this block, including original line endings.
    pub body: String,
    pub rendered_body: String,
    pub breadcrumb: String,
    pub nearest_heading: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChunk {
    pub ordinal: u64,
    pub source_start: u64,
    pub source_end: u64,
    pub line_start: u64,
    pub line_end: u64,
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

pub fn scan_repository(root: &Path, options: &CorpusOptions) -> Result<RepositoryCorpus> {
    validate_patterns(&options.include, &options.exclude)?;
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("canonicalize documentation root {}", root.display()))?;
    if !canonical_root.is_dir() {
        bail!(
            "documentation root is not a directory: {}",
            canonical_root.display()
        );
    }

    let include = build_glob_set(&options.include, "include")?;
    let exclude = build_glob_set(&options.exclude, "exclude")?;
    let mut builder = WalkBuilder::new(&canonical_root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    let mut matchers = builder.build_matchers();
    let mut ignore = matchers
        .pop()
        .ok_or_else(|| anyhow!("documentation ignore matcher was not built"))?;

    let mut scanner = Scanner {
        root: &canonical_root,
        options,
        include,
        exclude,
        ignore: &mut ignore,
        candidates: Vec::new(),
        source_files: Vec::new(),
        documents: Vec::new(),
        decisions: Vec::new(),
        rejections: Vec::new(),
    };
    scanner.walk_directory(Path::new(""))?;
    scanner.acquire_candidates()?;
    scanner
        .source_files
        .sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    scanner
        .documents
        .sort_by(|left, right| left.file.path.cmp(&right.file.path));
    scanner.decisions.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| left.subject.cmp(&right.subject))
            .then_with(|| left.rule.cmp(&right.rule))
    });
    scanner.rejections.sort_by(|left, right| {
        left.path
            .as_os_str()
            .cmp(right.path.as_os_str())
            .then_with(|| left.stage.cmp(right.stage))
    });
    let source_files = std::mem::take(&mut scanner.source_files);
    let documents = std::mem::take(&mut scanner.documents);
    let decisions = std::mem::take(&mut scanner.decisions);
    let rejections = std::mem::take(&mut scanner.rejections);
    drop(scanner);
    Ok(RepositoryCorpus {
        source_files,
        documents,
        decisions,
        rejections,
    })
}

/// Parser-focused compatibility helper. Production indexing uses
/// [`scan_repository`] so Markdown is never published through an independent
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

struct Scanner<'a> {
    root: &'a Path,
    options: &'a CorpusOptions,
    include: GlobSet,
    exclude: GlobSet,
    ignore: &'a mut ignore::IncrementalIgnore,
    candidates: Vec<InventoryCandidate>,
    source_files: Vec<PathBuf>,
    documents: Vec<CapturedDocument>,
    decisions: Vec<Decision>,
    rejections: Vec<InventoryRejection>,
}

#[derive(Debug)]
struct InventoryCandidate {
    relative: PathBuf,
    normalized: String,
}

impl Scanner<'_> {
    fn walk_directory(&mut self, relative_directory: &Path) -> Result<()> {
        let directory = self.root.join(relative_directory);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                return self.handle_walk_error(relative_directory, &directory, error);
            }
        };
        let mut collected = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => collected.push(entry),
                Err(error) => {
                    self.handle_walk_error(relative_directory, &directory, error)?;
                    return Ok(());
                }
            }
        }
        let mut entries = collected;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let relative = relative_directory.join(entry.file_name());
            let absolute = entry.path();
            let metadata = match fs::symlink_metadata(&absolute) {
                Ok(metadata) => metadata,
                Err(error) if crate::io_policy::is_inventory_race(&error) => continue,
                Err(error) if crate::io_policy::is_retryable(&error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "inspect documentation inventory entry {}",
                            absolute.display()
                        )
                    });
                }
                Err(error) => {
                    self.rejections.push(InventoryRejection {
                        path: absolute,
                        stage: "walk",
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let file_type = metadata.file_type();
            let is_directory = file_type.is_dir();

            if is_directory && is_hard_skip(&relative) {
                self.decisions
                    .push(decision(&relative, "directory", "hard-skip", None));
                continue;
            }

            let (ignored, ignore_error) = self.ignore.matched_with_errors(&relative, is_directory);
            if let Some(error) = ignore_error {
                return Err(anyhow!(error.to_string())).with_context(|| {
                    format!("load ignore rules while matching {}", absolute.display())
                });
            }
            if ignored.is_ignore() {
                self.decisions.push(decision(
                    &relative,
                    if is_directory { "directory" } else { "file" },
                    "ignored",
                    None,
                ));
                continue;
            }

            if file_type.is_symlink() {
                self.decisions
                    .push(decision(&relative, "entry", "symlink-not-followed", None));
                continue;
            }

            if hidden_path_is_excluded(&relative) {
                self.decisions.push(decision(
                    &relative,
                    if is_directory { "directory" } else { "file" },
                    "hidden-not-allowlisted",
                    None,
                ));
                continue;
            }

            if is_directory {
                self.walk_directory(&relative)?;
                continue;
            }
            if !file_type.is_file() {
                // The code walker historically ignored non-file entries that
                // were neither directories nor symlinks. Preserve that
                // behavior for unrelated extensions, while a Markdown special
                // file remains a documentation inventory failure.
                if normalized_utf8_path(&relative).is_some_and(|path| path.ends_with(".md")) {
                    ensure_regular_inventory_file(&absolute, file_type)?;
                }
                continue;
            }

            // Hidden paths admitted specially for documentation must remain
            // invisible to the existing code corpus.
            if !path_has_hidden_component(&relative) && crate::walk::is_indexable(&absolute) {
                self.source_files.push(absolute.clone());
            }

            let Some(path) = normalized_utf8_path(&relative) else {
                self.decisions
                    .push(decision(&relative, "file", "non-utf8-path", None));
                continue;
            };
            if !path.ends_with(".md") {
                self.decisions
                    .push(decision(&relative, "file", "unsupported-extension", None));
                continue;
            }
            if self.exclude.is_match(&path) {
                self.decisions
                    .push(decision(&relative, "file", "excluded", None));
                continue;
            }
            if !self.include.is_match(&path) {
                self.decisions
                    .push(decision(&relative, "file", "not-included", None));
                continue;
            }

            self.candidates.push(InventoryCandidate {
                relative,
                normalized: path,
            });
        }
        Ok(())
    }

    fn handle_walk_error(
        &mut self,
        relative_directory: &Path,
        directory: &Path,
        error: std::io::Error,
    ) -> Result<()> {
        if relative_directory.as_os_str().is_empty() || crate::io_policy::is_retryable(&error) {
            return Err(error)
                .with_context(|| format!("discover repository under {}", directory.display()));
        }
        if !crate::io_policy::is_inventory_race(&error) {
            self.rejections.push(InventoryRejection {
                path: directory.to_path_buf(),
                stage: "walk",
                error: error.to_string(),
            });
        }
        Ok(())
    }

    fn acquire_candidates(&mut self) -> Result<()> {
        self.candidates
            .sort_by(|left, right| left.normalized.as_bytes().cmp(right.normalized.as_bytes()));
        for candidate in std::mem::take(&mut self.candidates) {
            let absolute = self.root.join(&candidate.relative);
            match capture_file(&absolute, self.options.max_file_bytes) {
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

fn is_hard_skip(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == ".git"
            || name
                .to_str()
                .is_some_and(|name| crate::walk::SKIP_DIRS.contains(&name))
    })
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

fn path_has_hidden_component(path: &Path) -> bool {
    path.components()
        .any(|component| os_str_starts_with_dot(component.as_os_str()))
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
    let protected = protected_code_ranges(body, body_base, options);
    let removals = comment_removals(bytes, body_base..bytes.len(), &protected);
    let items = document_items(body, body_base, options);
    let lines = LineIndex::new(bytes);

    let mut heading_levels = vec![None::<String>; 6];
    let mut headings = Vec::new();
    let mut blocks = Vec::new();
    let mut section = 0_u64;
    let mut first_h1 = None;

    for item in items {
        match item {
            DocumentItem::Heading { level, text, range } => {
                section += 1;
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
            DocumentItem::Boundary => section += 1,
            DocumentItem::Body { kind, range } => {
                let rendered_body = render_source_range(bytes, range.clone(), &removals);
                if rendered_body.is_empty() {
                    continue;
                }
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
                        content_hash: blake3::hash(&bytes[range.clone()]).to_hex().to_string(),
                        body: raw,
                        rendered_body,
                        breadcrumb,
                        nearest_heading,
                    },
                    kind,
                    section,
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
    let mut chunks = build_chunks(bytes, &blocks, &removals, &lines);
    if chunks.is_empty() {
        let breadcrumb = headings
            .iter()
            .map(|heading| heading.text.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        let (line_start, line_end) = lines.span(0..bytes.len());
        chunks.push(DocChunk {
            ordinal: 0,
            source_start: 0,
            source_end: bytes.len() as u64,
            line_start,
            line_end,
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
) -> Vec<Range<usize>> {
    let mut removals = Vec::new();
    let mut protected_index = 0;
    let mut cursor = body_range.start;
    while cursor + 4 <= body_range.end {
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
        if &bytes[cursor..cursor + 4] != b"<!--" {
            cursor += 1;
            continue;
        }
        let Some(close_offset) = find_bytes(&bytes[cursor + 4..body_range.end], b"-->") else {
            cursor += 4;
            continue;
        };
        let end = cursor + 4 + close_offset + 3;
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

fn document_items(body: &str, base: usize, options: Options) -> Vec<DocumentItem> {
    let mut items = Vec::new();
    let mut consumed_until = 0;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        if range.start < consumed_until {
            continue;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let absolute = base + range.start..base + range.end;
                items.push(DocumentItem::Heading {
                    level: heading_level(level),
                    text: render_heading(&body[range.clone()], options),
                    range: absolute,
                });
                consumed_until = range.end;
            }
            Event::Start(tag) => {
                if let Some(kind) = body_block_kind(&tag, &body[range.clone()], options) {
                    items.push(DocumentItem::Body {
                        kind,
                        range: base + range.start..base + range.end,
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

#[derive(Debug)]
struct ChunkDraft {
    source_start: usize,
    source_end: usize,
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
            drafts.extend(split_block(bytes, block, removals, nearest));
            continue;
        }

        let candidate = ChunkDraft {
            source_start: block.public.source_start as usize,
            source_end: block.public.source_end as usize,
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

    drafts
        .into_iter()
        .enumerate()
        .map(|(ordinal, draft)| {
            let embedding_text =
                provider_text(draft.nearest_heading.as_deref(), &draft.rendered_body);
            debug_assert!(embedding_text.len() <= HARD_MAX_BYTES);
            let identity =
                embedding_identity(draft.nearest_heading.as_deref(), &draft.rendered_body);
            let (line_start, line_end) = lines.span(draft.source_start..draft.source_end);
            DocChunk {
                ordinal: ordinal as u64,
                source_start: draft.source_start as u64,
                source_end: draft.source_end as u64,
                line_start,
                line_end,
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
) -> Vec<ChunkDraft> {
    let block_range = block.public.source_start as usize..block.public.source_end as usize;
    let synthetic = block.kind.synthetic_context().unwrap_or_default();
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
            drafts.push(fragment_draft(
                bytes,
                start..block_range.end,
                block,
                removals,
                &synthetic,
                nearest_heading,
            ));
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
        drafts.push(fragment_draft(
            bytes,
            start..end,
            block,
            removals,
            &synthetic,
            nearest_heading.clone(),
        ));
        start = end;
    }
    drafts
}

fn fragment_draft(
    bytes: &[u8],
    range: Range<usize>,
    block: &ParsedBlock,
    removals: &[Range<usize>],
    synthetic: &str,
    nearest_heading: Option<String>,
) -> ChunkDraft {
    let mut rendered = String::with_capacity(synthetic.len() + range.len());
    rendered.push_str(synthetic);
    rendered.push_str(&render_source_range(bytes, range.clone(), removals));
    ChunkDraft {
        source_start: range.start,
        source_end: range.end,
        breadcrumb: block.public.breadcrumb.clone(),
        nearest_heading,
        rendered_body: rendered,
        block_ordinals: vec![block.public.ordinal],
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
        validate_patterns(&["**/*.md".to_owned()], &["drafts/**".to_owned()])?;
        assert!(validate_patterns(&["{a,b}.md".to_owned()], &[]).is_err());
        assert!(validate_patterns(&["!README.md".to_owned()], &[]).is_err());
        assert!(validate_patterns(&[], &["drafts/".to_owned()]).is_err());
        let set = build_glob_set(&["**/*.md".to_owned()], "include")?;
        assert!(set.is_match("README.md"));
        assert!(set.is_match("guides/setup.md"));
        assert!(!set.is_match("README.MD"));
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
            vec![".github/workflows/help.md", "README.md"]
        );
        assert_rule(&corpus, "ignored.md", "ignored");
        assert_rule(&corpus, "drafts/hidden.md", "excluded");
        assert_rule(&corpus, "WRONG.MD", "unsupported-extension");
        assert_rule(&corpus, ".github/.private", "hidden-not-allowlisted");
        assert_rule(&corpus, "packages/app/.github", "hidden-not-allowlisted");
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
        assert!(inventory.source_files.is_empty());
        assert!(inventory.documents.is_empty());

        let markdown_repo = tempfile::tempdir()?;
        let markdown = markdown_repo.path().join("special.md");
        let markdown_native = CString::new(markdown.as_os_str().as_bytes())?;
        let result = unsafe { libc::mkfifo(markdown_native.as_ptr(), 0o600) };
        assert_eq!(result, 0);
        assert!(scan_repository(markdown_repo.path(), &CorpusOptions::default()).is_err());
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
        for chunk in &file.chunks {
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
