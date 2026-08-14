//! Durable repository-reconnaissance classifications and their disposable
//! file-policy projection. This plane never changes structural facts: it only
//! supplies an evidence-backed default policy to retrieval and later scouts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_ALGORITHM: &str = "repository-recon-evidence/v2";
const REPRESENTATIVE_FILE_LIMIT: usize = 8;
pub const ROLES: &[&str] = &[
    "runtime",
    "tooling",
    "documentation",
    "test",
    "generated",
    "mixed",
    "unknown",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubjectSelector {
    RepositoryArea {
        scope: String,
        #[serde(default)]
        direct_only: bool,
    },
    WorkspaceArea {
        package_root: String,
        scope: String,
        #[serde(default)]
        direct_only: bool,
    },
    Project {
        config: String,
        membership_fingerprint: String,
        config_fingerprint: String,
    },
}

impl SubjectSelector {
    fn specificity(&self) -> usize {
        match self {
            Self::RepositoryArea { scope, direct_only }
            | Self::WorkspaceArea {
                scope, direct_only, ..
            } => scope_components(scope) + usize::from(*direct_only),
            Self::Project { .. } => 0,
        }
    }

    pub fn scope(&self) -> Option<&str> {
        match self {
            Self::RepositoryArea { scope, .. } | Self::WorkspaceArea { scope, .. } => Some(scope),
            Self::Project { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberFile {
    pub id: i64,
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiskInput {
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct SubjectState {
    pub subject_key: String,
    pub selector: SubjectSelector,
    pub members: Vec<MemberFile>,
    pub disk_inputs: Vec<DiskInput>,
    pub evidence_fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct NewClassification<'a> {
    pub run_id: i64,
    pub subject_key: &'a str,
    pub subject_kind: &'a str,
    pub selector: &'a SubjectSelector,
    pub parent_subject_key: Option<&'a str>,
    pub depth: usize,
    pub role: &'a str,
    pub confidence: &'a str,
    pub explanation: &'a str,
    pub citations_json: &'a str,
    pub evidence_fingerprint: &'a str,
    pub classification_fingerprint: &'a str,
    pub source_snapshot: &'a str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FilePolicy {
    pub classification_id: i64,
    pub subject_key: String,
    pub role: String,
    pub depth: usize,
}

pub fn build_scope_state(
    root: &Path,
    conn: &Connection,
    subject_key: String,
    selector: SubjectSelector,
) -> Result<SubjectState> {
    if matches!(selector, SubjectSelector::Project { .. }) {
        bail!("project membership must come from the checker inventory");
    }
    let members = members_for_selector(conn, &selector)?;
    let disk_inputs = disk_inputs_for_selector(root, &selector)?;
    let evidence_fingerprint =
        evidence_fingerprint(&subject_key, &selector, &members, &disk_inputs)?;
    Ok(SubjectState {
        subject_key,
        selector,
        members,
        disk_inputs,
        evidence_fingerprint,
    })
}

pub fn build_project_state(
    root: &Path,
    subject_key: String,
    config: String,
    membership_fingerprint: String,
    config_fingerprint: String,
    mut members: Vec<MemberFile>,
) -> Result<SubjectState> {
    members.sort_by(|left, right| left.path.cmp(&right.path));
    members.dedup_by(|left, right| left.path == right.path);
    let selector = SubjectSelector::Project {
        config: config.clone(),
        membership_fingerprint,
        config_fingerprint,
    };
    let disk_inputs = project_disk_inputs(root, &config)?;
    let evidence_fingerprint =
        evidence_fingerprint(&subject_key, &selector, &members, &disk_inputs)?;
    Ok(SubjectState {
        subject_key,
        selector,
        members,
        disk_inputs,
        evidence_fingerprint,
    })
}

pub fn evidence_fingerprint(
    subject_key: &str,
    selector: &SubjectSelector,
    members: &[MemberFile],
    disk_inputs: &[DiskInput],
) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EVIDENCE_ALGORITHM.as_bytes());
    hasher.update(b"\0");
    hasher.update(subject_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(serde_json::to_string(selector)?.as_bytes());
    hasher.update(b"\0");
    // Every path participates in membership freshness. Only the bounded,
    // evenly spread representative files contribute content hashes because
    // only those files contribute outlines/imports/entities to the evidence
    // pack. This avoids re-scouting a large package after an unrelated edit
    // that was never shown to the model.
    for member in members {
        hasher.update(member.path.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\x02");
    for member in representative_members(members, REPRESENTATIVE_FILE_LIMIT) {
        hasher.update(member.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(member.hash.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\x01");
    for input in disk_inputs {
        hasher.update(input.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(input.hash.as_bytes());
        hasher.update(b"\0");
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn representative_members(members: &[MemberFile], limit: usize) -> Vec<&MemberFile> {
    if members.len() <= limit {
        return members.iter().collect();
    }
    (0..limit)
        .map(|index| {
            let offset = index * (members.len() - 1) / (limit - 1);
            &members[offset]
        })
        .collect()
}

pub fn persist_classification(
    conn: &Connection,
    classification: &NewClassification<'_>,
) -> Result<i64> {
    if !ROLES.contains(&classification.role) {
        bail!("repository role must be one of: {}", ROLES.join(", "));
    }
    if !matches!(classification.confidence, "likely" | "possible") {
        bail!("repository classification confidence must be likely or possible");
    }
    let selector_json = serde_json::to_string(classification.selector)?;
    conn.execute(
        "INSERT INTO repository_classifications(
           run_id, subject_key, subject_kind, selector_json, parent_subject_key,
           depth, role, confidence, explanation, citations_json,
           evidence_fingerprint, classification_fingerprint, source_snapshot, created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
                  strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            classification.run_id,
            classification.subject_key,
            classification.subject_kind,
            selector_json,
            classification.parent_subject_key,
            classification.depth as i64,
            classification.role,
            classification.confidence,
            classification.explanation,
            classification.citations_json,
            classification.evidence_fingerprint,
            classification.classification_fingerprint,
            classification.source_snapshot,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rebuild the snapshot-local file projection from every subject whose exact
/// content/evidence fingerprint still exists. A newer stale classification
/// does not mask an older exact match, which is what makes branch-return reuse
/// work without mutating immutable history.
pub fn reconcile_file_policy(root: &Path, conn: &Connection) -> Result<usize> {
    #[derive(Debug)]
    struct Candidate {
        id: i64,
        subject_key: String,
        selector: SubjectSelector,
        role: String,
        depth: usize,
        evidence_fingerprint: String,
    }

    let candidates = {
        let mut statement = conn.prepare(
            "SELECT classification.id, classification.subject_key,
                    classification.selector_json, classification.role,
                    classification.depth, classification.evidence_fingerprint
             FROM repository_classifications classification
             JOIN scout_runs run ON run.id=classification.run_id
             WHERE run.status='completed'
               AND classification.subject_kind IN ('package','area')
               AND classification.confidence='likely'
               AND classification.role IN (
                 'runtime','tooling','documentation','test','generated'
               )
             ORDER BY classification.id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let selector_json = row.get::<_, String>(2)?;
            let selector = serde_json::from_str(&selector_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    selector_json.len(),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(Candidate {
                id: row.get(0)?,
                subject_key: row.get(1)?,
                selector,
                role: row.get(3)?,
                depth: row.get::<_, i64>(4)? as usize,
                evidence_fingerprint: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut selected = Vec::<(Candidate, Vec<MemberFile>)>::new();
    let mut resolved_subjects = BTreeSet::new();
    for candidate in candidates {
        if resolved_subjects.contains(&candidate.subject_key) {
            continue;
        }
        let state = build_scope_state(
            root,
            conn,
            candidate.subject_key.clone(),
            candidate.selector.clone(),
        )?;
        if state.evidence_fingerprint == candidate.evidence_fingerprint {
            resolved_subjects.insert(candidate.subject_key.clone());
            selected.push((candidate, state.members));
        }
    }
    selected.sort_by(|(left, _), (right, _)| {
        left.selector
            .specificity()
            .cmp(&right.selector.specificity())
            .then(left.depth.cmp(&right.depth))
            .then(left.id.cmp(&right.id))
    });

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<usize> {
        conn.execute("DELETE FROM repository_file_policy", [])?;
        let mut statement = conn.prepare_cached(
            "INSERT INTO repository_file_policy(
               file_id, classification_id, subject_key, role, source_hash, depth
             ) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(file_id) DO UPDATE SET
               classification_id=excluded.classification_id,
               subject_key=excluded.subject_key,
               role=excluded.role,
               source_hash=excluded.source_hash,
               depth=excluded.depth",
        )?;
        let mut written = 0;
        for (classification, members) in &selected {
            for member in members {
                statement.execute(params![
                    member.id,
                    classification.id,
                    classification.subject_key,
                    classification.role,
                    member.hash,
                    classification.depth as i64,
                ])?;
                written += 1;
            }
        }
        Ok(written)
    })();
    match result {
        Ok(written) => {
            conn.execute_batch("COMMIT")?;
            Ok(written)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

pub fn file_policy_by_path(conn: &Connection, path: &str) -> Result<Option<FilePolicy>> {
    Ok(conn
        .query_row(
            "SELECT policy.classification_id, policy.subject_key, policy.role, policy.depth
             FROM repository_file_policy policy
             JOIN files file ON file.id=policy.file_id
             WHERE file.path=?1",
            [path],
            |row| {
                Ok(FilePolicy {
                    classification_id: row.get(0)?,
                    subject_key: row.get(1)?,
                    role: row.get(2)?,
                    depth: row.get::<_, i64>(3)? as usize,
                })
            },
        )
        .optional()?)
}

pub fn effective_runtime(
    conn: &Connection,
    path: Option<&str>,
    deterministic_role: Option<&str>,
) -> Result<bool> {
    let policy = path
        .map(|path| file_policy_by_path(conn, path))
        .transpose()?
        .flatten();
    Ok(match policy.as_ref().map(|policy| policy.role.as_str()) {
        Some("runtime") => true,
        Some(role) if is_auxiliary(role) => false,
        _ => deterministic_role == Some("production"),
    })
}

pub fn project_policy(
    conn: &Connection,
    project_id: &str,
    membership_fingerprint: &str,
    config_fingerprint: &str,
) -> Result<Option<FilePolicy>> {
    Ok(conn
        .query_row(
            "SELECT classification.id, classification.subject_key,
                    classification.role, classification.depth
             FROM repository_classifications classification
             JOIN scout_runs run ON run.id=classification.run_id
             WHERE classification.subject_key=?1
               AND classification.subject_kind='project'
               AND classification.confidence='likely'
               AND json_extract(classification.selector_json, '$.membership_fingerprint')=?2
               AND json_extract(classification.selector_json, '$.config_fingerprint')=?3
               AND run.status='completed'
             ORDER BY classification.id DESC LIMIT 1",
            params![
                format!("project:{project_id}"),
                membership_fingerprint,
                config_fingerprint,
            ],
            |row| {
                Ok(FilePolicy {
                    classification_id: row.get(0)?,
                    subject_key: row.get(1)?,
                    role: row.get(2)?,
                    depth: row.get::<_, i64>(3)? as usize,
                })
            },
        )
        .optional()?)
}

pub fn chunk_policy_penalty(conn: &Connection, chunk_id: i64) -> Result<f64> {
    let role = conn
        .query_row(
            "SELECT policy.role
             FROM chunks chunk
             JOIN repository_file_policy policy ON policy.file_id=chunk.file_id
             WHERE chunk.id=?1",
            [chunk_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(role.as_deref().map_or(1.0, policy_penalty))
}

pub fn policy_penalty(role: &str) -> f64 {
    match role {
        "runtime" => 1.0,
        "tooling" => 0.45,
        "documentation" => 0.4,
        "test" => 0.3,
        "generated" => 0.1,
        _ => 1.0,
    }
}

pub fn is_auxiliary(role: &str) -> bool {
    matches!(role, "tooling" | "documentation" | "test" | "generated")
}

fn members_for_selector(conn: &Connection, selector: &SubjectSelector) -> Result<Vec<MemberFile>> {
    let mut members = Vec::new();
    match selector {
        SubjectSelector::RepositoryArea { scope, direct_only } => {
            let mut statement = conn.prepare(
                "SELECT id, path, hash FROM files
                 WHERE origin='repository' AND package_instance_id IS NULL
                 ORDER BY path",
            )?;
            let rows = statement.query_map([], member_row)?;
            for row in rows {
                let member = row?;
                if path_in_scope(&member.path, scope, *direct_only) {
                    members.push(member);
                }
            }
        }
        SubjectSelector::WorkspaceArea {
            package_root,
            scope,
            direct_only,
        } => {
            let mut statement = conn.prepare(
                "SELECT file.id, file.path, file.hash
                 FROM files file
                 JOIN package_instances package ON package.id=file.package_instance_id
                 WHERE package.origin='workspace' AND package.locator=?1
                 ORDER BY file.path",
            )?;
            let rows = statement.query_map([package_root], member_row)?;
            for row in rows {
                let member = row?;
                if path_in_scope(&member.path, scope, *direct_only) {
                    members.push(member);
                }
            }
        }
        SubjectSelector::Project { .. } => {
            bail!("project membership requires a checker inventory")
        }
    }
    Ok(members)
}

fn member_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberFile> {
    Ok(MemberFile {
        id: row.get(0)?,
        path: row.get(1)?,
        hash: row.get(2)?,
    })
}

pub fn path_in_scope(path: &str, scope: &str, direct_only: bool) -> bool {
    if scope == "." || scope.is_empty() {
        return !direct_only || !path.contains('/');
    }
    if path == scope {
        return true;
    }
    let Some(rest) = path.strip_prefix(scope) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('/') else {
        return false;
    };
    !direct_only || !rest.contains('/')
}

fn disk_inputs_for_selector(root: &Path, selector: &SubjectSelector) -> Result<Vec<DiskInput>> {
    match selector {
        SubjectSelector::RepositoryArea { scope, .. } => scope_disk_inputs(root, root, scope),
        SubjectSelector::WorkspaceArea {
            package_root,
            scope,
            ..
        } => scope_disk_inputs(root, &root.join(package_root), scope),
        SubjectSelector::Project { config, .. } => project_disk_inputs(root, config),
    }
}

fn project_disk_inputs(root: &Path, config: &str) -> Result<Vec<DiskInput>> {
    let mut paths = vec![root.join(config)];
    if let Some(manifest) = nearest_manifest(root, root.join(config).parent().unwrap_or(root)) {
        paths.push(manifest);
    }
    hash_disk_inputs(root, paths)
}

fn scope_disk_inputs(root: &Path, owner_root: &Path, scope: &str) -> Result<Vec<DiskInput>> {
    let mut paths = Vec::new();
    let manifest = owner_root.join("package.json");
    if manifest.is_file() {
        paths.push(manifest);
    }
    let scope_root = if scope == "." {
        root.to_path_buf()
    } else {
        root.join(scope)
    };
    if let Ok(entries) = fs::read_dir(&scope_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if is_recon_config(name) {
                paths.push(path);
            }
        }
    }
    hash_disk_inputs(root, paths)
}

fn is_recon_config(name: &str) -> bool {
    name == "package.json"
        || name == "pnpm-workspace.yaml"
        || name == "turbo.json"
        || name == "nx.json"
        || ((name.starts_with("tsconfig") || name.starts_with("jsconfig"))
            && name.ends_with(".json"))
}

fn nearest_manifest(root: &Path, start: &Path) -> Option<PathBuf> {
    let mut directory = start.to_path_buf();
    loop {
        let manifest = directory.join("package.json");
        if manifest.is_file() {
            return Some(manifest);
        }
        if directory == root || !directory.starts_with(root) || !directory.pop() {
            return None;
        }
    }
}

fn hash_disk_inputs(root: &Path, paths: Vec<PathBuf>) -> Result<Vec<DiskInput>> {
    let mut unique = BTreeMap::<String, PathBuf>::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        unique.insert(relative, path);
    }
    unique
        .into_iter()
        .map(|(path, source)| {
            let bytes = fs::read(&source)
                .with_context(|| format!("read reconnaissance input {}", source.display()))?;
            Ok(DiskInput {
                path,
                hash: blake3::hash(&bytes).to_hex().to_string(),
            })
        })
        .collect()
}

fn scope_components(scope: &str) -> usize {
    if scope == "." || scope.is_empty() {
        0
    } else {
        scope.split('/').filter(|part| !part.is_empty()).count()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{
        NewClassification, SubjectSelector, build_scope_state, file_policy_by_path, path_in_scope,
        persist_classification, reconcile_file_policy,
    };
    use crate::scouting::ledger::{RunClaim, RunOutcome, RunSpec};
    use crate::{scouting::ledger, store};

    fn run(conn: &rusqlite::Connection, fingerprint: &str) -> Result<i64> {
        let spec = RunSpec {
            scout_kind: "repository".into(),
            gateway_protocol: 1,
            provider: "openai-codex".into(),
            model: "gpt-5.6-terra".into(),
            billing_path: "plan".into(),
            reasoning: None,
            prompt_version: "repository-recon/v1".into(),
            source_snapshot: "snapshot".into(),
            input_fingerprint: fingerprint.into(),
            request_hash: "request".into(),
            config_json: "{}".into(),
            supersedes_artifact_id: None,
        };
        let RunClaim::Claimed { run_id, .. } = ledger::claim_run(conn, &spec, false)? else {
            panic!("new run");
        };
        Ok(run_id)
    }

    #[test]
    fn scope_matching_is_literal_and_root_direct_is_bounded() {
        assert!(path_in_scope("src/a.ts", "src", false));
        assert!(!path_in_scope("src2/a.ts", "src", false));
        assert!(path_in_scope("root.ts", ".", true));
        assert!(!path_in_scope("src/a.ts", ".", true));
    }

    #[test]
    fn policy_reuses_exact_subject_evidence_and_stales_only_changed_scope() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::create_dir_all(repo.path().join("docs"))?;
        std::fs::create_dir_all(repo.path().join("src"))?;
        std::fs::write(
            repo.path().join("docs/read.ts"),
            "export const guide = 1;\n",
        )?;
        std::fs::write(repo.path().join("src/run.ts"), "export const run = 1;\n")?;
        let conn = store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;

        let docs_selector = SubjectSelector::RepositoryArea {
            scope: "docs".into(),
            direct_only: false,
        };
        let docs = build_scope_state(
            repo.path(),
            &conn,
            "area:repository:docs".into(),
            docs_selector.clone(),
        )?;
        let run_id = run(&conn, "classification-1")?;
        persist_classification(
            &conn,
            &NewClassification {
                run_id,
                subject_key: &docs.subject_key,
                subject_kind: "area",
                selector: &docs_selector,
                parent_subject_key: None,
                depth: 0,
                role: "documentation",
                confidence: "likely",
                explanation: "guide source",
                citations_json: "[\"aggregate:file-kinds\"]",
                evidence_fingerprint: &docs.evidence_fingerprint,
                classification_fingerprint: "classification-1",
                source_snapshot: "snapshot",
            },
        )?;
        ledger::finish_run(&conn, run_id, RunOutcome::Completed, None, None)?;
        assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
        assert_eq!(
            file_policy_by_path(&conn, "docs/read.ts")?.unwrap().role,
            "documentation"
        );

        // A real reindex after an edit outside docs leaves the exact docs
        // evidence fingerprint current.
        std::fs::write(repo.path().join("src/run.ts"), "export const run = 2;\n")?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
        assert!(file_policy_by_path(&conn, "docs/read.ts")?.is_some());

        // A real membership change inside docs makes the classification
        // neutral without deleting its immutable history.
        std::fs::write(
            repo.path().join("docs/new.ts"),
            "export const anotherGuide = 1;\n",
        )?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 0);
        assert!(file_policy_by_path(&conn, "docs/read.ts")?.is_none());

        // Returning to the original branch content reactivates the old exact
        // classification without another model call.
        std::fs::remove_file(repo.path().join("docs/new.ts"))?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        assert_eq!(reconcile_file_policy(repo.path(), &conn)?, 1);
        assert_eq!(
            file_policy_by_path(&conn, "docs/read.ts")?.unwrap().role,
            "documentation"
        );
        Ok(())
    }

    #[test]
    fn scope_freshness_tracks_membership_and_only_bounded_representative_content() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::create_dir_all(repo.path().join("src"))?;
        for index in 0..10 {
            std::fs::write(
                repo.path().join(format!("src/{index:02}.ts")),
                format!("export const value{index} = {index};\n"),
            )?;
        }
        let conn = store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let selector = SubjectSelector::RepositoryArea {
            scope: "src".into(),
            direct_only: false,
        };
        let initial = build_scope_state(
            repo.path(),
            &conn,
            "area:repository:src".into(),
            selector.clone(),
        )?;

        // With ten members and an eight-file spread, 04.ts is not in the
        // bounded evidence sample and therefore must not create recurring
        // model cost for evidence the model never saw.
        std::fs::write(
            repo.path().join("src/04.ts"),
            "export const value4 = 400;\n",
        )?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let unselected_edit = build_scope_state(
            repo.path(),
            &conn,
            "area:repository:src".into(),
            selector.clone(),
        )?;
        assert_eq!(
            unselected_edit.evidence_fingerprint,
            initial.evidence_fingerprint
        );

        std::fs::write(
            repo.path().join("src/05.ts"),
            "export const value5 = 500;\n",
        )?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let selected_edit =
            build_scope_state(repo.path(), &conn, "area:repository:src".into(), selector)?;
        assert_ne!(
            selected_edit.evidence_fingerprint,
            initial.evidence_fingerprint
        );
        Ok(())
    }

    #[test]
    fn fresh_scope_policy_overrides_ambiguous_doc_path_heuristics_for_workflows() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::create_dir_all(repo.path().join("doc"))?;
        std::fs::create_dir_all(repo.path().join("docs"))?;
        std::fs::write(
            repo.path().join("doc/guide.ts"),
            "export function renderGuide() { return 'guide'; }\n",
        )?;
        std::fs::write(
            repo.path().join("docs/runtime.ts"),
            "export function finish() { return 1; }\n\
             export function start() { return finish(); }\n",
        )?;
        let conn = store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let deterministic: String = conn.query_row(
            "SELECT role FROM files WHERE path='docs/runtime.ts'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(deterministic, "documentation");

        let selector = SubjectSelector::RepositoryArea {
            scope: "docs".into(),
            direct_only: false,
        };
        let state = build_scope_state(
            repo.path(),
            &conn,
            "area:repository:docs".into(),
            selector.clone(),
        )?;
        let run_id = run(&conn, "runtime-docs")?;
        persist_classification(
            &conn,
            &NewClassification {
                run_id,
                subject_key: &state.subject_key,
                subject_kind: "area",
                selector: &selector,
                parent_subject_key: None,
                depth: 0,
                role: "runtime",
                confidence: "likely",
                explanation: "runtime document-domain implementation",
                citations_json: "[\"E001\"]",
                evidence_fingerprint: &state.evidence_fingerprint,
                classification_fingerprint: "runtime-docs",
                source_snapshot: "snapshot",
            },
        )?;
        ledger::finish_run(&conn, run_id, RunOutcome::Completed, None, None)?;

        let doc_selector = SubjectSelector::RepositoryArea {
            scope: "doc".into(),
            direct_only: false,
        };
        let doc_state = build_scope_state(
            repo.path(),
            &conn,
            "area:repository:doc".into(),
            doc_selector.clone(),
        )?;
        let doc_run_id = run(&conn, "documentation-doc")?;
        persist_classification(
            &conn,
            &NewClassification {
                run_id: doc_run_id,
                subject_key: &doc_state.subject_key,
                subject_kind: "area",
                selector: &doc_selector,
                parent_subject_key: None,
                depth: 0,
                role: "documentation",
                confidence: "likely",
                explanation: "reader-facing guide implementation",
                citations_json: "[\"E001\"]",
                evidence_fingerprint: &doc_state.evidence_fingerprint,
                classification_fingerprint: "documentation-doc",
                source_snapshot: "snapshot",
            },
        )?;
        ledger::finish_run(&conn, doc_run_id, RunOutcome::Completed, None, None)?;
        reconcile_file_policy(repo.path(), &conn)?;
        assert_eq!(
            file_policy_by_path(&conn, "docs/runtime.ts")?.unwrap().role,
            "runtime"
        );
        assert_eq!(
            file_policy_by_path(&conn, "doc/guide.ts")?.unwrap().role,
            "documentation"
        );

        let candidates = crate::semantic::workflow_candidates(
            repo.path(),
            &conn,
            &["docs/runtime.ts:start".into()],
            &crate::semantic::WorkflowCandidateOptions::default(),
        )?;
        assert!(
            candidates
                .candidates
                .iter()
                .any(|candidate| candidate.display_name == "start")
        );
        Ok(())
    }
}
