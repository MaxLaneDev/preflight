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
    assert_eq!(f.bind()["state"], "delivered");
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
