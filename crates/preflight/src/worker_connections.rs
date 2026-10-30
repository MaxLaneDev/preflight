//! Durable supervision of owned SSH launchers. Persisted PIDs never authorize killing.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
    worker_ssh,
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reconnect {
    pub max_retries: u8,
    #[serde(default = "retry_delay")]
    pub delay_seconds: u16,
}
fn retry_delay() -> u16 {
    10
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub request_id: String,
    pub connection: worker_ssh::Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<Reconnect>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub connection_id: String,
    #[serde(default)]
    pub details: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub project_id: String,
    pub connection_id: String,
    pub actor: String,
    pub reason: String,
    #[serde(default)]
    pub process_stopped: bool,
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
pub fn start(e: &Arc<Engine>, r: Start) -> Result<Value> {
    ensure!(
        cfg!(unix),
        invalid("managed SSH provisioning currently requires a Unix server")
    );
    crate::control::validate_name(&r.request_id)?;
    worker_ssh::validate(&r.connection)?;
    let (retry_limit, retry_delay_seconds) = match &r.reconnect {
        Some(policy) => {
            ensure!(
                (1..=10).contains(&policy.max_retries)
                    && (1..=300).contains(&policy.delay_seconds)
                    && !r.connection.once,
                invalid(
                    "reconnect requires max_retries=1..10, delay_seconds=1..300 and once=false"
                )
            );
            (policy.max_retries, policy.delay_seconds)
        }
        None => (0, 10),
    };
    e.db.project(&r.connection.project_id)?;
    let encoded = serde_json::to_string(&r.connection)?;
    ensure!(
        encoded.len() <= 16384,
        invalid("SSH request exceeds 16 KiB")
    );
    let recovery_worker = if let Some(recovery) = &r.connection.recovery {
        let proof =
            e.db.worker_recovery_status(&crate::worker_recovery::Query {
                project_id: r.connection.project_id.clone(),
                job_id: recovery.job_id.clone(),
            })?;
        ensure!(
            proof["recovery_allowed"] == true && proof["worker_name"] == r.connection.name,
            invalid(
                "journal recovery requires a lost job with operator-resolved protection and the same worker name"
            )
        );
        Some(
            proof["worker_id"]
                .as_str()
                .ok_or_else(|| invalid("missing recovery worker identity"))?
                .to_owned(),
        )
    } else {
        None
    };
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old: Option<(String, String, u8, u16)> = tx
        .query_row(
            "SELECT id,request_json,retry_limit,retry_delay_seconds FROM worker_connections WHERE project_id=?1 AND request_id=?2",
            params![r.connection.project_id, r.request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((id, previous, previous_limit, previous_delay)) = old {
        ensure!(
            previous == encoded
                && previous_limit == retry_limit
                && previous_delay == retry_delay_seconds,
            Fault::IdempotencyConflict
        );
        tx.commit()?;
        drop(c);
        return status(
            e,
            &Query {
                project_id: r.connection.project_id,
                connection_id: id,
                details: false,
            },
        );
    }
    ensure!(!e.is_stopping(), invalid("server is stopping"));
    ensure!(
        e.current_config().worker_bind.is_some(),
        invalid("configure the server worker_bind before provisioning")
    );
    let bundle = r
        .connection
        .bundle_dir
        .canonicalize()
        .context("resolve trusted package directory")?;
    ensure!(
        bundle == r.connection.bundle_dir
            && bundle.is_dir()
            && !bundle.starts_with(&e.config.data_dir)
            && e.current_config()
                .allowed_roots
                .iter()
                .any(|root| bundle.starts_with(root)),
        invalid(
            "bundle_dir must be a canonical directory inside operator allowed_roots and outside server data"
        )
    );
    let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worker_connections WHERE project_id=?1 AND name=?2 AND state IN ('queued','starting','connected','stopping','interrupted'))",params![r.connection.project_id,r.connection.name],|row|row.get(0))?;
    ensure!(
        !busy,
        invalid(
            "this project worker already has an active or interrupted connection; inspect it before retrying"
        )
    );
    let count:u64=tx.query_row("SELECT count(*) FROM worker_connections WHERE state IN ('queued','starting','connected','stopping')",[],|row|row.get(0))?;
    ensure!(count < 8, Fault::Busy);
    let id = uuid::Uuid::now_v7().to_string();
    tx.execute("INSERT INTO worker_connections(id,project_id,name,request_id,request_json,state,created_at,retry_limit,retry_delay_seconds,worker_id) VALUES (?1,?2,?3,?4,?5,'queued',?6,?7,?8,?9)",params![id,r.connection.project_id,r.connection.name,r.request_id,encoded,crate::db::now(),retry_limit,retry_delay_seconds,recovery_worker])?;
    tx.commit()?;
    drop(c);
    status(
        e,
        &Query {
            project_id: r.connection.project_id,
            connection_id: id,
            details: false,
        },
    )
}
pub fn status(e: &Engine, r: &Query) -> Result<Value> {
    let c = e.db.connection()?;
    let row:Option<(Value,String,Option<String>)>=c.query_row("SELECT id,name,state,worker_id,launcher_pid,created_at,finished_at,actor,reason,request_json,retry_limit,retry_delay_seconds,retries,attempt,next_retry_at,last_error FROM worker_connections WHERE id=?1 AND project_id=?2",params![r.connection_id,r.project_id],|row|{
        let worker:Option<String>=row.get(3)?;
        Ok((json!({"connection_id":row.get::<_,String>(0)?,"project_id":r.project_id,"name":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,"worker_id":worker,"launcher_pid":row.get::<_,Option<u32>>(4)?,"created_at":row.get::<_,i64>(5)?,"finished_at":row.get::<_,Option<i64>>(6)?,"actor":row.get::<_,Option<String>>(7)?,"reason":row.get::<_,Option<String>>(8)?,"reconnect":{"max_retries":row.get::<_,u8>(10)?,"delay_seconds":row.get::<_,u16>(11)?,"retries":row.get::<_,u8>(12)?,"attempt":row.get::<_,u16>(13)?,"next_retry_at":row.get::<_,Option<i64>>(14)?,"last_error":row.get::<_,Option<String>>(15)?}}),row.get(9)?,worker))
    }).optional()?;
    let (mut value, request, worker) = row.ok_or(Fault::NotFound)?;
    let blocked = worker
        .as_deref()
        .map(|id| crate::workers::blocked_operations(&c, id))
        .transpose()?
        .unwrap_or(0);
    let online = if let Some(worker) = worker {
        c.query_row("SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND project_id=?2 AND state='registered' AND job_protocol BETWEEN ?3 AND ?4 AND last_seen>?5)",params![worker,r.project_id,crate::worker_jobs::PROTOCOL,crate::worker_jobs::CONTAINER_PROTOCOL,crate::workers::now()-30],|row|row.get::<_,bool>(0))?
    } else {
        false
    };
    value["worker_online"] = online.into();
    value["execution_ready"] = (online && value["state"] == "connected" && blocked == 0).into();
    value["requires_recovery"] = (value["state"] == "interrupted" || blocked > 0).into();
    value["blocked_operations"] = blocked.into();
    if blocked > 0 {
        value["recovery_hint"] = "Inspect operations_abandoned and runs_operations; verify old processes stopped before operation_resolve. Lost jobs are never replayed.".into();
    }
    if r.details {
        value["connection"] = serde_json::from_str(&request)?;
    }
    Ok(value)
}
pub fn list(e: &Engine, r: &List) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    e.db.project(&r.project_id)?;
    let c = e.db.connection()?;
    let mut ids=c.prepare("SELECT id FROM worker_connections WHERE project_id=?1 AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT ?3")?.query_map(params![r.project_id,r.before,r.limit+1],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(c);
    let more = ids.len() > r.limit;
    ids.truncate(r.limit);
    let next = if more { ids.last().cloned() } else { None };
    let rows = ids
        .into_iter()
        .map(|id| {
            status(
                e,
                &Query {
                    project_id: r.project_id.clone(),
                    connection_id: id,
                    details: false,
                },
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"project_id":r.project_id,"connections":rows,"next_cursor":next}))
}
pub fn stop(e: &Engine, r: Stop) -> Result<Value> {
    ensure!(
        !r.actor.trim().is_empty()
            && r.actor.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 1024,
        invalid("actor/reason are required and bounded")
    );
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: Option<String> = tx
        .query_row(
            "SELECT state FROM worker_connections WHERE id=?1 AND project_id=?2",
            params![r.connection_id, r.project_id],
            |row| row.get(0),
        )
        .optional()?;
    let state = state.ok_or(Fault::NotFound)?;
    if state == "interrupted" {
        ensure!(
            r.process_stopped,
            invalid(
                "interrupted connection requires verified process_stopped=true; no persisted PID will be signalled"
            )
        );
    }
    if matches!(
        state.as_str(),
        "queued" | "starting" | "connected" | "interrupted"
    ) {
        let next = if matches!(state.as_str(), "queued" | "interrupted") {
            "stopped"
        } else {
            "stopping"
        };
        tx.execute("UPDATE worker_connections SET state=?2,actor=?3,reason=?4,next_retry_at=NULL,finished_at=CASE WHEN ?2='stopped' THEN ?5 ELSE NULL END WHERE id=?1",params![r.connection_id,next,r.actor,r.reason,crate::db::now()])?;
    }
    tx.commit()?;
    drop(c);
    status(
        e,
        &Query {
            project_id: r.project_id,
            connection_id: r.connection_id,
            details: false,
        },
    )
}
fn claim(e: &Engine) -> Result<Option<(String, worker_ssh::Request)>> {
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let next:Option<(String,String)>=tx.query_row("SELECT id,request_json FROM worker_connections WHERE state='queued' AND (next_retry_at IS NULL OR next_retry_at<=?1) ORDER BY id LIMIT 1",[crate::db::now()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let result = if let Some((id, encoded)) = next {
        tx.execute(
            "UPDATE worker_connections SET state='starting',attempt=attempt+1,next_retry_at=NULL WHERE id=?1",
            [&id],
        )?;
        Some((id, serde_json::from_str(&encoded)?))
    } else {
        None
    };
    tx.commit()?;
    Ok(result)
}
fn finish(e: &Engine, id: &str, result: Result<()>) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (state, limit, retries, delay): (String, u8, u8, u16) = tx.query_row(
        "SELECT state,retry_limit,retries,retry_delay_seconds FROM worker_connections WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let retry = result.is_err() && state != "stopping" && !e.is_stopping() && retries < limit;
    let next = if state == "stopping" || result.is_ok() {
        "stopped"
    } else if retry {
        "queued"
    } else {
        "failed"
    };
    let reason = result.err().map(|error| {
        let text = error.to_string().chars().take(4096).collect::<String>();
        regex::Regex::new("[a-fA-F0-9]{64}")
            .expect("static redaction pattern")
            .replace_all(&text, "[redacted]")
            .chars()
            .take(1024)
            .collect::<String>()
    });
    tx.execute("UPDATE worker_connections SET state=?2,launcher_pid=NULL,finished_at=CASE WHEN ?5 THEN NULL ELSE ?3 END,reason=CASE WHEN ?5 THEN reason ELSE COALESCE(reason,?4) END,last_error=COALESCE(?4,last_error),retries=retries+?5,next_retry_at=CASE WHEN ?5 THEN ?3+?6 ELSE NULL END WHERE id=?1 AND state IN ('starting','connected','stopping')",params![id,next,crate::db::now(),reason,retry,i64::from(delay)*1000])?;
    tx.commit()?;
    Ok(())
}

#[cfg(unix)]
async fn supervise(e: Arc<Engine>, id: &str, r: worker_ssh::Request) -> Result<()> {
    let live_config = e.current_config();
    let base = e.config.data_dir.join("worker-connections");
    std::fs::create_dir_all(&base)?;
    let attempt: u16 = e.db.connection()?.query_row(
        "SELECT attempt FROM worker_connections WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    let directory = base.join(id).join(format!("attempt-{attempt}"));
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let config = directory.join("config.toml");
    let request = directory.join("request.json");
    for (path, bytes) in [
        (&config, toml::to_string(&live_config)?.into_bytes()),
        (&request, serde_json::to_vec(&r)?),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::File::open(&directory)?.sync_all()?;
    let executable = live_config
        .worker_launcher
        .clone()
        .unwrap_or(std::env::current_exe()?);
    let resume_only: bool = e.db.connection()?.query_row(
        "SELECT worker_id IS NOT NULL FROM worker_connections WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--config")
        .arg(config)
        .arg("worker-ssh")
        .arg(request);
    if resume_only {
        command.arg("--resume");
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("start managed SSH launcher")?;
    let mut tree = crate::executor::ProcessTree::attach(&child)?;
    e.db.connection()?.execute(
        "UPDATE worker_connections SET launcher_pid=?2 WHERE id=?1",
        params![id, child.id()],
    )?;
    let mut stdout = child.stdout.take().context("launcher stdout")?;
    let mut stderr = child.stderr.take().context("launcher stderr")?;
    let (mut obuf, mut ebuf) = ([0u8; 4096], [0u8; 4096]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut output_open, mut error_open, mut ready) = (true, true, false);
    let start = Instant::now();
    let mut stopping: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {biased;
            _=tick.tick()=>{
                let state:String=e.db.connection()?.query_row("SELECT state FROM worker_connections WHERE id=?1",[id],|row|row.get(0))?;
                if (e.is_stopping() || state=="stopping") && stopping.is_none() {tree.interrupt()?;stopping=Some(Instant::now());}
                if stopping.is_some_and(|at|at.elapsed()>=Duration::from_secs(5)) {tree.terminate()?;}
                if !ready && stopping.is_none() && start.elapsed()>Duration::from_secs(150) {tree.terminate()?;let _=child.wait().await;anyhow::bail!("SSH installation/heartbeat deadline exceeded");}
            },
            n=stdout.read(&mut obuf),if output_open=>{
                let n=n?;output_open=n>0;
                if !ready {
                    out.extend_from_slice(&obuf[..n]);ensure!(out.len()<=8192,"launcher readiness reply exceeds 8 KiB");
                    if let Some(end)=out.iter().position(|b|*b==b'\n') {
                        let hello:Value=serde_json::from_slice(&out[..end]).context("invalid launcher readiness reply")?;
                        ensure!(hello["authenticated_heartbeat"]==true && (hello["native_jobs_ready"]==true || hello["execution_ready"]==true) && crate::worker_jobs::compatible(hello["job_protocol"].as_u64().and_then(|v|v.try_into().ok())),"launcher did not confirm compatible authenticated readiness");
                        let worker=hello["worker_id"].as_str().context("missing worker ID")?;
                        let verified:bool=e.db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND project_id=?2 AND name=?3 AND state='registered' AND job_protocol BETWEEN ?4 AND ?5 AND last_seen>?6)",params![worker,r.project_id,r.name,crate::worker_jobs::PROTOCOL,crate::worker_jobs::CONTAINER_PROTOCOL,crate::workers::now()-30],|row|row.get(0))?;
                        ensure!(verified,"worker readiness is not corroborated by server heartbeat");
                        let previous: Option<String> = e.db.connection()?.query_row("SELECT worker_id FROM worker_connections WHERE id=?1",[id],|row|row.get(0))?;
                        ensure!(previous.as_deref().is_none_or(|id|id==worker),"reconnected worker identity changed; inspect and explicitly provision a new connection");
                        e.db.connection()?.execute("UPDATE worker_connections SET state='connected',worker_id=?2 WHERE id=?1 AND state='starting'",params![id,worker])?;
                        ready=true;out.clear();
                    }
                }
            },
            n=stderr.read(&mut ebuf),if error_open=>{let n=n?;error_open=n>0;let remaining=8192usize.saturating_sub(err.len());err.extend_from_slice(&ebuf[..n.min(remaining)]);},
            status=child.wait()=>{
                let status=status?;tree.terminate()?;
                if stopping.is_some(){return Ok(());}
                ensure!(status.success() && ready,"SSH launcher failed before successful completion: {}",String::from_utf8_lossy(&err));
                ensure!(r.once, "SSH worker connection ended without a stop request");
                return Ok(());
            }
        }
    }
}
#[cfg(not(unix))]
async fn supervise(_: Arc<Engine>, _: &str, _: worker_ssh::Request) -> Result<()> {
    anyhow::bail!("managed SSH requires a Unix server")
}

pub async fn run(e: Arc<Engine>) {
    let mut tasks: tokio::task::JoinSet<(String, Result<()>)> = tokio::task::JoinSet::new();
    let mut ids = std::collections::HashMap::new();
    loop {
        while let Some(result) = tasks.try_join_next_with_id() {
            match result {
                Ok((task, (id, result))) => {
                    ids.remove(&task);
                    if let Err(error) = finish(&e, &id, result) {
                        eprintln!("SSH connection finalization failed: {error}");
                    }
                }
                Err(error) => {
                    if let Some(id) = ids.remove(&error.id()) {
                        let _=e.db.connection().and_then(|c|Ok(c.execute("UPDATE worker_connections SET state='interrupted',launcher_pid=NULL,finished_at=?2,reason='supervisor task failed; verify process stopped' WHERE id=?1",params![id,crate::db::now()])?));
                    }
                }
            }
        }
        if e.is_stopping() {
            let _=e.db.connection().and_then(|c|{
                c.execute("UPDATE worker_connections SET state='stopped',finished_at=?1,actor='shutdown',reason='server shutdown before provisioning' WHERE state='queued'",[crate::db::now()])?;
                c.execute("UPDATE worker_connections SET state='stopping',actor=COALESCE(actor,'shutdown'),reason=COALESCE(reason,'server shutdown') WHERE state IN ('starting','connected')",[])?;Ok(())
            });
            if tasks.is_empty() {
                return;
            }
        } else if tasks.len() < 8 {
            match claim(&e) {
                Ok(Some((id, r))) => {
                    let engine = e.clone();
                    let task_id = id.clone();
                    let handle = tasks.spawn(async move {
                        let result = supervise(engine, &task_id, r).await;
                        (task_id, result)
                    });
                    ids.insert(handle.id(), id);
                }
                Ok(None) => {}
                Err(error) => eprintln!("SSH provisioning dispatch failed: {error}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
