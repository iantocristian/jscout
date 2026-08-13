use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::protocol::{
    DeclarationSite, InputValidation, MemberQuery, ProjectAnswer, TypeScriptIdentity,
};

#[derive(Debug, Clone)]
pub struct EnrichOptions<'a> {
    pub database: Option<&'a Path>,
    pub sidecar: Option<&'a Path>,
    pub timeout: Duration,
}

#[derive(Debug, Serialize)]
pub struct EnrichReport {
    pub snapshot: String,
    pub batch_id: i64,
    pub occurrences_queried: usize,
    pub unknown_answers: usize,
    pub unknown_projects: Vec<String>,
    pub unmapped_declarations: usize,
    pub facts_published: usize,
    pub checker_version: String,
    pub checker_source: String,
    pub projects: usize,
    pub configuration_problems: usize,
}

#[derive(Debug, Clone)]
struct Occurrence {
    id: i64,
    file_id: i64,
    file: String,
    hash: String,
    call_start: i64,
    call_end: i64,
    receiver_start: i64,
    receiver_end: i64,
    property_start: i64,
    property_end: i64,
    member: String,
}

#[derive(Debug, Clone)]
struct Target {
    anchor: String,
    fingerprint: String,
}

#[derive(Debug, Clone)]
struct PendingFact {
    occurrence: Occurrence,
    project_id: String,
    receiver_type: Option<String>,
    target: Target,
    confidence: String,
    input_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ValidatedInput {
    kind: String,
    path: String,
    source_hash: String,
}

#[derive(Debug, Clone)]
struct PendingProjectAnswer {
    occurrence: Occurrence,
    project_id: String,
    input_fingerprint: String,
    status: &'static str,
}

/// What one occurrence's whole checker answer contributed.
#[derive(Debug, Default)]
struct OccurrenceOutcome {
    facts: Vec<PendingFact>,
    projects: Vec<PendingProjectAnswer>,
    unknown_answers: usize,
    unmapped_declarations: usize,
}

struct PublishPlan<'a> {
    snapshot: &'a str,
    checker: &'a TypeScriptIdentity,
    protocol: u32,
    input_fingerprint: &'a str,
    facts: &'a [PendingFact],
    projects: &'a [PendingProjectAnswer],
    inputs: &'a [ValidatedInput],
}

pub fn enrich(root: &Path, options: &EnrichOptions<'_>) -> Result<EnrichReport> {
    if options.timeout.is_zero() {
        bail!("--timeout must be greater than zero seconds");
    }
    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    let conn = match options.database {
        Some(path) => crate::store::open_path(path)?,
        None => crate::store::open(&canonical_root)?,
    };
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let occurrences = load_occurrences(&conn)?;
    let mut checker = super::launch(&canonical_root, options.sidecar)?;
    checker
        .register_interrupts()
        .context("failed to install checker Ctrl-C handler")?;
    let capabilities = checker.capabilities(options.timeout)?;
    let mut facts = Vec::new();
    let mut projects = Vec::new();
    let mut validation = BTreeMap::<(String, String), InputValidation>::new();
    let mut unknown_answers = 0;
    let mut unmapped_declarations = 0;
    let mut checker_identity: Option<TypeScriptIdentity> = None;

    for (index, occurrence) in occurrences.iter().enumerate() {
        let source = source_path(&canonical_root, &conn, occurrence)?;
        verify_source_hash(&source, &occurrence.hash, &occurrence.file)?;
        let result = checker.resolve_member(
            MemberQuery {
                file: occurrence.file.clone(),
                indexed_hash: occurrence.hash.clone(),
                call_start: occurrence.call_start,
                call_end: occurrence.call_end,
                receiver_start: occurrence.receiver_start,
                receiver_end: occurrence.receiver_end,
                property_start: occurrence.property_start,
                property_end: occurrence.property_end,
            },
            options.timeout,
        )?;
        if result.indexed_hash != occurrence.hash || result.source_hash != occurrence.hash {
            bail!(
                "checker returned a source identity that does not match indexed file {}",
                occurrence.file
            );
        }
        let _configuration_problem_count = result.configuration_problems.len();
        if let Some(identity) = &checker_identity {
            if identity.version != result.typescript.version
                || identity.source != result.typescript.source
            {
                bail!("checker TypeScript identity changed during the enrichment batch");
            }
        } else {
            checker_identity = Some(result.typescript.clone());
        }

        for answer in &result.projects {
            validation
                .entry((
                    answer.project_id.clone(),
                    answer.checker_input_fingerprint.clone(),
                ))
                .or_insert_with(|| InputValidation {
                    file: occurrence.file.clone(),
                    project_id: answer.project_id.clone(),
                    fingerprint: answer.checker_input_fingerprint.clone(),
                });
        }
        let outcome = map_occurrence(&conn, occurrence, &result.projects)?;
        unknown_answers += outcome.unknown_answers;
        unmapped_declarations += outcome.unmapped_declarations;
        facts.extend(outcome.facts);
        projects.extend(outcome.projects);
        if (index + 1) % 100 == 0 {
            eprintln!(
                "checker enrichment: queried {}/{} occurrences",
                index + 1,
                occurrences.len()
            );
        }
    }

    let validation_entries = validation.into_values().collect::<Vec<_>>();
    let checked = checker.validate_inputs(validation_entries.clone(), options.timeout)?;
    let expected_validation = validation_entries
        .iter()
        .map(|entry| {
            (
                entry.project_id.as_str(),
                entry.file.as_str(),
                entry.fingerprint.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let returned_validation = checked
        .results
        .iter()
        .filter_map(|entry| {
            entry
                .fingerprint
                .as_deref()
                .map(|fingerprint| (entry.project_id.as_str(), entry.file.as_str(), fingerprint))
        })
        .collect::<BTreeSet<_>>();
    if !checked.valid
        || checked.results.iter().any(|entry| !entry.valid)
        || returned_validation != expected_validation
    {
        bail!("checker inputs changed while enrichment was running; published nothing");
    }
    let mut validated_inputs = BTreeMap::<(String, String), String>::new();
    for entry in &checked.results {
        entry
            .fingerprint
            .as_ref()
            .context("checker omitted a validated project fingerprint")?;
        for input in &entry.inputs {
            if !Path::new(&input.path).is_absolute() {
                bail!("checker returned a non-absolute input path; published nothing");
            }
            let absolute_path = Path::new(&input.path);
            let (kind, stored_path) = match absolute_path.strip_prefix(&canonical_root) {
                Ok(relative) => (
                    "repository".to_string(),
                    relative
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                ),
                Err(_) => ("absolute".to_string(), input.path.clone()),
            };
            let key = (kind, stored_path);
            if let Some(previous) = validated_inputs.insert(key, input.source_hash.clone())
                && previous != input.source_hash
            {
                bail!("checker returned conflicting hashes for one input; published nothing");
            }
        }
    }
    let validated_inputs = validated_inputs
        .into_iter()
        .map(|((kind, path), source_hash)| ValidatedInput {
            kind,
            path,
            source_hash,
        })
        .collect::<Vec<_>>();
    let identity = checker_identity.unwrap_or(capabilities.typescript.clone());
    let batch_fingerprint = batch_fingerprint(&validation_entries);
    let batch_id = publish(
        &canonical_root,
        &conn,
        &PublishPlan {
            snapshot: &snapshot,
            checker: &identity,
            protocol: checker.versions.protocol,
            input_fingerprint: &batch_fingerprint,
            facts: &facts,
            projects: &projects,
            inputs: &validated_inputs,
        },
    )?;
    crate::structural::rebuild_projection(&conn, &snapshot)?;

    Ok(EnrichReport {
        snapshot,
        batch_id,
        occurrences_queried: occurrences.len(),
        unknown_answers,
        unknown_projects: projects
            .iter()
            .filter(|project| project.status == "unknown")
            .map(|project| project.project_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        unmapped_declarations,
        facts_published: facts.len(),
        checker_version: identity.version,
        checker_source: identity.source,
        projects: capabilities.projects.len(),
        configuration_problems: capabilities.configuration_problems.len(),
    })
}

fn load_occurrences(conn: &Connection) -> Result<Vec<Occurrence>> {
    let mut statement = conn.prepare(
        "SELECT call.rowid, file.id, file.path, file.hash,
                call.start, call.end, call.receiver_start, call.receiver_end,
                call.property_start, call.property_end, call.prop
         FROM member_calls call
         JOIN files file ON file.id=call.file_id
         WHERE file.origin IN ('repository', 'workspace')
           AND call.end > call.start
           AND call.receiver_end > call.receiver_start
           AND call.property_end > call.property_start
           AND EXISTS(SELECT 1 FROM symbols target WHERE target.name=call.prop)
         ORDER BY file.path, call.start, call.rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Occurrence {
            id: row.get(0)?,
            file_id: row.get(1)?,
            file: row.get(2)?,
            hash: row.get(3)?,
            call_start: row.get(4)?,
            call_end: row.get(5)?,
            receiver_start: row.get(6)?,
            receiver_end: row.get(7)?,
            property_start: row.get(8)?,
            property_end: row.get(9)?,
            member: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn source_path(root: &Path, conn: &Connection, occurrence: &Occurrence) -> Result<PathBuf> {
    let path = crate::store::file_source_path(conn, root, occurrence.file_id)?;
    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("indexed source is missing: {}", occurrence.file))?;
    if !canonical.starts_with(root) {
        bail!(
            "indexed query source resolves outside repository root: {}",
            occurrence.file
        );
    }
    Ok(canonical)
}

fn verify_source_hash(path: &Path, expected: &str, display: &str) -> Result<()> {
    let bytes =
        fs::read(path).with_context(|| format!("could not read indexed source {display}"))?;
    let actual = blake3::hash(&bytes).to_hex().to_string();
    if actual != expected {
        bail!("indexed source changed since the structural snapshot: {display}");
    }
    Ok(())
}

/// Turn one occurrence's whole checker answer into pending facts.
///
/// Ambiguity is judged over the checker's WHOLE answer, not over the subset
/// that happened to map: a valid declaration jscout cannot anchor (an
/// interface member, a `.d.ts` overload, or a declaration outside the root
/// still means the resolved candidate set was ambiguous. Owning projects that
/// return `unknown` are retained as coverage metadata but do not contradict a
/// clean resolution from another project.
fn map_occurrence(
    conn: &Connection,
    occurrence: &Occurrence,
    answers: &[ProjectAnswer],
) -> Result<OccurrenceOutcome> {
    let mut outcome = OccurrenceOutcome::default();
    for answer in answers {
        let status = if answer.status == "resolved" {
            "resolved"
        } else {
            "unknown"
        };
        outcome.projects.push(PendingProjectAnswer {
            occurrence: occurrence.clone(),
            project_id: answer.project_id.clone(),
            input_fingerprint: answer.checker_input_fingerprint.clone(),
            status,
        });
        if answer.status != "resolved" {
            outcome.unknown_answers += 1;
            continue;
        }
        for declaration in &answer.declarations {
            match map_declaration(conn, &occurrence.member, declaration)? {
                Some(target) => outcome.facts.push(PendingFact {
                    occurrence: occurrence.clone(),
                    project_id: answer.project_id.clone(),
                    receiver_type: answer.receiver_type.clone(),
                    target,
                    confidence: String::new(),
                    input_fingerprint: answer.checker_input_fingerprint.clone(),
                }),
                None => outcome.unmapped_declarations += 1,
            }
        }
    }
    outcome.facts.sort_by(|left, right| {
        (&left.project_id, &left.target.anchor).cmp(&(&right.project_id, &right.target.anchor))
    });
    outcome.facts.dedup_by(|left, right| {
        left.project_id == right.project_id && left.target.anchor == right.target.anchor
    });
    let target_count = outcome
        .facts
        .iter()
        .map(|fact| fact.target.anchor.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let unambiguous = target_count == 1 && outcome.unmapped_declarations == 0;
    for fact in &mut outcome.facts {
        fact.confidence = if unambiguous { "likely" } else { "possible" }.into();
    }
    Ok(outcome)
}

fn map_declaration(
    conn: &Connection,
    member: &str,
    declaration: &DeclarationSite,
) -> Result<Option<Target>> {
    if declaration.outside_root {
        return Ok(None);
    }
    let Some(path) = declaration.file.as_deref() else {
        return Ok(None);
    };
    // Containment alone fabricates targets: any declaration nested inside an
    // indexed symbol's body would anchor to its container (an object-literal
    // method inside a function became a `likely` self-edge on the function).
    // The mapped symbol must BE the member's declaration: same name, and the
    // tightest span containing the checker's declaration node.
    let mut statement = conn.prepare(
        "SELECT node.node_key, file.hash, symbol.decl_start, symbol.decl_end
         FROM symbols symbol
         JOIN files file ON file.id=symbol.file_id
         JOIN graph_nodes node ON node.native_table='symbols' AND node.native_id=symbol.id
         WHERE file.path=?1
           AND symbol.name=?4
           AND symbol.decl_start<=?2 AND symbol.decl_end>=?3
         ORDER BY (symbol.decl_end-symbol.decl_start), node.node_key",
    )?;
    let rows = statement.query_map(
        params![path, declaration.start, declaration.end, member],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )?;
    let mut candidates = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let tightest = candidates[0].3 - candidates[0].2;
    candidates.retain(|candidate| candidate.3 - candidate.2 == tightest);
    if candidates.len() != 1 || candidates[0].1 != declaration.source_hash {
        return Ok(None);
    }
    let (anchor, source_hash, start, end) = candidates.remove(0);
    Ok(Some(Target {
        fingerprint: target_fingerprint(&anchor, &source_hash, start, end),
        anchor,
    }))
}

pub(crate) fn target_fingerprint(anchor: &str, source_hash: &str, start: i64, end: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-target-v1\0");
    for value in [anchor, source_hash, &start.to_string(), &end.to_string()] {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn batch_fingerprint(entries: &[InputValidation]) -> String {
    let mut values = entries
        .iter()
        .map(|entry| {
            format!(
                "{}\0{}\0{}",
                entry.project_id, entry.file, entry.fingerprint
            )
        })
        .collect::<Vec<_>>();
    values.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-checker-batch-v1\0");
    for value in values {
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn publish(root: &Path, conn: &Connection, plan: &PublishPlan<'_>) -> Result<i64> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<i64> {
        let current = crate::structural::current_snapshot(conn)?;
        if current != plan.snapshot {
            bail!("structural snapshot changed while enrichment was running; published nothing");
        }
        for fact in plan.facts {
            recheck_occurrence(root, conn, &fact.occurrence)?;
            recheck_target(conn, &fact.target)?;
        }
        for project in plan.projects {
            recheck_occurrence(root, conn, &project.occurrence)?;
        }
        for input in plan.inputs {
            let path = input_path(root, input);
            verify_source_hash(&path, &input.source_hash, &input.path)?;
        }
        conn.execute(
            "UPDATE checker_enrichment_batches SET active=0 WHERE active=1",
            [],
        )?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1,?2,?3,?4,?5,datetime('now'),1)",
            params![
                plan.snapshot,
                plan.checker.version,
                plan.checker.source,
                plan.input_fingerprint,
                plan.protocol
            ],
        )?;
        let batch_id = conn.last_insert_rowid();
        // Nothing reads a retired batch: projection keys on `active=1` and the
        // exact source snapshot. Drop the superseded one and its facts so a
        // repeatedly enriched repository keeps one batch instead of one per
        // pass.
        conn.execute("DELETE FROM checker_enrichment_batches WHERE active=0", [])?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'checker',?17
             )",
        )?;
        for fact in plan.facts {
            insert.execute(params![
                batch_id,
                fact.occurrence.id,
                fact.occurrence.file_id,
                fact.occurrence.file,
                fact.occurrence.hash,
                fact.occurrence.call_start,
                fact.occurrence.call_end,
                fact.occurrence.receiver_start,
                fact.occurrence.receiver_end,
                fact.occurrence.property_start,
                fact.occurrence.property_end,
                fact.project_id,
                fact.receiver_type,
                fact.target.anchor,
                fact.target.fingerprint,
                fact.confidence,
                fact.input_fingerprint,
            ])?;
        }
        let mut insert_project = conn.prepare_cached(
            "INSERT INTO checker_occurrence_projects(
               batch_id, member_call_id, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for project in plan.projects {
            insert_project.execute(params![
                batch_id,
                project.occurrence.id,
                project.project_id,
                project.input_fingerprint,
                project.status,
            ])?;
        }
        Ok(batch_id)
    })();
    match result {
        Ok(batch_id) => {
            conn.execute_batch("COMMIT")?;
            Ok(batch_id)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn input_path(root: &Path, input: &ValidatedInput) -> PathBuf {
    if input.kind == "repository" {
        root.join(&input.path)
    } else {
        PathBuf::from(&input.path)
    }
}

fn recheck_occurrence(root: &Path, conn: &Connection, occurrence: &Occurrence) -> Result<()> {
    let current = conn
        .query_row(
            "SELECT file.id, file.hash, call.start, call.end,
                    call.receiver_start, call.receiver_end,
                    call.property_start, call.property_end
             FROM member_calls call JOIN files file ON file.id=call.file_id
             WHERE call.rowid=?1 AND file.path=?2",
            params![occurrence.id, occurrence.file],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    let expected = (
        occurrence.file_id,
        occurrence.hash.clone(),
        occurrence.call_start,
        occurrence.call_end,
        occurrence.receiver_start,
        occurrence.receiver_end,
        occurrence.property_start,
        occurrence.property_end,
    );
    if current != Some(expected) {
        bail!("member-call occurrence changed while enrichment was running; published nothing");
    }
    let path = crate::store::file_source_path(conn, root, occurrence.file_id)?;
    verify_source_hash(&path, &occurrence.hash, &occurrence.file)
}

fn recheck_target(conn: &Connection, target: &Target) -> Result<()> {
    let current = conn
        .query_row(
            "SELECT file.hash, symbol.decl_start, symbol.decl_end
             FROM graph_nodes node
             JOIN symbols symbol ON node.native_table='symbols' AND node.native_id=symbol.id
             JOIN files file ON file.id=symbol.file_id
             WHERE node.node_key=?1",
            [&target.anchor],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((hash, start, end)) = current else {
        bail!("checker target anchor disappeared while enrichment was running; published nothing");
    };
    if target_fingerprint(&target.anchor, &hash, start, end) != target.fingerprint {
        bail!("checker target changed while enrichment was running; published nothing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    /// Index one file and hand back its connection plus its indexed hash.
    fn indexed(repo: &Path, source: &str) -> Result<(Connection, String)> {
        fs::write(repo.join("main.ts"), source)?;
        let conn = crate::store::open(repo)?;
        crate::indexer::index_repo(repo, &conn)?;
        let hash = conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
            row.get::<_, String>(0)
        })?;
        Ok((conn, hash))
    }

    /// The checker reports a declaration by its *name* span; `at` locates the
    /// same name the same way the sidecar's `declarationResult` does.
    fn declaration_at(source: &str, anchor: &str, name: &str, hash: &str) -> DeclarationSite {
        let start = source.find(anchor).expect("anchor present") as i64;
        DeclarationSite {
            file: Some("main.ts".into()),
            outside_root: false,
            start,
            end: start + name.len() as i64,
            source_hash: hash.to_string(),
        }
    }

    fn answer(declarations: Vec<DeclarationSite>) -> ProjectAnswer {
        ProjectAnswer {
            project_id: "tsconfig.json".into(),
            status: "resolved".into(),
            receiver_type: Some("CardTable".into()),
            declarations,
            checker_input_fingerprint: "inputs".into(),
        }
    }

    fn project_answer(project_id: &str, declarations: Vec<DeclarationSite>) -> ProjectAnswer {
        ProjectAnswer {
            project_id: project_id.into(),
            checker_input_fingerprint: format!("{project_id}-inputs"),
            ..answer(declarations)
        }
    }

    fn occurrence(conn: &Connection) -> Result<Occurrence> {
        Ok(load_occurrences(conn)?
            .into_iter()
            .find(|occurrence| occurrence.member == "insert")
            .expect("indexed insert occurrence"))
    }

    /// Anchoring by span containment alone let any declaration nested inside an
    /// indexed symbol's body claim that symbol: an object-literal method inside
    /// a function published a fabricated `likely` self-edge on the function.
    /// Mapping must require the symbol to BE the member's declaration.
    #[test]
    fn an_object_literal_method_inside_a_function_maps_to_nothing_instead_of_its_container()
    -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export function caller(): void {\n\
                      \x20 const rows = { insert(): void {} };\n\
                      \x20 rows.insert();\n\
                      }\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let literal_method_start =
            source.rfind("insert(): void {}").expect("literal method") as i64;
        let declaration = DeclarationSite {
            file: Some("main.ts".into()),
            outside_root: false,
            start: literal_method_start,
            end: literal_method_start + "insert".len() as i64,
            source_hash: hash,
        };

        // The containment trap is live: indexed symbols really do enclose this
        // declaration, so refusing it is a decision and not an accident.
        let containers: i64 = conn.query_row(
            "SELECT count(*) FROM symbols symbol JOIN files file ON file.id=symbol.file_id
             WHERE file.path='main.ts'
               AND symbol.decl_start<=?1 AND symbol.decl_end>=?2",
            params![declaration.start, declaration.end],
            |row| row.get(0),
        )?;
        assert!(containers > 0, "the enclosing symbol must exist");

        assert!(map_declaration(&conn, "insert", &declaration)?.is_none());
        let outcome = map_occurrence(&conn, &occurrence(&conn)?, &[answer(vec![declaration])])?;
        assert!(
            outcome.facts.is_empty(),
            "an unindexed declaration must publish no edge, least of all a self-edge"
        );
        assert_eq!(outcome.unmapped_declarations, 1);
        Ok(())
    }

    /// Ambiguity is judged over the checker's whole answer. Two valid targets
    /// where only one maps (jscout indexes no members for erased interfaces)
    /// must not collapse into a lone arbitrary `likely` edge.
    #[test]
    fn a_declaration_that_cannot_map_keeps_every_surviving_edge_possible() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export interface Vendor { insert(): void }\n\
                      declare const target: CardTable | Vendor;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
        assert!(
            map_declaration(&conn, "insert", &class_member)?.is_some(),
            "the class method is the mappable target"
        );
        assert!(
            map_declaration(&conn, "insert", &interface_member)?.is_none(),
            "the erased interface member has no indexed anchor"
        );
        let occurrence = occurrence(&conn)?;

        let ambiguous = map_occurrence(
            &conn,
            &occurrence,
            &[answer(vec![class_member.clone(), interface_member])],
        )?;
        assert_eq!(ambiguous.facts.len(), 1);
        assert_eq!(ambiguous.unmapped_declarations, 1);
        assert_eq!(
            ambiguous.facts[0].confidence, "possible",
            "a target the checker saw but jscout could not map still means ambiguity"
        );

        // Control: the same lone survivor is `likely` only when the checker
        // itself named exactly one target.
        let unambiguous = map_occurrence(&conn, &occurrence, &[answer(vec![class_member])])?;
        assert_eq!(unambiguous.facts.len(), 1);
        assert_eq!(unambiguous.unmapped_declarations, 0);
        assert_eq!(unambiguous.facts[0].confidence, "likely");
        Ok(())
    }

    #[test]
    fn later_project_cannot_upgrade_an_earlier_ambiguous_answer() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      export interface Vendor { insert(): void }\n\
                      declare const target: CardTable | Vendor;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
        let occurrence = occurrence(&conn)?;

        for answers in [
            vec![
                project_answer(
                    "tsconfig.a.json",
                    vec![class_member.clone(), interface_member.clone()],
                ),
                project_answer("tsconfig.b.json", vec![class_member.clone()]),
            ],
            vec![
                project_answer("tsconfig.b.json", vec![class_member.clone()]),
                project_answer(
                    "tsconfig.a.json",
                    vec![class_member.clone(), interface_member.clone()],
                ),
            ],
        ] {
            let outcome = map_occurrence(&conn, &occurrence, &answers)?;
            assert_eq!(outcome.facts.len(), 2);
            assert_eq!(outcome.unmapped_declarations, 1);
            assert!(
                outcome
                    .facts
                    .iter()
                    .all(|fact| fact.confidence == "possible"),
                "confidence must be independent of project answer order"
            );
        }
        Ok(())
    }

    #[test]
    fn an_unknown_owning_project_is_visible_without_demoting_a_clean_resolution() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let source = "export class CardTable { insert(): void {} }\n\
                      declare const target: CardTable;\n\
                      target.insert();\n";
        let (conn, hash) = indexed(repo.path(), source)?;
        let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
        let occurrence = occurrence(&conn)?;
        let unknown = ProjectAnswer {
            project_id: "tsconfig.unknown.json".into(),
            status: "unknown".into(),
            receiver_type: None,
            declarations: Vec::new(),
            checker_input_fingerprint: "unknown-inputs".into(),
        };
        let outcome = map_occurrence(
            &conn,
            &occurrence,
            &[
                project_answer("tsconfig.resolved.json", vec![class_member]),
                unknown,
            ],
        )?;
        assert_eq!(outcome.unknown_answers, 1);
        assert_eq!(outcome.facts.len(), 1);
        assert_eq!(outcome.facts[0].confidence, "likely");
        assert_eq!(outcome.projects.len(), 2);
        assert!(outcome.projects.iter().any(|project| {
            project.project_id == "tsconfig.unknown.json" && project.status == "unknown"
        }));
        Ok(())
    }

    #[test]
    fn raced_snapshot_or_checker_input_publishes_nothing() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        conn.execute(
            "INSERT INTO checker_enrichment_batches(
               source_snapshot, checker_version, checker_source,
               checker_input_fingerprint, sidecar_protocol, created_at, active
             ) VALUES(?1,'5.9.3','bundled','previous',1,datetime('now'),1)",
            [&snapshot],
        )?;
        let previous_batch = conn.last_insert_rowid();
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };

        let stale_snapshot = publish(
            repo.path(),
            &conn,
            &PublishPlan {
                snapshot: "stale",
                checker: &identity,
                protocol: 1,
                input_fingerprint: "new",
                facts: &[],
                projects: &[],
                inputs: &[],
            },
        )
        .expect_err("snapshot race");
        assert!(stale_snapshot.to_string().contains("snapshot changed"));

        let ambient = repo.path().join("ambient.d.ts");
        fs::write(&ambient, "declare const version: 1;\n")?;
        let expected = blake3::hash(&fs::read(&ambient)?).to_hex().to_string();
        fs::write(&ambient, "declare const version: 2;\n")?;
        let raced_input = publish(
            repo.path(),
            &conn,
            &PublishPlan {
                snapshot: &snapshot,
                checker: &identity,
                protocol: 1,
                input_fingerprint: "new",
                facts: &[],
                projects: &[],
                inputs: &[ValidatedInput {
                    kind: "absolute".into(),
                    path: ambient.to_string_lossy().into_owned(),
                    source_hash: expected,
                }],
            },
        )
        .expect_err("input race");
        assert!(raced_input.to_string().contains("changed since"));

        let batches: (i64, i64) = conn.query_row(
            "SELECT count(*), sum(active) FROM checker_enrichment_batches",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let active: i64 = conn.query_row(
            "SELECT active FROM checker_enrichment_batches WHERE id=?1",
            [previous_batch],
            |row| row.get(0),
        )?;
        assert_eq!(batches, (1, 1));
        assert_eq!(active, 1);
        Ok(())
    }

    /// Nothing reads a superseded batch, so a repeatedly enriched repository
    /// must not accumulate one dead batch and its facts per pass.
    #[test]
    fn publishing_a_batch_prunes_the_one_it_supersedes() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
        let conn = crate::store::open(repo.path())?;
        crate::indexer::index_repo(repo.path(), &conn)?;
        let snapshot = crate::structural::current_snapshot(&conn)?;
        let identity = TypeScriptIdentity {
            version: "5.9.3".into(),
            source: "bundled".into(),
        };
        let plan = |fingerprint: &'static str| PublishPlan {
            snapshot: &snapshot,
            checker: &identity,
            protocol: 1,
            input_fingerprint: fingerprint,
            facts: &[],
            projects: &[],
            inputs: &[],
        };

        let first = publish(repo.path(), &conn, &plan("one"))?;
        let second = publish(repo.path(), &conn, &plan("two"))?;
        let third = publish(repo.path(), &conn, &plan("three"))?;
        assert_ne!(first, second);

        let surviving: Vec<(i64, i64)> = conn
            .prepare("SELECT id, active FROM checker_enrichment_batches ORDER BY id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(surviving, vec![(third, 1)]);
        Ok(())
    }
}
