#![cfg(unix)]
#[allow(dead_code)]
mod common;
use common::*;
use preflight::workers;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
async fn bootstrap(bundle: &Path, gateway: &str, root: &Path, token: Option<&str>) -> Value {
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg(bundle.join("bootstrap.sh"))
        .arg(gateway)
        .arg(root)
        .arg("--once")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    if let Some(token) = token {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{token}\n").as_bytes())
            .await
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    // Installation verifies/copies the full debug binary; this is not a heartbeat deadline.
    let result = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
#[tokio::test]
async fn package_bootstrap_and_ssh_launcher_install_reconnect_and_reject_tampering() {
    let f = Fixture::new();
    let root = f.temp.path().canonicalize().unwrap();
    let packages = root.join("packages");
    let bundle = packages.join(format!(
        "{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let packaged = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "package"])
        .arg(&bundle)
        .output()
        .await
        .unwrap();
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stderr)
    );
    let manifest: Value = serde_json::from_slice(&packaged.stdout).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = listener.local_addr().unwrap();
    let base = format!("http://{bind}");
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let app = workers::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let issued = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "bootstrap".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let installed = root.join("installed worker");
    let first = bootstrap(
        &bundle,
        &base,
        &installed,
        issued["enrollment_token"].as_str(),
    )
    .await;
    let second = bootstrap(&bundle, &base, &installed, None).await;
    assert_eq!(first["worker_id"], second["worker_id"]);
    for adapter in ["launchd", "systemd"] {
        let packaged_binary = installed
            .join("versions")
            .join(manifest["sha256"].as_str().unwrap());
        let output = tokio::process::Command::new(packaged_binary)
            .args(["worker", "service", "export", adapter, &base])
            .arg(installed.join("state"))
            .arg(root.join(format!("{adapter}-worker-service")))
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(exported["activated"], false);
        assert!(
            exported["service"]
                .as_str()
                .unwrap()
                .starts_with("preflight-worker-")
        );
    }

    assert_eq!(first["authenticated_heartbeat"], true);
    assert_eq!(
        std::fs::read_dir(installed.join("versions"))
            .unwrap()
            .count(),
        1
    );
    assert!(
        installed
            .join("versions")
            .join(manifest["sha256"].as_str().unwrap())
            .is_file()
    );
    // Execute the launcher's exact installation/connect commands through a local transport double.
    let shim = root.join("transport");
    std::fs::create_dir(&shim).unwrap();
    let compiled = std::process::Command::new("rustc")
        .args([
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/ssh.rs"),
            "-o",
        ])
        .arg(shim.join("ssh"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_bind = admin_listener.local_addr().unwrap();
    let admin_stop = CancellationToken::new();
    let admin_shutdown = admin_stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let admin = tokio::spawn(async move {
        axum::serve(admin_listener, app)
            .with_graceful_shutdown(admin_shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let mut config = Fixture::config(&root);
    config.bind = admin_bind;
    config.worker_bind = Some(bind);
    let config_path = root.join("preflight.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
    let request = root.join("connection.json");
    let remote = root.join("remote ' worker");
    std::fs::write(&request,json!({"project_id":f.project.id,"name":"ssh-worker","host":"test-host","bundle_dir":packages,"remote_dir":remote,"remote_port":bind.port(),"once":true}).to_string()).unwrap();
    let mut worker_id = None;
    for _ in 0..2 {
        let output = tokio::time::timeout(
            Duration::from_secs(90),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
                .arg("--config")
                .arg(&config_path)
                .arg("worker-ssh")
                .arg(&request)
                .env(
                    "PATH",
                    format!("{}:{}", shim.display(), std::env::var("PATH").unwrap()),
                )
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let connected: Value = serde_json::from_slice(&output.stdout).unwrap();
        if let Some(id) = &worker_id {
            assert_eq!(&connected["worker_id"], id);
        } else {
            worker_id = Some(connected["worker_id"].clone());
        }
        assert_eq!(connected["authenticated_heartbeat"], true);
    }
    let list = f
        .engine
        .db
        .workers(&workers::List {
            project_id: f.project.id.clone(),
            after: None,
        })
        .unwrap();
    assert_eq!(list["workers"].as_array().unwrap().len(), 2);
    // Repair a damaged owned binary atomically without changing the enrollment identity.
    let installed_binary = installed
        .join("versions")
        .join(manifest["sha256"].as_str().unwrap());
    std::fs::write(&installed_binary, b"damaged installed binary").unwrap();
    let repaired = bootstrap(&bundle, &base, &installed, None).await;
    assert_eq!(repaired["worker_id"], first["worker_id"]);
    assert_eq!(
        std::fs::metadata(&installed_binary).unwrap().len(),
        manifest["bytes"].as_u64().unwrap()
    );
    let original = std::fs::read(bundle.join("manifest.json")).unwrap();
    let mut legacy: Value = serde_json::from_slice(&original).unwrap();
    legacy["format"] = "preflight-worker-package-v1".into();
    legacy.as_object_mut().unwrap().remove("cli_protocol");
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    assert_eq!(
        preflight::worker_package::verify(&bundle, std::env::consts::OS, std::env::consts::ARCH)
            .unwrap()
            .0
            .cli_protocol,
        1
    );
    let state = remote.join("state/identity.json");
    let identity_before = std::fs::read(&state).unwrap();
    let rejected = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
            .arg("--config")
            .arg(&config_path)
            .arg("worker-ssh")
            .arg(&request)
            .env(
                "PATH",
                format!("{}:{}", shim.display(), std::env::var("PATH").unwrap()),
            )
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("worker package v2"));
    assert_eq!(
        std::fs::read(&state).unwrap(),
        identity_before,
        "legacy SSH rejection must not alter enrollment"
    );
    std::fs::write(bundle.join("manifest.json"), &original).unwrap();
    let mut incompatible: Value = serde_json::from_slice(&original).unwrap();
    incompatible["job_protocol"] = 999.into();
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(&incompatible).unwrap(),
    )
    .unwrap();
    assert!(
        preflight::worker_package::verify(&bundle, std::env::consts::OS, std::env::consts::ARCH)
            .is_err()
    );
    std::fs::write(bundle.join("manifest.json"), original).unwrap();
    // Tampered packages cannot execute the installer or change established enrollment state.
    let before = std::fs::read(installed.join("state/identity.json")).unwrap();
    std::fs::write(bundle.join("preflight"), b"corrupt").unwrap();
    let denied = tokio::process::Command::new("/bin/sh")
        .arg(bundle.join("bootstrap.sh"))
        .arg(&base)
        .arg(&installed)
        .arg("--once")
        .output()
        .await
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("SHA-256 mismatch"));
    assert_eq!(
        std::fs::read(installed.join("state/identity.json")).unwrap(),
        before
    );
    admin_stop.cancel();
    admin.await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn resume_without_identity_fails_without_creating_state_or_enrollment() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().canonicalize().unwrap().join("missing-state");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "resume", "http://127.0.0.1:1"])
        .arg(&state)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(!state.exists());
}
