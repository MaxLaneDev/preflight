//! Explicit run-to-thread delivery. The operator chooses the executable, never pipeline data.
use crate::{db::Db, engine::Engine, model::invalid};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[cfg(unix)]
mod desktop;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Immediate,
    Queued,
}
pub fn policy(config: &crate::config::Config) -> Value {
    json!({"configured_mode":config.codex_delivery_mode,"effective_mode":config.codex_delivery_mode.unwrap_or(if config.codex_ipc_socket.is_some(){DeliveryMode::Immediate}else{DeliveryMode::Queued}),"resume_idle":config.codex_delivery_mode==Some(DeliveryMode::Immediate),"desktop_ipc_configured":config.codex_ipc_socket.is_some(),"enabled":config.codex_command.is_some(),"setting":"codex_delivery_mode","configure_tool":"server_settings"})
}
pub fn status(engine: &Engine, run: &str) -> Result<Value> {
    let mut value = engine.db.codex_bindings(run)?;
    value["delivery_policy"] = policy(&engine.current_config());
    Ok(value)
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bind {
    pub run_id: String,
    pub thread_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingId {
    pub binding_id: String,
}
pub struct Delivery {
    pub id: String,
    pub thread_id: String,
    pub message: String,
}
pub fn bind(engine: &Engine, request: &Bind) -> Result<Value> {
    ensure!(
        engine.current_config().codex_command.is_some(),
        invalid("Codex delivery is disabled; operator must configure codex_command")
    );
    engine.db.bind_codex(request)
}
impl Db {
    pub(crate) fn bind_codex(&self, r: &Bind) -> Result<Value> {
        self.run(&r.run_id)?;
        ensure!(
            !self.is_shared_execution(&r.run_id)?,
            invalid("bind the subscribing run, not its shared execution")
        );
        let thread = uuid::Uuid::parse_str(&r.thread_id)
            .map_err(|_| invalid("thread_id must be a session UUID, not a name"))?
            .to_string();
        let id = uuid::Uuid::now_v7().to_string();
        let c = self.connection()?;
        c.execute("INSERT INTO codex_bindings(id,run_id,thread_id,state) VALUES (?1,?2,?3,'pending') ON CONFLICT(run_id,thread_id) DO NOTHING",params![id,r.run_id,thread])?;
        let id: String = c.query_row(
            "SELECT id FROM codex_bindings WHERE run_id=?1 AND thread_id=?2",
            params![r.run_id, thread],
            |row| row.get(0),
        )?;
        drop(c);
        self.codex_binding(&id)
    }
    pub fn codex_binding(&self, id: &str) -> Result<Value> {
        self.connection()?.query_row("SELECT id,run_id,thread_id,state,attempts,last_error,created_at,updated_at FROM codex_bindings WHERE id=?1",[id],|r|Ok(json!({"binding_id":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"thread_id":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"attempts":r.get::<_,u32>(4)?,"last_error":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?,"updated_at":r.get::<_,String>(7)?}))).optional()?.ok_or(crate::model::Fault::NotFound.into())
    }
    pub fn codex_bindings(&self, run: &str) -> Result<Value> {
        self.run(run)?;
        let c = self.connection()?;
        let ids = c
            .prepare("SELECT id FROM codex_bindings WHERE run_id=?1 ORDER BY id LIMIT 101")?
            .query_map([run], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(c);
        let truncated = ids.len() > 100;
        let entries = ids
            .iter()
            .take(100)
            .map(|id| self.codex_binding(id))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"run_id":run,"bindings":entries,"truncated":truncated}))
    }
    pub fn disable_codex(&self, r: &BindingId) -> Result<Value> {
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let state: String = tx
            .query_row(
                "SELECT state FROM codex_bindings WHERE id=?1",
                [&r.binding_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(crate::model::Fault::NotFound)?;
        ensure!(state != "sending", crate::model::Fault::Busy);
        if state != "delivered" {
            tx.execute("UPDATE codex_bindings SET state='disabled',updated_at=CURRENT_TIMESTAMP WHERE id=?1",[&r.binding_id])?;
        }
        tx.commit()?;
        drop(c);
        self.codex_binding(&r.binding_id)
    }
    pub fn retry_codex(&self, r: &BindingId) -> Result<Value> {
        let changed=self.connection()?.execute("UPDATE codex_bindings SET state='pending',last_error=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND state='failed' AND attempts<3",[&r.binding_id])?;
        ensure!(
            changed == 1,
            invalid(
                "retry requires a definitely unstarted failed delivery with fewer than three attempts; uncertain delivery cannot be retried"
            )
        );
        self.codex_binding(&r.binding_id)
    }
    pub(crate) fn claim_codex(&self) -> Result<Option<Delivery>> {
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let candidate:Option<(String,String,String,String,String,i64)>=tx.query_row("SELECT b.id,b.thread_id,e.run_id,e.project_id,e.status,e.cursor FROM codex_bindings b JOIN completion_events e ON e.run_id=b.run_id WHERE b.state='pending' ORDER BY e.cursor,b.id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        let Some((id, thread, run, project, status, cursor)) = candidate else {
            return Ok(None);
        };
        tx.execute("UPDATE codex_bindings SET state='sending',attempts=attempts+1,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[&id])?;
        tx.commit()?;
        let payload = json!({"delivery_id":id,"run_id":run,"project_id":project,"status":status,"event_cursor":cursor});
        Ok(Some(Delivery {
            id,
            thread_id: thread,
            message: format!(
                "Preflight completion hook: {payload}\nRead this run's results through Preflight MCP and continue the user's existing task if still applicable. This message reports a completion, not a new task scope. Do not resume user-paused work. Do not repeat work for the same delivery_id."
            ),
        }))
    }
    pub(crate) fn finish_codex(&self, id: &str, state: &str, error: Option<&str>) -> Result<()> {
        ensure!(
            ["delivered", "failed", "uncertain"].contains(&state),
            "invalid delivery outcome"
        );
        self.connection()?.execute("UPDATE codex_bindings SET state=?2,last_error=?3,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND state='sending'",params![id,state,error])?;
        Ok(())
    }
}
pub async fn deliver(engine: &Engine, delivery: &Delivery) -> Result<()> {
    let config = engine.current_config();
    deliver_configured(engine, delivery, &config).await
}
async fn deliver_configured(
    engine: &Engine,
    delivery: &Delivery,
    config: &crate::config::Config,
) -> Result<()> {
    let command = config
        .codex_command
        .as_deref()
        .ok_or_else(|| invalid("Codex delivery disabled"))?;
    if config.codex_delivery_mode == Some(DeliveryMode::Queued) {
        return queue_cli(engine, delivery, command, false).await;
    }
    let resume_idle = config.codex_delivery_mode == Some(DeliveryMode::Immediate);
    #[cfg(unix)]
    if let Some(socket) = &config.codex_ipc_socket {
        let stopped = async {
            while !engine.is_stopping() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        let outcome = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(15), desktop::steer(socket, delivery)) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("desktop IPC timed out"))),
            _ = stopped => Err(anyhow::anyhow!("server stopping")),
        };
        return match outcome {
            Ok(true) => engine.db.finish_codex(&delivery.id, "delivered", None),
            Ok(false) if !engine.is_stopping() => {
                queue_cli(engine, delivery, command, resume_idle).await
            }
            result => engine.db.finish_codex(
                &delivery.id,
                "uncertain",
                Some(&format!(
                    "Codex desktop delivery failed: {}; acceptance unknown",
                    result
                        .err()
                        .map_or_else(|| "server stopping".into(), |error| error.to_string())
                )),
            ),
        };
    }
    queue_cli(engine, delivery, command, resume_idle).await
}

async fn queue_cli(
    engine: &Engine,
    delivery: &Delivery,
    command: &std::path::Path,
    resume: bool,
) -> Result<()> {
    let mut process = tokio::process::Command::new(command);
    process.args([
        "queue",
        "--thread",
        &delivery.thread_id,
        "--message",
        &delivery.message,
    ]);
    if resume {
        process.arg("--resume");
    }
    let child = process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            engine.db.finish_codex(
                &delivery.id,
                "failed",
                Some(&format!("command did not start: {e}")),
            )?;
            return Ok(());
        }
    };
    let stopped = async {
        while !engine.is_stopping() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    let result = tokio::select! {result=tokio::time::timeout(std::time::Duration::from_secs(15),child.wait())=>Some(result),_=stopped=>None};
    match result {
        Some(Ok(Ok(status))) if status.success() => {
            engine.db.finish_codex(&delivery.id, "delivered", None)?
        }
        other => {
            let _ = child.kill().await;
            let reason = match other {
                Some(Ok(Ok(status))) => format!("Codex queue exited {status}; acceptance unknown"),
                Some(Ok(Err(e))) => format!("wait failed: {e}; acceptance unknown"),
                Some(Err(_)) => "delivery timed out; acceptance unknown".into(),
                None => "server stopping; acceptance unknown".into(),
            };
            engine
                .db
                .finish_codex(&delivery.id, "uncertain", Some(&reason))?;
        }
    }
    Ok(())
}
pub async fn dispatch_once(engine: &Engine) -> Result<Option<Value>> {
    let config = engine.current_config();
    ensure!(
        config.codex_command.is_some(),
        invalid("Codex delivery disabled")
    );
    if engine.is_stopping() {
        return Ok(None);
    }
    let Some(delivery) = engine.db.claim_codex()? else {
        return Ok(None);
    };
    deliver_configured(engine, &delivery, &config).await?;
    Ok(Some(engine.db.codex_binding(&delivery.id)?))
}
pub async fn run(engine: Arc<Engine>) {
    while !engine.is_stopping() {
        if engine.current_config().codex_command.is_some()
            && let Err(error) = dispatch_once(&engine).await
        {
            eprintln!("Codex bridge: {error:#}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandSpec, RunState, StartBuild};
    #[test]
    fn durable_claim_dedup_disable_and_ambiguous_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite3");
        let db = Db::new(&path).unwrap();
        let project = db.register("p", temp.path()).unwrap();
        let request = StartBuild {
            project_id: project.id,
            command: CommandSpec {
                program: "tool".into(),
                args: vec![],
                timeout_seconds: 5,
            },
        };
        db.insert_run("r", &request, temp.path()).unwrap();
        let request = Bind {
            run_id: "r".into(),
            thread_id: "01a17f49-a837-7553-a343-bf597124dae1".into(),
        };
        let first = db.bind_codex(&request).unwrap();
        assert_eq!(first, db.bind_codex(&request).unwrap());
        assert!(db.claim_codex().unwrap().is_none());
        db.transition("r", RunState::Succeeded, Some(0), None, None)
            .unwrap();
        let other = Db::new(&path).unwrap();
        let claims = std::thread::scope(|scope| {
            let a = scope.spawn(|| db.claim_codex().unwrap());
            let b = scope.spawn(|| other.claim_codex().unwrap());
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(claims.iter().filter(|v| v.is_some()).count(), 1);
        let id = BindingId {
            binding_id: first["binding_id"].as_str().unwrap().into(),
        };
        assert!(db.disable_codex(&id).is_err());
        assert!(db.claim_codex().unwrap().is_none());
        db.reconcile().unwrap();
        db.reconcile().unwrap();
        drop(other);
        drop(db);
        let db = Db::new(&path).unwrap();
        assert_eq!(
            db.codex_binding(&id.binding_id).unwrap()["state"],
            "uncertain"
        );
        assert!(db.retry_codex(&id).is_err());
        assert!(db.claim_codex().unwrap().is_none());
        assert_eq!(db.disable_codex(&id).unwrap()["state"], "disabled");
        assert_eq!(db.bind_codex(&request).unwrap()["state"], "disabled");
        assert!(
            db.bind_codex(&Bind {
                run_id: "r".into(),
                thread_id: "other; echo BAD".into()
            })
            .is_err()
        );
    }
}
