//! SSE invalidation hints. Durable HTTP pages remain authoritative on reconnect.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;
pub const MAX_STREAMS: usize = 16;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub hints: bool,
    pub project_id: Option<String>,
    #[serde(default = "crate::db::now")]
    pub since: i64,
    pub run_id: Option<String>,
}
pub struct Events(mpsc::Receiver<Result<Event, Infallible>>);
impl futures_core::Stream for Events {
    type Item = Result<Event, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}
pub fn subscribe(e: &Arc<Engine>, q: Query) -> Result<Response> {
    let permit = e
        .live_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Fault::Busy)?;
    validate(e, &q)?;
    let (tx, rx) = mpsc::channel(4);
    let weak = Arc::downgrade(e);
    let stop = e.transport_stop();
    tokio::spawn(async move {
        let _permit = permit;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut previous = String::new();
        loop {
            tokio::select! {_=stop.cancelled()=>break,_=tx.closed()=>break,_=interval.tick()=>{}}
            let Some(e) = weak.upgrade() else { break };
            let query = q.clone();
            let value = tokio::task::spawn_blocking(move || {
                if query.hints {
                    let c = e.db.connection()?;
                    let version: i64 = c.query_row("PRAGMA data_version", [], |r| r.get(0))?;
                    Ok(json!({"hint":true,"revision":[c.total_changes(),version,crate::releases::revision(&e)]}))
                } else {
                    snapshot(&e, &query)
                }
            })
            .await;
            let Ok(value) = value else { break };
            let Ok(value) = value else { break };
            let current = value.to_string();
            if current != previous {
                previous = current.clone();
                let sent = tokio::select! {_=stop.cancelled()=>break,result=tx.send(Ok(Event::default().event("change").data(current)))=>result};
                if sent.is_err() {
                    break;
                }
            }
        }
    });
    Ok(Sse::new(Events(rx))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response())
}

pub fn snapshot(e: &Engine, q: &Query) -> Result<serde_json::Value> {
    validate(e, q)?;
    if q.project_id.is_none() {
        return Ok(
            json!({"global":crate::executions::summary(e,&crate::executions::Summary{since:q.since})?}),
        );
    }
    let completion = e.db.completion_cursor(q.project_id.as_ref().unwrap())?;
    let c = e.db.connection()?;
    let logs:i64=c.query_row("SELECT COALESCE(MAX(l.id),0) FROM log_events l JOIN runs r ON r.id=l.run_id WHERE r.project_id=?1 AND (?2 IS NULL OR r.id=?2)",rusqlite::params![q.project_id,q.run_id],|r|r.get(0))?;
    let run = if let Some(id) = &q.run_id {
        let status: String =
            c.query_row("SELECT status FROM runs WHERE id=?1", [id], |r| r.get(0))?;
        let stages = c
            .prepare(
                "SELECT name,status,finished_at FROM stage_runs WHERE run_id=?1 ORDER BY ordinal",
            )?
            .query_map([id], |r| {
                Ok(json!([
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?
                ]))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Some(json!({"id":id,"status":status,"stages":stages}))
    } else {
        None
    };
    Ok(json!({"completion_cursor":completion,"log_cursor":logs,"run":run}))
}
fn validate(e: &Engine, q: &Query) -> Result<()> {
    ensure!(q.since >= 0, invalid("since must be nonnegative"));
    ensure!(
        !q.hints || q.project_id.is_none() && q.run_id.is_none(),
        invalid("hints cannot select a project or run")
    );
    if let Some(project) = &q.project_id {
        ensure!(project.len() <= 64, invalid("invalid project"));
        e.db.project(project)?;
    }
    if let Some(id) = &q.run_id {
        let project = q
            .project_id
            .as_ref()
            .ok_or_else(|| invalid("project_id required with run_id"))?;
        ensure!(e.db.run(id)?.project_id == *project, Fault::NotFound);
    }
    Ok(())
}
