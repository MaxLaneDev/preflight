#[allow(dead_code)]
mod common;
use common::*;
use preflight::{engine::Engine, releases};
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
#[tokio::test]
async fn release_mcp_http_admin_and_disabled_settings_contracts() {
    let mut f = Fixture::new();
    let token = "test-release-admin-credential-".repeat(2);
    let path = f.temp.path().join("admin-token");
    std::fs::write(&path, &token).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut config = f.engine.config.clone();
    config.admin_token_file = Some(path);
    drop(f.engine);
    f.engine = Engine::open(config).unwrap();
    assert!(
        releases::request(
            &f.engine,
            releases::Check {
                admin_token: "wrong".into(),
                details: false
            }
        )
        .await
        .is_err()
    );
    let config_path = f.temp.path().join("server.toml");
    std::fs::write(
        &config_path,
        toml::to_string_pretty(&f.engine.current_config()).unwrap(),
    )
    .unwrap();
    f.engine.config_file.set(config_path).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{base}/mcp"
        )))
        .await
        .unwrap();
    let mcp = client
        .call_tool(
            CallToolRequestParams::new("server_release_status")
                .with_arguments(json!({}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(mcp.is_error, Some(true), "{mcp:?}");
    let mcp: Value = serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
    let http: Value = reqwest::get(format!("{base}/api/v1/server-release"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(http, mcp);
    assert_eq!(http["status"], "disabled");
    assert!(http.get("changelog").is_none());
    let checked: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/server-release"))
        .json(&json!({"admin_token":token,"details":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(checked, http);
    assert!(!serde_json::to_string(&checked).unwrap().contains(&token));
    let caps: Value = reqwest::get(format!("{base}/api/v1/capabilities"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["features"]["release_notifications"], true);
    let run=f.engine.start_pipeline(serde_json::from_value(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"ongoing","command":{"program":"sh","args":["-c","sleep 1; printf completed"],"timeout_seconds":5}}]}}})).unwrap()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if f.engine.db.run(&run.id).unwrap().stages[0].status
                == preflight::model::RunState::Running
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let saved = preflight::settings::request(
        &f.engine,
        preflight::settings::Request {
            admin_token: token.clone(),
            expected_revision: None,
            configuration: None,
        },
    )
    .unwrap();
    let mut configuration = saved["saved"].clone();
    configuration["release_repository"] = json!("example/preflight");
    let saved:Value=reqwest::Client::new().post(format!("{base}/api/v1/server-settings")).json(&json!({"admin_token":token,"expected_revision":saved["revision"],"configuration":configuration})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(saved["release_notifications_supported"], true);
    assert_eq!(saved["active"]["release_repository"], "example/preflight");
    assert_eq!(releases::status(&f.engine, false)["status"], "unchecked");
    configuration
        .as_object_mut()
        .unwrap()
        .remove("release_repository");
    preflight::settings::request(
        &f.engine,
        preflight::settings::Request {
            admin_token: token.clone(),
            expected_revision: Some(saved["revision"].as_str().unwrap().into()),
            configuration: Some(configuration),
        },
    )
    .unwrap();
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        preflight::model::RunState::Succeeded
    );
    assert!(output(&f.engine, &run.id, "stdout").contains("completed"));
    client.cancel().await.unwrap();
    server.abort();
    let _ = server.await;
}
