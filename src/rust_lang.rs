use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
pub use ra_ap_syntax::Edition;
use ra_ap_syntax::SourceFile;

use crate::chunk::{Chunk, ChunkKind, LineIndex};
use crate::fs_ops::FileSystem;

const TARGET_BYTES: usize = 4_800;
const HARD_MAX_BYTES: usize = 8_000;
pub(crate) const EDITION_CONTEXT_META_KEY: &str = "rust_edition_context_fingerprint";
pub(crate) const EDITION_CONTEXTS_META_KEY: &str = "rust_edition_contexts";

pub struct RustExtraction {
    pub chunks: Vec<Chunk>,
    pub parse_error_count: usize,
}

#[derive(Debug)]
pub struct EditionRejection {
    pub path: PathBuf,
    pub error: String,
}

/// Effective parser editions for the Rust files admitted by one repository
/// inventory. The fingerprint is an extraction input: changing a manifest's
/// effective edition must reparse unchanged source bytes and advance the
/// published snapshot.
#[derive(Debug)]
pub struct EditionResolution {
    editions: BTreeMap<PathBuf, Edition>,
    contexts: BTreeMap<String, String>,
    pub fingerprint: String,
    pub rejections: Vec<EditionRejection>,
}

impl EditionResolution {
    pub fn edition_for(&self, path: &Path) -> Edition {
        self.editions.get(path).copied().unwrap_or(Edition::DEFAULT)
    }

    pub fn has_rust_files(&self) -> bool {
        !self.editions.is_empty()
    }

    pub fn context_for_relative(&self, path: &str) -> Option<&str> {
        self.contexts
            .get(&path.replace('\\', "/"))
            .map(String::as_str)
    }

    pub fn contexts_json(&self) -> String {
        serde_json::to_string(&self.contexts).expect("Rust edition contexts serialize")
    }
}

#[derive(Debug)]
struct Manifest {
    path: PathBuf,
    value: Option<toml::Value>,
    error: Option<String>,
}

/// Resolve Cargo editions without invoking Cargo or rescanning the repository.
/// Only visible manifests captured by the authoritative walk participate.
/// Invalid or unreadable manifests are reported and recover with Cargo's
/// language default (Rust 2015), while retryable I/O remains transaction-fatal.
pub(crate) fn resolve_editions<F: FileSystem>(
    root: &Path,
    files: &[PathBuf],
    manifest_paths: &[PathBuf],
    fs: &F,
) -> Result<EditionResolution> {
    let rust_files = files
        .iter()
        .filter(|path| {
            crate::formats::repository_code_for_path(path)
                .is_some_and(|format| format.id == crate::formats::RUST)
        })
        .collect::<Vec<_>>();
    if rust_files.is_empty() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"jscout-rust-edition-context-v1\0");
        return Ok(EditionResolution {
            editions: BTreeMap::new(),
            contexts: BTreeMap::new(),
            fingerprint: hasher.finalize().to_hex().to_string(),
            rejections: Vec::new(),
        });
    }
    let mut manifests = BTreeMap::new();
    let mut rejection_by_path = BTreeMap::new();
    for path in manifest_paths {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let directory = relative.parent().unwrap_or(Path::new("")).to_path_buf();
        let (value, error) = match fs.read_to_string(path) {
            Ok(source) => match toml::from_str::<toml::Value>(&source) {
                Ok(value) => (Some(value), None),
                Err(error) => (
                    None,
                    Some(format!("parse Cargo manifest for Rust edition: {error}")),
                ),
            },
            Err(error) if crate::io_policy::is_inventory_race(&error) => (None, None),
            Err(error) if crate::io_policy::is_retryable(&error) => {
                return Err(error)
                    .with_context(|| format!("read Cargo edition input `{}`", path.display()));
            }
            Err(error) => (
                None,
                Some(format!("read Cargo manifest for Rust edition: {error}")),
            ),
        };
        manifests.insert(
            directory,
            Manifest {
                path: path.clone(),
                value,
                error,
            },
        );
    }

    let mut editions = BTreeMap::new();
    for file in rust_files {
        let relative = file.strip_prefix(root).unwrap_or(file);
        let Some((manifest_dir, manifest)) = nearest_manifest(relative, &manifests) else {
            editions.insert(file.clone(), Edition::DEFAULT);
            continue;
        };
        let edition = match effective_manifest_edition(manifest_dir, manifest, &manifests) {
            Ok(edition) => edition,
            Err(error) => {
                rejection_by_path
                    .entry(manifest.path.clone())
                    .or_insert(error);
                Edition::DEFAULT
            }
        };
        editions.insert(file.clone(), edition);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-rust-edition-context-v1\0");
    let mut contexts = BTreeMap::new();
    for (path, edition) in &editions {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let edition = edition.to_string();
        contexts.insert(normalized.clone(), edition.clone());
        for value in [normalized.as_str(), edition.as_str()] {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    let rejections = rejection_by_path
        .into_iter()
        .map(|(path, error)| EditionRejection { path, error })
        .collect();
    Ok(EditionResolution {
        editions,
        contexts,
        fingerprint: hasher.finalize().to_hex().to_string(),
        rejections,
    })
}

fn nearest_manifest<'a>(
    relative_file: &Path,
    manifests: &'a BTreeMap<PathBuf, Manifest>,
) -> Option<(&'a Path, &'a Manifest)> {
    let mut directory = relative_file.parent().unwrap_or(Path::new(""));
    loop {
        if let Some((directory, manifest)) = manifests.get_key_value(directory) {
            return Some((directory.as_path(), manifest));
        }
        let parent = directory.parent()?;
        directory = parent;
    }
}

fn effective_manifest_edition(
    manifest_dir: &Path,
    manifest: &Manifest,
    manifests: &BTreeMap<PathBuf, Manifest>,
) -> std::result::Result<Edition, String> {
    if let Some(error) = manifest.error.as_ref() {
        return Err(error.clone());
    }
    let Some(value) = manifest.value.as_ref() else {
        return Ok(Edition::DEFAULT);
    };
    let Some(package) = value.get("package").and_then(toml::Value::as_table) else {
        return Ok(Edition::DEFAULT);
    };
    let Some(edition) = package.get("edition") else {
        return Ok(Edition::DEFAULT);
    };
    if let Some(edition) = edition.as_str() {
        return edition
            .parse()
            .map_err(|error| format!("resolve Rust package edition: {error}"));
    }
    let inherited = edition
        .as_table()
        .and_then(|table| table.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true);
    if !inherited {
        return Err("resolve Rust package edition: expected a year or `workspace = true`".into());
    }

    let (_, workspace) = match package.get("workspace") {
        Some(workspace) => {
            let workspace = workspace.as_str().ok_or_else(|| {
                "resolve Rust workspace: `package.workspace` must be a string".to_string()
            })?;
            let directory = normalize_relative(manifest_dir, Path::new(workspace))?;
            manifests.get_key_value(&directory).ok_or_else(|| {
                format!(
                    "resolve Rust workspace: manifest referenced by `package.workspace` was not found at `{}`",
                    directory.display()
                )
            })?
        }
        None => {
            let mut directory = Some(manifest_dir);
            let mut workspace = None;
            while let Some(candidate) = directory {
                if let Some(entry) = manifests.get_key_value(candidate)
                    && entry
                        .1
                        .value
                        .as_ref()
                        .and_then(|value| value.get("workspace"))
                        .and_then(toml::Value::as_table)
                        .is_some()
                {
                    workspace = Some(entry);
                    break;
                }
                directory = candidate.parent();
            }
            workspace.ok_or_else(|| {
                "resolve inherited Rust edition: workspace manifest was not found".to_string()
            })?
        }
    };
    if let Some(error) = workspace.error.as_ref() {
        return Err(error.clone());
    }
    let edition = workspace
        .value
        .as_ref()
        .and_then(|value| value.get("workspace"))
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("package"))
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("edition"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            "resolve inherited Rust edition: `workspace.package.edition` is missing".to_string()
        })?;
    edition
        .parse()
        .map_err(|error| format!("resolve inherited Rust edition: {error}"))
}

fn normalize_relative(base: &Path, value: &Path) -> std::result::Result<PathBuf, String> {
    if value.is_absolute() {
        return Err("resolve Rust workspace: absolute `package.workspace` is unsupported".into());
    }
    let mut normalized = base.to_path_buf();
    for component in value.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir => {
                return Err(
                    "resolve Rust workspace: `package.workspace` escapes the index root".into(),
                );
            }
            Component::Normal(component) => normalized.push(component),
            Component::Prefix(_) | Component::RootDir => {
                return Err("resolve Rust workspace: invalid `package.workspace` path".into());
            }
        }
    }
    Ok(normalized)
}

pub fn extract(path: &Path, source: &str, edition: Edition) -> Result<RustExtraction> {
    let parsed = SourceFile::parse(source, edition);
    let syntax = parsed.syntax_node();
    let source_len = source.len();
    let syntax_end = u32::from(syntax.text_range().end()) as usize;
    ensure!(
        syntax_end == source_len,
        "Rust parser lossless span ended at {syntax_end}, expected {source_len}"
    );

    let mut units = Vec::new();
    let mut cursor = 0;
    for child in syntax.children() {
        let range = child.text_range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        ensure!(
            start >= cursor && end >= start && end <= source_len,
            "Rust parser emitted an invalid or overlapping top-level range"
        );
        if end > cursor {
            // Include the lossless tokens before the node so comments,
            // whitespace, shebangs, and malformed residual text cannot vanish.
            units.push(cursor..end);
            cursor = end;
        }
    }
    if cursor < source_len {
        units.push(cursor..source_len);
    }
    if units.is_empty() && !source.is_empty() {
        units.push(0..source_len);
    }

    let ranges = coalesce_and_split(source, units);
    validate_partition(source, &ranges)?;
    let lines = LineIndex::new(source);
    let file = path.to_string_lossy().into_owned();
    let chunks = ranges
        .into_iter()
        .map(|range| {
            let content = &source[range.clone()];
            let start = range.start as u32;
            let end = range.end as u32;
            Chunk {
                file: file.clone(),
                kind: ChunkKind::RustText,
                name: None,
                scope_chain: Vec::new(),
                symbols: Vec::new(),
                start,
                end,
                start_line: lines.line(start),
                end_line: lines.line(end.saturating_sub(1).max(start)),
                hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
                content: content.to_string(),
                file_imports: Vec::new(),
            }
        })
        .collect();

    Ok(RustExtraction {
        chunks,
        parse_error_count: parsed.errors().len(),
    })
}

fn coalesce_and_split(source: &str, units: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut pending: Option<Range<usize>> = None;
    for unit in units {
        if unit.is_empty() {
            continue;
        }
        if unit.len() > HARD_MAX_BYTES {
            if let Some(range) = pending.take() {
                chunks.push(range);
            }
            chunks.extend(split_oversized(source, unit));
            continue;
        }
        match &mut pending {
            Some(range) if unit.end - range.start <= TARGET_BYTES => range.end = unit.end,
            Some(range) => {
                let previous = std::mem::replace(range, unit);
                chunks.push(previous);
            }
            None => pending = Some(unit),
        }
    }
    if let Some(range) = pending {
        chunks.push(range);
    }
    chunks
}

fn split_oversized(source: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = range.start;
    while range.end - start > HARD_MAX_BYTES {
        let hard_end = start + HARD_MAX_BYTES;
        let safe_end = if source.as_bytes()[hard_end - 1] == b'\r'
            && source.as_bytes().get(hard_end) == Some(&b'\n')
        {
            hard_end - 1
        } else {
            hard_end
        };
        let newline = source.as_bytes()[start..safe_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|position| start + position + 1)
            .filter(|boundary| *boundary > start);
        let mut end = newline.unwrap_or(safe_end);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        debug_assert!(end > start);
        chunks.push(start..end);
        start = end;
    }
    if start < range.end {
        chunks.push(start..range.end);
    }
    chunks
}

fn validate_partition(source: &str, ranges: &[Range<usize>]) -> Result<()> {
    if source.is_empty() {
        ensure!(ranges.is_empty(), "empty Rust source emitted chunks");
        return Ok(());
    }
    let mut cursor = 0;
    for range in ranges {
        ensure!(
            range.start == cursor,
            "Rust chunks are not a gap-free source partition"
        );
        ensure!(
            range.end > range.start
                && range.end <= source.len()
                && source.is_char_boundary(range.start)
                && source.is_char_boundary(range.end),
            "Rust chunk range is empty, out of bounds, or not UTF-8 aligned"
        );
        ensure!(
            range.len() <= HARD_MAX_BYTES,
            "Rust chunk exceeds the hard byte bound"
        );
        ensure!(
            !(range.end < source.len()
                && source.as_bytes()[range.end - 1] == b'\r'
                && source.as_bytes()[range.end] == b'\n'),
            "Rust chunk boundary split CRLF"
        );
        cursor = range.end;
    }
    ensure!(
        cursor == source.len(),
        "Rust chunks do not cover the complete source"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn assert_contract(source: &str, extraction: &RustExtraction) {
        let mut cursor = 0_usize;
        let mut rebuilt = String::new();
        for chunk in &extraction.chunks {
            let start = chunk.start as usize;
            let end = chunk.end as usize;
            assert_eq!(start, cursor);
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
            assert_eq!(chunk.content.as_bytes(), &source.as_bytes()[start..end]);
            assert!(end - start <= HARD_MAX_BYTES);
            assert!(
                !(end < source.len()
                    && source.as_bytes()[end - 1] == b'\r'
                    && source.as_bytes()[end] == b'\n')
            );
            assert_eq!(chunk.kind, ChunkKind::RustText);
            assert!(chunk.name.is_none());
            assert!(chunk.scope_chain.is_empty());
            assert!(chunk.symbols.is_empty());
            assert!(chunk.file_imports.is_empty());
            rebuilt.push_str(&chunk.content);
            cursor = end;
        }
        assert_eq!(cursor, source.len());
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn chunks_are_exact_for_rust_lexical_edge_cases() -> Result<()> {
        let mut source = String::from(
            "#![allow(dead_code)]\r\n/* outer /* nested */ done */\r\n\
             pub fn lifetime<'a>(value: &'a str) -> &'a str { value }\r\n\
             const RAW: &str = r###\"quote \" and ## lookalike — λ\"###;\r\n\
             const BYTE: &[u8] = br#\"bytes \\xFF\"#;\r\n",
        );
        for index in 0..700 {
            let _ = writeln!(
                source,
                "pub const ITEM_{index}: &str = \"padding-{index}-界\";\r"
            );
        }
        let extraction = extract(Path::new("src/lib.rs"), &source, Edition::Edition2024)?;
        assert!(extraction.chunks.len() > 3);
        assert_eq!(extraction.parse_error_count, 0);
        assert_contract(&source, &extraction);
        Ok(())
    }

    #[test]
    fn malformed_mid_edit_remains_lossless_and_counted() -> Result<()> {
        let source = "pub fn before() {}\nfn broken() { let value = ;\npub fn after() {}\n";
        let extraction = extract(Path::new("broken.rs"), source, Edition::Edition2024)?;
        assert!(extraction.parse_error_count > 0);
        assert_contract(source, &extraction);
        assert!(
            extraction
                .chunks
                .iter()
                .any(|chunk| chunk.content.contains("pub fn after"))
        );
        Ok(())
    }

    #[test]
    fn empty_source_emits_no_chunks() -> Result<()> {
        let extraction = extract(Path::new("empty.rs"), "", Edition::Edition2024)?;
        assert!(extraction.chunks.is_empty());
        assert_eq!(extraction.parse_error_count, 0);
        Ok(())
    }

    #[test]
    fn hard_bound_never_splits_crlf_or_utf8() -> Result<()> {
        let mut source = " ".repeat(HARD_MAX_BYTES - 1);
        source.push_str("\r\n");
        source.push_str(&" ".repeat(HARD_MAX_BYTES - 2));
        source.push('界');
        source.push_str(&" ".repeat(HARD_MAX_BYTES));

        let extraction = extract(Path::new("boundaries.rs"), &source, Edition::Edition2024)?;

        assert!(extraction.chunks.len() >= 3);
        assert_contract(&source, &extraction);
        assert_eq!(extraction.chunks[0].end as usize, HARD_MAX_BYTES - 1);
        Ok(())
    }

    #[test]
    fn parser_diagnostics_respect_the_selected_edition() -> Result<()> {
        let source = "pub fn gen() {}\n";
        let legacy = extract(Path::new("legacy.rs"), source, Edition::Edition2021)?;
        let current = extract(Path::new("current.rs"), source, Edition::Edition2024)?;
        assert_eq!(legacy.parse_error_count, 0);
        assert!(current.parse_error_count > 0);
        assert_contract(source, &legacy);
        assert_contract(source, &current);
        Ok(())
    }

    #[test]
    fn resolves_direct_default_and_workspace_inherited_editions() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        std::fs::create_dir_all(root.join("member/src"))?;
        std::fs::create_dir_all(root.join("legacy/src"))?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=['member', 'legacy']\n[workspace.package]\nedition='2021'\n",
        )?;
        std::fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname='member'\nversion='0.1.0'\nedition.workspace=true\n",
        )?;
        std::fs::write(
            root.join("legacy/Cargo.toml"),
            "[package]\nname='legacy'\nversion='0.1.0'\n",
        )?;
        for path in ["member/src/lib.rs", "legacy/src/lib.rs", "standalone.rs"] {
            std::fs::write(root.join(path), "pub fn value() {}\n")?;
        }
        let files = ["member/src/lib.rs", "legacy/src/lib.rs", "standalone.rs"]
            .into_iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        let manifests = ["Cargo.toml", "legacy/Cargo.toml", "member/Cargo.toml"]
            .into_iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        let resolution = resolve_editions(&root, &files, &manifests, &crate::fs_ops::OsFileSystem)?;
        assert_eq!(
            resolution.edition_for(&root.join("member/src/lib.rs")),
            Edition::Edition2021
        );
        assert_eq!(
            resolution.edition_for(&root.join("legacy/src/lib.rs")),
            Edition::Edition2015
        );
        assert_eq!(
            resolution.edition_for(&root.join("standalone.rs")),
            Edition::Edition2015
        );
        assert!(resolution.rejections.is_empty());
        Ok(())
    }

    #[test]
    fn explicit_workspace_pointer_is_authoritative() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        for package in ["implicit", "explicit", "missing", "non-string"] {
            std::fs::create_dir_all(root.join(package).join("src"))?;
            std::fs::write(root.join(package).join("src/lib.rs"), "pub fn value() {}\n")?;
        }
        std::fs::create_dir_all(root.join("alternate"))?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=['implicit', 'missing', 'non-string']\n[workspace.package]\nedition='2024'\n",
        )?;
        std::fs::write(
            root.join("alternate/Cargo.toml"),
            "[workspace]\nmembers=['../explicit']\n[workspace.package]\nedition='2021'\n",
        )?;
        std::fs::write(
            root.join("implicit/Cargo.toml"),
            "[package]\nname='implicit'\nversion='0.1.0'\nedition.workspace=true\n",
        )?;
        std::fs::write(
            root.join("explicit/Cargo.toml"),
            "[package]\nname='explicit'\nversion='0.1.0'\nworkspace='../alternate'\nedition.workspace=true\n",
        )?;
        std::fs::write(
            root.join("missing/Cargo.toml"),
            "[package]\nname='missing'\nversion='0.1.0'\nworkspace='../absent'\nedition.workspace=true\n",
        )?;
        std::fs::write(
            root.join("non-string/Cargo.toml"),
            "[package]\nname='non-string'\nversion='0.1.0'\nworkspace=true\nedition.workspace=true\n",
        )?;

        let files = [
            "explicit/src/lib.rs",
            "implicit/src/lib.rs",
            "missing/src/lib.rs",
            "non-string/src/lib.rs",
        ]
        .into_iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
        let manifests = [
            "Cargo.toml",
            "alternate/Cargo.toml",
            "explicit/Cargo.toml",
            "implicit/Cargo.toml",
            "missing/Cargo.toml",
            "non-string/Cargo.toml",
        ]
        .into_iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
        let resolution = resolve_editions(&root, &files, &manifests, &crate::fs_ops::OsFileSystem)?;

        assert_eq!(
            resolution.edition_for(&root.join("implicit/src/lib.rs")),
            Edition::Edition2024
        );
        assert_eq!(
            resolution.edition_for(&root.join("explicit/src/lib.rs")),
            Edition::Edition2021
        );
        for path in ["missing/src/lib.rs", "non-string/src/lib.rs"] {
            assert_eq!(
                resolution.edition_for(&root.join(path)),
                Edition::Edition2015
            );
        }

        let rejections = resolution
            .rejections
            .iter()
            .map(|rejection| (rejection.path.as_path(), rejection.error.as_str()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(rejections.len(), 2);
        assert!(
            rejections[&root.join("missing/Cargo.toml").as_path()]
                .contains("manifest referenced by `package.workspace` was not found")
        );
        assert!(
            rejections[&root.join("non-string/Cargo.toml").as_path()]
                .contains("`package.workspace` must be a string")
        );
        Ok(())
    }

    #[test]
    fn effective_edition_changes_the_context_fingerprint() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        std::fs::create_dir_all(root.join("src"))?;
        let manifest = root.join("Cargo.toml");
        let source = root.join("src/lib.rs");
        std::fs::write(&source, "pub fn gen() {}\n")?;
        std::fs::write(
            &manifest,
            "[package]\nname='sample'\nversion='0.1.0'\nedition='2021'\n",
        )?;
        let first = resolve_editions(
            &root,
            std::slice::from_ref(&source),
            std::slice::from_ref(&manifest),
            &crate::fs_ops::OsFileSystem,
        )?;
        std::fs::write(
            &manifest,
            "[package]\nname='sample'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        let second = resolve_editions(
            &root,
            std::slice::from_ref(&source),
            std::slice::from_ref(&manifest),
            &crate::fs_ops::OsFileSystem,
        )?;
        assert_eq!(first.edition_for(&source), Edition::Edition2021);
        assert_eq!(second.edition_for(&source), Edition::Edition2024);
        assert_ne!(first.fingerprint, second.fingerprint);
        Ok(())
    }

    #[test]
    fn invalid_manifest_edition_is_visible_and_uses_cargo_default() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let root = repo.path().canonicalize()?;
        std::fs::create_dir_all(root.join("src"))?;
        let manifest = root.join("Cargo.toml");
        let source = root.join("src/lib.rs");
        std::fs::write(&source, "pub fn gen() {}\n")?;
        std::fs::write(
            &manifest,
            "[package]\nname='sample'\nversion='0.1.0'\nedition='2099'\n",
        )?;
        let resolution = resolve_editions(
            &root,
            std::slice::from_ref(&source),
            std::slice::from_ref(&manifest),
            &crate::fs_ops::OsFileSystem,
        )?;
        assert_eq!(resolution.edition_for(&source), Edition::Edition2015);
        assert_eq!(resolution.rejections.len(), 1);
        assert_eq!(resolution.rejections[0].path, manifest);
        assert!(resolution.rejections[0].error.contains("invalid edition"));
        Ok(())
    }
}
