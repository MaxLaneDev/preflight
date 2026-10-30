//! Explicit owners of retained run output bytes, independent of module publication.
use crate::{
    db::Db,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub project_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub artifact_name: String,
    pub owner: String,
    pub pinned: bool,
    pub expected_revision: u64,
    pub actor: String,
    pub reason: String,
    pub request_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub artifact_name: String,
    #[serde(default)]
    pub offset: u32,
}
fn artifact(
    c: &rusqlite::Connection,
    project: &str,
    run: &str,
    stage: &str,
    name: &str,
) -> Result<Value> {
    let row:Option<(String,u64,String,Option<String>)>=c.query_row("SELECT a.sha256,a.size,a.path,json_extract(s.definition_json,'$.cache.pipeline_id') FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN stage_runs s ON s.run_id=a.run_id AND s.name=a.stage_id WHERE r.project_id=?1 AND a.run_id=?2 AND a.stage_id=?3 AND a.name=?4",params![project,run,stage,name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (sha, size, path, pipeline) = row.ok_or(Fault::NotFound)?;
    let expired: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1 AND stage_id=?2)",
        params![run, stage],
        |r| r.get(0),
    )?;
    let available = !expired && std::path::Path::new(&path).try_exists()?;
    Ok(
        json!({"project_id":project,"run_id":run,"stage_id":stage,"artifact_name":name,"sha256":sha,"logical_bytes":size,"path":path,"pipeline_id":pipeline,"availability":if expired{"artifact_expired"}else if available{"available"}else{"artifact_missing"}}),
    )
}
impl Db {
    pub fn artifact_pin_set(&self, r: &Pin) -> Result<Value> {
        crate::pipeline::identifier(&r.stage_id)?;
        crate::pipeline::identifier(&r.artifact_name)?;
        crate::control::validate_name(&r.owner)?;
        crate::control::validate_name(&r.request_id)?;
        ensure!(
            !r.actor.trim().is_empty()
                && r.actor.len() <= 128
                && !r.reason.trim().is_empty()
                && r.reason.len() <= 1024,
            invalid("bounded actor and reason required")
        );
        ensure!(
            r.expected_revision < i64::MAX as u64,
            invalid("revision out of range")
        );
        let request = serde_json::to_string(r)?;
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replay:Option<(String,String)>=tx.query_row("SELECT request_json,result_json FROM artifact_pin_changes WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((old, result)) = replay {
            ensure!(old == request, Fault::IdempotencyConflict);
            return Ok(serde_json::from_str(&result)?);
        }
        let metadata = artifact(&tx, &r.project_id, &r.run_id, &r.stage_id, &r.artifact_name)?;
        ensure!(
            !r.pinned || metadata["availability"] == "available",
            invalid("cannot pin unavailable bytes; rerun the pipeline")
        );
        let revision:Option<u64>=tx.query_row("SELECT revision FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND artifact_name=?3 AND owner=?4",params![r.run_id,r.stage_id,r.artifact_name,r.owner],|row|row.get(0)).optional()?;
        ensure!(
            revision.unwrap_or(0) == r.expected_revision,
            Fault::Conflict
        );
        ensure!(
            r.expected_revision > 0 || r.pinned,
            invalid("cannot release an unknown retention owner")
        );
        let next = r.expected_revision + 1;
        tx.execute("INSERT INTO artifact_pins VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(run_id,stage_id,artifact_name,owner) DO UPDATE SET pinned=excluded.pinned,revision=excluded.revision,actor=excluded.actor,reason=excluded.reason",params![r.run_id,r.stage_id,r.artifact_name,r.owner,r.pinned,next,r.actor,r.reason])?;
        let result = json!({"project_id":r.project_id,"run_id":r.run_id,"stage_id":r.stage_id,"artifact_name":r.artifact_name,"owner":r.owner,"pipeline_id":metadata["pipeline_id"],"pinned":r.pinned,"revision":next,"actor":r.actor,"reason":r.reason,"request_id":r.request_id,"physical_bytes_deleted":0});
        tx.execute("INSERT INTO artifact_pin_changes(project_id,request_id,request_json,result_json) VALUES (?1,?2,?3,?4)",params![r.project_id,r.request_id,request,result.to_string()])?;
        tx.commit()?;
        Ok(result)
    }
    pub fn artifact_retention(&self, r: &Query) -> Result<Value> {
        let c = self.connection()?;
        let mut result = artifact(&c, &r.project_id, &r.run_id, &r.stage_id, &r.artifact_name)?;
        let total:u64=c.query_row("SELECT COUNT(*) FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND artifact_name=?3",params![r.run_id,r.stage_id,r.artifact_name],|row|row.get(0))?;
        let active:u64=c.query_row("SELECT COUNT(*) FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND artifact_name=?3 AND pinned=1",params![r.run_id,r.stage_id,r.artifact_name],|row|row.get(0))?;
        let rows=c.prepare("SELECT owner,pinned,revision,actor,reason FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND artifact_name=?3 ORDER BY owner LIMIT 100 OFFSET ?4")?.query_map(params![r.run_id,r.stage_id,r.artifact_name,r.offset],|row|Ok(json!({"owner":row.get::<_,String>(0)?,"pinned":row.get::<_,bool>(1)?,"revision":row.get::<_,u64>(2)?,"actor":row.get::<_,String>(3)?,"reason":row.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        result["owners"] = json!(rows);
        result["total"] = json!(total);
        result["active_owners"] = json!(active);
        let stage_owners: u64 = c.query_row(
            "SELECT COUNT(*) FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND pinned=1",
            params![r.run_id, r.stage_id],
            |row| row.get(0),
        )?;
        result["stage_active_owners"] = json!(stage_owners);
        let next = u64::from(r.offset) + 100;
        result["next_cursor"] = json!((next < total).then_some(next));
        result["policy"] = json!(
            "any active owner protects the complete stage output directory; releasing a pin never deletes files; expiry preview rechecks all protection"
        );
        Ok(result)
    }
}
