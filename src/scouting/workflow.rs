//! Candidate-closed workflow output contract. The model classifies every
//! deterministic candidate exactly once; Rust validation is authoritative and
//! malformed or partial output publishes nothing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::evidence::EvidencePack;
use super::ledger::ClassificationRow;
use crate::semantic::{WorkflowCandidate, WorkflowParticipantInput};

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
                "type": "string",
                "minLength": 3,
                "maxLength": MAX_NAME_CHARS,
                "description": "Short domain name of the workflow"
            },
            "description": {
                "type": "string",
                "minLength": 10,
                "maxLength": MAX_DESCRIPTION_CHARS,
                "description": "What the workflow does, grounded in the evidence"
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
    pub name: String,
    pub description: String,
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
    pub participants: Vec<WorkflowParticipantInput>,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
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

    let name = submission.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        bail!("workflow name must be 1-{MAX_NAME_CHARS} characters");
    }
    let description = submission.description.trim();
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
                    participants.push(WorkflowParticipantInput {
                        anchor: known_candidate.anchor.clone(),
                        role: role.to_string(),
                        scope: candidate.decision.clone(),
                        evidence_file: known_candidate.file.clone(),
                        evidence_start_line: range.start_line,
                        evidence_end_line: range.end_line,
                        // Generated claims never exceed `likely`.
                        confidence: "likely".into(),
                    });
                }
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
