//! Existing operator resolutions authorize retirement of one lost local journal.
use crate::{
    db::Db,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub job_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub protocol: u32,
    pub job_id: String,
    pub claim_secret: String,
    pub process_stopped: bool,
}

type RecoveryRow = (
    String,
    u64,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    u64,
);
fn status(c: &rusqlite::Connection, q: &Query) -> Result<Value> {
    uuid::Uuid::parse_str(&q.job_id).map_err(|_| invalid("invalid recovery job ID"))?;
    let row: Option<RecoveryRow> = c.query_row(
        "SELECT j.worker_id,j.generation,j.state,j.operation_id,l.state,w.name,o.request_json,w.state,w.generation FROM worker_jobs j JOIN runs r ON r.id=j.run_id JOIN workers w ON w.id=j.worker_id JOIN operation_leases l ON l.id=j.operation_id LEFT JOIN operation_resolutions o ON o.lease_id=l.id AND o.project_id=r.project_id WHERE j.id=?1 AND r.project_id=?2",
        params![q.job_id,q.project_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))
    ).optional()?;
    let (
        worker,
        generation,
        job_state,
        lease,
        lease_state,
        name,
        resolution,
        worker_state,
        current_generation,
    ) = row.ok_or(Fault::NotFound)?;
    let resolution: Option<Value> = resolution.map(|r| serde_json::from_str(&r)).transpose()?;
    let identity_current = worker_state == "registered" && current_generation == generation;
    let allowed = identity_current
        && job_state == "lost"
        && lease_state == "released"
        && resolution
            .as_ref()
            .is_some_and(|r| r["process_stopped"] == true);
    Ok(
        json!({"project_id":q.project_id,"job_id":q.job_id,"worker_id":worker,"worker_name":name,"generation":generation,"identity_current":identity_current,"job_state":job_state,"lease_id":lease,"lease_state":lease_state,"recovery_allowed":allowed,"operator_resolution":resolution.map(|r|json!({"request_id":r["request_id"],"actor":r["actor"],"reason":r["reason"]})),"policy":"retire only this lost journal after operator-attested resolution; retain evidence and never replay the command"}),
    )
}
impl Db {
    pub fn worker_recovery_status(&self, q: &Query) -> Result<Value> {
        self.project(&q.project_id)?;
        let c = self.connection()?;
        status(&c, q)
    }
    pub fn worker_recovery_proof(&self, credential: &str, r: &Proof) -> Result<Value> {
        ensure!(
            crate::worker_jobs::compatible(Some(r.protocol)),
            invalid("unsupported worker job protocol")
        );
        ensure!(
            r.process_stopped,
            invalid("operator must attest old processes stopped before journal recovery")
        );
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let (worker, generation) = crate::worker_jobs::auth(&tx, credential)?;
        let project:Option<String>=tx.query_row("SELECT r.project_id FROM worker_jobs j JOIN runs r ON r.id=j.run_id WHERE j.id=?1 AND j.worker_id=?2 AND j.generation=?3 AND j.claim_hash=?4",params![r.job_id,worker,generation,crate::workers::hash(&r.claim_secret)?],|r|r.get(0)).optional()?;
        let proof = status(
            &tx,
            &Query {
                project_id: project.ok_or(Fault::NotFound)?,
                job_id: r.job_id.clone(),
            },
        )?;
        ensure!(
            proof["recovery_allowed"] == true,
            invalid(
                "lost job protection must first be released with operator-attested operation_resolve"
            )
        );
        tx.commit()?;
        Ok(proof)
    }
}
pub async fn proof_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Proof>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    e.db.worker_recovery_proof(crate::worker_jobs::credential(&headers)?, &r)
        .map(axum::Json)
        .map_err(crate::workers::gateway_error)
}
