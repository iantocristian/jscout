//! Run-ledger persistence. One row per model run; the partial unique index on
//! (scout_kind, input_fingerprint) over running/completed rows guarantees a
//! single live claim per input across processes: concurrent scouts either
//! reuse the completed run or fail loudly against the in-flight one.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub scout_kind: String,
    pub gateway_protocol: u32,
    pub provider: String,
    pub model: String,
    pub billing_path: String,
    pub reasoning: Option<String>,
    pub prompt_version: String,
    pub source_snapshot: String,
    pub input_fingerprint: String,
    pub request_hash: String,
    pub config_json: String,
    /// Explicit current artifact replaced by a refresh. Regular rebuilds
    /// discover their predecessor from the matching input fingerprint.
    pub supersedes_artifact_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClaim {
    /// A completed run with identical inputs exists; reuse its outputs.
    Reused(i64),
    /// This process now owns the only live run for these inputs.
    Claimed {
        run_id: i64,
        supersedes_artifact_id: Option<i64>,
    },
}

/// Terminal states. `Completed` is the only reusable state; everything else
/// releases the input fingerprint for a later attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Incomplete,
    Failed,
    Canceled,
}

impl RunOutcome {
    fn as_str(self) -> &'static str {
        match self {
            RunOutcome::Completed => "completed",
            RunOutcome::Incomplete => "incomplete",
            RunOutcome::Failed => "failed",
            RunOutcome::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClassificationRow {
    pub anchor_key: String,
    pub decision: String,
    pub role: Option<String>,
    /// Evidence line ranges for included candidates; `{"reason": ...}` for
    /// exclusions, which never become semantic supports.
    pub evidence_json: String,
}

/// Claim a run for these inputs, reusing a completed identical run unless
/// `rebuild` is set (which supersedes it first). Runs its own transaction;
/// callers must not hold one.
pub fn claim_run(conn: &Connection, spec: &RunSpec, rebuild: bool) -> Result<RunClaim> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let claim = (|| -> Result<RunClaim> {
        let current_artifact = current_artifact_for_input(conn, spec)?;
        let supersedes_artifact_id = if rebuild {
            conn.execute(
                "UPDATE scout_runs SET status='superseded'
                 WHERE scout_kind=?1 AND input_fingerprint=?2 AND status='completed'",
                params![spec.scout_kind, spec.input_fingerprint],
            )?;
            spec.supersedes_artifact_id.or(current_artifact)
        } else if let Some(existing) = reusable_run(conn, spec)? {
            return Ok(RunClaim::Reused(existing));
        } else {
            // A previous rebuild attempt may have superseded the run before
            // failing. Its artifact remains current and the retry must still
            // replace it rather than creating a parallel current record.
            spec.supersedes_artifact_id.or(current_artifact)
        };
        let inserted = conn.execute(
            "INSERT INTO scout_runs(
               scout_kind, status, gateway_protocol, provider, model, billing_path,
               reasoning, prompt_version, source_snapshot, input_fingerprint,
               request_hash, config_json, started_at
             ) VALUES(?1,'running',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,
                      strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![
                spec.scout_kind,
                spec.gateway_protocol,
                spec.provider,
                spec.model,
                spec.billing_path,
                spec.reasoning,
                spec.prompt_version,
                spec.source_snapshot,
                spec.input_fingerprint,
                spec.request_hash,
                spec.config_json,
            ],
        );
        match inserted {
            Ok(_) => Ok(RunClaim::Claimed {
                run_id: conn.last_insert_rowid(),
                supersedes_artifact_id,
            }),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                bail!(
                    "another {} scout run is already in progress for these inputs; \
                     wait for it or clear it with sweep_orphaned_runs",
                    spec.scout_kind
                );
            }
            Err(error) => Err(error).context("insert scout run"),
        }
    })();
    match claim {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn current_artifact_for_input(conn: &Connection, spec: &RunSpec) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT artifact.id
             FROM semantic_artifacts artifact
             JOIN scout_runs run ON run.id=artifact.scout_run_id
             WHERE run.scout_kind=?1 AND run.input_fingerprint=?2
               AND NOT EXISTS(
                 SELECT 1 FROM semantic_artifacts successor
                 WHERE successor.supersedes_artifact_id=artifact.id
               )
             ORDER BY artifact.id DESC LIMIT 1",
            params![spec.scout_kind, spec.input_fingerprint],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn reusable_run(conn: &Connection, spec: &RunSpec) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM scout_runs
             WHERE scout_kind=?1 AND input_fingerprint=?2 AND status='completed'",
            params![spec.scout_kind, spec.input_fingerprint],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?)
}

/// Terminal transition. Safe inside a caller-owned transaction: this is a
/// plain UPDATE so G4 can commit it atomically with artifact publication.
pub fn finish_run(
    conn: &Connection,
    run_id: i64,
    outcome: RunOutcome,
    usage_json: Option<&str>,
    error_code: Option<&str>,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE scout_runs
         SET status=?2, usage_json=COALESCE(?3, usage_json), error_code=?4,
             completed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 AND status='running'",
        params![run_id, outcome.as_str(), usage_json, error_code],
    )?;
    if updated != 1 {
        bail!("scout run {run_id} is not in the running state");
    }
    Ok(())
}

/// Record the model's decision for every deterministic candidate of a run.
pub fn record_classifications(
    conn: &Connection,
    run_id: i64,
    classifications: &[ClassificationRow],
) -> Result<()> {
    let mut statement = conn.prepare_cached(
        "INSERT INTO scout_classifications(run_id, anchor_key, decision, role, evidence_json)
         VALUES(?1,?2,?3,?4,?5)",
    )?;
    for row in classifications {
        statement.execute(params![
            run_id,
            row.anchor_key,
            row.decision,
            row.role,
            row.evidence_json,
        ])?;
    }
    Ok(())
}

/// Mark long-dead `running` rows failed so their input fingerprints become
/// claimable again after a crash. Called at scout-command startup.
pub fn sweep_orphaned_runs(conn: &Connection, older_than_minutes: i64) -> Result<usize> {
    let swept = conn.execute(
        "UPDATE scout_runs
         SET status='failed', error_code='orphaned',
             completed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE status='running'
           AND started_at < strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1)",
        params![format!("-{older_than_minutes} minutes")],
    )?;
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{ClassificationRow, RunClaim, RunOutcome, RunSpec, claim_run, finish_run};
    use crate::store;

    fn spec(fingerprint: &str) -> RunSpec {
        RunSpec {
            scout_kind: "workflow".into(),
            gateway_protocol: 1,
            provider: "openai-codex".into(),
            model: "gpt-5.6-terra".into(),
            billing_path: "plan".into(),
            reasoning: Some("high".into()),
            prompt_version: "workflow/v1".into(),
            source_snapshot: "snap".into(),
            input_fingerprint: fingerprint.into(),
            request_hash: "req".into(),
            config_json: "{}".into(),
            supersedes_artifact_id: None,
        }
    }

    #[test]
    fn claims_reuses_supersedes_and_rejects_concurrent_runs() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;

        let RunClaim::Claimed {
            run_id: first,
            supersedes_artifact_id: None,
        } = claim_run(&conn, &spec("f1"), false)?
        else {
            panic!("expected a fresh claim");
        };
        // A second claim against the in-flight run fails loudly.
        let error = claim_run(&conn, &spec("f1"), false).expect_err("running conflict");
        assert!(error.to_string().contains("already in progress"));

        finish_run(
            &conn,
            first,
            RunOutcome::Completed,
            Some("{\"total\":3}"),
            None,
        )?;
        assert_eq!(
            claim_run(&conn, &spec("f1"), false)?,
            RunClaim::Reused(first)
        );

        // --rebuild supersedes the completed run and claims a fresh one.
        let RunClaim::Claimed {
            run_id: second,
            supersedes_artifact_id: None,
        } = claim_run(&conn, &spec("f1"), true)?
        else {
            panic!("expected a rebuild claim");
        };
        assert_ne!(first, second);
        let first_status: String = conn.query_row(
            "SELECT status FROM scout_runs WHERE id=?1",
            [first],
            |row| row.get(0),
        )?;
        assert_eq!(first_status, "superseded");

        // Failed runs release the fingerprint without becoming reusable.
        finish_run(&conn, second, RunOutcome::Failed, None, Some("timeout"))?;
        let RunClaim::Claimed {
            run_id: third,
            supersedes_artifact_id: None,
        } = claim_run(&conn, &spec("f1"), false)?
        else {
            panic!("expected a fresh claim after failure");
        };
        assert_ne!(second, third);
        finish_run(&conn, third, RunOutcome::Canceled, None, None)?;

        // Terminal transitions are one-way.
        assert!(finish_run(&conn, third, RunOutcome::Completed, None, None).is_err());
        Ok(())
    }

    #[test]
    fn records_every_candidate_decision_including_exclusions() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;
        let RunClaim::Claimed {
            run_id: run,
            supersedes_artifact_id: None,
        } = claim_run(&conn, &spec("f2"), false)?
        else {
            panic!("expected a claim");
        };
        super::record_classifications(
            &conn,
            run,
            &[
                ClassificationRow {
                    anchor_key: "sym:a.ts#::start@1".into(),
                    decision: "defining".into(),
                    role: Some("initiates the retry".into()),
                    evidence_json: "[{\"start_line\":1,\"end_line\":4}]".into(),
                },
                ClassificationRow {
                    anchor_key: "sym:b.ts#::log@1".into(),
                    decision: "excluded".into(),
                    role: None,
                    evidence_json: "{\"reason\":\"generic logging helper\"}".into(),
                },
            ],
        )?;
        let decisions: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT anchor_key, decision FROM scout_classifications
                 WHERE run_id=?1 ORDER BY anchor_key",
            )?;
            let rows = stmt.query_map([run], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].1, "defining");
        assert_eq!(decisions[1].1, "excluded");

        // Unknown decisions are schema violations, not silent writes.
        let invalid = super::record_classifications(
            &conn,
            run,
            &[ClassificationRow {
                anchor_key: "sym:c.ts#::x@1".into(),
                decision: "maybe".into(),
                role: None,
                evidence_json: "{}".into(),
            }],
        );
        assert!(invalid.is_err());
        Ok(())
    }

    #[test]
    fn sweeps_only_long_dead_running_rows() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;
        let RunClaim::Claimed {
            run_id: fresh,
            supersedes_artifact_id: None,
        } = claim_run(&conn, &spec("f3"), false)?
        else {
            panic!("expected a claim");
        };
        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind, status, gateway_protocol, provider, model, billing_path,
               prompt_version, source_snapshot, input_fingerprint, request_hash, started_at
             ) VALUES('workflow','running',1,'p','m','api','v','s','stale-f','r',
                      strftime('%Y-%m-%dT%H:%M:%fZ','now','-3 hours'))",
            [],
        )?;
        assert_eq!(super::sweep_orphaned_runs(&conn, 60)?, 1);
        let fresh_status: String = conn.query_row(
            "SELECT status FROM scout_runs WHERE id=?1",
            [fresh],
            |row| row.get(0),
        )?;
        assert_eq!(fresh_status, "running");
        let swept: (String, String) = conn.query_row(
            "SELECT status, error_code FROM scout_runs WHERE input_fingerprint='stale-f'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(swept, ("failed".into(), "orphaned".into()));
        Ok(())
    }
}
