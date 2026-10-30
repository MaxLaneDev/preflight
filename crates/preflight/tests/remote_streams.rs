#[allow(dead_code)]
mod common;
use common::*;
use preflight::{model::RunState, pipeline::StartPipeline, workers};
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
async fn worker(base: &str, state: &Path, token: Option<&str>) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args([
            "worker",
            "connect",
            base,
            state.to_str().unwrap(),
            if token.is_some() {
                "--once"
            } else {
                "--job-once"
            },
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    if let Some(token) = token {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{token}\n").as_bytes())
            .await
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_reads_remote_output_before_completion_and_preserves_more_than_snapshot() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let end = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(end.cancelled_owned())
            .await
            .unwrap();
    });
    let admin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_base = format!("http://{}/mcp", admin.local_addr().unwrap());
    let end = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let api = tokio::spawn(async move {
        axum::serve(admin, app)
            .with_graceful_shutdown(end.cancelled_owned())
            .await
            .unwrap();
    });
    let mcp = ().serve(StreamableHttpClientTransport::from_uri(admin_base)).await.unwrap();
    let enrollment = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "stream-worker".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let state = f.temp.path().join("worker");
    worker(&base, &state, enrollment["enrollment_token"].as_str()).await;
    let identity: Value =
        serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap()).unwrap();
    let credential = identity["credential"].as_str().unwrap();
    let definition = json!({"version":1,"pipeline":{"stages":[{"id":"remote","worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"env":{"PART_A":&credential[..20],"PART_B":&credential[20..]},"command":{"program":"sh","args":["-c","printf 'remote-first\\n'; sleep 3; printf '%s' \"$PART_A\"; sleep 0.3; printf '%s\\n' \"$PART_B\"; i=0; while [ $i -lt 2048 ]; do printf 'remote-line-%04d-with-details\\n' \"$i\"; i=$((i+1)); done; printf 'remote-last\\n'"],"timeout_seconds":15}}]}});
    let run = f
        .engine
        .start_pipeline(
            serde_json::from_value::<StartPipeline>(
                json!({"project_id":f.project.id,"definition":definition}),
            )
            .unwrap(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !f.engine.db.worker_jobs(&run.id).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .is_empty()
            {
                break;
            }
            assert!(!f.engine.db.run(&run.id).unwrap().status.terminal());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let execution = worker(&base, &state, None);
    let observation = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = mcp
                    .call_tool(
                        CallToolRequestParams::new("logs_read").with_arguments(
                            json!({"run_id":run.id,"stage":"remote","after":0})
                                .as_object()
                                .unwrap()
                                .clone(),
                        ),
                    )
                    .await
                    .unwrap();
                assert_ne!(response.is_error, Some(true));
                let page: Value =
                    serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
                if page["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["text"].as_str().unwrap().contains("remote-first"))
                {
                    assert!(!f.engine.db.run(&run.id).unwrap().status.terminal());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .expect("MCP receives worker output while command is still running");
    };
    tokio::join!(execution, observation);
    let ended = finished(&f.engine, &run.id).await;
    assert_eq!(ended.status, RunState::Succeeded);
    let mut after = 0;
    let mut text = String::new();
    loop {
        let page = f
            .engine
            .db
            .stage_logs(&run.id, Some("remote"), after)
            .unwrap();
        for event in page.events {
            if event.stream == "stdout" {
                text.push_str(&event.text);
            }
        }
        if let Some(next) = page.next_cursor {
            after = next
        } else {
            break;
        }
    }
    assert!(text.len() > 4096);
    assert_eq!(text.matches("remote-first").count(), 1);
    assert_eq!(text.matches("remote-last").count(), 1);
    assert_eq!(text.matches("remote-line-").count(), 2048);
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains(credential), "worker credential leaked");
    assert!(!text.contains("output truncated"));
    let jobs = f.engine.db.worker_jobs(&run.id).unwrap();
    assert!(jobs["jobs"][0]["log_cursor"].as_i64().unwrap() > 0);
    assert_eq!(jobs["jobs"][0]["log_truncated"], false);
    let hits = preflight::evidence::search(
        &f.engine,
        &serde_json::from_value(
            json!({"project_id":f.project.id,"run_id":run.id,"query":"remote-last"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!hits["matches"].as_array().unwrap().is_empty());
    mcp.cancel().await.unwrap();
    stop.cancel();
    api.await.unwrap();
    server.await.unwrap();
}
