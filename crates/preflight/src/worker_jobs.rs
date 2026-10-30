//! Durable single-attempt worker jobs. Lost claims are fenced and never auto-replayed.
use crate::{
    db::Db,
    model::{CommandSpec, Fault, RunState, invalid},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const PROTOCOL: u32 = 3;
pub const CONTAINER_PROTOCOL: u32 = 4;
pub fn compatible(value: Option<u32>) -> bool {
    matches!(value, Some(PROTOCOL | CONTAINER_PROTOCOL))
}
pub const LEASE_SECONDS: i64 = 15;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
    pub command: CommandSpec,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub returns: BTreeMap<String, crate::pipeline::ArtifactKind>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub spec: crate::docker::Spec,
    pub memory_mib: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub protocol: u32,
    pub claim_secret: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renew {
    pub protocol: u32,
    pub job_id: String,
    pub claim_secret: String,
    pub sequence: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_receipt: Option<crate::worker_receipts::Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_cursor: Option<i64>,
    pub status: RunState,
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finish {
    pub protocol: u32,
    pub job_id: String,
    pub claim_secret: String,
    pub outcome: Outcome,
}
pub(crate) fn auth(tx: &Transaction<'_>, credential: &str) -> Result<(String, u64)> {
    tx.query_row(
        "SELECT id,generation FROM workers WHERE credential_hash=?1 AND state='registered'",
        [crate::workers::hash(credential)?],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Fault::NotFound.into())
}
fn terminal(state: &str) -> bool {
    !matches!(state, "queued" | "leased" | "cancelling")
}
pub(crate) fn sweep(tx: &Transaction<'_>) -> Result<()> {
    let now = crate::workers::now();
    // Unclaimed cancellation never had a remote process. Claimed uncertainty retains protection.
    tx.execute("UPDATE worker_jobs SET state='cancelled',reason='run cancelled before claim' WHERE state='queued' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=run_id AND r.cancel_actor IS NOT NULL)",[])?;
    tx.execute("UPDATE worker_jobs SET state='cancelling',reason='run cancellation requested' WHERE state='leased' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=run_id AND r.cancel_actor IS NOT NULL)",[])?;
    tx.execute("UPDATE worker_jobs SET state='lost',reason='worker lease, registration or dispatch deadline expired' WHERE state IN ('queued','leased','cancelling') AND (deadline<=?1 OR (lease_expires_at IS NOT NULL AND lease_expires_at<=?1) OR NOT EXISTS(SELECT 1 FROM workers w WHERE w.id=worker_id AND w.generation=worker_jobs.generation AND w.state='registered') OR (state='queued' AND NOT EXISTS(SELECT 1 FROM worker_transfers t WHERE t.id=transfer_id AND t.expires_at>?1)))",[now])?;
    tx.execute("UPDATE operation_leases SET state=CASE WHEN EXISTS(SELECT 1 FROM worker_jobs j WHERE j.operation_id=operation_leases.id AND j.state='lost' AND j.claim_hash IS NOT NULL) THEN 'abandoned' ELSE 'released' END,finished_at=?1 WHERE state='active' AND id IN (SELECT operation_id FROM worker_jobs WHERE state IN ('lost','cancelled'))",[crate::db::now()])?;
    Ok(())
}
impl Db {
    /// Internal engine boundary; never accepts arbitrary remote commands from the worker gateway.
    pub fn worker_job_enqueue(
        &self,
        run: &str,
        stage: &str,
        transfer: &str,
        spec: &Spec,
    ) -> Result<Value> {
        crate::pipeline::identifier(stage)?;
        crate::pipeline::relative(&spec.cwd)?;
        ensure!(
            !spec.command.program.is_empty()
                && !spec.command.program.contains('\0')
                && (1..=3600).contains(&spec.command.timeout_seconds),
            invalid("invalid worker command")
        );
        crate::worker_outputs::validate_paths(&spec.returns)?;
        if let Some(container) = &spec.container {
            container.spec.validate()?;
            ensure!(
                container.spec.image_from.is_none(),
                invalid("worker container image must already be resolved")
            );
            ensure!(
                (16..=1048576).contains(&container.memory_mib),
                invalid("invalid worker memory limit")
            );
        }
        let encoded = serde_json::to_string(spec)?;
        ensure!(
            encoded.len() <= 16384,
            invalid("worker job specification exceeds 16 KiB")
        );
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx)?;
        let old: Option<(String, String, String)> = tx
            .query_row(
                "SELECT id,transfer_id,spec_json FROM worker_jobs WHERE run_id=?1 AND stage_id=?2",
                params![run, stage],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, previous_transfer, previous_spec)) = old {
            ensure!(
                previous_transfer == transfer && previous_spec == encoded,
                Fault::IdempotencyConflict
            );
            return Ok(json!({"job_id":id}));
        }
        let target:Option<(String,u64,String)>=tx.query_row("SELECT t.worker_id,t.generation,t.run_id FROM worker_transfers t JOIN runs source ON source.id=t.run_id JOIN runs target ON target.id=?2 JOIN stage_runs s ON s.run_id=target.id AND s.name=?3 JOIN workers w ON w.id=t.worker_id WHERE t.id=?1 AND t.expires_at>?4 AND w.generation=t.generation AND w.state='registered' AND source.project_id=target.project_id AND target.status IN ('queued','preparing','running','collecting') AND target.cancel_actor IS NULL AND s.status IN ('queued','preparing','running')",params![transfer,run,stage,crate::workers::now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (worker, generation, source_run) = target.context(
            "worker job requires a current transfer and unfinished stage in the same project",
        )?;
        if spec.container.is_some() {
            let protocol: Option<u32> = tx.query_row(
                "SELECT job_protocol FROM workers WHERE id=?1",
                [&worker],
                |r| r.get(0),
            )?;
            ensure!(
                protocol == Some(CONTAINER_PROTOCOL),
                invalid("worker does not advertise container execution")
            );
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM worker_jobs WHERE state IN ('queued','leased','cancelling')",
            [],
            |r| r.get(0),
        )?;
        ensure!(count < 32, Fault::Busy);
        let id = uuid::Uuid::now_v7().to_string();
        let operation = uuid::Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO operation_leases VALUES (?1,?2,'worker_job','active',?3,NULL)",
            params![operation, run, crate::db::now()],
        )?;
        for resource in std::collections::BTreeSet::from([
            format!("run:{run}"),
            format!("run:{source_run}"),
            format!("worker-transfer:{transfer}"),
        ]) {
            tx.execute(
                "INSERT INTO operation_resources VALUES (?1,?2)",
                params![operation, resource],
            )?;
        }
        tx.execute("INSERT INTO worker_jobs(id,run_id,stage_id,worker_id,generation,transfer_id,operation_id,spec_json,state,deadline) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'queued',?9)",params![id,run,stage,worker,generation,transfer,operation,encoded,crate::workers::now()+spec.command.timeout_seconds as i64+120])?;
        tx.commit()?;
        Ok(json!({"job_id":id}))
    }
    pub fn worker_job_claim(&self, credential: &str, r: &Claim) -> Result<Value> {
        ensure!(
            compatible(Some(r.protocol)),
            invalid("unsupported worker job protocol")
        );
        let claim = crate::workers::hash(&r.claim_secret)?;
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx)?;
        let (worker, generation) = auth(&tx, credential)?;
        let existing:Option<(String,String)>=tx.query_row("SELECT id,state FROM worker_jobs WHERE worker_id=?1 AND generation=?2 AND claim_hash=?3",params![worker,generation,claim],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let id = if let Some((id, state)) = existing {
            ensure!(
                state == "leased" || state == "cancelling",
                invalid("claim already finished or lost; never rerun")
            );
            id
        } else {
            let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worker_jobs j JOIN operation_leases l ON l.id=j.operation_id WHERE j.worker_id=?1 AND (j.state IN ('leased','cancelling') OR l.state='abandoned'))",[&worker],|r|r.get(0))?;
            ensure!(!busy, Fault::Busy);
            let id:Option<String>=tx.query_row("SELECT id FROM worker_jobs WHERE worker_id=?1 AND generation=?2 AND state='queued' ORDER BY id LIMIT 1",params![worker,generation],|r|r.get(0)).optional()?;
            let Some(id) = id else {
                tx.commit()?;
                return Ok(json!({"job":null}));
            };
            tx.execute("UPDATE worker_jobs SET state='leased',claim_hash=?2,lease_expires_at=MIN(deadline,?3) WHERE id=?1",params![id,claim,crate::workers::now()+LEASE_SECONDS])?;
            id
        };
        let job:Value=tx.query_row("SELECT j.spec_json,j.transfer_id,t.tree_sha256,j.lease_expires_at,j.state,j.renew_sequence FROM worker_jobs j JOIN worker_transfers t ON t.id=j.transfer_id WHERE j.id=?1",[&id],|row|Ok(json!({"job_id":id,"spec":row.get::<_,String>(0)?,"transfer_id":row.get::<_,String>(1)?,"tree_sha256":row.get::<_,String>(2)?,"lease_expires_at":row.get::<_,i64>(3)?,"cancel":row.get::<_,String>(4)?=="cancelling","renew_sequence":row.get::<_,u64>(5)?})))?;
        if r.protocol == PROTOCOL {
            let spec: Spec = serde_json::from_str(job["spec"].as_str().unwrap())?;
            ensure!(
                spec.container.is_none(),
                invalid("container job requires protocol 4")
            );
        }
        let mut job = job;
        job["spec"] = serde_json::from_str(job["spec"].as_str().unwrap())?;
        job["log_stream"] = true.into();
        job["runtime_receipt"] = crate::worker_receipts::FORMAT.into();
        tx.commit()?;
        Ok(json!({"job":job,"job_protocol":PROTOCOL,"renew_interval_seconds":3}))
    }
    pub fn worker_job_renew(&self, credential: &str, r: &Renew) -> Result<Value> {
        ensure!(
            compatible(Some(r.protocol)) && r.sequence > 0 && r.sequence <= i64::MAX as u64,
            invalid("invalid renewal protocol or sequence")
        );
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx)?;
        let (worker, generation) = auth(&tx, credential)?;
        let row:Option<(String,u64)>=tx.query_row("SELECT state,renew_sequence FROM worker_jobs WHERE id=?1 AND worker_id=?2 AND generation=?3 AND claim_hash=?4",params![r.job_id,worker,generation,crate::workers::hash(&r.claim_secret)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (state, seq) = row.ok_or(Fault::NotFound)?;
        ensure!(
            !terminal(&state) && state != "queued",
            invalid("job claim is no longer active")
        );
        ensure!(r.sequence >= seq, invalid("stale job renewal"));
        if r.sequence > seq {
            tx.execute(
                "UPDATE workers SET last_seen=?2 WHERE id=?1",
                params![worker, crate::workers::now()],
            )?;
            tx.execute("UPDATE worker_jobs SET renew_sequence=?2,lease_expires_at=MIN(deadline,?3) WHERE id=?1",params![r.job_id,r.sequence,crate::workers::now()+LEASE_SECONDS])?;
        }
        let deadline: i64 = tx.query_row(
            "SELECT lease_expires_at FROM worker_jobs WHERE id=?1",
            [&r.job_id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(
            json!({"cancel":state=="cancelling","lease_expires_at":deadline,"accepted_sequence":r.sequence}),
        )
    }
    pub fn worker_job_finish(&self, credential: &str, r: &Finish) -> Result<Value> {
        ensure!(
            compatible(Some(r.protocol)),
            invalid("unsupported worker job protocol")
        );
        ensure!(
            matches!(
                r.outcome.status,
                RunState::Succeeded
                    | RunState::Failed
                    | RunState::Cancelled
                    | RunState::TimedOut
                    | RunState::InfrastructureError
            ),
            invalid("invalid worker outcome")
        );
        ensure!(
            r.outcome.stdout.len() <= 12288 && r.outcome.stderr.len() <= 12288,
            invalid("worker outcome log limit exceeded")
        );
        ensure!(
            r.outcome.status != RunState::Succeeded || r.outcome.exit == Some(0),
            invalid("success requires exit zero")
        );
        let encoded = serde_json::to_string(&r.outcome)?;
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx)?;
        let (worker, generation) = auth(&tx, credential)?;
        let row:Option<(String,String,Option<String>)>=tx.query_row("SELECT state,operation_id,result_json FROM worker_jobs WHERE id=?1 AND worker_id=?2 AND generation=?3 AND claim_hash=?4",params![r.job_id,worker,generation,crate::workers::hash(&r.claim_secret)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (state, operation, old) = row.ok_or(Fault::NotFound)?;
        if let Some(old) = old {
            ensure!(old == encoded, Fault::IdempotencyConflict);
            return Ok(json!({"job_id":r.job_id,"state":state}));
        }
        ensure!(
            state == "leased" || state == "cancelling",
            invalid("late or lost worker result rejected")
        );
        let (log_cursor, log_truncated): (i64, bool) = tx.query_row(
            "SELECT log_cursor,log_truncated FROM worker_jobs WHERE id=?1",
            [&r.job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            !log_truncated || r.outcome.truncated,
            invalid("worker must report truncated log receipt")
        );
        if let Some(cursor) = r.outcome.log_cursor {
            ensure!(
                cursor == log_cursor,
                invalid("worker log acknowledgement mismatch")
            );
        } else {
            ensure!(
                log_cursor == 0 || r.outcome.status == RunState::InfrastructureError,
                invalid("streamed worker logs require an acknowledged final cursor")
            );
        }
        crate::worker_logs::flush(&tx, self, &r.job_id)?;
        if matches!(r.outcome.status, RunState::Succeeded | RunState::Failed) {
            let spec: String = tx.query_row(
                "SELECT spec_json FROM worker_jobs WHERE id=?1",
                [&r.job_id],
                |row| row.get(0),
            )?;
            let spec: Spec = serde_json::from_str(&spec)?;
            let uploaded: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM worker_outputs WHERE job_id=?1)",
                [&r.job_id],
                |row| row.get(0),
            )?;
            ensure!(
                spec.returns.is_empty() || uploaded,
                invalid("worker outputs must be verified before completion")
            );
        }
        let final_state = if state == "cancelling" {
            "cancelled"
        } else {
            r.outcome.status.as_str()
        };
        if let Some(receipt) = &r.outcome.runtime_receipt {
            crate::worker_receipts::accept(&tx, &r.job_id, receipt)?;
        }
        tx.execute(
            "UPDATE worker_jobs SET state=?2,result_json=?3 WHERE id=?1",
            params![r.job_id, final_state, encoded],
        )?;
        tx.execute("UPDATE operation_leases SET state='released',finished_at=?2 WHERE id=?1 AND state='active'",params![operation,crate::db::now()])?;
        tx.commit()?;
        Ok(json!({"job_id":r.job_id,"state":final_state}))
    }
    pub fn worker_jobs_reap(&self) -> Result<()> {
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx)?;
        tx.commit()?;
        Ok(())
    }
    pub fn worker_jobs(&self, run: &str) -> Result<Value> {
        self.run(run)?;
        self.worker_jobs_reap()?;
        let c = self.connection()?;
        let rows=c.prepare("SELECT id,stage_id,worker_id,generation,state,lease_expires_at,reason,(SELECT tree_sha256 FROM worker_outputs o WHERE o.job_id=worker_jobs.id),log_cursor,log_bytes,log_truncated FROM worker_jobs WHERE run_id=?1 ORDER BY stage_id LIMIT 32")?.query_map([run],|r|Ok(json!({"job_id":r.get::<_,String>(0)?,"stage_id":r.get::<_,String>(1)?,"worker_id":r.get::<_,String>(2)?,"generation":r.get::<_,u64>(3)?,"state":r.get::<_,String>(4)?,"lease_expires_at":r.get::<_,Option<i64>>(5)?,"reason":r.get::<_,Option<String>>(6)?,"outputs_tree_sha256":r.get::<_,Option<String>>(7)?,"log_cursor":r.get::<_,i64>(8)?,"log_bytes":r.get::<_,u64>(9)?,"log_truncated":r.get::<_,bool>(10)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"run_id":run,"jobs":rows,"job_protocol":PROTOCOL}))
    }
}

pub(crate) fn credential(headers: &axum::http::HeaderMap) -> Result<&str, axum::http::StatusCode> {
    use axum::http::{StatusCode, header};
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)
}
pub async fn claim_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Claim>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    e.db.worker_job_claim(credential(&headers)?, &r)
        .map(axum::Json)
        .map_err(crate::workers::gateway_error)
}
pub async fn renew_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Renew>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    e.db.worker_job_renew(credential(&headers)?, &r)
        .map(axum::Json)
        .map_err(crate::workers::gateway_error)
}
pub async fn finish_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Finish>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    e.db.worker_job_finish(credential(&headers)?, &r)
        .map(axum::Json)
        .map_err(crate::workers::gateway_error)
}
pub async fn reaper(engine: std::sync::Arc<crate::engine::Engine>) {
    while !engine.is_stopping() {
        if let Err(error) = engine.db.worker_jobs_reap() {
            eprintln!("worker job reconciliation failed: {error}")
        };
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Db, String, String) {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::new(&temp.path().join("db")).unwrap();
        let p = db.register("p", temp.path()).unwrap();
        for run in ["source", "target", "next"] {
            db.insert_run(
                run,
                &crate::model::StartBuild {
                    project_id: p.id.clone(),
                    command: spec().command,
                },
                temp.path(),
            )
            .unwrap();
        }
        let e = db
            .worker_enrollment(&crate::workers::CreateEnrollment {
                project_id: p.id.clone(),
                name: "worker".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                ttl_seconds: 60,
            })
            .unwrap();
        let credential = crate::workers::secret().unwrap();
        let w = db
            .worker_enroll(&crate::workers::Enroll {
                protocol: 1,
                enrollment_token: e["enrollment_token"].as_str().unwrap().into(),
                credential: credential.clone(),
                os: "linux".into(),
                arch: "x86_64".into(),
            })
            .unwrap();
        db.connection().unwrap().execute("INSERT INTO worker_transfers VALUES ('transfer','source',?1,1,'fixture',?2,1,?3,0)",params![w["worker_id"].as_str().unwrap(),"a".repeat(64),crate::workers::now()+600]).unwrap();
        let id = db
            .worker_job_enqueue("target", "native", "transfer", &spec())
            .unwrap()["job_id"]
            .as_str()
            .unwrap()
            .to_owned();
        (temp, db, credential, id)
    }
    fn spec() -> Spec {
        Spec {
            container: None,
            command: CommandSpec {
                program: "tool".into(),
                args: vec![],
                timeout_seconds: 10,
            },
            cwd: ".".into(),
            env: BTreeMap::new(),
            returns: BTreeMap::new(),
        }
    }
    fn outcome() -> Outcome {
        Outcome {
            runtime_receipt: None,
            log_cursor: None,
            status: RunState::Succeeded,
            exit: Some(0),
            stdout: "ok".into(),
            stderr: String::new(),
            truncated: false,
        }
    }
    #[test]
    fn runtime_receipt_is_claim_bound_atomic_and_idempotent() {
        let (_temp, db, credential, id) = setup();
        let claim = crate::workers::secret().unwrap();
        db.worker_job_claim(
            &credential,
            &Claim {
                protocol: PROTOCOL,
                claim_secret: claim.clone(),
            },
        )
        .unwrap();
        let mut receipt = crate::worker_receipts::Receipt {
            format: crate::worker_receipts::FORMAT.into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            user: Some("1000:1000".into()),
            spec_sha256: crate::worker_receipts::digest(&spec()).unwrap(),
            source_tree_sha256: "a".repeat(64),
            environment_sha256: "b".repeat(64),
            launcher_sha256: None,
            container: None,
        };
        let evidence = crate::stage_keys::Evidence {
            format: "preflight-stage-evidence-v2".into(),
            sha256: "c".repeat(64),
            components: BTreeMap::from([("source_tree".into(), "d".repeat(64))]),
            reusable: false,
            reason: "pending".into(),
        };
        db.save_stage_key("target", "native", &evidence).unwrap();
        let mut finish = Finish {
            protocol: PROTOCOL,
            job_id: id.clone(),
            claim_secret: claim.clone(),
            outcome: outcome(),
        };
        receipt.arch = "aarch64".into();
        finish.outcome.runtime_receipt = Some(receipt.clone());
        assert!(db.worker_job_finish(&credential, &finish).is_err());
        assert_eq!(
            db.worker_jobs("target").unwrap()["jobs"][0]["state"],
            "leased"
        );
        assert_eq!(
            db.stage_key("target", "native").unwrap().unwrap().sha256,
            evidence.sha256
        );
        receipt.arch = "x86_64".into();
        finish.outcome.runtime_receipt = Some(receipt);
        finish.claim_secret = crate::workers::secret().unwrap();
        assert!(db.worker_job_finish(&credential, &finish).is_err());
        finish.claim_secret = claim;
        db.worker_job_finish(&credential, &finish).unwrap();
        let accepted = db.stage_key("target", "native").unwrap().unwrap();
        assert!(accepted.components.contains_key("remote_runtime"));
        assert!(!accepted.reusable);
        db.worker_job_finish(&credential, &finish).unwrap();
        assert_eq!(
            db.stage_key("target", "native").unwrap().unwrap().sha256,
            accepted.sha256
        );
        finish
            .outcome
            .runtime_receipt
            .as_mut()
            .unwrap()
            .environment_sha256 = "e".repeat(64);
        assert!(db.worker_job_finish(&credential, &finish).is_err());
        assert_eq!(
            db.stage_key("target", "native").unwrap().unwrap().sha256,
            accepted.sha256
        );
    }
    #[test]
    fn journal_recovery_proof_requires_owner_claim_and_operator_resolution() {
        let (_temp, db, credential, id) = setup();
        let claim = crate::workers::secret().unwrap();
        db.worker_job_claim(
            &credential,
            &Claim {
                protocol: PROTOCOL,
                claim_secret: claim.clone(),
            },
        )
        .unwrap();
        let project = db.run("target").unwrap().project_id;
        let query = crate::worker_recovery::Query {
            project_id: project.clone(),
            job_id: id.clone(),
        };
        let mut proof = crate::worker_recovery::Proof {
            protocol: PROTOCOL,
            job_id: id.clone(),
            claim_secret: claim.clone(),
            process_stopped: true,
        };
        assert!(
            !db.worker_recovery_status(&query).unwrap()["recovery_allowed"]
                .as_bool()
                .unwrap()
        );
        assert!(db.worker_recovery_proof(&credential, &proof).is_err());
        db.connection()
            .unwrap()
            .execute(
                "UPDATE worker_jobs SET lease_expires_at=1 WHERE id=?1",
                [&id],
            )
            .unwrap();
        db.worker_jobs_reap().unwrap();
        assert!(db.worker_recovery_proof(&credential, &proof).is_err());
        db.transition(
            "target",
            RunState::InfrastructureError,
            None,
            None,
            Some("lost worker"),
        )
        .unwrap();
        db.transition("source", RunState::Succeeded, Some(0), None, None)
            .unwrap();
        let lease = db.worker_recovery_status(&query).unwrap()["lease_id"]
            .as_str()
            .unwrap()
            .to_owned();
        db.resolve_operation(&crate::operation_resolution::Resolve {
            project_id: project,
            lease_id: lease,
            request_id: "journal-recovery".into(),
            actor: "operator".into(),
            reason: "verified old process exited".into(),
            process_stopped: true,
        })
        .unwrap();
        proof.process_stopped = false;
        assert!(db.worker_recovery_proof(&credential, &proof).is_err());
        proof.process_stopped = true;
        proof.claim_secret = crate::workers::secret().unwrap();
        assert!(db.worker_recovery_proof(&credential, &proof).is_err());
        proof.claim_secret = claim;
        assert!(
            db.worker_recovery_proof(&crate::workers::secret().unwrap(), &proof)
                .is_err()
        );
        let permitted = db.worker_recovery_proof(&credential, &proof).unwrap();
        assert_eq!(permitted["recovery_allowed"], true);
        assert_eq!(
            permitted["operator_resolution"]["request_id"],
            "journal-recovery"
        );
        assert!(!permitted.to_string().contains(&proof.claim_secret));
        assert!(!permitted.to_string().contains(&credential));
        assert_eq!(
            permitted,
            db.worker_recovery_proof(&credential, &proof).unwrap()
        );
        assert_eq!(
            db.worker_jobs("target").unwrap()["jobs"][0]["state"],
            "lost"
        );
        db.connection()
            .unwrap()
            .execute(
                "UPDATE workers SET generation=generation+1 WHERE id=?1",
                [permitted["worker_id"].as_str().unwrap()],
            )
            .unwrap();
        assert_eq!(
            db.worker_recovery_status(&query).unwrap()["recovery_allowed"],
            false
        );
        assert!(db.worker_recovery_proof(&credential, &proof).is_err());
    }
    #[test]
    fn readiness_tracks_abandoned_claims_until_operator_resolution() {
        let (_temp, db, credential, id) = setup();
        let heartbeat = |sequence| {
            db.worker_heartbeat(
                &credential,
                &crate::workers::Heartbeat {
                    protocol: crate::workers::PROTOCOL,
                    sequence,
                    job_protocol: Some(PROTOCOL),
                },
            )
            .unwrap()
        };
        assert_eq!(heartbeat(1)["execution_ready"], true);
        db.worker_job_claim(
            &credential,
            &Claim {
                protocol: PROTOCOL,
                claim_secret: crate::workers::secret().unwrap(),
            },
        )
        .unwrap();
        db.connection()
            .unwrap()
            .execute(
                "UPDATE worker_jobs SET lease_expires_at=1 WHERE id=?1",
                [&id],
            )
            .unwrap();
        db.worker_jobs_reap().unwrap();
        let reply = heartbeat(2);
        assert_eq!(reply["execution_ready"], false);
        assert_eq!(reply["requires_recovery"], true);
        assert_eq!(reply["blocked_operations"], 1);
        let project_id = db.run("target").unwrap().project_id;
        let listed = db
            .workers(&crate::workers::List {
                project_id: project_id.clone(),
                after: None,
            })
            .unwrap();
        assert_eq!(listed["workers"][0]["execution_ready"], false);
        assert_eq!(listed["workers"][0]["requires_recovery"], true);
        db.transition(
            "target",
            RunState::InfrastructureError,
            None,
            None,
            Some("worker lost"),
        )
        .unwrap();
        db.transition("source", RunState::Succeeded, Some(0), None, None)
            .unwrap();
        let abandoned = db
            .abandoned_operations(&crate::operation_resolution::Query {
                project_id: project_id.clone(),
                after: None,
            })
            .unwrap();
        let mut resolve = crate::operation_resolution::Resolve {
            project_id,
            lease_id: abandoned["operations"][0]["lease_id"]
                .as_str()
                .unwrap()
                .into(),
            request_id: "readiness-resolve".into(),
            actor: "test".into(),
            reason: "no native process was started".into(),
            process_stopped: false,
        };
        assert!(db.resolve_operation(&resolve).is_err());
        assert_eq!(heartbeat(3)["execution_ready"], false);
        resolve.process_stopped = true;
        db.resolve_operation(&resolve).unwrap();
        assert_eq!(heartbeat(4)["execution_ready"], true);
        assert_eq!(heartbeat(5)["blocked_operations"], 0);
        assert_eq!(
            db.worker_jobs("target").unwrap()["jobs"][0]["state"],
            "lost"
        );
    }
    #[test]
    fn claim_and_finish_replay_do_not_execute_or_replace_results() {
        let (_temp, db, credential, id) = setup();
        let claim = Claim {
            protocol: PROTOCOL,
            claim_secret: crate::workers::secret().unwrap(),
        };
        assert_eq!(
            db.worker_job_enqueue("target", "native", "transfer", &spec())
                .unwrap()["job_id"],
            id
        );
        let claimed = db.worker_job_claim(&credential, &claim).unwrap();
        assert_eq!(claimed["job"]["job_id"], id);
        assert_eq!(claimed, db.worker_job_claim(&credential, &claim).unwrap());
        assert!(
            db.worker_job_claim(
                &credential,
                &Claim {
                    protocol: PROTOCOL,
                    claim_secret: crate::workers::secret().unwrap()
                }
            )
            .is_err()
        );
        let mut result = Finish {
            protocol: PROTOCOL,
            job_id: id.clone(),
            claim_secret: claim.claim_secret.clone(),
            outcome: outcome(),
        };
        let done = db.worker_job_finish(&credential, &result).unwrap();
        assert_eq!(done, db.worker_job_finish(&credential, &result).unwrap());
        result.outcome.stdout = "changed".into();
        assert!(db.worker_job_finish(&credential, &result).is_err());
        assert!(db.worker_job_claim(&credential, &claim).is_err());
        assert_eq!(
            db.operations("target").unwrap()["operations"][0]["state"],
            "released"
        );
    }
    #[test]
    fn expiry_fences_late_results_and_keeps_uncertain_operations_protected() {
        let (_temp, db, credential, id) = setup();
        let claim = Claim {
            protocol: PROTOCOL,
            claim_secret: crate::workers::secret().unwrap(),
        };
        db.worker_job_claim(&credential, &claim).unwrap();
        let r = Renew {
            protocol: PROTOCOL,
            job_id: id.clone(),
            claim_secret: claim.claim_secret.clone(),
            sequence: 1,
        };
        db.worker_job_renew(&credential, &r).unwrap();
        db.connection()
            .unwrap()
            .execute(
                "UPDATE worker_jobs SET lease_expires_at=1 WHERE id=?1",
                [&id],
            )
            .unwrap();
        db.worker_jobs_reap().unwrap();
        assert!(db.worker_job_renew(&credential, &r).is_err());
        assert!(
            db.worker_job_finish(
                &credential,
                &Finish {
                    protocol: PROTOCOL,
                    job_id: id,
                    claim_secret: claim.claim_secret,
                    outcome: outcome()
                }
            )
            .is_err()
        );
        assert_eq!(
            db.operations("target").unwrap()["operations"][0]["state"],
            "abandoned"
        );
        db.worker_job_enqueue("next", "native", "transfer", &spec())
            .unwrap();
        assert!(
            db.worker_job_claim(
                &credential,
                &Claim {
                    protocol: PROTOCOL,
                    claim_secret: crate::workers::secret().unwrap()
                }
            )
            .is_err()
        );
        db.reconcile().unwrap();
        assert_eq!(db.worker_jobs("next").unwrap()["jobs"][0]["state"], "lost");
        assert_eq!(
            db.operations("next").unwrap()["operations"][0]["state"],
            "released"
        );
    }
    #[test]
    fn cancellation_wins_over_late_success_and_renew_replay_does_not_extend_lease() {
        let (_temp, db, credential, id) = setup();
        let claim = Claim {
            protocol: PROTOCOL,
            claim_secret: crate::workers::secret().unwrap(),
        };
        db.worker_job_claim(&credential, &claim).unwrap();
        let r = Renew {
            protocol: PROTOCOL,
            job_id: id.clone(),
            claim_secret: claim.claim_secret.clone(),
            sequence: 1,
        };
        db.worker_job_renew(&credential, &r).unwrap();
        let deadline = crate::workers::now() + 5;
        db.connection()
            .unwrap()
            .execute(
                "UPDATE worker_jobs SET lease_expires_at=?1 WHERE id=?2",
                params![deadline, id],
            )
            .unwrap();
        assert_eq!(
            db.worker_job_renew(&credential, &r).unwrap()["lease_expires_at"],
            deadline
        );
        db.request_cancel("target", "user", "cancel test").unwrap();
        assert_eq!(
            db.worker_job_renew(&credential, &r).unwrap()["cancel"],
            true
        );
        assert_eq!(
            db.worker_job_finish(
                &credential,
                &Finish {
                    protocol: PROTOCOL,
                    job_id: id,
                    claim_secret: claim.claim_secret,
                    outcome: outcome()
                }
            )
            .unwrap()["state"],
            "cancelled"
        );
        let next = db
            .worker_job_enqueue("next", "native", "transfer", &spec())
            .unwrap();
        db.request_cancel("next", "user", "queued cancel").unwrap();
        assert!(
            db.worker_job_claim(
                &credential,
                &Claim {
                    protocol: PROTOCOL,
                    claim_secret: crate::workers::secret().unwrap()
                }
            )
            .unwrap()["job"]
                .is_null()
        );
        assert_eq!(
            db.worker_jobs("next").unwrap()["jobs"][0]["job_id"],
            next["job_id"]
        );
        assert_eq!(
            db.worker_jobs("next").unwrap()["jobs"][0]["state"],
            "cancelled"
        );
    }
}
