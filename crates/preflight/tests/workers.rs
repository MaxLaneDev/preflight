#[allow(dead_code)]
mod common;
use common::*;
use preflight::workers;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn workers_cli_connect_reconnect_auth_and_gateway_isolation() {
    let f = Fixture::new();
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_base = format!("http://{}", admin_listener.local_addr().unwrap());
    let admin_stop = tokio_util::sync::CancellationToken::new();
    let admin_shutdown = admin_stop.clone();
    let admin_app = preflight::api::router(f.engine.clone());
    let admin_server = tokio::spawn(async move {
        axum::serve(admin_listener, admin_app)
            .with_graceful_shutdown(admin_shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let mcp = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{admin_base}/mcp"
        )))
        .await
        .unwrap();
    let response = mcp.call_tool(CallToolRequestParams::new("worker_enrollment_create").with_arguments(json!({"project_id":f.project.id,"name":"local-worker","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"ttl_seconds":60}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true));
    let issued: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let state = f.temp.path().join("worker state");
    let mut ids = Vec::new();
    for attempt in 0..2 {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
            .args([
                "worker",
                "connect",
                &base,
                state.to_str().unwrap(),
                "--once",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if attempt == 0 {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("{}\n", issued["enrollment_token"].as_str().unwrap()).as_bytes())
                .await
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["authenticated_heartbeat"], true);
        assert_eq!(report["execution_ready"], true);
        ids.push(report["worker_id"].clone());
        assert!(
            !String::from_utf8_lossy(&result.stdout)
                .contains(issued["enrollment_token"].as_str().unwrap())
        );
    }
    assert_eq!(ids[0], ids[1]);
    let listed = f
        .engine
        .db
        .workers(&workers::List {
            project_id: f.project.id.clone(),
            after: None,
        })
        .unwrap();
    assert_eq!(listed["workers"].as_array().unwrap().len(), 1);
    assert_eq!(listed["workers"][0]["online"], true);
    let response = mcp
        .call_tool(
            CallToolRequestParams::new("workers_list").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let listed_mcp: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(listed_mcp["workers"][0]["execution_ready"], true);
    assert_eq!(listed_mcp["workers"][0]["worker_id"], ids[0]);

    let identity: Value =
        serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap()).unwrap();
    assert_eq!(identity["sequence"], 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(state.join("identity.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/projects"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .post(format!("{base}/mcp"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .post(format!("{base}/worker/v1/heartbeat"))
            .bearer_auth("bad")
            .json(&json!({"protocol":1,"sequence":3}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{base}/worker/v1/heartbeat"))
            .header("Origin", "http://evil.example")
            .bearer_auth(identity["credential"].as_str().unwrap())
            .json(&json!({"protocol":1,"sequence":3}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    std::fs::create_dir_all(f.project.root.join("src/empty")).unwrap();
    std::fs::write(f.project.root.join("src/code"), "original snapshot").unwrap();
    let definition:preflight::pipeline::PipelineFile=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"build","command":f.request(&[],5).command}]}})).unwrap();
    let run = f
        .engine
        .start_pipeline(preflight::pipeline::StartPipeline {
            project_id: f.project.id.clone(),
            definition: Some(definition),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        preflight::model::RunState::Succeeded
    );
    std::fs::write(f.project.root.join("src/code"), "changed project").unwrap();
    let response = mcp
        .call_tool(
            CallToolRequestParams::new("worker_inputs_prepare").with_arguments(
                json!({"run_id":run.id,"worker_id":ids[0],"generation":1,"ttl_seconds":60})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    let transfer: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let other_enrollment = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "other-worker".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let other_credential = workers::secret().unwrap();
    f.engine
        .db
        .worker_enroll(&workers::Enroll {
            protocol: 1,
            enrollment_token: other_enrollment["enrollment_token"]
                .as_str()
                .unwrap()
                .into(),
            credential: other_credential.clone(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        })
        .unwrap();
    assert_eq!(
        client
            .get(format!(
                "{base}/worker/v1/transfers/{}",
                transfer["transfer_id"].as_str().unwrap()
            ))
            .bearer_auth(&other_credential)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(
        preflight::worker_transfers::prepare(
            f.engine.clone(),
            preflight::worker_transfers::Prepare {
                run_id: run.id.clone(),
                worker_id: ids[0].as_str().unwrap().into(),
                generation: 2,
                ttl_seconds: 60
            }
        )
        .await
        .is_err()
    );
    let fetch_args = [
        "worker",
        "fetch",
        state.to_str().unwrap(),
        transfer["transfer_id"].as_str().unwrap(),
        transfer["tree_sha256"].as_str().unwrap(),
    ];
    let fetched = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(fetch_args)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        fetched.status.success(),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let received: Value = serde_json::from_slice(&fetched.stdout).unwrap();
    let received_path = std::path::Path::new(received["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(received_path.join("src/code")).unwrap(),
        "original snapshot"
    );
    assert!(received_path.join("src/empty").is_dir());
    let operations = f.engine.db.operations(&run.id).unwrap();
    assert!(
        operations["operations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "worker_transfer_read")
            .all(|v| v["state"] == "released")
    );
    let c = rusqlite::Connection::open(f.temp.path().join("data/preflight.sqlite3")).unwrap();
    // Exercise the real worker process before corrupting the immutable transfer.
    for (case, args, timeout, expected) in [
        ("success", vec![], 5, "succeeded"),
        ("failure", vec!["fail"], 5, "failed"),
        ("timeout", vec!["sleep"], 1, "timed_out"),
        ("cancel", vec!["sleep"], 15, "cancelled"),
        // This checks bounded output, not host disk throughput for hundreds of durable log writes.
        (
            "bounded-output",
            vec!["flood"],
            if cfg!(windows) { 30 } else { 5 },
            "succeeded",
        ),
        ("returned-file", vec!["write"], 5, "succeeded"),
        #[cfg(unix)]
        ("hangup", vec!["sleep"], 15, "cancelled"),
        #[cfg(unix)]
        ("terminate", vec!["sleep"], 15, "cancelled"),
    ] {
        let target = uuid::Uuid::now_v7().to_string();
        let request = f.request(&args, timeout);
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
                &preflight::worker_jobs::Spec {
                    container: None,
                    command: request.command,
                    cwd: ".".into(),
                    env: Default::default(),
                    returns: if case == "returned-file" {
                        std::collections::BTreeMap::from([(
                            "result.txt".into(),
                            preflight::pipeline::ArtifactKind::File,
                        )])
                    } else {
                        Default::default()
                    },
                },
            )
            .unwrap();
        let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
            .args([
                "worker",
                "connect",
                &base,
                state.to_str().unwrap(),
                "--job-once",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if matches!(case, "cancel" | "hangup" | "terminate") {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    let journal: Value = serde_json::from_slice(
                        &std::fs::read(state.join("identity.json")).unwrap(),
                    )
                    .unwrap();
                    if journal["pending"]["phase"] == "running" {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("worker started native execution");
            if case == "cancel" {
                f.engine
                    .db
                    .request_cancel(&target, "test", "cancel running worker job")
                    .unwrap();
            } else {
                #[cfg(unix)]
                assert_eq!(
                    unsafe {
                        libc::kill(
                            child.id().unwrap() as i32,
                            if case == "hangup" {
                                libc::SIGHUP
                            } else {
                                libc::SIGTERM
                            },
                        )
                    },
                    0
                );
            }
        }
        let exit_deadline = if case == "bounded-output" && cfg!(windows) {
            37
        } else {
            12
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(exit_deadline),
            child.wait_with_output(),
        )
        .await
        .expect("worker exits promptly")
        .unwrap();
        assert!(
            result.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let raw: String = c
            .query_row(
                "SELECT result_json FROM worker_jobs WHERE id=?1",
                [job["job_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        let outcome: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            f.engine.db.worker_jobs(&target).unwrap()["jobs"][0]["state"],
            expected,
            "{case}: {raw}"
        );

        assert_eq!(outcome["status"], expected, "{case}");
        if case == "returned-file" {
            let directory: String = c
                .query_row(
                    "SELECT directory FROM worker_outputs WHERE job_id=?1",
                    [job["job_id"].as_str().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            let returned = f.temp.path().join("data/worker-outputs").join(directory);
            assert_eq!(
                std::fs::read_to_string(returned.join("result.txt")).unwrap(),
                "workspace only"
            );
            assert_eq!(std::fs::read_dir(returned).unwrap().count(), 1);
        }

        let response = mcp
            .call_tool(
                CallToolRequestParams::new("runs_worker_jobs")
                    .with_arguments(json!({"run_id": target}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        let mcp_jobs: Value =
            serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
        let rest_jobs: Value = client
            .get(format!("{admin_base}/api/v1/worker-jobs?run_id={target}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(mcp_jobs, rest_jobs);
        assert_eq!(mcp_jobs["jobs"][0]["state"], expected);
        if case == "returned-file" {
            let digest: String = c
                .query_row(
                    "SELECT tree_sha256 FROM worker_outputs WHERE job_id=?1",
                    [job["job_id"].as_str().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(mcp_jobs["jobs"][0]["outputs_tree_sha256"], digest);
        } else {
            assert!(mcp_jobs["jobs"][0]["outputs_tree_sha256"].is_null());
        }

        assert!(mcp_jobs["jobs"][0].get("claim_secret").is_none());
        assert!(mcp_jobs["jobs"][0].get("spec").is_none());

        if case == "success" {
            assert!(
                common::output(&f.engine, &target, "stdout").contains("Preflight helper: stdout")
            );
            assert!(outcome["log_cursor"].as_i64().unwrap() > 0);
            assert_eq!(outcome["stdout"], "");
        }
        if case == "failure" {
            assert_eq!(outcome["exit"], 7);
        }
        if case == "bounded-output" {
            assert_eq!(outcome["truncated"], true);
            assert!(outcome["stdout"].as_str().unwrap().len() <= 4096);
        }
        let journal: Value =
            serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap()).unwrap();
        assert!(journal.get("pending").is_none());
        assert!(
            f.engine.db.operations(&target).unwrap()["operations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["state"] == "released")
        );
    }
    // Authenticate uploads and verify bytes before accepting any job completion.
    let target = uuid::Uuid::now_v7().to_string();
    let request = f.request(&[], 30);
    f.engine
        .db
        .insert_run(&target, &request, &f.project.root)
        .unwrap();
    let returns = std::collections::BTreeMap::from([(
        "result.txt".into(),
        preflight::pipeline::ArtifactKind::File,
    )]);
    let job = f
        .engine
        .db
        .worker_job_enqueue(
            &target,
            "native",
            transfer["transfer_id"].as_str().unwrap(),
            &preflight::worker_jobs::Spec {
                container: None,
                command: request.command,
                cwd: ".".into(),
                env: Default::default(),
                returns: returns.clone(),
            },
        )
        .unwrap();
    let claim_secret = workers::secret().unwrap();
    let credential = identity["credential"].as_str().unwrap();
    f.engine
        .db
        .worker_job_claim(
            credential,
            &preflight::worker_jobs::Claim {
                protocol: preflight::worker_jobs::PROTOCOL,
                claim_secret: claim_secret.clone(),
            },
        )
        .unwrap();
    let id = job["job_id"].as_str().unwrap();
    let finish = preflight::worker_jobs::Finish {
        protocol: preflight::worker_jobs::PROTOCOL,
        job_id: id.into(),
        claim_secret: claim_secret.clone(),
        outcome: preflight::worker_jobs::Outcome {
            runtime_receipt: None,
            log_cursor: None,
            status: preflight::model::RunState::Succeeded,
            exit: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            truncated: false,
        },
    };
    assert!(f.engine.db.worker_job_finish(credential, &finish).is_err());
    let produced = tempfile::tempdir_in(f.temp.path()).unwrap();
    std::fs::write(produced.path().join("result.txt"), "verified output").unwrap();
    let (bundle, digest) =
        preflight::worker_outputs::pack(produced.path(), &returns, f.temp.path()).unwrap();
    let bytes = std::fs::read(bundle.path()).unwrap();
    let upload = |body: Vec<u8>, digest: String, claim: String, credential: String| {
        client
            .put(format!("{base}/worker/v1/jobs/{id}/outputs"))
            .bearer_auth(credential)
            .header("x-preflight-claim", claim)
            .header("x-preflight-tree-sha256", digest)
            .body(body)
            .send()
    };
    assert!(
        !upload(
            bytes.clone(),
            digest.clone(),
            workers::secret().unwrap(),
            credential.into()
        )
        .await
        .unwrap()
        .status()
        .is_success()
    );
    assert!(
        !upload(
            bytes.clone(),
            digest.clone(),
            claim_secret.clone(),
            other_credential.clone()
        )
        .await
        .unwrap()
        .status()
        .is_success()
    );
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        !upload(
            corrupt,
            digest.clone(),
            claim_secret.clone(),
            credential.into()
        )
        .await
        .unwrap()
        .status()
        .is_success()
    );
    std::fs::write(produced.path().join("private.txt"), "not allowed").unwrap();
    let (extra, extra_digest) = preflight::worker_outputs::pack(
        produced.path(),
        &std::collections::BTreeMap::from([(
            "private.txt".into(),
            preflight::pipeline::ArtifactKind::File,
        )]),
        f.temp.path(),
    )
    .unwrap();
    assert!(
        !upload(
            std::fs::read(extra.path()).unwrap(),
            extra_digest,
            claim_secret.clone(),
            credential.into()
        )
        .await
        .unwrap()
        .status()
        .is_success()
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM worker_outputs WHERE job_id=?1",
            [id],
            |row| row.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    for _ in 0..2 {
        assert!(
            upload(
                bytes.clone(),
                digest.clone(),
                claim_secret.clone(),
                credential.into()
            )
            .await
            .unwrap()
            .status()
            .is_success()
        );
    }
    std::fs::write(produced.path().join("result.txt"), "different bytes").unwrap();
    let (changed, changed_digest) =
        preflight::worker_outputs::pack(produced.path(), &returns, f.temp.path()).unwrap();
    assert!(
        !upload(
            std::fs::read(changed.path()).unwrap(),
            changed_digest,
            claim_secret.clone(),
            credential.into()
        )
        .await
        .unwrap()
        .status()
        .is_success()
    );
    assert_eq!(
        f.engine.db.worker_job_finish(credential, &finish).unwrap()["state"],
        "succeeded"
    );
    assert!(
        !upload(bytes, digest, claim_secret, credential.into())
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let directory: String = c
        .query_row(
            "SELECT directory FROM worker_transfers WHERE id=?1",
            [transfer["transfer_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let bundle = f
        .temp
        .path()
        .join("data/worker-transfers")
        .join(directory)
        .join("bundle");
    let mut bytes = std::fs::read(&bundle).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&bundle, bytes).unwrap();
    let before = std::fs::read_dir(&state).unwrap().count();
    let corrupted = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(fetch_args)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!corrupted.status.success());
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), before);
    c.execute(
        "UPDATE worker_transfers SET expires_at=0 WHERE id=?1",
        [transfer["transfer_id"].as_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        client
            .get(format!(
                "{base}/worker/v1/transfers/{}",
                transfer["transfer_id"].as_str().unwrap()
            ))
            .bearer_auth(identity["credential"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let source = run.workspace.join("source/src/code");
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, "tampered snapshot").unwrap();
    assert!(
        preflight::worker_transfers::prepare(
            f.engine.clone(),
            preflight::worker_transfers::Prepare {
                run_id: run.id.clone(),
                worker_id: ids[0].as_str().unwrap().into(),
                generation: 1,
                ttl_seconds: 60
            }
        )
        .await
        .is_err()
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM worker_transfers", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        std::fs::read_dir(f.temp.path().join("data/worker-transfers"))
            .unwrap()
            .count(),
        1
    );
    drop(c);
    let response = mcp
        .call_tool(
            CallToolRequestParams::new("workers_list").with_arguments(
                json!({"project_id":f.project.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let mcp_list: Value =
        serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let rest_list: Value = client
        .get(format!(
            "{admin_base}/api/v1/workers?project_id={}",
            f.project.id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mcp_list, rest_list);
    let response = mcp.call_tool(CallToolRequestParams::new("worker_revoke").with_arguments(json!({"project_id":f.project.id,"worker_id":ids[0],"generation":1,"actor":"test","reason":"acceptance complete"}).as_object().unwrap().clone())).await.unwrap();
    assert_ne!(response.is_error, Some(true));
    assert_eq!(
        client
            .post(format!("{base}/worker/v1/heartbeat"))
            .bearer_auth(identity["credential"].as_str().unwrap())
            .json(&json!({"protocol":1,"sequence":3}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    mcp.cancel().await.unwrap();
    admin_stop.cancel();
    admin_server.await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
