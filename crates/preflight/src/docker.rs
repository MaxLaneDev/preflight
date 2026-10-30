//! Optional local-daemon execution. Only labelled owned containers are removed.
use crate::{
    config::Config,
    db::Db,
    executor::{self, Execution, LogMode, ProcessOutput},
    model::{Artifact, Fault, RunState, invalid},
    pipeline::{ArtifactKind, StageDefinition},
};
use anyhow::{Context as _, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const RUNTIME_FORMAT: &str = "preflight-local-docker-v1";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    None,
    Bridge,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image: String,
    /// File artifact containing an immutable Docker image ID or repository digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_from: Option<String>,
    pub platform: String,
    #[serde(default)]
    pub network: Network,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default = "read_only")]
    pub read_only: bool,
}
fn read_only() -> bool {
    true
}
impl Spec {
    pub fn validate(&self) -> Result<()> {
        let valid_digest = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        let pinned = if let Some(id) = self.image.strip_prefix("sha256:") {
            valid_digest(id)
        } else {
            self.image
                .rsplit_once("@sha256:")
                .is_some_and(|(name, id)| {
                    !name.is_empty()
                        && name.len() <= 256
                        && name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"/._-:".contains(&c))
                        && !name.starts_with('-')
                        && valid_digest(id)
                })
        };
        if let Some(reference) = &self.image_from {
            ensure!(
                self.image.is_empty() && crate::pipeline::artifact_ref(reference)?.is_some(),
                invalid(
                    "container image_from requires one artifact reference and no literal image"
                )
            );
        }
        ensure!(
            pinned || self.image_from.is_some(),
            invalid(
                "Docker image must be an immutable sha256:<image-id> or repository@sha256:<digest>"
            )
        );
        ensure!(
            matches!(self.platform.as_str(), "linux/amd64" | "linux/arm64"),
            invalid("Docker platform must be linux/amd64 or linux/arm64")
        );
        if let Some(user) = &self.user {
            let parts = user.split(':').collect::<Vec<_>>();
            ensure!(
                (1..=2).contains(&parts.len())
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.len() <= 10 && p.parse::<u32>().is_ok()),
                invalid("Docker user must be numeric UID or UID:GID")
            );
        }
        Ok(())
    }
    pub fn effective_user(&self) -> Option<String> {
        self.user.clone().or_else(|| {
            #[cfg(unix)]
            {
                Some(format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
                    libc::getegid()
                }))
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
    }
}
pub(crate) fn resolve_image(db: &Db, run: &str, stage: &mut StageDefinition) -> Result<()> {
    let Some(spec) = stage.container.as_mut() else {
        return Ok(());
    };
    let Some(reference) = &spec.image_from else {
        return Ok(());
    };
    let (producer, name) =
        crate::pipeline::artifact_ref(reference)?.context("prepared image artifact reference")?;
    let artifact = db
        .artifacts(run)?
        .into_iter()
        .find(|a| a.stage_id == producer && a.name == name)
        .context("prepared image artifact is not published")?;
    ensure!(
        artifact.kind == ArtifactKind::File,
        "prepared image requires a file artifact"
    );
    let metadata = std::fs::symlink_metadata(&artifact.path)?;
    ensure!(
        metadata.is_file() && !metadata.is_symlink() && metadata.len() <= 512,
        "prepared image reference must be a regular file below 512 bytes"
    );
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(std::fs::File::open(&artifact.path)?, 513),
        &mut bytes,
    )?;
    ensure!(
        bytes.len() <= 512
            && bytes.len() as u64 == artifact.size
            && crate::hashing::hex(Sha256::digest(&bytes)) == artifact.sha256,
        "prepared image artifact digest mismatch"
    );
    spec.image = std::str::from_utf8(&bytes)?.trim().to_owned();
    spec.image_from = None;
    spec.validate()?;
    Ok(())
}

pub struct Context {
    pub spec: Spec,
    runtime: Artifact,
    memory_mib: u64,
    endpoint: tokio::sync::OnceCell<String>,
}
pub fn context(config: &Config, run: &str, stage: &StageDefinition) -> Result<Option<Context>> {
    let Some(spec) = &stage.container else {
        return Ok(None);
    };
    spec.validate()?;
    let path = config
        .docker_command
        .as_ref()
        .ok_or_else(|| {
            invalid("Docker execution is disabled; configure an operator-owned docker_command")
        })?
        .canonicalize()?;
    let mut file = std::fs::File::open(&path)?;
    let size = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(Some(Context {
        spec: spec.clone(),
        runtime: Artifact {
            run_id: run.into(),
            stage_id: stage.id.clone(),
            name: "docker-runtime".into(),
            kind: ArtifactKind::Executable,
            sha256: crate::hashing::hex(digest.finalize()),
            size,
            path,
        },
        memory_mib: config.stage_memory(stage)?,
        endpoint: tokio::sync::OnceCell::new(),
    }))
}
fn runtime(config: &Config) -> Result<Artifact> {
    let stage: StageDefinition = serde_json::from_value(
        json!({"id":"docker-control","executor":"docker","container":{"image":format!("sha256:{}","0".repeat(64)),"platform":"linux/arm64"},"command":{"program":"control"}}),
    )?;
    Ok(context(config, "control", &stage)?
        .context("Docker execution disabled")?
        .runtime)
}
async fn probe(
    db: &Db,
    run: &str,
    stage: &str,
    runtime: &Artifact,
    endpoint: Option<&str>,
    args: &[String],
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let mut argv = Vec::new();
    if let Some(host) = endpoint {
        argv.extend(["--host".into(), host.into()]);
    }
    argv.extend_from_slice(args);
    let command = crate::model::CommandSpec {
        program: runtime
            .path
            .to_str()
            .context("Docker executable path is not UTF-8")?
            .into(),
        args: argv,
        timeout_seconds: 8,
    };
    let temporary = std::env::temp_dir().canonicalize()?;
    let output = executor::execute_captured_native(
        db,
        Execution {
            container: None,
            log_mode: LogMode::None,
            verified_executable: Some(runtime),
            run_id: run,
            stage_id: stage,
            workspace: &temporary,
            cwd: &temporary,
            command: &command,
            stdin: None,
            capture_limit: 65536,
            env: &BTreeMap::new(),
        },
        cancel,
    )
    .await?;
    ensure!(
        output.status == RunState::Succeeded,
        "Docker control request failed ({}; exit {}): {}",
        output.status.as_str(),
        output.exit.map_or("unavailable".into(), |v| v.to_string()),
        db.redact(&String::from_utf8_lossy(&output.stderr))
            .chars()
            .take(512)
            .collect::<String>()
    );
    ensure!(!output.truncated, "Docker control response exceeds 64 KiB");
    Ok(output.stdout)
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn local_endpoint(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 4096
            && !value.chars().any(char::is_control)
            && (value.starts_with("unix:///") || cfg!(windows) && value.starts_with("npipe://")),
        invalid(
            "Docker execution requires a local Unix socket or Windows named pipe; remote daemons are not supported"
        )
    );
    Ok(())
}
pub(crate) fn mapped(value: &str, workspace: &Path) -> Result<String> {
    Ok(value.replace(
        workspace
            .to_str()
            .context("Docker workspace is not UTF-8")?,
        "/workspace",
    ))
}
fn mount(workspace: &Path) -> Result<String> {
    Ok(format!(
        "type=bind,\"src={}\",dst=/workspace",
        workspace
            .to_str()
            .context("Docker workspace is not UTF-8")?
            .replace('"', "\"\"")
    ))
}
const INSPECT: &str = r#"{"id":{{json .Id}},"labels":{{json .Config.Labels}},"state":{{json .State}},"image":{{json .Image}}}"#;
const IMAGE: &str = r#"{"id":{{json .Id}},"os":{{json .Os}},"arch":{{json .Architecture}}}"#;
fn owned(value: &Value, job: &Job) -> Result<()> {
    ensure!(
        value["labels"]["io.preflight.owner"] == job.owner
            && value["labels"]["io.preflight.run"] == job.run
            && value["labels"]["io.preflight.stage"] == job.stage,
        "Docker ownership labels do not match; container was not removed"
    );
    Ok(())
}
struct Job {
    id: String,
    run: String,
    stage: String,
    lease: String,
    owner: String,
    endpoint: String,
    cid: Option<String>,
}
fn job(db: &Db, id: &str, project: Option<&str>) -> Result<Job> {
    Ok(db.connection()?.query_row("SELECT j.id,j.run_id,j.stage_id,j.lease_id,j.owner_token,j.endpoint,j.container_id FROM container_jobs j JOIN runs r ON r.id=j.run_id WHERE j.id=?1 AND (?2 IS NULL OR r.project_id=?2)",params![id,project],|r|Ok(Job{id:r.get(0)?,run:r.get(1)?,stage:r.get(2)?,lease:r.get(3)?,owner:r.get(4)?,endpoint:r.get(5)?,cid:r.get(6)?})).optional()?.ok_or(Fault::NotFound)?)
}
async fn cleanup(
    db: &Db,
    runtime: &Artifact,
    job: &Job,
    cancel: &CancellationToken,
    creation_settled: bool,
) -> Result<()> {
    local_endpoint(&job.endpoint)?;
    // Query by the durable owner label, so renaming a container cannot hide a live mount.
    let ids = probe(
        db,
        &job.run,
        &job.stage,
        runtime,
        Some(&job.endpoint),
        &args(&[
            "container",
            "ls",
            "--all",
            "--filter",
            &format!("label=io.preflight.owner={}", job.owner),
            "--format",
            "{{.ID}}",
        ]),
        cancel,
    )
    .await?;
    let ids = std::str::from_utf8(&ids)?
        .lines()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    ensure!(
        ids.len() <= 1,
        "Multiple Docker containers claim one owner; protection retained"
    );
    ensure!(
        job.cid.is_some() || !ids.is_empty() || creation_settled,
        invalid(
            "creation_uncertain: no create acknowledgement; retry when the owned container appears, or an operator confirms daemon create requests have settled"
        )
    );
    for id in ids {
        ensure!(
            id.len() >= 12 && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid Docker container identity"
        );
        let value: Value = serde_json::from_slice(
            &probe(
                db,
                &job.run,
                &job.stage,
                runtime,
                Some(&job.endpoint),
                &args(&["container", "inspect", "--format", INSPECT, id]),
                cancel,
            )
            .await?,
        )?;
        owned(&value, job)?;
        if let Some(expected) = &job.cid {
            ensure!(
                value["id"] == expected.as_str(),
                "Docker container identity changed; protection retained"
            );
        }
        let full = value["id"]
            .as_str()
            .context("missing Docker container ID")?;
        db.connection()?.execute(
            "UPDATE container_jobs SET container_id=COALESCE(container_id,?2) WHERE id=?1",
            params![job.id, full],
        )?;
        probe(
            db,
            &job.run,
            &job.stage,
            runtime,
            Some(&job.endpoint),
            &args(&["container", "rm", "--force", full]),
            cancel,
        )
        .await?;
    }
    let mut c = db.connection()?;
    let tx = c.transaction()?;
    tx.execute(
        "UPDATE container_jobs SET state='cleaned',reason=NULL,finished_at=?2 WHERE id=?1",
        params![job.id, crate::db::now()],
    )?;
    tx.execute("UPDATE operation_leases SET state='released',finished_at=?2 WHERE id=?1 AND kind='docker_container' AND state IN ('active','abandoned')",params![job.lease,crate::db::now()])?;
    tx.commit()?;
    Ok(())
}
fn pending(db: &Db, id: &str, error: &anyhow::Error) -> Result<()> {
    db.connection()?.execute(
        "UPDATE container_jobs SET state='cleanup_pending',reason=?2 WHERE id=?1 AND state!='cleaned'",
        params![
            id,
            db.redact(&error.to_string())
                .chars()
                .take(512)
                .collect::<String>()
        ],
    )?;
    Ok(())
}
pub async fn execute(
    db: &Db,
    input: Execution<'_>,
    context: &Context,
    cancel: &CancellationToken,
) -> Result<ProcessOutput> {
    let endpoint = context
        .endpoint
        .get_or_try_init(|| async {
            let value: Value = serde_json::from_slice(
                &probe(
                    db,
                    input.run_id,
                    input.stage_id,
                    &context.runtime,
                    None,
                    &args(&[
                        "context",
                        "inspect",
                        "--format",
                        "{{json .Endpoints.docker.Host}}",
                    ]),
                    cancel,
                )
                .await?,
            )?;
            let endpoint = value.as_str().context("Docker context has no endpoint")?;
            local_endpoint(endpoint)?;
            Ok::<String, anyhow::Error>(endpoint.into())
        })
        .await?
        .clone();
    if let Some(pin) = input.verified_executable {
        ensure!(
            pin.path.starts_with(input.workspace),
            "Host executable pins cannot be executed inside a container; use image tooling or a private input artifact"
        );
    }
    ensure!(
        input.cwd.starts_with(input.workspace),
        "Docker cwd escapes the stage workspace"
    );
    let mut lease = db.begin_operation(input.run_id, "docker_container", BTreeSet::new())?;
    let id = uuid::Uuid::now_v7().to_string();
    let owner = uuid::Uuid::now_v7().to_string();
    let name = format!("preflight-{id}");
    db.connection()?.execute("INSERT INTO container_jobs(id,run_id,stage_id,lease_id,name,owner_token,image,platform,workspace,endpoint,state,may_run,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'creating',0,?11)",params![id,input.run_id,input.stage_id,lease.id,name,owner,context.spec.image,context.spec.platform,input.workspace.to_string_lossy(),endpoint,crate::db::now()])?;
    // Only proven daemon cleanup releases this lease, including unwinding/error paths.
    lease.release_on_drop = false;
    let result = drive(db, &input, context, &id, &owner, &name, &endpoint, cancel).await;
    let cleanup_result = async {
        let record = job(db, &id, None)?;
        cleanup(
            db,
            &context.runtime,
            &record,
            &CancellationToken::new(),
            false,
        )
        .await
    }
    .await;
    if let Err(error) = cleanup_result {
        lease.abandon()?;
        pending(db, &id, &error)?;
        anyhow::bail!("Docker cleanup pending; workspace remains protected: {error}");
    }
    drop(lease);
    result
}
#[allow(clippy::too_many_arguments)]
async fn drive(
    db: &Db,
    input: &Execution<'_>,
    context: &Context,
    id: &str,
    owner: &str,
    name: &str,
    endpoint: &str,
    cancel: &CancellationToken,
) -> Result<ProcessOutput> {
    let mut argv = args(&[
        "container",
        "create",
        "--pull=never",
        "--name",
        name,
        "--label",
        &format!("io.preflight.owner={owner}"),
        "--label",
        &format!("io.preflight.run={}", input.run_id),
        "--label",
        &format!("io.preflight.stage={}", input.stage_id),
        "--platform",
        &context.spec.platform,
        "--network",
        if context.spec.network == Network::None {
            "none"
        } else {
            "bridge"
        },
        "--mount",
        &mount(input.workspace)?,
        "--workdir",
        &mapped(
            input.cwd.to_str().context("Docker cwd is not UTF-8")?,
            input.workspace,
        )?,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "1024",
        "--tmpfs",
        "/tmp:rw,exec,nosuid,nodev,size=268435456,mode=1777",
    ]);
    if context.spec.read_only {
        argv.push("--read-only".into());
    }
    if let Some(user) = context.spec.effective_user() {
        argv.extend(["--user".into(), user]);
    }
    if context.memory_mib > 0 {
        argv.extend(["--memory".into(), format!("{}m", context.memory_mib)]);
    }
    if input.stdin.is_some() {
        argv.push("--interactive".into());
    }
    let mut environment = BTreeMap::from([
        ("HOME".into(), "/tmp".into()),
        ("PREFLIGHT_WORKSPACE".into(), "/workspace".into()),
    ]);
    environment.extend(input.env.clone());
    for (name, value) in &environment {
        argv.extend([
            "--env".into(),
            format!("{name}={}", mapped(value, input.workspace)?),
        ]);
    }
    argv.extend([
        "--entrypoint".into(),
        mapped(&input.command.program, input.workspace)?,
        context.spec.image.clone(),
    ]);
    for argument in &input.command.args {
        argv.push(mapped(argument, input.workspace)?);
    }
    let created = probe(
        db,
        input.run_id,
        input.stage_id,
        &context.runtime,
        Some(endpoint),
        &argv,
        cancel,
    )
    .await?;
    let cid = std::str::from_utf8(&created)?.trim();
    ensure!(
        cid.len() == 64 && cid.bytes().all(|c| c.is_ascii_hexdigit()),
        "Docker create did not return a full container ID"
    );
    db.connection()?.execute(
        "UPDATE container_jobs SET container_id=?2,state='created' WHERE id=?1",
        params![id, cid],
    )?;
    let record = job(db, id, None)?;
    let metadata: Value = serde_json::from_slice(
        &probe(
            db,
            input.run_id,
            input.stage_id,
            &context.runtime,
            Some(endpoint),
            &args(&["container", "inspect", "--format", INSPECT, cid]),
            cancel,
        )
        .await?,
    )?;
    owned(&metadata, &record)?;
    let image: Value = serde_json::from_slice(
        &probe(
            db,
            input.run_id,
            input.stage_id,
            &context.runtime,
            Some(endpoint),
            &args(&[
                "image",
                "inspect",
                "--format",
                IMAGE,
                metadata["image"]
                    .as_str()
                    .context("Docker container has no image ID")?,
            ]),
            cancel,
        )
        .await?,
    )?;
    ensure!(
        format!(
            "{}/{}",
            image["os"].as_str().unwrap_or(""),
            image["arch"].as_str().unwrap_or("")
        ) == context.spec.platform,
        "Docker image does not match the pinned platform"
    );
    db.connection()?.execute(
        "UPDATE container_jobs SET actual_image_id=?2,state='starting',may_run=1 WHERE id=?1",
        params![id, image["id"].as_str()],
    )?;
    db.append_stage_log(
        input.run_id,
        input.stage_id,
        "lifecycle",
        0,
        &format!(
            "Preflight container execution requested: {} · {} · network {:?}\n",
            context.spec.image, context.spec.platform, context.spec.network
        ),
    )?;
    let mut start = args(&["--host", endpoint, "container", "start", "--attach"]);
    if input.stdin.is_some() {
        start.push("--interactive".into());
    }
    start.push(cid.into());
    let command = crate::model::CommandSpec {
        program: context
            .runtime
            .path
            .to_str()
            .context("Docker runtime path is not UTF-8")?
            .into(),
        args: start,
        timeout_seconds: input.command.timeout_seconds,
    };
    let temporary = std::env::temp_dir().canonicalize()?;
    let mut output = executor::execute_captured_native(
        db,
        Execution {
            container: None,
            log_mode: LogMode::Output,
            verified_executable: Some(&context.runtime),
            run_id: input.run_id,
            stage_id: input.stage_id,
            workspace: &temporary,
            cwd: &temporary,
            command: &command,
            stdin: input.stdin,
            capture_limit: input.capture_limit,
            env: &BTreeMap::new(),
        },
        cancel,
    )
    .await?;
    let mut outcome = json!({});
    if !matches!(output.status, RunState::Cancelled | RunState::TimedOut) {
        let state: Value = serde_json::from_slice(
            &probe(
                db,
                input.run_id,
                input.stage_id,
                &context.runtime,
                Some(endpoint),
                &args(&["container", "inspect", "--format", INSPECT, cid]),
                &CancellationToken::new(),
            )
            .await?,
        )?;
        owned(&state, &record)?;
        ensure!(
            state["state"]["Running"] == false && state["state"]["Status"] == "exited",
            "Docker attachment ended without a proven container exit"
        );
        let exit = i32::try_from(
            state["state"]["ExitCode"]
                .as_i64()
                .context("Docker exit code unavailable")?,
        )?;
        outcome = json!({"oom_killed":state["state"]["OOMKilled"],"started_at":state["state"]["StartedAt"],"finished_at":state["state"]["FinishedAt"]});
        output.exit = Some(exit);
        output.status = if exit == 0 {
            RunState::Succeeded
        } else {
            RunState::Failed
        };
    }
    outcome["status"] = output.status.as_str().into();
    outcome["exit_code"] = output.exit.into();
    db.connection()?.execute(
        "UPDATE container_jobs SET result_json=?2 WHERE id=?1",
        params![id, serde_json::to_string(&outcome)?],
    )?;
    db.append_stage_log(
        input.run_id,
        input.stage_id,
        "lifecycle",
        1,
        &format!(
            "Preflight container finished: {} exit={}\n",
            output.status.as_str(),
            output.exit.map_or("unavailable".into(), |v| v.to_string())
        ),
    )?;
    Ok(output)
}
pub fn admission_pending(db: &Db) -> Result<bool> {
    Ok(db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM container_jobs j JOIN operation_leases l ON l.id=j.lease_id JOIN runs r ON r.id=j.run_id LEFT JOIN stage_runs s ON s.run_id=j.run_id AND s.name=j.stage_id WHERE j.state!='cleaned' AND j.may_run=1 AND (j.state='cleanup_pending' OR l.state='abandoned' OR r.finished_at IS NOT NULL OR s.finished_at IS NOT NULL))",[],|r|r.get(0))?)
}
pub fn ensure_admission(db: &Db) -> Result<()> {
    ensure!(
        !admission_pending(db)?,
        invalid(
            "container_cleanup_pending: new commands are paused until owned-container cleanup; current commands continue"
        )
    );
    Ok(())
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    pub before: Option<String>,
    pub limit: Option<usize>,
    pub max_bytes: Option<usize>,
}
pub fn list(db: &Db, q: &Query) -> Result<Value> {
    let limit = q.limit.unwrap_or(10);
    ensure!((1..=100).contains(&limit), invalid("limit must be 1..100"));
    let c = db.connection()?;
    let rows=c.prepare("SELECT j.id,r.project_id,j.run_id,j.stage_id,j.name,j.image,j.platform,j.workspace,j.state,j.reason,j.created_at,j.finished_at,j.actual_image_id,j.result_json FROM container_jobs j JOIN runs r ON r.id=j.run_id WHERE (?1 IS NULL OR r.project_id=?1) AND (?2 IS NULL OR j.run_id=?2) AND (?3 IS NULL OR j.id<?3) ORDER BY j.id DESC LIMIT ?4")?.query_map(params![q.project_id,q.run_id,q.before,limit+1],|r|Ok(json!({"job_id":r.get::<_,String>(0)?,"project_id":r.get::<_,String>(1)?,"run_id":r.get::<_,String>(2)?,"stage_id":r.get::<_,String>(3)?,"name":r.get::<_,String>(4)?,"image":r.get::<_,String>(5)?,"platform":r.get::<_,String>(6)?,"host_workspace":r.get::<_,String>(7)?,"container_workspace":"/workspace","state":r.get::<_,String>(8)?,"reason":r.get::<_,Option<String>>(9)?,"created_at":r.get::<_,i64>(10)?,"finished_at":r.get::<_,Option<i64>>(11)?,"actual_image_id":r.get::<_,Option<String>>(12)?,"result":r.get::<_,Option<String>>(13)?.map(|v|serde_json::from_str::<Value>(&v)).transpose().map_err(|e|rusqlite::Error::FromSqlConversionFailure(13,rusqlite::types::Type::Text,Box::new(e)))?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let max_bytes = q.max_bytes.unwrap_or(8192);
    ensure!(
        (4096..=65536).contains(&max_bytes),
        invalid("max_bytes must be 4096..65536")
    );
    let mut count = limit;
    loop {
        let result = json!({"jobs":rows.iter().take(count).collect::<Vec<_>>(),"next_cursor":(rows.len()>count).then(||rows[count-1]["job_id"].clone())});
        if serde_json::to_vec(&result)?.len() <= max_bytes {
            return Ok(result);
        }
        ensure!(
            count > 1,
            invalid("container metadata exceeds budget; increase max_bytes")
        );
        count -= 1;
    }
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cleanup {
    /// Operator assertion for a missing create acknowledgement: outstanding daemon create requests have settled. Never infer this from the CLI process exiting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub creation_settled: bool,
    pub project_id: String,
    pub job_id: String,
    pub request_id: String,
    pub actor: String,
    pub reason: String,
}
pub async fn cleanup_request(e: &crate::engine::Engine, r: &Cleanup) -> Result<Value> {
    let _gate = e.container_cleanup_gate.lock().await;
    crate::control::validate_name(&r.request_id)?;
    ensure!(
        !r.actor.trim().is_empty()
            && r.actor.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 1024,
        invalid("bounded actor and reason are required")
    );
    let record = job(&e.db, &r.job_id, Some(&r.project_id))?;
    let request = serde_json::to_string(r)?;
    {
        let mut c = e.db.connection()?;
        let tx = c.transaction()?;
        if let Some(old)=tx.query_row("SELECT request_json FROM container_cleanup_requests WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|v|v.get::<_,String>(0)).optional()?{ensure!(old==request,Fault::IdempotencyConflict);}
    else{tx.execute("INSERT INTO container_cleanup_requests VALUES (?1,?2,?3,?4)",params![r.project_id,r.request_id,request,crate::db::now()])?;}
        let state: String = tx.query_row(
            "SELECT state FROM container_jobs WHERE id=?1",
            [&r.job_id],
            |v| v.get(0),
        )?;
        if state == "cleaned" {
            tx.commit()?;
            return Ok(json!({"job_id":r.job_id,"state":"cleaned","replayed":true}));
        }
        let abandoned: bool = tx.query_row(
            "SELECT l.state='abandoned' OR r.finished_at IS NOT NULL OR s.finished_at IS NOT NULL FROM operation_leases l JOIN runs r ON r.id=l.run_id LEFT JOIN container_jobs j ON j.lease_id=l.id LEFT JOIN stage_runs s ON s.run_id=j.run_id AND s.name=j.stage_id WHERE l.id=?1",
            [&record.lease],
            |v| v.get(0),
        )?;
        ensure!(
            state == "cleanup_pending" || abandoned,
            invalid("cancel the active run before cleaning its container")
        );
        tx.commit()?;
    }
    let runtime = runtime(&e.current_config())?;
    if let Err(error) = cleanup(
        &e.db,
        &runtime,
        &record,
        &CancellationToken::new(),
        r.creation_settled,
    )
    .await
    {
        pending(&e.db, &record.id, &error)?;
        return Err(error);
    }
    Ok(json!({"job_id":r.job_id,"state":"cleaned"}))
}
pub async fn run(e: Arc<crate::engine::Engine>) {
    let stop = e.transport_stop();
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {_=stop.cancelled()=>break,_=interval.tick()=>{}}
        let ids=e.db.connection().and_then(|c|Ok(c.prepare("SELECT j.id FROM container_jobs j JOIN operation_leases l ON l.id=j.lease_id JOIN runs r ON r.id=j.run_id LEFT JOIN stage_runs s ON s.run_id=j.run_id AND s.name=j.stage_id WHERE j.state!='cleaned' AND (j.state='cleanup_pending' OR l.state='abandoned' OR r.finished_at IS NOT NULL OR s.finished_at IS NOT NULL) ORDER BY j.created_at LIMIT 10")?.query_map([],|v|v.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?));
        let Ok(ids) = ids else { continue };
        if ids.is_empty() {
            continue;
        }
        let Ok(runtime) = runtime(&e.current_config()) else {
            continue;
        };
        for id in ids {
            let _gate = e.container_cleanup_gate.lock().await;
            if stop.is_cancelled() {
                break;
            }
            if let Ok(job) = job(&e.db, &id, None)
                && let Err(error) = cleanup(&e.db, &runtime, &job, &stop, false).await
            {
                let _ = pending(&e.db, &id, &error);
            }
        }
    }
}
/// Caller holds the worker's exclusive state lock; remote jobs are never replayed.
pub(crate) async fn cleanup_worker(
    db: &Db,
    config: &Config,
    run: &str,
    creation_settled: bool,
) -> Result<()> {
    let ids = db
        .connection()?
        .prepare("SELECT id FROM container_jobs WHERE run_id=?1 AND state!='cleaned'")?
        .query_map([run], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let runtime = runtime(config)?;
    for id in ids {
        let record = job(db, &id, None)?;
        if let Err(error) = cleanup(
            db,
            &runtime,
            &record,
            &CancellationToken::new(),
            creation_settled,
        )
        .await
        {
            pending(db, &id, &error)?;
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn images_platforms_users_and_mounts_are_explicit() {
        let mut spec = Spec {
            image: format!("repo@sha256:{}", "a".repeat(64)),
            image_from: None,
            platform: "linux/arm64".into(),
            network: Network::None,
            user: None,
            read_only: true,
        };
        spec.validate().unwrap();
        spec.image = "repo:latest".into();
        assert!(spec.validate().is_err());
        spec.image = format!("sha256:{}", "b".repeat(64));
        spec.platform = "windows/amd64".into();
        assert!(spec.validate().is_err());
        spec.platform = "linux/amd64".into();
        spec.user = Some("root".into());
        assert!(spec.validate().is_err());
        assert_eq!(
            mount(Path::new("/tmp/a,b\"c")).unwrap(),
            "type=bind,\"src=/tmp/a,b\"\"c\",dst=/workspace"
        );
        assert!(local_endpoint("ssh://remote").is_err());
        assert!(local_endpoint("tcp://127.0.0.1:2375").is_err());
    }
}
