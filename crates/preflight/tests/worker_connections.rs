#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{engine::Engine, model::RegisterProject, worker_connections as wc};
use serde_json::{Value, json};
fn setup() -> (tempfile::TempDir, std::sync::Arc<Engine>, Value) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    let mut config = common::Fixture::config(&root);
    config.worker_bind = Some("127.0.0.1:4171".parse().unwrap());
    let engine = Engine::open(config).unwrap();
    let project = engine
        .register(RegisterProject {
            name: "test".into(),
            path: project.to_string_lossy().into(),
        })
        .unwrap();
    let request = json!({"request_id":"first","connection":{"project_id":project.id,"name":"linux","host":"worker.example","bundle_dir":root}});
    (temp, engine, request)
}
fn start(e: &std::sync::Arc<Engine>, r: &Value) -> anyhow::Result<Value> {
    wc::start(e, serde_json::from_value(r.clone())?)
}
fn query(v: &Value) -> wc::Query {
    serde_json::from_value(json!({"project_id":v["project_id"],"connection_id":v["connection_id"]}))
        .unwrap()
}
fn stop(e: &Engine, v: &Value, verified: bool) -> anyhow::Result<Value> {
    wc::stop(
        e,
        serde_json::from_value(
            json!({"project_id":v["project_id"],"connection_id":v["connection_id"],"actor":"operator","reason":"requested stop","process_stopped":verified}),
        )?,
    )
}
#[test]
fn idempotency_scope_cancellation_and_restart_recovery() {
    let (_temp, e, r) = setup();
    let first = start(&e, &r).unwrap();
    assert_eq!(first["state"], "queued");
    assert!(first.get("connection").is_none());
    assert_eq!(first, start(&e, &r).unwrap());
    let mut changed = r.clone();
    changed["connection"]["host"] = "other.example".into();
    assert!(start(&e, &changed).is_err());
    changed = r.clone();
    changed["request_id"] = "second".into();
    assert!(start(&e, &changed).is_err());
    let mut foreign = query(&first);
    foreign.project_id = uuid::Uuid::now_v7().to_string();
    assert!(wc::status(&e, &foreign).is_err());
    assert_eq!(stop(&e, &first, false).unwrap()["state"], "stopped");
    assert_eq!(start(&e, &r).unwrap()["state"], "stopped");
    let second = start(&e, &changed).unwrap();
    rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE worker_connections SET state='connected',launcher_pid=1 WHERE id=?1",
            [second["connection_id"].as_str().unwrap()],
        )
        .unwrap();
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    let recovered = wc::status(&e, &query(&second)).unwrap();
    assert_eq!(recovered["state"], "interrupted");
    assert!(recovered["launcher_pid"].is_null());
    assert!(stop(&e, &second, false).is_err());
    assert_eq!(stop(&e, &second, true).unwrap()["state"], "stopped");
    changed["request_id"] = "third".into();
    let third = start(&e, &changed).unwrap();
    let config = e.config.clone();
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(wc::status(&e, &query(&third)).unwrap()["state"], "failed");
}
#[tokio::test]
async fn failed_launcher_is_terminal_and_replay_never_restarts() {
    let (_temp, e, r) = setup();
    // Actual CLI subprocess, but the deliberately absent package prevents any host contact.
    let mut config = e.config.clone();
    config.worker_launcher = Some(env!("CARGO_BIN_EXE_preflight").into());
    // A local executable that exits immediately avoids any network/SSH authority in this test.
    config.ssh_command = Some("/usr/bin/false".into());
    drop(e);
    let e = Engine::open(config).unwrap();
    let first = start(&e, &r).unwrap();
    let task = tokio::spawn(wc::run(e.clone()));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = wc::status(&e, &query(&first)).unwrap();
            if value["state"] == "failed" {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["execution_ready"], false);
    assert!(result["launcher_pid"].is_null());
    assert!(result["reason"].as_str().unwrap().len() <= 1024);
    assert_eq!(start(&e, &r).unwrap(), result);
    e.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn mcp_and_http_share_connection_identity_and_stop_state() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let (_temp, e, mut r) = setup();
    r["reconnect"] = json!({"max_retries":2,"delay_seconds":5});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let signal = shutdown.clone();
    let router = preflight::api::router(e.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let response = client
        .call_tool(
            CallToolRequestParams::new("worker_ssh_start")
                .with_arguments(r.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let started: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let fetched: Value = http
        .get(format!(
            "http://{address}/api/v1/worker-connection?project_id={}&connection_id={}",
            started["project_id"].as_str().unwrap(),
            started["connection_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(started, fetched);
    assert_eq!(started["reconnect"]["max_retries"], 2);
    assert_eq!(started["reconnect"]["delay_seconds"], 5);
    let stopped:Value=http.post(format!("http://{address}/api/v1/worker-connections/stop")).json(&json!({"project_id":started["project_id"],"connection_id":started["connection_id"],"actor":"test","reason":"cancel pending"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(stopped["state"], "stopped");
    let response = client
        .call_tool(
            CallToolRequestParams::new("worker_ssh_list").with_arguments(
                json!({"project_id":started["project_id"],"limit":1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let listed: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(listed["connections"][0], stopped);
    client.cancel().await.unwrap();
    shutdown.cancel();
    server.await.unwrap();
    e.shutdown().await.unwrap();
}

#[path = "support/real_ssh.rs"]
mod real_ssh;
#[tokio::test]
async fn managed_install_connect_cancel_and_reconnect_keep_worker_identity() {
    managed_connect(false, false, false, false, false, false).await;
}
#[tokio::test]
#[ignore = "requires usable local OpenSSH sshd and ssh-keygen; run explicitly"]
async fn actual_ssh_host_key_tunnel_install_cancel_and_reconnect() {
    managed_connect(true, false, false, false, false, false).await;
}
#[tokio::test]
#[ignore = "requires usable local OpenSSH sshd and ssh-keygen; run explicitly"]
async fn actual_ssh_transport_loss_resumes_enrolled_worker() {
    managed_connect(true, true, false, false, false, false).await;
}
#[tokio::test]
#[ignore = "requires usable local OpenSSH sshd and ssh-keygen; run explicitly"]
async fn actual_ssh_active_job_loss_is_terminal_without_replay() {
    managed_connect(true, true, true, false, false, false).await;
}
#[tokio::test]
#[ignore = "requires usable local OpenSSH sshd and ssh-keygen; run explicitly"]
async fn actual_ssh_journal_recovery_restores_worker_without_replay() {
    managed_connect(true, true, true, true, false, false).await;
}
#[tokio::test]
async fn managed_connection_recovers_loss_with_same_worker_and_can_stop() {
    managed_connect(false, true, false, false, false, false).await;
}
#[tokio::test]
#[ignore = "requires usable local OpenSSH sshd and ssh-keygen; run explicitly"]
async fn actual_ssh_silent_partition_fences_job_without_replay() {
    managed_connect(true, true, true, false, true, false).await;
}
async fn managed_connect(
    actual_ssh: bool,
    reconnect: bool,
    active_loss: bool,
    recover_journal: bool,
    silent_partition: bool,
    container: bool,
) {
    use std::time::Duration;
    let (_temp, e, mut request) = setup();
    let root = e.config.allowed_roots[0].clone();
    let packages = root.join("packages");
    let bundle = packages.join(format!(
        "{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "package"])
        .arg(&bundle)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut ssh_server = actual_ssh.then(|| real_ssh::Server::new(&root));
    let relay = if silent_partition {
        Some(ssh_server.as_mut().unwrap().relay().await)
    } else {
        None
    };
    let shim = if let Some(server) = &ssh_server {
        server.command.clone()
    } else {
        let shim = root.join("ssh-test-transport");
        let compiled = std::process::Command::new("rustc")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/ssh.rs"))
            .arg("-o")
            .arg(&shim)
            .output()
            .unwrap();
        assert!(compiled.status.success());
        shim
    };
    let admin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = e.config.clone();
    config.bind = admin.local_addr().unwrap();
    config.worker_bind = Some(gateway.local_addr().unwrap());
    config.worker_launcher = Some(env!("CARGO_BIN_EXE_preflight").into());
    config.ssh_command = Some(shim);
    drop(e);
    let e = Engine::open(config).unwrap();
    if container {
        request["connection"]["runtime"] =
            json!({"docker_command":"/usr/local/bin/docker","max_memory_mib":512});
    }
    request["connection"]["bundle_dir"] = json!(packages);
    request["connection"]["remote_dir"] = json!(root.join("remote ' worker"));
    request["connection"]["remote_port"] = json!(e.config.worker_bind.unwrap().port());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let signal = shutdown.clone();
    let router = preflight::api::router(e.clone());
    let admin = tokio::spawn(async move {
        axum::serve(admin, router)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let signal = shutdown.clone();
    let router = preflight::workers::router(e.clone());
    let gateway = tokio::spawn(async move {
        axum::serve(gateway, router)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    if let Some(server) = &ssh_server {
        request["connection"]["host"] = json!("127.0.0.1");
        request["connection"]["port"] = json!(server.port);
        request["connection"]["remote_port"] = json!(real_ssh::free_port());
    }
    let manager = tokio::spawn(wc::run(e.clone()));
    if let Some(server) = &ssh_server {
        request["request_id"] = json!("untrusted-host");
        let started = start(&e, &request).unwrap();
        let failed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = wc::status(&e, &query(&started)).unwrap();
                if value["state"] == "failed" {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            failed["reason"]
                .as_str()
                .unwrap()
                .contains("Host key verification failed"),
            "{failed}"
        );
        assert!(!root.join("remote ' worker").exists());
        server.trust();
    }
    if reconnect {
        request["reconnect"] = json!({"max_retries":2,"delay_seconds":1});
    }
    let mut identity = None;
    let mut recovery_target = None;
    for attempt in 0..if reconnect { 1 } else { 2 } {
        request["request_id"] = json!(format!("attempt-{attempt}"));
        let started = start(&e, &request).unwrap();
        let connected = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let value = wc::status(&e, &query(&started)).unwrap();
                assert_ne!(value["state"], "failed", "{value}");
                if value["state"] == "connected" {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let connected = if reconnect {
            let interrupted = if active_loss {
                Some(start_interrupted_job(&e, &root, &connected, container).await)
            } else {
                None
            };
            let ledger =
                rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3")).unwrap();
            let issued_before: u64 = ledger
                .query_row("SELECT count(*) FROM worker_enrollments", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let pid = connected["launcher_pid"].as_i64().unwrap() as i32;
            if let Some(relay) = &relay {
                relay.pause();
                let began = tokio::time::Instant::now();
                let retry = tokio::time::timeout(Duration::from_secs(35), async {
                    loop {
                        let value = wc::status(&e, &query(&started)).unwrap();
                        if value["reconnect"]["attempt"].as_u64().unwrap() > 1 {
                            break value;
                        }
                        assert_ne!(value["state"], "failed", "{value}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .expect("SSH keepalive must detect the silent partition");
                assert_eq!(
                    unsafe { libc::kill(pid, 0) },
                    -1,
                    "the original launcher must be reaped before restoring traffic"
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                eprintln!(
                    "ssh_silent_partition_evidence {}",
                    json!({"connection_id":started["connection_id"],"retry_attempt":retry["reconnect"]["attempt"],"blackhole_ms":began.elapsed().as_millis(),"transport_signalled":false,"original_launcher_reaped":true})
                );
                relay.resume();
            } else if let Some(server) = &ssh_server {
                server.disconnect_transport(pid);
            } else {
                // The transport double's launcher also belongs exclusively to this test.
                assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
            }
            let resumed = tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    let value = wc::status(&e, &query(&started)).unwrap();
                    if !active_loss {
                        assert_ne!(value["state"], "failed", "{value}");
                    }
                    if (value["state"] == "connected"
                        || (active_loss && value["requires_recovery"] == true))
                        && value["reconnect"]["attempt"].as_u64().unwrap() >= 2
                    {
                        break value;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            let issued_after: u64 = ledger
                .query_row("SELECT count(*) FROM worker_enrollments", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                issued_before, issued_after,
                "resume must not mint a new enrollment token"
            );
            assert_eq!(resumed["worker_id"], connected["worker_id"]);
            assert_ne!(resumed["launcher_pid"], connected["launcher_pid"]);
            assert_eq!(resumed["connection_id"], connected["connection_id"]);
            if let Some((run, job, journal)) = interrupted {
                recovery_target = Some((run.clone(), job.clone(), journal.clone()));
                let ended = common::finished(&e, &run).await;
                assert_eq!(
                    ended.status,
                    preflight::model::RunState::InfrastructureError,
                    "{ended:?}"
                );
                let jobs = e.db.worker_jobs(&run).unwrap();
                assert_eq!(jobs["jobs"].as_array().unwrap().len(), 1);
                assert_eq!(jobs["jobs"][0]["job_id"], job);
                assert!(e.db.artifacts(&run).unwrap().is_empty());
                let local = preflight::db::Db::new(&journal).unwrap();
                assert!(local.run(&job).unwrap().status.terminal());
                assert_eq!(
                    local
                        .logs(&job, 0)
                        .unwrap()
                        .events
                        .iter()
                        .map(|e| e.text.as_str())
                        .collect::<String>()
                        .matches("active-ssh-loss-started")
                        .count(),
                    1,
                    "an interrupted command must never be replayed"
                );
                eprintln!(
                    "ssh_active_loss_evidence {}",
                    json!({"run_id":run,"job_id":job,"status":ended.status,"job_state":jobs["jobs"][0]["state"],"command_executions":1,"published_artifacts":0})
                );
            }
            if actual_ssh {
                eprintln!(
                    "ssh_resume_evidence {}",
                    json!({"connection_id":resumed["connection_id"],"worker_id":resumed["worker_id"],"attempt":resumed["reconnect"]["attempt"],"new_enrollment_tokens":issued_after-issued_before,"same_identity":true})
                );
            }
            resumed
        } else {
            connected
        };
        assert_eq!(connected["execution_ready"], !active_loss);
        if attempt == 0 {
            std::fs::write(root.join("project/input.txt"), b"verified SSH payload\n").unwrap();
            std::fs::write(root.join("project/prepare.sh"), format!(
                "{}\nworkspace-tool\nprintf '#!/bin/sh\\ncat input.txt > result.txt\\n' > .tools/bin/prepared-copy\nchmod +x .tools/bin/prepared-copy\n",
                include_str!("support/workspace-tools.sh")
            )).unwrap();
            let mut definition = json!({
                "project_id":connected["project_id"],"definition":{"version":1,"pipeline":{"stages":[{
                    "id":"remote-copy","worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"worker_id":connected["worker_id"]},
                    "setup":[{"program":".","args":["./prepare.sh"]}],
                    "command":{"program":"prepared-copy","args":[],"timeout_seconds":5},
                    "outputs":{"result":{"path":"result.txt","type":"file"}}
                }]}}
            });
            if container {
                let stage = &mut definition["definition"]["pipeline"]["stages"][0];
                stage["executor"] = json!("docker");
                stage["container"] = json!({"image":std::env::var("PREFLIGHT_TEST_DOCKER_IMAGE").unwrap(),"platform":"linux/arm64"});
            }
            let run = e
                .start_pipeline(serde_json::from_value(definition).unwrap())
                .unwrap();
            let finished = common::finished(&e, &run.id).await;
            assert_eq!(
                finished.status,
                if active_loss {
                    preflight::model::RunState::InfrastructureError
                } else {
                    preflight::model::RunState::Succeeded
                },
                "{finished:?}"
            );
            if active_loss {
                assert!(e.db.artifacts(&run.id).unwrap().is_empty());
                assert!(
                    e.db.worker_jobs(&run.id).unwrap()["jobs"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                let status = wc::status(&e, &query(&started)).unwrap();
                assert_eq!(status["requires_recovery"], true);
                assert_eq!(status["execution_ready"], false);
                assert!(status["blocked_operations"].as_u64().unwrap() > 0);
                let listed =
                    e.db.workers(&preflight::workers::List {
                        project_id: connected["project_id"].as_str().unwrap().into(),
                        after: None,
                    })
                    .unwrap();
                assert_eq!(listed["workers"][0]["execution_ready"], false);
                assert_eq!(listed["workers"][0]["requires_recovery"], true);
                assert_recovery_contracts(&e, &started).await;
                eprintln!(
                    "ssh_recovery_readiness_evidence {}",
                    json!({"worker_id":connected["worker_id"],"requires_recovery":true,"execution_ready":false,"blocked_operations":status["blocked_operations"],"new_job_enqueued":false})
                );
            } else {
                let artifacts = e.db.artifacts(&run.id).unwrap();
                assert_eq!(artifacts.len(), 1);
                assert_eq!(
                    std::fs::read(&artifacts[0].path).unwrap(),
                    b"verified SSH payload\n"
                );
                let jobs = e.db.worker_jobs(&run.id).unwrap();
                assert_eq!(jobs["jobs"][0]["state"], "succeeded");
                assert!(jobs["jobs"][0]["outputs_tree_sha256"].is_string());
                if actual_ssh {
                    eprintln!(
                        "ssh_execution_evidence {}",
                        json!({"run_id":run.id,"worker_id":connected["worker_id"],"status":"succeeded","output_verified":true,"outputs_tree_sha256":jobs["jobs"][0]["outputs_tree_sha256"]})
                    );
                }
            }
        }

        if actual_ssh && !active_loss && attempt == 0 {
            assert_ssh_command_timeout(&e, &root, &connected).await;
        }

        if let Some(id) = &identity {
            assert_eq!(&connected["worker_id"], id);
        } else {
            identity = Some(connected["worker_id"].clone());
        }
        assert_eq!(
            start(&e, &request).unwrap()["connection_id"],
            started["connection_id"]
        );
        let pid = connected["launcher_pid"].as_i64().map(|v| v as i32);
        let state = stop(&e, &started, false).unwrap()["state"]
            .as_str()
            .unwrap()
            .to_owned();
        // An interrupted journal can exhaust reconnects before the operator stops it.
        // A failed launcher is already terminal and stop must remain idempotent.
        let already_failed = active_loss && state == "failed";
        assert!(
            already_failed || matches!(state.as_str(), "stopping" | "stopped"),
            "{state}"
        );
        let stopped = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = wc::status(&e, &query(&started)).unwrap();
                if value["state"] == "stopped" || (already_failed && value["state"] == "failed") {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        if !already_failed {
            assert_eq!(stopped["actor"], "operator");
        } else {
            assert!(stopped["finished_at"].is_number());
            assert!(stopped["launcher_pid"].is_null());
        }
        assert_eq!(stopped["execution_ready"], false);
        // Signal zero observes existence only; never sends a signal to a persisted PID.
        if let Some(pid) = pid {
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
    if recover_journal {
        restore_resolved_journal(&e, &root, &request, recovery_target.unwrap()).await;
    }
    e.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), manager)
        .await
        .unwrap()
        .unwrap();
    shutdown.cancel();
    admin.await.unwrap();
    gateway.await.unwrap();
}

async fn assert_ssh_command_timeout(
    e: &std::sync::Arc<Engine>,
    root: &std::path::Path,
    connected: &Value,
) {
    use preflight::model::RunState;
    let marker = root.join("timeout-descendant-survived");
    let worker = json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"worker_id":connected["worker_id"]});
    let run = e.start_pipeline(serde_json::from_value(json!({
        "project_id":connected["project_id"],"definition":{"version":1,"pipeline":{"stages":[{
            "id":"timeout","worker":worker,
            "command":{"program":"sh","args":["-c","printf ssh-timeout-started; printf partial > result.txt; (sleep 5; printf survived > \"$1\") & wait","preflight-timeout",marker],"timeout_seconds":1},
            "outputs":{"partial":{"path":"result.txt","type":"file"}}
        }]}}
    })).unwrap()).unwrap();
    let ended = common::finished(e, &run.id).await;
    assert_eq!(ended.status, RunState::TimedOut, "{ended:?}");
    assert_eq!(ended.stages[0].status, RunState::TimedOut);
    assert!(e.db.artifacts(&run.id).unwrap().is_empty());
    let jobs = e.db.worker_jobs(&run.id).unwrap();
    let jobs = jobs["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["state"], "timed_out");
    let logs =
        e.db.logs(&run.id, 0)
            .unwrap()
            .events
            .into_iter()
            .map(|event| event.text)
            .collect::<String>();
    assert_eq!(logs.matches("ssh-timeout-started").count(), 1);
    let next = e.start_pipeline(serde_json::from_value(json!({
        "project_id":connected["project_id"],"definition":{"version":1,"pipeline":{"stages":[{
            "id":"after-timeout","worker":worker,
            "command":{"program":"sh","args":["-c","printf after-timeout > result.txt"],"timeout_seconds":5},
            "outputs":{"result":{"path":"result.txt","type":"file"}}
        }]}}
    })).unwrap()).unwrap();
    let next = common::finished(e, &next.id).await;
    assert_eq!(next.status, RunState::Succeeded, "{next:?}");
    let artifacts = e.db.artifacts(&next.id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(std::fs::read(&artifacts[0].path).unwrap(), b"after-timeout");
    // Observe beyond the descendant's delayed write, even if SSH finished quickly.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert!(
        !marker.exists(),
        "timed-out descendant survived process-group cleanup"
    );
    let listed =
        e.db.workers(&preflight::workers::List {
            project_id: connected["project_id"].as_str().unwrap().into(),
            after: None,
        })
        .unwrap();
    let current = listed["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["worker_id"] == connected["worker_id"])
        .unwrap();
    assert_eq!(current["requires_recovery"], false);
    assert_eq!(current["execution_ready"], true);
    eprintln!(
        "ssh_timeout_evidence {}",
        json!({"run_id":run.id,"job_id":jobs[0]["job_id"],"status":"timed_out","command_executions":1,"published_artifacts":0,"descendant_survived":false,"same_worker":connected["worker_id"],"next_run":next.id,"next_status":"succeeded"})
    );
}

#[test]
fn reconnect_policy_validation_idempotency_and_restart_are_explicit() {
    let (_temp, e, mut r) = setup();
    for policy in [
        json!({"max_retries":0}),
        json!({"max_retries":11}),
        json!({"max_retries":1,"delay_seconds":0}),
        json!({"max_retries":1,"delay_seconds":301}),
    ] {
        r["reconnect"] = policy;
        assert!(start(&e, &r).is_err());
    }
    r["reconnect"] = json!({"max_retries":1});
    r["connection"]["once"] = json!(true);
    assert!(start(&e, &r).is_err());
    r["connection"]["once"] = json!(false);
    let started = start(&e, &r).unwrap();
    assert_eq!(started["reconnect"]["delay_seconds"], 10);
    assert_eq!(start(&e, &r).unwrap(), started);
    r["reconnect"]["max_retries"] = json!(2);
    assert!(start(&e, &r).is_err());
    rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE worker_connections SET next_retry_at=?2,retries=1,attempt=1 WHERE id=?1",
            rusqlite::params![started["connection_id"].as_str().unwrap(), i64::MAX],
        )
        .unwrap();
    let config = e.config.clone();
    drop(e);
    let reopened = Engine::open(config).unwrap();
    let status = wc::status(&reopened, &query(&started)).unwrap();
    assert_eq!(status["state"], "failed");
    assert!(status["reconnect"]["next_retry_at"].is_null());
    assert_eq!(status["reconnect"]["retries"], 1);
}

#[tokio::test]
async fn bounded_retries_exhaust_and_stop_cancels_backoff() {
    use std::time::Duration;
    let (_temp, e, mut r) = setup();
    let mut config = e.config.clone();
    config.worker_launcher = Some(env!("CARGO_BIN_EXE_preflight").into());
    config.ssh_command = Some("/usr/bin/false".into());
    drop(e);
    let e = Engine::open(config).unwrap();
    r["reconnect"] = json!({"max_retries":2,"delay_seconds":1});
    let started = start(&e, &r).unwrap();
    let manager = tokio::spawn(wc::run(e.clone()));
    let failed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = wc::status(&e, &query(&started)).unwrap();
            if status["state"] == "failed" {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failed["reconnect"]["attempt"], 3);
    assert_eq!(failed["reconnect"]["retries"], 2);
    assert!(failed["reconnect"]["next_retry_at"].is_null());
    assert!(failed["reconnect"]["last_error"].is_string());
    assert_eq!(start(&e, &r).unwrap(), failed);
    r["request_id"] = json!("stop-backoff");
    r["reconnect"]["delay_seconds"] = json!(300);
    let pending = start(&e, &r).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = wc::status(&e, &query(&pending)).unwrap();
            if status["reconnect"]["next_retry_at"].is_number() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let stopped = stop(&e, &pending, false).unwrap();
    assert_eq!(stopped["state"], "stopped");
    assert!(stopped["reconnect"]["next_retry_at"].is_null());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        wc::status(&e, &query(&pending)).unwrap()["reconnect"]["attempt"],
        1
    );
    e.shutdown().await.unwrap();
    manager.await.unwrap();
}

async fn start_interrupted_job(
    e: &std::sync::Arc<Engine>,
    root: &std::path::Path,
    connected: &Value,
    container: bool,
) -> (String, String, std::path::PathBuf) {
    let mut definition = json!({"project_id":connected["project_id"],"definition":{"version":1,"pipeline":{"stages":[{
        "id":"interrupted","worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"worker_id":connected["worker_id"]},
        "command":{"program":"sh","args":["-c","echo active-ssh-loss-started; sleep 60; echo must-not-publish > result.txt"],"timeout_seconds":90},
        "outputs":{"result":{"path":"result.txt","type":"file"}}
    }]}}});
    if container {
        let stage = &mut definition["definition"]["pipeline"]["stages"][0];
        stage["executor"] = json!("docker");
        stage["container"] = json!({"image":std::env::var("PREFLIGHT_TEST_DOCKER_IMAGE").unwrap(),"platform":"linux/arm64"});
    }
    let run = e
        .start_pipeline(serde_json::from_value(definition).unwrap())
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let jobs = e.db.worker_jobs(&run.id).unwrap();
            if let Some(job) = jobs["jobs"].as_array().unwrap().first() {
                let id = job["job_id"].as_str().unwrap();
                let path = root
                    .join("remote ' worker/state/jobs")
                    .join(format!("{id}.sqlite3"));
                if path.exists() {
                    let db = preflight::db::Db::new(&path).unwrap();
                    if db.logs(id, 0).is_ok_and(|p| {
                        p.events
                            .iter()
                            .any(|e| e.text.contains("active-ssh-loss-started"))
                    }) {
                        return (run.id.clone(), id.to_owned(), path);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("remote command must actually produce output before transport loss")
}

async fn assert_recovery_contracts(e: &Engine, connection: &Value) {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let project = connection["project_id"].as_str().unwrap();
    let id = connection["connection_id"].as_str().unwrap();
    let base = format!("http://{}", e.config.bind);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let rest: Value = http
        .get(format!(
            "{base}/api/v1/worker-connection?project_id={project}&connection_id={id}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rest["requires_recovery"], true);
    assert_eq!(rest["execution_ready"], false);
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{base}/mcp"
        )))
        .await
        .unwrap();
    for (tool, args) in [
        (
            "worker_ssh_status",
            json!({"project_id":project,"connection_id":id}),
        ),
        ("workers_list", json!({"project_id":project})),
    ] {
        let response = client
            .call_tool(
                CallToolRequestParams::new(tool.to_owned())
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true));
        let value: Value =
            serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
        let state = if tool == "workers_list" {
            value["workers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["worker_id"] == rest["worker_id"])
                .unwrap()
        } else {
            &value
        };
        assert_eq!(state["requires_recovery"], true);
        assert_eq!(state["execution_ready"], false);
        assert_eq!(state["blocked_operations"], rest["blocked_operations"]);
    }
    client.cancel().await.unwrap();
}

async fn restore_resolved_journal(
    e: &std::sync::Arc<Engine>,
    root: &std::path::Path,
    request: &Value,
    target: (String, String, std::path::PathBuf),
) {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let (old_run, job, journal) = target;
    let project = request["connection"]["project_id"].as_str().unwrap();
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{}/mcp",
            e.config.bind
        )))
        .await
        .unwrap();
    async fn call(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
        tool: &str,
        args: Value,
    ) -> Value {
        let response = client
            .call_tool(
                CallToolRequestParams::new(tool.to_owned())
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap()
    }
    let query = json!({"project_id":project,"job_id":job});
    let before = call(&client, "worker_recovery_status", query.clone()).await;
    assert_eq!(before["recovery_allowed"], false);
    let mut recovery = request.clone();
    recovery["request_id"] = json!("recover-journal");
    recovery["connection"]["recovery"] = json!({"job_id":job,"process_stopped":true});
    assert!(
        start(e, &recovery).is_err(),
        "unresolved server protection must forbid SSH recovery"
    );
    let mut resolve = json!({"project_id":project,"lease_id":before["lease_id"],"request_id":"job-journal-recovery","actor":"qa-operator","reason":"verified native job terminal and old SSH launcher stopped","process_stopped":false});
    let rejected = client
        .call_tool(
            CallToolRequestParams::new("operation_resolve")
                .with_arguments(resolve.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(rejected.is_error, Some(true));
    resolve["process_stopped"] = json!(true);
    call(&client, "operation_resolve", resolve).await;
    let allowed = call(&client, "worker_recovery_status", query).await;
    assert_eq!(allowed["recovery_allowed"], true);
    let http: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/api/v1/worker-recovery?project_id={project}&job_id={job}",
            e.config.bind
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(http, allowed);
    let ledger = rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3")).unwrap();
    let tokens = || {
        ledger
            .query_row("SELECT count(*) FROM worker_enrollments", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap()
    };
    let issued = tokens();
    let started = call(&client, "worker_ssh_start", recovery.clone()).await;
    assert_eq!(started["worker_id"], before["worker_id"]);
    assert_eq!(
        call(&client, "worker_ssh_start", recovery).await["connection_id"],
        started["connection_id"]
    );
    let connected = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let state = wc::status(e, &query_connection(&started)).unwrap();
            assert_ne!(state["state"], "failed", "{state}");
            if state["execution_ready"] == true {
                return state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(connected["worker_id"], before["worker_id"]);
    assert_eq!(tokens(), issued);
    let state = root.join("remote ' worker/state");
    let receipt = state.join("recoveries").join(format!("{job}.json"));
    assert!(receipt.is_file());
    std::fs::write(root.join("project/input.txt"), b"after journal recovery").unwrap();
    let next=e.start_pipeline(serde_json::from_value(json!({"project_id":project,"definition":{"version":1,"pipeline":{"stages":[{"id":"after-recovery","worker":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"worker_id":before["worker_id"]},"command":{"program":"sh","args":["-c","cat input.txt > result.txt"],"timeout_seconds":5},"outputs":{"result":{"path":"result.txt","type":"file"}}}]}}})).unwrap()).unwrap();
    assert_eq!(
        common::finished(e, &next.id).await.status,
        preflight::model::RunState::Succeeded
    );
    assert_eq!(
        std::fs::read(&e.db.artifacts(&next.id).unwrap()[0].path).unwrap(),
        b"after journal recovery"
    );
    assert_eq!(
        e.db.run(&old_run).unwrap().status,
        preflight::model::RunState::InfrastructureError
    );
    assert_eq!(
        e.db.worker_jobs(&old_run).unwrap()["jobs"][0]["state"],
        "lost"
    );
    let local = preflight::db::Db::new(&journal).unwrap();
    assert_eq!(
        local
            .logs(&job, 0)
            .unwrap()
            .events
            .iter()
            .map(|e| e.text.as_str())
            .collect::<String>()
            .matches("active-ssh-loss-started")
            .count(),
        1
    );
    stop(e, &started, false).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while wc::status(e, &query_connection(&started)).unwrap()["state"] != "stopped" {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(state.join("client.lock"))
                .unwrap();
            if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
                drop(file);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the actual remote worker must release its state lock after SSH stop");
    let identity: Value =
        serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap()).unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "recover", identity["gateway"].as_str().unwrap()])
        .arg(&state)
        .args([&job, "--process-stopped"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replay: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(replay["already_recovered"], true);
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(identity["credential"].as_str().unwrap())
    );
    eprintln!(
        "ssh_journal_recovery_evidence {}",
        json!({"old_run":old_run,"job_id":job,"new_run":next.id,"worker_id":before["worker_id"],"old_job":"lost","new_run_status":"succeeded","new_enrollments":tokens()-issued,"evidence_retained":true,"cli_replay":true,"mcp_http_parity":true})
    );
    client.cancel().await.unwrap();
}
fn query_connection(value: &Value) -> wc::Query {
    serde_json::from_value(
        json!({"project_id":value["project_id"],"connection_id":value["connection_id"]}),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires local OpenSSH sshd and explicitly cached ARM64 Docker image"]
async fn actual_ssh_container_setup_install_cancel_and_cleanup() {
    managed_connect(true, false, false, false, false, true).await;
}

#[tokio::test]
#[ignore = "requires local OpenSSH sshd and explicitly cached ARM64 Docker image"]
async fn actual_ssh_container_loss_and_journal_recovery_never_replay() {
    managed_connect(true, true, true, true, false, true).await;
}
