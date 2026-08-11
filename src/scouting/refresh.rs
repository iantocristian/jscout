//! Selection of current generated artifacts whose evidence is no longer
//! wholly fresh. Execution stays in the parent orchestrator so every refresh
//! uses the same candidate closure and publication path as an initial scout.

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Serialize;

use super::{CardRunConfig, WorkflowRunConfig};
use crate::llm::config::ModelSpec;
use crate::semantic;

/// Recorded replay configuration, one variant per scouted artifact type. The
/// scout kind of the producing run decides which shape is authoritative.
#[derive(Debug, Clone)]
pub enum RefreshConfig {
    Workflow(WorkflowRunConfig),
    Card(CardRunConfig),
}

impl RefreshConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            RefreshConfig::Workflow(_) => "workflow",
            RefreshConfig::Card(_) => "card",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefreshTarget {
    pub artifact_id: i64,
    pub freshness: String,
    pub model: ModelSpec,
    pub reasoning: Option<String>,
    pub config: RefreshConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshSelectionSummary {
    pub selected: usize,
    pub skipped_fresh: Vec<i64>,
    pub unsupported_legacy: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct RefreshSelection {
    pub targets: Vec<RefreshTarget>,
    pub summary: RefreshSelectionSummary,
}

/// Select current model-generated workflow and card artifacts. Explicit IDs
/// are validated against that same boundary; fresh artifacts are never
/// refreshed.
pub fn select(conn: &Connection, requested_ids: &[i64]) -> Result<RefreshSelection> {
    let mut ids = if requested_ids.is_empty() {
        let mut statement = conn.prepare(
            "SELECT artifact.id
             FROM semantic_artifacts artifact
             JOIN scout_runs run ON run.id=artifact.scout_run_id
             WHERE artifact.artifact_type=run.scout_kind
               AND run.scout_kind IN ('workflow','card')
               AND NOT EXISTS(
                 SELECT 1 FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id
               )
             ORDER BY artifact.id",
        )?;
        statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let mut ids = requested_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    ids.sort_unstable();

    let mut targets = Vec::new();
    let mut skipped_fresh = Vec::new();
    let mut unsupported_legacy = Vec::new();
    for id in ids {
        let row = conn.query_row(
            "SELECT run.provider, run.model, run.reasoning, run.config_json,
                    artifact.artifact_type, run.scout_kind,
                    EXISTS(
                      SELECT 1 FROM semantic_artifacts successor
                      WHERE successor.supersedes_artifact_id=artifact.id
                    )
             FROM semantic_artifacts artifact
             JOIN scout_runs run ON run.id=artifact.scout_run_id
             WHERE artifact.id=?1 AND run.scout_kind IN ('workflow','card')",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            },
        );
        let (provider, model, reasoning, config_json, artifact_type, scout_kind, has_successor) =
            row.with_context(|| {
                format!("artifact {id} is not a model-generated artifact or does not exist")
            })?;
        if artifact_type != scout_kind || has_successor {
            bail!("artifact {id} is not a current generated workflow or card");
        }
        let artifact = semantic::load_artifact(conn, id)?
            .with_context(|| format!("semantic artifact {id} disappeared"))?;
        if artifact.freshness == "fresh" {
            skipped_fresh.push(id);
            continue;
        }
        let Some(config) = replay_config(&scout_kind, &config_json) else {
            unsupported_legacy.push(id);
            continue;
        };
        targets.push(RefreshTarget {
            artifact_id: id,
            freshness: artifact.freshness,
            model: ModelSpec::parse(&format!("{provider}:{model}"))?,
            reasoning,
            config,
        });
    }
    Ok(RefreshSelection {
        summary: RefreshSelectionSummary {
            selected: targets.len(),
            skipped_fresh,
            unsupported_legacy,
        },
        targets,
    })
}

/// Runs created before replay configuration was stored, or with a shape that
/// no longer parses, are reported rather than replayed against a guess.
fn replay_config(scout_kind: &str, config_json: &str) -> Option<RefreshConfig> {
    match scout_kind {
        "workflow" => {
            let config = serde_json::from_str::<WorkflowRunConfig>(config_json).ok()?;
            (!config.seeds.is_empty()).then_some(RefreshConfig::Workflow(config))
        }
        "card" => {
            let config = serde_json::from_str::<CardRunConfig>(config_json).ok()?;
            (!config.anchor.is_empty()).then_some(RefreshConfig::Card(config))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::store;

    #[test]
    fn selects_only_current_stale_generated_workflows_with_replay_config() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;
        conn.execute(
            "INSERT INTO files(path, hash, role) VALUES('flow.ts','new','production')",
            [],
        )?;
        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind,status,gateway_protocol,provider,model,billing_path,reasoning,
               prompt_version,source_snapshot,input_fingerprint,request_hash,config_json,started_at
             ) VALUES(
               'workflow','completed',1,'faux','model','api',NULL,'workflow-scout/v1',
               'old-snapshot','input','request',
               '{\"seeds\":[\"sym:flow.ts#::start@1\"],\"depth\":2,\"candidate_limit\":31,\"service_tier\":null}',
               '2026-08-10T00:00:00Z'
             )",
            [],
        )?;
        let run_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type,canonical_name,body_json,model,prompt_version,confidence,
               source_snapshot,created_at,scout_run_id,input_fingerprint,artifact_fingerprint
             ) VALUES('workflow','flow','{\"description\":\"flow\",\"participants\":[]}',
                      'faux:model','workflow-scout/v1','likely','old-snapshot',
                      '2026-08-10T00:00:00Z',?1,'input','artifact')",
            [run_id],
        )?;
        let artifact_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id,claim_path,anchor_key,role,evidence_file,evidence_start_line,
               evidence_end_line,source_hash,context_hash,confidence
             ) VALUES(?1,'/description','sym:flow.ts#::start@1',NULL,'flow.ts',1,1,
                      'old','context','likely')",
            [artifact_id],
        )?;

        let selected = super::select(&conn, &[])?;
        assert_eq!(selected.targets.len(), 1);
        assert_eq!(selected.targets[0].artifact_id, artifact_id);
        assert_eq!(selected.targets[0].freshness, "stale");
        let super::RefreshConfig::Workflow(config) = &selected.targets[0].config else {
            panic!("expected a workflow replay configuration");
        };
        assert_eq!(config.depth, 2);
        Ok(())
    }
}
