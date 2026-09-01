use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::formats::{self, Corpus};

pub(crate) const CODE_DIGEST_META_KEY: &str = "code_digest";
pub(crate) const DOCUMENTATION_DIGEST_META_KEY: &str = "documentation_digest";
pub(crate) const PUBLICATION_SNAPSHOT_META_KEY: &str = "snapshot";
pub(crate) const CODE_SOURCE_PREFIX: &str = "code-v1:";

const CODE_DIGEST_DOMAIN: &[u8] = b"jscout-code-digest-v1\0";
const DOCUMENTATION_DIGEST_DOMAIN: &[u8] = b"jscout-documentation-digest-v1\0";
const PUBLICATION_SNAPSHOT_DOMAIN: &[u8] = b"jscout-publication-snapshot-v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identities {
    pub code: String,
    pub documentation: String,
    pub provenance: String,
    pub publication: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plane {
    Code,
    Documentation,
}

/// The one identity block carried by a public response. `snapshot` is the
/// response plane's invalidation identity; `publication_snapshot` only
/// correlates responses that observed the same atomic publication.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResponseIdentity {
    pub snapshot: String,
    pub publication_snapshot: String,
}

impl Identities {
    pub(crate) fn response(self, plane: Plane) -> ResponseIdentity {
        ResponseIdentity {
            snapshot: match plane {
                Plane::Code => self.code,
                Plane::Documentation => self.documentation,
            },
            publication_snapshot: self.publication,
        }
    }

    pub(crate) fn compute(
        conn: &Connection,
        resolution_hash: &str,
        provenance_digest: &str,
    ) -> Result<Self> {
        let code = compute_code_digest(conn, resolution_hash)?;
        let documentation = compute_documentation_digest(conn)?;
        let publication = fold(&code, &documentation, provenance_digest);
        Ok(Self {
            code,
            documentation,
            provenance: provenance_digest.to_owned(),
            publication,
        })
    }

    pub(crate) fn read(conn: &Connection) -> Result<Self> {
        let (code, documentation, provenance, publication): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn.query_row(
            "SELECT
               (SELECT value FROM meta WHERE key=?1),
               (SELECT value FROM meta WHERE key=?2),
               (SELECT value FROM meta WHERE key=?3),
               (SELECT value FROM meta WHERE key=?4)",
            params![
                CODE_DIGEST_META_KEY,
                DOCUMENTATION_DIGEST_META_KEY,
                crate::docs::PROVENANCE_DIGEST_META_KEY,
                PUBLICATION_SNAPSHOT_META_KEY,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let code = code.context("published index has no code digest; run `jscout index`")?;
        let documentation = documentation
            .context("published index has no documentation digest; run `jscout index`")?;
        let provenance = provenance.context(
            "published index has no documentation provenance digest; run `jscout index`",
        )?;
        let publication = publication
            .context("published index has no publication snapshot; run `jscout index`")?;
        ensure!(
            publication == fold(&code, &documentation, &provenance),
            "published index identity fold is inconsistent; run `jscout index`"
        );
        Ok(Self {
            code,
            documentation,
            provenance,
            publication,
        })
    }

    pub(crate) fn publish(&self, conn: &Connection) -> Result<()> {
        conn.execute(
            "INSERT INTO meta(key,value) VALUES
               (?1,?2), (?3,?4), (?5,?6), (?7,?8)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                CODE_DIGEST_META_KEY,
                self.code,
                DOCUMENTATION_DIGEST_META_KEY,
                self.documentation,
                crate::docs::PROVENANCE_DIGEST_META_KEY,
                self.provenance,
                PUBLICATION_SNAPSHOT_META_KEY,
                self.publication,
            ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn publish_test(
        conn: &Connection,
        code: &str,
        documentation: &str,
        provenance: &str,
    ) -> Result<Self> {
        let identities = Self {
            code: code.to_owned(),
            documentation: documentation.to_owned(),
            provenance: provenance.to_owned(),
            publication: fold(code, documentation, provenance),
        };
        identities.publish(conn)?;
        Ok(identities)
    }
}

pub(crate) fn current_code_digest(conn: &Connection) -> Result<String> {
    Ok(Identities::read(conn)?.code)
}

pub(crate) fn current_documentation_digest(conn: &Connection) -> Result<String> {
    Ok(Identities::read(conn)?.documentation)
}

#[cfg(test)]
pub(crate) fn current_publication_snapshot(conn: &Connection) -> Result<String> {
    Ok(Identities::read(conn)?.publication)
}

pub(crate) fn durable_code_source(code_digest: &str) -> String {
    format!("{CODE_SOURCE_PREFIX}{code_digest}")
}

fn fold(code: &str, documentation: &str, provenance: &str) -> String {
    let mut digest = FramedDigest::new(PUBLICATION_SNAPSHOT_DOMAIN);
    digest.field(code);
    digest.field(documentation);
    digest.field(provenance);
    digest.finish()
}

pub(crate) fn compute_code_digest(conn: &Connection, resolution_hash: &str) -> Result<String> {
    let mut digest = FramedDigest::new(CODE_DIGEST_DOMAIN);
    digest.field("projection-contract");
    digest.field(crate::structural::PROJECTION_VERSION);
    digest.optional(meta_value(conn, "projection_version")?.as_deref());
    digest.field("code-extraction-contract");
    digest.field(crate::entity::EXTRACTION_VERSION);
    digest.optional(meta_value(conn, "extraction_version")?.as_deref());
    hash_active_format_contracts(conn, Corpus::Code, &mut digest)?;

    let rust_present = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM files WHERE corpus='code' AND format=?1
         )",
        [formats::RUST],
        |row| row.get::<_, bool>(0),
    )?;
    digest.field("rust-edition-context");
    let rust_edition_context = rust_present
        .then(|| meta_value(conn, crate::rust_lang::EDITION_CONTEXT_META_KEY))
        .transpose()?
        .flatten();
    digest.optional(rust_edition_context.as_deref());

    let row_count = count_rows(conn, "code")?;
    digest.count(row_count);
    // Preserve the package identity fields from the pre-split structural
    // digest. `canonical_root` is deliberately not a file-content input: its
    // resolver-visible effects are covered by `resolution_hash`, while the
    // absolute installation path itself must not make content non-portable.
    let mut statement = conn.prepare(
        "SELECT file.path, file.hash, file.role, file.origin, file.format,
                file.package_path,
                package.origin, package.name, package.version,
                package.locator, package.manifest_hash, package.status
         FROM files file
         LEFT JOIN package_instances package ON package.id=file.package_instance_id
         WHERE file.corpus='code'
         ORDER BY file.path COLLATE BINARY",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
        ))
    })?;
    for row in rows {
        let (
            path,
            hash,
            role,
            origin,
            format,
            package_path,
            package_origin,
            package_name,
            package_version,
            package_locator,
            package_manifest_hash,
            package_status,
        ) = row?;
        digest.field(&path);
        digest.field(&hash);
        digest.field(&role);
        digest.field(&origin);
        digest.field(&format);
        for value in [
            package_path,
            package_origin,
            package_name,
            package_version,
            package_locator,
            package_manifest_hash,
            package_status,
        ] {
            digest.optional(value.as_deref());
        }
    }
    digest.field("module-resolution");
    digest.field(resolution_hash);
    Ok(digest.finish())
}

pub(crate) fn compute_documentation_digest(conn: &Connection) -> Result<String> {
    let mut digest = FramedDigest::new(DOCUMENTATION_DIGEST_DOMAIN);
    digest.field("documentation-chunk-contract");
    digest.field(crate::docs::CHUNK_FORMAT_VERSION);
    digest.optional(meta_value(conn, "documentation_chunk_format_version")?.as_deref());
    hash_active_format_contracts(conn, Corpus::Docs, &mut digest)?;

    let row_count = count_rows(conn, "docs")?;
    digest.count(row_count);
    let mut statement = conn.prepare(
        "SELECT path,hash,format
         FROM files
         WHERE corpus='docs'
         ORDER BY path COLLATE BINARY",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (path, hash, format) = row?;
        digest.field(&path);
        digest.field(&hash);
        digest.field(&format);
    }
    Ok(digest.finish())
}

fn hash_active_format_contracts(
    conn: &Connection,
    corpus: Corpus,
    digest: &mut FramedDigest,
) -> Result<()> {
    let active = formats::ALL
        .iter()
        .filter(|format| format.corpus == corpus)
        .map(|format| {
            let present = conn.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM files WHERE corpus=?1 AND format=?2
                 )",
                params![corpus.as_str(), format.id],
                |row| row.get::<_, bool>(0),
            )?;
            Ok(present.then_some(format))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    digest.count(active.len());
    for format in active {
        digest.field("active-format-contract");
        digest.field(format.id);
        digest.field(format.extractor_version);
        digest.optional(meta_value(conn, &formats::contract_meta_key(format))?.as_deref());
    }
    Ok(())
}

fn count_rows(conn: &Connection, corpus: &str) -> Result<usize> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE corpus=?1",
        [corpus],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).context("indexed file count exceeded this platform")
}

fn meta_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

struct FramedDigest(blake3::Hasher);

impl FramedDigest {
    fn new(domain: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain);
        Self(hasher)
    }

    fn field(&mut self, value: &str) {
        self.0.update(&(value.len() as u64).to_le_bytes());
        self.0.update(value.as_bytes());
    }

    fn optional(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.0.update(&[1]);
                self.field(value);
            }
            None => {
                self.0.update(&[0]);
            }
        }
    }

    fn count(&mut self, value: usize) {
        self.0.update(&(value as u64).to_le_bytes());
    }

    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::{Connection, params};

    use super::{Identities, compute_code_digest, compute_documentation_digest};

    fn seeded_index() -> Result<(tempfile::TempDir, Connection)> {
        let root = tempfile::tempdir()?;
        let conn = crate::store::open(root.path())?;
        conn.execute_batch(&format!(
            "INSERT INTO meta(key,value) VALUES
               ('extraction_version','{}'),
               ('documentation_chunk_format_version','{}'),
               ('format_contract_version:typescript','{}'),
               ('format_contract_version:markdown','{}');",
            crate::entity::EXTRACTION_VERSION,
            crate::docs::CHUNK_FORMAT_VERSION,
            crate::entity::EXTRACTION_VERSION,
            crate::docs::CHUNK_FORMAT_VERSION,
        ))?;
        conn.execute(
            "INSERT INTO files(
               id,path,hash,corpus,format,role,origin
             ) VALUES(1,'src/main.ts','code-a','code','typescript','runtime','repository')",
            [],
        )?;
        conn.execute(
            "INSERT INTO files(
               id,path,hash,corpus,format,role,origin
             ) VALUES(2,'README.md','docs-a','docs','markdown','documentation','repository')",
            [],
        )?;
        Ok((root, conn))
    }

    #[test]
    fn plane_digests_ignore_foreign_plane_rows() -> Result<()> {
        let (_root, conn) = seeded_index()?;
        let code = compute_code_digest(&conn, "resolution-a")?;
        let documentation = compute_documentation_digest(&conn)?;

        conn.execute(
            "UPDATE files SET hash='docs-b',role='generated',origin='workspace' WHERE id=2",
            [],
        )?;
        assert_eq!(compute_code_digest(&conn, "resolution-a")?, code);
        assert_ne!(compute_documentation_digest(&conn)?, documentation);

        let documentation = compute_documentation_digest(&conn)?;
        conn.execute("UPDATE files SET hash='code-b',role='test' WHERE id=1", [])?;
        assert_ne!(compute_code_digest(&conn, "resolution-a")?, code);
        assert_eq!(compute_documentation_digest(&conn)?, documentation);
        Ok(())
    }

    #[test]
    fn documentation_digest_excludes_code_owned_file_metadata() -> Result<()> {
        let (_root, conn) = seeded_index()?;
        let documentation = compute_documentation_digest(&conn)?;
        conn.execute(
            "UPDATE files
             SET role='generated',origin='workspace',package_path='README.md'
             WHERE id=2",
            [],
        )?;
        assert_eq!(compute_documentation_digest(&conn)?, documentation);
        Ok(())
    }

    #[test]
    fn code_digest_includes_joined_package_identity() -> Result<()> {
        let (_root, conn) = seeded_index()?;
        conn.execute(
            "INSERT INTO package_instances(
               id,origin,name,version,canonical_root,locator,manifest_hash,status
             ) VALUES(1,'workspace','pkg','1.0.0','packages/pkg','workspace:pkg','manifest-a','complete')",
            [],
        )?;
        conn.execute(
            "UPDATE files SET package_instance_id=1,package_path='src/main.ts' WHERE id=1",
            [],
        )?;
        let code = compute_code_digest(&conn, "resolution-a")?;
        conn.execute(
            "UPDATE package_instances SET locator='workspace:pkg-next' WHERE id=1",
            [],
        )?;
        assert_ne!(compute_code_digest(&conn, "resolution-a")?, code);
        Ok(())
    }

    #[test]
    fn publication_fold_correlates_planes_and_provenance() -> Result<()> {
        let (_root, conn) = seeded_index()?;
        let first = Identities::compute(&conn, "resolution-a", "provenance-a")?;
        let provenance_only = Identities::compute(&conn, "resolution-a", "provenance-b")?;
        assert_eq!(first.code, provenance_only.code);
        assert_eq!(first.documentation, provenance_only.documentation);
        assert_ne!(first.publication, provenance_only.publication);
        first.publish(&conn)?;
        assert_eq!(Identities::read(&conn)?, first);
        conn.execute(
            "UPDATE meta SET value=?1 WHERE key='snapshot'",
            params![provenance_only.publication],
        )?;
        assert!(
            Identities::read(&conn)
                .unwrap_err()
                .to_string()
                .contains("identity fold is inconsistent")
        );
        Ok(())
    }
}
