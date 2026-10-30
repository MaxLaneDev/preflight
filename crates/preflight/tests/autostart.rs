#[allow(dead_code)]
mod common;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use preflight::{api, engine::Engine};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn startup_contract_requires_credentials_and_is_admin_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let secret = temp.path().join("admin-token");
    std::fs::write(&secret, "credential_012345678901234567890123456789").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut config = common::Fixture::config(temp.path());
    config.admin_token_file = Some(secret);
    let config_path = temp.path().join("preflight.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
    let engine = Engine::open(config).unwrap();
    engine.config_file.set(config_path).unwrap();
    let router = api::router(engine.clone());
    let response = router
        .clone()
        .oneshot(
            Request::post("/api/v1/server-autostart")
                .header("content-type", "application/json")
                .header("host", "localhost")
                .body(Body::from(json!({"admin_token":"wrong"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.status().is_success());
    // Read-only inspection remains safe even where the test runner has no GUI session.
    let response = router
        .oneshot(
            Request::post("/api/v1/server-autostart")
                .header("content-type", "application/json")
                .header("host", "localhost")
                .body(Body::from(
                    json!({"admin_token":"credential_012345678901234567890123456789"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["supported"], cfg!(target_os = "macos"));
    assert!(
        serde_json::from_value::<preflight::autostart::Request>(
            json!({"admin_token":"x","enabled":"yes"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<preflight::autostart::Request>(
            json!({"admin_token":"x","enabled":true,"path":"/unrelated"})
        )
        .is_err()
    );
    assert!(
        preflight::autostart::request(
            &engine,
            preflight::autostart::Request {
                admin_token: "credential_012345678901234567890123456789".into(),
                enabled: Some(true),
                expected_revision: Some("stale".into())
            }
        )
        .is_err()
    );
}
