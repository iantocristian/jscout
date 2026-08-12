//! Hierarchical summary output contract. One summary describes one scope
//! (a file, a workspace module, or the repository) strictly bottom-up: every
//! claim must cite the enumerated child artifacts that support it, and those
//! citations become `summarizes` relations pinned to child fingerprints.
//! Prose without a support chain never validates. Rust validation is
//! authoritative regardless of what the schema accepted.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::ledger::ClassificationRow;
use crate::semantic::{AnnotateInput, RelationInput};

pub const PROMPT_VERSION: &str = "summary-scout/v1";
pub const SUBMIT_TOOL_NAME: &str = "submit_scope_summary";

const MAX_OVERVIEW_CHARS: usize = 900;
const MAX_POINT_CHARS: usize = 300;
const MAX_POINTS: usize = 8;
const MAX_REASON_CHARS: usize = 300;
/// Body budget below `semantic::MAX_BODY_BYTES`, checked here so an oversized
/// summary fails as a validation error instead of a publication problem.
const MAX_SUMMARY_BODY_BYTES: usize = 10_000;

/// The deterministic scope of one summary. Identity comes from the index and
/// the workspace manifest set, never from the model.
#[derive(Debug, Clone)]
pub struct SummaryScope {
    /// file | module | repository
    pub level: String,
    /// Canonical name: `file:<path>`, `module:<package>`, or `repo`.
    pub scope_key: String,
    pub display: String,
}

/// One child artifact the model may cite, pinned by fingerprint at planning
/// time. Publication rechecks that every cited child is still current with
/// this exact fingerprint.
#[derive(Debug, Clone)]
pub struct SummaryChild {
    /// Stable reference used in the prompt and the submission: C1, C2, ...
    pub reference: String,
    pub artifact_id: i64,
    pub artifact_type: String,
    pub name: Option<String>,
    pub fingerprint: String,
    pub confidence: String,
    pub body_json: String,
}

/// JSON Schema for the synthetic submit tool. Claims pair text with child
/// citations so the schema itself forbids uncited prose; `overview` is
/// nullable ONLY together with `incomplete_reason`.
pub fn submit_tool_schema(child_references: &[String]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["overview", "key_points", "incomplete_reason"],
        "properties": {
            "overview": {
                "anyOf": [
                    claim_schema(
                        10,
                        MAX_OVERVIEW_CHARS,
                        "What this scope does and why it exists, synthesized from the cited children",
                        child_references,
                    ),
                    { "type": "null" }
                ],
                "description": "The summary's one required claim; null ONLY together \
                                with incomplete_reason"
            },
            "key_points": {
                "type": "array",
                "maxItems": MAX_POINTS,
                "description": "Distinct load-bearing facts about the scope, each cited",
                "items": claim_schema(
                    3,
                    MAX_POINT_CHARS,
                    "One key point",
                    child_references,
                ),
            },
            "incomplete_reason": {
                "type": ["string", "null"],
                "maxLength": MAX_REASON_CHARS,
                "description": "Set ONLY when the child evidence cannot support a summary \
                                of this scope; the run is then recorded incomplete and \
                                nothing is published"
            }
        }
    })
}

fn claim_schema(
    min_chars: usize,
    max_chars: usize,
    description: &str,
    child_references: &[String],
) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["text", "children"],
        "description": description,
        "properties": {
            "text": { "type": "string", "minLength": min_chars, "maxLength": max_chars },
            "children": {
                "type": "array",
                "minItems": 1,
                "description": "References of the child artifacts supporting THIS claim",
                "items": { "type": "string", "enum": child_references }
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    #[serde(default)]
    pub overview: Option<Claim>,
    #[serde(default)]
    pub key_points: Option<Vec<Claim>>,
    #[serde(default)]
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    pub text: String,
    pub children: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedClaim {
    pub text: String,
    /// Indexes into the planned child list.
    pub children: Vec<usize>,
}

/// A fully validated summary plus its ledger row. `incomplete` carries the
/// model's refusal and suppresses publication entirely.
#[derive(Debug, Clone)]
pub struct ValidatedSummary {
    pub scope_key: String,
    pub level: String,
    pub overview: Option<ValidatedClaim>,
    pub key_points: Vec<ValidatedClaim>,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
}

pub fn validate(
    submission: &Submission,
    scope: &SummaryScope,
    children: &[SummaryChild],
) -> Result<ValidatedSummary> {
    if let Some(reason) = &submission.incomplete_reason {
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("incomplete_reason must be a non-empty explanation when set");
        }
        // A refusal and a summary are mutually exclusive: claims alongside an
        // incomplete_reason are contradictory output, not a partial summary.
        if submission.overview.is_some()
            || submission
                .key_points
                .as_deref()
                .is_some_and(|points| !points.is_empty())
        {
            bail!(
                "an incomplete submission must not carry claims; set overview to null \
                 and omit key_points when refusing"
            );
        }
        return Ok(ValidatedSummary {
            scope_key: scope.scope_key.clone(),
            level: scope.level.clone(),
            overview: None,
            key_points: Vec::new(),
            classifications: vec![ClassificationRow {
                anchor_key: scope.scope_key.clone(),
                decision: "excluded".into(),
                role: None,
                evidence_json: serde_json::to_string(&json!({
                    "reason": reason.chars().take(MAX_REASON_CHARS).collect::<String>(),
                }))?,
            }],
            incomplete: Some(reason.chars().take(MAX_REASON_CHARS).collect()),
        });
    }

    let overview = submission
        .overview
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("a summary requires a cited `overview` claim"))?;
    let overview = claim(overview, MAX_OVERVIEW_CHARS, "overview", children)?;

    let submitted_points = submission.key_points.as_deref().unwrap_or_default();
    if submitted_points.len() > MAX_POINTS {
        bail!(
            "`key_points` carries {} claims; at most {MAX_POINTS}",
            submitted_points.len()
        );
    }
    let mut key_points = Vec::with_capacity(submitted_points.len());
    for (index, point) in submitted_points.iter().enumerate() {
        if submitted_points[..index]
            .iter()
            .any(|earlier| earlier.text.trim().eq_ignore_ascii_case(point.text.trim()))
        {
            bail!("`key_points` repeats the claim `{}`", point.text.trim());
        }
        key_points.push(claim(
            point,
            MAX_POINT_CHARS,
            &format!("key_points/{index}"),
            children,
        )?);
    }

    let summary = ValidatedSummary {
        scope_key: scope.scope_key.clone(),
        level: scope.level.clone(),
        overview: Some(overview),
        key_points,
        classifications: vec![ClassificationRow {
            anchor_key: scope.scope_key.clone(),
            decision: "defining".into(),
            role: Some("summary scope".into()),
            evidence_json: serde_json::to_string(&json!({
                "children": children
                    .iter()
                    .map(|child| child.fingerprint.as_str())
                    .collect::<Vec<_>>(),
            }))?,
        }],
        incomplete: None,
    };
    let body_bytes = serde_json::to_vec(&body(&summary))?.len();
    if body_bytes > MAX_SUMMARY_BODY_BYTES {
        bail!("summary body is {body_bytes} bytes, over the {MAX_SUMMARY_BODY_BYTES} byte limit");
    }
    Ok(summary)
}

fn claim(
    claim: &Claim,
    max_chars: usize,
    label: &str,
    children: &[SummaryChild],
) -> Result<ValidatedClaim> {
    let text = claim.text.trim();
    if text.is_empty() {
        bail!("`{label}` claim text must not be empty");
    }
    if text.chars().count() > max_chars {
        bail!("`{label}` claim exceeds {max_chars} characters");
    }
    if claim.children.is_empty() {
        bail!("`{label}` claim requires at least one child citation");
    }
    let mut cited = BTreeSet::new();
    for reference in &claim.children {
        let index = children
            .iter()
            .position(|child| &child.reference == reference)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "`{label}` cites unknown child `{reference}`; \
                     the model cannot add children"
                )
            })?;
        if !cited.insert(index) {
            bail!("`{label}` cites child `{reference}` more than once");
        }
    }
    Ok(ValidatedClaim {
        text: text.to_string(),
        children: cited.into_iter().collect(),
    })
}

/// The stored body carries the level, scope, and claim texts; child identity
/// lives in `semantic_relations`, never restated as prose.
fn body(summary: &ValidatedSummary) -> Value {
    let mut body = Map::new();
    body.insert("level".into(), Value::String(summary.level.clone()));
    body.insert("scope".into(), Value::String(summary.scope_key.clone()));
    if let Some(overview) = &summary.overview {
        body.insert("overview".into(), Value::String(overview.text.clone()));
    }
    if !summary.key_points.is_empty() {
        body.insert(
            "key_points".into(),
            Value::Array(
                summary
                    .key_points
                    .iter()
                    .map(|point| Value::String(point.text.clone()))
                    .collect(),
            ),
        );
    }
    Value::Object(body)
}

/// The validated artifact input plus one `summarizes` relation per cited
/// child per claim, pinned to the fingerprints recorded at planning.
pub fn annotate_input(
    summary: &ValidatedSummary,
    children: &[SummaryChild],
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<(AnnotateInput, Vec<RelationInput>)> {
    if summary.overview.is_none() {
        bail!("an incomplete summary must not be published");
    }
    // A generated summary cannot be more certain than any model-visible
    // child it synthesized. The same cap applies to claim citations and
    // whole-input dependencies so their confidence remains internally
    // consistent with the artifact.
    let confidence = if children.iter().any(|child| child.confidence == "possible") {
        "possible"
    } else {
        "likely"
    };
    let mut relations = Vec::new();
    let mut push = |claim_path: &str, claim: &ValidatedClaim| {
        for &child_index in &claim.children {
            let child = &children[child_index];
            relations.push(RelationInput {
                claim_path: claim_path.to_string(),
                relation: "summarizes".into(),
                dst_artifact_id: child.artifact_id,
                dst_fingerprint: child.fingerprint.clone(),
                confidence: confidence.into(),
            });
        }
    };
    if let Some(overview) = &summary.overview {
        push("/overview", overview);
    }
    for (index, point) in summary.key_points.iter().enumerate() {
        push(&format!("/key_points/{index}"), point);
    }
    // Every planned child is an input dependency regardless of citation: the
    // model saw it and chose what to keep, so its later change must stale the
    // summary and block publication like any cited child. The empty claim
    // path marks whole-artifact dependencies apart from claim supports.
    for child in children {
        relations.push(RelationInput {
            claim_path: String::new(),
            relation: "summarizes".into(),
            dst_artifact_id: child.artifact_id,
            dst_fingerprint: child.fingerprint.clone(),
            confidence: confidence.into(),
        });
    }
    Ok((
        AnnotateInput {
            artifact_type: "summary".into(),
            name: Some(summary.scope_key.clone()),
            body: body(summary),
            supports: Vec::new(),
            confidence: confidence.into(),
            snapshot,
            supersedes,
        },
        relations,
    ))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::json;

    use super::{Submission, SummaryChild, SummaryScope, validate};

    fn scope() -> SummaryScope {
        SummaryScope {
            level: "file".into(),
            scope_key: "file:src/flow.ts".into(),
            display: "src/flow.ts".into(),
        }
    }

    fn children() -> Vec<SummaryChild> {
        vec![
            SummaryChild {
                reference: "C1".into(),
                artifact_id: 11,
                artifact_type: "card".into(),
                name: Some("sym:src/flow.ts#::start@1".into()),
                fingerprint: "fp-card".into(),
                confidence: "likely".into(),
                body_json: r#"{"purpose":"starts the flow"}"#.into(),
            },
            SummaryChild {
                reference: "C2".into(),
                artifact_id: 12,
                artifact_type: "workflow".into(),
                name: Some("settlement".into()),
                fingerprint: "fp-workflow".into(),
                confidence: "likely".into(),
                body_json: r#"{"description":"settles"}"#.into(),
            },
        ]
    }

    fn submission(value: serde_json::Value) -> Submission {
        serde_json::from_value(value).expect("submission shape")
    }

    #[test]
    fn claims_must_cite_known_children_and_become_relations() -> Result<()> {
        let summary = validate(
            &submission(json!({
                "overview": {
                    "text": "orchestrates the settlement flow end to end",
                    "children": ["C1", "C2"],
                },
                "key_points": [
                    { "text": "starts from the exported entry point", "children": ["C1"] },
                ],
                "incomplete_reason": null,
            })),
            &scope(),
            &children(),
        )?;
        assert_eq!(
            summary.overview.as_ref().expect("overview").children,
            [0, 1]
        );
        assert_eq!(summary.key_points.len(), 1);

        let (input, relations) =
            super::annotate_input(&summary, &children(), "snapshot".into(), None)?;
        assert_eq!(input.artifact_type, "summary");
        assert_eq!(input.name.as_deref(), Some("file:src/flow.ts"));
        assert_eq!(
            relations.len(),
            5,
            "one relation per cited child per claim, plus one input dependency per planned child"
        );
        assert_eq!(
            relations
                .iter()
                .filter(|relation| relation.claim_path.is_empty())
                .count(),
            2,
            "every planned child is a whole-artifact input dependency"
        );
        assert!(
            relations.iter().all(
                |relation| relation.relation == "summarizes" && relation.confidence == "likely"
            )
        );
        assert_eq!(relations[0].claim_path, "/overview");
        assert_eq!(relations[2].claim_path, "/key_points/0");
        assert_eq!(relations[2].dst_fingerprint, "fp-card");

        let mut possible_children = children();
        possible_children[1].confidence = "possible".into();
        let (possible_input, possible_relations) =
            super::annotate_input(&summary, &possible_children, "snapshot".into(), None)?;
        assert_eq!(possible_input.confidence, "possible");
        assert!(
            possible_relations
                .iter()
                .all(|relation| relation.confidence == "possible")
        );

        let unknown = validate(
            &submission(json!({
                "overview": { "text": "cites a child that does not exist", "children": ["C9"] },
                "key_points": [],
                "incomplete_reason": null,
            })),
            &scope(),
            &children(),
        )
        .expect_err("unknown child");
        assert!(unknown.to_string().contains("unknown child"));

        let uncited = validate(
            &submission(json!({
                "overview": { "text": "prose without any support chain here", "children": [] },
                "key_points": [],
                "incomplete_reason": null,
            })),
            &scope(),
            &children(),
        )
        .expect_err("uncited claim");
        assert!(uncited.to_string().contains("child citation"));
        Ok(())
    }

    #[test]
    fn refusals_are_exclusive_and_publish_nothing() -> Result<()> {
        let summary = validate(
            &submission(json!({
                "overview": null,
                "key_points": [],
                "incomplete_reason": "child artifacts cover none of this scope",
            })),
            &scope(),
            &children(),
        )?;
        assert!(summary.incomplete.is_some());
        assert_eq!(summary.classifications[0].decision, "excluded");
        assert!(
            super::annotate_input(&summary, &children(), "snapshot".into(), None).is_err(),
            "an incomplete summary must not be published"
        );

        let contradictory = validate(
            &submission(json!({
                "overview": { "text": "still writes a summary anyway", "children": ["C1"] },
                "key_points": [],
                "incomplete_reason": "cannot summarize",
            })),
            &scope(),
            &children(),
        )
        .expect_err("claims beside a refusal");
        assert!(contradictory.to_string().contains("must not carry claims"));
        Ok(())
    }
}
