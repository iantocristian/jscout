use anyhow::Result;
use serde_json::{Value, json};

use super::{
    ConceptSource, ConceptSourceAlias, SourceSupport, Submission, normalize, submit_tool_schema,
    validate,
};

fn support(anchor: &str, file: &str, line: i64, confidence: &str) -> SourceSupport {
    SourceSupport {
        anchor: anchor.into(),
        role: Some("source vocabulary".into()),
        evidence_file: file.into(),
        evidence_start_line: line,
        evidence_end_line: line + 1,
        confidence: confidence.into(),
    }
}

fn sources() -> Vec<ConceptSource> {
    vec![
        ConceptSource {
            reference: "S1".into(),
            artifact_id: 11,
            artifact_type: "workflow".into(),
            name: Some("Invoice Reconciliation".into()),
            fingerprint: "fp-workflow".into(),
            confidence: "likely".into(),
            body_json: json!({
                "description": "settles invoice state across the payment boundary",
                "participants": [],
            })
            .to_string(),
            aliases: vec![ConceptSourceAlias {
                text: "Invoice Reconciliation".into(),
                claim_path: "/name".into(),
                supports: vec![support(
                    "sym:workflow.ts#::reconcile@1",
                    "workflow.ts",
                    4,
                    "likely",
                )],
            }],
        },
        ConceptSource {
            reference: "S2".into(),
            artifact_id: 12,
            artifact_type: "card".into(),
            name: Some("sym:invoice.ts#::apply@1".into()),
            fingerprint: "fp-card".into(),
            confidence: "likely".into(),
            body_json: json!({
                "purpose": "applies settlement state to one invoice",
                "domain_terms": ["invoice reconciliation"],
            })
            .to_string(),
            aliases: vec![ConceptSourceAlias {
                text: "invoice reconciliation".into(),
                claim_path: "/domain_terms/0".into(),
                supports: vec![support(
                    "sym:invoice.ts#::apply@1",
                    "invoice.ts",
                    8,
                    "likely",
                )],
            }],
        },
    ]
}

fn submission(mut value: Value) -> Submission {
    // Most tests focus on a different validation boundary. Supply the
    // otherwise mandatory exhaustive candidate decisions from each
    // claim's citations so those tests stay narrow.
    let is_complete = value
        .get("incomplete_reason")
        .is_some_and(|reason| reason.is_null());
    if is_complete && value.get("candidates").is_none() {
        let mut by_source = std::collections::BTreeMap::<String, Vec<String>>::new();
        if let Some(sources) = value
            .get("definition")
            .and_then(|definition| definition.get("sources"))
            .and_then(Value::as_array)
        {
            for source in sources.iter().filter_map(Value::as_str) {
                by_source
                    .entry(source.to_string())
                    .or_default()
                    .push("/definition".into());
            }
        }
        if let Some(aliases) = value.get("aliases").and_then(Value::as_array) {
            for (index, alias) in aliases.iter().enumerate() {
                if let Some(sources) = alias.get("sources").and_then(Value::as_array) {
                    for source in sources.iter().filter_map(Value::as_str) {
                        by_source
                            .entry(source.to_string())
                            .or_default()
                            .push(format!("/aliases/{index}"));
                    }
                }
            }
        }
        let candidates = ["S1", "S2"]
            .into_iter()
            .map(|source| match by_source.remove(source) {
                Some(claims) => json!({
                    "source": source,
                    "decision": "included",
                    "claims": claims,
                }),
                None => json!({
                    "source": source,
                    "decision": "excluded",
                    "reason": "not cited by the submitted concept claims",
                }),
            })
            .collect();
        value
            .as_object_mut()
            .expect("test submission object")
            .insert("candidates".into(), Value::Array(candidates));
    }
    serde_json::from_value(value).expect("submission shape")
}

#[test]
fn normalizer_is_nfkc_lowercase_whitespace_stable_and_keeps_punctuation() {
    assert_eq!(
        normalize("  Ｉnvoice\tReconciliation  "),
        "invoice reconciliation"
    );
    assert_eq!(normalize("Straße"), "straße");
    assert_ne!(normalize("invoice-id"), normalize("invoice id"));
}

#[test]
fn schema_closes_claims_and_enumerates_sources() {
    let schema = submit_tool_schema(&["S1".into(), "S2".into()]);
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(
        schema["properties"]["definition"]["anyOf"][0]["properties"]["sources"]["items"]["enum"],
        json!(["S1", "S2"])
    );
    assert_eq!(
        schema["properties"]["aliases"]["items"]["additionalProperties"],
        json!(false)
    );
    assert_eq!(
        schema["required"],
        json!(["definition", "aliases", "candidates", "incomplete_reason"])
    );
    assert_eq!(
        schema["properties"]["candidates"]["items"]["oneOf"][0]["properties"]["source"]["enum"],
        json!(["S1", "S2"])
    );
}

#[test]
fn validated_claims_link_to_fingerprinted_children_without_copying_supports() -> Result<()> {
    let planned = sources();
    let concept = validate(
        &submission(json!({
            "definition": {
                "text": "  Reconciles invoice state against external settlement records. ",
                "sources": ["S1", "S2"]
            },
            "aliases": [
                {"text": "Invoice Reconciliation", "sources": ["S1"]},
                {"text": "invoice reconciliation", "sources": ["S2"]}
            ],
            "incomplete_reason": null
        })),
        &planned,
    )?;
    assert_eq!(concept.canonical_name, "invoice reconciliation");
    assert_eq!(
        concept.definition.as_ref().expect("definition").text,
        "Reconciles invoice state against external settlement records."
    );

    let (input, relations) = super::annotate_input(&concept, &planned, "snapshot".into(), None)?;
    assert_eq!(input.artifact_type, "concept");
    assert_eq!(input.name.as_deref(), Some("invoice reconciliation"));
    assert_eq!(
        input.body,
        json!({
            "definition": "Reconciles invoice state against external settlement records.",
            "aliases": ["Invoice Reconciliation", "invoice reconciliation"]
        })
    );
    assert!(
        input.supports.is_empty(),
        "concept provenance stays on child artifacts"
    );

    assert_eq!(relations.len(), 6);
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.claim_path.is_empty())
            .count(),
        2,
        "every model-visible source is an input dependency"
    );
    assert!(relations.iter().all(|relation| {
        relation.relation == "related_to"
            && relation.confidence == "likely"
            && !relation.dst_fingerprint.is_empty()
    }));
    assert_eq!(concept.classifications[0].anchor_key, "S1");
    assert!(
        concept.classifications[1]
            .evidence_json
            .contains("/aliases/1")
    );
    Ok(())
}

#[test]
fn possible_source_support_caps_the_whole_generated_concept() -> Result<()> {
    let mut planned = sources();
    planned[1].aliases[0].supports[0].confidence = "possible".into();
    let concept = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S2"]},
            "aliases": [
                {"text": "Invoice Reconciliation", "sources": ["S1"]},
                {"text": "invoice reconciliation", "sources": ["S2"]}
            ],
            "incomplete_reason": null
        })),
        &planned,
    )?;
    let (input, relations) = super::annotate_input(&concept, &planned, "snapshot".into(), None)?;
    assert_eq!(input.confidence, "possible");
    assert!(input.supports.is_empty());
    assert!(
        relations
            .iter()
            .all(|relation| relation.confidence == "possible")
    );
    Ok(())
}

#[test]
fn possible_child_artifact_caps_the_whole_generated_concept() -> Result<()> {
    let mut planned = sources();
    planned[0].confidence = "possible".into();
    let concept = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1"]},
            "aliases": [
                {"text": "Invoice Reconciliation", "sources": ["S1"]},
                {"text": "invoice reconciliation", "sources": ["S2"]}
            ],
            "incomplete_reason": null
        })),
        &planned,
    )?;
    let (input, relations) = super::annotate_input(&concept, &planned, "snapshot".into(), None)?;
    assert_eq!(input.confidence, "possible");
    assert!(
        relations
            .iter()
            .all(|relation| relation.confidence == "possible")
    );
    Ok(())
}

#[test]
fn aliases_are_closed_over_exact_enumerated_spellings_and_sources() {
    let planned = sources();
    let invented = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1"]},
            "aliases": [{"text": "INVOICE RECONCILIATION", "sources": ["S1"]}],
            "incomplete_reason": null
        })),
        &planned,
    )
    .expect_err("invented display spelling");
    assert!(
        invented
            .to_string()
            .contains("not an enumerated source spelling")
    );

    let wrong_child = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1"]},
            "aliases": [{"text": "Invoice Reconciliation", "sources": ["S2"]}],
            "incomplete_reason": null
        })),
        &planned,
    )
    .expect_err("alias citation to child with different spelling");
    assert!(
        wrong_child
            .to_string()
            .contains("must cite exactly every source")
    );

    let unknown = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S9"]},
            "aliases": [{"text": "Invoice Reconciliation", "sources": ["S1"]}],
            "incomplete_reason": null
        })),
        &planned,
    )
    .expect_err("unknown source");
    assert!(unknown.to_string().contains("unknown source `S9`"));
}

#[test]
fn alias_order_is_canonical_even_when_the_provider_reverses_it() -> Result<()> {
    let planned = sources();
    let concept = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1", "S2"]},
            "aliases": [
                {"text": "invoice reconciliation", "sources": ["S2"]},
                {"text": "Invoice Reconciliation", "sources": ["S1"]}
            ],
            "candidates": [
                {"source": "S1", "decision": "included", "claims": ["/definition", "/aliases/1"]},
                {"source": "S2", "decision": "included", "claims": ["/definition", "/aliases/0"]}
            ],
            "incomplete_reason": null
        })),
        &planned,
    )?;
    let (input, relations) = super::annotate_input(&concept, &planned, "snapshot".into(), None)?;
    assert_eq!(
        input.body["aliases"],
        json!(["Invoice Reconciliation", "invoice reconciliation"])
    );
    assert!(
        relations.iter().any(|relation| {
            relation.claim_path == "/aliases/0" && relation.dst_artifact_id == 11
        })
    );
    assert!(
        relations.iter().any(|relation| {
            relation.claim_path == "/aliases/1" && relation.dst_artifact_id == 12
        })
    );
    Ok(())
}

#[test]
fn candidates_are_exhaustive_and_exactly_match_claim_citations() {
    let planned = sources();
    let valid = json!({
        "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1", "S2"]},
        "aliases": [
            {"text": "Invoice Reconciliation", "sources": ["S1"]},
            {"text": "invoice reconciliation", "sources": ["S2"]}
        ],
        "candidates": [
            {"source": "S1", "decision": "included", "claims": ["/definition", "/aliases/0"]},
            {"source": "S2", "decision": "included", "claims": ["/definition", "/aliases/1"]}
        ],
        "incomplete_reason": null
    });
    let concept = validate(&submission(valid.clone()), &planned).expect("closed candidate set");
    assert_eq!(concept.classifications.len(), 2);
    assert_eq!(concept.classifications[0].decision, "supporting");
    assert_eq!(concept.classifications[1].decision, "supporting");

    let mut excluded_cited = valid.clone();
    excluded_cited["candidates"][0] = json!({
        "source": "S1",
        "decision": "excluded",
        "reason": "incorrectly excludes a cited source"
    });
    let error = validate(&submission(excluded_cited), &planned).expect_err("cited source excluded");
    assert!(error.to_string().contains("is cited by output claims"));

    let mut mismatched_link = valid.clone();
    mismatched_link["candidates"][0]["claims"] = json!(["/definition"]);
    let error = validate(&submission(mismatched_link), &planned).expect_err("missing cited link");
    assert!(error.to_string().contains("exactly the output claims"));

    let mut missing = valid;
    missing["candidates"] = json!([{
        "source": "S1", "decision": "included", "claims": ["/definition", "/aliases/0"]
    }]);
    let error = validate(&submission(missing), &planned).expect_err("missing child decision");
    assert!(error.to_string().contains("candidate closure violated"));
}

#[test]
fn source_groups_reject_near_duplicates_and_invalid_provenance() {
    let mut planned = sources();
    planned[1].aliases[0].text = "invoice-reconciliation".into();
    let near_duplicate = validate(
        &submission(json!({
            "definition": null,
            "aliases": [],
            "incomplete_reason": "not enough evidence"
        })),
        &planned,
    )
    .expect_err("punctuation-distinct source group");
    assert!(near_duplicate.to_string().contains("not concept group"));

    let mut planned = sources();
    planned[0].aliases[0].supports.clear();
    let missing_support = validate(
        &submission(json!({
            "definition": null,
            "aliases": [],
            "incomplete_reason": "not enough evidence"
        })),
        &planned,
    )
    .expect_err("source without exact evidence");
    assert!(
        missing_support
            .to_string()
            .contains("requires exact source supports")
    );
}

#[test]
fn refusal_is_exclusive_and_never_publishable() -> Result<()> {
    let planned = sources();
    let concept = validate(
        &submission(json!({
            "definition": null,
            "aliases": [],
            "incomplete_reason": "The vocabulary is used inconsistently across the sources."
        })),
        &planned,
    )?;
    assert!(concept.incomplete.is_some());
    assert_eq!(concept.classifications.len(), planned.len());
    assert_eq!(concept.classifications[0].decision, "excluded");
    assert!(super::annotate_input(&concept, &planned, "snapshot".into(), None).is_err());

    let contradictory = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1"]},
            "aliases": [],
            "incomplete_reason": "The vocabulary is ambiguous."
        })),
        &planned,
    )
    .expect_err("claims alongside refusal");
    assert!(contradictory.to_string().contains("must not carry claims"));

    let decision_with_refusal = validate(
        &submission(json!({
            "definition": null,
            "aliases": [],
            "candidates": [{"source": "S1", "decision": "excluded", "reason": "irrelevant"}],
            "incomplete_reason": "The vocabulary is ambiguous."
        })),
        &planned,
    )
    .expect_err("decisions alongside refusal");
    assert!(
        decision_with_refusal
            .to_string()
            .contains("child decisions")
    );
    Ok(())
}

#[test]
fn publication_rejects_changed_or_reordered_source_identity() -> Result<()> {
    let planned = sources();
    let concept = validate(
        &submission(json!({
            "definition": {"text": "Defines the invoice reconciliation boundary.", "sources": ["S1"]},
            "aliases": [
                {"text": "Invoice Reconciliation", "sources": ["S1"]},
                {"text": "invoice reconciliation", "sources": ["S2"]}
            ],
            "incomplete_reason": null
        })),
        &planned,
    )?;
    let mut changed = planned.clone();
    changed[0].fingerprint = "changed".into();
    let error = super::annotate_input(&concept, &changed, "snapshot".into(), None)
        .expect_err("changed child identity");
    assert!(error.to_string().contains("plan changed or was reordered"));
    Ok(())
}
