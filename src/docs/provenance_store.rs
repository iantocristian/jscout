use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::corpus::CapturedDocument;
use super::provenance::{
    BlameCacheKey, BlameMapping, ChunkGitProvenance, DocumentBlame, DocumentPreparation,
    GitChunkBasis, LineProvenance, ProvenanceDiagnostic, RepositoryCapture, validate_blame_input,
};

const PROJECTION_DOMAIN: &[u8] = b"jscout-doc-provenance-projection-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentProvenanceStatus {
    Disabled,
    Resolved,
    GitUnavailable,
    UntrackedOrNew,
    PrepareFailed,
    BlameFailed,
}

impl DocumentProvenanceStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Resolved => "resolved",
            Self::GitUnavailable => "git_unavailable",
            Self::UntrackedOrNew => "untracked_or_new",
            Self::PrepareFailed => "prepare_failed",
            Self::BlameFailed => "blame_failed",
        }
    }
}

/// Produce deterministic provenance rows without consulting Git or the blame
/// cache. This keeps the canonical documentation schema complete while the
/// opt-in freshness projection is disabled.
pub(crate) fn disabled_document_provenance(documents: &[CapturedDocument]) -> ProvenanceResolution {
    ProvenanceResolution {
        documents: documents
            .iter()
            .map(|document| unknown_document(document, DocumentProvenanceStatus::Disabled, None))
            .collect(),
        diagnostics: Vec::new(),
        cache_updates: Vec::new(),
        publication_checks: Vec::new(),
        retained_cache_keys: BTreeMap::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDocumentProvenance {
    pub path: String,
    pub chunks: Vec<ChunkGitProvenance>,
    pub projection_hash: String,
    pub status: DocumentProvenanceStatus,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ProvenanceResolution {
    pub documents: Vec<ResolvedDocumentProvenance>,
    pub diagnostics: Vec<ProvenanceDiagnostic>,
    /// Fresh mappings to upsert in the publication transaction. Cache hits are
    /// deliberately absent.
    pub cache_updates: Vec<BlameMapping>,
    /// Every tracked request prepared from this immutable capture, including
    /// requests that later degraded to unknown. Publication revalidates their
    /// conversion identity against the same captured bytes.
    pub publication_checks: Vec<BlameCacheKey>,
    /// Exact current keys whose mappings resolved successfully. Every other
    /// cache row, including a stale row for an active but failed path, is
    /// removed in the publication transaction.
    pub retained_cache_keys: BTreeMap<(String, String), BlameCacheKey>,
}

/// Resolve current documentation provenance without mutating SQLite. The
/// returned cache changes are applied only inside the same transaction that
/// publishes the source projection.
pub(crate) fn resolve_document_provenance(
    conn: &Connection,
    repository: &RepositoryCapture,
    documents: &[CapturedDocument],
) -> Result<ProvenanceResolution> {
    let mut resolution = ProvenanceResolution {
        documents: Vec::with_capacity(documents.len()),
        diagnostics: Vec::new(),
        cache_updates: Vec::new(),
        publication_checks: Vec::new(),
        retained_cache_keys: BTreeMap::new(),
    };

    match repository {
        RepositoryCapture::Unknown(diagnostic) => {
            if !documents.is_empty() {
                resolution.diagnostics.push(diagnostic.clone());
            }
            for document in documents {
                resolution.documents.push(unknown_document(
                    document,
                    DocumentProvenanceStatus::GitUnavailable,
                    Some(diagnostic.detail.clone()),
                ));
            }
        }
        RepositoryCapture::Git(repository) => {
            for document in documents {
                resolve_git_document(conn, repository, document, &mut resolution)?;
            }
        }
    }
    Ok(resolution)
}

fn resolve_git_document(
    conn: &Connection,
    repository: &super::provenance::GitRepository,
    document: &CapturedDocument,
    resolution: &mut ProvenanceResolution,
) -> Result<()> {
    let path = &document.file.path;
    // Refuse provenance work for logical-line bombs before path history,
    // conversion filters, or the SQLite cache are consulted. Source indexing
    // remains successful; only this file's optional blame projection degrades.
    if let Err(error) = validate_blame_input(&document.bytes) {
        let diagnostic = ProvenanceDiagnostic {
            path: Some(path.clone()),
            operation: "blame captured documentation bytes".to_owned(),
            detail: format!("{error:#}"),
        };
        resolution.diagnostics.push(diagnostic.clone());
        resolution.documents.push(unknown_document(
            document,
            DocumentProvenanceStatus::BlameFailed,
            Some(diagnostic.detail),
        ));
        return Ok(());
    }
    let request = match repository.prepare_document(path, &document.bytes) {
        DocumentPreparation::Unknown => {
            resolution.documents.push(unknown_document(
                document,
                DocumentProvenanceStatus::UntrackedOrNew,
                Some("path is absent from the recorded HEAD".to_owned()),
            ));
            return Ok(());
        }
        DocumentPreparation::Failed(diagnostic) => {
            resolution.diagnostics.push(diagnostic.clone());
            resolution.documents.push(unknown_document(
                document,
                DocumentProvenanceStatus::PrepareFailed,
                Some(diagnostic.detail),
            ));
            return Ok(());
        }
        DocumentPreparation::Tracked(request) => request,
    };
    resolution
        .publication_checks
        .push(request.cache_key.clone());

    let cached = load_blame_mapping(conn, &request.cache_key, &document.bytes)?;
    let (mapping, successful_status, successful_detail, cache_update) = match cached {
        CacheLookup::Hit(mapping) => (mapping, DocumentProvenanceStatus::Resolved, None, false),
        CacheLookup::Miss => match repository.blame_document(&request, &document.bytes) {
            DocumentBlame::Attributed(mapping) => {
                (mapping, DocumentProvenanceStatus::Resolved, None, true)
            }
            DocumentBlame::Unknown(diagnostic) => {
                resolution.diagnostics.push(diagnostic.clone());
                resolution.documents.push(unknown_document(
                    document,
                    DocumentProvenanceStatus::BlameFailed,
                    Some(diagnostic.detail),
                ));
                return Ok(());
            }
        },
        CacheLookup::Invalid(diagnostic) => {
            resolution.diagnostics.push(diagnostic);
            match repository.blame_document(&request, &document.bytes) {
                DocumentBlame::Attributed(mapping) => {
                    (mapping, DocumentProvenanceStatus::Resolved, None, true)
                }
                DocumentBlame::Unknown(blame_diagnostic) => {
                    resolution.diagnostics.push(blame_diagnostic.clone());
                    resolution.documents.push(unknown_document(
                        document,
                        DocumentProvenanceStatus::BlameFailed,
                        Some(blame_diagnostic.detail),
                    ));
                    return Ok(());
                }
            }
        }
    };

    match aggregate_document(document, &mapping, successful_status, successful_detail) {
        Ok(document) => {
            resolution.retained_cache_keys.insert(
                (
                    mapping.cache_key.path_scope.clone(),
                    mapping.cache_key.path.clone(),
                ),
                mapping.cache_key.clone(),
            );
            if cache_update {
                resolution.cache_updates.push(mapping);
            }
            resolution.documents.push(document);
        }
        Err(error) => {
            let diagnostic = ProvenanceDiagnostic {
                path: Some(path.clone()),
                operation: "aggregate documentation provenance".to_owned(),
                detail: format!("{error:#}"),
            };
            resolution.diagnostics.push(diagnostic.clone());
            resolution.documents.push(unknown_document(
                document,
                DocumentProvenanceStatus::BlameFailed,
                Some(diagnostic.detail),
            ));
        }
    }
    Ok(())
}

fn aggregate_document(
    document: &CapturedDocument,
    mapping: &BlameMapping,
    status: DocumentProvenanceStatus,
    detail: Option<String>,
) -> Result<ResolvedDocumentProvenance> {
    let mut chunks = Vec::with_capacity(document.file.chunks.len());
    for chunk in &document.file.chunks {
        let ranges = chunk
            .contributing_lines
            .iter()
            .map(|range| (range.start, range.end));
        chunks.push(
            mapping
                .aggregate_chunk_ranges(chunk.ordinal, ranges)
                .with_context(|| {
                    format!(
                        "aggregate chunk {}:{} from blamed body lines",
                        document.file.path, chunk.ordinal
                    )
                })?,
        );
    }
    Ok(resolved_document(
        &document.file.path,
        chunks,
        status,
        detail,
    ))
}

fn unknown_document(
    document: &CapturedDocument,
    status: DocumentProvenanceStatus,
    detail: Option<String>,
) -> ResolvedDocumentProvenance {
    let chunks = document
        .file
        .chunks
        .iter()
        .map(|chunk| ChunkGitProvenance {
            chunk_ordinal: chunk.ordinal,
            basis: GitChunkBasis::Unknown,
            author_time: None,
            committer_time: None,
        })
        .collect();
    resolved_document(&document.file.path, chunks, status, detail)
}

fn resolved_document(
    path: &str,
    chunks: Vec<ChunkGitProvenance>,
    status: DocumentProvenanceStatus,
    detail: Option<String>,
) -> ResolvedDocumentProvenance {
    let projection_hash = projection_hash(status, &chunks);
    ResolvedDocumentProvenance {
        path: path.to_owned(),
        chunks,
        projection_hash,
        status,
        detail,
    }
}

/// Hash only stable, snapshot-visible semantic state. Diagnostic text and
/// whether a mapping came from cache are intentionally excluded.
fn projection_hash(status: DocumentProvenanceStatus, chunks: &[ChunkGitProvenance]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROJECTION_DOMAIN);
    hash_field(&mut hasher, super::PROVENANCE_FORMAT_VERSION.as_bytes());
    hash_field(&mut hasher, status.as_str().as_bytes());
    hasher.update(&(chunks.len() as u64).to_le_bytes());
    for chunk in chunks {
        hasher.update(&chunk.chunk_ordinal.to_le_bytes());
        hash_field(
            &mut hasher,
            match chunk.basis {
                GitChunkBasis::Git => b"git",
                GitChunkBasis::WorkingTree => b"working_tree",
                GitChunkBasis::Unknown => b"unknown",
            },
        );
        hash_optional_i64(&mut hasher, chunk.author_time);
        hash_optional_i64(&mut hasher, chunk.committer_time);
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_optional_i64(hasher: &mut blake3::Hasher, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

enum CacheLookup {
    Hit(BlameMapping),
    Miss,
    Invalid(ProvenanceDiagnostic),
}

fn load_blame_mapping(conn: &Connection, key: &BlameCacheKey, bytes: &[u8]) -> Result<CacheLookup> {
    let attribution_json = conn
        .query_row(
            "SELECT attribution_json
             FROM doc_blame_cache
             WHERE path_scope=?1 AND path=?2 AND bytes_hash=?3 AND path_tip=?4
               AND converted_blob_oid=?5 AND shallow_fingerprint=?6
               AND format_version=?7",
            params![
                key.path_scope,
                key.path,
                key.bytes_hash,
                key.path_tip,
                key.converted_blob_oid,
                key.shallow_fingerprint,
                super::PROVENANCE_FORMAT_VERSION,
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("load exact documentation blame cache entry")?;
    let Some(attribution_json) = attribution_json else {
        return Ok(CacheLookup::Miss);
    };
    let lines = match serde_json::from_str::<Vec<LineProvenance>>(&attribution_json) {
        Ok(lines) => lines,
        Err(error) => {
            return Ok(CacheLookup::Invalid(cache_diagnostic(
                &key.path,
                format!("cached line attribution is invalid JSON: {error}"),
            )));
        }
    };
    let expected_lines = git_logical_line_count(bytes);
    if lines.len() != expected_lines {
        return Ok(CacheLookup::Invalid(cache_diagnostic(
            &key.path,
            format!(
                "cached line attribution has {} lines; captured bytes have {expected_lines}",
                lines.len()
            ),
        )));
    }
    Ok(CacheLookup::Hit(BlameMapping {
        cache_key: key.clone(),
        lines,
    }))
}

fn cache_diagnostic(path: &str, detail: String) -> ProvenanceDiagnostic {
    ProvenanceDiagnostic {
        path: Some(path.to_owned()),
        operation: "load documentation blame cache".to_owned(),
        detail,
    }
}

fn git_logical_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes.split(|byte| *byte == b'\n').count() - usize::from(bytes.last() == Some(&b'\n'))
}

/// Upsert fresh misses in the caller's publication transaction.
pub(crate) fn upsert_blame_cache(conn: &Connection, mappings: &[BlameMapping]) -> Result<usize> {
    let mut statement = conn.prepare_cached(
        "INSERT INTO doc_blame_cache(
           path_scope, path, bytes_hash, converted_blob_oid, path_tip,
           shallow_fingerprint, attribution_json, format_version
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(path_scope,path) DO UPDATE SET
           bytes_hash=excluded.bytes_hash,
           converted_blob_oid=excluded.converted_blob_oid,
           path_tip=excluded.path_tip,
           shallow_fingerprint=excluded.shallow_fingerprint,
           attribution_json=excluded.attribution_json,
           format_version=excluded.format_version",
    )?;
    let mut changed = 0_usize;
    for mapping in mappings {
        let attribution_json = serde_json::to_string(&mapping.lines)
            .with_context(|| format!("serialize blame cache for {}", mapping.cache_key.path))?;
        changed += statement.execute(params![
            mapping.cache_key.path_scope,
            mapping.cache_key.path,
            mapping.cache_key.bytes_hash,
            mapping.cache_key.converted_blob_oid,
            mapping.cache_key.path_tip,
            mapping.cache_key.shallow_fingerprint,
            attribution_json,
            super::PROVENANCE_FORMAT_VERSION,
        ])?;
    }
    Ok(changed)
}

/// Remove mappings for paths outside the current admitted corpus. This keeps
/// the rebuildable cache from becoming document history.
pub(crate) fn prune_blame_cache(
    conn: &Connection,
    retained_cache_keys: &BTreeMap<(String, String), BlameCacheKey>,
) -> Result<usize> {
    let cached_rows = {
        let mut statement = conn.prepare(
            "SELECT path_scope,path,bytes_hash,converted_blob_oid,path_tip,
                    shallow_fingerprint,format_version
             FROM doc_blame_cache ORDER BY path_scope,path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut statement =
        conn.prepare_cached("DELETE FROM doc_blame_cache WHERE path_scope=?1 AND path=?2")?;
    let mut removed = 0_usize;
    for (
        path_scope,
        path,
        bytes_hash,
        converted_blob_oid,
        path_tip,
        shallow_fingerprint,
        format_version,
    ) in cached_rows
    {
        let retained = retained_cache_keys
            .get(&(path_scope.clone(), path.clone()))
            .is_some_and(|key| {
                key.bytes_hash == bytes_hash
                    && key.converted_blob_oid == converted_blob_oid
                    && key.path_tip == path_tip
                    && key.shallow_fingerprint == shallow_fingerprint
                    && format_version == super::PROVENANCE_FORMAT_VERSION
            });
        if !retained {
            removed += statement.execute(params![path_scope, path])?;
        }
    }
    Ok(removed)
}

/// Persist one document projection after the caller inserts or reuses its
/// `files` row. Provenance-only refreshes deliberately keep chunk row IDs so
/// an unchanged search snapshot cannot lose a ready vector generation.
pub(crate) fn upsert_file_provenance(
    conn: &Connection,
    file_id: i64,
    provenance: &ResolvedDocumentProvenance,
) -> Result<()> {
    conn.execute(
        "INSERT INTO doc_file_provenance(file_id,projection_hash,status,detail)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(file_id) DO UPDATE SET
           projection_hash=excluded.projection_hash,
           status=excluded.status,
           detail=excluded.detail",
        params![
            file_id,
            provenance.projection_hash,
            provenance.status.as_str(),
            provenance.detail,
        ],
    )
    .with_context(|| format!("insert documentation provenance for {}", provenance.path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use anyhow::{Result, bail};
    use rusqlite::Connection;

    use super::*;
    use crate::docs::corpus::{self, CorpusOptions};

    struct TestRepository {
        directory: tempfile::TempDir,
    }

    impl TestRepository {
        fn new() -> Result<Option<Self>> {
            if Command::new("git").arg("--version").output().is_err() {
                return Ok(None);
            }
            let repository = Self {
                directory: tempfile::tempdir()?,
            };
            repository.git(&["init", "--quiet"])?;
            repository.git(&["config", "user.name", "Documentation Test"])?;
            repository.git(&["config", "user.email", "docs@example.invalid"])?;
            Ok(Some(repository))
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn git(&self, args: &[&str]) -> Result<()> {
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
            Ok(())
        }

        fn commit(&self) -> Result<()> {
            self.commit_at(
                "documentation",
                "2001-01-01T00:00:00 +0000",
                "2002-01-01T00:00:00 +0000",
            )
        }

        fn commit_at(&self, message: &str, author_date: &str, committer_date: &str) -> Result<()> {
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
    }

    fn cache_connection() -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE doc_blame_cache(
               path_scope TEXT NOT NULL,
               path TEXT NOT NULL,
               bytes_hash TEXT NOT NULL,
               converted_blob_oid TEXT NOT NULL,
               path_tip TEXT NOT NULL,
               shallow_fingerprint TEXT NOT NULL,
               attribution_json TEXT NOT NULL,
               format_version TEXT NOT NULL,
               PRIMARY KEY(path_scope,path)
             );
             CREATE TABLE files(
               id INTEGER PRIMARY KEY,
               path TEXT UNIQUE NOT NULL,
               corpus TEXT NOT NULL
             );
             CREATE TABLE doc_file_provenance(
               file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
               projection_hash TEXT NOT NULL,
               status TEXT NOT NULL,
               detail TEXT
             );",
        )?;
        Ok(conn)
    }

    #[test]
    fn cache_round_trip_invalid_repair_prune_and_file_row() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        fs::write(
            repository.path().join("guide.md"),
            "# Guide\n\nCurrent instruction.\n",
        )?;
        repository.commit()?;

        // Repository state is deliberately captured before immutable corpus
        // collection, matching the production attempt order.
        let repository_capture = RepositoryCapture::capture(repository.path());
        let corpus = corpus::repository_inventory(repository.path(), &CorpusOptions::default())?;
        assert_eq!(corpus.documents.len(), 1);
        let conn = cache_connection()?;

        let first = resolve_document_provenance(&conn, &repository_capture, &corpus.documents)?;
        assert_eq!(
            first.documents[0].status,
            DocumentProvenanceStatus::Resolved
        );
        assert_eq!(first.cache_updates.len(), 1);
        assert!(
            first.documents[0]
                .chunks
                .iter()
                .any(|chunk| chunk.basis == GitChunkBasis::Git)
        );
        let resolved_hash = first.documents[0].projection_hash.clone();
        assert_eq!(upsert_blame_cache(&conn, &first.cache_updates)?, 1);

        let second = resolve_document_provenance(&conn, &repository_capture, &corpus.documents)?;
        assert!(second.cache_updates.is_empty());
        assert_eq!(second.documents[0].projection_hash, resolved_hash);

        conn.execute(
            "UPDATE doc_blame_cache SET attribution_json='{' WHERE path='guide.md'",
            [],
        )?;
        let invalid = resolve_document_provenance(&conn, &repository_capture, &corpus.documents)?;
        assert_eq!(
            invalid.documents[0].status,
            DocumentProvenanceStatus::Resolved
        );
        assert_eq!(invalid.documents[0].projection_hash, resolved_hash);
        assert_eq!(invalid.cache_updates.len(), 1);
        assert_eq!(invalid.diagnostics.len(), 1);
        upsert_blame_cache(&conn, &invalid.cache_updates)?;

        let mut stale = invalid.cache_updates[0].clone();
        stale.cache_key.path = "gone.md".to_owned();
        upsert_blame_cache(&conn, &[stale])?;
        assert_eq!(prune_blame_cache(&conn, &invalid.retained_cache_keys)?, 1);
        conn.execute(
            "UPDATE doc_blame_cache SET bytes_hash='stale' WHERE path='guide.md'",
            [],
        )?;
        assert_eq!(prune_blame_cache(&conn, &invalid.retained_cache_keys)?, 1);
        upsert_blame_cache(&conn, &invalid.cache_updates)?;
        conn.execute(
            "UPDATE doc_blame_cache SET converted_blob_oid='stale' WHERE path='guide.md'",
            [],
        )?;
        assert_eq!(prune_blame_cache(&conn, &invalid.retained_cache_keys)?, 1);

        conn.execute(
            "INSERT INTO files(id,path,corpus) VALUES(1,'guide.md','docs')",
            [],
        )?;
        upsert_file_provenance(&conn, 1, &invalid.documents[0])?;
        let stored = conn.query_row(
            "SELECT projection_hash,status,detail FROM doc_file_provenance WHERE file_id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        assert_eq!(stored.0, invalid.documents[0].projection_hash);
        assert_eq!(stored.1, "resolved");
        assert!(stored.2.is_none());
        Ok(())
    }

    #[test]
    fn conversion_change_misses_cache_when_raw_bytes_and_path_tip_are_unchanged() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        repository.git(&["config", "core.autocrlf", "true"])?;
        let bytes = b"# Guide\r\n\r\nCurrent instruction.\r\n";
        fs::write(repository.path().join("guide.md"), bytes)?;
        repository.commit()?;
        assert_eq!(fs::read(repository.path().join("guide.md"))?, bytes);

        let conn = cache_connection()?;
        let first_capture = RepositoryCapture::capture(repository.path());
        let first_corpus =
            corpus::repository_inventory(repository.path(), &CorpusOptions::default())?;
        let first = resolve_document_provenance(&conn, &first_capture, &first_corpus.documents)?;
        assert_eq!(
            first.documents[0].status,
            DocumentProvenanceStatus::Resolved
        );
        assert!(
            first.documents[0]
                .chunks
                .iter()
                .any(|chunk| chunk.basis == GitChunkBasis::Git)
        );
        assert_eq!(first.cache_updates.len(), 1);
        upsert_blame_cache(&conn, &first.cache_updates)?;

        fs::write(
            repository.path().join(".gitattributes"),
            b"guide.md -text\n",
        )?;
        repository.git(&["add", ".gitattributes"])?;
        repository.git(&["commit", "--quiet", "-m", "disable conversion"])?;
        assert_eq!(fs::read(repository.path().join("guide.md"))?, bytes);

        let second_capture = RepositoryCapture::capture(repository.path());
        let second_corpus =
            corpus::repository_inventory(repository.path(), &CorpusOptions::default())?;
        let second = resolve_document_provenance(&conn, &second_capture, &second_corpus.documents)?;
        assert_eq!(
            second.documents[0].status,
            DocumentProvenanceStatus::Resolved
        );
        assert!(
            second.documents[0]
                .chunks
                .iter()
                .any(|chunk| chunk.basis == GitChunkBasis::WorkingTree)
        );
        assert_eq!(
            second.cache_updates.len(),
            1,
            "conversion drift must miss cache"
        );
        assert_eq!(
            first.cache_updates[0].cache_key.bytes_hash,
            second.cache_updates[0].cache_key.bytes_hash
        );
        assert_eq!(
            first.cache_updates[0].cache_key.path_tip,
            second.cache_updates[0].cache_key.path_tip
        );
        assert_ne!(
            first.cache_updates[0].cache_key.converted_blob_oid,
            second.cache_updates[0].cache_key.converted_blob_oid
        );
        Ok(())
    }

    #[test]
    fn logical_line_limit_degrades_to_blame_failed_without_cache_retention() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let bytes = vec![b'\n'; super::super::provenance::MAX_BLAME_LOGICAL_LINES + 1];
        fs::write(repository.path().join("guide.md"), &bytes)?;
        repository.commit()?;

        let capture = RepositoryCapture::capture(repository.path());
        let corpus = corpus::repository_inventory(repository.path(), &CorpusOptions::default())?;
        assert_eq!(corpus.documents.len(), 1);
        let resolution =
            resolve_document_provenance(&cache_connection()?, &capture, &corpus.documents)?;
        assert_eq!(resolution.documents.len(), 1);
        assert_eq!(
            resolution.documents[0].status,
            DocumentProvenanceStatus::BlameFailed
        );
        assert_eq!(
            resolution.documents[0].chunks.len(),
            corpus.documents[0].file.chunks.len()
        );
        assert!(resolution.cache_updates.is_empty());
        assert!(resolution.retained_cache_keys.is_empty());
        assert!(resolution.publication_checks.is_empty());
        assert!(
            resolution.diagnostics[0]
                .detail
                .contains("65537 Git logical lines")
        );
        Ok(())
    }

    #[test]
    fn cache_scope_separates_nested_index_roots_with_the_same_document_path() -> Result<()> {
        let Some(repository) = TestRepository::new()? else {
            return Ok(());
        };
        let original = b"# Shared\n\nSame instruction.\n";
        let final_bytes = b"# Shared\n\nSame instruction.\n\n<!-- aligned -->\n";
        fs::create_dir_all(repository.path().join("a"))?;
        fs::create_dir_all(repository.path().join("b"))?;
        fs::write(repository.path().join("a/README.md"), original)?;
        repository.commit_at(
            "add a",
            "2001-01-01T00:00:00 +0000",
            "2001-01-01T00:00:00 +0000",
        )?;
        fs::write(repository.path().join("b/README.md"), original)?;
        repository.commit_at(
            "add b",
            "2010-01-01T00:00:00 +0000",
            "2010-01-01T00:00:00 +0000",
        )?;
        fs::write(repository.path().join("a/README.md"), final_bytes)?;
        fs::write(repository.path().join("b/README.md"), final_bytes)?;
        repository.commit_at(
            "align comments",
            "2020-01-01T00:00:00 +0000",
            "2020-01-01T00:00:00 +0000",
        )?;

        let conn = cache_connection()?;
        let resolve_root = |root: &Path| -> Result<ProvenanceResolution> {
            let capture = RepositoryCapture::capture(root);
            let corpus = corpus::repository_inventory(root, &CorpusOptions::default())?;
            resolve_document_provenance(&conn, &capture, &corpus.documents)
        };
        let first = resolve_root(&repository.path().join("a"))?;
        assert_eq!(first.cache_updates.len(), 1);
        assert_eq!(
            first.documents[0]
                .chunks
                .iter()
                .filter_map(|chunk| chunk.author_time)
                .max(),
            Some(978_307_200)
        );
        upsert_blame_cache(&conn, &first.cache_updates)?;

        let second = resolve_root(&repository.path().join("b"))?;
        assert_eq!(second.cache_updates.len(), 1);
        assert_ne!(
            first.cache_updates[0].cache_key.path_scope,
            second.cache_updates[0].cache_key.path_scope
        );
        assert_eq!(first.cache_updates[0].cache_key.path, "README.md");
        assert_eq!(second.cache_updates[0].cache_key.path, "README.md");
        assert_eq!(
            second.documents[0]
                .chunks
                .iter()
                .filter_map(|chunk| chunk.author_time)
                .max(),
            Some(1_262_304_000)
        );
        Ok(())
    }

    #[test]
    fn projection_hash_includes_status_and_fields_but_excludes_detail() {
        let chunks = vec![ChunkGitProvenance {
            chunk_ordinal: 0,
            basis: GitChunkBasis::Git,
            author_time: Some(10),
            committer_time: Some(20),
        }];
        let first = resolved_document(
            "a.md",
            chunks.clone(),
            DocumentProvenanceStatus::Resolved,
            Some("first diagnostic".to_owned()),
        );
        let other_detail = resolved_document(
            "a.md",
            chunks.clone(),
            DocumentProvenanceStatus::Resolved,
            Some("other diagnostic".to_owned()),
        );
        let other_status = resolved_document(
            "a.md",
            chunks.clone(),
            DocumentProvenanceStatus::BlameFailed,
            None,
        );
        let other_time = resolved_document(
            "a.md",
            vec![ChunkGitProvenance {
                author_time: Some(11),
                ..chunks[0].clone()
            }],
            DocumentProvenanceStatus::Resolved,
            None,
        );
        assert_eq!(first.projection_hash, other_detail.projection_hash);
        assert_ne!(first.projection_hash, other_status.projection_hash);
        assert_ne!(first.projection_hash, other_time.projection_hash);
    }
}
