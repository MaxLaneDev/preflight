//! Cross-project observation and explicit cancellation; scheduling remains project-owned.
use crate::{
    engine::Engine,
    model::{Actor, CancelRun, Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default = "active")]
    pub view: String,
    #[serde(default = "all")]
    pub kind: String,
    pub project_id: Option<String>,
    #[serde(default)]
    pub query: String,
    pub after: Option<String>,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default)]
    pub since: i64,
}
fn active() -> String {
    "active".into()
}
fn all() -> String {
    "all".into()
}
fn limit() -> usize {
    25
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    #[serde(default = "crate::db::now")]
    pub since: i64,
}
const ACTIVE: &str = "('queued','preparing','running','collecting')";
const RUNS: &str = "SELECT r.id AS key,r.id AS target_id,'run' AS target_kind,r.project_id,p.name AS project_name,COALESCE(x.name,i.name,'Run') AS name,CASE WHEN x.run_id IS NULL THEN 'pipeline' ELSE 'experiment' END AS kind,r.status AS state,r.created_at AS at_ms,r.id AS run_id,COALESCE(i.pipeline_key,'') AS pipeline_key,x.name AS experiment_name,NULL AS revision,NULL AS ordinal,(SELECT COUNT(*) FROM stage_runs s WHERE s.run_id=r.id) AS total,(SELECT COUNT(*) FROM stage_runs s WHERE s.run_id=r.id AND s.status IN ('succeeded','failed','cancelled','timed_out','infrastructure_error','blocked','skipped')) AS done FROM runs r JOIN projects p ON p.id=r.project_id LEFT JOIN run_pipeline_identity i ON i.run_id=r.id LEFT JOIN experiment_runs x ON x.run_id=r.id WHERE NOT EXISTS(SELECT 1 FROM shared_executions z WHERE z.run_id=r.id)";
const PLANNED: &str = "SELECT 's:'||printf('%020d',s.at_ms)||':'||s.id AS key,s.id AS target_id,'schedule' AS target_kind,s.project_id,p.name AS project_name,json_extract(s.request_json,'$.name') AS name,CASE WHEN json_extract(s.request_json,'$.experiment.name') IS NULL THEN 'pipeline' ELSE 'experiment' END AS kind,s.state,s.at_ms,s.run_id,'saved:'||json_extract(s.request_json,'$.pipeline.name') AS pipeline_key,json_extract(s.request_json,'$.experiment.name') AS experiment_name,s.revision,NULL AS ordinal,0 AS total,0 AS done FROM schedules s JOIN projects p ON p.id=s.project_id WHERE s.state IN ('pending','paused','dispatching') AND NOT EXISTS(SELECT 1 FROM schedule_hidden h WHERE h.schedule_id=s.id) UNION ALL SELECT 'q:'||q.id||':'||printf('%010d',i.ordinal),q.id,'queue_item',q.project_id,p.name,i.name,'pipeline',i.state,0,i.run_id,'',NULL,NULL,i.ordinal,0,0 FROM change_queue_items i JOIN change_queues q ON q.id=i.queue_id JOIN projects p ON p.id=q.project_id WHERE q.state IN ('active','cancelling') AND i.state='pending'";
pub fn list(e: &Engine, q: &Query) -> Result<Value> {
    ensure!(
        matches!(q.view.as_str(), "all" | "active" | "planned" | "recent")
            && matches!(q.kind.as_str(), "all" | "pipeline" | "experiment")
            && (1..=100).contains(&q.limit)
            && q.query.len() <= 256
            && q.after.as_ref().is_none_or(|s| s.len() <= 512)
            && q.since >= 0,
        invalid("invalid execution list query")
    );
    if let Some(id) = &q.project_id {
        e.db.project(id)?;
    }
    let mut base = if q.view == "all" {
        let runs = RUNS.replace("r.id AS key", &format!("CASE WHEN r.status IN {ACTIVE} THEN '0:'||r.id ELSE '2:'||printf('%020d',999999999999999-COALESCE(r.finished_at,r.created_at))||':'||r.id END AS key"));
        let planned = PLANNED.replace("'s:'", "'1:s:'").replace("'q:'", "'1:q:'");
        format!(
            "{runs} AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.kind='run' AND t.target_id=r.id AND t.restored_at IS NULL) UNION ALL {planned} UNION ALL SELECT '2:'||printf('%020d',999999999999999-i.finished_at)||':queue:'||q.id||':'||i.ordinal,q.id,'queue_item',q.project_id,p.name,i.name,'pipeline',i.state,i.finished_at,NULL,'',NULL,NULL,i.ordinal,0,0 FROM change_queue_items i JOIN change_queues q ON q.id=i.queue_id JOIN projects p ON p.id=q.project_id WHERE i.run_id IS NULL AND i.finished_at>=?6"
        )
    } else if q.view == "planned" {
        PLANNED.to_owned()
    } else {
        format!(
            "{RUNS} AND {}",
            if q.view == "active" {
                format!("r.status IN {ACTIVE}")
            } else {
                "r.finished_at>=?6 AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.kind='run' AND t.target_id=r.id AND t.restored_at IS NULL)".into()
            }
        )
    };
    if q.view == "recent" {
        base = base.replace(
            "r.id AS key",
            "'r:'||printf('%020d',r.finished_at)||':'||r.id AS key",
        );
        base.push_str(" UNION ALL SELECT 'r:'||printf('%020d',i.finished_at)||':queue:'||q.id||':'||i.ordinal,q.id,'queue_item',q.project_id,p.name,i.name,'pipeline',i.state,i.finished_at,NULL,'',NULL,NULL,i.ordinal,0,0 FROM change_queue_items i JOIN change_queues q ON q.id=i.queue_id JOIN projects p ON p.id=q.project_id WHERE i.run_id IS NULL AND i.finished_at>=?6");
    }
    let descending = q.view == "recent";
    let comparison = if descending { "<" } else { ">" };
    let direction = if descending { "DESC" } else { "ASC" };
    let sql = format!(
        "SELECT * FROM ({base}) WHERE (?1 IS NULL OR project_id=?1) AND (?2='all' OR kind=?2) AND preflight_contains(project_name||' '||COALESCE(name,'')||' '||target_id||' '||project_id||' '||COALESCE(run_id,'')||' '||COALESCE(pipeline_key,''),?3) AND key{comparison}?4 AND ?6>=0 ORDER BY key {direction} LIMIT ?5"
    );
    let c = e.db.connection()?;
    let mut rows=c.prepare(&sql)?.query_map(params![q.project_id,q.kind,q.query.trim().to_lowercase(),q.after.as_deref().unwrap_or(if descending{"~"}else{""}),q.limit+1,q.since],|r|Ok(json!({"key":r.get::<_,String>(0)?,"target_id":r.get::<_,String>(1)?,"target_kind":r.get::<_,String>(2)?,"project_id":r.get::<_,String>(3)?,"project_name":r.get::<_,String>(4)?,"name":r.get::<_,String>(5)?,"kind":r.get::<_,String>(6)?,"state":r.get::<_,String>(7)?,"at_ms":r.get::<_,i64>(8)?,"run_id":r.get::<_,Option<String>>(9)?,"pipeline_key":r.get::<_,String>(10)?,"experiment_name":r.get::<_,Option<String>>(11)?,"revision":r.get::<_,Option<u64>>(12)?,"ordinal":r.get::<_,Option<usize>>(13)?,"progress":{"total":r.get::<_,u64>(14)?,"done":r.get::<_,u64>(15)?,"basis":"terminal stages; not elapsed time"}})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for item in &mut rows {
        if let Some(fields) = item.as_object_mut() {
            fields.retain(|_, v| !v.is_null() && v.as_str() != Some(""));
            if fields.get("target_kind").and_then(Value::as_str) == Some("run") {
                fields.remove("run_id");
            }
            if let Some(progress) = fields.get_mut("progress").and_then(Value::as_object_mut) {
                progress.remove("basis");
            }
        }
    }
    let more = rows.len() > q.limit;
    rows.truncate(q.limit);
    let next = more.then(|| rows.last().unwrap()["key"].clone());
    Ok(json!({"items":rows,"next_cursor":next,"has_more":more,"since":q.since}))
}
pub fn summary(e: &Engine, q: &Summary) -> Result<Value> {
    ensure!(q.since >= 0, invalid("since must be nonnegative"));
    let c = e.db.connection()?;
    let (active,finished):(u64,u64)=c.query_row(&format!("SELECT COALESCE(SUM(r.status IN {ACTIVE}),0),COALESCE(SUM(r.finished_at>=?1),0) FROM runs r WHERE (r.status IN {ACTIVE} OR r.finished_at>=?1) AND NOT EXISTS(SELECT 1 FROM shared_executions z WHERE z.run_id=r.id)"),[q.since],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let (stages,done,running):(u64,u64,u64)=c.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(s.status IN ('succeeded','failed','cancelled','timed_out','infrastructure_error','blocked','skipped')),0),COALESCE(SUM(s.status='running'),0) FROM stage_runs s JOIN runs r ON r.id=s.run_id WHERE r.status IN {ACTIVE} AND NOT EXISTS(SELECT 1 FROM shared_executions z WHERE z.run_id=r.id)"),[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let pending:u64=c.query_row("SELECT COUNT(*) FROM change_queue_items i JOIN change_queues q ON q.id=i.queue_id WHERE q.state IN ('active','cancelling') AND i.state='pending'",[],|r|r.get(0))?;
    let completed_changes: u64 = c.query_row(
        "SELECT COUNT(*) FROM change_queue_items WHERE run_id IS NULL AND finished_at>=?1",
        [q.since],
        |r| r.get(0),
    )?;
    let finished = finished + completed_changes;
    let planned:u64=c.query_row("SELECT COUNT(*) FROM schedules s WHERE s.state IN ('pending','paused','dispatching') AND NOT EXISTS(SELECT 1 FROM schedule_hidden h WHERE h.schedule_id=s.id)",[],|r|r.get(0))?;
    let schedule_cursor: i64 =
        c.query_row("SELECT COALESCE(MAX(id),0) FROM schedule_events", [], |r| {
            r.get(0)
        })?;
    drop(c);
    let items = list(
        e,
        &Query {
            view: active_name(),
            kind: all(),
            project_id: None,
            query: String::new(),
            after: None,
            limit: 50,
            since: q.since,
        },
    )?;
    Ok(
        json!({"since":q.since,"active_runs":active,"running_stages":running,"planned":planned,"pending_changes":pending,"current":{"done":done,"total":stages,"basis":"terminal stages in active runs"},"queue":{"done":finished,"total":active+finished+pending,"basis":"active runs, pending changes and runs finished since monitoring began; future schedules excluded"},"items":items["items"],"has_more":items["has_more"],"schedule_cursor":schedule_cursor}),
    )
}
fn active_name() -> String {
    "active".into()
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub project_id: String,
    pub target_kind: String,
    pub target_id: String,
    pub ordinal: Option<usize>,
    pub revision: Option<u64>,
    pub actor: Actor,
    pub reason: String,
}
pub fn cancel(e: &Engine, r: &Cancel) -> Result<Value> {
    ensure!(
        !r.reason.trim().is_empty() && r.reason.len() <= 512,
        invalid("reason must be 1..512 bytes")
    );
    e.db.project(&r.project_id)?;
    let cancel_run = |id: String| -> Result<Value> {
        let run = e.db.run(&id)?;
        ensure!(run.project_id == r.project_id, Fault::NotFound);
        let result = e.cancel(CancelRun {
            run_id: id,
            actor: r.actor,
            reason: r.reason.clone(),
        })?;
        Ok(
            json!({"run_id":result.id,"status":result.status,"cancel_requested":!result.status.terminal()}),
        )
    };
    match r.target_kind.as_str() {
        "run" => cancel_run(r.target_id.clone()),
        "queue" => {
            let project: String =
                e.db.connection()?
                    .query_row(
                        "SELECT project_id FROM change_queues WHERE id=?1",
                        [&r.target_id],
                        |v| v.get(0),
                    )
                    .optional()?
                    .ok_or(Fault::NotFound)?;
            ensure!(project == r.project_id, Fault::NotFound);
            crate::queues::cancel(
                e,
                crate::queues::Cancel {
                    queue_id: r.target_id.clone(),
                    actor: r.actor,
                    reason: r.reason.clone(),
                },
            )
        }
        "queue_item" => {
            let ordinal = r.ordinal.ok_or_else(|| invalid("ordinal required"))?;
            let _gate = e
                .queue_gate
                .lock()
                .map_err(|_| anyhow::anyhow!("queue lock poisoned"))?;
            let row:Option<(String,Option<String>,String)>=e.db.connection()?.query_row("SELECT q.project_id,i.run_id,i.state FROM change_queues q JOIN change_queue_items i ON i.queue_id=q.id WHERE q.id=?1 AND i.ordinal=?2",params![r.target_id,ordinal],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?;
            let (project, mut run, state) = row.ok_or(Fault::NotFound)?;
            ensure!(project == r.project_id, Fault::NotFound);
            if run.is_none() {
                run =
                    e.db.connection()?
                        .query_row(
                            "SELECT run_id FROM submissions WHERE project_id=?1 AND request_key=?2",
                            params![project, format!("queue:{}:{ordinal}", r.target_id)],
                            |row| row.get(0),
                        )
                        .optional()?;
            }
            if let Some(run_id) = run {
                let result = cancel_run(run_id.clone())?;
                let actual = e.db.run(&run_id)?;
                let state = if !actual.status.terminal() {
                    "running"
                } else if actual.status == crate::model::RunState::Succeeded {
                    "succeeded"
                } else if actual.status == crate::model::RunState::Cancelled {
                    "cancelled"
                } else {
                    "failed"
                };
                e.db.connection()?.execute("UPDATE change_queue_items SET run_id=?3,state=?4,finished_at=?5 WHERE queue_id=?1 AND ordinal=?2",params![r.target_id,ordinal,run_id,state,actual.finished_at])?;
                return Ok(result);
            }
            if state == "pending" {
                e.db.connection()?.execute("UPDATE change_queue_items SET state='cancelled',error=?3,finished_at=?4 WHERE queue_id=?1 AND ordinal=?2 AND state='pending'",params![r.target_id,ordinal,format!("Cancelled by {}: {}",r.actor.as_str(),r.reason),crate::db::now()])?;
            }
            Ok(
                json!({"queue_id":r.target_id,"ordinal":ordinal,"state":if state=="pending"{"cancelled"}else{&state}}),
            )
        }
        "schedule" => {
            let id = crate::schedules::Id {
                project_id: r.project_id.clone(),
                schedule_id: r.target_id.clone(),
            };
            let schedule = crate::schedules::get(e, &id)?;
            let current = schedule["revision"].as_u64().unwrap();
            ensure!(r.revision == Some(current), Fault::Conflict);
            let state = schedule["state"].as_str().unwrap();
            if state == "cancelled" {
                return Ok(schedule);
            }
            crate::schedules::change(
                e,
                &crate::schedules::Change {
                    project_id: r.project_id.clone(),
                    schedule_id: r.target_id.clone(),
                    action: crate::schedules::Action::Cancel,
                    expected_revision: current,
                    request_id: uuid::Uuid::now_v7().to_string(),
                    actor: r.actor.as_str().into(),
                    reason: r.reason.clone(),
                },
            )
        }
        _ => Err(invalid("unknown execution target")),
    }
}
