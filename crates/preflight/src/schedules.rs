//! Durable schedules; the saved-start idempotency key bridges dispatch crashes.
use crate::{
    control::ConfigRef,
    engine::Engine,
    model::{Fault, invalid},
    profiles::{self, Revision, SavedStart},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Misfire {
    RunOnce,
    Skip,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    Skip,
    Queue,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Repeat {
    Interval {
        every_seconds: u64,
        overlap: Overlap,
    },
    Calendar {
        cron: String,
        timezone: String,
        overlap: Overlap,
    },
}
impl Repeat {
    fn overlap(&self) -> &Overlap {
        match self {
            Self::Interval { overlap, .. } | Self::Calendar { overlap, .. } => overlap,
        }
    }
    fn next(&self, at: i64, now: i64) -> Result<i64> {
        match self {
            Self::Interval { every_seconds, .. } => {
                ensure!(
                    (60..=31_536_000).contains(every_seconds),
                    invalid("repeat.every_seconds must be 60..31536000")
                );
                let step = (*every_seconds * 1000) as i64;
                at.checked_add((now.saturating_sub(at) / step + 1).saturating_mul(step))
                    .ok_or_else(|| invalid("schedule timestamp overflow"))
            }
            Self::Calendar { cron, timezone, .. } => crate::calendar::next(cron, timezone, now),
        }
    }
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub project_id: String,
    pub name: String,
    pub pipeline: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<Revision>,
    pub profile: Option<Revision>,
    /// Absolute UTC Unix milliseconds, avoiding ambiguous local/DST timestamps.
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
    pub misfire: Misfire,
    pub grace_ms: u64,
    pub request_id: String,
    pub actor: String,
    pub reason: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Id {
    pub project_id: String,
    pub schedule_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub project_id: String,
    pub before: Option<String>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Pause,
    Resume,
    Cancel,
    Delete,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub project_id: String,
    pub schedule_id: String,
    pub action: Action,
    pub expected_revision: u64,
    pub request_id: String,
    pub actor: String,
    pub reason: String,
}
/// Replace only future dispatch configuration; creation identity remains immutable.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub schedule_id: String,
    pub expected_revision: u64,
    pub definition: Create,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Due {
    pub project_id: Option<String>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    10
}
fn audit(actor: &str, reason: &str) -> Result<()> {
    ensure!(
        !actor.trim().is_empty()
            && actor.len() <= 128
            && !reason.trim().is_empty()
            && reason.len() <= 1024,
        invalid("bounded actor and reason required")
    );
    Ok(())
}
fn event(c: &rusqlite::Connection, id: &str, state: &str, actor: &str, reason: &str) -> Result<()> {
    c.execute("INSERT INTO schedule_events(schedule_id,state,actor,reason,timestamp) VALUES (?1,?2,?3,?4,?5)",params![id,state,actor,reason,crate::db::now()])?;
    Ok(())
}
fn read(c: &rusqlite::Connection, r: &Id) -> Result<Value> {
    type ScheduleRow = (String, i64, String, u64, Option<String>, Option<String>);
    let row: Option<ScheduleRow> = c.query_row("SELECT request_json,at_ms,state,revision,run_id,reason FROM schedules WHERE id=?1 AND project_id=?2",params![r.schedule_id,r.project_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?,v.get(3)?,v.get(4)?,v.get(5)?))).optional()?;
    let (request, at, state, revision, run, reason) = row.ok_or(Fault::NotFound)?;
    let request: Create = serde_json::from_str(&request)?;
    let deleted: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedule_hidden WHERE schedule_id=?1)",
        [&r.schedule_id],
        |v| v.get(0),
    )?;
    let run_status: Option<String> = if let Some(id) = &run {
        c.query_row("SELECT status FROM runs WHERE id=?1", [id], |v| v.get(0))
            .optional()?
    } else {
        None
    };
    Ok(
        json!({"schedule_id":r.schedule_id,"deleted":deleted,"project_id":r.project_id,"name":request.name,"pipeline":request.pipeline,"experiment":request.experiment,"profile":request.profile,"repeat":request.repeat,"at_ms":at,"time_basis":"UTC Unix milliseconds","misfire":request.misfire,"grace_ms":request.grace_ms,"state":state,"revision":revision,"run_id":run,"run_status":run_status,"reason":reason}),
    )
}
pub fn create(e: &Engine, r: &Create) -> Result<Value> {
    audit(&r.actor, &r.reason)?;
    crate::control::validate_name(&r.name)?;
    crate::control::validate_name(&r.request_id)?;
    ensure!(
        r.at_ms > 0 && r.grace_ms <= 7 * 24 * 60 * 60 * 1000,
        invalid("positive at_ms and grace_ms <= 7 days required")
    );
    let first_at = match &r.repeat {
        Some(Repeat::Calendar { cron, timezone, .. }) => {
            crate::calendar::next(cron, timezone, r.at_ms - 1)?
        }
        Some(repeat) => {
            repeat.next(r.at_ms, r.at_ms)?;
            r.at_ms
        }
        None => r.at_ms,
    };
    let encoded = serde_json::to_string(r)?;
    {
        let c = e.db.connection()?;
        let old: Option<(String, String)> = c
            .query_row(
                "SELECT COALESCE(o.request_json,s.request_json),s.id FROM schedules s LEFT JOIN schedule_original_requests o ON o.schedule_id=s.id WHERE s.project_id=?1 AND s.request_id=?2",
                params![r.project_id, r.request_id],
                |v| Ok((v.get(0)?, v.get(1)?)),
            )
            .optional()?;
        if let Some((old, id)) = old {
            ensure!(old == encoded, Fault::IdempotencyConflict);
            return read(
                &c,
                &Id {
                    project_id: r.project_id.clone(),
                    schedule_id: id,
                },
            );
        }
    }
    ensure!(
        r.pipeline.revision > 0,
        invalid("explicit pipeline revision required")
    );
    validate_experiment(e, r)?;
    e.db.pipeline_config(&ConfigRef {
        project_id: r.project_id.clone(),
        name: r.pipeline.name.clone(),
        revision: Some(r.pipeline.revision),
    })?;
    if let Some(p) = &r.profile {
        ensure!(
            p.revision > 0,
            invalid("explicit profile revision required")
        );
        profiles::get(
            e,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: p.name.clone(),
                revision: Some(p.revision),
            },
        )?;
    }
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT COALESCE(o.request_json,s.request_json),s.id FROM schedules s LEFT JOIN schedule_original_requests o ON o.schedule_id=s.id WHERE s.project_id=?1 AND s.request_id=?2",
            params![r.project_id, r.request_id],
            |v| Ok((v.get(0)?, v.get(1)?)),
        )
        .optional()?;
    if let Some((old, id)) = old {
        ensure!(old == encoded, Fault::IdempotencyConflict);
        return read(
            &tx,
            &Id {
                project_id: r.project_id.clone(),
                schedule_id: id,
            },
        );
    }
    let id = uuid::Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO schedules VALUES (?1,?2,?3,?4,?5,'pending',1,NULL,NULL,?6)",
        params![
            id,
            r.project_id,
            r.request_id,
            encoded,
            first_at,
            crate::db::now()
        ],
    )?;
    event(&tx, &id, "pending", &r.actor, &r.reason)?;
    let result = read(
        &tx,
        &Id {
            project_id: r.project_id.clone(),
            schedule_id: id,
        },
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn get(e: &Engine, r: &Id) -> Result<Value> {
    let c = e.db.connection()?;
    let mut value = read(&c, r)?;
    let events=c.prepare("SELECT state,actor,reason,timestamp FROM schedule_events WHERE schedule_id=?1 ORDER BY id DESC LIMIT 10")?.query_map([&r.schedule_id],|v|Ok(json!({"state":v.get::<_,String>(0)?,"actor":v.get::<_,String>(1)?,"reason":v.get::<_,String>(2)?,"timestamp":v.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    value["recent_events"] = json!(events);
    let occurrences=c.prepare("SELECT at_ms,state,run_id,reason FROM schedule_occurrences WHERE schedule_id=?1 ORDER BY at_ms DESC LIMIT 20")?.query_map([&r.schedule_id],|v|Ok(json!({"at_ms":v.get::<_,i64>(0)?,"state":v.get::<_,String>(1)?,"run_id":v.get::<_,Option<String>>(2)?,"reason":v.get::<_,String>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    value["recent_occurrences"] = json!(occurrences);
    Ok(value)
}
pub fn list(e: &Engine, r: &List) -> Result<Value> {
    e.db.project(&r.project_id)?;
    let c = e.db.connection()?;
    let ids=c.prepare("SELECT id FROM schedules WHERE project_id=?1 AND NOT EXISTS(SELECT 1 FROM schedule_hidden h WHERE h.schedule_id=schedules.id) AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT 21")?.query_map(params![r.project_id,r.before],|v|v.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = ids.len() > 20;
    let mut rows = Vec::new();
    for id in ids.into_iter().take(20) {
        rows.push(read(
            &c,
            &Id {
                project_id: r.project_id.clone(),
                schedule_id: id,
            },
        )?);
    }
    let cursor = more.then(|| rows.last().unwrap()["schedule_id"].clone());
    Ok(json!({"schedules":rows,"next_cursor":cursor}))
}
pub fn update(e: &Engine, r: &Update) -> Result<Value> {
    let request = &r.definition;
    validate_experiment(e, request)?;
    audit(&request.actor, &request.reason)?;
    crate::control::validate_name(&request.name)?;
    crate::control::validate_name(&request.request_id)?;
    let encoded = serde_json::to_string(r)?;
    {
        let c = e.db.connection()?;
        let old: Option<(String, String)> = c.query_row(
        "SELECT request_json,result_json FROM schedule_changes WHERE project_id=?1 AND request_id=?2",
        params![request.project_id,request.request_id], |v| Ok((v.get(0)?,v.get(1)?)),
    ).optional()?;
        if let Some((old, result)) = old {
            ensure!(old == encoded, Fault::IdempotencyConflict);
            return Ok(serde_json::from_str(&result)?);
        }
    }
    ensure!(
        request.pipeline.revision > 0,
        invalid("explicit pipeline revision required")
    );
    e.db.pipeline_config(&ConfigRef {
        project_id: request.project_id.clone(),
        name: request.pipeline.name.clone(),
        revision: Some(request.pipeline.revision),
    })?;
    if let Some(profile) = &request.profile {
        ensure!(
            profile.revision > 0,
            invalid("explicit profile revision required")
        );
        profiles::get(
            e,
            &ConfigRef {
                project_id: request.project_id.clone(),
                name: profile.name.clone(),
                revision: Some(profile.revision),
            },
        )?;
    }
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let old: Option<(String, String)> = tx.query_row(
        "SELECT request_json,result_json FROM schedule_changes WHERE project_id=?1 AND request_id=?2",
        params![request.project_id,request.request_id], |v| Ok((v.get(0)?,v.get(1)?)),
    ).optional()?;
    if let Some((old, result)) = old {
        ensure!(old == encoded, Fault::IdempotencyConflict);
        return Ok(serde_json::from_str(&result)?);
    }
    let id = Id {
        project_id: request.project_id.clone(),
        schedule_id: r.schedule_id.clone(),
    };
    let before = read(&tx, &id)?;
    ensure!(before["revision"] == r.expected_revision, Fault::Conflict);
    ensure!(
        before["deleted"] != true,
        invalid("schedule has been deleted")
    );
    ensure!(
        matches!(before["state"].as_str(), Some("pending" | "paused")),
        invalid("only pending or paused schedules can be edited")
    );
    ensure!(
        request.at_ms > 0 && request.grace_ms <= 7 * 24 * 60 * 60 * 1000,
        invalid("positive at_ms and grace_ms <= 7 days required")
    );
    let first_at = match &request.repeat {
        Some(Repeat::Calendar { cron, timezone, .. }) => {
            crate::calendar::next(cron, timezone, request.at_ms - 1)?
        }
        Some(repeat) => {
            repeat.next(request.at_ms, request.at_ms)?;
            request.at_ms
        }
        None => request.at_ms,
    };
    let collision: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedule_occurrences WHERE schedule_id=?1 AND at_ms=?2)",
        params![r.schedule_id, first_at],
        |v| v.get(0),
    )?;
    ensure!(
        !collision,
        invalid("this occurrence time already has history; choose a new time")
    );
    tx.execute("INSERT OR IGNORE INTO schedule_original_requests SELECT id,request_json FROM schedules WHERE id=?1",[&r.schedule_id])?;
    tx.execute(
        "UPDATE schedules SET request_json=?2,at_ms=?3,revision=revision+1,reason=?4 WHERE id=?1",
        params![
            r.schedule_id,
            serde_json::to_string(request)?,
            first_at,
            request.reason
        ],
    )?;
    event(
        &tx,
        &r.schedule_id,
        before["state"].as_str().unwrap(),
        &request.actor,
        &request.reason,
    )?;
    let result = read(&tx, &id)?;
    tx.execute(
        "INSERT INTO schedule_changes VALUES (?1,?2,?3,?4)",
        params![
            request.project_id,
            request.request_id,
            encoded,
            result.to_string()
        ],
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn change(e: &Engine, r: &Change) -> Result<Value> {
    audit(&r.actor, &r.reason)?;
    crate::control::validate_name(&r.request_id)?;
    let encoded = serde_json::to_string(r)?;
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let old:Option<(String,String)>=tx.query_row("SELECT request_json,result_json FROM schedule_changes WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|v|Ok((v.get(0)?,v.get(1)?))).optional()?;
    if let Some((old, result)) = old {
        ensure!(old == encoded, Fault::IdempotencyConflict);
        return Ok(serde_json::from_str(&result)?);
    }
    let id = Id {
        project_id: r.project_id.clone(),
        schedule_id: r.schedule_id.clone(),
    };
    let before = read(&tx, &id)?;
    ensure!(before["revision"] == r.expected_revision, Fault::Conflict);
    ensure!(
        before["deleted"] != true,
        invalid("schedule has been deleted")
    );
    let state = before["state"].as_str().unwrap();
    let next = match r.action {
        Action::Delete => {
            ensure!(state != "dispatching", Fault::Busy);
            tx.execute(
                "INSERT INTO schedule_hidden VALUES (?1,?2)",
                params![r.schedule_id, crate::db::now()],
            )?;
            "cancelled"
        }
        Action::Pause => {
            ensure!(
                state == "pending",
                invalid("only pending schedules can pause")
            );
            "paused"
        }
        Action::Resume => {
            ensure!(
                state == "paused",
                invalid("only paused schedules can resume")
            );
            "pending"
        }
        Action::Cancel => {
            ensure!(
                matches!(state, "pending" | "paused"),
                invalid(
                    "already dispatching/submitted; inspect run_id and cancel the run separately"
                )
            );
            "cancelled"
        }
    };
    tx.execute(
        "UPDATE schedules SET state=?2,revision=revision+1,reason=?3 WHERE id=?1",
        params![r.schedule_id, next, r.reason],
    )?;
    event(&tx, &r.schedule_id, next, &r.actor, &r.reason)?;
    let result = read(&tx, &id)?;
    tx.execute(
        "INSERT INTO schedule_changes VALUES (?1,?2,?3,?4)",
        params![r.project_id, r.request_id, encoded, result.to_string()],
    )?;
    tx.commit()?;
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
fn advance(
    c: &rusqlite::Connection,
    id: &str,
    at: i64,
    now: i64,
    repeat: &Option<Repeat>,
    state: &str,
    run: Option<&str>,
    reason: &str,
) -> Result<()> {
    c.execute(
        "INSERT INTO schedule_occurrences VALUES (?1,?2,?3,?4,?5) ON CONFLICT(schedule_id,at_ms) DO NOTHING",
        params![id, at, state, run, reason],
    )?;
    let next = repeat.as_ref().map(|r| r.next(at, now)).transpose()?;
    c.execute("UPDATE schedules SET state=?2,at_ms=?3,run_id=COALESCE(?4,run_id),reason=?5,revision=revision+1 WHERE id=?1",params![id,if next.is_some(){"pending"}else{state},next.unwrap_or(at),run,reason])?;
    Ok(())
}
pub fn run_due(e: &Arc<Engine>, r: &Due) -> Result<Value> {
    ensure!((1..=32).contains(&r.limit), invalid("limit must be 1..32"));
    if let Some(p) = &r.project_id {
        e.db.project(p)?;
    }
    let Ok(_gate) = e.schedule_gate.try_lock() else {
        return Ok(json!({"dispatched":[],"busy":true}));
    };
    let now = crate::db::now();
    let rows = {
        let c = e.db.connection()?;
        let mut q=c.prepare("SELECT id,project_id,request_json,at_ms FROM schedules WHERE state IN ('pending','dispatching') AND at_ms<=?1 AND (?2 IS NULL OR project_id=?2) ORDER BY at_ms,id LIMIT ?3")?;
        q.query_map(params![now, r.project_id, r.limit], |v| {
            Ok((
                v.get::<_, String>(0)?,
                v.get::<_, String>(1)?,
                v.get::<_, String>(2)?,
                v.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut result = Vec::new();
    for (id, project, encoded, at) in rows {
        if e.is_stopping() {
            break;
        }
        let request: Create = serde_json::from_str(&encoded)?;
        {
            let mut c = e.db.connection()?;
            let tx = c.transaction()?;
            let state: String =
                tx.query_row("SELECT state FROM schedules WHERE id=?1", [&id], |v| {
                    v.get(0)
                })?;
            if !matches!(state.as_str(), "pending" | "dispatching") {
                continue;
            }
            if state == "pending"
                && matches!(request.misfire, Misfire::Skip)
                && now.saturating_sub(at) > request.grace_ms as i64
            {
                advance(
                    &tx,
                    &id,
                    at,
                    now,
                    &request.repeat,
                    "skipped",
                    None,
                    "missed scheduled time",
                )?;
                event(&tx, &id, "skipped", "scheduler", "missed scheduled time")?;
                tx.commit()?;
                drop(c);
                result.push(get(
                    e,
                    &Id {
                        project_id: project,
                        schedule_id: id,
                    },
                )?);
                continue;
            }
            if let Some(repeat) = &request.repeat
                && state == "pending"
            {
                let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM schedule_occurrences o JOIN runs r ON r.id=o.run_id WHERE o.schedule_id=?1 AND r.status IN ('queued','preparing','running','collecting'))", [&id], |r|r.get(0))?;
                if active {
                    if matches!(repeat.overlap(), Overlap::Skip) {
                        advance(
                            &tx,
                            &id,
                            at,
                            now,
                            &request.repeat,
                            "skipped",
                            None,
                            "previous occurrence is active",
                        )?;
                        event(
                            &tx,
                            &id,
                            "skipped",
                            "scheduler",
                            "previous occurrence is active",
                        )?;
                    }
                    tx.commit()?;
                    continue;
                }
            }
            if state == "pending" {
                tx.execute("UPDATE schedules SET state='dispatching',revision=revision+1,reason=NULL WHERE id=?1",[&id])?;
                event(&tx, &id, "dispatching", "scheduler", "claimed due schedule")?;
            }
            tx.commit()?;
        }
        let key = if request.repeat.is_some() {
            format!("schedule:{id}:{at}")
        } else {
            format!("schedule:{id}")
        };
        let started = if let Some(experiment) = request.experiment {
            crate::experiments::start(
                e,
                crate::experiments::Start {
                    project_id: project.clone(),
                    name: experiment.name,
                    revision: experiment.revision,
                    idempotency_key: key,
                },
            )
            .and_then(|v| {
                e.db.run(
                    v["run_id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing experiment run ID"))?,
                )
            })
        } else {
            profiles::start(
                e,
                SavedStart {
                    project_id: project.clone(),
                    name: request.pipeline.name,
                    revision: Some(request.pipeline.revision),
                    profile: request.profile,
                    idempotency_key: Some(key),
                },
            )
        };
        let (state, run, reason) = match started {
            Ok(run) => (
                "submitted",
                Some(run.id),
                "scheduled run accepted".to_string(),
            ),
            Err(error) => {
                let retry =
                    matches!(error.downcast_ref::<Fault>(), Some(Fault::Busy)) || e.is_stopping();
                (
                    if retry { "pending" } else { "failed" },
                    None,
                    error.to_string().chars().take(1024).collect(),
                )
            }
        };
        {
            let mut c = e.db.connection()?;
            let tx = c.transaction()?;
            if state == "pending" {
                tx.execute("UPDATE schedules SET state='pending',reason=?2,revision=revision+1 WHERE id=?1", params![id,reason])?;
            } else {
                advance(
                    &tx,
                    &id,
                    at,
                    now,
                    &request.repeat,
                    state,
                    run.as_deref(),
                    &reason,
                )?;
            }
            event(&tx, &id, state, "scheduler", &reason)?;
            tx.commit()?;
        }
        result.push(get(
            e,
            &Id {
                project_id: project,
                schedule_id: id,
            },
        )?);
    }
    Ok(json!({"dispatched":result,"busy":false}))
}
pub async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {biased;_=stop.cancelled()=>break,_=tick.tick()=>{}}
        if let Err(error) = run_due(
            &e,
            &Due {
                project_id: None,
                limit: 10,
            },
        ) {
            eprintln!("schedule dispatcher: {error:#}");
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub at_ms: i64,
    pub repeat: Option<Repeat>,
    #[serde(default = "preview_count")]
    pub count: usize,
}
fn preview_count() -> usize {
    5
}
pub fn preview(r: &Preview) -> Result<Value> {
    ensure!(
        r.at_ms > 0 && (1..=10).contains(&r.count),
        invalid("positive at_ms and count 1..10 required")
    );
    let mut at = match &r.repeat {
        Some(Repeat::Calendar { cron, timezone, .. }) => {
            crate::calendar::next(cron, timezone, r.at_ms - 1)?
        }
        _ => r.at_ms,
    };
    if let Some(repeat) = &r.repeat {
        repeat.next(at, at)?;
    }
    let mut times = vec![at];
    if let Some(repeat) = &r.repeat {
        for _ in 1..r.count {
            at = repeat.next(at, at)?;
            times.push(at);
        }
    }
    Ok(
        json!({"at_ms":times,"dst":"skip nonexistent wall times; repeated wall times run once at earlier offset","missed":"coalesce to at most one execution"}),
    )
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub project_id: String,
    pub schedule_id: String,
    pub before_at: Option<i64>,
}
pub fn history(e: &Engine, r: &History) -> Result<Value> {
    let c = e.db.connection()?;
    read(
        &c,
        &Id {
            project_id: r.project_id.clone(),
            schedule_id: r.schedule_id.clone(),
        },
    )?;
    let mut rows=c.prepare("SELECT o.at_ms,o.state,o.run_id,o.reason,r.status FROM schedule_occurrences o LEFT JOIN runs r ON r.id=o.run_id WHERE o.schedule_id=?1 AND (?2 IS NULL OR o.at_ms<?2) ORDER BY o.at_ms DESC LIMIT 21")?.query_map(params![r.schedule_id,r.before_at],|v|Ok(json!({"at_ms":v.get::<_,i64>(0)?,"state":v.get::<_,String>(1)?,"run_id":v.get::<_,Option<String>>(2)?,"reason":v.get::<_,String>(3)?,"run_status":v.get::<_,Option<String>>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > 20;
    rows.truncate(20);
    let cursor = if more {
        rows.last().map(|r| r["at_ms"].clone())
    } else {
        None
    };
    Ok(json!({"occurrences":rows,"next_cursor":cursor}))
}

fn validate_experiment(e: &Engine, r: &Create) -> Result<()> {
    if let Some(experiment) = &r.experiment {
        ensure!(
            experiment.revision > 0 && r.profile.is_none(),
            invalid("explicit experiment revision required; its saved profile is used")
        );
        let saved = crate::experiments::get(
            e,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: experiment.name.clone(),
                revision: Some(experiment.revision),
            },
        )?;
        ensure!(
            saved["definition"]["pipeline"]["name"] == r.pipeline.name
                && saved["definition"]["pipeline"]["revision"] == r.pipeline.revision,
            invalid("schedule pipeline must match the experiment's recorded base pipeline")
        );
    }
    Ok(())
}
