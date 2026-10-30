#[allow(dead_code)]
mod common;
use preflight::auth::{Role, Token};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    base: String,
    http: reqwest::Client,
    config: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    async fn response(
        &self,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> reqwest::Response {
        let request = if let Some(body) = body {
            self.http.post(format!("{}{path}", self.base)).json(&body)
        } else {
            self.http.get(format!("{}{path}", self.base))
        };
        let request = if let Some(token) = token {
            request.bearer_auth(token)
        } else {
            request
        };
        request.send().await.unwrap()
    }
    async fn value(&self, path: &str, body: Option<Value>, token: &str) -> Value {
        let response = self.response(path, body, Some(token)).await;
        assert!(
            response.status().is_success(),
            "HTTP status {}",
            response.status()
        );
        response.json().await.unwrap()
    }
    async fn finished(&self, id: &str, token: &str) -> Value {
        for _ in 0..200 {
            let run = self.value(&format!("/api/v1/runs/{id}"), None, token).await;
            if !run["finished_at"].is_null() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("owned acceptance command did not complete");
    }
}
#[tokio::test]
async fn actual_roles_sessions_cli_reload_redaction_and_audit_share_one_boundary() {
    let fixture = common::Fixture::new();
    let root = fixture.temp.path().canonicalize().unwrap();
    let project = fixture.project.id.clone();
    let project_root = fixture.project.root.clone();
    let mut config = fixture.engine.config.clone();
    fixture.engine.shutdown().await.unwrap();
    drop(fixture.engine);
    let code = root.join("access-helper.rs");
    std::fs::write(&code,r#"fn main(){let a=std::env::args().skip(1).collect::<Vec<_>>();if a.first().is_some_and(|v|v=="wait"){std::thread::sleep(std::time::Duration::from_secs(2));}println!("{}",a.get(1).map(String::as_str).unwrap_or("marker"));}"#).unwrap();
    let helper = project_root.join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    assert!(
        Command::new("rustc")
            .arg(code)
            .arg("-o")
            .arg(&helper)
            .status()
            .unwrap()
            .success()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.bind = listener.local_addr().unwrap();
    drop(listener);
    config.access_tokens = vec![
        Token {
            name: "observer".into(),
            token_env: "PF_TEST_READ".into(),
            role: Role::ReadOnly,
        },
        Token {
            name: "runner".into(),
            token_env: "PF_TEST_RUN".into(),
            role: Role::Execute,
        },
        Token {
            name: "operator".into(),
            token_env: "PF_TEST_ADMIN".into(),
            role: Role::Admin,
        },
    ];
    let configuration = root.join("preflight.toml");
    std::fs::write(&configuration, toml::to_string(&config).unwrap()).unwrap();
    let read = "a".repeat(48);
    let execute = "b".repeat(48);
    let admin = "c".repeat(48);
    let settings = "d".repeat(48);
    let log = std::fs::File::create(root.join("server.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["--config"])
        .arg(&configuration)
        .arg("serve")
        .env("PF_TEST_READ", &read)
        .env("PF_TEST_RUN", &execute)
        .env("PF_TEST_ADMIN", &admin)
        .env("PREFLIGHT_ADMIN_TOKEN", &settings)
        .env_remove("PREFLIGHT_API_TOKEN")
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        base: format!("http://{}", config.bind),
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
        config: configuration,
    };
    for _ in 0..200 {
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "owned server failed to start"
        );
        if server
            .http
            .get(format!("{}/health", server.base))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(server.response("/health", None, None).await.status(), 200);
    assert_eq!(server.response("/ready", None, None).await.status(), 200);
    for asset in [
        "/",
        "/app.js",
        "/app.css",
        "/projects/example/pipelines",
        "/connection",
    ] {
        assert_eq!(server.response(asset, None, None).await.status(), 200);
    }
    let login = server
        .response("/api/v1/browser-session", Some(json!({})), Some(&read))
        .await;
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(cookie.contains("HttpOnly; SameSite=Strict; Path=/api/v1/; Max-Age=3600"));
    let cookie = cookie.split(';').next().unwrap();
    let request = |method, path: &str| {
        server
            .http
            .request(method, format!("{}{path}", server.base))
            .header("Cookie", cookie)
    };
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/projects")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        request(reqwest::Method::POST, "/api/v1/projects")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        request(reqwest::Method::GET, "/mcp")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/projects")
            .header("Origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/projects")
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        request(reqwest::Method::DELETE, "/api/v1/browser-session")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/projects")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server
            .response("/api/v1/projects", None, None)
            .await
            .status(),
        401
    );
    assert_eq!(
        server
            .response("/api/v1/projects", None, Some("wrong"))
            .await
            .status(),
        401
    );
    let caps = server.value("/api/v1/capabilities", None, &read).await;
    assert_eq!(caps["access"]["role"], "read_only");
    assert_eq!(
        server
            .response("/api/v1/access-audit", None, Some(&read))
            .await
            .status(),
        403
    );
    let request = json!({"project_id":project,"command":{"program":if cfg!(windows){"./helper.exe"}else{"./helper"},"args":["echo",execute],"timeout_seconds":5}});
    assert_eq!(
        server
            .response("/api/v1/runs", Some(request.clone()), Some(&read))
            .await
            .status(),
        403
    );
    assert_eq!(
        server
            .response(
                "/api/v1/server-settings",
                Some(json!({"admin_token":settings})),
                Some(&execute)
            )
            .await
            .status(),
        403
    );
    assert_eq!(
        server
            .response("/api/v1/future-operation", Some(json!({})), Some(&execute))
            .await
            .status(),
        403
    );
    let transport = StreamableHttpClientTransport::with_client(
        server.http.clone(),
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", server.base))
            .auth_header(read.clone()),
    );
    let mcp = ().serve(transport).await.unwrap();
    let listed = mcp
        .call_tool(CallToolRequestParams::new("projects_list"))
        .await
        .unwrap();
    assert!(!listed.is_error.unwrap_or(false));
    assert!(
        mcp.call_tool(
            CallToolRequestParams::new("build_start")
                .with_arguments(request.as_object().unwrap().clone())
        )
        .await
        .is_err()
    );
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"owner-test","version":"1"}}});
    let response = server
        .http
        .post(format!("{}/mcp", server.base))
        .bearer_auth(&read)
        .header("Accept", "application/json, text/event-stream")
        .json(&initialize)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let session = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    drop(response);
    let other = server
        .http
        .get(format!("{}/mcp", server.base))
        .bearer_auth(&admin)
        .header("Accept", "text/event-stream")
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 401);
    let _ = server
        .http
        .delete(format!("{}/mcp", server.base))
        .bearer_auth(&read)
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    let started = server.value("/api/v1/runs", Some(request), &execute).await;
    let text = serde_json::to_string(&started).unwrap();
    assert!(
        !text.contains(&execute),
        "credential appeared in serialized run response"
    );
    let id = started["id"].as_str().unwrap();
    let completed = server.finished(id, &execute).await;
    assert_eq!(completed["status"], "succeeded");
    let logs = server
        .value(&format!("/api/v1/runs/{id}/logs"), None, &read)
        .await;
    assert!(
        logs["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["text"].as_str().unwrap_or("").contains("[REDACTED]"))
    );
    assert!(!serde_json::to_string(&logs).unwrap().contains(&execute));
    let raw = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    let stored: String = raw
        .query_row(
            "SELECT group_concat(text,'') FROM log_events WHERE run_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !stored.contains(&execute),
        "credential persisted in log payload"
    );
    let cli = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .arg("--config")
        .arg(&server.config)
        .args(["status", id])
        .env("PREFLIGHT_API_TOKEN", &read)
        .output()
        .await
        .unwrap();
    assert!(cli.status.success(), "CLI role delegation failed");
    assert!(!String::from_utf8_lossy(&cli.stdout).contains(&execute));
    let waiting=server.value("/api/v1/runs",Some(json!({"project_id":project,"command":{"program":if cfg!(windows){"./helper.exe"}else{"./helper"},"args":["wait","active survives permission reload"],"timeout_seconds":5}})),&execute).await;
    assert!(waiting["finished_at"].is_null());
    let mut running = false;
    for _ in 0..100 {
        let observed = server
            .value(
                &format!("/api/v1/runs/{}", waiting["id"].as_str().unwrap()),
                None,
                &read,
            )
            .await;
        if observed["status"] == "running" {
            running = true;
            break;
        }
        assert!(
            observed["finished_at"].is_null(),
            "permission reload acceptance requires a live owned command"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        running,
        "owned command did not start before permission reload"
    );
    let settings_before = server
        .value(
            "/api/v1/server-settings",
            Some(json!({"admin_token":settings})),
            &admin,
        )
        .await;
    let mut saved = settings_before["saved"].clone();
    saved["access_tokens"][1]["role"] = json!("read_only");
    server.value("/api/v1/server-settings",Some(json!({"admin_token":settings,"expected_revision":settings_before["revision"],"configuration":saved})),&admin).await;
    assert_eq!(
        server
            .response(
                "/api/v1/projects",
                Some(json!({"name":"forbidden","path":root})),
                Some(&execute)
            )
            .await
            .status(),
        403
    );
    assert_eq!(
        server
            .finished(waiting["id"].as_str().unwrap(), &read)
            .await["status"],
        "succeeded"
    );
    assert_eq!(
        server
            .response(&format!("/api/v1/{admin}"), Some(json!({})), Some(&read))
            .await
            .status(),
        403
    );
    let stored_audit: String =
        rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT group_concat(operation,' ') FROM access_audit",
                [],
                |row| row.get(0),
            )
            .unwrap();
    for secret in [&read, &execute, &admin, &settings] {
        assert!(!stored_audit.contains(secret));
    }
    let audit = server
        .value("/api/v1/access-audit?limit=100", None, &admin)
        .await;
    assert!(
        audit["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["principal"] == "runner" && v["outcome"] == "authorized")
    );
    for secret in [&read, &execute, &admin, &settings] {
        assert!(!serde_json::to_string(&audit).unwrap().contains(secret));
    }
    mcp.cancel().await.unwrap();
}
