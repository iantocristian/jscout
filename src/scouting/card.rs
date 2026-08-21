//! Selected symbol card output contract. One card describes one subject
//! anchor; every claim carries its own evidence in the subject's declaring
//! file, and optional fields the model cannot support are omitted rather than
//! filled speculatively. Rust validation is authoritative regardless of what
//! the schema accepted.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::evidence::EvidencePack;
use super::ledger::ClassificationRow;
use super::workflow::LineRange;
use crate::semantic::{AnnotateInput, SupportInput};

pub const PROMPT_VERSION: &str = "card-scout/v1";
pub const SUBMIT_TOOL_NAME: &str = "submit_symbol_card";

const MAX_PURPOSE_CHARS: usize = 600;
const MAX_ROLE_CHARS: usize = 300;
const MAX_TERM_CHARS: usize = 80;
const MAX_CLAIM_CHARS: usize = 240;
const MAX_LIST_ITEMS: usize = 6;
const MAX_RANGES_PER_CLAIM: usize = 4;
const MAX_RANGE_REPAIR_INPUT: usize = 12;
const MAX_REASON_CHARS: usize = 300;
/// Body budget below `semantic::MAX_BODY_BYTES`, checked here so an oversized
/// card fails as a validation error instead of surfacing as a publication
/// problem.
const MAX_CARD_BODY_BYTES: usize = 10_000;

/// The deterministic subject of one card: identity and declaration site come
/// from the index, never from the model.
#[derive(Debug, Clone)]
pub struct CardSubject {
    pub anchor: String,
    pub display_name: String,
    pub file: String,
    pub declaration_start_line: i64,
    pub declaration_end_line: i64,
}

/// JSON Schema for the synthetic submit tool. Claim objects pair text with
/// their own evidence so the schema itself forbids an unsupported claim;
/// optional fields are simply absent when the model has no evidence.
pub fn submit_tool_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["purpose", "incomplete_reason"],
        "properties": {
            "purpose": {
                "anyOf": [
                    claim_schema(
                        "text",
                        10,
                        MAX_PURPOSE_CHARS,
                        "What this symbol is for, in domain terms",
                    ),
                    { "type": "null" }
                ],
                "description": "The card's one required claim; null ONLY together \
                                with incomplete_reason"
            },
            "architectural_role": claim_schema(
                "text",
                3,
                MAX_ROLE_CHARS,
                "Where this symbol sits in the system's architecture",
            ),
            "domain_terms": {
                "type": "array",
                "maxItems": MAX_LIST_ITEMS,
                "description": "Domain vocabulary this symbol establishes or relies on",
                "items": claim_schema("term", 2, MAX_TERM_CHARS, "One domain term"),
            },
            "side_effects": {
                "type": "array",
                "maxItems": MAX_LIST_ITEMS,
                "description": "Observable effects outside the returned value",
                "items": claim_schema("text", 3, MAX_CLAIM_CHARS, "One side effect"),
            },
            "invariants": {
                "type": "array",
                "maxItems": MAX_LIST_ITEMS,
                "description": "Conditions this symbol assumes or guarantees",
                "items": claim_schema("text", 3, MAX_CLAIM_CHARS, "One invariant"),
            },
            "failure_modes": {
                "type": "array",
                "maxItems": MAX_LIST_ITEMS,
                "description": "How this symbol fails and what it does about it",
                "items": claim_schema("text", 3, MAX_CLAIM_CHARS, "One failure mode"),
            },
            "incomplete_reason": {
                "type": ["string", "null"],
                "maxLength": MAX_REASON_CHARS,
                "description": "Set ONLY when the evidence cannot support a card for this \
                                subject; the run is then recorded incomplete and nothing \
                                is published"
            }
        }
    })
}

fn claim_schema(text_key: &str, min_chars: usize, max_chars: usize, description: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [text_key, "evidence"],
        "description": description,
        "properties": {
            text_key: { "type": "string", "minLength": min_chars, "maxLength": max_chars },
            "evidence": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_RANGES_PER_CLAIM,
                "description": "Line ranges in the subject's numbered source supporting THIS claim",
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
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    #[serde(default)]
    pub purpose: Option<Claim>,
    #[serde(default)]
    pub architectural_role: Option<Claim>,
    #[serde(default)]
    pub domain_terms: Option<Vec<TermClaim>>,
    #[serde(default)]
    pub side_effects: Option<Vec<Claim>>,
    #[serde(default)]
    pub invariants: Option<Vec<Claim>>,
    #[serde(default)]
    pub failure_modes: Option<Vec<Claim>>,
    #[serde(default)]
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    pub text: String,
    pub evidence: Vec<LineRange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TermClaim {
    pub term: String,
    pub evidence: Vec<LineRange>,
}

#[derive(Debug, Clone)]
pub struct ValidatedClaim {
    pub text: String,
    pub evidence: Vec<LineRange>,
}

/// A fully validated card plus the ledger row for its subject. `incomplete`
/// carries the model's refusal and suppresses publication entirely.
#[derive(Debug, Clone)]
pub struct ValidatedCard {
    pub anchor: String,
    pub file: String,
    pub purpose: Option<ValidatedClaim>,
    pub architectural_role: Option<ValidatedClaim>,
    pub domain_terms: Vec<ValidatedClaim>,
    pub side_effects: Vec<ValidatedClaim>,
    pub invariants: Vec<ValidatedClaim>,
    pub failure_modes: Vec<ValidatedClaim>,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
}

impl ValidatedCard {
    /// Every claim in publication order, paired with its JSON pointer.
    fn claims(&self) -> Vec<(String, &ValidatedClaim)> {
        let mut claims = Vec::new();
        if let Some(purpose) = &self.purpose {
            claims.push(("/purpose".to_string(), purpose));
        }
        if let Some(role) = &self.architectural_role {
            claims.push(("/architectural_role".to_string(), role));
        }
        for (field, list) in [
            ("domain_terms", &self.domain_terms),
            ("side_effects", &self.side_effects),
            ("invariants", &self.invariants),
            ("failure_modes", &self.failure_modes),
        ] {
            for (index, claim) in list.iter().enumerate() {
                claims.push((format!("/{field}/{index}"), claim));
            }
        }
        claims
    }
}

pub fn validate(
    submission: &Submission,
    subject: &CardSubject,
    evidence: &EvidencePack,
) -> Result<ValidatedCard> {
    if let Some(reason) = &submission.incomplete_reason {
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("incomplete_reason must be a non-empty explanation when set");
        }
        // A refusal and a card are mutually exclusive: claims alongside an
        // incomplete_reason are contradictory output, not a partial card.
        if submission.purpose.is_some()
            || submission.architectural_role.is_some()
            || submission
                .domain_terms
                .as_deref()
                .is_some_and(|terms| !terms.is_empty())
            || submission
                .side_effects
                .as_deref()
                .is_some_and(|claims| !claims.is_empty())
            || submission
                .invariants
                .as_deref()
                .is_some_and(|claims| !claims.is_empty())
            || submission
                .failure_modes
                .as_deref()
                .is_some_and(|claims| !claims.is_empty())
        {
            bail!(
                "an incomplete submission must not carry claims; set purpose to null \
                 and omit every optional field when refusing"
            );
        }
        return Ok(ValidatedCard {
            anchor: subject.anchor.clone(),
            file: subject.file.clone(),
            purpose: None,
            architectural_role: None,
            domain_terms: Vec::new(),
            side_effects: Vec::new(),
            invariants: Vec::new(),
            failure_modes: Vec::new(),
            classifications: vec![ClassificationRow {
                anchor_key: subject.anchor.clone(),
                decision: "excluded".into(),
                role: None,
                evidence_json: serde_json::to_string(&json!({
                    "reason": reason.chars().take(MAX_REASON_CHARS).collect::<String>(),
                }))?,
            }],
            incomplete: Some(reason.chars().take(MAX_REASON_CHARS).collect()),
        });
    }

    let line_count = evidence
        .files
        .get(&subject.file)
        .map(|file| file.line_count)
        .with_context(|| {
            format!(
                "subject file `{}` is missing from the evidence pack",
                subject.file
            )
        })?;
    let check = |claim: &ValidatedClaim, label: &str| -> Result<()> {
        for range in &claim.evidence {
            if range.start_line < 1
                || range.end_line < range.start_line
                || range.end_line > line_count
            {
                bail!(
                    "evidence range {}-{} for {label} is outside `{}` (1-{line_count})",
                    range.start_line,
                    range.end_line,
                    subject.file,
                );
            }
        }
        Ok(())
    };

    let purpose = submission
        .purpose
        .as_ref()
        .context("a card requires a supported `purpose` claim")?;
    let purpose = claim(
        &purpose.text,
        &purpose.evidence,
        MAX_PURPOSE_CHARS,
        "purpose",
    )?;
    check(&purpose, "purpose")?;
    let purpose = repair_claim(purpose);

    let architectural_role = submission
        .architectural_role
        .as_ref()
        .map(|role| {
            claim(
                &role.text,
                &role.evidence,
                MAX_ROLE_CHARS,
                "architectural_role",
            )
        })
        .transpose()?;
    if let Some(role) = &architectural_role {
        check(role, "architectural_role")?;
    }
    let architectural_role = architectural_role.map(repair_claim);

    let domain_terms = submission.domain_terms.as_deref().unwrap_or_default();
    let domain_terms: Vec<ValidatedClaim> = domain_terms
        .iter()
        .map(|term| claim(&term.term, &term.evidence, MAX_TERM_CHARS, "domain_terms"))
        .collect::<Result<_>>()?;
    let mut lists = Vec::new();
    for (field, submitted) in [
        ("side_effects", &submission.side_effects),
        ("invariants", &submission.invariants),
        ("failure_modes", &submission.failure_modes),
    ] {
        let items = submitted.as_deref().unwrap_or_default();
        lists.push(
            items
                .iter()
                .map(|item| claim(&item.text, &item.evidence, MAX_CLAIM_CHARS, field))
                .collect::<Result<Vec<_>>>()?,
        );
    }
    let mut lists = lists.into_iter();
    let mut card = ValidatedCard {
        anchor: subject.anchor.clone(),
        file: subject.file.clone(),
        purpose: Some(purpose.clone()),
        architectural_role,
        domain_terms,
        side_effects: lists.next().unwrap_or_default(),
        invariants: lists.next().unwrap_or_default(),
        failure_modes: lists.next().unwrap_or_default(),
        classifications: vec![ClassificationRow {
            anchor_key: subject.anchor.clone(),
            decision: "defining".into(),
            role: Some("card subject".into()),
            evidence_json: serde_json::to_string(&json!({ "ranges": purpose.evidence }))?,
        }],
        incomplete: None,
    };

    for (field, list) in [
        ("domain_terms", &card.domain_terms),
        ("side_effects", &card.side_effects),
        ("invariants", &card.invariants),
        ("failure_modes", &card.failure_modes),
    ] {
        if list.len() > MAX_LIST_ITEMS {
            bail!(
                "`{field}` carries {} claims; at most {MAX_LIST_ITEMS}",
                list.len()
            );
        }
        for (index, item) in list.iter().enumerate() {
            check(item, &format!("{field}/{index}"))?;
            if list[..index]
                .iter()
                .any(|earlier| earlier.text.eq_ignore_ascii_case(&item.text))
            {
                bail!("`{field}` repeats the claim `{}`", item.text);
            }
        }
    }

    // Every submitted range has now been validated; apply the bounded repair.
    card.domain_terms = card.domain_terms.into_iter().map(repair_claim).collect();
    card.side_effects = card.side_effects.into_iter().map(repair_claim).collect();
    card.invariants = card.invariants.into_iter().map(repair_claim).collect();
    card.failure_modes = card.failure_modes.into_iter().map(repair_claim).collect();

    let body_bytes = serde_json::to_vec(&body(&card))?.len();
    if body_bytes > MAX_CARD_BODY_BYTES {
        bail!("card body is {body_bytes} bytes, over the {MAX_CARD_BODY_BYTES} byte limit");
    }
    Ok(card)
}

/// Applied only after every submitted range has passed the source-file
/// check: retain the first `MAX_RANGES_PER_CLAIM` valid ranges.
fn repair_claim(mut claim: ValidatedClaim) -> ValidatedClaim {
    claim.evidence.truncate(MAX_RANGES_PER_CLAIM);
    claim
}

fn claim(
    text: &str,
    evidence: &[LineRange],
    max_chars: usize,
    label: &str,
) -> Result<ValidatedClaim> {
    let text = text.trim();
    if text.is_empty() {
        bail!("`{label}` claim text must not be empty");
    }
    if text.chars().count() > max_chars {
        bail!("`{label}` claim exceeds {max_chars} characters");
    }
    if evidence.is_empty() {
        bail!("`{label}` claim requires at least one evidence range");
    }
    if evidence.len() > MAX_RANGE_REPAIR_INPUT {
        bail!(
            "`{label}` claim cites {} evidence ranges; over the {MAX_RANGE_REPAIR_INPUT}-range repair bound",
            evidence.len()
        );
    }
    // Deduplicate in model order; the full deduplicated list is validated
    // against the source file before any repair. Truncation to
    // MAX_RANGES_PER_CLAIM happens in `repair_claim` only after that check,
    // so an invalid over-cap range still fails the card rather than being
    // silently dropped — the repository scout's repair boundary.
    let mut seen = std::collections::BTreeSet::new();
    let evidence = evidence
        .iter()
        .filter(|range| seen.insert((range.start_line, range.end_line)))
        .cloned()
        .collect::<Vec<_>>();
    Ok(ValidatedClaim {
        text: text.to_string(),
        evidence,
    })
}

/// Optional fields the model omitted stay out of the body entirely: an empty
/// list would otherwise become a claim path with nothing to support it.
fn body(card: &ValidatedCard) -> Value {
    let mut body = Map::new();
    if let Some(purpose) = &card.purpose {
        body.insert("purpose".into(), Value::String(purpose.text.clone()));
    }
    if let Some(role) = &card.architectural_role {
        body.insert(
            "architectural_role".into(),
            Value::String(role.text.clone()),
        );
    }
    for (field, list) in [
        ("domain_terms", &card.domain_terms),
        ("side_effects", &card.side_effects),
        ("invariants", &card.invariants),
        ("failure_modes", &card.failure_modes),
    ] {
        if list.is_empty() {
            continue;
        }
        body.insert(
            field.into(),
            Value::Array(
                list.iter()
                    .map(|claim| Value::String(claim.text.clone()))
                    .collect(),
            ),
        );
    }
    Value::Object(body)
}

/// Convert a validated card into the generic semantic write shape: one
/// support per claim per evidence range, all anchored on the card subject.
pub fn annotate_input(
    card: &ValidatedCard,
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<AnnotateInput> {
    if card.purpose.is_none() {
        bail!("an incomplete card must not be published");
    }
    let mut supports = Vec::new();
    for (claim_path, claim) in card.claims() {
        for range in &claim.evidence {
            supports.push(SupportInput {
                claim_path: claim_path.clone(),
                anchor: card.anchor.clone(),
                role: None,
                evidence_file: card.file.clone(),
                evidence_start_line: range.start_line,
                evidence_end_line: range.end_line,
                // Generated claims never exceed `likely`.
                confidence: "likely".into(),
            });
        }
    }
    Ok(AnnotateInput {
        artifact_type: "card".into(),
        name: Some(card.anchor.clone()),
        body: body(card),
        supports,
        confidence: "likely".into(),
        snapshot,
        supersedes,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Result;
    use serde_json::json;

    use super::{CardSubject, Submission, validate};
    use crate::scouting::evidence::{EvidencePack, FileEvidence};

    fn pack() -> EvidencePack {
        let mut files = BTreeMap::new();
        files.insert(
            "card.ts".to_string(),
            FileEvidence {
                hash: "hash".into(),
                line_count: 20,
            },
        );
        EvidencePack {
            rendered: String::new(),
            files,
        }
    }

    fn subject() -> CardSubject {
        CardSubject {
            anchor: "sym:card.ts#::charge@1".into(),
            display_name: "charge".into(),
            file: "card.ts".into(),
            declaration_start_line: 1,
            declaration_end_line: 8,
        }
    }

    fn submission(value: serde_json::Value) -> Submission {
        serde_json::from_value(value).expect("submission shape")
    }

    #[test]
    fn valid_excess_citations_are_repaired_to_the_claim_cap() -> Result<()> {
        let card = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [
                        {"start_line": 1, "end_line": 2},
                        {"start_line": 3, "end_line": 4},
                        {"start_line": 5, "end_line": 6},
                        {"start_line": 7, "end_line": 8},
                        {"start_line": 9, "end_line": 10},
                    ],
                },
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )?;
        let purpose = card.purpose.expect("purpose claim");
        assert_eq!(purpose.evidence.len(), 4);
        assert_eq!(purpose.evidence[0].start_line, 1);
        assert_eq!(purpose.evidence[3].end_line, 8);
        Ok(())
    }

    #[test]
    fn invalid_excess_citation_still_fails_the_card() {
        let error = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [
                        {"start_line": 1, "end_line": 2},
                        {"start_line": 3, "end_line": 4},
                        {"start_line": 5, "end_line": 6},
                        {"start_line": 7, "end_line": 8},
                        {"start_line": 19, "end_line": 99},
                    ],
                },
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )
        .expect_err("out-of-file fifth range must fail, not be dropped");
        assert!(error.to_string().contains("outside"), "{error}");
    }

    #[test]
    fn citations_beyond_the_repair_bound_fail() {
        let ranges = (1..=13)
            .map(|line| json!({"start_line": line, "end_line": line}))
            .collect::<Vec<_>>();
        let error = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": ranges,
                },
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )
        .expect_err("13 ranges exceed the repair bound");
        assert!(error.to_string().contains("repair bound"), "{error}");
    }

    #[test]
    fn omitted_optional_fields_stay_out_of_the_body() -> Result<()> {
        let card = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [{"start_line": 1, "end_line": 8}],
                },
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )?;
        let input = super::annotate_input(&card, "snapshot".into(), None)?;
        assert_eq!(input.artifact_type, "card");
        assert_eq!(input.name.as_deref(), Some("sym:card.ts#::charge@1"));
        assert_eq!(input.body.as_object().expect("body object").len(), 1);
        assert_eq!(input.supports.len(), 1);
        assert_eq!(input.supports[0].claim_path, "/purpose");
        assert_eq!(input.supports[0].confidence, "likely");
        Ok(())
    }

    #[test]
    fn every_present_claim_carries_its_own_bounded_evidence() -> Result<()> {
        let card = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [{"start_line": 1, "end_line": 8}],
                },
                "side_effects": [
                    {"text": "writes an audit row", "evidence": [
                        {"start_line": 3, "end_line": 3},
                        {"start_line": 5, "end_line": 6},
                    ]},
                ],
                "domain_terms": [{"term": "settlement", "evidence": [{"start_line": 2, "end_line": 2}]}],
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )?;
        let input = super::annotate_input(&card, "snapshot".into(), None)?;
        let paths: Vec<&str> = input
            .supports
            .iter()
            .map(|support| support.claim_path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "/purpose",
                "/domain_terms/0",
                "/side_effects/0",
                "/side_effects/0"
            ]
        );
        assert_eq!(input.body["side_effects"][0], json!("writes an audit row"));

        // Out-of-bounds ranges are rejected against the pack's line count.
        let error = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [{"start_line": 1, "end_line": 40}],
                },
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )
        .expect_err("out-of-bounds evidence");
        assert!(error.to_string().contains("outside `card.ts` (1-20)"));

        // A claim without evidence is a contract violation, not a soft warning.
        let error = validate(
            &submission(json!({
                "purpose": {
                    "text": "charges the customer for a settled order",
                    "evidence": [{"start_line": 1, "end_line": 8}],
                },
                "invariants": [{"text": "orders are settled once", "evidence": []}],
                "incomplete_reason": null,
            })),
            &subject(),
            &pack(),
        )
        .expect_err("unsupported claim");
        assert!(
            error
                .to_string()
                .contains("requires at least one evidence range")
        );
        Ok(())
    }

    #[test]
    fn incomplete_submissions_publish_nothing() -> Result<()> {
        // The honest refusal shape: purpose is null, no fabricated claim.
        let card = validate(
            &submission(json!({
                "purpose": null,
                "incomplete_reason": "the dispatch target is generated at runtime",
            })),
            &subject(),
            &pack(),
        )?;
        assert!(card.purpose.is_none());
        assert_eq!(card.classifications.len(), 1);
        assert_eq!(card.classifications[0].decision, "excluded");
        assert!(super::annotate_input(&card, "snapshot".into(), None).is_err());
        Ok(())
    }

    #[test]
    fn a_refusal_carrying_claims_is_contradictory_output() -> Result<()> {
        let error = validate(
            &submission(json!({
                "purpose": {
                    "text": "cannot be determined from this file alone",
                    "evidence": [{"start_line": 1, "end_line": 1}],
                },
                "incomplete_reason": "the dispatch target is generated at runtime",
            })),
            &subject(),
            &pack(),
        )
        .expect_err("claims beside a refusal must be rejected");
        assert!(error.to_string().contains("must not carry claims"));
        Ok(())
    }
}
