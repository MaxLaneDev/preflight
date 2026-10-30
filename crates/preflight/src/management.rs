//! Explicit bounded maintenance plans; recoverable deletion never removes evidence.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub action: String,
    pub target_id: Option<String>,
    pub pipeline_key: Option<String>,
    pub before: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Execute {
    pub project_id: String,
    pub plan_id: String,
    pub request_id: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trash {
    pub project_id: String,
    pub after: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Restore {
    pub project_id: String,
    pub kind: String,
    pub target_id: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
fn target(e: &Engine, project: &str, kind: &str, id: &str) -> Result<()> {
    ensure!(id.len() <= 512, invalid("target too long"));
    if kind == "run" {
        let r = e.db.run(id)?;
        ensure!(r.project_id == project, Fault::NotFound);
        ensure!(
            r.status.terminal(),
            invalid("stop the run and wait for completion before moving it to Trash")
        );
    } else {
        ensure!(kind == "pipeline", invalid("kind must be pipeline or run"));
        let p = e.db.project(project)?;
        if let Some(name) = id.strip_prefix("saved:") {
            crate::control::validate_name(name)?;
            ensure!(
                crate::control::latest(&p.root, "pipelines", name)? > 0,
                Fault::NotFound
            );
        } else {
            let exists:bool=e.db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM run_pipeline_identity i JOIN runs r ON r.id=i.run_id WHERE r.project_id=?1 AND i.pipeline_key=?2)",params![project,id],|r|r.get(0))?;
            ensure!(exists, Fault::NotFound);
        }
        let busy:bool=e.db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM runs r JOIN run_pipeline_identity i ON i.run_id=r.id WHERE r.project_id=?1 AND i.pipeline_key=?2 AND r.finished_at IS NULL)",params![project,id],|r|r.get(0))?;
        ensure!(
            !busy,
            invalid("stop active runs before moving the pipeline to Trash")
        );
    }
    Ok(())
}
pub async fn preview(e: Arc<Engine>, r: Preview) -> Result<Value> {
    e.db.project(&r.project_id)?;
    if let Some(key) = &r.pipeline_key {
        ensure!(
            !key.is_empty() && key.len() <= 512,
            invalid("invalid pipeline key")
        );
    }
    ensure!(
        r.before.as_ref().is_none_or(|s| s.len() <= 512),
        invalid("invalid cleanup cursor")
    );
    if r.action == "artifacts" {
        ensure!(
            r.target_id.is_none(),
            invalid("artifact scope uses pipeline_key; run target_id is not supported")
        );
        return crate::artifact_expiry::preview_pipeline(
            e,
            crate::artifact_expiry::Preview {
                project_id: r.project_id,
                pipeline_id: None,
                before: r.before,
                expire_artifacts: true,
                finished_before: crate::db::now(),
                limit: 100,
            },
            r.pipeline_key,
        )
        .await;
    }
    ensure!(
        r.before.is_none(),
        invalid("cursor only applies to artifacts")
    );
    let entries = match r.action.as_str() {
        "delete_run" | "delete_pipeline" => {
            ensure!(
                r.pipeline_key.is_none(),
                invalid("deletion uses target_id only")
            );
            let kind = if r.action == "delete_run" {
                "run"
            } else {
                "pipeline"
            };
            let id = r
                .target_id
                .as_deref()
                .ok_or_else(|| invalid("target_id required"))?;
            target(&e, &r.project_id, kind, id)?;
            vec![
                json!({"kind":kind,"target_id":id,"effect":"move to Trash; definitions, history and files retained"}),
            ]
        }
        "clear_queue" => {
            ensure!(
                r.target_id.is_none(),
                invalid("clear_queue is project scoped; target_id is not accepted")
            );
            ensure!(
                r.pipeline_key.is_none(),
                invalid("clear_queue is project scoped")
            );
            let c = e.db.connection()?;
            let mut items=c.prepare("SELECT q.id,i.ordinal,i.name FROM change_queue_items i JOIN change_queues q ON q.id=i.queue_id WHERE q.project_id=?1 AND q.state IN ('active','cancelling') AND i.state='pending' AND i.run_id IS NULL ORDER BY q.id,i.ordinal LIMIT 101")?.query_map([&r.project_id],|v|Ok(json!({"kind":"queue_item","target_id":v.get::<_,String>(0)?,"ordinal":v.get::<_,u64>(1)?,"name":v.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let remaining = 101usize.saturating_sub(items.len());
            let mut s=c.prepare("SELECT id,revision,request_json FROM schedules WHERE project_id=?1 AND state IN ('pending','paused') AND NOT EXISTS(SELECT 1 FROM schedule_hidden h WHERE h.schedule_id=schedules.id) ORDER BY at_ms,id LIMIT ?2")?;
            for v in s.query_map(params![r.project_id, remaining], |v| {
                Ok((
                    v.get::<_, String>(0)?,
                    v.get::<_, u64>(1)?,
                    v.get::<_, String>(2)?,
                ))
            })? {
                let (id, rev, body) = v?;
                let body: Value = serde_json::from_str(&body)?;
                items.push(
                    json!({"kind":"schedule","target_id":id,"revision":rev,"name":body["name"]}),
                );
            }
            items
        }
        _ => {
            return Err(invalid(
                "action must be clear_queue, artifacts, delete_run or delete_pipeline",
            ));
        }
    };
    let more = entries.len() > 100;
    let entries: Vec<_> = entries.into_iter().take(100).collect();
    let id = uuid::Uuid::now_v7().to_string();
    let plan = json!({"plan_id":id,"project_id":r.project_id,"action":r.action,"entries":entries,"has_more":more,"execute_tool":"management_execute","retained":["running work","evidence and revisions","artifact files"],"effect":if r.action=="clear_queue"{"cancel only listed pending tasks and future schedules; progressed tasks are skipped"}else{"recoverable deletion; restore from Trash"}});
    e.db.connection()?.execute(
        "INSERT INTO management_plans(id,project_id,preview_json,created_at) VALUES (?1,?2,?3,?4)",
        params![id, r.project_id, plan.to_string(), crate::db::now()],
    )?;
    Ok(plan)
}
pub fn execute(e: &Engine, r: Execute) -> Result<Value> {
    ensure!(
        !r.request_id.is_empty()
            && r.request_id.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 512,
        invalid("request_id and reason required")
    );
    let request = serde_json::to_string(&r)?;
    let _gate = e
        .queue_gate
        .lock()
        .map_err(|_| anyhow::anyhow!("queue lock poisoned"))?;
    let row:Option<(String,Option<String>,Option<String>)>=e.db.connection()?.query_row("SELECT preview_json,request_json,result_json FROM management_plans WHERE id=?1 AND project_id=?2",params![r.plan_id,r.project_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?;
    let (body, previous, result) = row.ok_or(Fault::NotFound)?;
    if let Some(previous) = previous {
        ensure!(previous == request, Fault::Conflict);
        return Ok(serde_json::from_str(&result.ok_or(Fault::Conflict)?)?);
    }
    let p: Value = serde_json::from_str(&body)?;
    for item in p["entries"].as_array().ok_or(Fault::Conflict)? {
        if matches!(item["kind"].as_str(), Some("run" | "pipeline")) {
            target(
                e,
                &r.project_id,
                item["kind"].as_str().unwrap(),
                item["target_id"].as_str().unwrap(),
            )?;
        }
    }
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut outcomes = Vec::new();
    for item in p["entries"].as_array().ok_or(Fault::Conflict)? {
        let kind = item["kind"].as_str().unwrap_or_default();
        let id = item["target_id"].as_str().unwrap_or_default();
        if kind == "queue_item" {
            // Submission and item linking can straddle a crash; never cancel a run during pending-only cleanup.
            let changed=tx.execute("UPDATE change_queue_items SET state='cancelled',finished_at=?3,error=?4 WHERE queue_id=?1 AND ordinal=?2 AND state='pending' AND run_id IS NULL AND NOT EXISTS(SELECT 1 FROM submissions s WHERE s.project_id=?5 AND s.request_key='queue:'||?1||':'||?2)",params![id,item["ordinal"].as_u64(),crate::db::now(),format!("Cleared by {}: {}",r.actor.as_str(),r.reason),r.project_id])?;
            outcomes.push(json!({"kind":kind,"target_id":id,"ordinal":item["ordinal"],"state":if changed==1{"cancelled"}else{"skipped_progressed"}}));
        } else if kind == "schedule" {
            // Use the schedule revision to avoid cancelling a concurrently edited or dispatched occurrence.
            let changed=tx.execute("UPDATE schedules SET state='cancelled',revision=revision+1,reason=?3 WHERE id=?1 AND project_id=?2 AND revision=?4 AND state IN ('pending','paused')",params![id,r.project_id,r.reason,item["revision"].as_u64()])?;
            if changed == 1 {
                tx.execute("INSERT INTO schedule_events(schedule_id,state,actor,reason,timestamp) VALUES (?1,'cancelled',?2,?3,?4)",params![id,r.actor.as_str(),r.reason,crate::db::now()])?;
            }
            outcomes.push(json!({"kind":kind,"target_id":id,"state":if changed==1{"cancelled"}else{"skipped_progressed"}}));
        } else {
            if kind == "pipeline" {
                let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs r JOIN run_pipeline_identity i ON i.run_id=r.id WHERE r.project_id=?1 AND i.pipeline_key=?2 AND r.finished_at IS NULL)",params![r.project_id,id],|v|v.get(0))?;
                ensure!(!busy, Fault::Conflict);
            }
            tx.execute("INSERT INTO management_trash(project_id,kind,target_id,deleted_at) VALUES (?1,?2,?3,?4) ON CONFLICT(project_id,kind,target_id) DO UPDATE SET deleted_at=excluded.deleted_at,restored_at=NULL",params![r.project_id,kind,id,crate::db::now()])?;
            outcomes.push(json!({"kind":kind,"target_id":id,"state":"trashed"}));
        }
    }
    let result = json!({"plan_id":r.plan_id,"project_id":r.project_id,"items":outcomes});
    tx.execute(
        "UPDATE management_plans SET request_json=?2,result_json=?3 WHERE id=?1",
        params![r.plan_id, request, result.to_string()],
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn trash(e: &Engine, r: Trash) -> Result<Value> {
    e.db.project(&r.project_id)?;
    ensure!(
        r.after.as_ref().is_none_or(|s| s.len() <= 520),
        invalid("invalid cursor")
    );
    let c = e.db.connection()?;
    let mut rows=c.prepare("SELECT kind,target_id,deleted_at FROM management_trash WHERE project_id=?1 AND restored_at IS NULL AND kind||':'||target_id>?2 ORDER BY kind,target_id LIMIT 51")?.query_map(params![r.project_id,r.after.unwrap_or_default()],|v|Ok(json!({"kind":v.get::<_,String>(0)?,"target_id":v.get::<_,String>(1)?,"deleted_at":v.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > 50;
    rows.truncate(50);
    let next = more.then(|| {
        format!(
            "{}:{}",
            rows.last().unwrap()["kind"].as_str().unwrap(),
            rows.last().unwrap()["target_id"].as_str().unwrap()
        )
    });
    Ok(json!({"items":rows,"next_cursor":next}))
}
pub fn restore(e: &Engine, r: Restore) -> Result<Value> {
    ensure!(
        matches!(r.kind.as_str(), "pipeline" | "run")
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 512
            && r.target_id.len() <= 512,
        invalid("invalid restore request")
    );
    e.db.project(&r.project_id)?;
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let changed=tx.execute("UPDATE management_trash SET restored_at=?4 WHERE project_id=?1 AND kind=?2 AND target_id=?3 AND restored_at IS NULL",params![r.project_id,r.kind,r.target_id,crate::db::now()])?;
    let result = json!({"project_id":r.project_id,"kind":r.kind,"target_id":r.target_id,"restored":changed==1,"actor":r.actor,"reason":r.reason});
    let id = uuid::Uuid::now_v7().to_string();
    tx.execute("INSERT INTO management_plans(id,project_id,preview_json,created_at,request_json,result_json) VALUES (?1,?2,?3,?4,?3,?3)",params![id,r.project_id,result.to_string(),crate::db::now()])?;
    tx.commit()?;
    Ok(result)
}
