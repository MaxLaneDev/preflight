#[allow(dead_code)]
mod common;
use common::*;
use preflight::{
    model::{Actor, CancelRun, RunState},
    pipeline::StartPipeline,
    worker_runtime, workers,
};
use serde_json::json;
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
async fn worker(base: &str, state: &Path, token: Option<&str>) -> std::process::Output {
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
    tokio::time::timeout(Duration::from_secs(35), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
async fn queued(f: &Fixture, id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !f.engine.db.worker_jobs(id).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .is_empty()
            {
                break;
            }
            let r = f.engine.db.run(id).unwrap();
            assert!(!r.status.terminal(), "{r:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 image and local Docker daemon"]
async fn remote_container_setup_reports_failure_timeout_cancel_and_cleanup() {
    let f = Fixture::new();
    let image = std::env::var("PREFLIGHT_TEST_DOCKER_IMAGE").unwrap();
    std::fs::write(
        f.project.root.join("activate.sh"),
        format!(
            "{}\nworkspace-tool\ncp /bin/true .tools/bin/prepared-tool\n\
         test ! -S /var/run/docker.sock\n\
         if touch /preflight-job-must-not-write-root 2>/dev/null; then exit 10; fi\n\
         test ! -e /tmp/preflight-previous-job\n\
         printf private > /tmp/preflight-previous-job\n",
            include_str!("support/workspace-tools.sh")
        ),
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
            name: "container-worker".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let state = f.temp.path().join("worker");
    std::fs::create_dir(&state).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    worker_runtime::save(
        &state.canonicalize().unwrap(),
        worker_runtime::Policy {
            docker_command: "/usr/local/bin/docker".into(),
            max_memory_mib: 512,
        },
    )
    .unwrap();
    let output = worker(&base, &state, issued["enrollment_token"].as_str()).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed = f
        .engine
        .db
        .workers(&workers::List {
            project_id: f.project.id.clone(),
            after: None,
        })
        .unwrap();
    let w = &listed["workers"][0];
    assert_eq!(w["container_jobs"], true);
    let selector =
        json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"worker_id":w["worker_id"]});
    for (name, script, timeout, expected) in [
        (
            "success",
            "prepared-tool; test -f /.dockerenv; printf remote-output > result.txt; printf '<testsuite><testcase name=\"remote container\"/></testsuite>' > report.xml",
            10,
            RunState::Succeeded,
        ),
        ("failed", "exit 7", 10, RunState::Failed),
        ("timeout", "sleep 30", 1, RunState::TimedOut),
        ("cancel", "sleep 30", 30, RunState::Cancelled),
    ] {
        let mut stage = json!({"id":name,"worker":selector,"executor":"docker","container":{"image":image,"platform":"linux/arm64"},"setup":[{"program":".","args":["./activate.sh"]}],"command":{"program":"sh","args":["-c",script],"timeout_seconds":timeout}});
        if name == "success" {
            stage["kind"] = json!("test");
            stage["junit"] = json!(["report.xml"]);
            stage["outputs"] = json!({"result":{"path":"result.txt","type":"file"}});
        }
        let run=f.engine.start_pipeline(serde_json::from_value::<StartPipeline>(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[stage]}}})).unwrap()).unwrap();
        queued(&f, &run.id).await;
        let work = worker(&base, &state, None);
        let cancel = async {
            if name == "cancel" {
                tokio::time::sleep(Duration::from_secs(2)).await;
                f.engine
                    .cancel(CancelRun {
                        run_id: run.id.clone(),
                        actor: Actor::User,
                        reason: "owned container cancellation".into(),
                    })
                    .unwrap();
            }
        };
        let (output, ()) = tokio::join!(work, cancel);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ended = finished(&f.engine, &run.id).await;
        assert_eq!(ended.status, expected, "{ended:?}");
        let jobs = f.engine.db.worker_jobs(&run.id).unwrap();
        let id = jobs["jobs"][0]["job_id"].as_str().unwrap();
        let db =
            rusqlite::Connection::open(state.join("jobs").join(format!("{id}.sqlite3"))).unwrap();
        let (cleaned, path): (bool, String) = db
            .query_row(
                "SELECT state='cleaned',workspace FROM container_jobs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(cleaned);
        assert!(!Path::new(&path).exists());
        assert!(preflight::worker_client::journal_status(&state).unwrap()["pending"].is_null());
        if name == "success" {
            let evidence = preflight::stage_keys::explain(
                &f.engine.db,
                &preflight::stage_keys::Query {
                    run_id: run.id.clone(),
                    stage_id: name.into(),
                    baseline_id: None,
                },
            )
            .unwrap();
            let receipt = &evidence["remote_execution"]["runtime_receipt"];
            assert_eq!(
                evidence["evidence"]["format"],
                "preflight-stage-evidence-v2"
            );
            assert_eq!(receipt["container"]["image_id"], image);
            assert_eq!(receipt["container"]["platform"], "linux/arm64");
            assert_eq!(receipt["container"]["user"], receipt["user"]);
            assert!(evidence["evidence"]["components"]["container_runtime"].is_null());
            assert_eq!(ended.stages[0].tests.as_ref().unwrap().counts.passed, 1);
            assert_eq!(
                std::fs::read(&f.engine.db.artifacts(&run.id).unwrap()[0].path).unwrap(),
                b"remote-output"
            );
        }
    }
    stop.cancel();
    server.await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached ARM64 Docker image; kills only its owned worker"]
async fn sigkill_preserves_container_and_workspace_until_verified_journal_recovery() {
    use preflight::operation_resolution::Resolve;
    use std::{
        path::PathBuf,
        process::{Child, Command, Stdio},
    };
    struct CrashFixture {
        root: PathBuf,
        state: PathBuf,
        job: Option<String>,
        child: Option<Child>,
        completed: bool,
    }
    impl Drop for CrashFixture {
        fn drop(&mut self) {
            if let Some(child) = &mut self.child {
                let _ = child.kill();
                let _ = child.wait();
            }
            let cleaned = self.completed
                || self.job.as_ref().is_none_or(|job| {
                    Command::new(env!("CARGO_BIN_EXE_preflight"))
                        .args([
                            "worker",
                            "containers-cleanup",
                            self.state.to_str().unwrap(),
                            job,
                        ])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
                });
            if self.completed && cleaned {
                let _ = std::fs::remove_dir_all(&self.root);
            } else {
                eprintln!("retained owned crash fixture {}", self.root.display());
            }
        }
    }
    let Fixture {
        temp,
        engine,
        project,
    } = Fixture::new();
    let root = temp.keep();
    let state = root.join("worker");
    let mut owned = CrashFixture {
        root,
        state: state.clone(),
        job: None,
        child: None,
        completed: false,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let signal = stop.clone();
    let app = workers::router(engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap();
    });
    std::fs::create_dir(&state).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    worker_runtime::save(
        &state.canonicalize().unwrap(),
        worker_runtime::Policy {
            docker_command: "/usr/local/bin/docker".into(),
            max_memory_mib: 512,
        },
    )
    .unwrap();
    let enrollment = engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: project.id.clone(),
            name: "sigkill-worker".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    assert!(
        worker(&base, &state, enrollment["enrollment_token"].as_str())
            .await
            .status
            .success()
    );
    let run=engine.start_pipeline(serde_json::from_value(json!({"project_id":project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"crash","worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"executor":"docker","container":{"image":std::env::var("PREFLIGHT_TEST_DOCKER_IMAGE").unwrap(),"platform":"linux/arm64"},"command":{"program":"sh","args":["-c","printf 'owned-sigkill-ready\\n'; sleep 60; printf should-not-publish > result.txt"],"timeout_seconds":90},"outputs":{"result":{"path":"result.txt","type":"file"}}}]}}})).unwrap()).unwrap();
    let job = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = engine.db.worker_jobs(&run.id).unwrap();
            if let Some(job) = page["jobs"].as_array().unwrap().first() {
                break job["job_id"].as_str().unwrap().to_owned();
            }
            assert!(!engine.db.run(&run.id).unwrap().status.terminal());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    owned.job = Some(job.clone());
    owned.child = Some(
        Command::new(env!("CARGO_BIN_EXE_preflight"))
            .args([
                "worker",
                "connect",
                &base,
                state.to_str().unwrap(),
                "--job-once",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if output(&engine, &run.id, "stdout").contains("owned-sigkill-ready") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("container reached actual stdout before SIGKILL");
    let path = state.join("jobs").join(format!("{job}.sqlite3"));
    let local = rusqlite::Connection::open(&path).unwrap();
    let (cid, workspace): (String, String) = local
        .query_row(
            "SELECT container_id,workspace FROM container_jobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(local);
    let child = owned.child.as_mut().unwrap();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    owned.child = None;
    let inspect = Command::new("/usr/local/bin/docker")
        .args([
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            &cid,
        ])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    assert_eq!(String::from_utf8_lossy(&inspect.stdout).trim(), "true");
    assert!(Path::new(&workspace).exists());
    assert!(
        !worker(&base, &state, None).await.status.success(),
        "a running journal cannot restart a command"
    );
    tokio::time::timeout(Duration::from_secs(22), async {
        loop {
            if engine.db.worker_jobs(&run.id).unwrap()["jobs"][0]["state"] == "lost" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("actual lease expires after worker SIGKILL");
    assert_eq!(
        finished(&engine, &run.id).await.status,
        RunState::InfrastructureError
    );
    assert!(engine.db.artifacts(&run.id).unwrap().is_empty());
    let cleaned = Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args([
            "worker",
            "containers-cleanup",
            state.to_str().unwrap(),
            &job,
        ])
        .output()
        .unwrap();
    assert!(
        cleaned.status.success(),
        "{}",
        String::from_utf8_lossy(&cleaned.stderr)
    );
    assert!(!Path::new(&workspace).exists());
    assert!(
        !Command::new("/usr/local/bin/docker")
            .args(["container", "inspect", &cid])
            .output()
            .unwrap()
            .status
            .success()
    );
    let operations = engine.db.operations(&run.id).unwrap();
    let lease = operations["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "worker_job")
        .unwrap();
    assert_eq!(lease["state"], "abandoned");
    engine
        .db
        .resolve_operation(&Resolve {
            project_id: project.id.clone(),
            lease_id: lease["id"].as_str().unwrap().into(),
            request_id: "sigkill-cleaned".into(),
            actor: "test".into(),
            reason: "owned worker reaped and exact owned container removed".into(),
            process_stopped: true,
        })
        .unwrap();
    let recovered = Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args([
            "worker",
            "recover",
            &base,
            state.to_str().unwrap(),
            &job,
            "--process-stopped",
        ])
        .output()
        .unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(preflight::worker_client::journal_status(&state).unwrap()["pending"].is_null());
    assert_eq!(
        preflight::db::Db::new(&path)
            .unwrap()
            .run(&job)
            .unwrap()
            .status,
        RunState::InfrastructureError,
        "recovered local execution must not remain running"
    );
    assert_eq!(
        output(&engine, &run.id, "stdout")
            .matches("owned-sigkill-ready")
            .count(),
        1
    );
    assert!(!output(&engine, &run.id, "stdout").contains("should-not-publish"));
    stop.cancel();
    server.await.unwrap();
    engine.shutdown().await.unwrap();
    owned.completed = true;
}
