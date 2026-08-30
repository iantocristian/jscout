use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use rusqlite::{Connection, OptionalExtension, params};

use crate::chunk::{Chunk, Chunker, LineIndex};
use crate::dependency::{self, DependencyLimits};
use crate::docs::corpus::{self, CapturedDocument, CorpusOptions, Decision, DocFile};
use crate::docs::provenance::{
    BlameCacheKey, GitChunkBasis, PublicationValidation, RepositoryCapture,
};
use crate::docs::provenance_store::{self, ResolvedDocumentProvenance};
use crate::formats::{self, Capability, Extractor};
use crate::fs_ops::{FileSystem, OsFileSystem};
use crate::graph::{self, FileGraph};
use crate::package_exports::RESOLVE_CONDITIONS;
use crate::{file_role, io_policy, parse, store};

const DOC_CHUNK_FORMAT_META_KEY: &str = "documentation_chunk_format_version";
const DOC_PROVENANCE_FORMAT_META_KEY: &str = "documentation_provenance_format_version";
const CODE_CORPUS: &str = formats::Corpus::Code.as_str();
const DOCS_CORPUS: &str = formats::Corpus::Docs.as_str();

#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub dependencies: Vec<String>,
    pub dependency_limits: DependencyLimits,
    pub docs_include: Vec<String>,
    pub docs_exclude: Vec<String>,
    pub docs_freshness: bool,
    pub timing: bool,
    pub debug: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            dependencies: Vec::new(),
            dependency_limits: DependencyLimits::default(),
            docs_include: crate::docs::default_include_globs(),
            docs_exclude: Vec::new(),
            docs_freshness: false,
            timing: false,
            debug: false,
        }
    }
}

pub struct IndexOutcome {
    pub indexed: usize,
    pub unchanged: usize,
    /// Previously indexed first-party files omitted from the resulting
    /// snapshot because they disappeared or had a non-retryable
    /// read/extraction failure.
    pub removed: usize,
    /// Inputs or repository boundaries omitted because they had a
    /// non-retryable traversal, manifest, read, or extraction failure. These
    /// are visible corpus exclusions, not phase failures.
    pub rejected: usize,
    pub rejections: Vec<IndexRejection>,
    pub diagnostics: Vec<IndexDiagnostic>,
    pub chunks: usize,
    pub refs: usize,
    pub dependency_packages: usize,
    pub dependency_files: usize,
    pub dependency_bytes: u64,
    pub dependency_skipped: usize,
    pub dependency_skipped_bytes: u64,
    pub dependency_plans: Vec<String>,
    /// Current-snapshot Rust files that contain one or more recoverable parser
    /// diagnostics. These files remain indexed and searchable.
    pub rust_files_with_parse_errors: usize,
    /// Current-snapshot recoverable parser diagnostics across Rust files.
    pub rust_parse_error_count: usize,
    /// True when a validated active checker publication was rebound to this
    /// snapshot without planning or invoking the checker provider.
    pub checker_rebound: bool,
    /// False when the structural projection was provably identical (same
    /// snapshot, projection version, and module resolution) and was kept.
    pub projection_rebuilt: bool,
    /// True when an explicit fixed-snapshot refresh truncated the disposable
    /// snapshot tables wholesale instead of replacing files one at a time.
    pub extraction_reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRejection {
    pub path: String,
    pub stage: &'static str,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDiagnostic {
    pub path: Option<String>,
    pub stage: &'static str,
    pub detail: String,
}

fn retryable_read_failure(path: &str, error: std::io::Error) -> anyhow::Error {
    anyhow::Error::new(error).context(format!("retryable read failure for `{path}`"))
}

impl IndexOutcome {
    fn record_rejection(
        &mut self,
        path: impl Into<String>,
        stage: &'static str,
        error: impl std::fmt::Display,
    ) {
        self.rejected += 1;
        self.rejections.push(IndexRejection {
            path: path.into(),
            stage,
            error: error.to_string(),
        });
    }
}

pub fn report_rejections(outcome: &IndexOutcome) {
    if !outcome.rejections.is_empty() {
        eprintln!("index inputs rejected ({}):", outcome.rejections.len());
        for rejection in &outcome.rejections {
            let error = rejection.error.replace('\n', "\n      ");
            eprintln!("  [{}] {}: {error}", rejection.stage, rejection.path);
        }
    }
    for diagnostic in &outcome.diagnostics {
        let detail = diagnostic.detail.replace('\n', "\n      ");
        eprintln!(
            "index diagnostic [{}] {}: {detail}",
            diagnostic.stage,
            diagnostic.path.as_deref().unwrap_or("<repository>"),
        );
    }
}

struct FileData {
    chunks: Vec<Chunk>,
    graph: FileGraph,
    lines: LineIndex,
    parse_error_count: usize,
}

struct PreparedDependencyFile {
    package_root: PathBuf,
    display: String,
    source_path: PathBuf,
    package_path: String,
    source: String,
    hash: String,
    role: &'static str,
}

struct StoredDependencyFile {
    id: i64,
    hash: String,
    role: String,
    corpus: String,
    format: String,
    package_instance_id: i64,
    package_path: String,
}

struct FileIdentity<'a> {
    path: &'a str,
    hash: &'a str,
    corpus: &'a str,
    format: &'a str,
    role: &'a str,
    origin: &'a str,
    package_instance_id: Option<i64>,
    package_path: Option<&'a str>,
}

pub(crate) fn resolver_options(
    alias: oxc_resolver::Alias,
    tsconfig: Option<TsconfigDiscovery>,
) -> ResolveOptions {
    ResolveOptions {
        // Workspace package names -> in-repo source, so monorepo cross-package
        // imports resolve to indexed files instead of missing/dist targets.
        alias,
        extensions: formats::ecmascript_resolution_extensions()
            .into_iter()
            .map(|extension| format!(".{extension}"))
            .chain(std::iter::once(".json".to_string()))
            .collect(),
        // TS convention: `./x.js` in source may mean `./x.ts` on disk.
        extension_alias: formats::ecmascript_resolver_extension_aliases(),
        condition_names: RESOLVE_CONDITIONS
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        main_fields: vec!["module".into(), "main".into()],
        tsconfig,
        ..ResolveOptions::default()
    }
}

/// Incrementally index a repository for differential implementation tests.
#[cfg(test)]
pub fn index_repo(root: &Path, conn: &Connection) -> Result<IndexOutcome> {
    index_repo_with_options(root, conn, &IndexOptions::default())
}

/// Test convenience wrapper for the production incremental refresh.
#[cfg(test)]
pub fn index_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::new(&OsFileSystem),
    )
}

#[cfg(test)]
pub(crate) fn index_repo_with_fs(
    root: &Path,
    conn: &Connection,
    fs: &impl FileSystem,
) -> Result<IndexOutcome> {
    index_repo_with_options_and_fs(root, conn, &IndexOptions::default(), fs)
}

#[cfg(test)]
pub(crate) fn index_repo_with_options_and_fs(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    fs: &impl FileSystem,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::new(fs),
    )
}

/// Refresh the published snapshot by retaining unchanged first-party rows and
/// replacing changed or missing files. This is a watch latency optimization:
/// it still scans and hashes the complete current source tree, re-evaluates
/// dependency ownership and module resolution, and publishes the same snapshot
/// contract as a full refresh.
pub fn incremental_refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::Incremental,
        CheckerRetention::PreserveActiveForWatch,
        IndexOperation::new(&OsFileSystem),
    )
}

/// Incremental watcher refresh for a generation known to contain only
/// checker-ineligible source changes. When the complete checker-eligible
/// canonical identity and module-resolution identity are unchanged, the
/// active checker batch is rebound to the replacement code digest without
/// planning or launching the checker.
pub fn incremental_refresh_repo_rebinding_checker(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::Incremental,
        CheckerRetention::ValidateActive {
            retain_failed_for_watch: true,
        },
        IndexOperation::new(&OsFileSystem),
    )
}

/// Rebuild every repository-derived row for the current checkout while
/// preserving content-addressed embeddings and semantic memory.
pub fn refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::FullRefresh,
        CheckerRetention::ValidateActive {
            retain_failed_for_watch: false,
        },
        IndexOperation::new(&OsFileSystem),
    )
}

/// Full structural refresh for the watcher. Unlike manual `jscout index`, it
/// keeps the previous active checker batch and newest superseded staging batch
/// as hidden carry sources for the following enrichment phase.
pub fn watch_full_refresh_repo_with_options(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::FullRefresh,
        CheckerRetention::PreserveActiveForWatch,
        IndexOperation::new(&OsFileSystem),
    )
}

/// Full watcher refresh for a generation classified as checker-ineligible.
/// Canonical rows are rebuilt from scratch, while an active checker batch is
/// rebound only after its complete canonical and recorded-input identity is
/// proven unchanged.
pub fn watch_full_refresh_repo_rebinding_checker(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        options,
        IndexMode::FullRefresh,
        CheckerRetention::ValidateActive {
            retain_failed_for_watch: true,
        },
        IndexOperation::new(&OsFileSystem),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    Incremental,
    FullRefresh,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckerRetention {
    #[cfg(test)]
    Drop,
    PreserveActiveForWatch,
    ValidateActive {
        retain_failed_for_watch: bool,
    },
}

impl CheckerRetention {
    fn validates_active(self) -> bool {
        matches!(self, Self::ValidateActive { .. })
    }
}

/// Environment capabilities shared by every filesystem-sensitive phase of a
/// single indexing operation. User policy remains plain data in
/// `IndexOptions`; this private context carries the replaceable runtime seam.
struct IndexOperation<'a, F: FileSystem> {
    fs: &'a F,
    rust_extractor:
        fn(&Path, &str, crate::rust_lang::Edition) -> Result<crate::rust_lang::RustExtraction>,
    #[cfg(test)]
    fail_after_canonical_replacement: bool,
}

impl<'a, F: FileSystem> IndexOperation<'a, F> {
    const fn new(fs: &'a F) -> Self {
        Self {
            fs,
            rust_extractor: crate::rust_lang::extract,
            #[cfg(test)]
            fail_after_canonical_replacement: false,
        }
    }

    #[cfg(test)]
    const fn failing_after_canonical_replacement(fs: &'a F) -> Self {
        Self {
            fs,
            rust_extractor: crate::rust_lang::extract,
            fail_after_canonical_replacement: true,
        }
    }

    #[cfg(test)]
    const fn failing_rust_extraction(fs: &'a F) -> Self {
        Self {
            fs,
            rust_extractor: injected_rust_extraction_failure,
            fail_after_canonical_replacement: false,
        }
    }
}

#[cfg(test)]
fn injected_rust_extraction_failure(
    _path: &Path,
    _source: &str,
    _edition: crate::rust_lang::Edition,
) -> Result<crate::rust_lang::RustExtraction> {
    anyhow::bail!("injected Rust extraction invariant failure")
}

#[cfg(test)]
pub(crate) fn index_repo_with_post_replacement_failure(
    root: &Path,
    conn: &Connection,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        &IndexOptions::default(),
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::failing_after_canonical_replacement(&OsFileSystem),
    )
}

#[cfg(test)]
pub(crate) fn index_repo_with_rust_extraction_failure(
    root: &Path,
    conn: &Connection,
) -> Result<IndexOutcome> {
    index_repo_impl(
        root,
        conn,
        &IndexOptions::default(),
        IndexMode::Incremental,
        CheckerRetention::Drop,
        IndexOperation::failing_rust_extraction(&OsFileSystem),
    )
}

fn index_repo_impl<F: FileSystem>(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    mode: IndexMode,
    checker_retention: CheckerRetention,
    operation: IndexOperation<'_, F>,
) -> Result<IndexOutcome> {
    const MAX_PROVENANCE_ATTEMPTS: usize = 3;
    for attempt in 1..=MAX_PROVENANCE_ATTEMPTS {
        let attempt_operation = IndexOperation {
            fs: operation.fs,
            rust_extractor: operation.rust_extractor,
            #[cfg(test)]
            fail_after_canonical_replacement: operation.fail_after_canonical_replacement,
        };
        match index_repo_attempt(
            root,
            conn,
            options,
            mode,
            checker_retention,
            attempt_operation,
        ) {
            Err(error)
                if error
                    .downcast_ref::<DocumentationProvenanceDrift>()
                    .is_some()
                    && attempt < MAX_PROVENANCE_ATTEMPTS =>
            {
                if options.debug {
                    eprintln!(
                        "documentation provenance changed during indexing; retrying immutable capture ({}/{})",
                        attempt + 1,
                        MAX_PROVENANCE_ATTEMPTS
                    );
                }
            }
            result => return result,
        }
    }
    unreachable!("the bounded documentation provenance retry loop always returns")
}

#[derive(Debug)]
struct DocumentationProvenanceDrift(String);

impl std::fmt::Display for DocumentationProvenanceDrift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DocumentationProvenanceDrift {}

fn index_repo_attempt<F: FileSystem>(
    root: &Path,
    conn: &Connection,
    options: &IndexOptions,
    mode: IndexMode,
    checker_retention: CheckerRetention,
    operation: IndexOperation<'_, F>,
) -> Result<IndexOutcome> {
    let root = root.canonicalize()?;
    // Git state is recorded before the immutable documentation capture only
    // when freshness is opted in. Every blame and final drift check then
    // belongs to this one attempted snapshot.
    let provenance_repository = options
        .docs_freshness
        .then(|| RepositoryCapture::capture(&root));
    let inventory_started = std::time::Instant::now();
    let inventory = corpus::repository_inventory(
        &root,
        &CorpusOptions {
            include: options.docs_include.clone(),
            exclude: options.docs_exclude.clone(),
            ..CorpusOptions::default()
        },
    )?;
    if options.timing {
        // Markdown parsing is part of the shared repository inventory pass.
        eprintln!(
            "timing repository-inventory={:?}",
            inventory_started.elapsed()
        );
    }
    let rust_editions = crate::rust_lang::resolve_editions(
        &root,
        &inventory.files,
        &inventory.cargo_manifests,
        operation.fs,
    )?;
    let documentation_provenance = match provenance_repository.as_ref() {
        Some(repository) => {
            provenance_store::resolve_document_provenance(conn, repository, &inventory.documents)?
        }
        None => provenance_store::disabled_document_provenance(&inventory.documents),
    };
    let workspace_discovery =
        crate::workspace::WorkspaceMap::discover_with_fs(&root, &inventory.files, operation.fs)?;
    let workspace = workspace_discovery.map;
    let mut outcome = IndexOutcome {
        indexed: 0,
        unchanged: 0,
        removed: 0,
        rejected: 0,
        rejections: Vec::new(),
        diagnostics: Vec::new(),
        chunks: 0,
        refs: 0,
        dependency_packages: 0,
        dependency_files: 0,
        dependency_bytes: 0,
        dependency_skipped: 0,
        dependency_skipped_bytes: 0,
        dependency_plans: Vec::new(),
        rust_files_with_parse_errors: 0,
        rust_parse_error_count: 0,
        checker_rebound: false,
        projection_rebuilt: true,
        extraction_reset: false,
    };
    for rejection in inventory.rejections {
        outcome.record_rejection(
            display_repository_path(&root, &rejection.path),
            rejection.stage,
            rejection.error,
        );
    }
    for rejection in &rust_editions.rejections {
        outcome.record_rejection(
            display_repository_path(&root, &rejection.path),
            "rust-edition",
            &rejection.error,
        );
    }
    outcome.diagnostics.extend(
        documentation_provenance
            .diagnostics
            .iter()
            .map(|diagnostic| IndexDiagnostic {
                path: diagnostic.path.clone(),
                stage: "documentation-provenance",
                detail: format!("{}: {}", diagnostic.operation, diagnostic.detail),
            }),
    );
    for rejection in workspace_discovery.rejections {
        outcome.record_rejection(
            display_repository_path(&root, &rejection.path),
            rejection.stage,
            rejection.error,
        );
    }

    // The complete canonical replacement, dependency synchronization, cached
    // vector materialization, module resolution, structural projection, and
    // marker publication form one SQLite commit. WAL readers continue to see
    // the last-good committed snapshot while this writer is active, and any
    // failure below rolls the replacement back as a unit.
    let documentation_provenance_by_path = documentation_provenance
        .documents
        .iter()
        .map(|provenance| (provenance.path.as_str(), provenance))
        .collect::<HashMap<_, _>>();
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let preparation = (|| -> Result<(_, _, _, _, _, _)> {
        let previous = ProjectionIdentity::read(conn)?;
        let previous_checker_identity = checker_retention
            .validates_active()
            .then(|| checker_canonical_identity(conn))
            .transpose()?;
        let changed_formats = ensure_format_contracts(conn)?;
        let documentation_provenance_format_changed = ensure_documentation_provenance_format(conn)?;
        let previous_rust_edition_contexts = ensure_rust_edition_context(
            conn,
            rust_editions.has_rust_files().then(|| {
                (
                    rust_editions.contexts_json(),
                    rust_editions.fingerprint.as_str(),
                )
            }),
        )?;
        let checker_contract_changed = formats::eligible_ids(Capability::Checker)
            .into_iter()
            .any(|format| changed_formats.contains(format));
        let code_contract_changed = formats::ALL.iter().any(|format| {
            format.corpus == formats::Corpus::Code && changed_formats.contains(format.id)
        });
        let stored: HashMap<String, (i64, String, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT path, id, hash, role, corpus, format
                 FROM files WHERE origin!='dependency'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ),
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let previous_paths = stored
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut existing = if mode == IndexMode::FullRefresh {
            HashMap::new()
        } else {
            stored
        };

        // Incremental contract invalidation is selective per format. A full
        // refresh remains the only operation that truncates the whole
        // disposable snapshot before rebuilding it.
        if mode == IndexMode::FullRefresh {
            store::reset_snapshot_state(conn)?;
            existing.clear();
            outcome.extraction_reset = true;
        }

        replace_documentation_inventory(conn, &inventory.decisions)?;
        let mut seen = std::collections::HashSet::new();
        let mut published = std::collections::HashSet::new();
        for file in &inventory.files {
            let rel = display_repository_path(&root, file);
            let format = formats::repository_code_for_path(file).ok_or_else(|| {
                anyhow::anyhow!(
                    "repository inventory admitted unsupported code file `{}`",
                    file.display()
                )
            })?;
            let source = match operation.fs.read_to_string(file) {
                Ok(source) => {
                    seen.insert(rel.clone());
                    source
                }
                Err(error) if io_policy::is_inventory_race(&error) => continue,
                Err(error) if io_policy::is_retryable(&error) => {
                    return Err(retryable_read_failure(&rel, error));
                }
                Err(error) => {
                    seen.insert(rel.clone());
                    if let Some((old_id, _, _, _, _)) = existing.get(&rel) {
                        store::delete_file(conn, *old_id)?;
                    }
                    outcome.record_rejection(rel, "read", error);
                    continue;
                }
            };
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let role = file_role::classify(Path::new(&rel), &source);
            if let Some((id, old_hash, old_role, old_corpus, old_format)) = existing.get(&rel)
                && *old_hash == hash
                && old_corpus == CODE_CORPUS
                && old_format == format.id
                && (format.id != formats::RUST
                    || previous_rust_edition_contexts
                        .get(&rel.replace('\\', "/"))
                        .map(String::as_str)
                        == rust_editions.context_for_relative(&rel))
            {
                if old_role != role {
                    conn.execute("UPDATE files SET role=?1 WHERE id=?2", params![role, id])?;
                }
                outcome.unchanged += 1;
                published.insert(rel);
                continue;
            }
            if options.debug {
                eprintln!("extracting {rel}");
            }
            match extract_file(
                file,
                &rel,
                &source,
                format,
                rust_editions.edition_for(file),
                operation.rust_extractor,
            ) {
                Ok(data) => {
                    if let Some((old_id, _, _, _, _)) = existing.get(&rel) {
                        store::delete_file(conn, *old_id)?;
                    }
                    let identity = FileIdentity {
                        path: &rel,
                        hash: &hash,
                        corpus: CODE_CORPUS,
                        format: format.id,
                        role,
                        origin: "repository",
                        package_instance_id: None,
                        package_path: None,
                    };
                    let (chunks, refs) = insert_file(conn, &identity, &data)?;
                    outcome.indexed += 1;
                    outcome.chunks += chunks;
                    outcome.refs += refs;
                    published.insert(rel);
                }
                Err(error) => {
                    if let Some((old_id, _, _, _, _)) = existing.get(&rel) {
                        store::delete_file(conn, *old_id)?;
                    }
                    outcome.record_rejection(rel, "extract", error);
                }
            }
        }
        let documentation_projection_started = std::time::Instant::now();
        for document in &inventory.documents {
            let rel = document.file.path.clone();
            let provenance = documentation_provenance_by_path
                .get(rel.as_str())
                .copied()
                .with_context(|| format!("missing resolved documentation provenance for {rel}"))?;
            seen.insert(rel.clone());
            let hash = document.file.content_hash.as_str();
            let format = documentation_format(Path::new(&rel))?;
            let role = "documentation";
            if !changed_formats.contains(format.id)
                && let Some((id, old_hash, old_role, old_corpus, old_format)) = existing.get(&rel)
                && old_hash == hash
                && old_corpus == DOCS_CORPUS
                && old_format == format.id
            {
                if old_role != role {
                    conn.execute("UPDATE files SET role=?1 WHERE id=?2", params![role, id])?;
                }
                if !documentation_provenance_format_changed
                    && stored_documentation_provenance_hash(conn, *id)?.as_deref()
                        == Some(provenance.projection_hash.as_str())
                {
                    outcome.unchanged += 1;
                } else {
                    update_documentation_provenance(conn, *id, provenance)?;
                    outcome.indexed += 1;
                }
                published.insert(rel);
                continue;
            }
            if options.debug {
                eprintln!("extracting {}", document.file.path);
            }
            if let Some((old_id, _, _, _, _)) = existing.get(&document.file.path) {
                store::delete_file(conn, *old_id)?;
            }
            let identity = FileIdentity {
                path: &document.file.path,
                hash,
                corpus: DOCS_CORPUS,
                format: format.id,
                role,
                origin: "repository",
                package_instance_id: None,
                package_path: None,
            };
            let chunks = insert_documentation_file(conn, &identity, document, provenance)?;
            outcome.indexed += 1;
            outcome.chunks += chunks;
            published.insert(rel);
        }
        if options.docs_freshness {
            provenance_store::upsert_blame_cache(conn, &documentation_provenance.cache_updates)?;
            provenance_store::prune_blame_cache(
                conn,
                &documentation_provenance.retained_cache_keys,
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                crate::docs::PROVENANCE_ENABLED_META_KEY,
                if options.docs_freshness {
                    "true"
                } else {
                    "false"
                }
            ],
        )?;
        if options.timing {
            eprintln!(
                "timing documentation-projection={:?}",
                documentation_projection_started.elapsed()
            );
        }
        for (path, (id, _, _, _, _)) in &existing {
            if !seen.contains(path) {
                store::delete_file(conn, *id)?;
            }
        }
        outcome.removed = previous_paths.difference(&published).count();

        // Dependency discovery sees the just-extracted, uncommitted importer
        // rows. Reading and parsing the selected corpus inside the outer
        // transaction ensures a transient dependency failure restores the
        // previous canonical rows and publication markers together.
        let discovered =
            dependency::discover(&root, conn, &options.dependencies, &workspace, operation.fs)?;
        let plans =
            dependency::plan_packages(&discovered, options.dependency_limits, operation.fs)?;
        let prepared = prepare_dependency_files(&plans, &mut outcome, operation.fs)?;

        conn.execute(
            "DELETE FROM meta
             WHERE key IN (
               'snapshot', 'code_digest', 'documentation_digest',
               'projection_version', 'resolution_hash',
               'documentation_provenance_digest'
             )",
            [],
        )?;
        Ok((
            previous,
            plans,
            prepared,
            previous_checker_identity,
            checker_contract_changed,
            code_contract_changed,
        ))
    })();
    let (
        previous,
        plans,
        prepared,
        previous_checker_identity,
        checker_contract_changed,
        code_contract_changed,
    ) = match preparation {
        Ok(preparation) => preparation,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    };
    let publication = (|| -> Result<()> {
        let instances = dependency::synchronize_instances(&root, conn, &workspace, &plans)?;
        index_dependency_files(conn, &prepared, &instances, &mut outcome)?;

        #[cfg(test)]
        if operation.fail_after_canonical_replacement {
            anyhow::bail!("injected failure after canonical replacement");
        }

        if outcome.indexed > 0 {
            crate::embed::materialize_cached_embeddings(conn)?;
        }

        resolve_module_edges(&root, conn, &workspace)?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('root', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [root.to_string_lossy()],
        )?;
        let resolution = crate::structural::compute_resolution_hash(conn)?;
        publish_projection_identity(conn, &resolution)?;
        let provenance_digest = provenance_store::compute_documentation_provenance_digest(conn)?;
        let identities =
            crate::publication::Identities::compute(conn, &resolution, &provenance_digest)?;
        if let Some(repository) = provenance_repository.as_ref() {
            validate_documentation_provenance_publication(
                repository,
                &inventory.documents,
                // Keep this early drift check cheap. Conversion identities are
                // recomputed once, at the final pre-COMMIT validation below.
                &[],
            )?;
        }
        // Manual indexing retains only a checker publication proven reusable;
        // watch may additionally keep failed predecessors as hidden carry for
        // the following enrichment step. Projection still accepts only the
        // exact current code digest.
        let checker_publication_changed = match checker_retention {
            #[cfg(test)]
            CheckerRetention::Drop => store::clear_checker_batches(conn)?,
            CheckerRetention::PreserveActiveForWatch => {
                let _ = store::preserve_checker_carry_source_for_watch(conn)?;
                false
            }
            CheckerRetention::ValidateActive {
                retain_failed_for_watch,
            } => {
                let current_checker_identity = checker_canonical_identity(conn)?;
                let can_rebind = !checker_contract_changed
                    && previous_checker_identity.as_deref()
                        == Some(current_checker_identity.as_str())
                    && previous.resolution_hash.as_deref() == Some(resolution.as_str())
                    && match previous.code_digest.as_deref() {
                        Some(old_code_digest) => {
                            crate::checker::active_batch_inputs_fresh(&root, conn, old_code_digest)?
                        }
                        None => false,
                    };
                let rebound = if can_rebind {
                    match previous.code_digest.as_deref() {
                        Some(old_code_digest) => store::rebind_active_checker_batch(
                            conn,
                            old_code_digest,
                            &identities.code,
                        )?,
                        None => false,
                    }
                } else {
                    false
                };
                outcome.checker_rebound = rebound;
                let already_bound =
                    previous.code_digest.as_deref() == Some(identities.code.as_str());
                let retained = can_rebind && (already_bound || rebound);
                let active_changed = if retained {
                    if retain_failed_for_watch {
                        let _ = store::preserve_checker_carry_source_for_watch(conn)?;
                    } else {
                        let _ = store::discard_inactive_checker_batches(conn)?;
                    }
                    false
                } else if retain_failed_for_watch {
                    let deactivated = already_bound
                        && store::deactivate_active_checker_batch_for_snapshot(
                            conn,
                            &identities.code,
                        )?;
                    let _ = store::preserve_checker_carry_source_for_watch(conn)?;
                    deactivated
                } else {
                    store::clear_checker_batches(conn)?
                };
                active_changed || rebound
            }
        };
        let projection_started = std::time::Instant::now();
        if !outcome.extraction_reset
            && !code_contract_changed
            && previous.code_digest.as_deref() == Some(identities.code.as_str())
            && !checker_publication_changed
        {
            // The projection is a pure function of the canonical tables: the
            // code digest covers every extracted code row, the projection
            // contract, and module resolution. Checker publication rebuilds
            // the projection immediately and its batch is accepted only for
            // this exact code digest.
            outcome.projection_rebuilt = false;
            if options.timing {
                eprintln!("timing structural-projection=skipped (unchanged)");
            }
        } else {
            crate::structural::rebuild_projection_with_timing(
                conn,
                &identities.code,
                options.timing,
            )?;
            if options.timing {
                eprintln!(
                    "timing structural-projection={:?}",
                    projection_started.elapsed()
                );
            }
        }
        identities.publish(conn)?;
        // Documentation readiness is exact-snapshot state. Rebuild it from the
        // durable shared cache after the new marker exists, but before the outer
        // publication commit. An incomplete cache remains a normal NotReady
        // state and never turns indexing into a provider operation.
        if outcome.extraction_reset
            || crate::docs::retrieval::cached_generation_rematerialization_needed(
                conn,
                &identities.documentation,
            )?
        {
            crate::docs::retrieval::rematerialize_cached_generations(
                conn,
                &identities.documentation,
            )?;
        }
        let (rust_files_with_errors, rust_error_count): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(parse_error_count), 0)
             FROM files
             WHERE format=?1 AND parse_error_count>0",
            [formats::RUST],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        outcome.rust_files_with_parse_errors = usize::try_from(rust_files_with_errors)
            .map_err(|_| anyhow::anyhow!("Rust parse-error file count exceeded this platform"))?;
        outcome.rust_parse_error_count = usize::try_from(rust_error_count)
            .map_err(|_| anyhow::anyhow!("Rust parse-error count exceeded this platform"))?;
        Ok(())
    })();
    match publication {
        Ok(()) => {
            // This is intentionally the last fallible operation before the
            // SQLite commit. A checkout or clone-deepening change during
            // projection must not publish provenance from the earlier state.
            if let Some(repository) = provenance_repository.as_ref()
                && let Err(error) = validate_documentation_provenance_publication(
                    repository,
                    &inventory.documents,
                    &documentation_provenance.publication_checks,
                )
            {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error.into());
            }
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    crate::recon::reconcile_file_policy_after_index(&root, conn);
    Ok(outcome)
}

fn validate_documentation_provenance_publication(
    repository: &RepositoryCapture,
    documents: &[CapturedDocument],
    conversion_keys: &[BlameCacheKey],
) -> Result<()> {
    let RepositoryCapture::Git(repository) = repository else {
        return Ok(());
    };
    let conversion_checks = if conversion_keys.is_empty() {
        Vec::new()
    } else {
        let documents_by_path = documents
            .iter()
            .map(|document| (document.file.path.as_str(), document.bytes.as_slice()))
            .collect::<HashMap<_, _>>();
        conversion_keys
            .iter()
            .map(|key| {
                documents_by_path
                    .get(key.path.as_str())
                    .copied()
                    .map(|bytes| (key, bytes))
                    .with_context(|| {
                        format!(
                            "missing captured documentation bytes for conversion check {}",
                            key.path
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    match repository.validate_before_publication(conversion_checks) {
        Ok(PublicationValidation::Stable) => Ok(()),
        Ok(PublicationValidation::Drift(drift)) => {
            Err(anyhow::Error::new(DocumentationProvenanceDrift(format!(
                "Git documentation provenance drifted before publication: HEAD {} -> {}, index membership {} -> {}, shallow {} -> {}",
                drift.recorded_head,
                drift.current_head,
                drift.recorded_index_fingerprint,
                drift.current_index_fingerprint,
                drift.recorded_shallow_fingerprint,
                drift.current_shallow_fingerprint,
            ))))
        }
        Ok(PublicationValidation::ConversionDrift(drift)) => {
            Err(anyhow::Error::new(DocumentationProvenanceDrift(format!(
                "Git documentation conversion drifted before publication for {}: {} -> {}",
                drift.path, drift.recorded_oid, drift.current_oid,
            ))))
        }
        Err(error) => Err(anyhow::Error::new(DocumentationProvenanceDrift(format!(
            "could not revalidate Git documentation provenance before publication: {error:#}"
        )))),
    }
}

fn display_repository_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn checker_canonical_identity(conn: &Connection) -> Result<String> {
    let eligible = formats::eligible_ids_json(Capability::Checker);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-canonical-identity-v1\0");
    let mut statement = conn.prepare(
        "SELECT file.path, file.hash, file.role, file.origin, file.format,
                COALESCE(file.package_path, ''),
                COALESCE(package.origin, ''), COALESCE(package.name, ''),
                COALESCE(package.version, ''), COALESCE(package.canonical_root, ''),
                COALESCE(package.locator, ''), COALESCE(package.manifest_hash, ''),
                COALESCE(package.status, '')
         FROM files file
         LEFT JOIN package_instances package ON package.id=file.package_instance_id
         WHERE file.origin IN ('repository','workspace')
           AND file.format IN (SELECT value FROM json_each(?1))
         ORDER BY file.path",
    )?;
    let rows = statement.query_map([eligible], |row| {
        Ok([
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
        ])
    })?;
    for row in rows {
        for value in row? {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Prior code-plane inputs retained across canonical replacement. The code
/// digest alone gates projection reuse; the separately stored resolution hash
/// is needed only by the checker-safe rebind optimization.
struct ProjectionIdentity {
    code_digest: Option<String>,
    resolution_hash: Option<String>,
}

impl ProjectionIdentity {
    fn read(conn: &Connection) -> Result<Self> {
        let read = |key: &str| -> Result<Option<String>> {
            Ok(conn
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()?)
        };
        Ok(Self {
            code_digest: read(crate::publication::CODE_DIGEST_META_KEY)?,
            resolution_hash: read("resolution_hash")?,
        })
    }
}

fn publish_projection_identity(conn: &Connection, resolution_hash: &str) -> Result<()> {
    for (key, value) in [
        ("projection_version", crate::structural::PROJECTION_VERSION),
        ("resolution_hash", resolution_hash),
    ] {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
    }
    Ok(())
}

fn ensure_format_contracts(conn: &Connection) -> Result<std::collections::HashSet<&'static str>> {
    let mut changed = std::collections::HashSet::new();
    let mut code_changed = false;
    let legacy_code = conn
        .query_row(
            "SELECT value FROM meta WHERE key='extraction_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let legacy_documentation = conn
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [DOC_CHUNK_FORMAT_META_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    for format in formats::ALL {
        let key = formats::contract_meta_key(format);
        let current = conn
            .query_row("SELECT value FROM meta WHERE key=?1", [&key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        if current.as_deref() == Some(format.extractor_version) {
            continue;
        }
        let has_rows = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE format=?1)",
            [format.id],
            |row| row.get::<_, bool>(0),
        )?;
        if !has_rows {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, format.extractor_version],
            )?;
            continue;
        }
        let covered_by_legacy = match format.snapshot_contract {
            formats::SnapshotContractPolicy::LegacyCode => {
                legacy_code.as_deref() == Some(format.extractor_version)
            }
            formats::SnapshotContractPolicy::LegacyDocumentation => {
                legacy_documentation.as_deref() == Some(format.extractor_version)
            }
            formats::SnapshotContractPolicy::PerFormatWhenPresent => false,
        };
        let bootstrap_only = current.is_none() && (!has_rows || covered_by_legacy);
        if bootstrap_only {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)",
                params![key, format.extractor_version],
            )?;
            continue;
        }
        changed.insert(format.id);
        if format.corpus == formats::Corpus::Code {
            code_changed = true;
            conn.execute(
                "UPDATE files SET hash='' WHERE corpus='code' AND format=?1",
                [format.id],
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, format.extractor_version],
        )?;
    }
    if code_changed {
        conn.execute("DELETE FROM resolved_edges", [])?;
        conn.execute("DELETE FROM graph_nodes", [])?;
        conn.execute(
            "DELETE FROM meta
             WHERE key IN ('snapshot', 'code_digest', 'projection_version')",
            [],
        )?;
    }
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('extraction_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [crate::entity::EXTRACTION_VERSION],
    )?;
    // Keep the existing docs marker as the public compatibility diagnostic;
    // the per-format keys above are the selective invalidation authority.
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![DOC_CHUNK_FORMAT_META_KEY, crate::docs::CHUNK_FORMAT_VERSION],
    )?;
    Ok(changed)
}

fn ensure_rust_edition_context(
    conn: &Connection,
    current: Option<(String, &str)>,
) -> Result<HashMap<String, String>> {
    let previous = conn
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [crate::rust_lang::EDITION_CONTEXTS_META_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let previous = previous
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    match current {
        Some((contexts, fingerprint)) => {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![crate::rust_lang::EDITION_CONTEXT_META_KEY, fingerprint],
            )?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![crate::rust_lang::EDITION_CONTEXTS_META_KEY, contexts],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM meta WHERE key IN (?1, ?2)",
                params![
                    crate::rust_lang::EDITION_CONTEXT_META_KEY,
                    crate::rust_lang::EDITION_CONTEXTS_META_KEY
                ],
            )?;
        }
    }
    Ok(previous)
}

fn ensure_documentation_provenance_format(conn: &Connection) -> Result<bool> {
    let current = conn
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [DOC_PROVENANCE_FORMAT_META_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if current.as_deref() == Some(crate::docs::PROVENANCE_FORMAT_VERSION) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![
            DOC_PROVENANCE_FORMAT_META_KEY,
            crate::docs::PROVENANCE_FORMAT_VERSION
        ],
    )?;
    Ok(true)
}

fn stored_documentation_provenance_hash(conn: &Connection, file_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT projection_hash FROM doc_file_provenance WHERE file_id=?1",
        [file_id],
        |row| row.get(0),
    )
    .optional()
    .context("read stored documentation provenance projection hash")
}

fn extract_file(
    abs: &Path,
    rel: &str,
    source: &str,
    format: &formats::FormatSpec,
    rust_edition: crate::rust_lang::Edition,
    rust_extractor: fn(
        &Path,
        &str,
        crate::rust_lang::Edition,
    ) -> Result<crate::rust_lang::RustExtraction>,
) -> Result<FileData> {
    match format.extractor {
        Extractor::EcmaScript => parse::with_parsed(source, abs, |ret, semantic| {
            let chunker = Chunker::new(Path::new(rel), source, ret);
            let chunks = chunker.chunk_program(&ret.program, &ret.program.comments);
            let graph = graph::extract(ret, semantic);
            FileData {
                chunks,
                graph,
                lines: LineIndex::new(source),
                parse_error_count: 0,
            }
        }),
        Extractor::RustText => {
            let extraction = rust_extractor(Path::new(rel), source, rust_edition)?;
            Ok(FileData {
                chunks: extraction.chunks,
                graph: FileGraph::default(),
                lines: LineIndex::new(source),
                parse_error_count: extraction.parse_error_count,
            })
        }
        Extractor::Documentation => anyhow::bail!(
            "documentation format `{}` cannot enter the code extractor",
            format.id
        ),
    }
}

fn documentation_format(path: &Path) -> Result<&'static formats::FormatSpec> {
    formats::documentation_for_path(path).ok_or_else(|| {
        anyhow::anyhow!(
            "indexed documentation file `{}` has an unsupported format",
            path.display()
        )
    })
}

fn fts_content(content: &str) -> Cow<'_, str> {
    if content.contains('\0') {
        // FTS5 indexes text after an embedded NUL, but highlight() can omit
        // the bytes between that NUL and a later match. A space keeps the NUL
        // as a token boundary without changing any subsequent line offsets.
        Cow::Owned(content.replace('\0', " "))
    } else {
        Cow::Borrowed(content)
    }
}

fn replace_documentation_inventory(conn: &Connection, decisions: &[Decision]) -> Result<()> {
    conn.execute("DELETE FROM doc_inventory", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO doc_inventory(
           path, subject, rule, detail, path_base64, path_encoding
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for decision in decisions {
        insert.execute(params![
            decision.path,
            decision.subject,
            decision.rule,
            decision.detail,
            decision.path_base64,
            decision.path_encoding,
        ])?;
    }
    Ok(())
}

fn insert_documentation_file(
    conn: &Connection,
    identity: &FileIdentity<'_>,
    captured: &CapturedDocument,
    provenance: &ResolvedDocumentProvenance,
) -> Result<usize> {
    let file = &captured.file;
    let format = formats::by_id(identity.format)
        .ok_or_else(|| anyhow::anyhow!("unknown documentation format `{}`", identity.format))?;
    ensure!(
        identity.path == file.path,
        "documentation file identity path does not match captured document"
    );
    ensure!(
        provenance.path == file.path && provenance.chunks.len() == file.chunks.len(),
        "documentation provenance does not match captured document {}",
        file.path
    );
    ensure!(
        identity.corpus == DOCS_CORPUS && format.documentation(),
        "documentation file identity has an invalid corpus or format"
    );
    ensure!(
        captured.bytes.len() as u64 == file.byte_len,
        "captured documentation byte length does not match parser metadata for {}",
        file.path
    );
    ensure!(
        blake3::hash(&captured.bytes).to_hex().as_str() == file.content_hash,
        "captured documentation hash does not match parser metadata for {}",
        file.path
    );

    conn.execute(
        "INSERT INTO files(
           path, hash, corpus, format, role, origin,
           package_instance_id, package_path
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            identity.path,
            identity.hash,
            identity.corpus,
            identity.format,
            identity.role,
            identity.origin,
            identity.package_instance_id,
            identity.package_path,
        ],
    )?;
    let file_id = conn.last_insert_rowid();
    provenance_store::upsert_file_provenance(conn, file_id, provenance)?;
    let metadata = documentation_metadata(file);
    let tags_json = serde_json::to_string(&file.tags)
        .with_context(|| format!("serialize Markdown tags for {}", file.path))?;
    let title_fts = fts_content(&file.title);
    let metadata_fts = fts_content(&metadata);
    let path_fts = fts_content(identity.path);

    let mut insert_chunk = conn.prepare_cached(
        "INSERT INTO chunks(
           file_id, kind, name, scope_chain, symbols, start, end,
           start_line, end_line, hash, content
         ) VALUES(?1, ?2, NULL, '', '', ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut insert_meta = conn.prepare_cached(
        "INSERT INTO doc_chunk_meta(
           chunk_id, title, description, tags_json, breadcrumb,
           nearest_heading, ordinal, embedding_identity, front_matter_state,
           freshness_basis, freshness_author_time, freshness_committer_time,
           freshness_detail
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    let mut insert_fts = format
        .lexical_eligible()
        .then(|| {
            conn.prepare_cached(
                "INSERT INTO docs_fts(rowid, title, metadata, breadcrumb, body, path)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            )
        })
        .transpose()?;

    for (position, chunk) in file.chunks.iter().enumerate() {
        let chunk_provenance = &provenance.chunks[position];
        ensure!(
            chunk.ordinal == position as u64,
            "Markdown chunk ordinals are not contiguous for {}",
            file.path
        );
        ensure!(
            chunk_provenance.chunk_ordinal == chunk.ordinal,
            "documentation provenance ordinals are not aligned for {}",
            file.path
        );
        let start = usize::try_from(chunk.source_start)
            .with_context(|| format!("invalid Markdown source start for {}", file.path))?;
        let end = usize::try_from(chunk.source_end)
            .with_context(|| format!("invalid Markdown source end for {}", file.path))?;
        ensure!(
            start <= end && end <= captured.bytes.len(),
            "Markdown chunk {}:{} has an invalid source span",
            file.path,
            chunk.ordinal
        );
        let source = std::str::from_utf8(&captured.bytes[start..end]).with_context(|| {
            format!(
                "Markdown chunk {}:{} source slice is not UTF-8",
                file.path, chunk.ordinal
            )
        })?;
        let start = u32::try_from(start).context("Markdown source start exceeds chunk schema")?;
        let end = u32::try_from(end).context("Markdown source end exceeds chunk schema")?;
        let start_line =
            u32::try_from(chunk.line_start).context("Markdown start line exceeds chunk schema")?;
        let end_line =
            u32::try_from(chunk.line_end).context("Markdown end line exceeds chunk schema")?;
        let same_heading_ordinal = i64::try_from(chunk.same_heading_ordinal)
            .context("Markdown same-heading ordinal exceeds SQLite integer range")?;
        let kind = if chunk.is_stub {
            "markdown_document"
        } else {
            "markdown_section"
        };
        ensure!(
            chunk.is_stub == chunk.embedding_identity.is_none(),
            "Markdown stub/embedding identity mismatch for {}:{}",
            file.path,
            chunk.ordinal
        );
        if !chunk.is_stub {
            let expected = crate::docs::corpus::embedding_identity(
                chunk.nearest_heading.as_deref(),
                &chunk.rendered_body,
            );
            ensure!(
                chunk.embedding_identity.as_deref() == Some(expected.as_str()),
                "Markdown embedding identity mismatch for {}:{}",
                file.path,
                chunk.ordinal
            );
        }
        let chunk_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        insert_chunk.execute(params![
            file_id, kind, start, end, start_line, end_line, chunk_hash, source,
        ])?;
        let chunk_id = conn.last_insert_rowid();
        insert_meta.execute(params![
            chunk_id,
            file.title,
            file.description,
            tags_json,
            chunk.breadcrumb,
            chunk.nearest_heading,
            same_heading_ordinal,
            chunk.embedding_identity,
            file.front_matter_state,
            git_chunk_basis_name(chunk_provenance.basis),
            chunk_provenance.author_time,
            chunk_provenance.committer_time,
            provenance.detail,
        ])?;
        let breadcrumb_fts = fts_content(&chunk.breadcrumb);
        if let Some(insert_fts) = insert_fts.as_mut() {
            insert_fts.execute(params![
                chunk_id,
                title_fts.as_ref(),
                metadata_fts.as_ref(),
                breadcrumb_fts.as_ref(),
                chunk.rendered_body,
                path_fts.as_ref(),
            ])?;
        }
    }
    Ok(file.chunks.len())
}

fn update_documentation_provenance(
    conn: &Connection,
    file_id: i64,
    provenance: &ResolvedDocumentProvenance,
) -> Result<()> {
    let chunk_rows = {
        let mut statement = conn.prepare(
            "SELECT chunk.id
             FROM chunks chunk
             JOIN doc_chunk_meta metadata ON metadata.chunk_id=chunk.id
             WHERE chunk.file_id=?1
             ORDER BY chunk.start, chunk.end, chunk.id",
        )?;
        let rows = statement.query_map([file_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    ensure!(
        chunk_rows.len() == provenance.chunks.len(),
        "stored documentation chunks do not match refreshed provenance for {}",
        provenance.path
    );

    let mut update = conn.prepare_cached(
        "UPDATE doc_chunk_meta
         SET freshness_basis=?1, freshness_author_time=?2,
             freshness_committer_time=?3, freshness_detail=?4
         WHERE chunk_id=?5",
    )?;
    for (position, (chunk_id, chunk_provenance)) in
        chunk_rows.into_iter().zip(&provenance.chunks).enumerate()
    {
        ensure!(
            position as u64 == chunk_provenance.chunk_ordinal,
            "stored documentation ordinal does not match refreshed provenance for {}",
            provenance.path
        );
        update.execute(params![
            git_chunk_basis_name(chunk_provenance.basis),
            chunk_provenance.author_time,
            chunk_provenance.committer_time,
            provenance.detail,
            chunk_id,
        ])?;
    }
    provenance_store::upsert_file_provenance(conn, file_id, provenance)
}

fn documentation_metadata(file: &DocFile) -> String {
    let mut parts = Vec::new();
    if let Some(description) = file
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        parts.push(description);
    }
    parts.extend(file.tags.iter().map(String::as_str));
    parts.join(" ")
}

const fn git_chunk_basis_name(basis: GitChunkBasis) -> &'static str {
    match basis {
        GitChunkBasis::Git => "git",
        GitChunkBasis::WorkingTree => "working_tree",
        GitChunkBasis::Unknown => "unknown",
    }
}

fn insert_file(
    conn: &Connection,
    identity: &FileIdentity<'_>,
    data: &FileData,
) -> Result<(usize, usize)> {
    let format = formats::by_id(identity.format)
        .ok_or_else(|| anyhow::anyhow!("unknown code format `{}`", identity.format))?;
    ensure!(
        identity.corpus == CODE_CORPUS && format.corpus == formats::Corpus::Code,
        "code file identity has an invalid corpus"
    );
    let parse_error_count = i64::try_from(data.parse_error_count)
        .map_err(|_| anyhow::anyhow!("parse-error count exceeded SQLite integer range"))?;
    conn.execute(
        "INSERT INTO files(
           path, hash, corpus, format, role, origin,
           package_instance_id, package_path, parse_error_count
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            identity.path,
            identity.hash,
            identity.corpus,
            identity.format,
            identity.role,
            identity.origin,
            identity.package_instance_id,
            identity.package_path,
            parse_error_count,
        ],
    )?;
    let file_id = conn.last_insert_rowid();

    // Chunk spans for assigning refs to chunks (sorted by construction).
    let mut chunk_ids: Vec<(u32, u32, i64)> = Vec::with_capacity(data.chunks.len());
    {
        let mut ins_chunk = conn.prepare_cached(
            "INSERT INTO chunks(file_id, kind, name, scope_chain, symbols, start, end, start_line, end_line, hash, content)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut ins_fts = format.lexical_eligible().then(|| {
            conn.prepare_cached(
                "INSERT INTO chunks_fts(rowid, content, name, symbols, path) VALUES(?1, ?2, ?3, ?4, ?5)",
            )
        }).transpose()?;
        for c in &data.chunks {
            let kind = serde_json::to_value(c.kind)?
                .as_str()
                .unwrap_or("module")
                .to_string();
            ins_chunk.execute(params![
                file_id,
                kind,
                c.name,
                c.scope_chain.join("."),
                c.symbols.join(" "),
                c.start,
                c.end,
                c.start_line,
                c.end_line,
                c.hash,
                c.content,
            ])?;
            let chunk_id = conn.last_insert_rowid();
            chunk_ids.push((c.start, c.end, chunk_id));
            if let Some(ins_fts) = ins_fts.as_mut() {
                let searchable_content = fts_content(&c.content);
                ins_fts.execute(params![
                    chunk_id,
                    searchable_content.as_ref(),
                    c.name.as_deref().unwrap_or(""),
                    c.symbols.join(" "),
                    identity.path,
                ])?;
            }
        }
    }
    let chunk_for = |offset: u32| -> Option<i64> {
        chunk_ids
            .iter()
            .find(|(s, e, _)| *s <= offset && offset < *e)
            .map(|(_, _, id)| *id)
    };

    let mut ins_sym = conn.prepare_cached(
        "INSERT INTO symbols(
           file_id, name, kind, start, end, decl_start, decl_end, scope_chain, line, exported
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for s in &data.graph.symbols {
        ins_sym.execute(params![
            file_id,
            s.name,
            s.kind,
            s.start,
            s.end,
            s.decl_start,
            s.decl_end,
            s.scope_chain,
            data.lines.line(s.decl_start),
            s.exported as i64,
        ])?;
    }

    let mut ins_imp = conn.prepare_cached(
        "INSERT INTO imports(file_id, local_name, imported_name, request) VALUES(?1,?2,?3,?4)",
    )?;
    for i in &data.graph.imports {
        ins_imp.execute(params![file_id, i.local, i.imported, i.request])?;
    }

    let mut ins_exp = conn.prepare_cached(
        "INSERT INTO exports(file_id, export_name, local_name, from_request, from_name) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for e in &data.graph.exports {
        ins_exp.execute(params![
            file_id,
            e.export_name,
            e.local_name,
            e.from_request,
            e.from_name
        ])?;
    }

    let mut ins_contract_imp = conn.prepare_cached(
        "INSERT INTO contract_imports(file_id, local_name, imported_name, request)
         VALUES(?1,?2,?3,?4)",
    )?;
    for import in &data.graph.contract_imports {
        ins_contract_imp.execute(params![
            file_id,
            import.local,
            import.imported,
            import.request,
        ])?;
    }

    let mut ins_contract_exp = conn.prepare_cached(
        "INSERT INTO contract_exports(
           file_id, export_name, local_name, from_request, from_name
         ) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for export in &data.graph.contract_exports {
        ins_contract_exp.execute(params![
            file_id,
            export.export_name,
            export.local_name,
            export.from_request,
            export.from_name,
        ])?;
    }

    let mut ins_event = conn.prepare_cached(
        "INSERT INTO events(file_id, chunk_id, line, role, name, method) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for e in &data.graph.events {
        ins_event.execute(params![
            file_id,
            chunk_for(e.span_start),
            data.lines.line(e.span_start),
            e.role,
            e.name,
            e.method,
        ])?;
    }

    let mut ins_mc = conn.prepare_cached(
        "INSERT INTO member_calls(
           file_id, chunk_id, start, end, line, end_line, prop, object, receiver,
           receiver_start, receiver_end, property_start, property_end,
           receiver_unbound
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
    )?;
    for m in &data.graph.member_calls {
        ins_mc.execute(params![
            file_id,
            chunk_for(m.span_start),
            m.span_start,
            m.span_end,
            data.lines.line(m.span_start),
            data.lines.line(m.span_end.saturating_sub(1)),
            m.prop,
            m.object,
            m.receiver,
            m.receiver_start,
            m.receiver_end,
            m.property_start,
            m.property_end,
            m.receiver_unbound,
        ])?;
    }

    let mut ins_receiver_flow = conn.prepare_cached(
        "INSERT INTO receiver_value_flows(
           file_id, call_start, call_end, receiver_kind, class_name, class_start,
           value_kind, target_kind, target_name, target_start
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for flow in &data.graph.receiver_flows {
        ins_receiver_flow.execute(params![
            file_id,
            flow.call_start,
            flow.call_end,
            flow.kind,
            flow.class_name,
            flow.class_start,
            flow.value.as_ref().map(|value| value.kind),
            flow.value.as_ref().map(|value| value.target.kind),
            flow.value.as_ref().map(|value| &value.target.name),
            flow.value.as_ref().map(|value| value.target.start),
        ])?;
    }

    let mut ins_function_flow = conn.prepare_cached(
        "INSERT INTO function_return_flows(
           file_id, function_name, function_start, function_async, return_index,
           value_kind, target_kind, target_name, target_start
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    for flow in &data.graph.function_flows {
        for (return_index, value) in flow.returns.iter().enumerate() {
            ins_function_flow.execute(params![
                file_id,
                flow.name,
                flow.start,
                flow.is_async,
                return_index as i64,
                value.kind,
                value.target.kind,
                value.target.name,
                value.target.start,
            ])?;
        }
    }

    let mut ins_binding_flow = conn.prepare_cached(
        "INSERT INTO value_binding_flows(
           file_id, binding_name, binding_start,
           value_kind, target_kind, target_name, target_start
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for flow in &data.graph.binding_flows {
        ins_binding_flow.execute(params![
            file_id,
            flow.name,
            flow.start,
            flow.value.kind,
            flow.value.target.kind,
            flow.value.target.name,
            flow.value.target.start,
        ])?;
    }

    let mut ins_class_flow = conn.prepare_cached(
        "INSERT INTO class_value_flows(
           file_id, class_name, class_start, super_name, super_start, super_kind
         ) VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    let mut ins_instance_method_flow = conn.prepare_cached(
        "INSERT INTO instance_method_value_flows(
           file_id, class_start, method_name, method_start
         ) VALUES(?1,?2,?3,?4)",
    )?;
    let mut ins_class_member_blocker = conn.prepare_cached(
        "INSERT INTO class_member_value_flow_blockers(
           file_id, class_start, member_name
         ) VALUES(?1,?2,?3)",
    )?;
    for flow in &data.graph.class_flows {
        ins_class_flow.execute(params![
            file_id,
            flow.name,
            flow.start,
            flow.super_class.as_ref().map(|class| &class.name),
            flow.super_class.as_ref().map(|class| class.start),
            flow.super_class.as_ref().map(|class| class.kind),
        ])?;
        for method in &flow.instance_methods {
            ins_instance_method_flow.execute(params![
                file_id,
                flow.start,
                method.name,
                method.start,
            ])?;
        }
        for member in &flow.blocked_instance_members {
            ins_class_member_blocker.execute(params![file_id, flow.start, member])?;
        }
    }

    let mut ins_entity_site = conn.prepare_cached(
        "INSERT INTO entity_sites(
           file_id, chunk_id, start, end, line, end_line, plane, entity_type,
           role, identity_kind, identity_name, identity_start, target_name,
           target_start, extractor, provenance, confidence, detail_json
         ) VALUES(
           ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18
         )",
    )?;
    for site in &data.graph.entity_sites {
        ins_entity_site.execute(params![
            file_id,
            chunk_for(site.span_start),
            site.span_start,
            site.span_end,
            data.lines.line(site.span_start),
            data.lines.line(site.span_end.saturating_sub(1)),
            site.plane,
            site.entity_type,
            site.role,
            site.identity_kind,
            site.identity_name,
            site.identity_start,
            site.target_name,
            site.target_start,
            site.extractor,
            site.provenance,
            site.confidence,
            site.detail.to_string(),
        ])?;
    }

    let mut ins_ref = conn.prepare_cached(
        "INSERT INTO refs(
           file_id, chunk_id, start, line, kind, confidence,
           target_request, target_name, local, detail
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for r in &data.graph.refs {
        ins_ref.execute(params![
            file_id,
            chunk_for(r.span_start),
            r.span_start,
            data.lines.line(r.span_start),
            r.kind,
            r.confidence,
            r.target_request,
            r.target_name,
            r.local as i64,
            r.detail,
        ])?;
    }
    Ok((data.chunks.len(), data.graph.refs.len()))
}

fn prepare_dependency_files(
    plans: &[dependency::PackagePlan],
    outcome: &mut IndexOutcome,
    fs: &impl FileSystem,
) -> Result<Vec<PreparedDependencyFile>> {
    let mut prepared = Vec::new();
    for plan in plans {
        outcome.dependency_packages += 1;
        outcome.dependency_skipped += plan.skipped_files;
        outcome.dependency_skipped_bytes += plan.skipped_bytes;
        outcome.dependency_plans.push(format!(
            "{}@{}: {} ({})",
            plan.package.name,
            plan.package.version.as_deref().unwrap_or("unknown"),
            plan.source_basis,
            plan.status,
        ));
        for file in &plan.files {
            let display = dependency_display_path(&plan.package, &file.package_path);
            let source = match fs.read_to_string(&file.source_path) {
                Ok(source) => source,
                Err(error) if io_policy::is_inventory_race(&error) => continue,
                Err(error) if io_policy::is_retryable(&error) => {
                    let path = format!("{display} ({})", file.source_path.display());
                    return Err(retryable_read_failure(&path, error));
                }
                Err(error) => {
                    outcome.record_rejection(
                        display,
                        "read",
                        format!("{}: {error}", file.source_path.display()),
                    );
                    continue;
                }
            };
            if dependency::should_skip_minified(&file.source_path, &source, file.forced_entry) {
                outcome.dependency_skipped += 1;
                continue;
            }
            outcome.dependency_bytes += file.bytes;
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let role = file_role::classify(Path::new(&file.package_path), &source);
            prepared.push(PreparedDependencyFile {
                package_root: plan.package.canonical_root.clone(),
                display,
                source_path: file.source_path.clone(),
                package_path: file.package_path.clone(),
                source,
                hash,
                role,
            });
        }
    }
    Ok(prepared)
}

fn index_dependency_files(
    conn: &Connection,
    prepared: &[PreparedDependencyFile],
    instances: &std::collections::BTreeMap<PathBuf, i64>,
    outcome: &mut IndexOutcome,
) -> Result<()> {
    conn.execute_batch("SAVEPOINT jscout_dependency_files")?;
    let result = (|| -> Result<()> {
        let existing: HashMap<String, StoredDependencyFile> = {
            let mut stmt = conn.prepare(
                "SELECT path, id, hash, role, corpus, format,
                        package_instance_id, package_path
                 FROM files WHERE origin='dependency'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    StoredDependencyFile {
                        id: row.get(1)?,
                        hash: row.get(2)?,
                        role: row.get(3)?,
                        corpus: row.get(4)?,
                        format: row.get(5)?,
                        package_instance_id: row.get(6)?,
                        package_path: row.get(7)?,
                    },
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut seen = std::collections::HashSet::new();
        for file in prepared {
            let package_id = *instances.get(&file.package_root).ok_or_else(|| {
                anyhow::anyhow!("dependency package instance was not synchronized")
            })?;
            seen.insert(file.display.clone());
            let format = formats::dependency_code_for_path(&file.source_path).ok_or_else(|| {
                anyhow::anyhow!(
                    "dependency plan admitted unsupported code file `{}`",
                    file.source_path.display()
                )
            })?;
            if let Some(old) = existing.get(&file.display)
                && old.hash == file.hash
                && old.corpus == CODE_CORPUS
                && old.format == format.id
                && old.package_instance_id == package_id
                && old.package_path == file.package_path
            {
                if old.role != file.role {
                    conn.execute(
                        "UPDATE files SET role=?1 WHERE id=?2",
                        params![file.role, old.id],
                    )?;
                }
                outcome.unchanged += 1;
                outcome.dependency_files += 1;
                continue;
            }
            if let Some(old) = existing.get(&file.display) {
                store::delete_file(conn, old.id)?;
            }
            match extract_file(
                &file.source_path,
                &file.display,
                &file.source,
                format,
                crate::rust_lang::Edition::DEFAULT,
                crate::rust_lang::extract,
            ) {
                Ok(data) => {
                    let identity = FileIdentity {
                        path: &file.display,
                        hash: &file.hash,
                        corpus: CODE_CORPUS,
                        format: format.id,
                        role: file.role,
                        origin: "dependency",
                        package_instance_id: Some(package_id),
                        package_path: Some(&file.package_path),
                    };
                    let (chunks, refs) = insert_file(conn, &identity, &data)?;
                    outcome.indexed += 1;
                    outcome.dependency_files += 1;
                    outcome.chunks += chunks;
                    outcome.refs += refs;
                }
                Err(error) => outcome.record_rejection(file.display.clone(), "extract", error),
            }
        }
        for (path, old) in &existing {
            if !seen.contains(path) {
                store::delete_file(conn, old.id)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE jscout_dependency_files")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO jscout_dependency_files; RELEASE jscout_dependency_files",
            );
            Err(error)
        }
    }
}

fn dependency_display_path(package: &dependency::DiscoveredPackage, package_path: &str) -> String {
    let version = package.version.as_deref().unwrap_or("unknown");
    let locator = blake3::hash(package.canonical_root.to_string_lossy().as_bytes()).to_hex();
    format!(
        "dependency:{}@{}#{}/{package_path}",
        package.name,
        version,
        &locator[..8]
    )
}

/// Resolve every (file, request) pair to an in-repo file or external package.
pub fn resolve_module_edges(
    root: &Path,
    conn: &Connection,
    workspace: &crate::workspace::WorkspaceMap,
) -> Result<()> {
    let resolver = Resolver::new(resolver_options(
        workspace.aliases.clone(),
        Some(TsconfigDiscovery::Auto),
    ));
    // A tsconfig that fails to load fails every resolution under it — e.g.
    // `extends: "@scope/tsconfig-pkg/..."` in a monorepo without node_modules
    // installed. Retry such failures without tsconfig discovery so a broken
    // tsconfig degrades resolution instead of dropping the whole file's edges.
    let no_tsconfig = Resolver::new(resolver_options(workspace.aliases.clone(), None));
    // Third-party source must follow the package installation graph. Applying
    // workspace aliases inside it can redirect a dependency's own imports to
    // an unrelated first-party package with the same name.
    let dependency_resolver = Resolver::new(resolver_options(Vec::new(), None));
    let resolver_formats = formats::eligible_ids_json(Capability::Resolver);
    conn.execute_batch("SAVEPOINT jscout_module_edges")?;
    let result = (|| -> Result<()> {
        let (file_ids, importer_paths) = {
            let mut stmt = conn.prepare(
                "SELECT f.id, f.path, f.origin, f.package_path, f.package_instance_id,
                        p.canonical_root
                 FROM code_files f
                 LEFT JOIN package_instances p ON p.id=f.package_instance_id
                 WHERE f.format IN (SELECT value FROM json_each(?1))",
            )?;
            let rows = stmt.query_map([&resolver_formats], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?;
            let mut by_path = HashMap::new();
            let mut by_id = HashMap::new();
            for row in rows {
                let (id, path, origin, package_path, package_instance, package_root) = row?;
                let physical = if origin == "dependency" {
                    PathBuf::from(package_root.ok_or_else(|| {
                        anyhow::anyhow!("dependency file {path} has no package root")
                    })?)
                    .join(package_path.ok_or_else(|| {
                        anyhow::anyhow!("dependency file {path} has no package path")
                    })?)
                } else {
                    root.join(path)
                };
                let physical = physical.canonicalize().unwrap_or(physical);
                by_path.insert(physical.clone(), (id, package_instance));
                by_id.insert(id, (physical, origin == "dependency"));
            }
            (by_path, by_id)
        };

        let mut package_roots: Vec<(PathBuf, i64)> = {
            let mut stmt = conn.prepare(
                "SELECT canonical_root, id FROM package_instances WHERE origin='dependency'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, i64>(1)?,
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        package_roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));

        let pairs: Vec<(i64, String, bool)> = {
            let mut stmt = conn.prepare(
                "SELECT f.id, requests.request, max(requests.is_runtime) = 0
                 FROM code_files f
                 JOIN (
                   SELECT file_id, request, 1 AS is_runtime FROM imports
                   UNION ALL
                   SELECT file_id, from_request, 1 FROM exports WHERE from_request IS NOT NULL
                   UNION ALL
                   SELECT file_id, target_request, 1 FROM refs WHERE target_request IS NOT NULL
                   UNION ALL
                   SELECT file_id, request, 0 FROM contract_imports
                   UNION ALL
                   SELECT file_id, from_request, 0 FROM contract_exports
                     WHERE from_request IS NOT NULL
                 ) requests ON requests.file_id = f.id
                 WHERE f.format IN (SELECT value FROM json_each(?1))
                 GROUP BY f.id, requests.request",
            )?;
            let rows = stmt.query_map([&resolver_formats], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        conn.execute("DELETE FROM module_edges", [])?;
        let mut ins = conn.prepare_cached(
            "INSERT INTO module_edges(
               from_file, request, to_file, package, resolution, package_instance_id, type_only
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;
        type Resolved = (
            Option<i64>,
            Option<String>,
            Option<&'static str>,
            Option<i64>,
        );
        let mut cache: HashMap<(PathBuf, String), Resolved> = HashMap::new();
        for (file_id, request, type_only) in pairs {
            let (importer, dependency_importer) = importer_paths
                .get(&file_id)
                .ok_or_else(|| anyhow::anyhow!("indexed importer {file_id} has no physical path"))?
                .clone();
            let key = (importer.clone(), request.clone());
            let (to_file, package, resolution, package_instance) = cache
                .entry(key)
                .or_insert_with(|| {
                    match if dependency_importer {
                        dependency_resolver.resolve_file(&importer, &request)
                    } else {
                        resolver
                            .resolve_file(&importer, &request)
                            .or_else(|_| no_tsconfig.resolve_file(&importer, &request))
                    } {
                        Ok(resolution) => {
                            let path = resolution.path().to_path_buf();
                            let path = path.canonicalize().unwrap_or(path);
                            match file_ids.get(&path) {
                                Some((id, package_instance)) => (
                                    Some(*id),
                                    None,
                                    Some(workspace.classify(&request)),
                                    *package_instance,
                                ),
                                None if external_package_name(&request).is_none() => {
                                    // Resolved to a real but un-indexable file
                                    // (styles, assets, JSON): keep the edge as
                                    // evidence without inventing a package.
                                    (None, None, Some("unresolved"), None)
                                }
                                None => {
                                    let package = external_package_name(&request)
                                        .expect("guarded external package request");
                                    let package_instance = package_roots
                                        .iter()
                                        .find(|(root, _)| path.starts_with(root))
                                        .map(|(_, id)| *id);
                                    (None, Some(package), None, package_instance)
                                }
                            }
                        }
                        Err(_) if external_package_name(&request).is_none() => {
                            (None, None, Some("unresolved"), None)
                        }
                        Err(_) => (None, external_package_name(&request), None, None),
                    }
                })
                .clone();
            ins.execute(params![
                file_id,
                request,
                to_file,
                package,
                resolution,
                package_instance,
                type_only,
            ])?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE jscout_module_edges")?;
            Ok(())
        }
        Err(error) => {
            let _ =
                conn.execute_batch("ROLLBACK TO jscout_module_edges; RELEASE jscout_module_edges");
            Err(error)
        }
    }
}

/// Return the package boundary for a syntactically valid bare package
/// specifier. Relative/absolute paths, package-import aliases (`#name`),
/// bundler aliases (`~/`, `@/`), URLs, and Windows paths are unresolved
/// evidence rather than invented `pkg:` identities.
fn external_package_name(request: &str) -> Option<String> {
    if let Some((scheme, _)) = request.split_once(':') {
        return matches!(scheme, "node" | "bun").then(|| package_name(request));
    }
    if request.is_empty()
        || request.starts_with(['.', '/', '~', '#'])
        || request.contains(['\\', '%', '?'])
    {
        return None;
    }
    let mut parts = request.split('/');
    let first = parts.next()?;
    if let Some(scope) = first.strip_prefix('@') {
        let name = parts.next()?;
        if scope.is_empty() || !valid_package_segment(scope) || !valid_package_segment(name) {
            return None;
        }
        Some(format!("@{scope}/{name}"))
    } else {
        valid_package_segment(first).then(|| first.to_string())
    }
}

fn valid_package_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '\\' | '%' | ':' | '#' | '?' | '@')
        })
}

/// "@scope/pkg/sub/path" -> "@scope/pkg"; "./x" stays as-is (unresolved relative).
pub(crate) fn package_name(request: &str) -> String {
    if request.starts_with('.') || request.starts_with('/') {
        return request.to_string();
    }
    let mut parts = request.splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(scope), Some(name)) if scope.starts_with('@') => format!("{scope}/{name}"),
        (Some(name), _) => name.to_string(),
        _ => request.to_string(),
    }
}

#[cfg(test)]
mod tests;
