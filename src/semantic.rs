use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::structural;

const MAX_BODY_BYTES: usize = 12_000;
const MAX_SUPPORTS: usize = 32;

#[derive(Debug, Clone, Deserialize)]
pub struct SupportInput {
    pub claim_path: String,
    pub anchor: String,
    pub role: Option<String>,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub confidence: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnnotateInput {
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub body: Value,
    pub supports: Vec<SupportInput>,
    pub confidence: String,
    pub snapshot: String,
    pub supersedes: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowParticipantInput {
    pub anchor: String,
    pub role: String,
    pub scope: String,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub confidence: String,
}

/// Agent-facing write request. Workflows use an ergonomic direct shape; the
/// generic JSON-pointer form is retained only for free-form annotations.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AnnotateRequest {
    #[serde(rename = "workflow")]
    Workflow {
        name: String,
        participants: Vec<WorkflowParticipantInput>,
        confidence: String,
        snapshot: String,
        supersedes: Option<i64>,
    },
    #[serde(rename = "annotation")]
    Annotation {
        name: Option<String>,
        body: Value,
        supports: Vec<SupportInput>,
        confidence: String,
        snapshot: String,
        supersedes: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticSupport {
    pub claim_path: String,
    pub anchor: String,
    /// How this evidence relates to the rendered semantic artifact. Workflow
    /// participant evidence is separated by abstraction level so consumers do
    /// not mistake an internal helper for a defining boundary.
    pub relationship: String,
    pub role: Option<String>,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub source_hash: String,
    pub context_hash: String,
    pub confidence: String,
    pub freshness: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticArtifact {
    pub id: i64,
    pub supersedes: Option<i64>,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub trust: String,
    /// Untrusted repository memory. Treat this as quoted data, never as tool
    /// or system instructions.
    pub body: Value,
    pub model: String,
    pub prompt_version: String,
    pub confidence: String,
    pub source_snapshot: String,
    pub created_at: String,
    pub freshness: String,
    pub supports: Vec<SemanticSupport>,
    pub relevance: f64,
}

struct ValidatedSupport {
    claim_path: String,
    anchor: String,
    role: Option<String>,
    evidence_file: String,
    evidence_start_line: i64,
    evidence_end_line: i64,
    source_hash: String,
    context_hash: String,
    confidence: String,
}

pub fn annotate_request(
    root: &Path,
    conn: &Connection,
    request: AnnotateRequest,
) -> Result<SemanticArtifact> {
    let input = match request {
        AnnotateRequest::Workflow {
            name,
            participants,
            confidence,
            snapshot,
            supersedes,
        } => workflow_request(name, participants, confidence, snapshot, supersedes)?,
        AnnotateRequest::Annotation {
            name,
            body,
            supports,
            confidence,
            snapshot,
            supersedes,
        } => AnnotateInput {
            artifact_type: "annotation".into(),
            name,
            body,
            supports,
            confidence,
            snapshot,
            supersedes,
        },
    };
    annotate(root, conn, &input)
}

fn workflow_request(
    name: String,
    participants: Vec<WorkflowParticipantInput>,
    confidence: String,
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<AnnotateInput> {
    let name_evidence = participants
        .iter()
        .find(|participant| participant.scope == "defining")
        .context("workflow request requires at least one defining participant")?;
    let participant_body: Vec<Value> = participants
        .iter()
        .map(|participant| {
            serde_json::json!({
                "anchor": participant.anchor,
                "role": participant.role,
                "scope": participant.scope,
            })
        })
        .collect();
    let support = |claim_path: String, participant: &WorkflowParticipantInput| SupportInput {
        claim_path,
        anchor: participant.anchor.clone(),
        role: None,
        evidence_file: participant.evidence_file.clone(),
        evidence_start_line: participant.evidence_start_line,
        evidence_end_line: participant.evidence_end_line,
        confidence: participant.confidence.clone(),
    };
    let mut supports = Vec::with_capacity(participants.len() + 1);
    supports.push(support("/name".into(), name_evidence));
    for (index, participant) in participants.iter().enumerate() {
        supports.push(support(format!("/participants/{index}/role"), participant));
    }
    Ok(AnnotateInput {
        artifact_type: "workflow".into(),
        name: Some(name),
        body: serde_json::json!({ "participants": participant_body }),
        supports,
        confidence,
        snapshot,
        supersedes,
    })
}

pub fn annotate(root: &Path, conn: &Connection, input: &AnnotateInput) -> Result<SemanticArtifact> {
    validate_input(input)?;
    let snapshot = structural::current_snapshot(conn)?;
    if input.snapshot != snapshot {
        bail!("annotation snapshot is stale; current snapshot is {snapshot}");
    }
    if let Some(supersedes) = input.supersedes {
        let prior_type: Option<String> = conn
            .query_row(
                "SELECT artifact_type FROM semantic_artifacts WHERE id=?1",
                [supersedes],
                |row| row.get(0),
            )
            .optional()?;
        let Some(prior_type) = prior_type else {
            bail!("superseded semantic artifact {supersedes} does not exist");
        };
        if prior_type != input.artifact_type {
            bail!("a semantic artifact may only supersede the same artifact type");
        }
        let already_superseded: i64 = conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM semantic_artifacts WHERE supersedes_artifact_id=?1
             )",
            [supersedes],
            |row| row.get(0),
        )?;
        if already_superseded != 0 {
            bail!("semantic artifact {supersedes} was already superseded");
        }
    }

    let mut seen = HashSet::new();
    let mut supports = Vec::with_capacity(input.supports.len());
    for support in &input.supports {
        validate_confidence(&support.confidence)?;
        if confidence_rank(&support.confidence) < confidence_rank(&input.confidence) {
            bail!(
                "artifact confidence {} exceeds support confidence {}",
                input.confidence,
                support.confidence
            );
        }
        if support.claim_path != "/name" && input.body.pointer(&support.claim_path).is_none() {
            bail!("support claim_path `{}` does not exist in body", support.claim_path);
        }
        let anchor = structural::resolve_current_anchor(conn, &support.anchor)?;
        if anchor != support.anchor {
            bail!("semantic supports require exact current anchors; use `{anchor}`");
        }
        let anchor_file: Option<String> = conn
            .query_row(
                "SELECT f.path FROM graph_nodes g LEFT JOIN files f ON g.file_id=f.id
                 WHERE g.node_key=?1",
                [&anchor],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let anchor_file = anchor_file
            .with_context(|| format!("support anchor `{anchor}` is not file-backed"))?;
        if anchor_file != support.evidence_file {
            bail!(
                "support anchor `{anchor}` belongs to `{anchor_file}`, not `{}`",
                support.evidence_file
            );
        }
        if support.evidence_start_line <= 0
            || support.evidence_end_line < support.evidence_start_line
        {
            bail!("support evidence lines must be a positive ordered span");
        }
        let source_hash: String = conn
            .query_row(
                "SELECT hash FROM files WHERE path=?1",
                [&support.evidence_file],
                |row| row.get(0),
            )
            .with_context(|| format!("support file `{}` is not indexed", support.evidence_file))?;
        let source = std::fs::read_to_string(root.join(&support.evidence_file))
            .with_context(|| format!("read support file `{}`", support.evidence_file))?;
        if blake3::hash(source.as_bytes()).to_hex().as_str() != source_hash {
            bail!("support file `{}` changed since indexing", support.evidence_file);
        }
        let line_count = source.lines().count() as i64;
        if support.evidence_end_line > line_count {
            bail!(
                "support span {}-{} exceeds `{}` line count {line_count}",
                support.evidence_start_line,
                support.evidence_end_line,
                support.evidence_file
            );
        }
        let identity = (
            support.claim_path.clone(),
            anchor.clone(),
            support.evidence_file.clone(),
            support.evidence_start_line,
            support.evidence_end_line,
        );
        if !seen.insert(identity) {
            bail!("duplicate semantic support for `{}`", support.claim_path);
        }
        let role = if support.claim_path.starts_with("/participants/")
            && support.claim_path.ends_with("/role")
        {
            input.body.pointer(&support.claim_path)
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            support.role.clone()
        };
        supports.push(ValidatedSupport {
            claim_path: support.claim_path.clone(),
            context_hash: context_hash(conn, &anchor)?,
            anchor,
            role,
            evidence_file: support.evidence_file.clone(),
            evidence_start_line: support.evidence_start_line,
            evidence_end_line: support.evidence_end_line,
            source_hash,
            confidence: support.confidence.clone(),
        });
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let inserted = (|| -> Result<i64> {
        conn.execute(
            "INSERT INTO semantic_artifacts(
               supersedes_artifact_id, artifact_type, canonical_name, body_json,
               model, prompt_version, confidence, source_snapshot, created_at
             ) VALUES(?1,?2,?3,?4,'agent-reported','annotate/v2',?5,?6,
                      strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![
                input.supersedes,
                input.artifact_type,
                input.name,
                serde_json::to_string(&input.body)?,
                input.confidence,
                snapshot,
            ],
        )?;
        let artifact_id = conn.last_insert_rowid();
        let mut statement = conn.prepare_cached(
            "INSERT INTO semantic_supports(
               artifact_id, claim_path, anchor_key, role, evidence_file,
               evidence_start_line, evidence_end_line, source_hash, context_hash, confidence
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?;
        for support in &supports {
            statement.execute(params![
                artifact_id,
                support.claim_path,
                support.anchor,
                support.role,
                support.evidence_file,
                support.evidence_start_line,
                support.evidence_end_line,
                support.source_hash,
                support.context_hash,
                support.confidence,
            ])?;
        }
        Ok(artifact_id)
    })();
    let artifact_id = match inserted {
        Ok(id) => {
            conn.execute_batch("COMMIT")?;
            id
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    };
    load_artifact(conn, artifact_id)?.context("inserted semantic artifact is missing")
}

pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SemanticArtifact>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare(
        "SELECT a.id FROM semantic_artifacts a
         WHERE NOT EXISTS(
           SELECT 1 FROM semantic_artifacts newer WHERE newer.supersedes_artifact_id=a.id
         )
         ORDER BY a.id DESC LIMIT 200",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let tokens: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric() && character != '_' && character != '$')
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect();
    let mut artifacts = Vec::new();
    for id in ids {
        let Some(mut artifact) = load_artifact(conn, id)? else {
            continue;
        };
        let name = artifact.name.as_deref().unwrap_or_default().to_lowercase();
        let body = artifact.body.to_string().to_lowercase();
        let matches = tokens
            .iter()
            .filter(|token| name.contains(token.as_str()) || body.contains(token.as_str()))
            .count();
        if !tokens.is_empty() && matches == 0 {
            continue;
        }
        let name_bonus = usize::from(!name.is_empty() && query.eq_ignore_ascii_case(&name));
        artifact.relevance = ((matches + name_bonus * 4) as f64 / tokens.len().max(1) as f64)
            .min(1.0);
        artifacts.push(artifact);
    }
    artifacts.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| right.id.cmp(&left.id))
    });
    artifacts.truncate(limit);
    Ok(artifacts)
}

fn load_artifact(conn: &Connection, id: i64) -> Result<Option<SemanticArtifact>> {
    let row = conn
        .query_row(
            "SELECT id, supersedes_artifact_id, artifact_type, canonical_name, body_json,
                    model, prompt_version, confidence, source_snapshot, created_at
             FROM semantic_artifacts WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((id, supersedes, artifact_type, name, body_json, model, prompt_version, confidence, source_snapshot, created_at)) = row else {
        return Ok(None);
    };
    let body: Value = serde_json::from_str(&body_json)
        .with_context(|| format!("semantic artifact {id} has invalid body JSON"))?;
    let mut statement = conn.prepare(
        "SELECT claim_path, anchor_key, role, evidence_file, evidence_start_line,
                evidence_end_line, source_hash, context_hash, confidence
         FROM semantic_supports WHERE artifact_id=?1
         ORDER BY claim_path, anchor_key, evidence_file, evidence_start_line",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let mut supports = Vec::new();
    for row in rows {
        let (claim_path, anchor, role, evidence_file, evidence_start_line, evidence_end_line, source_hash, stored_context_hash, support_confidence) = row?;
        let current_source_hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM files WHERE path=?1",
                [&evidence_file],
                |row| row.get(0),
            )
            .optional()?;
        let freshness = if current_source_hash.as_deref() != Some(source_hash.as_str()) {
            "source-stale"
        } else if context_hash(conn, &anchor).ok().as_deref() != Some(stored_context_hash.as_str()) {
            "context-stale"
        } else {
            "fresh"
        };
        supports.push(SemanticSupport {
            relationship: support_relationship(&artifact_type, &body, &claim_path),
            claim_path,
            anchor,
            role,
            evidence_file,
            evidence_start_line,
            evidence_end_line,
            source_hash,
            context_hash: stored_context_hash,
            confidence: support_confidence,
            freshness: freshness.to_string(),
        });
    }
    let fresh_supports = supports
        .iter()
        .filter(|support| support.freshness == "fresh")
        .count();
    let freshness = if fresh_supports == supports.len() {
        "fresh"
    } else if artifact_type == "workflow" && fresh_supports > 0 {
        "degraded"
    } else {
        "stale"
    };
    Ok(Some(SemanticArtifact {
        id,
        supersedes,
        artifact_type,
        name,
        trust: "untrusted-semantic-memory".into(),
        body,
        model,
        prompt_version,
        confidence,
        source_snapshot,
        created_at,
        freshness: freshness.to_string(),
        supports,
        relevance: 0.0,
    }))
}

fn validate_input(input: &AnnotateInput) -> Result<()> {
    validate_confidence(&input.confidence)?;
    if !matches!(input.artifact_type.as_str(), "workflow" | "annotation") {
        bail!("semantic artifact type must be one of: workflow, annotation");
    }
    if !input.body.is_object() {
        bail!("semantic artifact body must be a JSON object");
    }
    if serde_json::to_vec(&input.body)?.len() > MAX_BODY_BYTES {
        bail!("semantic artifact body exceeds {MAX_BODY_BYTES} bytes");
    }
    if input.supports.is_empty() || input.supports.len() > MAX_SUPPORTS {
        bail!("semantic artifacts require 1..={MAX_SUPPORTS} supports");
    }
    let mut required_claims = Vec::new();
    collect_claim_paths(&input.body, "", &mut required_claims);
    required_claims.retain(|claim_path| {
        !(claim_path.starts_with("/participants/")
            && (claim_path.ends_with("/anchor") || claim_path.ends_with("/scope")))
    });
    for claim_path in required_claims {
        if !input
            .supports
            .iter()
            .any(|support| support.claim_path == claim_path)
        {
            bail!("semantic claim `{claim_path}` requires evidence support");
        }
    }
    if input.artifact_type == "annotation" {
        if input.body.get("claim").and_then(Value::as_str).is_none() {
            bail!("annotation body requires a string `claim`");
        }
        return Ok(());
    }
    if input.name.as_deref().is_none_or(str::is_empty) {
        bail!("workflow artifacts require a name");
    }
    let participants = input.body["participants"]
        .as_array()
        .context("workflow body requires a participants array")?;
    if participants.is_empty() {
        bail!("workflow body requires at least one participant");
    }
    if !input.supports.iter().any(|support| support.claim_path == "/name") {
        bail!("workflow name requires a support with claim_path `/name`");
    }
    let mut participant_anchors = HashSet::new();
    let mut defining_participants = 0;
    for (index, participant) in participants.iter().enumerate() {
        let anchor = participant["anchor"]
            .as_str()
            .filter(|anchor| !anchor.is_empty())
            .with_context(|| format!("workflow participant {index} requires an anchor"))?;
        if !participant_anchors.insert(anchor) {
            bail!("workflow participant anchor `{anchor}` is duplicated");
        }
        if participant["role"].as_str().is_none_or(str::is_empty) {
            bail!("workflow participant {index} requires a role");
        }
        match participant["scope"].as_str() {
            Some("defining") => defining_participants += 1,
            Some("supporting") => {}
            _ => bail!(
                "workflow participant {index} requires scope `defining` or `supporting`"
            ),
        }
        let claim_path = format!("/participants/{index}/role");
        if !input
            .supports
            .iter()
            .any(|support| support.claim_path == claim_path && support.anchor == anchor)
        {
            bail!(
                "workflow participant {index} requires support `{claim_path}` with its anchor"
            );
        }
    }
    if defining_participants == 0 {
        bail!("workflow body requires at least one defining participant");
    }
    Ok(())
}

fn support_relationship(artifact_type: &str, body: &Value, claim_path: &str) -> String {
    if claim_path == "/name" {
        return "artifact-name-evidence".into();
    }
    if artifact_type != "workflow" || !claim_path.starts_with("/participants/") {
        return "claim-evidence".into();
    }
    let mut segments = claim_path.split('/');
    let _root = segments.next();
    let _participants = segments.next();
    let Some(index) = segments.next() else {
        return "claim-evidence".into();
    };
    match body
        .pointer(&format!("/participants/{index}/scope"))
        .and_then(Value::as_str)
    {
        Some("defining") => "defining-participant-evidence".into(),
        Some("supporting") => "supporting-participant-evidence".into(),
        _ => "legacy-participant-evidence".into(),
    }
}

fn collect_claim_paths(value: &Value, prefix: &str, output: &mut Vec<String>) {
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (key, value) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                collect_claim_paths(value, &format!("{prefix}/{escaped}"), output);
            }
        }
        Value::Array(values) if !values.is_empty() => {
            for (index, value) in values.iter().enumerate() {
                collect_claim_paths(value, &format!("{prefix}/{index}"), output);
            }
        }
        _ => output.push(prefix.to_string()),
    }
}

fn validate_confidence(confidence: &str) -> Result<()> {
    if !matches!(confidence, "likely" | "possible") {
        bail!("semantic confidence must be one of: likely, possible");
    }
    Ok(())
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "likely" => 1,
        _ => 0,
    }
}

fn context_hash(conn: &Connection, anchor: &str) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-semantic-context-v1\0");
    hasher.update(anchor.as_bytes());
    let mut statement = conn.prepare(
        "SELECT e.src_key, e.dst_key, e.kind, e.confidence, e.provenance,
                COALESCE(src_file.hash,''), COALESCE(dst_file.hash,'')
         FROM resolved_edges e
         LEFT JOIN graph_nodes src ON src.node_key=e.src_key
         LEFT JOIN files src_file ON src.file_id=src_file.id
         LEFT JOIN graph_nodes dst ON dst.node_key=e.dst_key
         LEFT JOIN files dst_file ON dst.file_id=dst_file.id
         WHERE e.src_key=?1 OR e.dst_key=?1
         ORDER BY e.src_key, e.dst_key, e.kind, e.confidence, e.provenance, e.id",
    )?;
    let rows = statement.query_map([anchor], |row| {
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
    for row in rows {
        let values = row?;
        for value in [values.0, values.1, values.2, values.3, values.4, values.5, values.6] {
            hasher.update(b"\0");
            hasher.update(value.as_bytes());
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::json;

    use super::{AnnotateInput, SupportInput, annotate, search, support_relationship};
    use crate::{indexer, store, structural};

    fn support(claim_path: &str, anchor: &str, file: &str) -> SupportInput {
        SupportInput {
            claim_path: claim_path.into(),
            anchor: anchor.into(),
            role: None,
            evidence_file: file.into(),
            evidence_start_line: 1,
            evidence_end_line: 1,
            confidence: "likely".into(),
        }
    }

    #[test]
    fn workflow_round_trips_with_evidence_and_degrades_after_supported_source_changes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export function alpha() { return 1; }\n")?;
        fs::write(repo.path().join("b.ts"), "export function beta() { return 2; }\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let beta = "sym:b.ts#::beta@1";
        let input = AnnotateInput {
            artifact_type: "workflow".into(),
            name: Some("handoff workflow".into()),
            body: json!({
                "participants": [
                    { "anchor": alpha, "role": "starts handoff", "scope": "defining" },
                    { "anchor": beta, "role": "finishes handoff", "scope": "supporting" }
                ]
            }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
                support("/participants/1/role", beta, "b.ts"),
            ],
            confidence: "likely".into(),
            snapshot,
            supersedes: None,
        };
        let artifact = annotate(repo.path(), &conn, &input)?;
        assert_eq!(artifact.freshness, "fresh");
        assert_eq!(artifact.trust, "untrusted-semantic-memory");
        assert_eq!(artifact.model, "agent-reported");
        assert_eq!(artifact.prompt_version, "annotate/v2");
        assert_eq!(artifact.supports.len(), 3);
        assert_eq!(artifact.supports[0].relationship, "artifact-name-evidence");
        assert!(artifact.supports.iter().any(|support| {
            support.relationship == "defining-participant-evidence"
        }));
        assert!(artifact.supports.iter().any(|support| {
            support.relationship == "supporting-participant-evidence"
        }));
        assert_eq!(
            support_relationship(
                "workflow",
                &json!({ "participants": [{ "anchor": alpha, "role": "legacy" }] }),
                "/participants/0/role",
            ),
            "legacy-participant-evidence"
        );
        assert_eq!(search(&conn, "handoff", 4)?.len(), 1);

        fs::write(repo.path().join("a.ts"), "export function alpha() { return 3; }\n")?;
        indexer::index_repo(repo.path(), &conn)?;
        let stale = search(&conn, "handoff", 4)?;
        assert_eq!(stale[0].freshness, "degraded");
        assert!(
            stale[0]
                .supports
                .iter()
                .any(|support| support.freshness == "source-stale")
        );
        assert!(
            stale[0]
                .supports
                .iter()
                .any(|support| support.freshness == "fresh")
        );
        Ok(())
    }

    #[test]
    fn workflow_requires_unique_scoped_participants_and_one_defining_boundary() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export function alpha() { return 1; }\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let snapshot = structural::current_snapshot(&conn)?;
        let make_input = |participants| AnnotateInput {
            artifact_type: "workflow".into(),
            name: Some("scoped workflow".into()),
            body: json!({ "participants": participants }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
            ],
            confidence: "likely".into(),
            snapshot: snapshot.clone(),
            supersedes: None,
        };

        let missing_scope = make_input(json!([
            { "anchor": alpha, "role": "entry" }
        ]));
        assert!(
            annotate(repo.path(), &conn, &missing_scope)
                .unwrap_err()
                .to_string()
                .contains("requires scope")
        );

        let only_supporting = make_input(json!([
            { "anchor": alpha, "role": "helper", "scope": "supporting" }
        ]));
        assert!(
            annotate(repo.path(), &conn, &only_supporting)
                .unwrap_err()
                .to_string()
                .contains("at least one defining participant")
        );

        let duplicate = AnnotateInput {
            body: json!({
                "participants": [
                    { "anchor": alpha, "role": "entry", "scope": "defining" },
                    { "anchor": alpha, "role": "helper", "scope": "supporting" }
                ]
            }),
            supports: vec![
                support("/name", alpha, "a.ts"),
                support("/participants/0/role", alpha, "a.ts"),
                support("/participants/1/role", alpha, "a.ts"),
            ],
            ..make_input(json!([]))
        };
        assert!(
            annotate(repo.path(), &conn, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicated")
        );
        Ok(())
    }

    #[test]
    fn annotate_rejects_untrusted_confidence_bad_spans_and_stale_snapshots() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export function alpha() { return 1; }\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let base = AnnotateInput {
            artifact_type: "annotation".into(),
            name: Some("claim".into()),
            body: json!({ "claim": "alpha participates in a workflow" }),
            supports: vec![support("/claim", alpha, "a.ts")],
            confidence: "certain".into(),
            snapshot: structural::current_snapshot(&conn)?,
            supersedes: None,
        };
        assert!(annotate(repo.path(), &conn, &base).unwrap_err().to_string().contains("likely"));

        let bad_span = AnnotateInput {
            confidence: "likely".into(),
            supports: vec![SupportInput {
                evidence_end_line: 99,
                ..support("/claim", alpha, "a.ts")
            }],
            ..base.clone()
        };
        assert!(annotate(repo.path(), &conn, &bad_span).unwrap_err().to_string().contains("line count"));

        let stale_snapshot = AnnotateInput {
            snapshot: "0".repeat(64),
            supports: vec![support("/claim", alpha, "a.ts")],
            ..bad_span
        };
        assert!(annotate(repo.path(), &conn, &stale_snapshot).unwrap_err().to_string().contains("stale"));
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM semantic_artifacts", [], |row| row.get(0))?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[test]
    fn superseding_annotation_hides_prior_record_from_default_search() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export function alpha() { return 1; }\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let snapshot = structural::current_snapshot(&conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let first = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({ "claim": "alpha returns one" }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: snapshot.clone(),
                supersedes: None,
            },
        )?;
        let second = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({ "claim": "alpha is the handoff entry" }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot,
                supersedes: Some(first.id),
            },
        )?;
        let results = search(&conn, "alpha", 4)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, second.id);
        assert_eq!(results[0].supersedes, Some(first.id));
        Ok(())
    }

    #[test]
    fn every_semantic_leaf_claim_requires_evidence_support() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("a.ts"), "export function alpha() { return 1; }\n")?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let alpha = "sym:a.ts#::alpha@1";
        let result = annotate(
            repo.path(),
            &conn,
            &AnnotateInput {
                artifact_type: "annotation".into(),
                name: Some("alpha behavior".into()),
                body: json!({
                    "claim": "alpha returns one",
                    "unsupported_detail": "alpha also starts the handoff"
                }),
                supports: vec![support("/claim", alpha, "a.ts")],
                confidence: "likely".into(),
                snapshot: structural::current_snapshot(&conn)?,
                supersedes: None,
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("/unsupported_detail")
        );
        Ok(())
    }
}
