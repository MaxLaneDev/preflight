use preflight::{
    codex_bridge::{self, Bind, BindingId},
    config::Config,
    engine::Engine,
    model::*,
};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
struct Fixture {
    temp: tempfile::TempDir,
    engine: Arc<Engine>,
    command: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        static BINARY: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
        let binary = &BINARY
            .get_or_init(|| {
                let tmp = tempfile::tempdir().unwrap();
                let path = tmp.path().join("queue.exe");
                let output = std::process::Command::new("rustc")
                    .arg(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/support/codex_queue.rs"
                    ))
                    .arg("-o")
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                (tmp, path)
            })
            .1;
        let temp = tempfile::tempdir().unwrap();
        let command = temp.path().join("queue.exe");
        std::fs::copy(binary, &command).unwrap();
        std::fs::write(temp.path().join("mode"), mode).unwrap();
        let engine = Engine::open(Config {
            release_repository: None,
            version: 1,
            bind: "127.0.0.1:4170".parse().unwrap(),
            data_dir: temp.path().join("data"),
            allowed_roots: vec![temp.path().to_path_buf()],
            storage_policies: vec![],
            max_active_runs: 32,
            max_parallel_commands: 2,
            ssh_command: None,
            worker_launcher: None,
            worker_bind: None,
            memory_budget_mib: None,
            min_free_disk_bytes: None,
            max_storage_mib: None,
            admin_token_file: None,
            docker_command: None,
            access_tokens: Vec::new(),
            ci_sources: Vec::new(),
            default_stage_memory_mib: 256,
            codex_command: Some(command.clone()),
            codex_delivery_mode: match mode {
                "desktop-queued" => Some(codex_bridge::DeliveryMode::Queued),
                "desktop-immediate" => Some(codex_bridge::DeliveryMode::Immediate),
                _ => None,
            },
            codex_ipc_socket: mode
                .starts_with("desktop-")
                .then(|| temp.path().join("ipc.sock")),
        })
        .unwrap();
        let project = engine.db.register("p", temp.path()).unwrap();
        engine
            .db
            .insert_run(
                "selected-run",
                &StartBuild {
                    project_id: project.id,
                    command: CommandSpec {
                        program: "tool".into(),
                        args: vec![],
                        timeout_seconds: 5,
                    },
                },
                &temp.path().join("run"),
            )
            .unwrap();
        Self {
            temp,
            engine,
            command,
        }
    }
    fn bind(&self) -> serde_json::Value {
        codex_bridge::bind(
            &self.engine,
            &Bind {
                run_id: "selected-run".into(),
                thread_id: "01a17f49-a837-7553-a343-bf597124dae1".into(),
            },
        )
        .unwrap()
    }
    fn complete(&self) {
        self.engine
            .db
            .transition("selected-run", RunState::Succeeded, Some(0), None, None)
            .unwrap();
    }
}
#[tokio::test]
async fn hook_waits_for_completion_queues_once_and_preserves_arguments() {
    let f = Fixture::new("");
    let binding = f.bind();
    assert_eq!(binding, f.bind());
    assert!(
        codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .is_none()
    );
    f.complete();
    let sent = codex_bridge::dispatch_once(&f.engine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent["state"], "delivered");
    assert_eq!(sent["attempts"], 1);
    assert!(
        codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .is_none()
    );
    let calls = std::fs::read_to_string(f.temp.path().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(calls.contains("01a17f49-a837-7553-a343-bf597124dae1"));
    assert!(calls.contains("selected-run"));
    assert!(calls.contains("delivery_id"));
    assert!(calls.contains(binding["binding_id"].as_str().unwrap()));
    assert!(!calls.contains("--resume"));
    assert_eq!(f.bind()["state"], "delivered");
}

#[cfg(unix)]
async fn desktop_mock(f: &Fixture, mode: &'static str) -> tokio::task::JoinHandle<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    std::fs::set_permissions(f.temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = f.temp.path().join("ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = f.temp.path().to_path_buf();
    tokio::spawn(async move {
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut source = String::new();
        for ordinal in 0..3 {
            let count = stream.read_u32_le().await.unwrap();
            let mut bytes = vec![0; count as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let result = match ordinal {
                0 => {
                    assert_eq!(request["params"]["clientType"], "preflight");
                    serde_json::json!({"clientId":"sender"})
                }
                1 => {
                    assert_eq!(request["method"], "thread-owner-discovery");
                    assert_eq!(
                        request["params"]["conversationId"],
                        "01a17f49-a837-7553-a343-bf597124dae1"
                    );
                    serde_json::json!({"supportsUntrustedAppInput":true})
                }
                _ => {
                    assert_eq!(request["method"], "thread-follower-steer-turn");
                    assert_eq!(request["targetClientId"], "owner");
                    let p = &request["params"];
                    assert_eq!(p["input"][0]["text_elements"], serde_json::json!([]));
                    assert!(
                        p["input"][0]["text"]
                            .as_str()
                            .unwrap()
                            .contains("without pausing or replacing")
                    );
                    assert_eq!(
                        p["additionalContext"]["preflight_completion"]["name"],
                        "preflight_completion"
                    );
                    assert_eq!(p["conversationId"], "01a17f49-a837-7553-a343-bf597124dae1");
                    assert_eq!(p["clientUserMessageId"], p["restoreMessage"]["id"]);
                    assert_eq!(
                        p["additionalContext"]["preflight_completion"]["kind"],
                        "untrusted"
                    );
                    assert!(p.get("model").is_none() && p.get("serviceTier").is_none());
                    assert!(
                        p["additionalContext"]["preflight_completion"]["value"]
                            .as_str()
                            .unwrap()
                            .contains("selected-run")
                    );
                    std::fs::write(
                        root.join("native-call"),
                        serde_json::to_vec(&request).unwrap(),
                    )
                    .unwrap();
                    if mode == "desktop-lost" {
                        return;
                    }
                    serde_json::json!({"result":{"turnId":"active-turn"}})
                }
            };
            let mut response = serde_json::json!({"type":"response","requestId":request["requestId"],"method":request["method"],"resultType":"success","handledByClientId":if ordinal==0 {"sender"}else{"owner"},"result":result});
            if (mode == "desktop-no-owner" && ordinal == 1)
                || (mode == "desktop-idle" && ordinal == 2)
                || (mode == "desktop-rejected" && ordinal == 2)
            {
                response["resultType"] = serde_json::json!("error");
                response["error"] = serde_json::json!(match mode {
                    "desktop-no-owner" => "no-client-found",
                    "desktop-idle" =>
                        "Cannot steer conversation 01a17f49-a837-7553-a343-bf597124dae1 because its active turn already ended",
                    _ => "request-timeout",
                });
            }
            if ["desktop-wrong-owner", "desktop-wrong-idle"].contains(&mode) && ordinal == 2 {
                response["handledByClientId"] = serde_json::json!("other-owner");
                if mode == "desktop-wrong-idle" {
                    response["resultType"] = serde_json::json!("error");
                    response["error"] = serde_json::json!(
                        "Cannot steer conversation 01a17f49-a837-7553-a343-bf597124dae1 because its active turn already ended"
                    );
                }
            }
            let bytes = serde_json::to_vec(&response).unwrap();
            stream.write_u32_le(bytes.len() as u32).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
            if mode == "desktop-no-owner" && ordinal == 1 {
                return;
            }
            if ordinal == 0 {
                source = "sender".into();
            } else {
                assert_eq!(request["sourceClientId"], source);
            }
        }
    })
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_queue_mode_bypasses_desktop_and_never_resumes() {
    let f = Fixture::new("desktop-queued");
    f.bind();
    f.complete();
    let result = codex_bridge::dispatch_once(&f.engine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["state"], "delivered");
    let calls = std::fs::read_to_string(f.temp.path().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(!calls.contains("--resume"));
    assert_eq!(
        codex_bridge::status(&f.engine, "selected-run").unwrap()["delivery_policy"]["effective_mode"],
        "queued"
    );
    assert!(
        codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_immediate_mode_steers_or_requests_idle_resume_without_replaying_uncertain() {
    for mode in [
        "desktop-active",
        "desktop-idle",
        "desktop-no-owner",
        "desktop-lost",
        "desktop-wrong-idle",
    ] {
        let f = Fixture::new("desktop-immediate");
        f.bind();
        let mock = desktop_mock(&f, mode).await;
        f.complete();
        let result = codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .unwrap();
        mock.await.unwrap();
        let uncertain = ["desktop-lost", "desktop-wrong-idle"].contains(&mode);
        assert_eq!(
            result["state"],
            if uncertain { "uncertain" } else { "delivered" },
            "{mode}"
        );
        let calls = std::fs::read_to_string(f.temp.path().join("calls")).unwrap_or_default();
        let idle = ["desktop-idle", "desktop-no-owner"].contains(&mode);
        assert_eq!(!calls.is_empty(), idle, "{mode}");
        assert_eq!(calls.contains("--resume"), idle, "{mode}");
        assert!(
            codex_bridge::dispatch_once(&f.engine)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn immediate_mode_requires_desktop_transport_and_rejects_unknown_modes() {
    let f = Fixture::new("");
    let mut config = f.engine.current_config();
    config.codex_delivery_mode = Some(codex_bridge::DeliveryMode::Immediate);
    assert!(config.validate_memory().is_err());
    config.codex_delivery_mode = Some(codex_bridge::DeliveryMode::Queued);
    config.validate_memory().unwrap();
    let mut value = serde_json::to_value(config).unwrap();
    value["codex_delivery_mode"] = serde_json::json!("invalid");
    assert!(serde_json::from_value::<Config>(value).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_steers_exact_owner_once_preserves_idle_and_never_replays_unknown_delivery() {
    for mode in [
        "desktop-active",
        "desktop-idle",
        "desktop-no-owner",
        "desktop-lost",
        "desktop-wrong-owner",
        "desktop-wrong-idle",
        "desktop-rejected",
    ] {
        let f = Fixture::new(mode);
        let binding = f.bind();
        let mock = desktop_mock(&f, mode).await;
        f.complete();
        let result = codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .unwrap();
        mock.await.unwrap();
        let uncertain = [
            "desktop-lost",
            "desktop-wrong-owner",
            "desktop-wrong-idle",
            "desktop-rejected",
        ]
        .contains(&mode);
        assert_eq!(
            result["state"],
            if uncertain { "uncertain" } else { "delivered" },
            "{mode}"
        );
        let queued = std::fs::read_to_string(f.temp.path().join("calls")).unwrap_or_default();
        assert_eq!(
            !queued.is_empty(),
            ["desktop-idle", "desktop-no-owner"].contains(&mode),
            "{mode}"
        );
        assert!(!queued.contains("--resume"));
        if mode != "desktop-no-owner" {
            let call = std::fs::read_to_string(f.temp.path().join("native-call")).unwrap();
            assert!(call.contains(binding["binding_id"].as_str().unwrap()));
        }
        assert!(
            codex_bridge::dispatch_once(&f.engine)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.engine
                .db
                .retry_codex(&BindingId {
                    binding_id: binding["binding_id"].as_str().unwrap().into()
                })
                .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_rejects_public_socket_without_sending_or_queueing() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("desktop-insecure");
    let path = f.temp.path().join("ipc.sock");
    let _listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    f.bind();
    f.complete();
    let result = codex_bridge::dispatch_once(&f.engine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["state"], "uncertain");
    assert!(!f.temp.path().join("calls").exists());
    assert!(!f.temp.path().join("native-call").exists());
}

#[cfg(unix)]
#[test]
fn desktop_socket_setting_is_persisted_and_hot_reloaded_by_admin_api() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("");
    let token = "local-settings-test-token-at-least-32-characters";
    let credential = f.temp.path().join("credential");
    std::fs::write(&credential, token).unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = f.engine.current_config();
    config.data_dir = f.temp.path().join("settings-data");
    config.admin_token_file = Some(credential);
    let path = f.temp.path().join("settings.toml");
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let engine = Engine::open(Config::load(&path).unwrap()).unwrap();
    engine.config_file.set(path.clone()).unwrap();
    let read = preflight::settings::request(
        &engine,
        preflight::settings::Request {
            admin_token: token.into(),
            expected_revision: None,
            configuration: None,
        },
    )
    .unwrap();
    let mut update = read["saved"].clone();
    let socket = f.temp.path().join("ipc.sock");
    update["codex_ipc_socket"] = serde_json::json!(socket);
    update["codex_delivery_mode"] = serde_json::json!("immediate");
    let saved = preflight::settings::request(
        &engine,
        preflight::settings::Request {
            admin_token: token.into(),
            expected_revision: Some(read["revision"].as_str().unwrap().into()),
            configuration: Some(update),
        },
    )
    .unwrap();
    assert_eq!(saved["restart_required"], false);
    assert_eq!(saved["reload_required"], false);
    assert_eq!(
        engine.current_config().codex_ipc_socket,
        Some(socket.clone())
    );
    assert_eq!(Config::load(&path).unwrap().codex_ipc_socket, Some(socket));
    assert_eq!(
        engine.current_config().codex_delivery_mode,
        Some(codex_bridge::DeliveryMode::Immediate)
    );
    let mut queued = saved["saved"].clone();
    queued["codex_delivery_mode"] = serde_json::json!("queued");
    let queued = preflight::settings::request(
        &engine,
        preflight::settings::Request {
            admin_token: token.into(),
            expected_revision: Some(saved["revision"].as_str().unwrap().into()),
            configuration: Some(queued),
        },
    )
    .unwrap();
    assert_eq!(queued["reload_required"], false);
    assert_eq!(
        Config::load(&path).unwrap().codex_delivery_mode,
        Some(codex_bridge::DeliveryMode::Queued)
    );
    assert_eq!(
        preflight::control::capabilities(&engine)["features"]["codex_delivery_policy"]["effective_mode"],
        "queued"
    );
}
#[tokio::test]
async fn failed_spawn_is_retryable_but_unknown_acceptance_is_not() {
    let f = Fixture::new("");
    let binding = f.bind();
    f.complete();
    let saved = std::fs::read(&f.command).unwrap();
    let permissions = std::fs::metadata(&f.command).unwrap().permissions();
    std::fs::remove_file(&f.command).unwrap();
    let failed = codex_bridge::dispatch_once(&f.engine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed["state"], "failed");
    let id = BindingId {
        binding_id: binding["binding_id"].as_str().unwrap().into(),
    };
    f.engine.db.retry_codex(&id).unwrap();
    std::fs::write(&f.command, saved).unwrap();
    std::fs::set_permissions(&f.command, permissions).unwrap();
    std::fs::write(f.temp.path().join("mode"), "fail").unwrap();
    let unknown = codex_bridge::dispatch_once(&f.engine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unknown["state"], "uncertain");
    assert!(f.engine.db.retry_codex(&id).is_err());
    assert!(
        codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn disable_prevents_delivery_and_shutdown_kills_only_queue_child() {
    let f = Fixture::new("sleep");
    let binding = f.bind();
    let id = BindingId {
        binding_id: binding["binding_id"].as_str().unwrap().into(),
    };
    f.engine.db.disable_codex(&id).unwrap();
    f.complete();
    assert!(
        codex_bridge::dispatch_once(&f.engine)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!f.temp.path().join("calls").exists());
    let f = Fixture::new("sleep");
    f.bind();
    f.complete();
    let engine = f.engine.clone();
    let delivery = tokio::spawn(async move { codex_bridge::dispatch_once(&engine).await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !f.temp.path().join("calls").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.engine.shutdown().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), delivery)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["state"], "uncertain");
    assert!(!f.temp.path().join("unexpected_finish").exists());
}

#[tokio::test]
async fn hook_mcp_and_rest_share_binding_and_delivery_status() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    use serde_json::{Value, json};
    let f = Fixture::new("");
    f.complete();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let args = json!({"run_id":"selected-run","thread_id":"01a17f49-a837-7553-a343-bf597124dae1"});
    let response = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_bind")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let bound: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(bound["state"], "pending");
    let repeated = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_bind")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(response.content, repeated.content);
    codex_bridge::dispatch_once(&f.engine).await.unwrap();
    let status = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_status").with_arguments(
                json!({"run_id":"selected-run"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(status.is_error, Some(true));
    let status: Value = serde_json::from_str(&status.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(status["bindings"][0]["state"], "delivered");
    let rest: Value = reqwest::Client::new()
        .get(format!(
            "http://{address}/api/v1/codex-hooks?run_id=selected-run"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status, rest);
    let disabled = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_disable").with_arguments(
                json!({"binding_id":bound["binding_id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(disabled.is_error, Some(true));
    let disabled: Value =
        serde_json::from_str(&disabled.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(disabled["state"], "delivered");
    let retry = client
        .call_tool(
            CallToolRequestParams::new("codex_hook_retry").with_arguments(
                json!({"binding_id":bound["binding_id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(retry.is_error, Some(true));
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
