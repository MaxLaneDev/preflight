#![cfg(target_os = "macos")]
#[allow(dead_code)]
mod common;
use common::Fixture;
use fs2::FileExt;
use preflight::workers;
use serde_json::Value;
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

struct Agent(String);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &self.0])
            .output();
    }
}
async fn sequence_after(state: &Path, previous: u64) -> u64 {
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let value: Value =
                serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap())
                    .unwrap();
            if let Some(sequence) = value["sequence"].as_u64().filter(|s| *s > previous) {
                return sequence;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker did not resume heartbeats")
}
fn pid(label: &str) -> u32 {
    let output = std::process::Command::new("launchctl")
        .args(["print", label])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pid = ")
                .and_then(|s| s.parse().ok())
        })
        .expect("launchd worker PID")
}

#[tokio::test]
#[ignore = "requires a logged-in macOS launchd user domain; creates and removes only its temporary worker agent"]
async fn actual_launchd_worker_restarts_and_releases_its_identity() {
    let f = Fixture::new();
    let root = f.temp.path().canonicalize().unwrap();
    let state = root.join("worker & state");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(
        axum::serve(listener, workers::router(f.engine.clone()))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .into_future(),
    );
    let issued = f
        .engine
        .db
        .worker_enrollment(&workers::CreateEnrollment {
            project_id: f.project.id.clone(),
            name: "service-acceptance".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ttl_seconds: 60,
        })
        .unwrap();
    let mut enroll = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "connect", &gateway])
        .arg(&state)
        .arg("--once")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    enroll
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", issued["enrollment_token"].as_str().unwrap()).as_bytes())
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(15), enroll.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let enrolled: Value = serde_json::from_slice(&output.stdout).unwrap();
    let initial = sequence_after(&state, 0).await;
    let units = root.join("units");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
        .args(["worker", "service", "export", "launchd", &gateway])
        .arg(&state)
        .arg(&units)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
    let plist = exported["files"][0].as_str().unwrap();
    assert!(
        std::process::Command::new("plutil")
            .args(["-lint", plist])
            .output()
            .unwrap()
            .status
            .success()
    );
    let uid = String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let domain = format!("gui/{}", uid.trim());
    let label = format!("{domain}/local.{}", exported["service"].as_str().unwrap());
    let result = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, plist])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let agent = Agent(label.clone());
    let sequence = sequence_after(&state, initial).await;
    let first_pid = pid(&label);
    assert!(
        std::process::Command::new("launchctl")
            .args(["kill", "SIGKILL", &label])
            .output()
            .unwrap()
            .status
            .success()
    );
    sequence_after(&state, sequence).await;
    let second_pid = pid(&label);
    assert_ne!(first_pid, second_pid);
    let retained: Value =
        serde_json::from_slice(&std::fs::read(state.join("identity.json")).unwrap()).unwrap();
    assert_eq!(retained["worker_id"], enrolled["worker_id"]);
    drop(agent);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.join("client.lock"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while lock.try_lock_exclusive().is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("worker did not release state lock after bootout");
    assert!(
        !std::process::Command::new("launchctl")
            .args(["print", &label])
            .output()
            .unwrap()
            .status
            .success()
    );
    stop.cancel();
    server.await.unwrap().unwrap();
    println!(
        "worker service accepted: restart changed PID; identity retained; agent removed; lock released"
    );
}
