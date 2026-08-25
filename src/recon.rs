//! Durable repository-reconnaissance classifications and their disposable
//! file-policy projection. This plane never changes structural facts: it only
//! supplies an evidence-backed default policy to retrieval and later scouts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_ALGORITHM: &str = "repository-recon-evidence/v3";
pub const REPRESENTATIVE_FILE_LIMIT: usize = 8;
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
    pub cited_evidence_json: &'a str,
    pub evidence_fingerprint: &'a str,
    pub classification_fingerprint: &'a str,
    pub source_snapshot: &'a str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FilePolicy {
    pub classification_id: i64,
    pub subject_key: String,
    pub scope_role: String,
    pub effective_role: String,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectPolicy {
    pub classification_id: i64,
    pub subject_key: String,
    pub role: String,
    pub depth: usize,
}

#[derive(Debug)]
struct PolicyCandidate {
    id: i64,
    subject_key: String,
    subject_kind: String,
    selector: SubjectSelector,
    selector_json: String,
    role: String,
    confidence: String,
    explanation: String,
    citations_json: String,
    cited_evidence_json: String,
    parent_subject_key: Option<String>,
    depth: usize,
    evidence_fingerprint: String,
    prompt_version: String,
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

pub fn representative_members(members: &[MemberFile], limit: usize) -> Vec<&MemberFile> {
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
           cited_evidence_json, evidence_fingerprint, classification_fingerprint,
           source_snapshot, created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
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
            classification.cited_evidence_json,
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
    let candidates = {
        let mut statement = conn.prepare(
            "SELECT classification.id, classification.subject_key,
                    classification.subject_kind, classification.selector_json,
                    classification.role, classification.confidence,
                    classification.explanation, classification.citations_json,
                    classification.cited_evidence_json,
                    classification.parent_subject_key, classification.depth,
                    classification.evidence_fingerprint, run.prompt_version
             FROM repository_classifications classification
             JOIN scout_runs run ON run.id=classification.run_id
             WHERE run.status='completed'
               AND classification.subject_kind IN ('package','area')
             ORDER BY classification.id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let selector_json = row.get::<_, String>(3)?;
            let selector = serde_json::from_str(&selector_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    selector_json.len(),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(PolicyCandidate {
                id: row.get(0)?,
                subject_key: row.get(1)?,
                subject_kind: row.get(2)?,
                selector_json,
                selector,
                role: row.get(4)?,
                confidence: row.get(5)?,
                explanation: row.get(6)?,
                citations_json: row.get(7)?,
                cited_evidence_json: row.get(8)?,
                parent_subject_key: row.get(9)?,
                depth: row.get::<_, i64>(10)? as usize,
                evidence_fingerprint: row.get(11)?,
                prompt_version: row.get(12)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut states = BTreeMap::<(String, String), SubjectState>::new();
    let mut current = BTreeMap::<String, (PolicyCandidate, Vec<MemberFile>)>::new();
    let mut resolved_subjects = BTreeSet::new();
    for candidate in candidates {
        if resolved_subjects.contains(&candidate.subject_key) {
            continue;
        }
        let cache_key = (
            candidate.subject_key.clone(),
            candidate.selector_json.clone(),
        );
        if !states.contains_key(&cache_key) {
            states.insert(
                cache_key.clone(),
                build_scope_state(
                    root,
                    conn,
                    candidate.subject_key.clone(),
                    candidate.selector.clone(),
                )?,
            );
        }
        let state = &states[&cache_key];
        if state.evidence_fingerprint == candidate.evidence_fingerprint {
            resolved_subjects.insert(candidate.subject_key.clone());
            current.insert(
                candidate.subject_key.clone(),
                (candidate, state.members.clone()),
            );
        }
    }
    let definite = current
        .iter()
        .filter_map(|(subject, (candidate, _))| actionable(candidate).then_some(subject.clone()))
        .collect::<BTreeSet<_>>();
    let mut suppressed = BTreeSet::new();
    for (subject, (candidate, _)) in &current {
        let mut ancestor = candidate.parent_subject_key.as_ref();
        let mut visited = BTreeSet::new();
        while let Some(parent) = ancestor {
            if !visited.insert(parent.clone()) {
                break;
            }
            if definite.contains(parent) {
                suppressed.insert(subject.clone());
                break;
            }
            ancestor = current
                .get(parent)
                .and_then(|(parent_candidate, _)| parent_candidate.parent_subject_key.as_ref());
        }
    }
    let active = current
        .into_iter()
        .filter_map(|(subject, value)| (!suppressed.contains(&subject)).then_some(value))
        .collect::<Vec<_>>();
    let mut selected = active
        .iter()
        .filter(|(candidate, _)| actionable(candidate))
        .collect::<Vec<_>>();
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
        conn.execute("DELETE FROM repository_current_classifications", [])?;
        let file_roles = {
            let mut statement = conn.prepare("SELECT id, role FROM code_files ORDER BY id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()?
        };
        let mut current_statement = conn.prepare_cached(
            "INSERT INTO repository_current_classifications(
               classification_id,subject_key,subject_kind,role,confidence,
               explanation,citations_json,cited_evidence_json,member_count,
               deterministic_roles_json,effective_roles_json,conflict_files,
               depth,prompt_version
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        for (classification, members) in &active {
            let is_actionable = actionable(classification);
            let mut deterministic_counts = BTreeMap::<String, usize>::new();
            let mut effective_counts = BTreeMap::<String, usize>::new();
            let mut conflicts = 0;
            for member in members {
                let deterministic = file_roles.get(&member.id).map_or("unknown", String::as_str);
                *deterministic_counts
                    .entry(deterministic.into())
                    .or_default() += 1;
                let effective = if is_actionable {
                    effective_file_role(deterministic, &classification.role)
                } else {
                    deterministic
                };
                *effective_counts.entry(effective.into()).or_default() += 1;
                if is_actionable && effective != classification.role {
                    conflicts += 1;
                }
            }
            current_statement.execute(params![
                classification.id,
                classification.subject_key,
                classification.subject_kind,
                classification.role,
                classification.confidence,
                classification.explanation,
                classification.citations_json,
                classification.cited_evidence_json,
                members.len() as i64,
                serde_json::to_string(&deterministic_counts)?,
                serde_json::to_string(&effective_counts)?,
                conflicts,
                classification.depth as i64,
                classification.prompt_version,
            ])?;
        }
        let mut statement = conn.prepare_cached(
            "INSERT INTO repository_file_policy(
               file_id,classification_id,subject_key,scope_role,effective_role,
               source_hash,depth
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(file_id) DO UPDATE SET
               classification_id=excluded.classification_id,
               subject_key=excluded.subject_key,
               scope_role=excluded.scope_role,
               effective_role=excluded.effective_role,
               source_hash=excluded.source_hash,
               depth=excluded.depth",
        )?;
        let mut written = 0;
        for (classification, members) in &selected {
            for member in members {
                let deterministic = file_roles.get(&member.id).map_or("unknown", String::as_str);
                statement.execute(params![
                    member.id,
                    classification.id,
                    classification.subject_key,
                    classification.role,
                    effective_file_role(deterministic, &classification.role),
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

/// L1 publication must never depend on the optional semantic policy plane.
/// If current evidence cannot be read or reconciled, remove the disposable
/// projection and keep the freshly published structural index fully usable.
pub fn reconcile_file_policy_after_index(root: &Path, conn: &Connection) {
    if let Err(error) = reconcile_file_policy(root, conn) {
        let clear_error = conn
            .execute_batch(
                "DELETE FROM repository_file_policy;
                 DELETE FROM repository_current_classifications;",
            )
            .err();
        eprintln!(
            "repository reconnaissance policy unavailable after index; using neutral defaults: {error}"
        );
        if let Some(clear_error) = clear_error {
            eprintln!(
                "failed to clear repository reconnaissance policy after reconciliation error: {clear_error}"
            );
        }
    }
}

/// Combine the high-precision deterministic artifact role with the semantic
/// purpose of its containing scope. Test, fixture, and generated are hard
/// artifact facts; a coarse runtime classification must not erase them.
/// Documentation and unknown remain intentionally overridable because those
/// bootstrap labels are ambiguous in document-domain repositories.
pub fn effective_file_role<'a>(deterministic_role: &'a str, scope_role: &'a str) -> &'a str {
    match deterministic_role {
        "test" | "fixture" | "generated" => deterministic_role,
        _ => scope_role,
    }
}

fn actionable(candidate: &PolicyCandidate) -> bool {
    candidate.confidence == "likely"
        && matches!(
            candidate.role.as_str(),
            "runtime" | "tooling" | "documentation" | "test" | "generated"
        )
}

pub fn file_policy_by_path(conn: &Connection, path: &str) -> Result<Option<FilePolicy>> {
    Ok(conn
        .query_row(
            "SELECT policy.classification_id, policy.subject_key,
                    policy.scope_role, policy.effective_role, policy.depth
             FROM repository_file_policy policy
             JOIN files file ON file.id=policy.file_id
             WHERE file.path=?1",
            [path],
            |row| {
                Ok(FilePolicy {
                    classification_id: row.get(0)?,
                    subject_key: row.get(1)?,
                    scope_role: row.get(2)?,
                    effective_role: row.get(3)?,
                    depth: row.get::<_, i64>(4)? as usize,
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
    Ok(
        match policy.as_ref().map(|policy| policy.effective_role.as_str()) {
            Some("runtime") => true,
            Some(role) if is_auxiliary(role) => false,
            _ => deterministic_role == Some("production"),
        },
    )
}

pub fn project_policy(
    conn: &Connection,
    project_id: &str,
    membership_fingerprint: &str,
    config_fingerprint: &str,
) -> Result<Option<ProjectPolicy>> {
    let policy = conn
        .query_row(
            "SELECT classification.id, classification.subject_key,
                    classification.role, classification.depth,
                    classification.confidence
             FROM repository_classifications classification
             JOIN scout_runs run ON run.id=classification.run_id
             WHERE classification.subject_key=?1
               AND classification.subject_kind='project'
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
                Ok((
                    ProjectPolicy {
                        classification_id: row.get(0)?,
                        subject_key: row.get(1)?,
                        role: row.get(2)?,
                        depth: row.get::<_, i64>(3)? as usize,
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(policy.and_then(|(policy, confidence)| {
        (confidence == "likely"
            && matches!(
                policy.role.as_str(),
                "runtime" | "tooling" | "documentation" | "test" | "generated"
            ))
        .then_some(policy)
    }))
}

pub fn chunk_policy_penalty(conn: &Connection, chunk_id: i64) -> Result<f64> {
    let role = conn
        .query_row(
            "SELECT policy.effective_role
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

/// Resolve one exact current reconnaissance subject to its current indexed
/// members. Only classifications admitted to the disposable current
/// projection are targetable; stale durable history must not silently widen a
/// targeted scout or semantic-memory query.
pub fn current_subject_members(conn: &Connection, subject_key: &str) -> Result<Vec<MemberFile>> {
    let selector_json = conn
        .query_row(
            "SELECT classification.selector_json
             FROM repository_current_classifications current
             JOIN repository_classifications classification
               ON classification.id=current.classification_id
             WHERE current.subject_key=?1",
            [subject_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| {
            format!(
                "reconnaissance subject `{subject_key}` is not current; run `jscout overview` to list current subject keys"
            )
        })?;
    let selector: SubjectSelector = serde_json::from_str(&selector_json).with_context(|| {
        format!("current reconnaissance subject `{subject_key}` has invalid selector JSON")
    })?;
    members_for_selector(conn, &selector)
}

/// Most-specific current reconnaissance scope for each indexed member. This
/// includes neutral mixed/unknown classifications that intentionally do not
/// enter `repository_file_policy`, allowing coverage accounting without
/// turning those classifications into hard retrieval policy.
pub fn current_scope_memberships(conn: &Connection) -> Result<BTreeMap<i64, String>> {
    let mut statement = conn.prepare(
        "SELECT current.subject_key, current.depth, classification.selector_json
         FROM repository_current_classifications current
         JOIN repository_classifications classification
           ON classification.id=current.classification_id
         ORDER BY current.depth DESC, current.subject_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut selectors = Vec::new();
    for row in rows {
        let (subject_key, _, selector_json) = row?;
        let selector: SubjectSelector =
            serde_json::from_str(&selector_json).with_context(|| {
                format!("current reconnaissance subject `{subject_key}` has invalid selector JSON")
            })?;
        selectors.push((subject_key, selector));
    }

    // Scope matching is a literal-prefix operation. Scan file identity once
    // instead of materializing every file once per current subject.
    let mut files = conn.prepare(
        "SELECT file.id, file.path, file.origin, package.origin, package.locator
         FROM code_files file
         LEFT JOIN package_instances package ON package.id=file.package_instance_id
         ORDER BY file.id",
    )?;
    let rows = files.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut memberships = BTreeMap::new();
    for row in rows {
        let (file_id, path, file_origin, package_origin, package_locator) = row?;
        let subject = selectors.iter().find_map(|(subject_key, selector)| {
            let matches = match selector {
                SubjectSelector::RepositoryArea { scope, direct_only } => {
                    file_origin == "repository"
                        && package_origin.is_none()
                        && path_in_scope(&path, scope, *direct_only)
                }
                SubjectSelector::WorkspaceArea {
                    package_root,
                    scope,
                    direct_only,
                } => {
                    package_origin.as_deref() == Some("workspace")
                        && package_locator.as_deref() == Some(package_root.as_str())
                        && path_in_scope(&path, scope, *direct_only)
                }
                SubjectSelector::Project { .. } => false,
            };
            matches.then_some(subject_key)
        });
        if let Some(subject) = subject {
            memberships.insert(file_id, subject.clone());
        }
    }
    Ok(memberships)
}

fn members_for_selector(conn: &Connection, selector: &SubjectSelector) -> Result<Vec<MemberFile>> {
    let mut members = Vec::new();
    match selector {
        SubjectSelector::RepositoryArea { scope, direct_only } => {
            let mut statement = conn.prepare(
                "SELECT file.id, file.path, file.hash FROM code_files file
                 WHERE file.origin='repository' AND file.package_instance_id IS NULL
                 ORDER BY file.path",
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
                 FROM code_files file
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
mod tests;
