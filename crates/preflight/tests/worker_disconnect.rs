#[allow(dead_code)]
mod common;
use common::*;
use preflight::{model::RunState, worker_jobs, worker_transfers, workers};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

fn journal(root: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(root.join("identity.json")).unwrap()).unwrap()
}
fn worker(base: &str, root: &Path, mode: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"));
    command
        .args(["worker", "connect", base, root.to_str().unwrap(), mode])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn disconnected_worker_stops_command_and_never_reexecutes_lost_claim() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let enrollment = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "disconnect-test".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let state = f.temp.path().join("worker");
    let mut child = worker(&base, &state, "--once").spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", enrollment["enrollment_token"].as_str().unwrap()).as_bytes())
        .await
        .unwrap();
    let connected = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        connected.status.success(),
        "{}",
        String::from_utf8_lossy(&connected.stderr)
    );
    let identity = journal(&state);
    let definition = serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"build","command":f.request(&[],5).command}]}})).unwrap();
    let source = f
        .engine
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: f.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &source.id).await.status,
        RunState::Succeeded
    );
    let transfer = worker_transfers::prepare(
        f.engine.clone(),
        worker_transfers::Prepare {
            run_id: source.id,
            worker_id: identity["worker_id"].as_str().unwrap().into(),
            generation: 1,
            ttl_seconds: 60,
        },
    )
    .await
    .unwrap();
    let target = uuid::Uuid::now_v7().to_string();
    let request = f.request(&["sleep"], 60);
    f.engine
        .db
        .insert_run(&target, &request, &f.project.root)
        .unwrap();
    let job = f
        .engine
        .db
        .worker_job_enqueue(
            &target,
            "native",
            transfer["transfer_id"].as_str().unwrap(),
            &worker_jobs::Spec {
                container: None,
                command: request.command,
                cwd: ".".into(),
                env: Default::default(),
                returns: Default::default(),
            },
        )
        .unwrap();
    let id = job["job_id"].as_str().unwrap();
    let mut child = worker(&base, &state, "--job-once").spawn().unwrap();
    drop(child.stdin.take());
    let db_path = state.join("jobs").join(format!("{id}.sqlite3"));
    // Wait for command stdout, not just the pre-execution journal write.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if db_path.exists() {
                let db = preflight::db::Db::new(&db_path).unwrap();
                if db.logs(id, 0).is_ok_and(|p| {
                    p.events
                        .iter()
                        .any(|e| e.text.contains("Preflight helper: stdout"))
                }) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native command produced output");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("connection loss must stop native execution well before its 60 second timeout")
        .unwrap();
    assert!(!result.status.success());
    let saved = journal(&state);
    assert_eq!(saved["pending"]["phase"], "finished");
    assert_eq!(
        saved["pending"]["outcome"]["status"],
        "infrastructure_error"
    );
    let db = preflight::db::Db::new(&db_path).unwrap();
    assert_eq!(db.run(id).unwrap().status, RunState::Cancelled);
    let before = db.logs(id, 0).unwrap().events.len();
    // Let the actual server lease expire while the gateway is unavailable.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if f.engine.db.worker_jobs(&target).unwrap()["jobs"][0]["state"] == "lost" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("lost claim fenced by lease deadline");
    assert!(
        f.engine.db.operations(&target).unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "worker_job" && o["state"] == "abandoned")
    );
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mut child = worker(&base, &state, "--job-once").spawn().unwrap();
    drop(child.stdin.take());
    let retry = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !retry.status.success(),
        "late result must not overwrite lost ownership"
    );
    assert_eq!(journal(&state)["pending"], saved["pending"]);
    assert_eq!(db.logs(id, 0).unwrap().events.len(), before);
    assert_eq!(db.run(id).unwrap().status, RunState::Cancelled);
    assert_eq!(
        f.engine.db.worker_jobs(&target).unwrap()["jobs"][0]["state"],
        "lost"
    );
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
