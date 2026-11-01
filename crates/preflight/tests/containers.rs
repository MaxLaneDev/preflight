#[allow(dead_code)]
mod common;
use preflight::{
    engine::Engine,
    model::*,
    pipeline::{PipelineFile, StartPipeline},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
struct Fixture {
    #[cfg_attr(
        windows,
        expect(
            dead_code,
            reason = "Retains the temporary directory for the lifetime of the fixture"
        )
    )]
    temp: tempfile::TempDir,
    e: Arc<Engine>,
    project: Project,
    image: String,
}
impl Fixture {
    fn new(runtime: PathBuf, image: String) -> Self {
        let temp = tempfile::Builder::new()
            .prefix("preflight-container,quoted\"")
            .tempdir()
            .unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("input.txt"), "payload\n").unwrap();
        let mut config = common::Fixture::config(temp.path());
        config.docker_command = Some(runtime);
        config.memory_budget_mib = Some(256);
        config.default_stage_memory_mib = 64;
        let e = Engine::open(config).unwrap();
        let project = e
            .register(RegisterProject {
                name: "container-test".into(),
                path: root.to_string_lossy().into(),
            })
            .unwrap();
        Self {
            temp,
            e,
            project,
            image,
        }
    }
    fn stage(&self, id: &str, script: &str) -> Value {
        json!({"id":id,"executor":"docker","container":{"image":self.image,"platform":"linux/arm64"},"command":{"program":"/bin/sh","args":["-c",script],"timeout_seconds":10}})
    }
    fn start(&self, stages: Vec<Value>) -> Run {
        let definition: PipelineFile = serde_json::from_value(
            json!({"version":1,"pipeline":{"max_parallel_stages":2,"stages":stages}}),
        )
        .unwrap();
        self.e
            .start_pipeline(StartPipeline {
                project_id: self.project.id.clone(),
                definition: Some(definition),
            })
            .unwrap()
    }
    fn jobs(&self, run: &str) -> Value {
        preflight::docker::list(
            &self.e.db,
            &preflight::docker::Query {
                project_id: Some(self.project.id.clone()),
                run_id: Some(run.into()),
                before: None,
                limit: Some(100),
                max_bytes: Some(65536),
            },
        )
        .unwrap()
    }
    async fn finish(&self, run: &str) -> Run {
        eprintln!("waiting for Docker run {run}");
        let r = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let run = self.e.db.run(run).unwrap();
                if run.status.terminal() {
                    break run;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Docker run timed out: {:?}; jobs={}; stdout={}; stderr={}",
                self.e.db.run(run),
                self.jobs(run),
                common::output(&self.e, run, "stdout"),
                common::output(&self.e, run, "stderr")
            )
        });
        eprintln!("Docker run {run} finished {:?}", r.status);
        if r.status != RunState::Succeeded {
            eprintln!(
                "run {run}: {:?}\nlogs: {}",
                r.stages,
                common::output(&self.e, run, "stderr")
            );
        }
        r
    }
}
fn policy(shared: bool) -> Value {
    json!({"pipeline_id":"docker-build","deterministic":true,"inputs_complete":true,"single_flight":shared})
}
#[cfg(unix)]
#[tokio::test]
async fn unavailable_docker_never_falls_back_to_host_execution() {
    let f = Fixture::new(
        PathBuf::from("/usr/bin/false"),
        format!("sha256:{}", "a".repeat(64)),
    );
    let mut stage = f.stage("build", "touch host-fallback-marker");
    stage["cache"] = policy(false);
    stage["outputs"] = json!({"marker":{"path":"host-fallback-marker","type":"file"}});
    let run = f.start(vec![stage]);
    assert_eq!(
        f.finish(&run.id).await.status,
        RunState::InfrastructureError
    );
    assert!(
        !std::fs::read_dir(f.temp.path().join("data/workspaces"))
            .unwrap()
            .any(|r| r
                .unwrap()
                .path()
                .join("stages/build/host-fallback-marker")
                .exists())
    );
    assert!(f.jobs(&run.id)["jobs"].as_array().unwrap().is_empty());
    f.e.shutdown().await.unwrap();
}
fn actual() -> Fixture {
    let image = std::env::var("PREFLIGHT_TEST_DOCKER_IMAGE")
        .expect("set an already-cached immutable Linux ARM64 image");
    Fixture::new(
        std::env::var_os("PREFLIGHT_TEST_DOCKER_COMMAND")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin/docker")),
        image,
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 Docker image and local daemon"]
async fn actual_container_artifacts_cache_exit_timeout_and_cancel() {
    let f = actual();
    let mut build=f.stage("build","test -f /.dockerenv; cp /bin/true /tmp/preflight-temp-executable; /tmp/preflight-temp-executable; test \"$HOME\" = /tmp; test \"$PREFLIGHT_WORKSPACE\" = /workspace; cat input.txt > product.txt; echo built");
    build["outputs"] = json!({"payload":{"path":"product.txt","type":"file"}});
    build["cache"] = policy(false);
    let mut test=f.stage("test","test \"$(cat input/product.txt)\" = payload; printf '%s' '<testsuite name=\"docker\" tests=\"1\"><testcase name=\"artifact\"/></testsuite>' > report.xml");
    test["kind"] = json!("test");
    test["needs"] = json!(["build"]);
    test["inputs"] = json!({"input/product.txt":"${artifact.build.payload}"});
    test["junit"] = json!(["report.xml"]);
    let first = f.start(vec![build.clone(), test.clone()]);
    let r = f.finish(&first.id).await;
    assert_eq!(r.status, RunState::Succeeded);
    assert_eq!(r.stages[1].tests.as_ref().unwrap().counts.passed, 1);
    let jobs = f.jobs(&first.id);
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 2);
    assert!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["state"] == "cleaned" && j["actual_image_id"] == f.image)
    );
    assert!(!preflight::docker::admission_pending(&f.e.db).unwrap());
    let module = preflight::modules::publish(
        &f.e,
        &preflight::modules::Publish {
            run_id: first.id.clone(),
            stage_id: "build".into(),
            artifact_name: "payload".into(),
        },
    )
    .unwrap();
    let contracts =
        f.e.db
            .module_contracts(&preflight::modules::ManifestQuery {
                project_id: f.project.id.clone(),
                sha256: module.sha256.clone(),
                offset: 0,
            })
            .unwrap();
    assert!(
        contracts
            .to_string()
            .contains("preflight-container-module-v1")
    );
    let mut consumer = f.stage("module", "test \"$(cat prepared.txt)\" = payload");
    consumer["inputs"] = json!({"prepared.txt":format!("${{module.{}}}",module.sha256)});
    let matching = f.start(vec![consumer.clone()]);
    assert_eq!(f.finish(&matching.id).await.status, RunState::Succeeded);
    consumer["container"]["read_only"] = json!(false);
    let incompatible = f.start(vec![consumer.clone()]);
    assert_eq!(
        f.finish(&incompatible.id).await.status,
        RunState::InfrastructureError
    );
    assert!(
        f.jobs(&incompatible.id)["jobs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "reject before command admission"
    );
    consumer.as_object_mut().unwrap().remove("container");
    consumer["executor"] = json!("native");
    consumer["command"] = json!({"program":"true","args":[]});
    let native = f.start(vec![consumer]);
    assert_eq!(
        f.finish(&native.id).await.status,
        RunState::InfrastructureError,
        "container modules must never be consumed as legacy native modules"
    );
    let second = f.start(vec![build.clone(), test.clone()]);
    assert_eq!(f.finish(&second.id).await.status, RunState::Succeeded);
    let decisions = f.e.db.cache_decisions(&second.id).unwrap();
    assert!(decisions.to_string().contains("hit"), "{decisions}");
    assert_eq!(
        f.jobs(&second.id)["jobs"].as_array().unwrap().len(),
        1,
        "tests must execute again"
    );
    build["container"]["read_only"] = json!(false);
    let third = f.start(vec![build]);
    assert_eq!(f.finish(&third.id).await.status, RunState::Succeeded);
    assert_eq!(
        f.jobs(&third.id)["jobs"].as_array().unwrap().len(),
        1,
        "container policy changes must invalidate cache"
    );
    let failed = f.start(vec![
        f.stage("failure", "echo intended-failure >&2; exit 7"),
    ]);
    let r = f.finish(&failed.id).await;
    assert_eq!(r.status, RunState::Failed);
    assert_eq!(r.stages[0].exit_code, Some(7));
    let mut timeout = f.stage("deadline", "echo ready; sleep 30");
    timeout["command"]["timeout_seconds"] = json!(1);
    let r = f.start(vec![timeout]);
    assert_eq!(f.finish(&r.id).await.status, RunState::TimedOut);
    assert_eq!(f.jobs(&r.id)["jobs"][0]["state"], "cleaned");
    let r = f.start(vec![f.stage("cancel", "echo ready; sleep 30")]);
    tokio::time::timeout(Duration::from_secs(15), async {
        while !common::output(&f.e, &r.id, "stdout").contains("ready") {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    f.e.cancel(CancelRun {
        run_id: r.id.clone(),
        actor: Actor::User,
        reason: "container acceptance".into(),
    })
    .unwrap();
    assert_eq!(f.finish(&r.id).await.status, RunState::Cancelled);
    assert_eq!(f.jobs(&r.id)["jobs"][0]["state"], "cleaned");
    assert!(!preflight::docker::admission_pending(&f.e.db).unwrap());
    f.e.shutdown().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 Docker image and local daemon"]
async fn actual_container_single_flight_has_independent_subscriber_cancel() {
    let f = actual();
    let mut stage = f.stage("shared", "echo ready; sleep 3; echo shared > output.txt");
    stage["outputs"] = json!({"payload":{"path":"output.txt","type":"file"}});
    stage["cache"] = policy(true);
    let a = f.start(vec![stage.clone()]);
    let b = f.start(vec![stage]);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a = f.e.db.cache_decisions(&a.id).unwrap();
            let b = f.e.db.cache_decisions(&b.id).unwrap();
            if a.to_string().contains("execution_run") && b.to_string().contains("execution_run") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.e.cancel(CancelRun {
        run_id: a.id.clone(),
        actor: Actor::User,
        reason: "one subscriber only".into(),
    })
    .unwrap();
    assert_eq!(f.finish(&a.id).await.status, RunState::Cancelled);
    assert_eq!(f.finish(&b.id).await.status, RunState::Succeeded);
    let decisions = f.e.db.cache_decisions(&b.id).unwrap();
    assert!(decisions.to_string().contains("shared"), "{decisions}");
    let all = preflight::docker::list(
        &f.e.db,
        &preflight::docker::Query {
            project_id: Some(f.project.id.clone()),
            run_id: None,
            before: None,
            limit: Some(100),
            max_bytes: Some(65536),
        },
    )
    .unwrap();
    assert_eq!(
        all["jobs"].as_array().unwrap().len(),
        1,
        "subscribers must use one real container"
    );
    assert_eq!(all["jobs"][0]["state"], "cleaned");
    f.e.shutdown().await.unwrap();
}
#[cfg(unix)]
fn fake_runtime() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("docker");
    std::fs::write(&script, include_str!("support/docker.sh")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    (temp, script)
}
#[cfg(unix)]
#[tokio::test]
async fn cleanup_failure_recovery_scope_and_request_identity_survive_restart() {
    let (runtime, path) = fake_runtime();
    std::fs::write(runtime.path().join("refuse-remove"), "").unwrap();
    let f = Fixture::new(path, format!("sha256:{}", "a".repeat(64)));
    let run = f.start(vec![f.stage("build", "ignored by fake runtime")]);
    assert_eq!(
        f.finish(&run.id).await.status,
        RunState::InfrastructureError
    );
    let jobs = f.jobs(&run.id);
    let id = jobs["jobs"][0]["job_id"].as_str().unwrap().to_string();
    assert_eq!(jobs["jobs"][0]["state"], "cleanup_pending");
    assert!(preflight::docker::admission_pending(&f.e.db).unwrap());
    let blocked = f.e.start_pipeline(StartPipeline {
        project_id: f.project.id.clone(),
        definition: Some(
            serde_json::from_value(
                json!({"version":1,"pipeline":{"stages":[f.stage("blocked","ignored")]}}),
            )
            .unwrap(),
        ),
    });
    assert!(
        blocked
            .unwrap_err()
            .to_string()
            .contains("container_cleanup_pending")
    );
    let mut request = preflight::docker::Cleanup {
        creation_settled: false,
        project_id: "wrong-project".into(),
        job_id: id,
        request_id: "cleanup-1".into(),
        actor: "test".into(),
        reason: "retry owned cleanup".into(),
    };
    assert!(
        preflight::docker::cleanup_request(&f.e, &request)
            .await
            .is_err()
    );
    request.project_id = f.project.id.clone();
    assert!(
        preflight::docker::cleanup_request(&f.e, &request)
            .await
            .is_err()
    );
    let config = f.e.current_config();
    f.e.shutdown().await.unwrap();
    let Fixture {
        temp,
        e,
        project,
        image,
    } = f;
    drop(e);
    tokio::task::yield_now().await;
    let e = Engine::open(config).unwrap();
    assert!(preflight::docker::admission_pending(&e.db).unwrap());
    std::fs::remove_file(runtime.path().join("refuse-remove")).unwrap();
    assert_eq!(
        preflight::docker::cleanup_request(&e, &request)
            .await
            .unwrap()["state"],
        "cleaned"
    );
    assert_eq!(
        preflight::docker::cleanup_request(&e, &request)
            .await
            .unwrap()["replayed"],
        true
    );
    request.reason = "changed request must conflict".into();
    assert!(
        preflight::docker::cleanup_request(&e, &request)
            .await
            .is_err()
    );
    assert!(!preflight::docker::admission_pending(&e.db).unwrap());
    let f = Fixture {
        temp,
        e,
        project,
        image,
    };
    let run = f.start(vec![f.stage("after-recovery", "ignored")]);
    assert_eq!(f.finish(&run.id).await.status, RunState::Succeeded);
    assert_eq!(f.jobs(&run.id)["jobs"][0]["result"]["exit_code"], 0);
    f.e.shutdown().await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn foreign_labels_are_never_removed_and_keep_the_workspace_protected() {
    let (runtime, path) = fake_runtime();
    std::fs::write(runtime.path().join("foreign-label"), "").unwrap();
    let f = Fixture::new(path, format!("sha256:{}", "a".repeat(64)));
    let run = f.start(vec![f.stage("build", "ignored")]);
    assert_eq!(
        f.finish(&run.id).await.status,
        RunState::InfrastructureError
    );
    assert!(runtime.path().join("container.json").exists());
    assert!(
        !preflight::docker::admission_pending(&f.e.db).unwrap(),
        "verified never-started containers do not consume execution slots"
    );
    let c = rusqlite::Connection::open(f.e.config.data_dir.join("preflight.sqlite3")).unwrap();
    let protected: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM container_jobs j JOIN operation_leases l ON l.id=j.lease_id WHERE j.run_id=?1 AND j.may_run=0 AND l.state='abandoned')",[&run.id],|r|r.get(0)).unwrap();
    assert!(protected);
    drop(c);

    let jobs = f.jobs(&run.id);
    let request = preflight::docker::Cleanup {
        creation_settled: false,
        project_id: f.project.id.clone(),
        job_id: jobs["jobs"][0]["job_id"].as_str().unwrap().into(),
        request_id: "labels-fixed".into(),
        actor: "test".into(),
        reason: "restore exact original ownership".into(),
    };
    assert!(
        preflight::docker::cleanup_request(&f.e, &request)
            .await
            .is_err()
    );
    assert!(runtime.path().join("container.json").exists());
    std::fs::remove_file(runtime.path().join("foreign-label")).unwrap();
    preflight::docker::cleanup_request(&f.e, &request)
        .await
        .unwrap();
    assert!(!runtime.path().join("container.json").exists());
    f.e.shutdown().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 Docker image and local daemon"]
async fn actual_container_fixture_compile_run_stdin_and_wrong_platform() {
    let f = actual();
    let mut stage = f.stage("fixtures", "");
    stage["kind"] = json!("test");
    stage["command"]["args"] = json!([]);
    stage["fixtures"] = json!({"adapter":{"name":"shell-copy","source_extension":"sh","compile":{"program":"{compiler}","args":["-c","cp \"$1\" \"$2\"; chmod +x \"$2\"","compile","{entry}","{output}"]},"run":{"program":"{output}","args":[]},"timeout_seconds":10,"capabilities":["compile_and_run"]},"suite":{"version":1,"suite":"container-stdin","adapter":"shell-copy","cases":[{"id":"echo","files":{"main.sh":"#!/bin/sh\ncat\n"},"stdin":"hello from stdin\n","execute":{"mode":"compile_and_run","entry":"main.sh"},"expect":{"compile":{"exit_code":0},"run":{"exit_code":0,"stdout":{"exact":"hello from stdin\n"}}}}]}});
    let r = f.start(vec![stage]);
    let result = f.finish(&r.id).await;
    assert_eq!(result.status, RunState::Succeeded);
    assert_eq!(result.stages[0].tests.as_ref().unwrap().counts.passed, 1);
    let jobs = f.jobs(&r.id);
    assert_eq!(
        jobs["jobs"].as_array().unwrap().len(),
        2,
        "compile and run are separate owned containers"
    );
    assert!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["state"] == "cleaned" && j["result"]["exit_code"] == 0)
    );
    let mut wrong = f.stage("wrong-platform", "echo must-not-run");
    wrong["container"]["platform"] = json!("linux/amd64");
    let r = f.start(vec![wrong]);
    assert_eq!(f.finish(&r.id).await.status, RunState::InfrastructureError);
    assert!(!common::output(&f.e, &r.id, "stdout").contains("must-not-run"));
    let jobs = f.jobs(&r.id);
    assert_eq!(jobs["jobs"][0]["state"], "cleanup_pending");
    // The operator fixture has observed the daemon reject the completed create request.
    preflight::docker::cleanup_request(
        &f.e,
        &preflight::docker::Cleanup {
            creation_settled: true,
            project_id: f.project.id.clone(),
            job_id: jobs["jobs"][0]["job_id"].as_str().unwrap().into(),
            request_id: "settled-platform-rejection".into(),
            actor: "operator-test".into(),
            reason: "daemon rejected platform; no outstanding create requests".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(f.jobs(&r.id)["jobs"][0]["state"], "cleaned");
    f.e.shutdown().await.unwrap();
}
#[cfg(unix)]
struct OwnedContainer {
    endpoint: String,
    runtime: PathBuf,
    cid: String,
    owner: String,
}
#[cfg(unix)]
impl Drop for OwnedContainer {
    fn drop(&mut self) {
        let output = std::process::Command::new(&self.runtime)
            .args(["--host", &self.endpoint])
            .args([
                "container",
                "inspect",
                "--format",
                "{{index .Config.Labels \"io.preflight.owner\"}}",
                &self.cid,
            ])
            .output();
        if output.is_ok_and(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == self.owner
        }) {
            let _ = std::process::Command::new(&self.runtime)
                .args(["--host", &self.endpoint])
                .args(["container", "rm", "--force", &self.cid])
                .output();
        }
    }
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 Docker image and local daemon"]
async fn actual_server_crash_quarantines_live_container_until_exact_cleanup() {
    let f = actual();
    let mut config = f.e.current_config();
    let project = f.project.clone();
    let stage = f.stage("crash", "echo ready; sleep 60");
    f.e.shutdown().await.unwrap();
    let Fixture { temp, e, .. } = f;
    drop(e);
    tokio::task::yield_now().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.bind = listener.local_addr().unwrap();
    drop(listener);
    let path = temp.path().join("server.toml");
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let mut process = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .arg("--config")
        .arg(&path)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{}", config.bind);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(process.try_wait().unwrap().is_none());
            if client
                .get(format!("{base}/ready"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let run:Run=client.post(format!("{base}/api/v1/pipeline-runs")).json(&json!({"project_id":project.id,"definition":{"version":1,"pipeline":{"stages":[stage]}}})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page: Value = client
                .get(format!("{base}/api/v1/runs/{}/logs", run.id))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if page["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["text"].as_str().unwrap().contains("ready"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    let c = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    let (cid, owner, endpoint): (String, String, String) = c
        .query_row(
            "SELECT container_id,owner_token,endpoint FROM container_jobs WHERE run_id=?1",
            [&run.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    drop(c);
    let guard = OwnedContainer {
        endpoint,
        runtime: config.docker_command.clone().unwrap(),
        cid,
        owner,
    };
    process.kill().await.unwrap();
    process.wait().await.unwrap();
    let output = std::process::Command::new(&guard.runtime)
        .args(["--host", &guard.endpoint])
        .args([
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            &guard.cid,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "true",
        "killing only the server must not be mistaken for daemon cleanup"
    );
    let e = Engine::open(config).unwrap();
    assert!(preflight::docker::admission_pending(&e.db).unwrap());
    assert_eq!(
        e.db.run(&run.id).unwrap().status,
        RunState::InfrastructureError
    );
    assert!(std::path::Path::new(&run.workspace).exists());
    let jobs = preflight::docker::list(
        &e.db,
        &preflight::docker::Query {
            project_id: Some(project.id.clone()),
            run_id: Some(run.id.clone()),
            before: None,
            limit: None,
            max_bytes: None,
        },
    )
    .unwrap();
    preflight::docker::cleanup_request(
        &e,
        &preflight::docker::Cleanup {
            creation_settled: false,
            project_id: project.id,
            job_id: jobs["jobs"][0]["job_id"].as_str().unwrap().into(),
            request_id: "after-crash".into(),
            actor: "test".into(),
            reason: "owned container survived server SIGKILL".into(),
        },
    )
    .await
    .unwrap();
    assert!(!preflight::docker::admission_pending(&e.db).unwrap());
    assert!(std::path::Path::new(&run.workspace).exists());
    assert!(common::output(&e, &run.id, "stdout").contains("ready"));
    e.shutdown().await.unwrap();
    drop(guard);
}
#[cfg(unix)]
#[tokio::test]
async fn docker_runtime_pin_is_rechecked_even_inside_the_host_temporary_directory() {
    let (runtime, path) = fake_runtime();
    std::fs::write(runtime.path().join("rotate-runtime"), "").unwrap();
    let f = Fixture::new(path, format!("sha256:{}", "a".repeat(64)));
    let run = f.start(vec![
        f.stage("build", "must not execute after runtime rotation"),
    ]);
    assert_eq!(
        f.finish(&run.id).await.status,
        RunState::InfrastructureError
    );
    assert!(
        !runtime.path().join("container.json").exists(),
        "changed runtime must not receive the create request"
    );
    assert_eq!(f.jobs(&run.id)["jobs"][0]["state"], "cleanup_pending");
    f.e.shutdown().await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn unknown_create_ack_cannot_release_a_lease_before_late_daemon_creation() {
    let (runtime, path) = fake_runtime();
    std::fs::write(runtime.path().join("uncertain-create"), "").unwrap();
    let f = Fixture::new(path, format!("sha256:{}", "a".repeat(64)));
    let run = f.start(vec![f.stage("build", "not started")]);
    assert_eq!(
        f.finish(&run.id).await.status,
        RunState::InfrastructureError
    );
    let jobs = f.jobs(&run.id);
    assert_eq!(jobs["jobs"][0]["state"], "cleanup_pending");
    assert!(
        jobs["jobs"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("creation_uncertain")
    );
    let request = preflight::docker::Cleanup {
        creation_settled: false,
        project_id: f.project.id.clone(),
        job_id: jobs["jobs"][0]["job_id"].as_str().unwrap().into(),
        request_id: "late-create".into(),
        actor: "test".into(),
        reason: "observe and clean exact late container".into(),
    };
    assert!(
        preflight::docker::cleanup_request(&f.e, &request)
            .await
            .is_err()
    );
    std::fs::rename(
        runtime.path().join("pending-container.json"),
        runtime.path().join("container.json"),
    )
    .unwrap();
    preflight::docker::cleanup_request(&f.e, &request)
        .await
        .unwrap();
    assert!(!runtime.path().join("container.json").exists());
    assert_eq!(f.jobs(&run.id)["jobs"][0]["state"], "cleaned");
    f.e.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected cached Linux ARM64 Docker image and local daemon"]
async fn actual_prepared_image_and_setup_supply_missing_tools() {
    let f = actual();
    struct OwnedImages(Vec<String>);
    impl Drop for OwnedImages {
        fn drop(&mut self) {
            for image in &self.0 {
                let _ = std::process::Command::new("/usr/local/bin/docker")
                    .args(["image", "rm", image])
                    .output();
            }
        }
    }
    let tag = format!("preflight-prepare-check:{}", f.project.id);
    assert!(
        std::process::Command::new("/usr/local/bin/docker")
            .args(["tag", &f.image, &tag])
            .status()
            .unwrap()
            .success()
    );
    let mut owned = OwnedImages(vec![tag.clone()]);
    std::fs::write(
        f.project.root.join("Dockerfile"),
        format!(
            "FROM {}\nRUN cp /bin/true /usr/local/bin/prepared-tool\n",
            tag
        ),
    )
    .unwrap();
    std::fs::write(f.project.root.join("activate.sh"), "mkdir -p tools\ncp /bin/true tools/setup-tool\nexport PATH=\"$PREFLIGHT_WORKSPACE/tools:$PATH\"\n").unwrap();
    let prepare = json!({"id":"prepare","command":{"program":"docker","args":["build","--network=none","--pull=false","--iidfile","image-id","."],"timeout_seconds":120},"outputs":{"image":{"path":"image-id","type":"file"}}});
    let mut test = f.stage(
        "test",
        "prepared-tool; setup-tool; test -f /.dockerenv; printf prepared-environment",
    );
    test["needs"] = json!(["prepare"]);
    test["container"].as_object_mut().unwrap().remove("image");
    test["container"]["image_from"] = json!("${artifact.prepare.image}");
    test["setup"] = json!([{"program":".","args":["./activate.sh"]}]);
    let run = f.start(vec![prepare, test]);
    let result = f.finish(&run.id).await;
    // Remove only the image produced by this owned fixture, including on assertion failure.
    let artifact =
        f.e.db
            .artifacts(&run.id)
            .unwrap()
            .into_iter()
            .find(|a| a.stage_id == "prepare" && a.name == "image");
    if let Some(artifact) = artifact {
        let image = std::fs::read_to_string(artifact.path).unwrap();
        owned.0.insert(0, image.trim().to_owned());
    }
    assert_eq!(result.status, RunState::Succeeded);
    assert!(common::output(&f.e, &run.id, "stdout").contains("prepared-environment"));
    assert_eq!(f.jobs(&run.id)["jobs"].as_array().unwrap().len(), 1);
}
