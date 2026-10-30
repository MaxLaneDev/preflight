//! Outbound worker with durable native-job journal and fail-closed lease renewal.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    gateway: String,
    credential: String,
    worker_id: Option<String>,
    sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<Pending>,
}
fn save(root: &Path, state: &State) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(state)?)?;
    file.as_file().sync_all()?;
    file.persist(root.join("identity.json"))?;
    #[cfg(unix)]
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}
fn no_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            !m.file_type().is_symlink(),
            "worker state must not use symlinks"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn gateway_url(gateway: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(gateway)?;
    // HTTP is allowed only inside the worker's host. Use an SSH tunnel for cross-host traffic.
    ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "gateway must be a loopback HTTP origin; remote access currently requires an authenticated SSH tunnel"
    );
    Ok(url)
}
fn check_identity(root: &Path) -> Result<()> {
    no_symlink(&root.join("identity.json"))?;
    if let Ok(metadata) = std::fs::metadata(root.join("identity.json")) {
        ensure!(
            metadata.len() <= 64 * 1024,
            "worker identity file is too large"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "worker identity file must be private (mode 0600)"
            );
        }
    }
    Ok(())
}
// Services cannot interactively enroll or replace an existing worker identity.
pub(crate) fn validate_service_identity(gateway: &str, root: &Path) -> Result<()> {
    let url = gateway_url(gateway)?;
    ensure!(
        root.is_absolute() && root.canonicalize()? == root,
        "service state directory must be absolute and canonical"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "worker state directory must be private (mode 0700)"
        );
    }
    check_identity(root)?;
    let state: State = serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
    ensure!(
        state.gateway == url.as_str(),
        "worker identity belongs to a different gateway"
    );
    ensure!(
        state.worker_id.as_ref().is_some_and(|id| !id.is_empty()) && !state.credential.is_empty(),
        "enroll the worker before exporting its service"
    );
    Ok(())
}

struct StopSignals {
    token: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for StopSignals {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl StopSignals {
    fn start() -> Result<Self> {
        let token = tokio_util::sync::CancellationToken::new();
        let notify = token.clone();
        #[cfg(unix)]
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        #[cfg(unix)]
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let task = tokio::spawn(async move {
            #[cfg(unix)]
            tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{}, _=hup.recv()=>{} }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            notify.cancel();
        });
        Ok(Self { token, task })
    }
}
/// Reconnect without allowing a missing identity to trigger enrollment.
pub async fn resume(gateway: &str, root: &Path) -> Result<()> {
    validate_service_identity(gateway, root)?;
    connect(gateway, root, false, false).await
}

struct StateLock(std::fs::File);
impl StateLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}
impl Drop for StateLock {
    fn drop(&mut self) {
        // A duplicated/inherited descriptor can outlive this owner, as for Engine's lock.
        let _ = FileExt::unlock(&self.0);
    }
}
fn recovery_journal(root: &Path) -> Result<(StateLock, State)> {
    no_symlink(root)?;
    check_identity(root)?;
    let state: State = serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
    validate_service_identity(&state.gateway, root)?;
    no_symlink(&root.join("client.lock"))?;
    let lock = StateLock::acquire(&root.join("client.lock"))
        .context("worker state is already in use; stop the old worker before recovery")?;
    // Reload after acquiring the same lock used by connect/resume.
    let state = serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
    Ok((lock, state))
}
pub fn journal_status(root: &Path) -> Result<serde_json::Value> {
    no_symlink(root)?;
    let root = root.canonicalize()?;
    check_identity(&root)?;
    let state: State = serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
    validate_service_identity(&state.gateway, &root)?;
    Ok(
        serde_json::json!({"worker_id":state.worker_id,"gateway":state.gateway,"pending":state.pending.as_ref().map(|p|serde_json::json!({"job_id":p.job_id,"phase":p.phase,"outcome_status":p.outcome.as_ref().map(|o|o.status)})),"container_cleanup_pending":state.pending.as_ref().and_then(|p|p.job_id.as_deref()).map(|job|container_pending(&root,job)).transpose()?,"policy":"stop old processes, clean owned containers and resolve server protection before explicit journal recovery"}),
    )
}
// The caller holds the worker lock and has durable server/cleanup recovery proof.
fn retire_local_execution(root: &Path, job: &str) -> Result<Option<crate::model::RunState>> {
    use rusqlite::{OptionalExtension, params};
    let directory = root.join("jobs");
    no_symlink(&directory)?;
    let path = directory.join(format!("{job}.sqlite3"));
    no_symlink(&path)?;
    if !path.exists() {
        return Ok(None);
    }
    let db = crate::db::Db::new(&path)?;
    let mut c = db.connection()?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let foreign: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE id!=?1)",
        [job],
        |r| r.get(0),
    )?;
    ensure!(
        !foreign,
        "worker job ledger contains a different run identity"
    );
    let present: Option<String> = tx
        .query_row("SELECT id FROM runs WHERE id=?1", [job], |r| r.get(0))
        .optional()?;
    if present.is_none() {
        return Ok(None);
    }
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM container_jobs WHERE run_id=?1 AND state!='cleaned')",
        [job],
        |r| r.get(0),
    )?;
    ensure!(
        !pending,
        "worker container cleanup must be verified before retiring its local execution"
    );
    let now = crate::db::now();
    let reason = "worker interrupted; explicitly recovered after verified process/container cleanup; command not replayed";
    tx.execute("UPDATE stage_runs SET status='infrastructure_error',finished_at=?2,cancel_actor=COALESCE(cancel_actor,'worker_lost'),reason=COALESCE(reason,?3) WHERE run_id=?1 AND status IN ('queued','preparing','running','collecting') AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND finished_at IS NULL)",params![job,now,reason])?;
    tx.execute("UPDATE runs SET status='infrastructure_error',finished_at=?2,cancel_actor=COALESCE(cancel_actor,'worker_lost'),reason=COALESCE(reason,?3) WHERE id=?1 AND finished_at IS NULL",params![job,now,reason])?;
    tx.execute("INSERT OR IGNORE INTO completion_events(run_id,project_id,status,finished_at) SELECT id,project_id,status,finished_at FROM runs WHERE id=?1 AND finished_at IS NOT NULL",[job])?;
    tx.commit()?;
    drop(c);
    Ok(Some(db.run(job)?.status))
}
fn public_recovery(
    receipt: &serde_json::Value,
    replayed: bool,
    local: Option<crate::model::RunState>,
) -> serde_json::Value {
    serde_json::json!({"job_id":receipt["job_id"],"worker_id":receipt["worker_id"],"resolution_request_id":receipt["proof"]["operator_resolution"]["request_id"],"journal_retired":true,"already_recovered":replayed,"command_replayed":false,"evidence_retained":true,"local_execution_status":local})
}
pub async fn recover(
    gateway: &str,
    root: &Path,
    job: &str,
    process_stopped: bool,
) -> Result<serde_json::Value> {
    ensure!(
        process_stopped,
        "explicit --process-stopped attestation is required"
    );
    uuid::Uuid::parse_str(job)?;
    no_symlink(root)?;
    let root = root.canonicalize()?;
    let (_lock, mut state) = recovery_journal(&root)?;
    ensure!(
        state.gateway == gateway_url(gateway)?.as_str(),
        "worker identity belongs to a different gateway"
    );
    let directory = root.join("recoveries");
    no_symlink(&directory)?;
    let receipt_path = directory.join(format!("{job}.json"));
    no_symlink(&receipt_path)?;
    let prior = if receipt_path.exists() {
        ensure!(
            std::fs::metadata(&receipt_path)?.len() <= 128 * 1024,
            "recovery receipt is too large"
        );
        Some(serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(&receipt_path)?,
        )?)
    } else {
        None
    };
    if let Some(receipt) = &prior {
        ensure!(
            receipt["version"] == 1
                && receipt["process_stopped"] == true
                && receipt["job_id"] == job
                && receipt["worker_id"] == serde_json::json!(state.worker_id)
                && receipt["gateway"] == state.gateway
                && receipt["proof"]["recovery_allowed"] == true,
            "recovery receipt identity mismatch"
        );
        if state.pending.as_ref().and_then(|p| p.job_id.as_deref()) != Some(job) {
            cleanup_containers(&root, job, false).await?;
            return Ok(public_recovery(
                receipt,
                true,
                retire_local_execution(&root, job)?,
            ));
        }
    }
    let pending = state
        .pending
        .as_ref()
        .context("no pending job journal to recover")?;
    ensure!(
        pending.job_id.as_deref() == Some(job),
        "recovery must name the exact pending job"
    );
    if let Some(receipt) = &prior {
        ensure!(
            receipt["pending"] == serde_json::to_value(pending)?,
            "pending journal differs from its retained recovery evidence"
        );
    }
    cleanup_containers(&root, job, false).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let response=client.post(format!("{}worker/v1/jobs/recovery",state.gateway)).bearer_auth(&state.credential).json(&serde_json::json!({"protocol":crate::worker_jobs::PROTOCOL,"job_id":job,"claim_secret":pending.claim_secret,"process_stopped":true})).send().await?;
    ensure!(
        response.status().is_success(),
        "server rejected journal recovery (HTTP {}); retain journal and inspect worker_recovery_status",
        response.status()
    );
    let proof: serde_json::Value = response.json().await?;
    ensure!(
        proof["recovery_allowed"] == true
            && proof["job_state"] == "lost"
            && proof["lease_state"] == "released"
            && proof["operator_resolution"]["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
            && proof["job_id"] == job
            && proof["worker_id"] == serde_json::json!(state.worker_id),
        "server recovery proof identity mismatch"
    );
    let receipt = serde_json::json!({"version":1,"process_stopped":true,"job_id":job,"worker_id":state.worker_id,"gateway":state.gateway,"proof":proof,"pending":pending});
    if prior.is_none() {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if !directory.exists() {
            builder.create(&directory)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(&directory)?.permissions().mode() & 0o077 == 0,
                "recovery evidence directory must be private"
            );
        }
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(&serde_json::to_vec(&receipt)?)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&receipt_path)?;
        #[cfg(unix)]
        std::fs::File::open(&directory)?.sync_all()?;
    }
    let local = retire_local_execution(&root, job)?;
    state.pending = None;
    save(&root, &state)?;
    Ok(public_recovery(
        prior.as_ref().unwrap_or(&receipt),
        prior.is_some(),
        local,
    ))
}

pub async fn connect(gateway: &str, root: &Path, once: bool, job_once: bool) -> Result<()> {
    connect_mode(gateway, root, once, job_once, false).await
}
/// Managed SSH binds a dedicated worker CLI process to its transport's stdin lifetime.
pub async fn ssh_session(gateway: &str, root: &Path, once: bool, resume_only: bool) -> Result<()> {
    if resume_only {
        validate_service_identity(gateway, root)?;
    }
    connect_mode(gateway, root, once, false, true).await
}
fn watch_session_eof<R: std::io::Read + Send + 'static>(
    mut input: R,
    stop: tokio_util::sync::CancellationToken,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ssh-session-stdin".into())
        .spawn(move || {
            let mut bytes = [0u8; 128];
            loop {
                match input.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            stop.cancel();
        })
}
async fn connect_mode(
    gateway: &str,
    root: &Path,
    once: bool,
    job_once: bool,
    session: bool,
) -> Result<()> {
    let stop = StopSignals::start()?;
    let url = gateway_url(gateway)?;
    no_symlink(root)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "worker state directory must be private (mode 0700)"
        );
    }
    no_symlink(&root.join("client.lock"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let _lock = loop {
        match StateLock::acquire(&root.join("client.lock")) {
            Ok(lock) => break lock,
            Err(error)
                if session
                    && error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                    && tokio::time::Instant::now() < deadline =>
            {
                // Wait for an old SSH session to retire; never expire or steal its lock.
                tokio::select! { _=stop.token.cancelled()=>anyhow::bail!("SSH session stopped before worker ownership was available"), _=tokio::time::sleep(std::time::Duration::from_millis(50))=>{} }
            }
            Err(error) => return Err(error).context("worker state is already in use"),
        }
    };
    check_identity(root)?;
    let mut state = match std::fs::read(root.join("identity.json")) {
        Ok(bytes) => serde_json::from_slice::<State>(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let state = State {
                gateway: url.to_string(),
                credential: crate::workers::secret()?,
                worker_id: None,
                sequence: 0,
                pending: None,
            };
            save(root, &state)?;
            state
        }
        Err(e) => return Err(e.into()),
    };
    ensure!(
        state.gateway == url.as_str(),
        "worker identity belongs to a different gateway"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let base = gateway.trim_end_matches('/');
    if state.worker_id.is_none() {
        eprintln!(
            "Read enrollment token from stdin (one line). Worker commands are accepted only from the authenticated server."
        );
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin()).take(66);
        let mut token = String::new();
        reader.read_line(&mut token).await?;
        let token = token.trim();
        ensure!(token.len() == 64, "invalid enrollment token length");
        let response = client
            .post(format!("{base}/worker/v1/enroll"))
            .json(&crate::workers::Enroll {
                protocol: crate::workers::PROTOCOL,
                enrollment_token: token.into(),
                credential: state.credential.clone(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
            })
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "worker enrollment failed (HTTP {}); check token expiry and platform",
            response.status()
        );
        let reply: serde_json::Value = response.json().await?;
        let id = reply["worker_id"]
            .as_str()
            .context("enrollment reply missing worker_id")?;
        uuid::Uuid::parse_str(id).context("invalid worker identity in response")?;
        state.worker_id = Some(id.into());
        save(root, &state)?;
    }
    if let Some(pending) = &state.pending {
        ensure!(
            pending.phase != Phase::Running,
            "worker journal records interrupted execution; inspect/stop the old process before operator recovery; no command will be repeated"
        );
        if pending.phase == Phase::Finished {
            report_pending(root, &mut state, &client, base).await?;
        }
    }
    if session {
        // Dedicated CLI-process thread: Tokio's blocking stdin read cannot be aborted.
        // Transport EOF ends it; process exit retires it on every other return path.
        let _ = watch_session_eof(std::io::stdin(), stop.token.clone())?;
    }
    let job_protocol = if crate::worker_runtime::load(root)?.is_some() {
        crate::worker_jobs::CONTAINER_PROTOCOL
    } else {
        crate::worker_jobs::PROTOCOL
    };
    let mut announced = false;
    loop {
        state.sequence = state
            .sequence
            .checked_add(1)
            .context("heartbeat sequence exhausted")?;
        save(root, &state)?;
        let response = client
            .post(format!("{base}/worker/v1/heartbeat"))
            .bearer_auth(&state.credential)
            .json(&crate::workers::Heartbeat {
                job_protocol: Some(job_protocol),
                protocol: crate::workers::PROTOCOL,
                sequence: state.sequence,
            })
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "worker heartbeat failed (HTTP {}); reconnect or check revocation",
            response.status()
        );
        let reply: serde_json::Value = response.json().await?;
        ensure!(
            reply["worker_id"].as_str() == state.worker_id.as_deref()
                && reply["accepted_sequence"].as_u64() == Some(state.sequence),
            "heartbeat reply identity/sequence mismatch"
        );
        ensure!(
            reply["requires_recovery"] != true,
            "worker has abandoned job operations; inspect operations_abandoned and verify processes stopped before operation_resolve; retained journal prevents replay"
        );
        ensure!(
            reply["job_protocol"] == job_protocol && reply["execution_ready"] == true,
            "worker job protocol mismatch; update server and worker packages before accepting jobs"
        );
        if once {
            println!(
                "{}",
                serde_json::json!({"worker_id":state.worker_id,"authenticated_heartbeat":true,"execution_ready":true,"job_protocol":job_protocol})
            );
            break;
        }
        if !job_once && !announced {
            println!(
                "{}",
                serde_json::json!({"worker_id":state.worker_id,"authenticated_heartbeat":true,"native_jobs_ready":true,"job_protocol":job_protocol})
            );
            announced = true;
        }
        if stop.token.is_cancelled() {
            break;
        }
        let worked = poll_job(root, &mut state, &client, base, &stop.token, session).await?;
        if job_once {
            if !worked {
                println!("{}", serde_json::json!({"job":null}));
            }
            break;
        }
        tokio::select! { _=stop.token.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(10))=>{} }
    }
    Ok(())
}

pub async fn fetch(root: &Path, transfer: &str, digest: &str) -> Result<()> {
    uuid::Uuid::parse_str(transfer).context("invalid transfer ID")?;
    crate::modules::validate_digest(digest)?;
    no_symlink(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "worker state directory must be private"
        );
    }
    check_identity(root)?;
    let state: State = serde_json::from_slice(&std::fs::read(root.join("identity.json"))?)?;
    ensure!(state.worker_id.is_some(), "worker is not enrolled");
    let verified = download(root, &state, transfer, digest).await?;
    let logical_bytes = verified.manifest.size();
    let path = verified.directory.keep();
    println!(
        "{}",
        serde_json::json!({"worker_id":state.worker_id,"transfer_id":transfer,"tree_sha256":digest,"path":path,"logical_bytes":logical_bytes,"verified":true})
    );
    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Claiming,
    Running,
    Finished,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    claim_secret: String,
    job_id: Option<String>,
    phase: Phase,
    outcome: Option<crate::worker_jobs::Outcome>,
}
#[derive(Deserialize)]
struct Job {
    job_id: String,
    #[serde(default)]
    runtime_receipt: Option<String>,
    #[serde(default)]
    log_stream: bool,
    spec: crate::worker_jobs::Spec,
    transfer_id: String,
    tree_sha256: String,
    cancel: bool,
    renew_sequence: u64,
}
async fn job_request(
    client: &reqwest::Client,
    base: &str,
    credential: &str,
    operation: &str,
    body: &impl Serialize,
) -> Result<serde_json::Value> {
    let response = client
        .post(format!("{base}/worker/v1/jobs/{operation}"))
        .bearer_auth(credential)
        .json(body)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "worker job {operation} failed (HTTP {}); retained journal prevents automatic re-execution",
        response.status()
    );
    Ok(response.json().await?)
}
async fn report_pending(
    root: &Path,
    state: &mut State,
    client: &reqwest::Client,
    base: &str,
) -> Result<()> {
    let pending = state.pending.as_ref().context("missing pending job")?;
    let reply = job_request(
        client,
        base,
        &state.credential,
        "finish",
        &crate::worker_jobs::Finish {
            protocol: crate::worker_jobs::PROTOCOL,
            job_id: pending.job_id.clone().context("missing pending job ID")?,
            claim_secret: pending.claim_secret.clone(),
            outcome: pending.outcome.clone().context("missing pending outcome")?,
        },
    )
    .await?;
    state.pending = None;
    save(root, state)?;
    println!("{reply}");
    Ok(())
}
async fn poll_job(
    root: &Path,
    state: &mut State,
    client: &reqwest::Client,
    base: &str,
    stop: &tokio_util::sync::CancellationToken,
    session: bool,
) -> Result<bool> {
    if let Some(pending) = &state.pending {
        ensure!(
            pending.phase != Phase::Running,
            "worker journal records interrupted execution; inspect/stop the old process before operator recovery; no command will be repeated"
        );
        if pending.phase == Phase::Finished {
            report_pending(root, state, client, base).await?;
            return Ok(true);
        }
    } else {
        state.pending = Some(Pending {
            claim_secret: crate::workers::secret()?,
            job_id: None,
            phase: Phase::Claiming,
            outcome: None,
        });
        save(root, state)?;
    }
    let claim = state.pending.as_ref().unwrap().claim_secret.clone();
    let reply = job_request(
        client,
        base,
        &state.credential,
        "claim",
        &crate::worker_jobs::Claim {
            protocol: if crate::worker_runtime::load(root)?.is_some() {
                crate::worker_jobs::CONTAINER_PROTOCOL
            } else {
                crate::worker_jobs::PROTOCOL
            },
            claim_secret: claim.clone(),
        },
    )
    .await?;
    if reply.get("job").is_some_and(serde_json::Value::is_null) {
        state.pending = None;
        save(root, state)?;
        return Ok(false);
    }
    let job: Job = serde_json::from_value(reply["job"].clone())?;
    uuid::Uuid::parse_str(&job.job_id)?;
    state.pending.as_mut().unwrap().job_id = Some(job.job_id.clone());
    save(root, state)?;
    let first_sequence = job
        .renew_sequence
        .checked_add(1)
        .context("renew sequence exhausted")?;
    let first = job_request(
        client,
        base,
        &state.credential,
        "renew",
        &crate::worker_jobs::Renew {
            protocol: crate::worker_jobs::PROTOCOL,
            job_id: job.job_id.clone(),
            claim_secret: claim.clone(),
            sequence: first_sequence,
        },
    )
    .await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    if job.cancel || first["cancel"] == true || stop.is_cancelled() {
        cancel.cancel();
    }
    let done = tokio_util::sync::CancellationToken::new();
    let credential = state.credential.clone();
    let watchdog = async {
        let mut sequence = first_sequence;
        loop {
            tokio::select! {biased;_=done.cancelled()=>return Ok::<_,anyhow::Error>(()),_=stop.cancelled()=>{cancel.cancel();return Ok(());},_=tokio::time::sleep(std::time::Duration::from_secs(3))=>{}}
            sequence = sequence
                .checked_add(1)
                .context("renew sequence exhausted")?;
            let renewal = crate::worker_jobs::Renew {
                protocol: crate::worker_jobs::PROTOCOL,
                job_id: job.job_id.clone(),
                claim_secret: claim.clone(),
                sequence,
            };
            let renewed = tokio::select! {biased;_=done.cancelled()=>return Ok(()),r=job_request(client,base,&credential,"renew",&renewal)=>r};
            match renewed {
                Ok(v) => {
                    if v["cancel"] == true {
                        cancel.cancel();
                    }
                }
                Err(error) => {
                    cancel.cancel();
                    return Err(error);
                }
            }
        }
    };
    state.pending.as_mut().unwrap().phase = Phase::Running;
    save(root, state)?;
    let execution_done = tokio_util::sync::CancellationToken::new();
    let work = async {
        let result = execute_job(root, state, &job, &cancel).await;
        execution_done.cancel();
        result
    };
    let forward = async {
        let result = if job.log_stream {
            let mut pump = match LogPump::new(root, &job.job_id) {
                Ok(v) => v,
                Err(error) => {
                    cancel.cancel();
                    done.cancel();
                    return Err(error);
                }
            };
            loop {
                if let Err(error) = pump.drain(client, base, &credential, &claim).await {
                    cancel.cancel();
                    break Err(error);
                }
                tokio::select! {biased;_=execution_done.cancelled()=>break pump.drain(client,base,&credential,&claim).await.map(|()|Some((pump.progress.after,pump.progress.closed))),_=tokio::time::sleep(std::time::Duration::from_millis(200))=>{}}
            }
        } else {
            execution_done.cancelled().await;
            Ok(None)
        };
        done.cancel();
        result
    };
    let (execution, monitor, logs) = tokio::join!(work, watchdog, forward);
    let mut outcome = match (execution, monitor) {
        (Ok(outcome), Ok(())) => outcome,
        (Err(e), _) | (_, Err(e)) => crate::worker_jobs::Outcome {
            runtime_receipt: None,
            log_cursor: None,
            status: crate::model::RunState::InfrastructureError,
            exit: None,
            stdout: String::new(),
            stderr: e.to_string().chars().take(1024).collect(),
            truncated: false,
        },
    };
    match logs {
        Ok(receipt) => {
            outcome.log_cursor = receipt.map(|(cursor, _)| cursor);
            if let Some((_, truncated)) = receipt {
                outcome.truncated |= truncated;
                outcome.stdout.clear();
                if outcome.status != crate::model::RunState::InfrastructureError {
                    outcome.stderr.clear();
                }
            }
        }
        Err(error) => {
            outcome.status = crate::model::RunState::InfrastructureError;
            outcome.exit = None;
            outcome.stdout.clear();
            outcome.stderr = error.to_string().chars().take(1024).collect();
            outcome.truncated = true;
        }
    }
    if container_pending(root, &job.job_id)? {
        state.pending.as_mut().unwrap().phase = Phase::Running;
        save(root, state)?;
        anyhow::bail!(
            "worker container cleanup pending; retained workspace/journal block further jobs; inspect worker journal and recover explicitly"
        );
    }
    if session && stop.is_cancelled() {
        // Transport loss is not a user cancellation; retain the claim for fenced recovery.
        state.pending.as_mut().unwrap().phase = Phase::Running;
        save(root, state)?;
        anyhow::bail!(
            "SSH session ended during a claimed job; execution stopped, explicit journal recovery required"
        );
    }
    let pending = state.pending.as_mut().unwrap();
    pending.phase = Phase::Finished;
    pending.outcome = Some(outcome);
    save(root, state)?;
    report_pending(root, state, client, base).await?;
    Ok(true)
}
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LogProgress {
    after: i64,
    #[serde(default)]
    closed: bool,
    pending: Option<crate::worker_logs::Frame>,
}
struct LogPump {
    database: std::path::PathBuf,
    path: std::path::PathBuf,
    job: String,
    progress: LogProgress,
}
impl LogPump {
    fn new(root: &Path, job: &str) -> Result<Self> {
        let directory = root.join("jobs");
        no_symlink(&directory)?;
        let path = directory.join(format!("{job}.logs.json"));
        no_symlink(&path)?;
        let progress = if path.exists() {
            ensure!(
                std::fs::metadata(&path)?.len() <= 512 * 1024,
                "worker log journal exceeds limit"
            );
            serde_json::from_slice(&std::fs::read(&path)?)?
        } else {
            LogProgress::default()
        };
        Ok(Self {
            database: directory.join(format!("{job}.sqlite3")),
            path,
            job: job.into(),
            progress,
        })
    }
    fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("worker log journal directory")?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec(&self.progress)?)?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
    async fn drain(
        &mut self,
        client: &reqwest::Client,
        base: &str,
        credential: &str,
        claim: &str,
    ) -> Result<()> {
        if self.progress.closed {
            return Ok(());
        }
        no_symlink(&self.database)?;
        if !self.database.exists() {
            return Ok(());
        }
        let db = crate::db::Db::new(&self.database)?;
        loop {
            if self.progress.pending.is_none() {
                let page = db.logs(&self.job, self.progress.after)?;
                if page.events.is_empty() {
                    return Ok(());
                }
                let mut bytes = 0;
                let mut events = vec![];
                for event in page.events {
                    if bytes + event.text.len() > crate::worker_logs::FRAME_BYTES {
                        break;
                    }
                    bytes += event.text.len();
                    events.push(crate::worker_logs::Event {
                        id: event.id,
                        stream: event.stream,
                        sequence: event.sequence,
                        timestamp: event.timestamp,
                        text: event.text,
                    });
                }
                ensure!(!events.is_empty(), "worker log event exceeds frame limit");
                self.progress.pending = Some(crate::worker_logs::Frame {
                    after: self.progress.after,
                    events,
                });
                self.save()?;
            }
            let frame = self.progress.pending.as_ref().unwrap();
            ensure!(
                frame.after == self.progress.after,
                "worker log journal cursor mismatch"
            );
            let request = crate::worker_logs::Request {
                protocol: crate::worker_jobs::PROTOCOL,
                job_id: self.job.clone(),
                claim_secret: claim.into(),
                frame: frame.clone(),
            };
            let response = match job_request(client, base, credential, "logs", &request).await {
                Ok(v) => v,
                Err(_) => job_request(client, base, credential, "logs", &request).await?,
            };
            let expected = frame.events.last().context("empty worker log frame")?.id;
            ensure!(
                response["job_id"] == self.job && response["cursor"] == expected,
                "worker log acknowledgement identity mismatch"
            );
            self.progress.after = expected;
            self.progress.pending = None;
            self.progress.closed = response["truncated"] == true;
            self.save()?;
            if self.progress.closed {
                return Ok(());
            }
        }
    }
}
fn snapshot(db: &crate::db::Db, bytes: &[u8]) -> String {
    let mut filter = crate::secrets::Filter::new(db.secret_values());
    let mut output = filter.push(bytes, false);
    output.extend(filter.truncate());
    let mut text = db.redact(&String::from_utf8_lossy(&output));
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1
    }
    text.truncate(end);
    text
}
struct WorkspaceGuard {
    directory: Option<tempfile::TempDir>,
    db: std::sync::Arc<crate::db::Db>,
}
impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        let pending = self
            .db
            .connection()
            .and_then(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM container_jobs WHERE state!='cleaned')",
                    [],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(true);
        if pending && let Some(directory) = self.directory.take() {
            let _ = directory.keep();
        }
    }
}
fn container_pending(root: &Path, job: &str) -> Result<bool> {
    let path = root.join("jobs").join(format!("{job}.sqlite3"));
    no_symlink(&path)?;
    if !path.exists() {
        return Ok(false);
    }
    let db = crate::db::Db::new(&path)?;
    Ok(db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM container_jobs WHERE state!='cleaned')",
        [],
        |r| r.get(0),
    )?)
}
async fn cleanup_containers(root: &Path, job: &str, settled: bool) -> Result<()> {
    let path = root.join("jobs").join(format!("{job}.sqlite3"));
    no_symlink(&path)?;
    if !path.exists() {
        return Ok(());
    }
    let db = crate::db::Db::new(&path)?;
    if !container_pending(root, job)? {
        return Ok(());
    }
    let policy = crate::worker_runtime::load(root)?
        .context("restore worker runtime policy before container recovery")?;
    let config = policy.config(root, 16)?;
    crate::docker::cleanup_worker(&db, &config, job, settled).await?;
    let paths = db
        .connection()?
        .prepare(
            "SELECT DISTINCT workspace FROM container_jobs WHERE run_id=?1 AND state='cleaned'",
        )?
        .query_map([job], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for path in paths {
        let path = std::path::PathBuf::from(path);
        no_symlink(&path)?;
        ensure!(
            path.parent() == Some(root),
            "worker cleanup path escapes state root"
        );
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}
pub async fn cleanup_container_journal(
    root: &Path,
    job: &str,
    settled: bool,
) -> Result<serde_json::Value> {
    uuid::Uuid::parse_str(job)?;
    let root = root.canonicalize()?;
    let (_lock, state) = recovery_journal(&root)?;
    ensure!(
        state.pending.as_ref().and_then(|p| p.job_id.as_deref()) == Some(job),
        "cleanup requires exact pending worker job"
    );
    cleanup_containers(&root, job, settled).await?;
    Ok(serde_json::json!({"job_id":job,"containers_cleaned":true,"journal_retained":true}))
}
async fn execute_job(
    root: &Path,
    state: &State,
    job: &Job,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<crate::worker_jobs::Outcome> {
    use crate::model::{RunState, StartBuild};
    use rusqlite::OptionalExtension;
    ensure!(
        job.runtime_receipt
            .as_deref()
            .is_none_or(|format| format == crate::worker_receipts::FORMAT),
        "unsupported worker runtime receipt format"
    );
    let cancelled = || crate::worker_jobs::Outcome {
        runtime_receipt: None,
        log_cursor: None,
        status: RunState::Cancelled,
        exit: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
    };
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    ensure!(
        (1..=3600).contains(&job.spec.command.timeout_seconds),
        "invalid worker timeout"
    );
    let tree = tokio::select! {biased;_=cancel.cancelled()=>return Ok(cancelled()),r=download(root,state,&job.transfer_id,&job.tree_sha256)=>r?};
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let canonical_workspace = tree.directory.path().canonicalize()?;
    let workspace = canonical_workspace.as_path();
    let cwd = crate::pipeline::contained(workspace, &job.spec.cwd)?;
    let mut command = job.spec.command.clone();
    let mut env = job.spec.env.clone();
    crate::remote_stage::expand(workspace, &mut command, &mut env)?;
    let database = root.join("jobs");
    no_symlink(&database)?;
    std::fs::create_dir_all(&database)?;
    let path = database.join(format!("{}.sqlite3", job.job_id));
    no_symlink(&path)?;
    let db = std::sync::Arc::new(crate::db::Db::new(&path)?);
    db.remember_secrets(vec![
        state.credential.clone(),
        state
            .pending
            .as_ref()
            .context("missing job journal")?
            .claim_secret
            .clone(),
    ]);
    let _workspace = WorkspaceGuard {
        directory: Some(tree.directory),
        db: db.clone(),
    };
    let container = if let Some(request) = &job.spec.container {
        let policy =
            crate::worker_runtime::load(root)?.context("worker Docker execution is disabled")?;
        let config = policy.config(root, request.memory_mib)?;
        let mut stage = crate::pipeline::StageDefinition::native(command.clone());
        stage.executor = crate::pipeline::Executor::Docker;
        stage.id = "native".into();
        stage.container = Some(request.spec.clone());
        crate::docker::context(&config, &job.job_id, &stage)?.map(std::sync::Arc::new)
    } else {
        None
    };
    let project = db.register("worker-job", workspace)?;
    db.insert_run(
        &job.job_id,
        &StartBuild {
            project_id: project.id,
            command: job.spec.command.clone(),
        },
        workspace,
    )?;
    db.transition(&job.job_id, RunState::Preparing, None, None, None)?;
    db.transition(&job.job_id, RunState::Running, None, None, None)?;
    db.stage_transition(&job.job_id, "native", RunState::Preparing, None, None)?;
    db.stage_transition(&job.job_id, "native", RunState::Running, None, None)?;
    let receipt = if job.runtime_receipt.is_some() {
        let spec = job.spec.clone();
        let tree = job.tree_sha256.clone();
        let program = command.program.clone();
        let workspace = workspace.to_owned();
        let cwd = cwd.clone();
        let env = env.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                crate::worker_receipts::observe(&spec, &tree, &program, &workspace, &cwd, &env)
            })
            .await??,
        )
    } else {
        None
    };
    let output = crate::executor::execute_captured(
        &db,
        crate::executor::Execution {
            container: container.as_deref(),
            log_mode: crate::executor::LogMode::All,
            verified_executable: None,
            run_id: &job.job_id,
            stage_id: "native",
            workspace,
            cwd: &cwd,
            command: &command,
            stdin: None,
            capture_limit: if job.log_stream { 1024 * 1024 } else { 4096 },
            env: &env,
        },
        cancel,
    )
    .await?;
    db.stage_transition(
        &job.job_id,
        "native",
        RunState::Collecting,
        output.exit,
        None,
    )?;
    db.finish_stage(
        &job.job_id,
        "native",
        output.status,
        output.exit,
        None,
        None,
        &[],
    )?;
    db.transition(&job.job_id, RunState::Collecting, output.exit, None, None)?;
    db.transition(&job.job_id, output.status, output.exit, None, None)?;
    if !job.spec.returns.is_empty()
        && matches!(output.status, RunState::Succeeded | RunState::Failed)
    {
        let source = workspace.to_path_buf();
        let returns = job.spec.returns.clone();
        let parent = root.to_path_buf();
        let (bundle, digest) = tokio::task::spawn_blocking(move || {
            crate::worker_outputs::pack(&source, &returns, &parent)
        })
        .await??;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let file = tokio::fs::File::open(bundle.path()).await?;
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
        let upload = client
            .put(format!(
                "{}worker/v1/jobs/{}/outputs",
                state.gateway, job.job_id
            ))
            .bearer_auth(&state.credential)
            .header(
                "x-preflight-claim",
                &state
                    .pending
                    .as_ref()
                    .context("missing job journal")?
                    .claim_secret,
            )
            .header("x-preflight-tree-sha256", &digest)
            .body(body)
            .send();
        let response = tokio::select! { biased; _=cancel.cancelled()=>return Ok(cancelled()), result=upload=>result? };
        ensure!(
            response.status().is_success(),
            "worker output upload rejected (HTTP {})",
            response.status()
        );
        let receipt: serde_json::Value = response.json().await?;
        ensure!(
            receipt["job_id"] == job.job_id && receipt["tree_sha256"] == digest,
            "worker output receipt mismatch"
        );
    }
    let mut receipt = receipt;
    if let Some(request) = &job.spec.container {
        let image: Option<String> = db.connection()?.query_row("SELECT actual_image_id FROM container_jobs WHERE run_id=?1 ORDER BY created_at DESC LIMIT 1",[&job.job_id],|r|r.get(0)).optional()?.flatten();
        if let (Some(receipt), Some(image_id)) = (&mut receipt, image) {
            receipt.container = Some(crate::worker_receipts::Container {
                image_id,
                platform: request.spec.platform.clone(),
                user: request.spec.effective_user(),
            });
        } else {
            receipt = None;
        }
    }
    Ok(crate::worker_jobs::Outcome {
        runtime_receipt: receipt,
        log_cursor: None,
        status: output.status,
        exit: output.exit,
        stdout: snapshot(&db, &output.stdout),
        stderr: snapshot(&db, &output.stderr),
        truncated: output.truncated,
    })
}

async fn download(
    root: &Path,
    state: &State,
    transfer: &str,
    digest: &str,
) -> Result<crate::worker_transfer::VerifiedTree> {
    use std::io::Seek;
    uuid::Uuid::parse_str(transfer)?;
    crate::modules::validate_digest(digest)?;
    let gateway = gateway_url(&state.gateway)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let mut response = client
        .get(format!(
            "{}/worker/v1/transfers/{transfer}",
            gateway.as_str().trim_end_matches('/')
        ))
        .bearer_auth(&state.credential)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "worker download failed (HTTP {})",
        response.status()
    );
    if let Some(len) = response.content_length() {
        ensure!(
            len <= crate::worker_transfer::MAX_WIRE,
            "transfer wire size exceeds limit"
        );
    }
    let mut spool = tempfile::NamedTempFile::new_in(root)?;
    let mut bytes = 0u64;
    while let Some(chunk) = response.chunk().await? {
        bytes = bytes
            .checked_add(chunk.len() as u64)
            .context("transfer size overflow")?;
        ensure!(
            bytes <= crate::worker_transfer::MAX_WIRE,
            "transfer wire size exceeds limit"
        );
        spool.write_all(&chunk)?;
    }
    spool.as_file_mut().rewind()?;
    let expected = digest.to_owned();
    let parent = root.to_path_buf();
    let verified = tokio::task::spawn_blocking(move || {
        crate::worker_transfer::decode(spool.as_file_mut(), &expected, &parent)
    })
    .await??;
    Ok(verified)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[tokio::test]
    async fn unknown_runtime_receipt_format_never_downloads_or_executes() {
        let root = tempfile::tempdir().unwrap();
        let state = State {
            gateway: "http://127.0.0.1:1/".into(),
            worker_id: None,
            credential: "unused".into(),
            sequence: 0,
            pending: None,
        };
        let job=Job {job_id:uuid::Uuid::now_v7().to_string(),runtime_receipt:Some("unknown-format".into()),log_stream:false,spec:serde_json::from_value(serde_json::json!({"command":{"program":"sh","args":["-c","touch must-not-run"],"timeout_seconds":5},"cwd":".","env":{}})).unwrap(),transfer_id:"unused".into(),tree_sha256:"a".repeat(64),cancel:false,renew_sequence:0};
        let error = execute_job(
            root.path(),
            &state,
            &job,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported worker runtime receipt format")
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn recovered_ledger_is_terminal_idempotent_and_preserves_completed_results() {
        use crate::model::{CommandSpec, RunState, StartBuild};
        for completed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("jobs");
            std::fs::create_dir(&directory).unwrap();
            let db = crate::db::Db::new(&directory.join("job.sqlite3")).unwrap();
            let project = db.register("owned", root.path()).unwrap();
            db.insert_run(
                "job",
                &StartBuild {
                    project_id: project.id,
                    command: CommandSpec {
                        program: "sh".into(),
                        args: vec![],
                        timeout_seconds: 5,
                    },
                },
                root.path(),
            )
            .unwrap();
            db.transition("job", RunState::Preparing, None, None, None)
                .unwrap();
            db.transition("job", RunState::Running, None, None, None)
                .unwrap();
            if completed {
                db.transition(
                    "job",
                    RunState::Cancelled,
                    None,
                    Some("user"),
                    Some("requested"),
                )
                .unwrap();
            }
            let expected = if completed {
                RunState::Cancelled
            } else {
                RunState::InfrastructureError
            };
            assert_eq!(
                retire_local_execution(root.path(), "job").unwrap(),
                Some(expected)
            );
            let first = serde_json::to_value(db.run("job").unwrap()).unwrap();
            assert_eq!(
                retire_local_execution(root.path(), "job").unwrap(),
                Some(expected)
            );
            assert_eq!(serde_json::to_value(db.run("job").unwrap()).unwrap(), first);
            let c = db.connection().unwrap();
            assert_eq!(
                c.query_row("SELECT count(*) FROM completion_events", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM stage_runs WHERE finished_at IS NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            c.execute("INSERT INTO operation_leases VALUES ('lease','job','docker_container','abandoned',0,NULL)", []).unwrap();
            c.execute("INSERT INTO container_jobs(id,run_id,stage_id,lease_id,platform,endpoint,name,owner_token,image,workspace,state,created_at) VALUES ('container','job','native','lease','linux/arm64','unix:///owned','owned','owned','sha256:owned','/owned','cleanup_pending',0)", []).unwrap();
            drop(c);
            assert!(retire_local_execution(root.path(), "job").is_err());
            assert_eq!(serde_json::to_value(db.run("job").unwrap()).unwrap(), first);
            let c = db.connection().unwrap();
            c.execute("UPDATE container_jobs SET state='cleaned'", [])
                .unwrap();
            drop(c);
            drop(db);
            std::fs::copy(
                directory.join("job.sqlite3"),
                directory.join("foreign.sqlite3"),
            )
            .unwrap();
            assert!(retire_local_execution(root.path(), "foreign").is_err());
            assert_eq!(retire_local_execution(root.path(), "absent").unwrap(), None);
        }
    }

    #[test]
    fn mounted_workspace_survives_uncertain_container_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir_in(root.path()).unwrap();
        let path = directory.path().to_owned();
        let db = std::sync::Arc::new(crate::db::Db::new(&root.path().join("db")).unwrap());
        let project = db.register("owned", &path).unwrap();
        db.insert_run(
            "run",
            &crate::model::StartBuild {
                project_id: project.id,
                command: crate::model::CommandSpec {
                    program: "sh".into(),
                    args: vec![],
                    timeout_seconds: 5,
                },
            },
            &path,
        )
        .unwrap();
        let lease = db
            .begin_operation("run", "docker_container", Default::default())
            .unwrap();
        db.connection().unwrap().execute("INSERT INTO container_jobs(id,run_id,stage_id,lease_id,platform,endpoint,name,owner_token,image,workspace,state,created_at) VALUES ('container','run','native',?1,'linux/arm64','unix:///owned','owned-name','owned-token','sha256:owned',?2,'cleanup_pending',0)",rusqlite::params![lease.id,path.to_string_lossy()]).unwrap();
        drop(WorkspaceGuard {
            directory: Some(directory),
            db: db.clone(),
        });
        assert!(path.exists());
        db.connection()
            .unwrap()
            .execute("UPDATE container_jobs SET state='cleaned'", [])
            .unwrap();
        let fresh = tempfile::tempdir_in(root.path()).unwrap();
        let cleared = fresh.path().to_owned();
        drop(WorkspaceGuard {
            directory: Some(fresh),
            db: db.clone(),
        });
        assert!(!cleared.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_session_eof_cancels_only_when_the_transport_closes() {
        let (input, mut transport) = std::os::unix::net::UnixStream::pair().unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let watcher = watch_session_eof(input, stop.clone()).unwrap();
        std::io::Write::write_all(&mut transport, b"control data\n").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!stop.is_cancelled());
        drop(transport);
        tokio::time::timeout(std::time::Duration::from_secs(2), stop.cancelled())
            .await
            .unwrap();
        watcher.join().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn state_lock_releases_ownership_even_while_a_duplicate_descriptor_survives() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("client.lock");
        let owner = StateLock::acquire(&path).unwrap();
        let duplicate = owner.0.try_clone().unwrap();
        assert!(StateLock::acquire(&path).is_err());
        drop(owner);
        let successor = StateLock::acquire(&path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
        drop(duplicate);
        assert!(StateLock::acquire(&path).is_err());
        drop(successor);
        assert!(StateLock::acquire(&path).is_ok());
    }

    use serde_json::json;
    #[tokio::test]
    async fn recovery_retains_evidence_replays_after_crash_and_never_clears_another_job() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}/", listener.local_addr().unwrap());
        let worker = uuid::Uuid::now_v7().to_string();
        let job = uuid::Uuid::now_v7().to_string();
        let claim = crate::workers::secret().unwrap();
        let credential = crate::workers::secret().unwrap();
        let mut state = State {
            gateway: gateway.clone(),
            worker_id: Some(worker.clone()),
            credential: credential.clone(),
            sequence: 1,
            pending: Some(Pending {
                job_id: Some(job.clone()),
                claim_secret: claim.clone(),
                phase: Phase::Finished,
                outcome: Some(crate::worker_jobs::Outcome {
                    runtime_receipt: None,
                    log_cursor: None,
                    status: crate::model::RunState::InfrastructureError,
                    exit: None,
                    stdout: String::new(),
                    stderr: "connection lost".into(),
                    truncated: false,
                }),
            }),
        };
        save(&root, &state).unwrap();
        let valid = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let toggle = valid.clone();
        let expected_job = job.clone();
        let expected_worker = worker.clone();
        let app=axum::Router::new().route("/worker/v1/jobs/recovery",axum::routing::post(move||{let valid=toggle.clone();let job=expected_job.clone();let worker=expected_worker.clone();async move{axum::Json(json!({"recovery_allowed":valid.load(std::sync::atomic::Ordering::SeqCst),"job_id":job,"worker_id":worker,"job_state":"lost","lease_state":"released","operator_resolution":{"request_id":"resolved"}}))}}));
        let stop = tokio_util::sync::CancellationToken::new();
        let signal = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
                .unwrap();
        });
        let before = std::fs::read(root.join("identity.json")).unwrap();
        assert!(
            !journal_status(&root)
                .unwrap()
                .to_string()
                .contains(&credential)
        );
        assert!(!journal_status(&root).unwrap().to_string().contains(&claim));
        assert!(recover(&gateway, &root, &job, false).await.is_err());
        assert!(
            recover(&gateway, &root, &uuid::Uuid::now_v7().to_string(), true)
                .await
                .is_err()
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("client.lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert_eq!(journal_status(&root).unwrap()["pending"]["job_id"], job);
        assert!(recover(&gateway, &root, &job, true).await.is_err());
        FileExt::unlock(&lock).unwrap();
        drop(lock);
        assert!(recover(&gateway, &root, &job, true).await.is_err());
        assert_eq!(std::fs::read(root.join("identity.json")).unwrap(), before);
        valid.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = recover(&gateway, &root, &job, true).await.unwrap();
        assert_eq!(result["journal_retired"], true);
        assert!(!result.to_string().contains(&claim));
        assert!(!result.to_string().contains(&credential));
        let receipt = root.join("recoveries").join(format!("{job}.json"));
        let evidence = std::fs::read(&receipt).unwrap();
        let retained: serde_json::Value = serde_json::from_slice(&evidence).unwrap();
        assert_eq!(retained["pending"]["claim_secret"], claim);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&receipt).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert!(journal_status(&root).unwrap()["pending"].is_null());
        assert_eq!(
            recover(&gateway, &root, &job, true).await.unwrap()["already_recovered"],
            true
        );
        // Receipt persisted but identity update was interrupted: repeat without replacing evidence.
        save(&root, &state).unwrap();
        assert_eq!(
            recover(&gateway, &root, &job, true).await.unwrap()["already_recovered"],
            true
        );
        assert_eq!(std::fs::read(&receipt).unwrap(), evidence);
        state.pending.as_mut().unwrap().job_id = Some(uuid::Uuid::now_v7().to_string());
        save(&root, &state).unwrap();
        let different = std::fs::read(root.join("identity.json")).unwrap();
        assert_eq!(
            recover(&gateway, &root, &job, true).await.unwrap()["already_recovered"],
            true
        );
        assert_eq!(
            std::fs::read(root.join("identity.json")).unwrap(),
            different
        );
        stop.cancel();
        server.await.unwrap();
    }
}
