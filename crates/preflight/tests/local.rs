mod common;
use common::*;
use preflight::{engine::Engine, model::*};

#[tokio::test]
async fn success_failure_isolation_and_safe_argv() {
    let f = Fixture::new();
    let run = f.start(&["write"], 5);
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded);
    assert_eq!(done.exit_code, Some(0));
    assert_eq!(done.stage.status, done.status);
    assert!(done.workspace.join("result.txt").exists());
    assert!(!f.project.root.join("result.txt").exists());
    assert!(output(&f.engine, &run.id, "stdout").contains("stdout"));
    assert!(output(&f.engine, &run.id, "stderr").contains("stderr"));
    let literal = "привет 🦀; $(touch SHOULD_NOT_EXIST)";
    let run = f.start(&["echo", literal], 5);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").contains(literal));
    assert!(!f.project.root.join("SHOULD_NOT_EXIST").exists());
    let fail = f.start(&["fail"], 5);
    let done = finished(&f.engine, &fail.id).await;
    assert_eq!((done.status, done.exit_code), (RunState::Failed, Some(7)));
    assert!(
        f.engine
            .db
            .transition(&done.id, RunState::Running, None, None, None)
            .is_err()
    );
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn timeout_cancel_and_shutdown_are_distinct() {
    let f = Fixture::new();
    let timed = f.start(&["sleep"], 1);
    let done = finished(&f.engine, &timed.id).await;
    assert_eq!(done.status, RunState::TimedOut);
    assert_eq!(done.cancel_actor.as_deref(), Some("timeout"));
    for actor in [Actor::User, Actor::Agent] {
        let run = f.start(&["sleep"], 10);
        f.engine
            .cancel(CancelRun {
                run_id: run.id.clone(),
                actor,
                reason: "requested cancellation".into(),
            })
            .unwrap();
        let done = finished(&f.engine, &run.id).await;
        assert_eq!(done.status, RunState::Cancelled);
        assert_eq!(done.cancel_actor.as_deref(), Some(actor.as_str()));
        assert_eq!(done.reason.as_deref(), Some("requested cancellation"));
    }
    let run = f.start(&["sleep"], 10);
    f.engine.shutdown().await.unwrap();
    let done = f.engine.db.run(&run.id).unwrap();
    assert_eq!(done.status, RunState::Cancelled);
    assert_eq!(done.cancel_actor.as_deref(), Some("shutdown"));
    let operations = f.engine.db.operations(&run.id).unwrap();
    assert!(
        operations["operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["state"] == "released")
    );
    assert!(f.engine.start(f.request(&[], 5)).is_err());
}
#[tokio::test]
async fn restart_preserves_results_logs_and_reconciles_interruption() {
    let f = Fixture::new();
    let run = f.start(&[], 5);
    finished(&f.engine, &run.id).await;
    f.engine.shutdown().await.unwrap();
    assert_eq!(
        std::sync::Arc::strong_count(&f.engine),
        1,
        "shutdown releases worker engine references"
    );
    assert!(
        Engine::open(Fixture::config(f.temp.path())).is_err(),
        "exclusive owner"
    );
    f.engine
        .db
        .insert_run(
            "interrupted",
            &f.request(&[], 5),
            &f.temp.path().join("never-used"),
        )
        .unwrap();
    let config = Fixture::config(f.temp.path());
    drop(f.engine);
    let reopened = Engine::open(config).unwrap();
    assert_eq!(
        reopened.db.run(&run.id).unwrap().status,
        RunState::Succeeded
    );
    assert!(output(&reopened, &run.id, "stderr").contains("stderr"));
    let interrupted = reopened.db.run("interrupted").unwrap();
    assert_eq!(interrupted.status, RunState::InfrastructureError);
    assert_eq!(interrupted.stage.status, interrupted.status);
    assert_eq!(interrupted.cancel_actor.as_deref(), Some("worker_lost"));
}
#[tokio::test]
async fn output_cap_drains_and_pages_logs() {
    let f = Fixture::new();
    let run = f.start(&["flood"], 10);
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let text = output(&f.engine, &run.id, "stdout");
    assert!(text.contains("output truncated after 1 MiB"));
    assert!(text.len() < 1024 * 1024 + 100);
    let page = f.engine.db.logs(&run.id, 0).unwrap();
    assert!(page.events.len() <= 16);
    assert!(page.next_cursor.is_some());
    let report = preflight::api::compact_report(&f.engine, &run.id).unwrap();
    assert!(report["tests"]["passed"].is_null());
    assert!(serde_json::to_vec(&report).unwrap().len() < 4096);
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn validates_paths_commands_and_missing_tools() {
    let f = Fixture::new();
    assert!(
        f.engine
            .register(RegisterProject {
                name: "escape".into(),
                path: "/".into()
            })
            .is_err()
    );
    assert!(
        f.engine
            .register(RegisterProject {
                name: "data".into(),
                path: f.engine.config.data_dir.to_string_lossy().into_owned()
            })
            .is_err()
    );
    assert!(f.engine.start(f.request(&[], 0)).is_err());
    let mut request = f.request(&[], 5);
    request.project_id = "missing".into();
    assert!(f.engine.start(request).is_err());
    let mut request = f.request(&[], 5);
    request.command.program = "preflight-uninstalled-command-12345".into();
    let run = f.engine.start(request).unwrap();
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::InfrastructureError
    );
    f.engine.shutdown().await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn symlinks_are_rejected() {
    let f = Fixture::new();
    std::os::unix::fs::symlink("/etc/passwd", f.project.root.join("escape")).unwrap();
    let run = f.start(&[], 5);
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::InfrastructureError);
    assert!(done.reason.unwrap().contains("symlink"));
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn actual_rust_project_builds_and_reports_compile_failure() {
    let f = Fixture::new();
    std::fs::create_dir(f.project.root.join("src")).unwrap();
    std::fs::write(
        f.project.root.join("Cargo.toml"),
        "[package]\nname = \"preflight-build-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nintentional-failure = []\n",
    ).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/process_helper.rs"
        ),
        f.project.root.join("src/main.rs"),
    )
    .unwrap();
    for (args, expected) in [
        (vec!["build", "--offline"], RunState::Succeeded),
        (
            vec!["build", "--offline", "--features", "intentional-failure"],
            RunState::Failed,
        ),
    ] {
        let run = f
            .engine
            .start(StartBuild {
                project_id: f.project.id.clone(),
                command: CommandSpec {
                    program: "cargo".into(),
                    args: args.into_iter().map(String::from).collect(),
                    timeout_seconds: 15,
                },
            })
            .unwrap();
        assert_eq!(
            finished(&f.engine, &run.id).await.status,
            expected,
            "{}",
            output(&f.engine, &run.id, "stderr")
        );
    }
    assert!(!f.project.root.join("target").exists());
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_does_not_start_a_command() {
    let f = Fixture::new();
    let first = f.start(&["sleep"], 10);
    let second = f.start(&["sleep"], 10);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if [first.id.as_str(), second.id.as_str()]
                .iter()
                .all(|id| f.engine.db.run(id).unwrap().status == RunState::Running)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let queued = f.start(&["write"], 5);
    f.engine
        .cancel(CancelRun {
            run_id: queued.id.clone(),
            actor: Actor::User,
            reason: "cancel while queued".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &queued.id).await.status,
        RunState::Cancelled
    );
    assert!(!queued.workspace.exists());
    f.engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_terminates_running_process_group() {
    let f = Fixture::new();
    let run = f
        .engine
        .start(StartBuild {
            project_id: f.project.id.clone(),
            command: CommandSpec {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "(sleep 1; echo child-survived > leaked.txt) & echo ready; wait".into(),
                ],
                timeout_seconds: 5,
            },
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !output(&f.engine, &run.id, "stdout").contains("ready") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await
        }
    })
    .await
    .unwrap();
    f.engine
        .cancel(CancelRun {
            run_id: run.id.clone(),
            actor: Actor::Agent,
            reason: "stop process group".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert!(!run.workspace.join("leaked.txt").exists());
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_active_run_limit_rejects_then_releases_after_cancel() {
    let f = common::Fixture::new();
    let request = f.request(&["sleep"], 10);
    let mut config = f.engine.config.clone();
    config.max_active_runs = 1;
    let project = f.project.clone();
    drop(f.engine);
    let engine = preflight::engine::Engine::open(config).unwrap();
    let first = engine.start(request.clone()).unwrap();
    let error = engine.start(request.clone()).unwrap_err();
    assert!(matches!(error.downcast_ref::<Fault>(), Some(Fault::Busy)));
    assert_eq!(
        preflight::control::capabilities(&engine)["limits"]["active_runs"],
        1
    );
    engine
        .cancel(CancelRun {
            run_id: first.id.clone(),
            actor: Actor::User,
            reason: "limit test".into(),
        })
        .unwrap();
    common::finished(&engine, &first.id).await;
    let mut next = None;
    for _ in 0..100 {
        match engine.start(request.clone()) {
            Ok(run) => {
                next = Some(run);
                break;
            }
            Err(e) => assert!(matches!(e.downcast_ref::<Fault>(), Some(Fault::Busy))),
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let next = next.expect("cancelled run must release admission slot");
    assert_eq!(next.project_id, project.id);
    engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn signal_exit_has_a_durable_diagnostic_without_fabricated_exit_code() {
    let f = Fixture::new();
    let mut request = f.request(&[], 10);
    request.command.program = "sh".into();
    request.command.args = vec!["-c".into(), "kill -TERM $$".into()];
    let run = f.engine.start(request).unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Failed);
    assert_eq!(done.exit_code, None);
    assert!(output(&f.engine, &run.id, "stderr").contains("process terminated by signal 15"));
    let report = preflight::api::compact_report(&f.engine, &run.id).unwrap();
    assert_eq!(report["stages"]["failed"], 1);
    assert_eq!(report["stage_failures"][0]["stage_id"], "native");
    assert_eq!(
        report["stage_failures"][0]["exit_code"],
        serde_json::Value::Null
    );
    f.engine.shutdown().await.unwrap();
}
