#[allow(dead_code)]
mod common;
use common::*;
use preflight::{model::RunState, pipeline::StartPipeline, workers};
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

async fn run_worker(base: &str, state: &Path, mode: &str, token: Option<&str>) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "connect", base, state.to_str().unwrap(), mode])
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
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
async fn queued(f: &Fixture, run: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.engine.db.worker_jobs(run).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|j| j["state"] == "queued")
            {
                break;
            }
            let state = f.engine.db.run(run).unwrap();
            assert!(
                !state.status.terminal(),
                "run ended before dispatch: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pipeline dispatched a worker job");
}

#[tokio::test]
async fn remote_setup_tools_are_job_local_and_removed_before_worker_reuse() {
    let f = Fixture::new();
    std::fs::write(
        f.project.root.join("prepare.sh"),
        include_str!("support/workspace-tools.sh"),
    )
    .unwrap();
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
    let issued = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "workspace-tools".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let state = f.temp.path().join("worker");
    run_worker(&base, &state, "--once", issued["enrollment_token"].as_str()).await;
    let definition: preflight::pipeline::PipelineFile =
        serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{
            "id":"tools", "worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},
            "setup":[{"program":".","args":["./prepare.sh"]}],
            "command":{"program":"workspace-tool","timeout_seconds":5},
            "outputs":{"installation":{"path":"installation.txt","type":"file"}}
        }]}}))
        .unwrap();
    let mut previous = None;
    for _ in 0..2 {
        let run = f
            .engine
            .start_pipeline(StartPipeline {
                project_id: f.project.id.clone(),
                definition: Some(definition.clone()),
            })
            .unwrap();
        queued(&f, &run.id).await;
        run_worker(&base, &state, "--job-once", None).await;
        let ended = finished(&f.engine, &run.id).await;
        assert_eq!(ended.status, RunState::Succeeded, "{ended:?}");
        let jobs = f.engine.db.worker_jobs(&run.id).unwrap();
        let id = jobs["jobs"][0]["job_id"].as_str().unwrap();
        let local =
            preflight::db::Db::new(&state.join("jobs").join(format!("{id}.sqlite3"))).unwrap();
        let workspace = local.run(id).unwrap().workspace;
        assert!(
            !workspace.exists(),
            "job tools and private HOME must be cleaned"
        );
        assert_ne!(previous.as_ref(), Some(&workspace));
        previous = Some(workspace.clone());
        let artifacts = f.engine.db.artifacts(&run.id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&artifacts[0].path).unwrap().trim(),
            workspace.to_str().unwrap()
        );
        assert!(preflight::worker_client::journal_status(&state).unwrap()["pending"].is_null());
    }
    for name in [
        ".tools",
        ".home",
        ".tmp",
        ".cache",
        ".cargo",
        ".rustup",
        "installation.txt",
    ] {
        assert!(
            !f.project.root.join(name).exists(),
            "source project must stay unchanged"
        );
        assert!(
            !state.join(name).exists(),
            "installation must not escape the job workspace"
        );
    }
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn remote_stages_share_artifacts_junit_and_cancellation_with_engine() {
    let f = Fixture::new();
    let helper = f.project.root.join("driver");
    let compiled = std::process::Command::new("rustc")
        .args([
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/worker.rs"),
            "-o",
        ])
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    std::fs::create_dir(f.project.root.join("suite")).unwrap();
    std::fs::write(
        f.project.root.join("prepare.sh"),
        "export PREFLIGHT_SETUP_CHECK=remote-ready\n",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_base = format!("http://{}", admin_listener.local_addr().unwrap());
    let admin_stop = CancellationToken::new();
    let admin_shutdown = admin_stop.clone();
    let admin_app = preflight::api::router(f.engine.clone());
    let admin = tokio::spawn(async move {
        axum::serve(admin_listener, admin_app)
            .with_graceful_shutdown(admin_shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mcp = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{admin_base}/mcp"
        )))
        .await
        .unwrap();
    let issued = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "pipeline-worker".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let state = f.temp.path().join("worker");
    run_worker(&base, &state, "--once", issued["enrollment_token"].as_str()).await;
    let worker = json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH});
    let definition: preflight::pipeline::PipelineFile = serde_json::from_value(json!({"version":1,"pipeline":{"stages":[
        {"id":"produce","worker":worker,"setup":[{"program":".","args":["./prepare.sh"]},{"program":"sh","args":["-c","test \"$PREFLIGHT_SETUP_CHECK\" = remote-ready"]}],"command":{"program":"./driver","args":["produce"],"timeout_seconds":5},"outputs":{"payload":{"path":"payload.txt","type":"file"},"tool":{"path":"worker.exe","type":"executable"}}},
        {"id":"consume","needs":["produce"],"command":{"program":"${artifact.produce.tool}","args":["consume","${artifact.produce.payload}"],"timeout_seconds":5}},
        {"id":"tests","worker":worker,"needs":["produce"],"kind":"test","cwd":"suite","command":{"program":"${artifact.produce.tool}","args":["junit","report.xml","<testsuite><testcase name='remote case' classname='worker'/></testsuite>"],"timeout_seconds":5},"junit":["report.xml"]}
    ]}})).unwrap();
    let response = mcp
        .call_tool(
            CallToolRequestParams::new("pipeline_start").with_arguments(
                json!({"project_id":f.project.id,"definition":definition})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let run = f
        .engine
        .db
        .run(started["run_id"].as_str().unwrap())
        .unwrap();
    for _ in 0..2 {
        queued(&f, &run.id).await;
        run_worker(&base, &state, "--job-once", None).await;
    }
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(
        done.status,
        RunState::Succeeded,
        "{done:?}; stderr={}",
        output(&f.engine, &run.id, "stderr")
    );
    assert!(output(&f.engine, &run.id, "stdout").contains("consumed artifact"));
    let artifacts = f.engine.db.artifacts(&run.id).unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&artifacts.iter().find(|a| a.name == "payload").unwrap().path)
            .unwrap(),
        "artifact payload"
    );
    assert!(
        done.stages
            .iter()
            .find(|s| s.definition.id == "tests")
            .unwrap()
            .tests
            .is_some()
    );
    let jobs = f.engine.db.worker_jobs(&run.id).unwrap();
    for stage in ["produce", "tests"] {
        let response = mcp
            .call_tool(
                CallToolRequestParams::new("runs_stage_key").with_arguments(
                    json!({"run_id":run.id,"stage_id":stage})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        let evidence: Value =
            serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
        assert_eq!(
            evidence["evidence"]["format"],
            "preflight-stage-evidence-v2"
        );
        assert_eq!(evidence["evidence"]["reusable"], false);
        assert!(evidence["evidence"]["components"]["inherited_environment"].is_null());
        assert!(evidence["evidence"]["components"]["remote_runtime"].is_string());
        let receipt = &evidence["remote_execution"]["runtime_receipt"];
        assert_eq!(receipt["os"], std::env::consts::OS);
        assert_eq!(receipt["arch"], std::env::consts::ARCH);
        assert_eq!(receipt["format"], preflight::worker_receipts::FORMAT);
        assert!(receipt["launcher_sha256"].is_string());
        let http: Value = reqwest::get(format!(
            "{admin_base}/api/v1/stage-key?run_id={}&stage_id={stage}",
            run.id
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(http, evidence);
    }
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 2);
    assert!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["state"] == "succeeded" && j["outputs_tree_sha256"].is_string())
    );
    let reports: Value = serde_json::to_value(
        done.stages
            .iter()
            .find(|s| s.definition.id == "tests")
            .unwrap()
            .tests
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reports["counts"]["passed"], 1);
    assert!(
        std::fs::read_to_string(done.workspace.join("stages/tests/suite/report.xml"))
            .unwrap()
            .contains("remote case")
    );
    // Exercise the ordinary engine cancellation entry point, not a direct ledger write.
    let definition = serde_json::from_value(json!({"version":1,"pipeline":{"stages":[
        {"id":"slow","worker":worker,"command":{"program":"./driver","args":["sleep","60000"],"timeout_seconds":60}}
    ]}})).unwrap();
    let run = f
        .engine
        .start_pipeline(StartPipeline {
            project_id: f.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    queued(&f, &run.id).await;
    let execution = run_worker(&base, &state, "--job-once", None);
    let cancellation = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let journal: Value =
                    serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap())
                        .unwrap();
                if journal["pending"]["phase"] == "running" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        f.engine
            .cancel(
                serde_json::from_value(
                    json!({"run_id":run.id,"actor":"user","reason":"cancel remote pipeline"}),
                )
                .unwrap(),
            )
            .unwrap();
    };
    tokio::join!(execution, cancellation);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.worker_jobs(&run.id).unwrap()["jobs"][0]["state"],
        "cancelled"
    );
    for (stage, expected, dispatch) in [
        (
            json!({"id":"missing_worker","worker":{"os":std::env::consts::OS,"arch":"unavailable_arch"},"command":{"program":"./driver","args":["fail"],"timeout_seconds":5}}),
            RunState::InfrastructureError,
            false,
        ),
        (
            json!({"id":"missing_output","worker":worker,"command":{"program":"./driver","args":["write","actual.txt"],"timeout_seconds":5},"outputs":{"required":{"path":"missing.txt","type":"file"}}}),
            RunState::InfrastructureError,
            true,
        ),
        (
            json!({"id":"failed_report","worker":worker,"kind":"test","command":{"program":"./driver","args":["junit","failed.xml","<testsuite><testcase name='remote failure'><failure message='expected failure'/></testcase></testsuite>"],"timeout_seconds":5},"junit":["failed.xml"]}),
            RunState::Failed,
            true,
        ),
    ] {
        let definition =
            serde_json::from_value(json!({"version":1,"pipeline":{"stages":[stage]}})).unwrap();
        let run = f
            .engine
            .start_pipeline(StartPipeline {
                project_id: f.project.id.clone(),
                definition: Some(definition),
            })
            .unwrap();
        if dispatch {
            queued(&f, &run.id).await;
            run_worker(&base, &state, "--job-once", None).await;
        }
        let done = finished(&f.engine, &run.id).await;
        assert_eq!(done.status, expected, "{done:?}");
        assert!(f.engine.db.artifacts(&run.id).unwrap().is_empty());
        if !dispatch {
            assert!(
                f.engine.db.worker_jobs(&run.id).unwrap()["jobs"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        if expected == RunState::Failed {
            assert_eq!(done.stages[0].tests.as_ref().unwrap().counts.failed, 1);
        }
    }
    mcp.cancel().await.unwrap();
    admin_stop.cancel();
    admin.await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
