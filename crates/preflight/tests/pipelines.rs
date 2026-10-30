mod common;
use common::*;
use preflight::{
    model::*,
    pipeline::{self, PipelineFile, StageKind, StartPipeline},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

fn fixture() -> Fixture {
    static WORKER: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let binary = &WORKER
        .get_or_init(|| {
            let temp = tempfile::tempdir().unwrap();
            let binary = temp.path().join("worker.exe");
            let output = std::process::Command::new("rustc")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/support/worker.rs"
                ))
                .arg("-o")
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            (temp, binary)
        })
        .1;
    let f = Fixture::new();
    std::fs::copy(binary, f.project.root.join("worker.exe")).unwrap();
    f
}
fn stage(id: &str, needs: &[&str], args: &[&str]) -> Value {
    json!({"id":id,"needs":needs,"command":{"program":"./worker.exe","args":args,"timeout_seconds":5}})
}
fn file(stages: Vec<Value>) -> PipelineFile {
    serde_json::from_value(
        json!({"version":1,"pipeline":{"max_parallel_stages":2,"stages":stages}}),
    )
    .unwrap()
}
fn start(f: &Fixture, definition: PipelineFile) -> Run {
    f.engine
        .start_pipeline(StartPipeline {
            project_id: f.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap()
}
fn producer() -> Value {
    let mut s = stage("build", &[], &["produce"]);
    s["outputs"] = json!({"worker":{"path":"worker.exe","type":"executable"},"payload":{"path":"payload.txt","type":"file"}});
    s
}
async fn stage_running(f: &Fixture, id: &str, name: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if f.engine
                .db
                .run(id)
                .unwrap()
                .stages
                .iter()
                .any(|s| s.definition.id == name && s.status == RunState::Running)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[test]
fn validates_dag_references_paths_types_and_schema() {
    let valid = file(vec![
        stage("a", &[], &["sleep", "1"]),
        stage("b", &["a"], &["sleep", "1"]),
    ]);
    assert!(pipeline::validate(&valid).is_ok());
    let mut invalids = vec![];
    let mut p = valid.clone();
    p.pipeline.stages[0].needs = vec!["b".into()];
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[1].needs = vec!["missing".into()];
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[1].id = "a".into();
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[0].cwd = "../outside".into();
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[1].command.args = vec!["prefix${artifact.a.file}".into()];
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[1].command.program = "${artifact.a.missing}".into();
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[1]
        .inputs
        .insert("x".into(), "not a reference".into());
    invalids.push(p);
    let mut p = valid.clone();
    p.pipeline.stages[0].env.insert(
        "MODULE_PATH".into(),
        format!("${{module.{}}}", "0".repeat(64)),
    );
    invalids.push(p);
    for p in invalids {
        assert!(pipeline::validate(&p).is_err(), "{p:?}");
    }
    for yaml in [
        "version: 2\npipeline: {stages: []}",
        "version: 1\npipeline: {stages: []}\nunknown: true",
        "version: 1\nversion: 1\npipeline: {stages: []}",
    ] {
        assert!(pipeline::parse(yaml).is_err());
    }
    let mut p = producer();
    p["outputs"]["alias"] = json!({"path":"./payload.txt","type":"file"});
    assert!(pipeline::validate(&file(vec![p])).is_err());
    let mut c = stage("consumer", &["build"], &[]);
    c["command"]["program"] = json!("${artifact.build.payload}");
    assert!(pipeline::validate(&file(vec![producer(), c])).is_err());
}
#[tokio::test]
async fn artifact_handoff_uses_isolated_verified_executable_and_inputs() {
    let f = fixture();
    let legacy = f.start(&["echo", "legacy remains supported"], 5);
    assert_eq!(
        finished(&f.engine, &legacy.id).await.status,
        RunState::Succeeded
    );
    // Producer must create its own declared executable, not reuse a source file.
    let mut p = producer();
    p["command"]["program"] = json!(if cfg!(windows) {
        "./helper.exe"
    } else {
        "./helper"
    });
    // Keep worker source under another name so the output name starts absent.
    std::fs::rename(
        f.project.root.join("worker.exe"),
        f.project.root.join("generator.exe"),
    )
    .unwrap();
    p["command"]["program"] = json!("./generator.exe");
    let mut consume = stage(
        "test",
        &["build"],
        &["consume", "${artifact.build.payload}"],
    );
    consume["kind"] = json!("test");
    consume["command"]["program"] = json!("${artifact.build.worker}");
    let run = start(&f, file(vec![p, consume]));
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:#?}");
    assert_eq!(done.stages[1].definition.kind, StageKind::Test);
    let artifacts = f.engine.db.artifacts(&run.id).unwrap();
    assert_eq!(artifacts.len(), 2);
    assert!(artifacts.iter().all(|a| a.sha256.len() == 64
        && a.path.exists()
        && a.path.metadata().unwrap().permissions().readonly()));
    assert!(output(&f.engine, &run.id, "stdout").contains(".preflight/inputs/build/worker"));
    assert!(output(&f.engine, &run.id, "stdout").contains("consumed artifact"));
    assert!(!f.project.root.join("payload.txt").exists());
    let report = preflight::api::compact_report(&f.engine, &run.id).unwrap();
    assert_eq!(report["tests"]["status"], "succeeded");
    assert!(report["tests"]["passed"].is_null());
    let page = f.engine.db.stage_logs(&run.id, Some("test"), 0).unwrap();
    assert!(page.events.iter().all(|e| e.stage_id == "test"));
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn independent_stages_overlap_failed_dependencies_are_blocked() {
    let f = fixture();
    let a = stage("a", &[], &["sleep", "500"]);
    let b = stage("b", &[], &["sleep", "500"]);
    let fail = stage("fail", &["a"], &["fail"]);
    let blocked = stage("blocked", &["fail"], &["write", "should-not-exist"]);
    let run = start(&f, file(vec![blocked, fail, b, a]));
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Failed);
    let a = done.stages.iter().find(|s| s.definition.id == "a").unwrap();
    let b = done.stages.iter().find(|s| s.definition.id == "b").unwrap();
    assert!(
        a.started_at.unwrap() < b.finished_at.unwrap()
            && b.started_at.unwrap() < a.finished_at.unwrap()
    );
    assert_eq!(done.stages[0].status, RunState::Blocked);
    assert!(!run.workspace.join("stages/blocked").exists());
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn successful_exit_with_junit_failure_fails_tests_and_survives_restart() {
    let f = fixture();
    let build = stage("build", &[], &["write", "built.txt"]);
    let xml = r#"<testsuite tests="3" failures="1" skipped="1"><testcase name="pass"/><testcase name="bad"><failure message="expected 2 got 3"/></testcase><testcase name="skip"><skipped/></testcase></testsuite>"#;
    let mut tests = stage("test", &["build"], &["junit", "results.xml", xml]);
    tests["kind"] = json!("test");
    tests["junit"] = json!(["results.xml"]);
    let run = start(&f, file(vec![build, tests]));
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Failed);
    assert_eq!(done.stages[1].exit_code, Some(0));
    let report = preflight::api::compact_report(&f.engine, &run.id).unwrap();
    assert_eq!(report["outcome"], "tests_failed");
    assert_eq!(report["build"]["status"], "succeeded");
    assert_eq!(report["tests"]["passed"], 1);
    assert_eq!(report["tests"]["failed"], 1);
    assert_eq!(report["tests"]["skipped"], 1);
    f.engine.shutdown().await.unwrap();
    let config = Fixture::config(f.temp.path());
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(config).unwrap();
    assert_eq!(
        reopened.db.run(&run.id).unwrap().stages[1]
            .tests
            .as_ref()
            .unwrap()
            .counts
            .failed,
        1
    );
}
#[tokio::test]
async fn missing_or_invalid_junit_and_missing_outputs_cannot_succeed() {
    let f = fixture();
    let mut missing = stage("test", &[], &["sleep", "1"]);
    missing["kind"] = json!("test");
    missing["junit"] = json!(["missing.xml"]);
    let mut invalid = stage("test", &[], &["junit", "results.xml", "<testsuite>"]);
    invalid["kind"] = json!("test");
    invalid["junit"] = json!(["results.xml"]);
    let mut output = stage("build", &[], &["sleep", "1"]);
    output["outputs"] = json!({"missing":{"path":"missing.txt","type":"file"}});
    for stage in [missing, invalid, output] {
        let run = start(&f, file(vec![stage]));
        assert_eq!(
            finished(&f.engine, &run.id).await.status,
            RunState::InfrastructureError
        );
        assert!(f.engine.db.artifacts(&run.id).unwrap().is_empty());
    }
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancellation_and_timeout_stop_stage_process_trees() {
    let f = fixture();
    let child = stage("child", &[], &["child", "child-survived.txt"]);
    let pending = stage("pending", &["child"], &["write", "unexpected"]);
    let run = start(&f, file(vec![child, pending]));
    stage_running(&f, &run.id, "child").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !output(&f.engine, &run.id, "stdout").contains("child ready") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.engine
        .cancel(CancelRun {
            run_id: run.id.clone(),
            actor: Actor::Agent,
            reason: "cancel DAG".into(),
        })
        .unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Cancelled);
    assert!(
        done.stages
            .iter()
            .all(|s| s.status == RunState::Cancelled && s.cancel_actor.as_deref() == Some("agent"))
    );
    let mut child = stage("child", &[], &["child", "timeout-survived.txt"]);
    child["command"]["timeout_seconds"] = json!(1);
    let run2 = start(
        &f,
        file(vec![child, stage("independent", &[], &["sleep", "50"])]),
    );
    let done = finished(&f.engine, &run2.id).await;
    assert_eq!(done.status, RunState::TimedOut);
    assert_eq!(done.stages[0].cancel_actor.as_deref(), Some("timeout"));
    assert_eq!(done.stages[1].status, RunState::Succeeded);
    tokio::time::sleep(Duration::from_millis(1700)).await;
    assert!(
        !run.workspace
            .join("stages/child/child-survived.txt")
            .exists()
    );
    assert!(
        !run2
            .workspace
            .join("stages/child/timeout-survived.txt")
            .exists()
    );
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn cwd_env_explicit_input_and_digest_tampering_are_checked() {
    let f = fixture();
    std::fs::rename(
        f.project.root.join("worker.exe"),
        f.project.root.join("generator.exe"),
    )
    .unwrap();
    std::fs::create_dir(f.project.root.join("work")).unwrap();
    let mut p = producer();
    p["command"]["program"] = json!("./generator.exe");
    let mut env = stage("env", &["build"], &["env"]);
    env["command"]["program"] = json!("${artifact.build.worker}");
    env["cwd"] = json!("work");
    env["env"] = json!({"STAGE_VALUE":"exact value"});
    env["inputs"] = json!({"work/input.txt":"${artifact.build.payload}"});
    env["outputs"] = json!({"value":{"path":"env.txt","type":"file"}});
    let run = start(&f, file(vec![p.clone(), env]));
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let a = f
        .engine
        .db
        .artifacts(&run.id)
        .unwrap()
        .into_iter()
        .find(|a| a.stage_id == "env")
        .unwrap();
    assert_eq!(std::fs::read_to_string(a.path).unwrap(), "exact value");
    let mut delay = stage("delay", &["build"], &["sleep", "500"]);
    delay["command"]["program"] = json!("./generator.exe");
    let mut consume = stage(
        "consume",
        &["build", "delay"],
        &["consume", "${artifact.build.payload}"],
    );
    consume["command"]["program"] = json!("${artifact.build.worker}");
    let run = start(&f, file(vec![p, delay, consume]));
    stage_running(&f, &run.id, "delay").await;
    let artifact = f
        .engine
        .db
        .artifacts(&run.id)
        .unwrap()
        .into_iter()
        .find(|a| a.name == "payload")
        .unwrap();
    #[cfg(windows)]
    {
        let mut p = std::fs::metadata(&artifact.path).unwrap().permissions();
        p.set_readonly(false);
        std::fs::set_permissions(&artifact.path, p).unwrap();
    }
    std::fs::remove_file(&artifact.path).unwrap();
    std::fs::write(&artifact.path, "tampered").unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::InfrastructureError);
    assert!(
        done.stages[2]
            .reason
            .as_ref()
            .unwrap()
            .contains("digest mismatch")
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepared_modules_cross_runs_preserve_paths_and_provenance() {
    use preflight::{
        inputs::InputQuery,
        modules::{self, Publish},
    };
    let f = fixture();
    std::fs::rename(
        f.project.root.join("worker.exe"),
        f.project.root.join("generator.exe"),
    )
    .unwrap();
    let mut p = producer();
    p["command"]["program"] = json!("./generator.exe");
    let first = start(&f, file(vec![p.clone()]));
    assert_eq!(
        finished(&f.engine, &first.id).await.status,
        RunState::Succeeded
    );
    let publish = |name: &str| {
        modules::publish(
            &f.engine,
            &Publish {
                run_id: first.id.clone(),
                stage_id: "build".into(),
                artifact_name: name.into(),
            },
        )
        .unwrap()
    };
    let compiler = publish("worker");
    let payload = publish("payload");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let engine = f.engine.clone();
        let run_id = first.id.clone();
        tasks.spawn_blocking(move || {
            modules::publish(
                &engine,
                &Publish {
                    run_id,
                    stage_id: "build".into(),
                    artifact_name: "worker".into(),
                },
            )
            .unwrap()
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().path, compiler.path);
    }
    let second = start(&f, file(vec![p]));
    finished(&f.engine, &second.id).await;
    let again = modules::publish(
        &f.engine,
        &Publish {
            run_id: second.id,
            stage_id: "build".into(),
            artifact_name: "worker".into(),
        },
    )
    .unwrap();
    assert_eq!(again.run_id, first.id); // Canonical publication preserves first producer.
    let consumer = json!({"id":"use","command":{"program":format!("${{module.{}}}",compiler.sha256),"args":["consume",format!("${{module.{}}}",payload.sha256)]},"inputs":{"deps/module.dat":format!("${{module.{}}}",payload.sha256)}});
    let run = start(&f, file(vec![consumer]));
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").contains("consumed artifact"));
    let query = InputQuery {
        run_id: run.id.clone(),
        offset: 0,
    };
    let records = f.engine.db.module_inputs(&query).unwrap();
    let entries = records["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    for entry in entries {
        assert_eq!(entry["producer"]["run_id"], first.id);
        let path = std::path::Path::new(entry["materialized_path"].as_str().unwrap());
        assert!(path.exists());
        assert!(path.starts_with(&run.workspace));
        assert_ne!(path, compiler.path);
    }
    let db = preflight::db::Db::new(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    assert_eq!(db.module_inputs(&query).unwrap(), records);
    assert_eq!(
        db.module(&f.project.id, &compiler.sha256).unwrap().run_id,
        first.id
    );
    assert!(
        db.module_inputs(&InputQuery {
            run_id: run.id,
            offset: 20_001
        })
        .is_err()
    );
}

#[tokio::test]
async fn prepared_modules_reject_corruption_wrong_kind_and_cross_project_access() {
    use preflight::modules::{self, Publish};
    let f = fixture();
    std::fs::rename(
        f.project.root.join("worker.exe"),
        f.project.root.join("generator.exe"),
    )
    .unwrap();
    let mut p = producer();
    p["command"]["program"] = json!("./generator.exe");
    let producer_run = start(&f, file(vec![p]));
    finished(&f.engine, &producer_run.id).await;
    let payload = modules::publish(
        &f.engine,
        &Publish {
            run_id: producer_run.id,
            stage_id: "build".into(),
            artifact_name: "payload".into(),
        },
    )
    .unwrap();
    let reference = format!("${{module.{}}}", payload.sha256);
    let bad_program = start(
        &f,
        file(vec![
            json!({"id":"use","command":{"program":reference,"args":[]}}),
        ]),
    );
    assert_ne!(
        finished(&f.engine, &bad_program.id).await.status,
        RunState::Succeeded
    );
    let root = f.temp.path().join("other-project");
    std::fs::create_dir(&root).unwrap();
    let other = f
        .engine
        .register(RegisterProject {
            name: "other".into(),
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    assert!(f.engine.db.module(&other.id, &payload.sha256).is_err());
    assert!(f.engine.db.module(&f.project.id, "../escape").is_err());
    let collision = start(
        &f,
        file(vec![
            json!({"id":"use","command":{"program":"./generator.exe","args":["sleep","1"]},"inputs":{"generator.exe":reference}}),
        ]),
    );
    assert_ne!(
        finished(&f.engine, &collision.id).await.status,
        RunState::Succeeded
    );
    assert!(
        modules::publish(
            &f.engine,
            &Publish {
                run_id: collision.id,
                stage_id: "use".into(),
                artifact_name: "missing".into()
            }
        )
        .is_err()
    );
    std::fs::remove_file(&payload.path).unwrap();
    std::fs::write(&payload.path, "tampered").unwrap();
    let bad = start(
        &f,
        file(vec![
            json!({"id":"use","command":{"program":"./generator.exe","args":["consume",reference]}}),
        ]),
    );
    assert_ne!(
        finished(&f.engine, &bad.id).await.status,
        RunState::Succeeded
    );
    assert!(!output(&f.engine, &bad.id, "stdout").contains("consumed artifact"));
    assert!(
        f.engine
            .db
            .module_inputs(&preflight::inputs::InputQuery {
                run_id: bad.id,
                offset: 0
            })
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for value in ["${module.bad}", "prefix${module.0123}", "${module./../x}"] {
        assert!(
            pipeline::validate(&file(vec![
                json!({"id":"invalid","command":{"program":"./generator.exe","args":[value]}})
            ]))
            .is_err()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_modules_publish_consume_page_and_reject_corruption() {
    use preflight::modules::{self, ManifestQuery, Publish};
    let f = fixture();
    let producer = json!({"id":"tree","command":{"program":"./worker.exe","args":["produce-tree"]},"outputs":{"sdk":{"path":"module-dir","type":"directory"}}});
    let consume = json!({"id":"same-run","needs":["tree"],"command":{"program":"./worker.exe","args":["consume-tree","${artifact.tree.sdk}"]}});
    let run = start(&f, file(vec![producer, consume]));
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let engine = f.engine.clone();
        let run_id = run.id.clone();
        tasks.spawn_blocking(move || {
            modules::publish(
                &engine,
                &Publish {
                    run_id,
                    stage_id: "tree".into(),
                    artifact_name: "sdk".into(),
                },
            )
            .unwrap()
        });
    }
    let mut published = vec![];
    while let Some(result) = tasks.join_next().await {
        published.push(result.unwrap());
    }
    let module = &published[0];
    assert!(
        published
            .iter()
            .all(|m| m.sha256 == module.sha256 && m.path == module.path)
    );
    assert_eq!(module.kind, pipeline::ArtifactKind::Directory);
    assert!(module.path.is_dir());
    let query = |offset| ManifestQuery {
        project_id: f.project.id.clone(),
        sha256: module.sha256.clone(),
        offset,
    };
    let page = f.engine.db.module_manifest(&query(0)).unwrap();
    assert_eq!(page["total"], 108);
    assert_eq!(page["entries"].as_array().unwrap().len(), 100);
    assert_eq!(page["next_cursor"], 100);
    let next = f.engine.db.module_manifest(&query(100)).unwrap();
    assert_eq!(next["entries"].as_array().unwrap().len(), 8);
    assert!(next["next_cursor"].is_null());
    let reference = format!("${{module.{}}}", module.sha256);
    let consumer = file(vec![
        json!({"id":"reuse","command":{"program":"./worker.exe","args":["consume-tree",reference]},"inputs":{"deps/sdk":reference}}),
    ]);
    let reused = start(&f, consumer.clone());
    assert_eq!(
        finished(&f.engine, &reused.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &reused.id, "stdout").contains("consumed directory module"));
    let paths = f
        .engine
        .db
        .module_inputs(&preflight::inputs::InputQuery {
            run_id: reused.id.clone(),
            offset: 0,
        })
        .unwrap();
    assert_eq!(paths["entries"].as_array().unwrap().len(), 2);
    for entry in paths["entries"].as_array().unwrap() {
        assert_eq!(entry["producer"]["run_id"], run.id);
        assert!(
            std::path::Path::new(entry["materialized_path"].as_str().unwrap())
                .join("lib/module.dat")
                .exists()
        );
    }
    let reopened =
        preflight::db::Db::new(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    assert_eq!(reopened.module_manifest(&query(0)).unwrap(), page);
    let bad_program = start(
        &f,
        file(vec![
            json!({"id":"bad","command":{"program":reference,"args":[]}}),
        ]),
    );
    assert_ne!(
        finished(&f.engine, &bad_program.id).await.status,
        RunState::Succeeded
    );
    std::fs::remove_file(module.path.join("lib/module.dat")).unwrap();
    std::fs::write(module.path.join("lib/module.dat"), "changed").unwrap();
    let bad = start(&f, consumer);
    assert_ne!(
        finished(&f.engine, &bad.id).await.status,
        RunState::Succeeded
    );
    assert!(!output(&f.engine, &bad.id, "stdout").contains("consumed directory module"));
    assert!(
        modules::publish(
            &f.engine,
            &Publish {
                run_id: run.id,
                stage_id: "tree".into(),
                artifact_name: "sdk".into()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn weighted_stage_reservations_serialize_heavy_work_and_release_on_cancel() {
    let f = fixture();
    let mut a = stage("a", &[], &["sleep", "150"]);
    a["resources"] = json!({"slots":2});
    let mut b = a.clone();
    b["id"] = "b".into();
    let run = start(&f, file(vec![a, b]));
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded);
    let a = &done.stages[0];
    let b = &done.stages[1];
    assert!(
        a.finished_at.unwrap() <= b.started_at.unwrap()
            || b.finished_at.unwrap() <= a.started_at.unwrap()
    );

    let mut heavy = stage("heavy", &[], &["sleep", "3000"]);
    heavy["resources"] = json!({"slots":2});
    let owner = start(&f, file(vec![heavy]));
    stage_running(&f, &owner.id, "heavy").await;
    assert_eq!(f.engine.scheduler_status()["reserved_slots"], 2);
    let other_root = f.temp.path().join("other-project");
    std::fs::create_dir(&other_root).unwrap();
    std::fs::copy(
        f.project.root.join("worker.exe"),
        other_root.join("worker.exe"),
    )
    .unwrap();
    let other_project = f
        .engine
        .register(RegisterProject {
            name: "other".into(),
            path: other_root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let waiter = f
        .engine
        .start_pipeline(StartPipeline {
            project_id: other_project.id,
            definition: Some(file(vec![stage("waiting", &[], &["sleep", "1"])])),
        })
        .unwrap();
    f.engine
        .cancel(CancelRun {
            run_id: waiter.id.clone(),
            actor: Actor::User,
            reason: "cancel waiting reservation".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &waiter.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.run(&owner.id).unwrap().status,
        RunState::Running
    );
    f.engine
        .cancel(CancelRun {
            run_id: owner.id.clone(),
            actor: Actor::User,
            reason: "release all reserved slots".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &owner.id).await.status,
        RunState::Cancelled
    );
    let next = start(&f, file(vec![stage("next", &[], &["sleep", "1"])]));
    assert_eq!(
        finished(&f.engine, &next.id).await.status,
        RunState::Succeeded
    );
    f.engine.shutdown().await.unwrap();
    assert_eq!(f.engine.scheduler_status()["available_slots"], 2);
}

#[test]
fn resource_admission_rejects_impossible_requests_without_creating_runs() {
    let f = fixture();
    for slots in [0, 3, 33] {
        let mut s = stage("too_big", &[], &["sleep", "1"]);
        s["resources"] = json!({"slots":slots});
        assert!(
            f.engine
                .start_pipeline(StartPipeline {
                    project_id: f.project.id.clone(),
                    definition: Some(file(vec![s]))
                })
                .is_err()
        );
    }
    assert!(f.engine.db.runs().unwrap().is_empty());
    let legacy = preflight::pipeline::StageDefinition::native(f.request(&[], 5).command);
    assert_eq!(legacy.required_slots(), 1);
    assert!(
        serde_json::to_value(legacy)
            .unwrap()
            .get("resources")
            .is_none()
    );
}

fn memory_fixture() -> Fixture {
    let Fixture {
        temp,
        engine,
        project,
    } = fixture();
    let mut config = engine.config.clone();
    drop(engine);
    config.memory_budget_mib = Some(100);
    config.default_stage_memory_mib = 20;
    let engine = preflight::engine::Engine::open(config).unwrap();
    Fixture {
        temp,
        engine,
        project,
    }
}
#[tokio::test]
async fn memory_admission_serializes_stages_and_cancels_cross_project_waiters() {
    let f = memory_fixture();
    let mut first = stage("first", &[], &["sleep", "150"]);
    first["resources"] = json!({"memory_mib":60});
    let mut second = first.clone();
    second["id"] = "second".into();
    let run = start(&f, file(vec![first, second]));
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded);
    let (a, b) = (&done.stages[0], &done.stages[1]);
    assert!(
        a.finished_at.unwrap() <= b.started_at.unwrap()
            || b.finished_at.unwrap() <= a.started_at.unwrap()
    );
    let mut heavy = stage("heavy", &[], &["sleep", "3000"]);
    heavy["resources"] = json!({"slots":1,"memory_mib":80});
    let owner = start(&f, file(vec![heavy]));
    stage_running(&f, &owner.id, "heavy").await;
    let root = f.temp.path().join("other");
    std::fs::create_dir(&root).unwrap();
    std::fs::copy(f.project.root.join("worker.exe"), root.join("worker.exe")).unwrap();
    let project = f
        .engine
        .register(RegisterProject {
            name: "other-memory-project".into(),
            path: root.to_string_lossy().into(),
        })
        .unwrap();
    let mut waiting = stage("waiting", &[], &["sleep", "1"]);
    waiting["resources"] = json!({"memory_mib":60});
    let waiter = f
        .engine
        .start_pipeline(StartPipeline {
            project_id: project.id,
            definition: Some(file(vec![waiting])),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if f.engine.scheduler_status()["waiting_reservations"]
                .as_u64()
                .unwrap()
                > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.engine.scheduler_status()["available_slots"], 1);
    assert_eq!(f.engine.scheduler_status()["reserved_memory_mib"], 80);
    f.engine
        .cancel(CancelRun {
            run_id: waiter.id.clone(),
            actor: Actor::User,
            reason: "cancel memory waiter".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &waiter.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.run(&owner.id).unwrap().status,
        RunState::Running
    );
    // Unspecified memory uses the configured default and fits alongside the owner.
    let small = start(&f, file(vec![stage("small", &[], &["sleep", "1"])]));
    assert_eq!(
        finished(&f.engine, &small.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine.db.run(&owner.id).unwrap().status,
        RunState::Running
    );
    f.engine
        .cancel(CancelRun {
            run_id: owner.id.clone(),
            actor: Actor::User,
            reason: "release reservation".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &owner.id).await.status,
        RunState::Cancelled
    );
    f.engine.shutdown().await.unwrap();
    let status = f.engine.scheduler_status();
    assert_eq!(status["available_slots"], 2);
    assert_eq!(status["available_memory_mib"], 100);
    assert_eq!(status["waiting_reservations"], 0);
}
#[test]
fn memory_requests_fail_before_persistence_if_unconfigured_zero_or_too_large() {
    let unconfigured = fixture();
    let configured = memory_fixture();
    for (f, memory) in [(&unconfigured, 1), (&configured, 0), (&configured, 101)] {
        let mut s = stage("invalid", &[], &["sleep", "1"]);
        s["resources"] = json!({"memory_mib":memory});
        assert!(
            f.engine
                .start_pipeline(StartPipeline {
                    project_id: f.project.id.clone(),
                    definition: Some(file(vec![s]))
                })
                .is_err()
        );
        assert!(f.engine.db.runs().unwrap().is_empty());
    }
    assert!(unconfigured.engine.scheduler_status()["memory_budget_mib"].is_null());
}
