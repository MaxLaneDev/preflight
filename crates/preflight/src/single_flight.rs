//! Shared execution owns native processes; subscribers own their independent outcomes.
use crate::{
    engine::Engine,
    executor::{self, Execution},
    model::*,
    pipeline::StageDefinition,
    profiles::Submission,
};
use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub struct Pool(Mutex<HashMap<String, Arc<Flight>>>);
struct Flight {
    run: String,
    cancel: CancellationToken,
    result: watch::Sender<Option<Outcome>>,
    subscribers: Mutex<usize>,
}
#[derive(Clone)]
struct Outcome {
    state: RunState,
    exit: Option<i32>,
    reason: Option<String>,
}
pub(crate) struct Prepared {
    pub container: Option<Arc<crate::docker::Context>>,
    pub memory: u64,
    pub verified_executable: Option<crate::model::Artifact>,
    pub owner_run: String,
    pub stage: StageDefinition,
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub command: CommandSpec,
    pub env: BTreeMap<String, String>,
    pub key: String,
}
struct Subscriber {
    engine: Arc<Engine>,
    flight: Arc<Flight>,
    key: String,
    run: String,
    stage: String,
    finished: bool,
}
impl Drop for Subscriber {
    fn drop(&mut self) {
        if !self.finished {
            let _=self.engine.db.connection().and_then(|c|Ok(c.execute("UPDATE shared_subscribers SET state='abandoned' WHERE run_id=?1 AND stage_id=?2 AND state='waiting'",params![self.run,self.stage])?));
        }
        let mut pool = self.engine.flights.0.lock().expect("flight pool");
        let mut count = self.flight.subscribers.lock().expect("flight members");
        *count -= 1;
        if *count == 0 {
            self.flight.cancel.cancel();
            if pool
                .get(&self.key)
                .is_some_and(|v| Arc::ptr_eq(v, &self.flight))
            {
                pool.remove(&self.key);
            }
        }
    }
}
impl crate::db::Db {
    pub(crate) fn is_shared_execution(&self, id: &str) -> Result<bool> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM shared_executions WHERE run_id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
}
fn subscribe(e: &Arc<Engine>, p: Prepared) -> Result<Subscriber> {
    let project = e.db.run(&p.owner_run)?.project_id;
    let policy = p.stage.cache.as_ref().context("missing sharing policy")?;
    let key = serde_json::to_string(&(&project, &policy.pipeline_id, &p.key))?;
    let mut pool = e
        .flights
        .0
        .lock()
        .map_err(|_| anyhow::anyhow!("flight pool poisoned"))?;
    let flight = if let Some(flight) = pool.get(&key).filter(|f| !f.cancel.is_cancelled()) {
        e.db.connection()?.execute(
            "INSERT INTO shared_subscribers VALUES (?1,?2,?3,'waiting')",
            params![p.owner_run, p.stage.id, flight.run],
        )?;
        *flight.subscribers.lock().expect("flight members") += 1;
        flight.clone()
    } else {
        let run = uuid::Uuid::now_v7().to_string();
        let root = e.config.data_dir.join("workspaces").join(&run);
        std::fs::create_dir_all(&root)?;
        let mut stage = p.stage.clone();
        stage.id = "native".into();
        stage.needs.clear();
        stage.inputs.clear();
        stage.cache = None;
        stage.command = p.command.clone();
        stage.env = p.env.clone();
        let submission = Submission::new(
            None,
            json!({"operation":"single_flight","owner_run":p.owner_run,"owner_stage":p.stage.id,"pipeline_id":policy.pipeline_id,"cache_key":p.key}),
        )?;
        e.db.insert_submission(
            &run,
            &project,
            &root,
            None,
            &[stage],
            Some(&submission),
            None,
        )?;
        let (result, _) = watch::channel(None);
        let flight = Arc::new(Flight {
            run: run.clone(),
            cancel: CancellationToken::new(),
            result,
            subscribers: Mutex::new(1),
        });
        pool.insert(key.clone(), flight.clone());
        e.active
            .lock()
            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?
            .insert(run, flight.cancel.clone());
        let engine = e.clone();
        let worker = flight.clone();
        let owner = p.owner_run.clone();
        // The execution and first subscription are durable before any command starts.
        let task = Prepared {
            memory: p.memory,
            container: p.container.clone(),
            verified_executable: p.verified_executable.clone(),
            owner_run: owner,
            stage: p.stage.clone(),
            workspace: p.workspace.clone(),
            cwd: p.cwd.clone(),
            command: p.command.clone(),
            env: p.env.clone(),
            key: p.key.clone(),
        };
        tokio::spawn(async move {
            let work_engine = engine.clone();
            let work_flight = worker.clone();
            let result =
                match tokio::spawn(
                    async move { work(&work_engine, &work_flight, &task, &root).await },
                )
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!("shared worker failed: {error}")),
                };
            let mut outcome = match result {
                Ok(v) => v,
                Err(error) => Outcome {
                    state: if worker.cancel.is_cancelled() {
                        RunState::Cancelled
                    } else {
                        RunState::InfrastructureError
                    },
                    exit: None,
                    reason: Some(format!("{error:#}").chars().take(1024).collect()),
                },
            };
            if let Err(error) = engine.db.transition(
                &worker.run,
                outcome.state,
                outcome.exit,
                None,
                outcome.reason.as_deref(),
            ) {
                eprintln!("shared execution completion: {error:#}");
                outcome = Outcome {
                    state: RunState::InfrastructureError,
                    exit: None,
                    reason: Some("shared completion could not be persisted".into()),
                };
            }
            worker.result.send_replace(Some(outcome));
            let active = engine.active.clone();
            let run = worker.run.clone();
            drop(engine);
            active.lock().expect("worker mutex").remove(&run);
        });
        flight
    };
    let subscriber = Subscriber {
        engine: e.clone(),
        flight,
        key,
        run: p.owner_run,
        stage: p.stage.id,
        finished: false,
    };
    drop(pool);
    Ok(subscriber)
}
async fn work(e: &Arc<Engine>, f: &Flight, p: &Prepared, root: &Path) -> Result<Outcome> {
    let mut resources = crate::leases::stage_resources(&p.stage)?;
    resources.insert(format!("run:{}", p.owner_run));
    let _operation =
        e.db.begin_operation(&f.run, "shared_execution", resources)?;
    let Some(_permit) = e
        .acquire_resource(p.stage.required_slots() as usize, p.memory, &f.cancel)
        .await?
    else {
        return Ok(Outcome {
            state: RunState::Cancelled,
            exit: None,
            reason: None,
        });
    };
    e.db.transition(&f.run, RunState::Preparing, None, None, None)?;
    e.db.stage_transition(&f.run, "native", RunState::Preparing, None, None)?;
    e.db.transition(&f.run, RunState::Running, None, None, None)?;
    e.db.stage_transition(&f.run, "native", RunState::Running, None, None)?;
    let (state, exit) = executor::execute(
        &e.db,
        Execution {
            container: p.container.as_deref(),
            log_mode: crate::executor::LogMode::All,
            verified_executable: p.verified_executable.as_ref(),
            run_id: &f.run,
            stage_id: "native",
            workspace: &p.workspace,
            cwd: &p.cwd,
            command: &p.command,
            env: &p.env,
            stdin: None,
            capture_limit: 1024 * 1024,
        },
        &f.cancel,
    )
    .await?;
    e.db.stage_transition(&f.run, "native", RunState::Collecting, exit, None)?;
    let mut stage = p.stage.clone();
    stage.id = "native".into();
    let outputs = if state == RunState::Succeeded {
        crate::artifacts::publish(&f.run, root, &stage, &p.cwd)?
    } else {
        vec![]
    };
    e.db.finish_stage(&f.run, "native", state, exit, None, None, &outputs)?;
    Ok(Outcome {
        state,
        exit,
        reason: (state == RunState::TimedOut).then(|| "shared command deadline exceeded".into()),
    })
}
pub(crate) async fn execute(
    e: &Arc<Engine>,
    p: Prepared,
    root: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let run = p.owner_run.clone();
    let stage = p.stage.clone();
    let key = p.key.clone();
    let policy = stage.cache.as_ref().context("sharing policy")?;
    let mut subscriber = subscribe(e, p)?;
    let execution = subscriber.flight.run.clone();
    e.db.cache_record(&run,&stage.id,json!({"status":"sharing","key":key,"execution_run":execution,"pipeline_id":policy.pipeline_id}))?;
    let mut receiver = subscriber.flight.result.subscribe();
    let outcome = loop {
        if cancel.is_cancelled() {
            break Outcome {
                state: RunState::Cancelled,
                exit: None,
                reason: None,
            };
        }
        if let Some(v) = receiver.borrow().clone() {
            break v;
        }
        tokio::select! {biased;_=cancel.cancelled()=>break Outcome{state:RunState::Cancelled,exit:None,reason:None},value=receiver.changed()=>{value.context("shared result unavailable")?;}}
    };
    let outputs = if outcome.state == RunState::Succeeded {
        crate::cache::restore_outputs(e.db.artifacts(&execution)?, &run, root, &stage)?
    } else {
        vec![]
    };
    let _active = e
        .active
        .lock()
        .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
    let state = if cancel.is_cancelled() {
        RunState::Cancelled
    } else {
        outcome.state
    };
    e.db.stage_transition(&run, &stage.id, RunState::Collecting, outcome.exit, None)?;
    if state == RunState::Succeeded {
        e.db.cache_publish(&run, &stage.id, &policy.pipeline_id, &key)?;
    }
    update_decision(
        &e.db,
        &run,
        &stage.id,
        &execution,
        json!({"status":if state==RunState::Succeeded{"shared"}else{"shared_terminal"},"outcome":state,"key":key,"format":crate::cache::FORMAT,"pipeline_id":policy.pipeline_id,"execution_run":execution,"verified_outputs":if state==RunState::Succeeded{outputs.len()}else{0}}),
    )?;
    e.db.finish_stage(
        &run,
        &stage.id,
        state,
        outcome.exit,
        outcome.reason.as_deref(),
        None,
        if state == RunState::Succeeded {
            &outputs
        } else {
            &[]
        },
    )?;

    e.db.connection()?.execute(
        "UPDATE shared_subscribers SET state=?3 WHERE run_id=?1 AND stage_id=?2",
        params![
            run,
            stage.id,
            if state == RunState::Succeeded {
                "succeeded"
            } else if state == RunState::Cancelled {
                "cancelled"
            } else {
                "failed"
            }
        ],
    )?;
    subscriber.finished = true;
    drop(_active);
    drop(subscriber);
    Ok(())
}

fn update_decision(
    db: &crate::db::Db,
    run: &str,
    stage: &str,
    execution: &str,
    value: serde_json::Value,
) -> Result<()> {
    let changed=db.connection()?.execute("UPDATE stage_cache_decisions SET decision_json=?4 WHERE run_id=?1 AND stage_id=?2 AND json_extract(decision_json,'$.execution_run')=?3 AND json_extract(decision_json,'$.status')='sharing'",params![run,stage,execution,serde_json::to_string(&value)?])?;
    anyhow::ensure!(
        changed == 1,
        "shared decision no longer matches subscription"
    );
    Ok(())
}

pub fn inspect(e: &Engine, run: &str) -> Result<serde_json::Value> {
    e.db.run(run)?;
    let rows=e.db.connection()?.prepare("SELECT s.stage_id,s.execution_run,s.state,r.status,(SELECT COUNT(*) FROM shared_subscribers a WHERE a.execution_run=s.execution_run AND a.state='waiting') FROM shared_subscribers s JOIN runs r ON r.id=s.execution_run WHERE s.run_id=?1 ORDER BY s.stage_id LIMIT 32")?.query_map([run],|r|Ok(json!({"stage_id":r.get::<_,String>(0)?,"execution_run":r.get::<_,String>(1)?,"subscription":r.get::<_,String>(2)?,"execution_status":r.get::<_,String>(3)?,"waiting_subscribers":r.get::<_,u64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"run_id":run,"stages":rows}))
}
