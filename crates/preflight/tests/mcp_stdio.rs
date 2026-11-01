#[allow(dead_code)]
mod common;
use common::*;
use preflight::model::RunState;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

fn child(endpoint: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"));
    command
        .args(["mcp-proxy", "--server", endpoint])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[cfg(unix)]
#[test]
fn review_setup_produces_valid_patches_and_real_compiler_evidence_without_overwrite() {
    use sha2::{Digest, Sha256};
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = root.join("integrations/codex/review/setup.sh");
    let destination = temp.path().join("review with spaces");
    let prepared = Command::new("sh")
        .arg(&script)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let inputs: Value =
        serde_json::from_slice(&std::fs::read(destination.join("inputs.json")).unwrap()).unwrap();
    let project = destination.join("project");
    let original = std::fs::read(project.join("main.c")).unwrap();
    for name in ["pipeline.json", "cancellation-pipeline.json"] {
        let definition: preflight::pipeline::PipelineFile =
            serde_json::from_slice(&std::fs::read(destination.join(name)).unwrap()).unwrap();
        preflight::pipeline::validate(&definition).unwrap();
    }
    let baseline = Command::new("git")
        .arg("-C")
        .arg(&project)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(
        String::from_utf8(baseline.stdout).unwrap().trim(),
        inputs["base_commit"].as_str().unwrap()
    );
    for (patch, expected) in inputs["patches"]
        .as_array()
        .unwrap()
        .iter()
        .zip([Some("review-a"), Some("review-b")])
        .chain(std::iter::once((&inputs["compiler_error_patch"], None)))
    {
        let bytes = std::fs::read(patch["path"].as_str().unwrap()).unwrap();
        assert_eq!(
            preflight::hashing::hex(Sha256::digest(&bytes)),
            patch["sha256"].as_str().unwrap()
        );
        let applied = Command::new("git")
            .arg("-C")
            .arg(&project)
            .arg("apply")
            .arg(patch["path"].as_str().unwrap())
            .output()
            .unwrap();
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let binary = destination.join("fixture-program");
        let built = Command::new("cc")
            .arg(project.join("main.c"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        if let Some(expected) = expected {
            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
            let result = Command::new(&binary).output().unwrap();
            assert!(result.status.success());
            assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), expected);
        } else {
            assert!(!built.status.success());
            assert!(
                String::from_utf8_lossy(&built.stderr).contains("preflight_review_unknown_symbol")
            );
        }
        std::fs::write(project.join("main.c"), &original).unwrap();
    }
    let refused = Command::new("sh")
        .arg(script)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(std::fs::read(project.join("main.c")).unwrap(), original);
    let status = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.status.success() && status.stdout.is_empty());
}

#[cfg(any(
    all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")),
    all(
        target_arch = "x86_64",
        any(target_os = "macos", target_os = "linux", target_os = "windows")
    )
))]
#[tokio::test]
async fn unpacked_stdio_plugin_uses_bundled_binary_without_path_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = preflight::distribution::run(&[
        "create".into(),
        "--server".into(),
        env!("CARGO_BIN_EXE_preflight").into(),
        "--destination".into(),
        temp.path().join("server-package").to_str().unwrap().into(),
        "--repository".into(),
        repository.to_str().unwrap().into(),
    ])
    .await
    .unwrap();
    let zip = temp.path().join("plugin.zip");
    preflight::distribution::run(&[
        "plugin-stdio".into(),
        package[0]["archive"].as_str().unwrap().into(),
        "--sha256".into(),
        package[0]["sha256"].as_str().unwrap().into(),
        "--destination".into(),
        zip.to_str().unwrap().into(),
        "--repository".into(),
        repository.to_str().unwrap().into(),
    ])
    .await
    .unwrap();
    let installed = temp.path().join("clean plugin installation");
    zip::ZipArchive::new(std::fs::File::open(zip).unwrap())
        .unwrap()
        .extract(&installed)
        .unwrap();
    let mcp: Value =
        serde_json::from_slice(&std::fs::read(installed.join("mcp.json")).unwrap()).unwrap();
    let entry = &mcp["mcpServers"]["preflight"];
    // Agent Plugins resolves contained ./ commands from the plugin root.
    let executable = installed.join(
        entry["command"]
            .as_str()
            .unwrap()
            .strip_prefix("./")
            .unwrap(),
    );
    let args: Vec<_> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();

    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let empty_path = temp.path().join("empty PATH");
    std::fs::create_dir(&empty_path).unwrap();
    let mut proxy = tokio::process::Command::new(executable)
        .args(args)
        .args(["--server", &endpoint])
        .current_dir(&installed)
        .env("PATH", &empty_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client =
        ().serve((proxy.stdout.take().unwrap(), proxy.stdin.take().unwrap()))
            .await
            .unwrap();
    let available = client.list_all_tools().await.unwrap();
    let plugin: Value =
        serde_json::from_slice(&std::fs::read(installed.join("plugin.json")).unwrap()).unwrap();
    for case in plugin["extensions"]["com.openai"]["review"]["test_cases"]["positive"]
        .as_array()
        .unwrap()
    {
        for name in case["tools_triggered"]
            .as_str()
            .unwrap()
            .split(',')
            .map(str::trim)
        {
            assert!(
                available.iter().any(|tool| tool.name == name),
                "review case references absent tool {name}"
            );
        }
    }
    let capabilities = client
        .call_tool(CallToolRequestParams::new("server_capabilities"))
        .await
        .unwrap();
    assert_ne!(capabilities.is_error, Some(true));
    let projects = client
        .call_tool(CallToolRequestParams::new("projects_list"))
        .await
        .unwrap();
    assert_ne!(projects.is_error, Some(true));
    client.cancel().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), proxy.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!installed.join("data").exists());
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stdio_backend_failure_exits_while_parent_keeps_stdin_open() {
    use axum::{http::StatusCode, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = axum::Router::new().route("/mcp", post(|| async { StatusCode::UNAUTHORIZED }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mut proxy = child(&endpoint).spawn().unwrap();
    let mut stdin = proxy.stdin.take().unwrap();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw-parent","version":"1"}}});
    stdin
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), proxy.wait())
        .await
        .expect("proxy waited for stdin EOF after backend failure")
        .unwrap();
    assert!(!status.success());
    drop(stdin);
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn stdio_uses_existing_engine_tools_cancellation_and_clean_eof() {
    let f = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mut proxy = child(&endpoint).current_dir(f.temp.path()).spawn().unwrap();
    let stdout = proxy.stdout.take().unwrap();
    let stdin = proxy.stdin.take().unwrap();
    let client = ().serve((stdout, stdin)).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "pipeline_cancel"));
    for tool in &tools {
        let hints = tool
            .annotations
            .as_ref()
            .expect("missing effect annotations");
        assert!(hints.read_only_hint.is_some(), "{}", tool.name);
        assert!(hints.destructive_hint.is_some(), "{}", tool.name);
        assert!(hints.open_world_hint.is_some(), "{}", tool.name);
    }
    let direct = ().serve(StreamableHttpClientTransport::from_uri(endpoint.clone())).await.unwrap();
    assert_eq!(
        serde_json::to_value(&tools).unwrap(),
        serde_json::to_value(direct.list_all_tools().await.unwrap()).unwrap()
    );
    direct.cancel().await.unwrap();
    let (first, second) = tokio::join!(
        client.call_tool(CallToolRequestParams::new("server_capabilities")),
        client.call_tool(CallToolRequestParams::new("projects_list")),
    );
    assert_ne!(first.unwrap().is_error, Some(true));
    assert_ne!(second.unwrap().is_error, Some(true));
    let started = client
        .call_tool(
            CallToolRequestParams::new("build_start").with_arguments(
                serde_json::to_value(f.request(&["sleep"], 30))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(started.is_error, Some(true), "{started:?}");
    let run: Value = serde_json::from_str(&started.content[0].as_text().unwrap().text).unwrap();
    let id = run["run_id"]
        .as_str()
        .or_else(|| run["id"].as_str())
        .unwrap();
    let cancelled = client
        .call_tool(
            CallToolRequestParams::new("pipeline_cancel").with_arguments(
                json!({"run_id":id,"actor":"agent","reason":"stdio cancellation"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(cancelled.is_error, Some(true), "{cancelled:?}");
    assert_eq!(finished(&f.engine, id).await.status, RunState::Cancelled);
    client.cancel().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), proxy.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!f.temp.path().join("data").join("preflight.toml").exists());
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stdio_forwards_credentials_and_never_replays_expired_session_mutations() {
    use axum::{
        Json,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let initializations = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicBool::new(false));
    let early_calls = Arc::new(AtomicUsize::new(0));
    let secret = "stdio-fixture-credential-private-value";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let (observed, initialized, became_ready, early) = (
        calls.clone(),
        initializations.clone(),
        ready,
        early_calls.clone(),
    );
    let app = axum::Router::new().route("/mcp", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let (calls, initializations, ready, early) = (observed.clone(), initialized.clone(), became_ready.clone(), early.clone());
        async move {
            if headers.get("authorization").and_then(|v|v.to_str().ok()) != Some("Bearer stdio-fixture-credential-private-value") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            match body["method"].as_str().unwrap() {
                "initialize" => {
                    initializations.fetch_add(1, Ordering::SeqCst);
                    ([("mcp-session-id","fixture-session")], Json(json!({"jsonrpc":"2.0","id":body["id"],"result":{"protocolVersion":body["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}))).into_response()
                }
                "notifications/initialized" => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    ready.store(true, Ordering::SeqCst);
                    StatusCode::ACCEPTED.into_response()
                },
                "tools/call" => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if !ready.load(Ordering::SeqCst) {
                        early.fetch_add(1, Ordering::SeqCst);
                        return StatusCode::BAD_REQUEST.into_response();
                    }
                    StatusCode::NOT_FOUND.into_response()
                }
                _ => StatusCode::ACCEPTED.into_response(),
            }
        }
    }));
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mut proxy = child(&endpoint)
        .args(["--token-env", "PF_STDIO_TEST_TOKEN"])
        .env("PF_STDIO_TEST_TOKEN", secret)
        .spawn()
        .unwrap();
    let client =
        ().serve((proxy.stdout.take().unwrap(), proxy.stdin.take().unwrap()))
            .await
            .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(CallToolRequestParams::new("mutation")),
    )
    .await
    .unwrap();
    assert!(result.is_err() || result.unwrap().is_error == Some(true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(initializations.load(Ordering::SeqCst), 1);
    assert_eq!(early_calls.load(Ordering::SeqCst), 0);
    let _ = client.cancel().await;
    let output = tokio::time::timeout(Duration::from_secs(10), proxy.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    stop.cancel();
    server.await.unwrap();
}
