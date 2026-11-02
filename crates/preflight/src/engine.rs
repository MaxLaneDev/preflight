use crate::{
    artifacts,
    config::Config,
    db::Db,
    executor::{self, Execution},
    model::*,
    pipeline::{self, PipelineFile, StageDefinition, StageKind, StartPipeline},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct Engine {
    pub(crate) releases: crate::releases::Monitor,
    pub(crate) auth_sessions: crate::auth::Sessions,
    pub(crate) policy_gate: tokio::sync::Mutex<()>,
    pub(crate) transfer_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) live_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) queue_gate: Mutex<()>,
    pub(crate) gc_gate: Mutex<()>,
    pub(crate) schedule_gate: Mutex<()>,
    pub db: Db,
    /// Startup identity; use current_config() for mutable operator settings.
    pub config: Config,
    pub(crate) container_cleanup_gate: tokio::sync::Mutex<()>,
    pub(crate) live_config: std::sync::RwLock<Config>,
    pub config_file: std::sync::OnceLock<std::path::PathBuf>,
    pub(crate) settings_gate: Mutex<()>,
    pub(crate) active: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub(crate) flights: crate::single_flight::Pool,
    pub(crate) scheduler: Arc<crate::scheduler::Scheduler>,
    stopping: CancellationToken,
    _lock: File,
}
impl Drop for Engine {
    fn drop(&mut self) {
        // Duplicated/inherited descriptors can outlive this owner. Closing our
        // descriptor alone need not release their shared advisory lock.
        let _ = FileExt::unlock(&self._lock);
    }
}
impl Engine {
    pub fn open(mut config: Config) -> Result<Arc<Self>> {
        for command in [
            &mut config.ssh_command,
            &mut config.worker_launcher,
            &mut config.docker_command,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                command.is_absolute(),
                "operator process executable must be absolute"
            );
            *command = command.canonicalize()?;
            ensure!(
                command.is_file(),
                "operator process executable must be a file"
            );
        }
        if let Some(command) = &mut config.codex_command {
            ensure!(
                command.is_absolute(),
                "codex_command must be an absolute operator-controlled executable path"
            );
            *command = command.canonicalize().context("resolve codex_command")?;
            ensure!(command.is_file(), "codex_command must be a file");
        }

        ensure!(
            (1..=32).contains(&config.max_parallel_commands),
            "max_parallel_commands must be 1..32"
        );
        config.validate_memory()?;
        crate::auth::validate_environment(&config.access_tokens)?;
        for root in &mut config.allowed_roots {
            *root = root.canonicalize()?;
        }
        std::fs::create_dir_all(&config.data_dir)?;
        config.data_dir = config.data_dir.canonicalize()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config.data_dir.join("server.lock"))?;
        lock.try_lock_exclusive()
            .context("another Preflight process owns this data directory")?;
        let db = Db::new(&config.data_dir.join("preflight.sqlite3"))?;
        db.remember_secrets(crate::secrets::configured(&config));
        db.reconcile()?;
        Ok(Arc::new(Self {
            releases: Default::default(),
            policy_gate: tokio::sync::Mutex::new(()),
            container_cleanup_gate: tokio::sync::Mutex::new(()),
            queue_gate: Mutex::new(()),
            gc_gate: Mutex::new(()),
            schedule_gate: Mutex::new(()),
            transfer_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            live_slots: Arc::new(tokio::sync::Semaphore::new(crate::live::MAX_STREAMS)),
            flights: Default::default(),
            scheduler: crate::scheduler::Scheduler::new(
                config.max_parallel_commands,
                config.memory_budget_mib,
            ),
            db,
            live_config: std::sync::RwLock::new(config.clone()),
            config,
            config_file: Default::default(),
            settings_gate: Mutex::new(()),
            auth_sessions: Default::default(),
            active: Arc::new(Mutex::new(HashMap::new())),
            stopping: CancellationToken::new(),
            _lock: lock,
        }))
    }
    pub(crate) fn transport_stop(&self) -> CancellationToken {
        self.stopping.child_token()
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.is_cancelled()
    }
    pub fn current_config(&self) -> Config {
        self.live_config.read().expect("configuration lock").clone()
    }
    pub(crate) async fn acquire_resource(
        &self,
        slots: usize,
        memory: u64,
        cancel: &CancellationToken,
    ) -> Result<Option<crate::scheduler::Permit>> {
        loop {
            while crate::docker::admission_pending(&self.db)? {
                tokio::select! {_=cancel.cancelled()=>return Ok(None),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            }
            let permit = tokio::select! {biased;_=cancel.cancelled()=>return Ok(None),permit=self.scheduler.acquire_admitted(slots,memory)=>permit?};
            if !crate::docker::admission_pending(&self.db)? {
                return Ok(Some(permit));
            }
            drop(permit);
        }
    }
    pub fn scheduler_status(&self) -> serde_json::Value {
        let config = self.live_config.read().expect("configuration lock");
        let mut status = self.scheduler.status();
        status["container_cleanup_pending"] = crate::docker::admission_pending(&self.db)
            .unwrap_or(true)
            .into();
        status["min_free_disk_bytes"] = config.min_free_disk_bytes.into();
        status["available_disk_bytes"] = fs2::available_space(&config.data_dir).ok().into();
        status["disk_admission"] = serde_json::json!(
            "checked before new preparation/stages; no space reservation or interruption of running commands"
        );
        status["default_stage_memory_mib"] = config
            .memory_budget_mib
            .map(|_| config.default_stage_memory_mib)
            .into();
        status
    }
    fn validate_root(&self, root: &Path) -> Result<PathBuf> {
        Self::validate_config_root(&self.current_config(), root)
    }
    fn validate_config_root(config: &Config, root: &Path) -> Result<PathBuf> {
        let root = root
            .canonicalize()
            .map_err(|_| invalid("project directory does not exist"))?;
        ensure!(root.is_dir(), invalid("project path is not a directory"));
        ensure!(
            config.allowed_roots.iter().any(|p| root.starts_with(p)),
            invalid("project is outside allowed_roots")
        );
        ensure!(
            !root.starts_with(&config.data_dir),
            invalid("project cannot be inside the data directory")
        );
        Ok(root)
    }
    pub fn register(&self, request: RegisterProject) -> Result<Project> {
        ensure!(
            !request.name.trim().is_empty() && request.name.len() <= 128,
            invalid("name must be 1..128 bytes")
        );
        let root = self.validate_root(Path::new(&request.path))?;
        self.db.register(&request.name, &root)
    }
    pub fn configured_pipeline(&self, project_id: &str) -> Result<PipelineFile> {
        let project = self.db.project(project_id)?;
        let root = self.validate_root(&project.root)?;
        let preferred = crate::control::config_path(&root, ".preflight/pipeline.yaml")?;
        let path = if preferred.exists() {
            preferred
        } else {
            pipeline::contained(&root, "preflight.yaml")?
        };
        let file =
            File::open(path).map_err(|_| invalid("project has no readable preflight.yaml"))?;
        ensure!(
            file.metadata()?.is_file(),
            invalid("preflight.yaml must be a regular file")
        );
        let mut yaml = String::new();
        file.take(64 * 1024 + 1).read_to_string(&mut yaml)?;
        pipeline::parse(&yaml)
    }
    pub fn configured_batch(&self, request: &crate::fixtures::StartBatch) -> Result<PipelineFile> {
        let project = self.db.project(&request.project_id)?;
        crate::fixtures::load(&self.validate_root(&project.root)?, &request.path)
    }
    pub fn start_batch(self: &Arc<Self>, request: crate::fixtures::StartBatch) -> Result<Run> {
        let file = self.configured_batch(&request)?;
        self.start_pipeline(StartPipeline {
            project_id: request.project_id,
            definition: Some(file),
        })
    }
    pub fn start(self: &Arc<Self>, request: StartBuild) -> Result<Run> {
        pipeline::validate_command(&request.command)?;
        ensure!(
            pipeline::artifact_ref(&request.command.program)?.is_none()
                && !request.command.program.contains("${tool")
                && !request.command.program.contains("${module")
                && request
                    .command
                    .args
                    .iter()
                    .all(|a| !a.contains("${artifact") && !a.contains("${module")),
            invalid("single commands cannot reference artifacts or profile tools")
        );
        self.start_plan(
            request.project_id,
            None,
            vec![StageDefinition::native(request.command)],
            None,
            None,
        )
    }
    pub fn start_pipeline(self: &Arc<Self>, request: StartPipeline) -> Result<Run> {
        self.start_pipeline_submission(request, None)
    }
    pub(crate) fn start_pipeline_submission(
        self: &Arc<Self>,
        request: StartPipeline,
        submission: Option<crate::profiles::Submission>,
    ) -> Result<Run> {
        self.start_pipeline_source(request, submission, None)
    }
    pub fn start_git_pipeline(
        self: &Arc<Self>,
        request: crate::sources::StartGitPipeline,
    ) -> Result<Run> {
        request.source.validate()?;
        self.start_pipeline_source(
            StartPipeline {
                project_id: request.project_id,
                definition: Some(request.definition),
            },
            None,
            Some(crate::sources::SourceRequest::Git(request.source)),
        )
    }
    pub fn start_dirty_pipeline(
        self: &Arc<Self>,
        request: crate::sources::StartDirtyPipeline,
    ) -> Result<Run> {
        request.source.validate()?;
        self.start_pipeline_source(
            StartPipeline {
                project_id: request.project_id,
                definition: Some(request.definition),
            },
            None,
            Some(crate::sources::SourceRequest::Dirty(request.source)),
        )
    }
    pub fn start_multi_pipeline(
        self: &Arc<Self>,
        request: crate::multi_source::StartMultiPipeline,
    ) -> Result<Run> {
        request.source.validate()?;
        self.resolve_source_roots(&request.source, &self.current_config())?;
        self.start_pipeline_source(
            StartPipeline {
                project_id: request.project_id,
                definition: Some(request.definition),
            },
            None,
            Some(crate::sources::SourceRequest::Multi(request.source)),
        )
    }
    fn resolve_source_roots(
        &self,
        source: &crate::multi_source::MultiSource,
        config: &Config,
    ) -> Result<Vec<(crate::multi_source::SourceRoot, PathBuf)>> {
        source
            .roots
            .iter()
            .map(|root| {
                let project = self.db.project(&root.project_id)?;
                Ok((
                    root.clone(),
                    Self::validate_config_root(config, &project.root)?,
                ))
            })
            .collect()
    }
    pub(crate) fn start_pipeline_source(
        self: &Arc<Self>,
        request: StartPipeline,
        submission: Option<crate::profiles::Submission>,
        source: Option<crate::sources::SourceRequest>,
    ) -> Result<Run> {
        let file = match request.definition {
            Some(file) => file,
            None => self.configured_pipeline(&request.project_id)?,
        };
        pipeline::validate(&file)?;
        ensure!(
            file.pipeline
                .stages
                .iter()
                .all(|s| !s.command.program.contains("${tool.")),
            invalid("tool references require a selected profile")
        );
        self.start_plan(
            request.project_id,
            Some(file.clone()),
            file.pipeline.stages,
            submission,
            source,
        )
    }
    fn start_plan(
        self: &Arc<Self>,
        project_id: String,
        file: Option<PipelineFile>,
        stages: Vec<StageDefinition>,
        submission: Option<crate::profiles::Submission>,
        source: Option<crate::sources::SourceRequest>,
    ) -> Result<Run> {
        let guard = self.live_config.read().expect("configuration lock");
        let config = Arc::new(guard.clone());
        let project = self.db.project(&project_id)?;
        let root = Self::validate_config_root(&config, &project.root)?;
        if let Some(crate::sources::SourceRequest::Multi(request)) = &source {
            self.resolve_source_roots(request, &config)?;
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
        if let Some(s) = &submission
            && let Some(run) = self.db.submitted(&project_id, s)?
        {
            return Ok(run);
        }
        crate::docker::ensure_admission(&self.db)?;
        for stage in &stages {
            ensure!(
                stage.container.is_none()
                    || stage.worker.is_some()
                    || config.docker_command.is_some(),
                invalid("Docker executor requires configured docker_command")
            );
            config.stage_memory(stage)?;
            ensure!(
                stage.required_slots() as usize <= config.max_parallel_commands,
                invalid(format!(
                    "stage {} requests {} slots, server capacity is {}",
                    stage.id,
                    stage.required_slots(),
                    config.max_parallel_commands
                ))
            );
        }
        if stages
            .iter()
            .any(|stage| stage.cache.is_some() && stage.container.is_none())
        {
            ensure!(
                submission
                    .as_ref()
                    .and_then(|s| s.request.get("profile_snapshot"))
                    .and_then(|p| p.get("pins"))
                    .is_some_and(|pins| !pins.is_null()),
                invalid("cache policy requires a pinned toolchain profile at submission")
            );
        }
        ensure!(
            !self.stopping.is_cancelled(),
            invalid("server is shutting down")
        );
        ensure!(active.len() < config.max_active_runs, Fault::Busy);
        let id = uuid::Uuid::now_v7().to_string();
        let workspace = self.config.data_dir.join("workspaces").join(&id);
        self.db.insert_submission(
            &id,
            &project_id,
            &workspace,
            file.as_ref(),
            &stages,
            submission.as_ref(),
            source.as_ref(),
        )?;
        let run = self.db.run(&id)?;
        let cancel = CancellationToken::new();
        active.insert(id.clone(), cancel.clone());
        let engine = self.clone();
        let completion = self.active.clone();
        tokio::spawn(async move {
            if let Err(error) = engine
                .execute_run(
                    &id,
                    root,
                    workspace,
                    file,
                    stages,
                    cancel.clone(),
                    source,
                    config,
                )
                .await
            {
                eprintln!("run {id}: {error:#}");
                let state = if cancel.is_cancelled() {
                    RunState::Cancelled
                } else {
                    RunState::InfrastructureError
                };
                let reason = format!("{error:#}").chars().take(1024).collect::<String>();
                if let Err(error) = engine.db.transition(
                    &id,
                    state,
                    None,
                    None,
                    if state == RunState::Cancelled {
                        None
                    } else {
                        Some(&reason)
                    },
                ) {
                    eprintln!("persist run {id} failure: {error:#}");
                }
            }
            // Shutdown must observe completion only after the task releases the
            // engine reference that owns the exclusive data-directory lock.
            drop(engine);
            if let Ok(mut active) = completion.lock() {
                active.remove(&id);
            }
        });
        Ok(run)
    }
    pub fn cancel(&self, request: CancelRun) -> Result<Run> {
        ensure!(
            !request.reason.trim().is_empty() && request.reason.len() <= 512,
            invalid("reason must be 1..512 bytes")
        );
        let active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
        ensure!(
            !self.db.is_shared_execution(&request.run_id)?,
            invalid("cancel the subscribing run, not its shared execution")
        );
        let run = self.db.run(&request.run_id)?;
        if !run.status.terminal() {
            self.db
                .request_cancel(&run.id, request.actor.as_str(), &request.reason)?;
            if let Some(token) = active.get(&run.id) {
                token.cancel();
            }
        }
        self.db.run(&run.id)
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.stopping.cancel();
        {
            let active = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
            for (id, token) in active.iter() {
                self.db.request_cancel(id, "shutdown", "server shutdown")?;
                token.cancel();
            }
        }
        loop {
            if self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?
                .is_empty()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn execute_run(
        self: &Arc<Self>,
        id: &str,
        root: PathBuf,
        workspace: PathBuf,
        file: Option<PipelineFile>,
        stages: Vec<StageDefinition>,
        cancel: CancellationToken,
        git_source: Option<crate::sources::SourceRequest>,
        config: Arc<Config>,
    ) -> Result<()> {
        let Some(permit) = self
            .acquire_resource(1, config.preparation_memory(), &cancel)
            .await?
        else {
            self.db
                .transition(id, RunState::Cancelled, None, None, None)?;
            return Ok(());
        };
        config.check_disk_admission().await?;
        self.db
            .transition(id, RunState::Preparing, None, None, None)?;
        let mut resources = std::collections::BTreeSet::new();
        for stage in &stages {
            resources.extend(crate::leases::stage_resources(stage)?);
        }
        let _operation = self.db.begin_operation(id, "run", resources)?;
        let legacy = file.is_none();
        let source = if legacy {
            workspace.clone()
        } else {
            workspace.join("source")
        };
        let provenance = match git_source {
            Some(crate::sources::SourceRequest::Multi(request)) => Some(
                crate::multi_source::prepare(
                    self.resolve_source_roots(&request, &config)?,
                    &source,
                    &self.config.data_dir,
                    &self.db.run(id)?,
                    &cancel,
                )
                .await?,
            ),
            Some(crate::sources::SourceRequest::Git(request)) => Some(
                crate::git_source::prepare(
                    &root,
                    &source,
                    &self.config.data_dir,
                    &request,
                    &cancel,
                )
                .await?,
            ),
            Some(crate::sources::SourceRequest::Dirty(request)) => Some(
                crate::dirty_source::prepare(
                    &root,
                    &source,
                    &self.config.data_dir,
                    &request,
                    &cancel,
                )
                .await?,
            ),
            None => {
                self.copy(root, source.clone(), cancel.clone()).await?;
                None
            }
        };
        if let Some(profile) = self
            .db
            .submission(id)?
            .and_then(|s| s.get("profile_snapshot").cloned())
        {
            let profile: crate::profiles::Profile = serde_json::from_value(profile)?;
            if profile.pins.is_some() {
                let config = config.clone();
                let project =
                    self.validate_root(&self.db.project(&self.db.run(id)?.project_id)?.root)?;
                let run = self.db.run(id)?;
                let token = cancel.clone();
                let verified = tokio::task::spawn_blocking(move || {
                    crate::pins::prepare(&config, &project, &run, &profile, &token)
                })
                .await??;
                self.db.save_toolchain(id, &verified)?;
            }
        }
        // Reject incompatible module references across the entire plan before any command runs.
        for stage in &stages {
            for value in std::iter::once(&stage.command.program)
                .chain(&stage.command.args)
                .chain(stage.inputs.values())
            {
                if let Some(digest) = crate::modules::reference(value)? {
                    self.db.compatible_module_for_stage(id, digest, stage)?;
                }
            }
        }
        let input_root = source.clone();
        let input_run = self.db.run(id)?;
        let mut manifest =
            tokio::task::spawn_blocking(move || crate::inputs::capture(&input_root, &input_run))
                .await??;
        manifest.source = provenance;
        if let Some(verified) = self.db.toolchain(id)? {
            use sha2::Digest;
            manifest.configuration_sha256 = crate::hashing::hex(sha2::Sha256::digest(
                serde_json::to_vec(
                    &serde_json::json!({"configuration":manifest.configuration_sha256,"toolchain":verified.contract_sha256}),
                )?,
            ));
        }
        self.db.save_inputs(id, &manifest)?;
        drop(permit);
        self.db
            .transition(id, RunState::Running, None, None, None)?;
        let max_parallel = file.as_ref().map_or(1, |p| p.pipeline.max_parallel_stages);
        let mut states = stages
            .iter()
            .map(|s| (s.id.clone(), RunState::Queued))
            .collect::<BTreeMap<_, _>>();
        let mut jobs = JoinSet::new();
        let mut task_stages = HashMap::new();
        loop {
            let mut changed = false;
            for stage in &stages {
                if states[&stage.id] != RunState::Queued {
                    continue;
                }
                let terminal = if cancel.is_cancelled() {
                    Some((RunState::Cancelled, None))
                } else if stage
                    .needs
                    .iter()
                    .any(|dep| states[dep].terminal() && states[dep] != RunState::Succeeded)
                {
                    Some((RunState::Blocked, Some("dependency did not succeed")))
                } else {
                    None
                };
                if let Some((status, reason)) = terminal {
                    self.db
                        .stage_transition(id, &stage.id, status, None, reason)?;
                    states.insert(stage.id.clone(), status);
                    changed = true;
                    continue;
                }
                if jobs.len() < max_parallel
                    && stage
                        .needs
                        .iter()
                        .all(|dep| states[dep] == RunState::Succeeded)
                {
                    let engine = self.clone();
                    let run_id = id.to_owned();
                    let stage = stage.clone();
                    let stage_id = stage.id.clone();
                    let config = config.clone();
                    let token = cancel.clone();
                    let root = workspace.clone();
                    let source = source.clone();
                    states.insert(stage.id.clone(), RunState::Preparing);
                    let task = jobs.spawn(async move {
                        let result = engine
                            .execute_stage(&run_id, &root, &source, &stage, legacy, &token, &config)
                            .await;
                        if let Err(error) = result {
                            let status = if token.is_cancelled() {
                                RunState::Cancelled
                            } else {
                                RunState::InfrastructureError
                            };
                            let reason =
                                format!("{error:#}").chars().take(1024).collect::<String>();
                            engine.db.stage_transition(
                                &run_id,
                                &stage.id,
                                status,
                                None,
                                if status == RunState::Cancelled {
                                    None
                                } else {
                                    Some(&reason)
                                },
                            )?;
                            if status != RunState::Cancelled {
                                engine.db.append_stage_log(
                                    &run_id,
                                    &stage.id,
                                    "diagnostic",
                                    0,
                                    &crate::log_search::redact(&format!(
                                        "Preflight stage error: {reason}\n"
                                    )),
                                )?;
                            }
                        }
                        engine
                            .db
                            .run(&run_id)?
                            .stages
                            .into_iter()
                            .find(|s| s.definition.id == stage.id)
                            .map(|s| s.status)
                            .context("stage disappeared")
                    });
                    task_stages.insert(task.id(), stage_id);
                    changed = true;
                }
            }
            if jobs.is_empty() {
                if states.values().all(|s| s.terminal()) {
                    break;
                }
                ensure!(changed, "DAG scheduler made no progress");
                continue;
            }
            match jobs
                .join_next_with_id()
                .await
                .context("stage task missing")?
            {
                Ok((task_id, result)) => {
                    let stage_id = task_stages
                        .remove(&task_id)
                        .context("stage task identity")?;
                    states.insert(stage_id, result?);
                }
                Err(error) => {
                    let stage_id = task_stages
                        .remove(&error.id())
                        .context("failed stage task identity")?;
                    self.db.stage_transition(
                        id,
                        &stage_id,
                        RunState::InfrastructureError,
                        None,
                        Some("stage worker failed"),
                    )?;
                    states.insert(stage_id, RunState::InfrastructureError);
                }
            }
        }
        let _active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
        let run = self.db.run(id)?;
        let mut status = if cancel.is_cancelled() {
            RunState::Cancelled
        } else {
            aggregate(&run.stages.iter().collect::<Vec<_>>()).unwrap_or(RunState::Succeeded)
        };
        let exit = if legacy { run.stage.exit_code } else { None };
        let reason = run
            .stages
            .iter()
            .find(|s| s.status == status)
            .and_then(|s| s.reason.as_deref());
        let verdict = crate::acceptance::evaluate(self, &run)?;
        let acceptance_failed =
            matches!(verdict["status"].as_str(), Some("rejected" | "incomplete"));
        if status == RunState::Succeeded && acceptance_failed {
            status = RunState::Failed;
        }
        self.db
            .transition(id, RunState::Collecting, exit, None, None)?;
        self.db.transition(
            id,
            status,
            exit,
            (status == RunState::TimedOut).then_some("timeout"),
            if acceptance_failed && reason.is_none() {
                Some("acceptance criteria were rejected or have incomplete evidence")
            } else {
                reason
            },
        )?;
        Ok(())
    }
    async fn copy(
        &self,
        source: PathBuf,
        destination: PathBuf,
        cancel: CancellationToken,
    ) -> Result<()> {
        let data = self.config.data_dir.clone();
        tokio::task::spawn_blocking(move || snapshot(&source, &destination, &data, &cancel))
            .await??;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn execute_stage(
        self: &Arc<Self>,
        id: &str,
        root: &Path,
        source: &Path,
        stage: &StageDefinition,
        legacy: bool,
        cancel: &CancellationToken,
        config: &Config,
    ) -> Result<()> {
        let memory = config.stage_memory(stage)?;
        let Some(permit) = self
            .acquire_resource(stage.required_slots() as usize, memory, cancel)
            .await?
        else {
            self.db
                .stage_transition(id, &stage.id, RunState::Cancelled, None, None)?;
            return Ok(());
        };
        config.check_disk_admission().await?;
        self.db
            .stage_transition(id, &stage.id, RunState::Preparing, None, None)?;
        let workspace = if legacy {
            root.to_path_buf()
        } else {
            root.join("stages").join(&stage.id)
        };
        if !legacy {
            self.copy(source.to_path_buf(), workspace.clone(), cancel.clone())
                .await?;
        }
        if cancel.is_cancelled() {
            self.db
                .stage_transition(id, &stage.id, RunState::Cancelled, None, None)?;
            return Ok(());
        }
        let _operation =
            self.db
                .begin_operation(id, "stage", crate::leases::stage_resources(stage)?)?;
        let paths = artifacts::prepare_inputs(&self.db, id, stage, &workspace)?;
        let mut effective_stage = stage.clone();
        crate::docker::resolve_image(&self.db, id, &mut effective_stage)?;
        let stage = &effective_stage;
        let mut command = stage.command.clone();
        command.program = artifacts::substitute(&command.program, &paths)?;
        command.args = command
            .args
            .iter()
            .map(|a| artifacts::substitute(a, &paths))
            .collect::<Result<_>>()?;
        let mut env = stage.env.clone();
        let mut verified_program = None;
        if let Some(verified) = self.db.toolchain(id)? {
            crate::pins::stage(&verified, &workspace, &mut command, &mut env)?;
            verified_program = crate::pins::original_program(&verified, &command.program);
            ensure!(
                stage.worker.is_none() || verified_program.is_none(),
                "run_in_place tools require local execution"
            );
        } else {
            ensure!(
                !command.program.contains("${pinned")
                    && !command
                        .args
                        .iter()
                        .chain(env.values())
                        .any(|v| v.contains("${sdk") || v.contains("${pinned")),
                "pinned references require a verified profile"
            );
        }
        let cwd = pipeline::contained(&workspace, &stage.cwd)?;
        ensure!(cwd.is_dir(), "stage cwd is not a directory");
        // Old reports/outputs in source must never masquerade as results of this command.
        for path in stage
            .report_paths()
            .chain(stage.outputs.values().map(|o| &o.path))
        {
            ensure!(
                !pipeline::contained(&cwd, path)?.exists(),
                "declared output/report already exists in source: {path}"
            );
        }
        let evidence = crate::stage_keys::capture(&self.db, id, stage)?;
        self.db.save_stage_key(id, &stage.id, &evidence)?;
        self.db
            .stage_transition(id, &stage.id, RunState::Running, None, None)?;
        let mut cache_key = None;
        let mut cache_decision =
            serde_json::json!({"status":"disabled","reason":"stage has no explicit cache policy"});
        if let Some(policy) = &stage.cache {
            ensure!(
                self.db.toolchain(id)?.is_some() || stage.container.is_some(),
                "cache reuse requires a verified pinned toolchain or immutable container image"
            );
            let key = crate::cache::key(&self.db, id, &evidence)?;
            cache_decision = serde_json::json!({"status":"miss","key":key,"format":crate::cache::FORMAT,"pipeline_id":policy.pipeline_id});
            if let Some(candidate) =
                self.db
                    .cache_candidate(&self.db.run(id)?.project_id, &policy.pipeline_id, &key)?
            {
                match crate::cache::restore(&self.db, &candidate, id, root, stage) {
                    Ok(outputs) => {
                        let _active = self
                            .active
                            .lock()
                            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
                        let cancelled = cancel.is_cancelled();
                        self.db.cache_record(id, &stage.id, serde_json::json!({"status":if cancelled {"cancelled_hit"} else {"hit"},"key":key,"format":crate::cache::FORMAT,"pipeline_id":policy.pipeline_id,"entry_id":candidate.id,"producer_run":candidate.run_id,"producer_stage":candidate.stage_id,"verified_outputs":outputs.len()}))?;
                        self.db.stage_transition(
                            id,
                            &stage.id,
                            RunState::Collecting,
                            None,
                            None,
                        )?;
                        self.db.finish_stage(
                            id,
                            &stage.id,
                            if cancelled {
                                RunState::Cancelled
                            } else {
                                RunState::Succeeded
                            },
                            None,
                            Some("verified cached outputs; command not executed"),
                            None,
                            if cancelled { &[] } else { &outputs },
                        )?;
                        return Ok(());
                    }
                    Err(error) => {
                        self.db.cache_invalidate(&candidate.id)?;
                        cache_decision["status"] = "invalidated_miss".into();
                        cache_decision["reason"] = error.to_string().into();
                        cache_decision["entry_id"] = candidate.id.into();
                    }
                }
            }
            cache_key = Some(key);
        }
        let container = if stage.worker.is_none() {
            crate::docker::context(config, id, stage)?.map(Arc::new)
        } else {
            None
        };
        crate::setup::prepare(stage, &workspace, &cwd, &mut command)?;
        if stage.cache.as_ref().is_some_and(|p| p.single_flight) {
            drop(permit);
            return crate::single_flight::execute(
                self,
                crate::single_flight::Prepared {
                    memory,
                    owner_run: id.into(),
                    stage: stage.clone(),
                    workspace,
                    cwd,
                    command,
                    env,
                    container: container.clone(),
                    verified_executable: verified_program.clone(),
                    key: cache_key.context("missing shared key")?,
                },
                root,
                cancel,
            )
            .await;
        }
        let _permit = permit;
        self.db.cache_record(id, &stage.id, cache_decision)?;
        if stage.fixtures.is_some() {
            let (mut status, report) = crate::fixtures::execute(
                &self.db,
                id,
                stage,
                &workspace,
                &command.program,
                container.as_deref(),
                cancel,
            )
            .await?;
            let _active = self
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
            if cancel.is_cancelled() {
                status = RunState::Cancelled;
            }
            self.db
                .stage_transition(id, &stage.id, RunState::Collecting, None, None)?;
            self.db
                .finish_stage(id, &stage.id, status, None, None, Some(&report), &[])?;
            return Ok(());
        }
        let (mut status, exit) = if let Some(selector) = &stage.worker {
            crate::remote_stage::execute(
                self.clone(),
                selector,
                stage,
                Execution {
                    container: None,
                    log_mode: crate::executor::LogMode::All,
                    verified_executable: None,
                    run_id: id,
                    stage_id: &stage.id,
                    workspace: &workspace,
                    cwd: &cwd,
                    command: &command,
                    env: &env,
                    stdin: None,
                    capture_limit: 4096,
                },
                cancel,
            )
            .await?
        } else {
            executor::execute(
                &self.db,
                Execution {
                    container: container.as_deref(),
                    log_mode: crate::executor::LogMode::All,
                    verified_executable: verified_program.as_ref(),
                    run_id: id,
                    stage_id: &stage.id,
                    workspace: &workspace,
                    cwd: &cwd,
                    command: &command,
                    env: &env,
                    stdin: None,
                    capture_limit: 1024 * 1024,
                },
                cancel,
            )
            .await?
        };
        self.db
            .stage_transition(id, &stage.id, RunState::Collecting, exit, None)?;
        let tests = if matches!(status, RunState::Succeeded | RunState::Failed) {
            crate::testing::read(stage, &cwd)?
        } else {
            None
        };
        if tests
            .as_ref()
            .is_some_and(|report| report.counts.failed > 0)
        {
            status = RunState::Failed;
        }
        let retry_error = if matches!(status, RunState::Succeeded | RunState::Failed) {
            crate::retry::report_error(self, id, tests.as_ref())?
        } else {
            None
        };
        if retry_error.is_some() {
            status = RunState::InfrastructureError;
        }
        let mut published = if status == RunState::Succeeded {
            artifacts::publish(id, root, stage, &cwd)?
        } else {
            vec![]
        };
        let _active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("worker mutex poisoned"))?;
        if cancel.is_cancelled() {
            status = RunState::Cancelled;
            published.clear();
        }
        if status == RunState::Succeeded
            && let (Some(policy), Some(key)) = (&stage.cache, &cache_key)
        {
            self.db
                .cache_publish(id, &stage.id, &policy.pipeline_id, key)?;
        }
        self.db.finish_stage(
            id,
            &stage.id,
            status,
            exit,
            retry_error
                .as_deref()
                .or_else(|| (status == RunState::TimedOut).then_some("command deadline exceeded")),
            tests.as_ref(),
            &published,
        )?;
        Ok(())
    }
}
pub fn aggregate(stages: &[&StageRun]) -> Option<RunState> {
    if stages.is_empty() {
        return None;
    }
    for status in [
        RunState::InfrastructureError,
        RunState::TimedOut,
        RunState::Cancelled,
        RunState::Failed,
    ] {
        if stages.iter().any(|s| s.status == status) {
            return Some(status);
        }
    }
    if stages.iter().any(|s| !s.status.terminal()) {
        return Some(RunState::Running);
    }
    if stages.iter().all(|s| s.status == RunState::Succeeded) {
        Some(RunState::Succeeded)
    } else {
        Some(RunState::Blocked)
    }
}
pub fn stage_group(run: &Run, kind: StageKind) -> Vec<&StageRun> {
    run.stages
        .iter()
        .filter(|s| s.definition.kind == kind)
        .collect()
}
fn snapshot(
    root: &Path,
    destination: &Path,
    data: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    fn copy(
        source: &Path,
        destination: &Path,
        data: &Path,
        cancel: &CancellationToken,
        budget: &mut (u64, usize),
    ) -> Result<()> {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            ensure!(!cancel.is_cancelled(), "snapshot cancelled");
            let entry = entry?;
            let path = entry.path();
            if path == data
                || [".git", "target", "node_modules", ".preflight"]
                    .iter()
                    .any(|n| entry.file_name() == *n)
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "snapshot refuses symlink: {}",
                path.display()
            );
            budget.1 += 1;
            ensure!(budget.1 <= 20_000, "snapshot exceeds 20000 entries");
            let target = destination.join(entry.file_name());
            if metadata.is_dir() {
                copy(&path, &target, data, cancel, budget)?;
            } else {
                ensure!(metadata.is_file(), "snapshot refuses special file");
                budget.0 += metadata.len();
                ensure!(budget.0 <= 128 * 1024 * 1024, "snapshot exceeds 128 MiB");
                std::fs::copy(path, target)?;
            }
        }
        Ok(())
    }
    std::fs::create_dir_all(destination.parent().context("workspace parent")?)?;
    copy(root, destination, data, cancel, &mut (0, 0))
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    #[test]
    fn owner_drop_unlocks_even_when_a_duplicate_descriptor_survives() {
        let temporary = tempfile::tempdir().unwrap();
        let config = Config {
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
            max_parallel_commands: 1,
            bind: "127.0.0.1:0".parse().unwrap(),
            data_dir: temporary.path().join("data"),
            allowed_roots: vec![temporary.path().to_path_buf()],
        };
        let engine = Engine::open(config.clone()).unwrap();
        let inherited = engine._lock.try_clone().unwrap();
        assert!(Engine::open(config.clone()).is_err());
        drop(engine);
        let reopened = Engine::open(config).unwrap();
        drop(inherited);
        drop(reopened);
    }
}
