//! Durable change queues: immutable inputs, bounded parallelism, cumulative patch chains.
use crate::{
    engine::Engine,
    model::{Actor, CancelRun, Fault, invalid},
    pipeline::{PipelineFile, StartPipeline},
    profiles::{Revision, Submission},
    sources::{GitSource, SourceRequest},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Independent,
    Cumulative,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub id: String,
    pub source: GitSource,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub project_id: String,
    pub name: String,
    pub request_id: String,
    pub definition: PipelineFile,
    pub profile: Option<Revision>,
    pub mode: Mode,
    #[serde(default = "one")]
    pub max_parallel: usize,
    pub changes: Vec<Change>,
}
fn one() -> usize {
    1
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Id {
    pub queue_id: String,
    #[serde(default)]
    pub details: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub project_id: String,
    pub before: Option<String>,
    #[serde(default = "page_size")]
    pub limit: usize,
}
fn page_size() -> usize {
    20
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    #[serde(default)]
    pub details: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub queue_id: String,
    pub actor: Actor,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    name: String,
    definition: PipelineFile,
    profile: Option<Value>,
    mode: Mode,
    max_parallel: usize,
}
fn submission(
    queue: &str,
    ordinal: usize,
    snapshot: &Snapshot,
    source: &GitSource,
) -> Result<Submission> {
    let mut request = json!({"operation":"change_queue","queue_id":queue,"ordinal":ordinal,"definition":snapshot.definition,"source":source});
    if let Some(profile) = &snapshot.profile {
        request["profile_snapshot"] = profile.clone();
    }
    Submission::new(Some(format!("queue:{queue}:{ordinal}")), request)
}
pub fn create(e: &Arc<Engine>, r: Create) -> Result<Value> {
    let request = serde_json::to_string(&r)?;
    ensure!(
        request.len() <= 65536,
        invalid("queue request exceeds 64 KiB")
    );
    e.db.project(&r.project_id)?;
    crate::control::validate_name(&r.name)?;
    crate::control::validate_name(&r.request_id)?;
    // Resolve immutable profile revisions only after checking exact request replay.
    let old: Option<(String, String)> =
        e.db.connection()?
            .query_row(
                "SELECT id,request_json FROM change_queues WHERE project_id=?1 AND request_key=?2",
                params![r.project_id, r.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
    if let Some((id, old)) = old {
        ensure!(request == old, Fault::IdempotencyConflict);
        return Ok(json!({"queue_id":id}));
    }
    ensure!(
        (1..=32).contains(&r.changes.len()),
        invalid("queue requires 1..32 changes")
    );
    ensure!(
        (1..=32).contains(&r.max_parallel),
        invalid("max_parallel must be 1..32")
    );
    ensure!(
        r.mode != Mode::Cumulative || r.max_parallel == 1,
        invalid("cumulative queues require max_parallel=1")
    );
    let mut definition = r.definition;
    let profile = if let Some(profile) = r.profile {
        ensure!(
            profile.revision > 0,
            invalid("profile revision must be explicit and positive")
        );
        let value = crate::profiles::get(
            e,
            &crate::control::ConfigRef {
                project_id: r.project_id.clone(),
                name: profile.name,
                revision: Some(profile.revision),
            },
        )?;
        let profile: crate::profiles::Profile =
            serde_json::from_value(value["definition"].clone())?;
        crate::profiles::apply(&mut definition, &profile)?;
        Some(serde_json::to_value(profile)?)
    } else {
        None
    };
    crate::pipeline::validate(&definition)?;
    for stage in &definition.pipeline.stages {
        ensure!(
            !stage.command.program.contains("${tool."),
            invalid("tool references require a profile")
        );
        e.current_config().stage_memory(stage)?;
        ensure!(
            stage.required_slots() as usize <= e.current_config().max_parallel_commands,
            invalid("stage exceeds server slot capacity")
        );
        if stage.cache.is_some() {
            ensure!(
                profile.as_ref().and_then(|p| p.get("pins")).is_some(),
                invalid("cache requires pinned profile")
            );
        }
    }
    let mut names = std::collections::BTreeSet::new();
    let mut sources = Vec::new();
    let mut patches = Vec::new();
    let base = &r.changes[0].source.commit;
    for change in &r.changes {
        crate::control::validate_name(&change.id)?;
        ensure!(names.insert(&change.id), invalid("duplicate change id"));
        let mut source = change.source.clone();
        source.validate()?;
        if r.mode == Mode::Cumulative {
            ensure!(
                &source.commit == base,
                invalid("cumulative changes must share one exact base commit")
            );
            patches.extend(source.patches);
            source.patches = patches.clone();
            source.validate()?;
        }
        sources.push(source);
    }
    let snapshot = Snapshot {
        name: r.name,
        definition,
        profile,
        mode: r.mode,
        max_parallel: r.max_parallel,
    };
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT id,request_json FROM change_queues WHERE project_id=?1 AND request_key=?2",
            params![r.project_id, r.request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, old)) = old {
        ensure!(old == request, Fault::IdempotencyConflict);
        return Ok(json!({"queue_id":id}));
    }
    let active: usize = tx.query_row(
        "SELECT COUNT(*) FROM change_queues WHERE state IN ('active','cancelling')",
        [],
        |row| row.get(0),
    )?;
    ensure!(active < 32, Fault::Busy);
    let id = uuid::Uuid::now_v7().to_string();
    tx.execute("INSERT INTO change_queues(id,project_id,request_key,request_json,snapshot_json,state) VALUES (?1,?2,?3,?4,?5,'active')",params![id,r.project_id,r.request_id,request,serde_json::to_string(&snapshot)?])?;
    for (ordinal, (change, source)) in r.changes.iter().zip(sources).enumerate() {
        tx.execute("INSERT INTO change_queue_items(queue_id,ordinal,name,source_json,state) VALUES (?1,?2,?3,?4,'pending')",params![id,ordinal,change.id,serde_json::to_string(&source)?])?;
    }
    tx.commit()?;
    Ok(json!({"queue_id":id}))
}
pub fn status(e: &Engine, id: &str, details: bool) -> Result<Value> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let (project, state, snapshot): (String, String, String) = tx
        .query_row(
            "SELECT project_id,state,snapshot_json FROM change_queues WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(Fault::NotFound)?;
    let snapshot: Snapshot = serde_json::from_str(&snapshot)?;
    let items=tx.prepare("SELECT ordinal,name,run_id,state,error,source_json FROM change_queue_items WHERE queue_id=?1 ORDER BY ordinal")?.query_map([id],|row|Ok((row.get::<_,usize>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut result = Vec::new();
    for (ordinal, name, run, state, error, source) in items {
        *counts.entry(state.clone()).or_default() += 1;
        let source: GitSource = serde_json::from_str(&source)?;
        let mut v = json!({"i":ordinal,"id":name,"state":state});
        if let Some(run) = run {
            v["run_id"] = run.into();
        }
        if let Some(error) = error {
            v["error"] = error.into();
        }
        if details {
            v["source"] = source.identity()?;
        }
        result.push(v);
    }
    Ok(
        json!({"queue_id":id,"project_id":project,"name":snapshot.name,"state":state,"mode":snapshot.mode,"max_parallel":snapshot.max_parallel,"counts":counts,"items":result}),
    )
}
pub fn cancel(e: &Engine, r: Cancel) -> Result<Value> {
    ensure!(
        !r.reason.trim().is_empty() && r.reason.len() <= 512,
        invalid("cancellation reason must contain 1..512 bytes")
    );
    let _gate = e
        .queue_gate
        .lock()
        .map_err(|_| anyhow::anyhow!("queue lock poisoned"))?;
    let changed=e.db.connection()?.execute("UPDATE change_queues SET state='cancelling',cancel_actor=?2,cancel_reason=?3 WHERE id=?1 AND state='active'",params![r.queue_id,serde_json::to_string(&r.actor)?,r.reason])?;
    let exists: bool = e.db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM change_queues WHERE id=?1)",
        [&r.queue_id],
        |row| row.get(0),
    )?;
    ensure!(exists, Fault::NotFound);
    Ok(json!({"queue_id":r.queue_id,"cancel_requested":changed>0}))
}
pub fn list(e: &Engine, r: &List) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    e.db.project(&r.project_id)?;
    let values=e.db.connection()?.prepare("SELECT id,state FROM change_queues WHERE project_id=?1 AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT ?3")?.query_map(params![r.project_id,r.before,r.limit+1],|row|Ok(json!({"queue_id":row.get::<_,String>(0)?,"state":row.get::<_,String>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"queues":values.iter().take(r.limit).collect::<Vec<_>>(),"next_cursor":if values.len()>r.limit {values[r.limit-1]["queue_id"].clone()}else{Value::Null}}),
    )
}
pub fn tick(e: &Arc<Engine>) -> Result<()> {
    let _gate = e
        .queue_gate
        .lock()
        .map_err(|_| anyhow::anyhow!("queue lock poisoned"))?;
    if e.is_stopping() {
        return Ok(());
    }
    let queues=e.db.connection()?.prepare("SELECT id,project_id,state,snapshot_json,cancel_actor,cancel_reason FROM change_queues WHERE state IN ('active','cancelling') ORDER BY id LIMIT 32")?.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, project, state, snapshot, actor, reason) in queues {
        let snapshot: Snapshot = serde_json::from_str(&snapshot)?;
        let items=e.db.connection()?.prepare("SELECT ordinal,source_json,run_id,state FROM change_queue_items WHERE queue_id=?1 ORDER BY ordinal")?.query_map([&id],|row|Ok((row.get::<_,usize>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut pending = Vec::new();
        let mut running = 0;
        let mut failed = false;
        for (ordinal, source, run, item_state) in items {
            if !matches!(item_state.as_str(), "pending" | "running") {
                failed |= item_state != "succeeded";
                continue;
            }
            let source: GitSource = serde_json::from_str(&source)?;
            let submission = submission(&id, ordinal, &snapshot, &source)?;
            let run = if let Some(run) = run {
                Some(e.db.run(&run)?)
            } else {
                e.db.submitted(&project, &submission)?
            };
            if let Some(run) = run {
                if state == "cancelling" && !run.status.terminal() {
                    e.cancel(CancelRun {
                        run_id: run.id.clone(),
                        actor: serde_json::from_str(actor.as_deref().unwrap_or("\"agent\""))?,
                        reason: reason.clone().unwrap_or_else(|| "queue cancelled".into()),
                    })?;
                }
                let item_state = if !run.status.terminal() {
                    running += 1;
                    "running"
                } else if run.status == crate::model::RunState::Succeeded {
                    "succeeded"
                } else if run.status == crate::model::RunState::Cancelled {
                    failed = true;
                    "cancelled"
                } else {
                    failed = true;
                    "failed"
                };
                e.db.connection()?.execute("UPDATE change_queue_items SET run_id=?3,state=?4,error=?5,finished_at=?6 WHERE queue_id=?1 AND ordinal=?2",params![id,ordinal,run.id,item_state,run.reason,run.finished_at])?;
            } else {
                pending.push((ordinal, source, submission));
            }
        }
        for (ordinal, source, submission) in pending {
            if state == "cancelling" || (snapshot.mode == Mode::Cumulative && failed) {
                e.db.connection()?.execute(
                    "UPDATE change_queue_items SET state=?3,finished_at=?4 WHERE queue_id=?1 AND ordinal=?2",
                    params![
                        id,
                        ordinal,
                        if state == "cancelling" {
                            "cancelled"
                        } else {
                            "blocked"
                        },crate::db::now()
                    ],
                )?;
                continue;
            }
            if running >= snapshot.max_parallel {
                break;
            }
            match e.start_pipeline_source(
                StartPipeline {
                    project_id: project.clone(),
                    definition: Some(snapshot.definition.clone()),
                },
                Some(submission),
                Some(SourceRequest::Git(source)),
            ) {
                Ok(run) => {
                    e.db.connection()?.execute("UPDATE change_queue_items SET run_id=?3,state='running' WHERE queue_id=?1 AND ordinal=?2",params![id,ordinal,run.id])?;
                    running += 1;
                }
                Err(error) if matches!(error.downcast_ref::<Fault>(), Some(Fault::Busy)) => break,
                Err(error) => {
                    e.db.connection()?.execute("UPDATE change_queue_items SET state='failed',error=?3,finished_at=?4 WHERE queue_id=?1 AND ordinal=?2",params![id,ordinal,format!("{error:#}").chars().take(1024).collect::<String>(),crate::db::now()])?;
                    failed = true;
                }
            }
        }
        e.db.connection()?.execute("UPDATE change_queues SET state=?2 WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM change_queue_items WHERE queue_id=?1 AND state IN ('pending','running'))",params![id,if state=="cancelling"{"cancelled"}else{"finished"}])?;
    }
    Ok(())
}
pub async fn run(e: Arc<Engine>) {
    while !e.is_stopping() {
        if let Err(error) = tick(&e) {
            eprintln!("change queues: {error:#}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
