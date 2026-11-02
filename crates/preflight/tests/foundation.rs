use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
#[tokio::test]
async fn health_and_embedded_ui_respond() {
    let dir = tempfile::tempdir().unwrap();
    let engine = preflight::engine::Engine::open(preflight::config::Config {
        release_repository: None,
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
        codex_command: None,
        codex_ipc_socket: None,
        codex_delivery_mode: None,
        version: 1,
        storage_policies: vec![],
        max_active_runs: 32,
        max_parallel_commands: 2,
        bind: "127.0.0.1:4170".parse().unwrap(),
        data_dir: dir.path().join("data"),
        allowed_roots: vec![dir.path().to_path_buf()],
    })
    .unwrap();
    for path in ["/health", "/"] {
        let response = preflight::api::router(engine.clone())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("Host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
