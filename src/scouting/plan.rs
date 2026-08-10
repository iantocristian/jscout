//! Deterministic workflow seed discovery and candidate/evidence planning.
//! Planning never starts the gateway and never makes a model call.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::Serialize;

use super::evidence::{self, EvidencePack};
use crate::semantic::{self, WorkflowCandidateOptions, WorkflowCandidateSet};
use crate::store;

const AUTO_SEED_LIMIT: usize = 256;

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
        let (mode, seed_groups, limit_reached) = if explicit_seeds.is_empty() {
            let discovered = automatic_seeds(root, conn)?;
            let limit_reached = discovered.len() > AUTO_SEED_LIMIT;
            (
                "automatic",
                discovered
                    .into_iter()
                    .take(AUTO_SEED_LIMIT)
                    .map(|(anchor, sources)| (vec![anchor], sources))
                    .collect(),
                limit_reached,
            )
        } else {
            (
                "explicit",
                vec![(explicit_seeds.to_vec(), vec!["agent-supplied".into()])],
                false,
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
        })
    })
}

/// Prefer actual runtime boundary endpoints. Exported symbols are included
/// only from conventional package/application entry files so a repository
/// with thousands of exports does not turn every utility into a workflow.
fn automatic_seeds(root: &Path, conn: &Connection) -> Result<Vec<(String, Vec<String>)>> {
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
        let left_export_only = left.1.iter().all(|source| source == "exported-entry-point");
        let right_export_only = right
            .1
            .iter()
            .all(|source| source == "exported-entry-point");
        left_export_only
            .cmp(&right_export_only)
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(seeds)
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
        assert!(first.duplicate_candidate_sets_skipped > 0);
        assert!(
            first
                .items
                .iter()
                .all(|item| item.sources == ["exported-entry-point"])
        );
        Ok(())
    }
}
