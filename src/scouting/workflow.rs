//! Candidate-closed workflow output contract. The model classifies every
//! deterministic candidate exactly once; Rust validation is authoritative and
//! malformed or partial output publishes nothing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::evidence::EvidencePack;
use super::ledger::ClassificationRow;
use crate::semantic::{AnnotateInput, SupportInput, WorkflowCandidate};

pub const PROMPT_VERSION: &str = "workflow-scout/v1";
pub const SUBMIT_TOOL_NAME: &str = "submit_workflow_classification";

const MAX_NAME_CHARS: usize = 160;
const MAX_DESCRIPTION_CHARS: usize = 2_000;
const MAX_ROLE_CHARS: usize = 300;
const MAX_REASON_CHARS: usize = 300;
const MAX_RANGES_PER_CANDIDATE: usize = 4;

/// JSON Schema for the synthetic submit tool. The conditional shape is in the
/// schema itself (included -> role+evidence, excluded -> reason) because
/// models comply better when the schema forbids the invalid shape; Rust
/// validation remains authoritative regardless.
pub fn submit_tool_schema(anchors: &[String]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "description", "candidates", "incomplete_reason"],
        "properties": {
            "name": {
                "type": ["string", "null"],
                "minLength": 3,
                "maxLength": MAX_NAME_CHARS,
                "description": "Short domain name of the workflow; null ONLY together \
                                with incomplete_reason"
            },
            "description": {
                "type": ["string", "null"],
                "minLength": 10,
                "maxLength": MAX_DESCRIPTION_CHARS,
                "description": "What the workflow does, grounded in the evidence; null \
                                ONLY together with incomplete_reason"
            },
            "candidates": {
                "type": "array",
                "description": "Exactly one entry per listed candidate anchor",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["anchor", "decision", "role", "evidence"],
                            "properties": {
                                "anchor": { "type": "string", "enum": anchors },
                                "decision": { "type": "string", "enum": ["defining", "supporting"] },
                                "role": {
                                    "type": "string",
                                    "minLength": 3,
                                    "maxLength": MAX_ROLE_CHARS,
                                    "description": "Concise role of this participant in the workflow"
                                },
                                "evidence": {
                                    "type": "array",
                                    "minItems": 1,
                                    "maxItems": MAX_RANGES_PER_CANDIDATE,
                                    "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "required": ["start_line", "end_line"],
                                        "properties": {
                                            "start_line": { "type": "integer", "minimum": 1 },
                                            "end_line": { "type": "integer", "minimum": 1 }
                                        }
                                    }
                                }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["anchor", "decision", "reason"],
                            "properties": {
                                "anchor": { "type": "string", "enum": anchors },
                                "decision": { "type": "string", "enum": ["excluded"] },
                                "reason": {
                                    "type": "string",
                                    "minLength": 3,
                                    "maxLength": MAX_REASON_CHARS
                                }
                            }
                        }
                    ]
                }
            },
            "incomplete_reason": {
                "type": ["string", "null"],
                "maxLength": MAX_REASON_CHARS,
                "description": "Set ONLY when a participant outside the candidate list is required; the run is then recorded incomplete and nothing is published"
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    /// Nullable so a refusal never forces the model to fabricate a workflow
    /// identity; the complete path still requires both.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub candidates: Vec<SubmittedCandidate>,
    #[serde(default)]
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedCandidate {
    pub anchor: String,
    pub decision: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub evidence: Option<Vec<LineRange>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize)]
pub struct LineRange {
    pub start_line: i64,
    pub end_line: i64,
}

/// A fully validated submission: participants ready for artifact publication
/// plus one ledger classification per candidate. `incomplete` carries the
/// model's refusal and suppresses publication entirely.
#[derive(Debug, Clone)]
pub struct ValidatedWorkflow {
    pub name: String,
    pub description: String,
    pub participants: Vec<ValidatedParticipant>,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedParticipant {
    pub anchor: String,
    pub role: String,
    pub scope: String,
    pub evidence_file: String,
    pub evidence: Vec<LineRange>,
    pub confidence: String,
}

pub fn validate(
    submission: &Submission,
    candidates: &[WorkflowCandidate],
    evidence: &EvidencePack,
) -> Result<ValidatedWorkflow> {
    if let Some(reason) = &submission.incomplete_reason {
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("incomplete_reason must be a non-empty explanation when set");
        }
        return Ok(ValidatedWorkflow {
            name: String::new(),
            description: String::new(),
            participants: Vec::new(),
            classifications: classification_rows(submission, candidates)?,
            incomplete: Some(reason.chars().take(MAX_REASON_CHARS).collect()),
        });
    }

    let name = submission.name.as_deref().unwrap_or("").trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        bail!("workflow name must be 1-{MAX_NAME_CHARS} characters");
    }
    let description = submission.description.as_deref().unwrap_or("").trim();
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        bail!("workflow description must be 1-{MAX_DESCRIPTION_CHARS} characters");
    }

    let known: BTreeMap<&str, &WorkflowCandidate> = candidates
        .iter()
        .map(|candidate| (candidate.anchor.as_str(), candidate))
        .collect();
    let mut decided: BTreeSet<&str> = BTreeSet::new();
    let mut participants = Vec::new();
    let mut defining = 0_usize;

    for candidate in &submission.candidates {
        let Some(known_candidate) = known.get(candidate.anchor.as_str()) else {
            bail!(
                "candidate `{}` is not in the deterministic candidate set; \
                 the model cannot add anchors",
                candidate.anchor
            );
        };
        if !decided.insert(known_candidate.anchor.as_str()) {
            bail!(
                "candidate `{}` was classified more than once",
                candidate.anchor
            );
        }
        match candidate.decision.as_str() {
            "defining" | "supporting" => {
                if candidate.decision == "defining" {
                    defining += 1;
                }
                let Some(role) = candidate
                    .role
                    .as_deref()
                    .map(str::trim)
                    .filter(|role| !role.is_empty())
                else {
                    bail!(
                        "included candidate `{}` requires a concise role",
                        candidate.anchor
                    );
                };
                if role.chars().count() > MAX_ROLE_CHARS {
                    bail!(
                        "role for `{}` exceeds {MAX_ROLE_CHARS} characters",
                        candidate.anchor
                    );
                }
                let Some(ranges) = candidate
                    .evidence
                    .as_deref()
                    .filter(|ranges| !ranges.is_empty())
                else {
                    bail!(
                        "included candidate `{}` requires at least one evidence range",
                        candidate.anchor
                    );
                };
                if ranges.len() > MAX_RANGES_PER_CANDIDATE {
                    bail!(
                        "candidate `{}` cites {} evidence ranges; at most {MAX_RANGES_PER_CANDIDATE}",
                        candidate.anchor,
                        ranges.len()
                    );
                }
                let file = evidence.files.get(&known_candidate.file).ok_or_else(|| {
                    anyhow::anyhow!(
                        "candidate file `{}` is missing from the evidence pack",
                        known_candidate.file
                    )
                })?;
                for range in ranges {
                    if range.start_line < 1
                        || range.end_line < range.start_line
                        || range.end_line > file.line_count
                    {
                        bail!(
                            "evidence range {}-{} for `{}` is outside `{}` (1-{})",
                            range.start_line,
                            range.end_line,
                            candidate.anchor,
                            known_candidate.file,
                            file.line_count
                        );
                    }
                }
                participants.push(ValidatedParticipant {
                    anchor: known_candidate.anchor.clone(),
                    role: role.to_string(),
                    scope: candidate.decision.clone(),
                    evidence_file: known_candidate.file.clone(),
                    evidence: ranges.to_vec(),
                    // Generated claims never exceed `likely`.
                    confidence: "likely".into(),
                });
            }
            "excluded" => {
                let reason = candidate
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty());
                if reason.is_none() {
                    bail!(
                        "excluded candidate `{}` requires a reason",
                        candidate.anchor
                    );
                }
                if candidate.role.is_some() || candidate.evidence.is_some() {
                    bail!(
                        "excluded candidate `{}` must not carry a role or evidence",
                        candidate.anchor
                    );
                }
            }
            other => bail!(
                "candidate `{}` has unknown decision `{other}`; \
                 expected defining, supporting, or excluded",
                candidate.anchor
            ),
        }
    }

    if decided.len() != known.len() {
        let missing: Vec<&str> = known
            .keys()
            .filter(|anchor| !decided.contains(**anchor))
            .copied()
            .collect();
        bail!(
            "candidate closure violated: {} of {} candidates unclassified ({})",
            missing.len(),
            known.len(),
            missing.join(", ")
        );
    }
    if defining == 0 {
        bail!("a workflow requires at least one defining participant");
    }

    Ok(ValidatedWorkflow {
        name: name.to_string(),
        description: description.to_string(),
        participants,
        classifications: classification_rows(submission, candidates)?,
        incomplete: None,
    })
}

/// Convert a validated model submission into the generic semantic write
/// shape. Participants and supports are intentionally separate: one workflow
/// participant may cite several evidence spans without becoming several body
/// entries.
pub fn annotate_input(
    workflow: &ValidatedWorkflow,
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<AnnotateInput> {
    let body_participants: Vec<Value> = workflow
        .participants
        .iter()
        .map(|participant| {
            json!({
                "anchor": participant.anchor,
                "role": participant.role,
                "scope": participant.scope,
            })
        })
        .collect();
    let support =
        |claim_path: String, participant: &ValidatedParticipant, range: &LineRange| SupportInput {
            claim_path,
            anchor: participant.anchor.clone(),
            role: None,
            evidence_file: participant.evidence_file.clone(),
            evidence_start_line: range.start_line,
            evidence_end_line: range.end_line,
            confidence: participant.confidence.clone(),
        };

    let defining: Vec<&ValidatedParticipant> = workflow
        .participants
        .iter()
        .filter(|participant| participant.scope == "defining")
        .collect();
    let first_defining = defining
        .first()
        .context("validated workflow requires a defining participant")?;
    let first_range = first_defining
        .evidence
        .first()
        .context("validated workflow participant requires evidence")?;
    let mut supports = Vec::new();
    supports.push(support("/name".into(), first_defining, first_range));
    // A workflow description summarizes its defining boundary. Ground it in
    // every defining stage rather than arbitrarily assigning it to the first.
    for participant in defining {
        let range = participant
            .evidence
            .first()
            .context("validated workflow participant requires evidence")?;
        supports.push(support("/description".into(), participant, range));
    }
    for (index, participant) in workflow.participants.iter().enumerate() {
        for range in &participant.evidence {
            supports.push(support(
                format!("/participants/{index}/role"),
                participant,
                range,
            ));
        }
    }

    Ok(AnnotateInput {
        artifact_type: "workflow".into(),
        name: Some(workflow.name.clone()),
        body: json!({
            "description": workflow.description,
            "participants": body_participants,
        }),
        supports,
        confidence: "likely".into(),
        snapshot,
        supersedes,
    })
}

/// Ledger rows for every candidate decision, including exclusions.
fn classification_rows(
    submission: &Submission,
    candidates: &[WorkflowCandidate],
) -> Result<Vec<ClassificationRow>> {
    let known: BTreeSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.anchor.as_str())
        .collect();
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in &submission.candidates {
        if !known.contains(candidate.anchor.as_str()) || !seen.insert(candidate.anchor.as_str()) {
            continue;
        }
        let evidence_json = if candidate.decision == "excluded" {
            json!({
                "reason": candidate
                    .reason
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(MAX_REASON_CHARS)
                    .collect::<String>()
            })
        } else {
            json!({ "ranges": candidate.evidence.clone().unwrap_or_default() })
        };
        rows.push(ClassificationRow {
            anchor_key: candidate.anchor.clone(),
            decision: candidate.decision.clone(),
            role: candidate.role.clone(),
            evidence_json: serde_json::to_string(&evidence_json)?,
        });
    }
    Ok(rows)
}
