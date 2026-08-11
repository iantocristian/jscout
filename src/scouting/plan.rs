//! Deterministic subject discovery and candidate/evidence planning for both
//! scouting kinds. Planning never starts the gateway and never makes a model
//! call.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::Serialize;

use super::card::CardSubject;
use super::evidence::{self, EvidencePack};
use crate::semantic::{self, WorkflowCandidateOptions, WorkflowCandidateSet};
use crate::{origin, store, structural};

const AUTO_SEED_LIMIT: usize = 256;
/// Automatic card selection is capped so a large repository reports a visibly
/// capped plan instead of silently planning thousands of calls.
const CARD_LIMIT: usize = 1024;

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowPlanItem {
    pub seeds: Vec<String>,
    pub sources: Vec<String>,
    pub candidate_count: usize,
    pub candidate_fingerprint: String,
    pub evidence_files: usize,
    pub evidence_bytes: usize,
    #[serde(skip)]
    pub(crate) candidate_set: WorkflowCandidateSet,
    #[serde(skip)]
    pub(crate) evidence: EvidencePack,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowPlanSkip {
    pub seeds: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowPlan {
    pub mode: String,
    pub snapshot: String,
    pub items: Vec<WorkflowPlanItem>,
    pub skipped: Vec<WorkflowPlanSkip>,
    pub duplicate_candidate_sets_skipped: usize,
    pub auto_seed_limit: Option<usize>,
    pub auto_seed_limit_reached: bool,
    /// Automatic mode: how many boundary seeds were discovered before the
    /// limit was applied, so a capped plan is visibly capped.
    pub auto_seeds_discovered: Option<usize>,
}

/// Build exact candidate/evidence inputs. Explicit seeds form one workflow
/// boundary. With no explicit seeds, each deterministic repository surface is
/// attempted independently and equal candidate fingerprints are collapsed.
pub fn workflows(
    root: &Path,
    conn: &Connection,
    explicit_seeds: &[String],
    depth: usize,
    candidate_limit: usize,
) -> Result<WorkflowPlan> {
    store::with_read_snapshot(conn, "jscout_scout_plan", || {
        let (mode, seed_groups, limit_reached, discovered_count) = if explicit_seeds.is_empty() {
            let discovered = automatic_seeds(root, conn)?;
            let discovered_count = discovered.len();
            let limit_reached = discovered_count > AUTO_SEED_LIMIT;
            (
                "automatic",
                discovered
                    .into_iter()
                    .take(AUTO_SEED_LIMIT)
                    .map(|(anchor, sources)| (vec![anchor], sources))
                    .collect(),
                limit_reached,
                Some(discovered_count),
            )
        } else {
            (
                "explicit",
                vec![(explicit_seeds.to_vec(), vec!["agent-supplied".into()])],
                false,
                None,
            )
        };

        if seed_groups.is_empty() {
            bail!(
                "no deterministic workflow entry surfaces were found; pass --seed with a symbol anchor"
            );
        }

        let mut seen = HashSet::new();
        let mut duplicates = 0;
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        let mut snapshot = None;
        for (seeds, sources) in seed_groups {
            let candidate_set = semantic::workflow_candidates(
                root,
                conn,
                &seeds,
                &WorkflowCandidateOptions {
                    expected_snapshot: snapshot.clone(),
                    depth,
                    candidate_limit,
                },
            )?;
            if candidate_set.traversal_truncated || candidate_set.candidate_truncated {
                let reason = format!(
                    "candidate set is truncated (traversal: {}, candidates: {})",
                    candidate_set.traversal_truncated, candidate_set.candidate_truncated,
                );
                if mode == "explicit" {
                    bail!(
                        "candidate set for {} is truncated (traversal: {}, candidates: {}); narrow the seed/depth or raise the supported deterministic limit",
                        seeds.join(", "),
                        candidate_set.traversal_truncated,
                        candidate_set.candidate_truncated,
                    );
                }
                snapshot.get_or_insert_with(|| candidate_set.snapshot.clone());
                skipped.push(WorkflowPlanSkip { seeds, reason });
                continue;
            }
            snapshot.get_or_insert_with(|| candidate_set.snapshot.clone());
            if !seen.insert(candidate_boundary_fingerprint(&candidate_set)) {
                duplicates += 1;
                continue;
            }
            let evidence = evidence::build(root, conn, &candidate_set.candidates)?;
            items.push(WorkflowPlanItem {
                seeds: candidate_set.seeds.clone(),
                sources,
                candidate_count: candidate_set.candidates.len(),
                candidate_fingerprint: candidate_set.fingerprint.clone(),
                evidence_files: evidence.files.len(),
                evidence_bytes: evidence.rendered.len(),
                candidate_set,
                evidence,
            });
        }

        Ok(WorkflowPlan {
            mode: mode.into(),
            snapshot: snapshot.expect("non-empty workflow plan has a snapshot"),
            items,
            skipped,
            duplicate_candidate_sets_skipped: duplicates,
            auto_seed_limit: (mode == "automatic").then_some(AUTO_SEED_LIMIT),
            auto_seed_limit_reached: limit_reached,
            auto_seeds_discovered: discovered_count,
        })
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CardPlanItem {
    pub anchor: String,
    pub display_name: String,
    pub file: String,
    pub sources: Vec<String>,
    pub declaration_start_line: i64,
    pub declaration_end_line: i64,
    /// Depth-1 resolved edges rendered as deterministic context.
    pub context_edges: usize,
    pub evidence_bytes: usize,
    #[serde(skip)]
    pub(crate) snapshot: String,
    #[serde(skip)]
    pub(crate) evidence: EvidencePack,
}

impl CardPlanItem {
    pub(crate) fn subject(&self) -> CardSubject {
        CardSubject {
            anchor: self.anchor.clone(),
            display_name: self.display_name.clone(),
            file: self.file.clone(),
            declaration_start_line: self.declaration_start_line,
            declaration_end_line: self.declaration_end_line,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CardPlanSkip {
    pub anchor: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CardPlan {
    pub mode: String,
    pub snapshot: String,
    pub items: Vec<CardPlanItem>,
    pub skipped: Vec<CardPlanSkip>,
    pub anchor_limit: Option<usize>,
    pub anchor_limit_reached: bool,
    /// Automatic mode: subjects discovered before the limit was applied, so a
    /// capped plan is visibly capped.
    pub anchors_discovered: Option<usize>,
    /// Selection sources of the planned items.
    pub sources: BTreeMap<String, usize>,
    /// Automatic mode: selection sources of every discovered subject, so a
    /// capped plan also shows which sources it left out.
    pub discovered_sources: BTreeMap<String, usize>,
}

/// Build exact card subjects and their bounded evidence. Explicit anchors are
/// resolved like workflow seeds and each becomes its own run; automatic mode
/// selects exported symbols, runtime boundary endpoints, and participants of
/// current published workflows.
pub fn cards(root: &Path, conn: &Connection, explicit_anchors: &[String]) -> Result<CardPlan> {
    store::with_read_snapshot(conn, "jscout_card_plan", || {
        let mut discovered_sources: BTreeMap<String, usize> = BTreeMap::new();
        let (mode, selected, limit_reached, discovered_count) = if explicit_anchors.is_empty() {
            let discovered = automatic_card_subjects(conn)?;
            let discovered_count = discovered.len();
            for (_, sources) in &discovered {
                for source in sources {
                    *discovered_sources.entry(source.clone()).or_insert(0) += 1;
                }
            }
            (
                "automatic",
                discovered.into_iter().take(CARD_LIMIT).collect::<Vec<_>>(),
                discovered_count > CARD_LIMIT,
                Some(discovered_count),
            )
        } else {
            let mut resolved = explicit_anchors
                .iter()
                .map(|anchor| {
                    structural::resolve_current_anchor_in_origins(conn, anchor, &origin::defaults())
                        .map(|resolved| (resolved, vec!["agent-supplied".to_string()]))
                })
                .collect::<Result<Vec<_>>>()?;
            resolved.sort();
            resolved.dedup_by(|left, right| left.0 == right.0);
            ("explicit", resolved, false, None)
        };
        if selected.is_empty() {
            bail!("no deterministic card subjects were found; pass --anchor with a symbol anchor");
        }

        let snapshot = structural::current_snapshot(conn)?;
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        let mut sources = BTreeMap::new();
        for (anchor, anchor_sources) in selected {
            let Some(subject) = semantic::symbol_candidate(root, conn, &anchor)? else {
                if mode == "explicit" {
                    bail!(
                        "anchor `{anchor}` is not a file-backed symbol in the current snapshot; \
                         cards describe symbols"
                    );
                }
                skipped.push(CardPlanSkip {
                    anchor,
                    reason: "not a file-backed symbol in the current snapshot".into(),
                });
                continue;
            };
            let mut evidence =
                evidence::build_titled(root, conn, std::slice::from_ref(&subject), "Subject")?;
            let (context, context_edges) = evidence::structural_context(conn, &anchor)?;
            evidence.rendered.push_str(&context);
            for source in &anchor_sources {
                *sources.entry(source.clone()).or_insert(0) += 1;
            }
            items.push(CardPlanItem {
                anchor,
                display_name: subject.display_name,
                file: subject.file,
                sources: anchor_sources,
                declaration_start_line: subject.evidence_start_line,
                declaration_end_line: subject.evidence_end_line,
                context_edges,
                evidence_bytes: evidence.rendered.len(),
                snapshot: snapshot.clone(),
                evidence,
            });
        }

        Ok(CardPlan {
            mode: mode.into(),
            snapshot,
            items,
            skipped,
            anchor_limit: (mode == "automatic").then_some(CARD_LIMIT),
            anchor_limit_reached: limit_reached,
            anchors_discovered: discovered_count,
            sources,
            discovered_sources,
        })
    })
}

/// Union of the three deterministic card sources, deduped by anchor. Runtime
/// boundary endpoints rank first, then workflow participants, so a capped
/// plan keeps the symbols with the most established meaning.
fn automatic_card_subjects(conn: &Connection) -> Result<Vec<(String, Vec<String>)>> {
    let mut subjects = runtime_boundary_endpoints(conn)?;
    let mut statement = conn.prepare(
        "SELECT node.node_key
         FROM graph_nodes node
         JOIN symbols symbol ON symbol.id=node.native_id AND node.native_table='symbols'
         JOIN files file ON file.id=node.file_id
         WHERE node.node_kind='symbol' AND symbol.exported=1 AND symbol.scope_chain=''
           AND file.role='production' AND file.origin IN ('repository','workspace')
         ORDER BY node.node_key",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        subjects
            .entry(row?)
            .or_default()
            .push("exported-symbol".into());
    }

    // Participants of current published workflows already carry established
    // meaning; a card gives each participant its own evidence-backed record.
    let mut statement = conn.prepare(
        "SELECT DISTINCT support.anchor_key
         FROM semantic_supports support
         JOIN semantic_artifacts artifact ON artifact.id=support.artifact_id
         WHERE artifact.artifact_type='workflow'
           AND support.claim_path LIKE '/participants/%/role'
           AND NOT EXISTS(
             SELECT 1 FROM semantic_artifacts successor
             WHERE successor.supersedes_artifact_id=artifact.id
           )
         ORDER BY support.anchor_key",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        subjects
            .entry(row?)
            .or_default()
            .push("workflow-participant".into());
    }

    for sources in subjects.values_mut() {
        sources.sort();
        sources.dedup();
    }
    let mut subjects = subjects.into_iter().collect::<Vec<_>>();
    subjects.sort_by(|left, right| {
        card_priority(&left.1)
            .cmp(&card_priority(&right.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(subjects)
}

fn card_priority(sources: &[String]) -> u8 {
    if sources.iter().any(|source| source.starts_with("runtime:")) {
        0
    } else if sources
        .iter()
        .any(|source| source == "workflow-participant")
    {
        1
    } else {
        2
    }
}

/// Prefer actual runtime boundary endpoints. Exported symbols are included
/// only from conventional package/application entry files so a repository
/// with thousands of exports does not turn every utility into a workflow.
fn automatic_seeds(root: &Path, conn: &Connection) -> Result<Vec<(String, Vec<String>)>> {
    let mut seeds = runtime_boundary_endpoints(conn)?;
    let mut statement = conn.prepare(
        "SELECT node.node_key, file.path
         FROM graph_nodes node
         JOIN symbols symbol ON symbol.id=node.native_id AND node.native_table='symbols'
         JOIN files file ON file.id=node.file_id
         WHERE node.node_kind='symbol' AND symbol.exported=1 AND symbol.scope_chain=''
           AND file.role='production' AND file.origin IN ('repository','workspace')
         ORDER BY node.node_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let manifest_entries = crate::workspace::package_entry_paths(root);
    for row in rows {
        let (anchor, path) = row?;
        if manifest_entries.binary_search(&path).is_ok() || is_entry_file(&path) {
            seeds
                .entry(anchor)
                .or_default()
                .push("exported-entry-point".into());
        }
    }

    for sources in seeds.values_mut() {
        sources.sort();
        sources.dedup();
    }
    let mut seeds = seeds.into_iter().collect::<Vec<_>>();
    seeds.sort_by(|left, right| {
        seed_priority(&left.1)
            .cmp(&seed_priority(&right.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(seeds)
}

/// Production symbols on a runtime boundary edge, labelled with the edge kind
/// that put them there. Shared by workflow seeding and card selection.
fn runtime_boundary_endpoints(conn: &Connection) -> Result<BTreeMap<String, Vec<String>>> {
    let inbound = [
        "handles_route",
        "handles_graphql",
        "registered_handler",
        "lifecycle_listener",
        "job_handler",
        "provides",
    ];
    let outbound = [
        "dispatches",
        "produces_lifecycle",
        "produces_lifecycle_via",
        "produces_job",
        "produces_job_via",
        "injects",
        "invokes_graphql",
    ];
    let mut seeds: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT edge.src_key, edge.dst_key, edge.kind,
                src.node_kind, dst.node_kind,
                COALESCE(src_file.role, ''), COALESCE(src_file.origin, ''),
                COALESCE(dst_file.role, ''), COALESCE(dst_file.origin, '')
         FROM resolved_edges edge
         JOIN graph_nodes src ON src.node_key=edge.src_key
         JOIN graph_nodes dst ON dst.node_key=edge.dst_key
         LEFT JOIN files src_file ON src_file.id=src.file_id
         LEFT JOIN files dst_file ON dst_file.id=dst.file_id
         WHERE edge.kind IN (
           'handles_route','handles_graphql','registered_handler','lifecycle_listener',
           'job_handler','provides','dispatches','produces_lifecycle',
           'produces_lifecycle_via','produces_job','produces_job_via','injects',
           'invokes_graphql'
         )
         ORDER BY edge.kind, edge.src_key, edge.dst_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    for row in rows {
        let (src, dst, kind, src_kind, dst_kind, src_role, src_origin, dst_role, dst_origin) = row?;
        let endpoint = if inbound.contains(&kind.as_str()) {
            (dst, dst_kind, dst_role, dst_origin)
        } else if outbound.contains(&kind.as_str()) {
            (src, src_kind, src_role, src_origin)
        } else {
            continue;
        };
        if endpoint.1 != "symbol"
            || endpoint.2 != "production"
            || !matches!(endpoint.3.as_str(), "repository" | "workspace")
        {
            continue;
        }
        let sources = seeds.entry(endpoint.0).or_default();
        let source = format!("runtime:{kind}");
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    Ok(seeds)
}

fn seed_priority(sources: &[String]) -> u8 {
    if sources.iter().any(|source| {
        matches!(
            source.as_str(),
            "runtime:handles_route"
                | "runtime:handles_graphql"
                | "runtime:registered_handler"
                | "runtime:lifecycle_listener"
                | "runtime:job_handler"
                | "runtime:provides"
        )
    }) {
        0
    } else if sources.iter().any(|source| {
        matches!(
            source.as_str(),
            "runtime:dispatches"
                | "runtime:produces_lifecycle"
                | "runtime:produces_lifecycle_via"
                | "runtime:produces_job"
                | "runtime:produces_job_via"
                | "runtime:invokes_graphql"
        )
    }) {
        1
    } else if sources.iter().any(|source| source == "runtime:injects") {
        2
    } else {
        3
    }
}

fn is_entry_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file).to_ascii_lowercase();
    matches!(stem.as_str(), "index" | "main" | "server" | "app" | "entry")
}

/// Auto-selected entry points in the same closed structural neighborhood
/// should not spend separate calls merely because a different member was the
/// seed. The execution fingerprint remains seed-aware for exact run reuse.
fn candidate_boundary_fingerprint(set: &WorkflowCandidateSet) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"jscout-workflow-auto-boundary-v1\0");
    hasher.update(set.snapshot.as_bytes());
    for candidate in &set.candidates {
        hasher.update(b"\0");
        hasher.update(candidate.anchor.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::params;

    use crate::{indexer, store};

    #[test]
    fn automatic_plans_are_deterministic_and_dedupe_equal_boundaries() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("index.ts"),
            "export function first() { return shared(); }\n\
             export function second() { return shared(); }\n\
             function shared() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let first = super::workflows(repo.path(), &conn, &[], 2, 31)?;
        let second = super::workflows(repo.path(), &conn, &[], 2, 31)?;
        assert_eq!(
            serde_json::to_value(&first)?,
            serde_json::to_value(&second)?
        );
        assert_eq!(first.mode, "automatic");
        assert!(!first.items.is_empty());
        assert!(
            first
                .items
                .iter()
                .all(|item| item.sources == ["exported-entry-point"])
        );
        let original = &first.items[0].candidate_set;
        let mut alternate_seed = original.clone();
        alternate_seed.seeds = vec!["sym:index.ts#::alternate@1".into()];
        for candidate in &mut alternate_seed.candidates {
            candidate.seed = !candidate.seed;
        }
        assert_eq!(
            super::candidate_boundary_fingerprint(original),
            super::candidate_boundary_fingerprint(&alternate_seed),
            "automatic dedupe must use the closed member set, not seed identity",
        );
        Ok(())
    }

    #[test]
    fn automatic_seed_directions_follow_runtime_boundary_roles() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;
        conn.execute(
            "INSERT INTO files(path, hash, role) VALUES('runtime.ts','h','production')",
            [],
        )?;
        let production_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files(path, hash, role) VALUES('runtime.test.ts','t','test')",
            [],
        )?;
        let test_file = conn.last_insert_rowid();
        for (key, kind, file) in [
            (
                "sym:runtime.ts#::dispatch@1",
                "symbol",
                Some(production_file),
            ),
            ("sym:runtime.ts#::inject@3", "symbol", Some(production_file)),
            ("sym:runtime.ts#::handle@2", "symbol", Some(production_file)),
            (
                "sym:runtime.test.ts#::testOnly@1",
                "symbol",
                Some(test_file),
            ),
            ("entity:registry:job", "entity", None),
        ] {
            conn.execute(
                "INSERT INTO graph_nodes(node_key,node_kind,display_name,file_id,meta_json)
                 VALUES(?1,?2,?1,?3,'{}')",
                params![key, kind, file],
            )?;
        }
        for (src, dst, kind) in [
            (
                "sym:runtime.ts#::dispatch@1",
                "entity:registry:job",
                "dispatches",
            ),
            (
                "entity:registry:job",
                "sym:runtime.ts#::handle@2",
                "registered_handler",
            ),
            (
                "entity:registry:job",
                "sym:runtime.ts#::handle@2",
                "handles_route",
            ),
            (
                "sym:runtime.test.ts#::testOnly@1",
                "entity:registry:job",
                "produces_job",
            ),
            (
                "sym:runtime.ts#::inject@3",
                "entity:registry:job",
                "injects",
            ),
        ] {
            conn.execute(
                "INSERT INTO resolved_edges(src_key,dst_key,kind,confidence,provenance)
                 VALUES(?1,?2,?3,'certain','test')",
                params![src, dst, kind],
            )?;
        }

        let seeds = super::automatic_seeds(repo.path(), &conn)?;
        assert_eq!(seeds.len(), 3, "test-role endpoints stay out of auto seeds");
        assert_eq!(seeds[0].0, "sym:runtime.ts#::handle@2");
        assert_eq!(
            seeds[0].1,
            ["runtime:handles_route", "runtime:registered_handler"]
        );
        assert_eq!(seeds[1].0, "sym:runtime.ts#::dispatch@1");
        assert_eq!(seeds[1].1, ["runtime:dispatches"]);
        assert_eq!(seeds[2].0, "sym:runtime.ts#::inject@3");
        assert_eq!(seeds[2].1, ["runtime:injects"]);
        Ok(())
    }

    #[test]
    fn automatic_card_selection_unions_its_sources_deterministically() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("index.ts"),
            "export function first() { return helper(); }\n\
             function helper() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let first = super::cards(repo.path(), &conn, &[])?;
        let second = super::cards(repo.path(), &conn, &[])?;
        assert_eq!(
            serde_json::to_value(&first)?,
            serde_json::to_value(&second)?,
            "card planning must be deterministic"
        );
        assert_eq!(first.mode, "automatic");
        assert_eq!(first.items.len(), 1, "only exported symbols are subjects");
        assert_eq!(first.items[0].sources, ["exported-symbol"]);
        assert_eq!(first.sources["exported-symbol"], 1);
        assert_eq!(first.anchors_discovered, Some(1));
        assert!(!first.anchor_limit_reached);
        assert!(
            first.items[0].evidence.rendered.contains("## Subject"),
            "a card pack leads with its subject, not a candidate set"
        );
        assert!(
            first.items[0]
                .evidence
                .rendered
                .contains("## Direct structural context"),
        );
        assert!(first.items[0].context_edges > 0, "helper call is depth-1");

        // A published workflow participant becomes a card subject even when it
        // is not exported.
        let helper = "sym:index.ts#::helper@1";
        let snapshot = crate::structural::current_snapshot(&conn)?;
        conn.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type,canonical_name,body_json,model,prompt_version,confidence,
               source_snapshot,created_at
             ) VALUES('workflow','flow','{\"participants\":[{\"role\":\"helper\"}]}',
                      'agent-reported','annotate/v2','likely',?1,'2026-08-10T00:00:00Z')",
            params![snapshot],
        )?;
        let artifact_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
               evidence_end_line,source_hash,context_hash,confidence
             ) VALUES(?1,'/participants/0/role',?2,'index.ts',2,2,'h','c','likely')",
            params![artifact_id, helper],
        )?;
        let widened = super::cards(repo.path(), &conn, &[])?;
        assert_eq!(widened.items.len(), 2);
        let participant = widened
            .items
            .iter()
            .find(|item| item.anchor == helper)
            .expect("participant subject");
        assert_eq!(participant.sources, ["workflow-participant"]);
        assert_eq!(
            widened.items[0].anchor, helper,
            "workflow participants outrank plain exports in a capped plan"
        );
        Ok(())
    }

    #[test]
    fn automatic_card_selection_reports_its_cap() -> Result<()> {
        // Every subject's pack renders its whole declaring file, so the
        // fixture spreads symbols over files: one huge file would make this
        // test quadratic in the cap.
        const PER_FILE: usize = 16;
        let files = (super::CARD_LIMIT + 4).div_ceil(PER_FILE);
        let discovered = files * PER_FILE;
        let repo = tempfile::tempdir()?;
        for file in 0..files {
            let mut source = String::new();
            for index in 0..PER_FILE {
                source.push_str(&format!(
                    "export function symbol{file}_{index}() {{ return {index}; }}\n"
                ));
            }
            std::fs::write(repo.path().join(format!("module{file}.ts")), source)?;
        }
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let plan = super::cards(repo.path(), &conn, &[])?;
        assert_eq!(plan.anchors_discovered, Some(discovered));
        assert_eq!(plan.anchor_limit, Some(super::CARD_LIMIT));
        assert!(plan.anchor_limit_reached);
        assert_eq!(plan.items.len(), super::CARD_LIMIT);
        assert_eq!(
            plan.discovered_sources["exported-symbol"], discovered,
            "a capped plan still reports everything discovery found"
        );
        assert_eq!(plan.sources["exported-symbol"], super::CARD_LIMIT);
        Ok(())
    }

    #[test]
    fn explicit_card_anchors_resolve_uniquely_or_fail_loudly() -> Result<()> {
        let repo = tempfile::tempdir()?;
        std::fs::write(
            repo.path().join("index.ts"),
            "export function only() { return 1; }\n",
        )?;
        std::fs::write(
            repo.path().join("other.ts"),
            "export function shared() { return 1; }\n",
        )?;
        std::fs::write(
            repo.path().join("second.ts"),
            "export function shared() { return 2; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let plan = super::cards(repo.path(), &conn, &["index.ts:only".into()])?;
        assert_eq!(plan.mode, "explicit");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].sources, ["agent-supplied"]);
        assert_eq!(plan.items[0].file, "index.ts");

        // Repeating one anchor still spends one run.
        let repeated = super::cards(
            repo.path(),
            &conn,
            &["index.ts:only".into(), "index.ts:only".into()],
        )?;
        assert_eq!(repeated.items.len(), 1);

        let error =
            super::cards(repo.path(), &conn, &["shared".into()]).expect_err("ambiguous anchor");
        assert!(error.to_string().contains("ambiguous"));
        let error = super::cards(repo.path(), &conn, &["file:index.ts".into()])
            .expect_err("file anchors are not card subjects");
        assert!(error.to_string().contains("not a file-backed symbol"));
        Ok(())
    }
}
