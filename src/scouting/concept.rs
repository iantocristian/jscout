//! Concept output contract. A concept is defined over one deterministic
//! normalized vocabulary group assembled from current workflow names and
//! card domain terms. The model may explain that group, but it cannot invent
//! vocabulary or evidence: every definition and alias cites enumerated child
//! artifacts, relations pin those children by fingerprint, and exact source
//! remains attributable through those child artifacts.
//!
//! Rust validation is authoritative regardless of what the synthetic submit
//! tool schema accepted.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::ledger::ClassificationRow;
use crate::semantic::{AnnotateInput, RelationInput};

pub const PROMPT_VERSION: &str = "concept-scout/v1";
pub const SUBMIT_TOOL_NAME: &str = "submit_concept";
pub const NORMALIZER_VERSION: &str = "concept-normalizer/nfkc-lower-ws-v1";

pub(crate) const MAX_CANONICAL_CHARS: usize = 120;
pub(crate) const MAX_ALIAS_CHARS: usize = 120;
const MAX_DEFINITION_CHARS: usize = 900;
const MAX_ALIASES: usize = 32;
const MAX_SOURCES: usize = 64;
const MAX_REASON_CHARS: usize = 300;
// Bound the serialized concept body independently of the generic semantic
// artifact limit so oversized model output fails at contract validation.
const MAX_CONCEPT_BODY_BYTES: usize = 10_000;

/// Unicode-stable identity for concept grouping and exact alias merging.
/// Punctuation is deliberately preserved: `invoice-id` and `invoice id` are
/// different concepts until an independently validated merge says otherwise.
pub fn normalize(value: &str) -> String {
    collapse_whitespace(value.nfkc().flat_map(char::to_lowercase))
}

/// Normalize representation without erasing meaningful display casing. The
/// canonical key uses [`normalize`]; stored aliases and prose use this form.
/// The planner uses this to preserve the observed spelling while grouping on
/// [`normalize`].
pub fn normalize_display(value: &str) -> String {
    collapse_whitespace(value.nfkc())
}

fn normalize_text(value: &str) -> String {
    normalize_display(value)
}

fn collapse_whitespace(chars: impl Iterator<Item = char>) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for character in chars {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    output
}

/// One exact support already attached to a source artifact claim. It remains
/// part of the deterministic concept plan and validation boundary; generated
/// concepts preserve the source-artifact hop instead of copying this span onto
/// a newly written definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConceptSupport {
    pub anchor: String,
    pub role: Option<String>,
    pub evidence_file: String,
    pub evidence_start_line: i64,
    pub evidence_end_line: i64,
    pub confidence: String,
}

/// Backwards-compatible spelling for planner call sites that treat the
/// support as belonging to its source. New code should use
/// [`ConceptSupport`], which makes the ownership clear at the public API.
pub type SourceSupport = ConceptSupport;

/// One spelling of the normalized vocabulary key in a source artifact. The
/// child claim path and source spans remain part of the deterministic plan and
/// ledger evidence. Published concept claims cite the fingerprinted child,
/// whose own supports retain the exact source coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConceptSourceAlias {
    pub text: String,
    pub claim_path: String,
    pub supports: Vec<ConceptSupport>,
}

/// One current child artifact the model may cite, pinned by fingerprint at
/// planning time. References are deterministic `S1`, `S2`, ... labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConceptSource {
    pub reference: String,
    pub artifact_id: i64,
    pub artifact_type: String,
    pub name: Option<String>,
    pub fingerprint: String,
    pub confidence: String,
    /// Complete validated child body, quoted as repository data in the
    /// prompt. Vocabulary decides candidacy; the body supplies the
    /// repository-specific context needed to define the concept.
    pub body_json: String,
    pub aliases: Vec<ConceptSourceAlias>,
}

/// JSON Schema for the synthetic submit tool. The concept identity is absent
/// by design: it is the deterministic normalized vocabulary group, never a
/// model-proposed name.
pub fn submit_tool_schema(source_references: &[String]) -> Value {
    let cited_claim = |text_description: &str, min_length: usize, max_length: usize| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["text", "sources"],
            "properties": {
                "text": {
                    "type": "string",
                    "minLength": min_length,
                    "maxLength": max_length,
                    "description": text_description,
                },
                "sources": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": source_references.len(),
                    "uniqueItems": true,
                    "items": { "type": "string", "enum": source_references },
                    "description": "Enumerated source artifacts supporting THIS claim",
                },
            },
        })
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["definition", "aliases", "candidates", "incomplete_reason"],
        "properties": {
            "definition": {
                "anyOf": [
                    cited_claim(
                        "A concise repository-specific definition of this domain concept",
                        10,
                        MAX_DEFINITION_CHARS,
                    ),
                    { "type": "null" }
                ],
                "description": "Required for a complete concept; null ONLY with incomplete_reason",
            },
            "aliases": {
                "type": "array",
                "maxItems": MAX_ALIASES,
                "description": "Only spellings enumerated in the source pack; each cited independently",
                "items": cited_claim(
                    "One exact source spelling of the concept",
                    1,
                    MAX_ALIAS_CHARS,
                ),
            },
            "candidates": {
                "type": "array",
                "description": "For a complete concept, exactly one decision per enumerated source. An included source lists every concept claim it supports; an excluded source gives only a reason. Use [] when refusing.",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["source", "decision", "claims"],
                            "properties": {
                                "source": { "type": "string", "enum": source_references },
                                "decision": { "type": "string", "enum": ["included"] },
                                "claims": {
                                    "type": "array",
                                    "minItems": 1,
                                    "uniqueItems": true,
                                    "items": {
                                        "type": "string",
                                        "pattern": "^/(definition|aliases/[0-9]+)$"
                                    },
                                    "description": "Only output claim paths that cite this source; list each such path exactly once"
                                }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["source", "decision", "reason"],
                            "properties": {
                                "source": { "type": "string", "enum": source_references },
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
                "description": "Set ONLY when the sources cannot support a definition; nothing is then published",
            },
        },
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    #[serde(default)]
    pub definition: Option<SubmittedClaim>,
    #[serde(default)]
    pub aliases: Option<Vec<SubmittedClaim>>,
    #[serde(default)]
    pub candidates: Option<Vec<SubmittedCandidate>>,
    #[serde(default)]
    pub incomplete_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmittedClaim {
    pub text: String,
    pub sources: Vec<String>,
}

/// One exhaustive decision about a planned child artifact. `claims` contains
/// output JSON pointers (`/definition`, `/aliases/N`) rather than model text,
/// preventing an included child from being linked to invented prose.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmittedCandidate {
    pub source: String,
    pub decision: String,
    #[serde(default)]
    pub claims: Option<Vec<String>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedClaim {
    pub text: String,
    /// Indexes into the exact planned source list.
    pub sources: Vec<usize>,
}

/// A completely validated concept. An incomplete value carries the model's
/// refusal and cannot be converted into an artifact.
#[derive(Debug, Clone)]
pub struct ValidatedConcept {
    pub canonical_name: String,
    pub definition: Option<ValidatedClaim>,
    pub aliases: Vec<ValidatedClaim>,
    pub classifications: Vec<ClassificationRow>,
    pub incomplete: Option<String>,
    // Retain the complete immutable plan, not just child ids/fingerprints:
    // publication must use precisely the relations and aliases that passed
    // validation, even if a caller accidentally reuses a mutable plan value.
    source_plan: Vec<ConceptSource>,
}

pub fn validate(submission: &Submission, sources: &[ConceptSource]) -> Result<ValidatedConcept> {
    let canonical_name = validate_sources(sources)?;
    let source_plan = sources.to_vec();

    if let Some(reason) = &submission.incomplete_reason {
        let reason = normalize_text(reason);
        if reason.is_empty() {
            bail!("incomplete_reason must be a non-empty explanation when set");
        }
        if reason.chars().count() > MAX_REASON_CHARS {
            bail!("incomplete_reason exceeds {MAX_REASON_CHARS} characters");
        }
        if submission.definition.is_some()
            || submission
                .aliases
                .as_deref()
                .is_some_and(|aliases| !aliases.is_empty())
            || submission
                .candidates
                .as_deref()
                .is_some_and(|candidates| !candidates.is_empty())
        {
            bail!(
                "an incomplete submission must not carry claims or child decisions; set definition to null and aliases/candidates to []"
            );
        }
        return Ok(ValidatedConcept {
            canonical_name,
            definition: None,
            aliases: Vec::new(),
            classifications: incomplete_classifications(sources, &reason)?,
            incomplete: Some(reason),
            source_plan,
        });
    }

    let definition = submission
        .definition
        .as_ref()
        .context("a concept requires a cited `definition` claim")?;
    let definition = validate_claim(definition, MAX_DEFINITION_CHARS, "definition", sources)?;
    if definition.text.chars().count() < 10 {
        bail!("`definition` claim must contain at least 10 characters");
    }

    let submitted_aliases = submission
        .aliases
        .as_deref()
        .context("a concept requires an `aliases` array")?;
    if submitted_aliases.is_empty() {
        bail!("a concept requires at least one cited alias");
    }
    if submitted_aliases.len() > MAX_ALIASES {
        bail!(
            "`aliases` carries {} claims; at most {MAX_ALIASES}",
            submitted_aliases.len()
        );
    }
    let known_spellings: BTreeSet<String> = sources
        .iter()
        .flat_map(|source| source.aliases.iter())
        .map(|alias| normalize_text(&alias.text))
        .collect();
    if known_spellings.len() > MAX_ALIASES {
        bail!(
            "concept plan carries {} observed aliases, over the {MAX_ALIASES} alias limit",
            known_spellings.len()
        );
    }
    let mut aliases = Vec::with_capacity(submitted_aliases.len());
    let mut seen_spellings = BTreeSet::new();
    for (index, submitted) in submitted_aliases.iter().enumerate() {
        let alias = validate_claim(
            submitted,
            MAX_ALIAS_CHARS,
            &format!("aliases/{index}"),
            sources,
        )?;
        if normalize(&alias.text) != canonical_name {
            bail!(
                "alias `{}` does not normalize to canonical concept key `{canonical_name}`",
                alias.text
            );
        }
        if !known_spellings.contains(&alias.text) {
            bail!(
                "alias `{}` is not an enumerated source spelling; the model cannot invent aliases",
                alias.text
            );
        }
        if !seen_spellings.insert(alias.text.clone()) {
            bail!("`aliases` repeats the spelling `{}`", alias.text);
        }
        // Alias citations are not a model judgment: every child artifact that
        // observes this spelling must be cited, and no other child may be.
        // This keeps the relation-backed provenance chain exhaustive.
        let expected_sources: BTreeSet<usize> = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| {
                source
                    .aliases
                    .iter()
                    .any(|source_alias| normalize_text(&source_alias.text) == alias.text)
            })
            .map(|(source_index, _)| source_index)
            .collect();
        let cited_sources: BTreeSet<usize> = alias.sources.iter().copied().collect();
        if cited_sources != expected_sources {
            bail!(
                "alias `{}` must cite exactly every source artifact that observes that spelling",
                alias.text
            );
        }
        aliases.push(alias);
    }
    if seen_spellings != known_spellings {
        let missing = known_spellings
            .difference(&seen_spellings)
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "alias closure violated: {} observed spellings were omitted ({})",
            missing.len(),
            missing.join(", ")
        );
    }

    // Alias identity and display spelling are deterministic inputs. Accept a
    // provider that returns the exhaustive set in a different order, but
    // canonicalize both stored paths and candidate decisions before building
    // the artifact so provider ordering cannot perturb its fingerprint.
    let submitted_order = aliases
        .iter()
        .map(|alias| alias.text.clone())
        .collect::<Vec<_>>();
    aliases.sort_by(|left, right| left.text.cmp(&right.text));
    let canonical_indices = aliases
        .iter()
        .enumerate()
        .map(|(index, alias)| (alias.text.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let path_remap = submitted_order
        .iter()
        .enumerate()
        .map(|(old_index, spelling)| {
            (
                format!("/aliases/{old_index}"),
                format!(
                    "/aliases/{}",
                    canonical_indices
                        .get(spelling)
                        .expect("validated alias has a canonical index")
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut canonical_candidates = submission.candidates.clone();
    if let Some(candidates) = &mut canonical_candidates {
        for candidate in candidates {
            if let Some(claims) = &mut candidate.claims {
                for claim in claims {
                    if let Some(canonical) = path_remap.get(claim) {
                        *claim = canonical.clone();
                    }
                }
            }
        }
    }

    let claims = std::iter::once(("/definition".to_string(), &definition))
        .chain(
            aliases
                .iter()
                .enumerate()
                .map(|(index, alias)| (format!("/aliases/{index}"), alias)),
        )
        .collect::<Vec<_>>();
    let classifications = validate_candidates(canonical_candidates.as_deref(), sources, &claims)?;

    let concept = ValidatedConcept {
        canonical_name,
        definition: Some(definition),
        aliases,
        classifications,
        incomplete: None,
        source_plan,
    };
    let body_bytes = serde_json::to_vec(&body(&concept))?.len();
    if body_bytes > MAX_CONCEPT_BODY_BYTES {
        bail!("concept body is {body_bytes} bytes, over the {MAX_CONCEPT_BODY_BYTES} byte limit");
    }
    Ok(concept)
}

fn validate_sources(sources: &[ConceptSource]) -> Result<String> {
    if sources.is_empty() {
        bail!("concept validation requires at least one planned source artifact");
    }
    if sources.len() > MAX_SOURCES {
        bail!(
            "concept plan carries {} source artifacts; at most {MAX_SOURCES}",
            sources.len()
        );
    }
    let mut references = BTreeSet::new();
    let mut artifact_ids = BTreeSet::new();
    let mut canonical_name = None;
    for (index, source) in sources.iter().enumerate() {
        let expected_reference = format!("S{}", index + 1);
        if source.reference != expected_reference {
            bail!(
                "concept source references must be deterministic S# labels; expected `{expected_reference}`, got `{}`",
                source.reference
            );
        }
        if source.reference.trim().is_empty() || !references.insert(source.reference.as_str()) {
            bail!(
                "concept source references must be unique and non-empty; duplicate `{}`",
                source.reference
            );
        }
        if source.artifact_id <= 0 || !artifact_ids.insert(source.artifact_id) {
            bail!(
                "concept source artifact ids must be unique and positive; invalid id {}",
                source.artifact_id
            );
        }
        if !matches!(source.artifact_type.as_str(), "workflow" | "card") {
            bail!(
                "concept source `{}` has unsupported artifact type `{}`; expected workflow or card",
                source.reference,
                source.artifact_type
            );
        }
        if source.fingerprint.trim().is_empty() {
            bail!(
                "concept source `{}` requires a pinned artifact fingerprint",
                source.reference
            );
        }
        if !matches!(source.confidence.as_str(), "likely" | "possible") {
            bail!(
                "concept source `{}` has invalid artifact confidence `{}`",
                source.reference,
                source.confidence
            );
        }
        let body: Value = serde_json::from_str(&source.body_json).with_context(|| {
            format!(
                "concept source `{}` carries invalid child body JSON",
                source.reference
            )
        })?;
        if !body.is_object() {
            bail!(
                "concept source `{}` child body must be a JSON object",
                source.reference
            );
        }
        if source.aliases.is_empty() {
            bail!(
                "concept source `{}` requires at least one vocabulary claim",
                source.reference
            );
        }
        let mut source_claims = BTreeSet::new();
        for alias in &source.aliases {
            let display = normalize_text(&alias.text);
            let normalized = normalize(&alias.text);
            if display.is_empty() || normalized.is_empty() {
                bail!(
                    "concept source `{}` carries an empty vocabulary spelling",
                    source.reference
                );
            }
            if normalized.chars().count() > MAX_CANONICAL_CHARS {
                bail!("normalized concept key exceeds {MAX_CANONICAL_CHARS} characters");
            }
            match &canonical_name {
                Some(expected) if expected != &normalized => bail!(
                    "source `{}` spelling `{}` normalizes to `{normalized}`, not concept group `{expected}`",
                    source.reference,
                    alias.text
                ),
                None => canonical_name = Some(normalized),
                _ => {}
            }
            if !alias.claim_path.starts_with('/') || alias.claim_path.len() < 2 {
                bail!(
                    "source `{}` alias `{}` requires an exact child claim_path",
                    source.reference,
                    alias.text
                );
            }
            if alias.supports.is_empty() {
                bail!(
                    "source `{}` alias `{}` requires exact source supports",
                    source.reference,
                    alias.text
                );
            }
            let claim_identity = (alias.claim_path.clone(), display.clone());
            if !source_claims.insert(claim_identity) {
                bail!(
                    "source `{}` repeats alias `{}` at `{}`",
                    source.reference,
                    alias.text,
                    alias.claim_path
                );
            }
            let mut supports = BTreeSet::new();
            for support in &alias.supports {
                if support.anchor.trim().is_empty() || support.evidence_file.trim().is_empty() {
                    bail!(
                        "source `{}` alias `{}` has an incomplete source support",
                        source.reference,
                        alias.text
                    );
                }
                if support.evidence_start_line <= 0
                    || support.evidence_end_line < support.evidence_start_line
                {
                    bail!(
                        "source `{}` alias `{}` has an invalid support span {}-{}",
                        source.reference,
                        alias.text,
                        support.evidence_start_line,
                        support.evidence_end_line
                    );
                }
                if !matches!(support.confidence.as_str(), "likely" | "possible") {
                    bail!(
                        "source `{}` alias `{}` has invalid support confidence `{}`",
                        source.reference,
                        alias.text,
                        support.confidence
                    );
                }
                let identity = (
                    support.anchor.as_str(),
                    support.evidence_file.as_str(),
                    support.evidence_start_line,
                    support.evidence_end_line,
                );
                if !supports.insert(identity) {
                    bail!(
                        "source `{}` alias `{}` repeats an exact source support",
                        source.reference,
                        alias.text
                    );
                }
            }
        }
    }
    canonical_name.context("concept source group has no normalized vocabulary key")
}

fn validate_claim(
    submitted: &SubmittedClaim,
    max_chars: usize,
    label: &str,
    sources: &[ConceptSource],
) -> Result<ValidatedClaim> {
    let text = normalize_text(&submitted.text);
    if text.is_empty() {
        bail!("`{label}` claim text must not be empty");
    }
    if text.chars().count() > max_chars {
        bail!("`{label}` claim exceeds {max_chars} characters");
    }
    if submitted.sources.is_empty() {
        bail!("`{label}` claim requires at least one source citation");
    }
    let known: BTreeMap<&str, usize> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| (source.reference.as_str(), index))
        .collect();
    let mut cited = BTreeSet::new();
    for reference in &submitted.sources {
        let Some(index) = known.get(reference.as_str()) else {
            bail!("`{label}` cites unknown source `{reference}`; the model cannot add sources");
        };
        if !cited.insert(*index) {
            bail!("`{label}` cites source `{reference}` more than once");
        }
    }
    Ok(ValidatedClaim {
        text,
        sources: cited.into_iter().collect(),
    })
}

/// Validate the candidate-closed part of the contract. The model must account
/// for every child it saw, and an inclusion is valid only when it is exactly
/// the set of output claims that explicitly cite that child.
fn validate_candidates(
    submitted: Option<&[SubmittedCandidate]>,
    sources: &[ConceptSource],
    claims: &[(String, &ValidatedClaim)],
) -> Result<Vec<ClassificationRow>> {
    let submitted = submitted.context("a complete concept requires a `candidates` array")?;
    if submitted.len() != sources.len() {
        bail!(
            "candidate closure violated: {} decisions for {} planned source artifacts",
            submitted.len(),
            sources.len()
        );
    }
    let known: BTreeMap<&str, usize> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| (source.reference.as_str(), index))
        .collect();
    let mut decisions = vec![None; sources.len()];
    let mut rows = Vec::with_capacity(sources.len());

    for candidate in submitted {
        let Some(&source_index) = known.get(candidate.source.as_str()) else {
            bail!(
                "candidate source `{}` is not in the deterministic source set; the model cannot add children",
                candidate.source
            );
        };
        if decisions[source_index].replace(()).is_some() {
            bail!(
                "candidate source `{}` was classified more than once",
                candidate.source
            );
        }
        let source = &sources[source_index];
        let cited_paths: BTreeSet<String> = claims
            .iter()
            .filter(|(_, claim)| claim.sources.contains(&source_index))
            .map(|(path, _)| path.clone())
            .collect();
        match candidate.decision.as_str() {
            "included" => {
                if candidate.reason.is_some() {
                    bail!(
                        "included source `{}` must not carry a reason",
                        candidate.source
                    );
                }
                let listed = candidate.claims.as_deref().context(format!(
                    "included source `{}` requires at least one cited claim path",
                    candidate.source
                ))?;
                let listed: BTreeSet<String> = listed.iter().cloned().collect();
                if listed.len() != candidate.claims.as_ref().expect("checked above").len() {
                    bail!(
                        "included source `{}` repeats a claim path",
                        candidate.source
                    );
                }
                if listed.is_empty() {
                    bail!(
                        "included source `{}` requires at least one cited claim path",
                        candidate.source
                    );
                }
                if listed != cited_paths {
                    bail!(
                        "included source `{}` must list exactly the output claims that cite it",
                        candidate.source
                    );
                }
                rows.push(ClassificationRow {
                    anchor_key: source.reference.clone(),
                    // The model-facing decision is `included`; the ledger
                    // reuses its DB-valid participant vocabulary.
                    decision: "supporting".into(),
                    role: None,
                    evidence_json: serde_json::to_string(&json!({
                        "normalizer": NORMALIZER_VERSION,
                        "artifact_id": source.artifact_id,
                        "fingerprint": source.fingerprint,
                        "claims": listed,
                    }))?,
                });
            }
            "excluded" => {
                if candidate.claims.is_some() {
                    bail!(
                        "excluded source `{}` must not carry claim paths",
                        candidate.source
                    );
                }
                if !cited_paths.is_empty() {
                    bail!(
                        "excluded source `{}` is cited by output claims and must be included",
                        candidate.source
                    );
                }
                let reason = candidate
                    .reason
                    .as_deref()
                    .map(normalize_text)
                    .filter(|reason| !reason.is_empty())
                    .context(format!(
                        "excluded source `{}` requires a reason",
                        candidate.source
                    ))?;
                if reason.chars().count() > MAX_REASON_CHARS {
                    bail!(
                        "reason for excluded source `{}` exceeds {MAX_REASON_CHARS} characters",
                        candidate.source
                    );
                }
                rows.push(ClassificationRow {
                    anchor_key: source.reference.clone(),
                    decision: "excluded".into(),
                    role: None,
                    evidence_json: serde_json::to_string(&json!({
                        "normalizer": NORMALIZER_VERSION,
                        "artifact_id": source.artifact_id,
                        "fingerprint": source.fingerprint,
                        "reason": reason,
                    }))?,
                });
            }
            other => bail!(
                "candidate source `{}` has unknown decision `{other}`; expected included or excluded",
                candidate.source
            ),
        }
    }
    if let Some((index, _)) = decisions
        .iter()
        .enumerate()
        .find(|(_, decision)| decision.is_none())
    {
        bail!(
            "candidate closure violated: planned source `{}` was not classified",
            sources[index].reference
        );
    }
    Ok(rows)
}

fn incomplete_classifications(
    sources: &[ConceptSource],
    reason: &str,
) -> Result<Vec<ClassificationRow>> {
    sources
        .iter()
        .map(|source| {
            Ok(ClassificationRow {
                anchor_key: source.reference.clone(),
                decision: "excluded".into(),
                role: None,
                evidence_json: serde_json::to_string(&json!({
                    "normalizer": NORMALIZER_VERSION,
                    "artifact_id": source.artifact_id,
                    "fingerprint": source.fingerprint,
                    "reason": reason,
                    "incomplete": true,
                }))?,
            })
        })
        .collect()
}

fn body(concept: &ValidatedConcept) -> Value {
    json!({
        "definition": concept.definition.as_ref().map(|claim| claim.text.as_str()),
        "aliases": concept.aliases.iter().map(|alias| alias.text.as_str()).collect::<Vec<_>>(),
    })
}

/// Convert a validated concept into the generic semantic write shape. Every
/// claim has outgoing `related_to` links to cited child fingerprints. Exact
/// supports remain on the child: copying a vocabulary span directly onto an
/// LLM-written definition would erase the semantic hop and overstate what the
/// source line proves. Empty-path links record every model-visible child as an
/// input dependency even when the model did not cite it.
pub fn annotate_input(
    concept: &ValidatedConcept,
    sources: &[ConceptSource],
    snapshot: String,
    supersedes: Option<i64>,
) -> Result<(AnnotateInput, Vec<RelationInput>)> {
    let definition = concept
        .definition
        .as_ref()
        .context("an incomplete concept must not be published")?;
    if sources != concept.source_plan {
        bail!("concept source plan changed or was reordered after validation");
    }

    let confidence = if sources.iter().any(|source| source.confidence == "possible")
        || sources
            .iter()
            .flat_map(|source| &source.aliases)
            .flat_map(|alias| &alias.supports)
            .any(|support| support.confidence == "possible")
    {
        "possible"
    } else {
        "likely"
    };

    let mut relations = Vec::new();
    let mut seen_relations = BTreeSet::new();
    let mut relate = |claim_path: &str, claim: &ValidatedClaim| {
        for &source_index in &claim.sources {
            let source = &sources[source_index];
            if seen_relations.insert((claim_path.to_string(), source.artifact_id)) {
                relations.push(RelationInput {
                    claim_path: claim_path.to_string(),
                    relation: "related_to".into(),
                    dst_artifact_id: source.artifact_id,
                    dst_fingerprint: source.fingerprint.clone(),
                    confidence: confidence.into(),
                });
            }
        }
    };
    relate("/definition", definition);
    for (index, alias) in concept.aliases.iter().enumerate() {
        relate(&format!("/aliases/{index}"), alias);
    }
    for source in sources {
        relations.push(RelationInput {
            claim_path: String::new(),
            relation: "related_to".into(),
            dst_artifact_id: source.artifact_id,
            dst_fingerprint: source.fingerprint.clone(),
            confidence: confidence.into(),
        });
    }

    Ok((
        AnnotateInput {
            artifact_type: "concept".into(),
            name: Some(concept.canonical_name.clone()),
            body: body(concept),
            supports: Vec::new(),
            confidence: confidence.into(),
            snapshot,
            supersedes,
        },
        relations,
    ))
}

#[cfg(test)]
mod tests;
