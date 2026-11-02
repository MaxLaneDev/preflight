use preflight::{config::Config, engine::Engine, model::*};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub engine: Arc<Engine>,
    pub project: Project,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let binary = helper();
        std::fs::copy(binary, root.join(binary.file_name().unwrap())).unwrap();
        let engine = Engine::open(Self::config(temp.path())).unwrap();
        let project = engine
            .register(RegisterProject {
                name: "example".into(),
                path: root.to_string_lossy().into_owned(),
            })
            .unwrap();
        Self {
            temp,
            engine,
            project,
        }
    }
    pub fn config(root: &std::path::Path) -> Config {
        Config {
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
            data_dir: root.join("data"),
            allowed_roots: vec![root.to_path_buf()],
        }
    }
    pub fn request(&self, args: &[&str], timeout_seconds: u64) -> StartBuild {
        StartBuild {
            project_id: self.project.id.clone(),
            command: CommandSpec {
                program: if cfg!(windows) {
                    "./helper.exe"
                } else {
                    "./helper"
                }
                .into(),
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout_seconds,
            },
        }
    }
    pub fn start(&self, args: &[&str], timeout_seconds: u64) -> Run {
        self.engine
            .start(self.request(args, timeout_seconds))
            .unwrap()
    }
}
fn helper() -> &'static PathBuf {
    static HELPER: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &HELPER
        .get_or_init(|| {
            let temp = tempfile::tempdir().unwrap();
            let binary = temp.path().join(if cfg!(windows) {
                "helper.exe"
            } else {
                "helper"
            });
            let output = std::process::Command::new("rustc")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/support/process_helper.rs"
                ))
                .arg("-o")
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            (temp, binary)
        })
        .1
}
pub async fn finished(engine: &Engine, id: &str) -> Run {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let run = engine.db.run(id).unwrap();
            if run.status.terminal() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await
        }
    })
    .await
    .expect("run completes")
}
pub fn output(engine: &Engine, id: &str, stream: &str) -> String {
    let mut text = String::new();
    let mut after = 0;
    loop {
        let page = engine.db.logs(id, after).unwrap();
        for event in page.events {
            if event.stream == stream {
                text.push_str(&event.text)
            }
        }
        match page.next_cursor {
            Some(cursor) => after = cursor,
            None => return text,
        }
    }
}

// These tests reconstruct older schemas from a current fixture, never a production downgrade.
#[allow(dead_code)]
pub fn remove_schema_53_for_historical_fixture(c: &rusqlite::Connection) {
    c.execute_batch("DROP TABLE worker_log_streams; DROP TABLE worker_log_batches; ALTER TABLE worker_jobs DROP COLUMN log_cursor; ALTER TABLE worker_jobs DROP COLUMN log_bytes; ALTER TABLE worker_jobs DROP COLUMN log_events; ALTER TABLE worker_jobs DROP COLUMN log_truncated; DELETE FROM schema_version WHERE version=53; DELETE FROM migration_history WHERE version=53;").unwrap();
}
