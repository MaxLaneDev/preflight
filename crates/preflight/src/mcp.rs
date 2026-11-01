use crate::{
    engine::Engine,
    model::*,
    pipeline::{self, PipelineFile, StartPipeline},
};
use rmcp::schemars;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use serde::Serialize;
use std::sync::Arc;
mod annotations;
#[derive(Clone)]
pub struct Mcp {
    engine: Arc<Engine>,
    tool_router: ToolRouter<Self>,
    watches: Arc<Watches>,
}
#[derive(Default)]
struct Watches(
    std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
);
impl Drop for Watches {
    fn drop(&mut self) {
        if let Ok(watches) = self.0.lock() {
            for token in watches.values() {
                token.cancel();
            }
        }
    }
}
fn result(value: anyhow::Result<impl Serialize>) -> Result<CallToolResult, ErrorData> {
    match value {
        Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&value)
                .map_err(|_| ErrorData::internal_error("serialization failed", None))?,
        )])),
        Err(error) => {
            let message = if error.downcast_ref::<Fault>().is_some() {
                error.to_string()
            } else {
                eprintln!("MCP error: {error:#}");
                "operation failed; see server log".into()
            };
            Ok(CallToolResult::error(vec![ContentBlock::text(message)]))
        }
    }
}
#[tool_router]
impl Mcp {
    #[tool(
        description = "Read bounded authorization audit metadata. Administration role required when API credentials are enabled; no credential values or request bodies."
    )]
    fn access_audit(
        &self,
        Parameters(r): Parameters<crate::auth::AuditQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::auth::audit_page(
            &self.engine,
            r.before,
            r.limit.unwrap_or(20),
        ))
    }
    #[tool(
        description = "List bounded local Docker jobs and exact host/container workspace paths. Pinned image/platform and cleanup state are explicit; no daemon credentials or ownership tokens."
    )]
    fn containers_list(
        &self,
        Parameters(q): Parameters<crate::docker::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::docker::list(&self.engine.db, &q))
    }
    #[tool(
        description = "Retry verified cleanup of an abandoned owned Docker job. Requires project/job ID, exact idempotency key, actor and reason; cannot stop an active run or delete a foreign container. Use pipeline_cancel for active work."
    )]
    async fn containers_cleanup(
        &self,
        Parameters(r): Parameters<crate::docker::Cleanup>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::docker::cleanup_request(&self.engine, &r).await)
    }
    pub fn new(engine: Arc<Engine>) -> Self {
        let mut tool_router = Self::tool_router();
        annotations::apply(&mut tool_router);
        Self {
            engine,
            tool_router,
            watches: Arc::default(),
        }
    }
    #[tool(
        description = "Create an asynchronous portable tar.zst snapshot (optionally wrapped to an age X25519 encryption_recipient) of one completed run: metadata/tests, optionally raw logs, artifacts, source and run workspace environment. No host OS image, no cleanup. Retain archive_id and inspect archives_get. Same request_id replays exactly."
    )]
    fn archives_create(
        &self,
        Parameters(r): Parameters<crate::archives::Create>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::create(&self.engine, r))
    }
    #[tool(
        description = "Read the private archive import inbox path and size limit. Stage regular files there; arbitrary server paths cannot be imported."
    )]
    fn archives_inbox(
        &self,
        Parameters(r): Parameters<crate::control::ProjectId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::inbox_location(&self.engine, r))
    }
    #[tool(
        description = "Import a staged tar.zst or tar.zst.age archive (encrypted input needs identity_file_env referencing a configured private identity file) by inbox file_name and expected SHA256. Verifies all paths, hashes and sizes before publication. Keeps origin identities separately; creates no synthetic run. Same request_id replays exactly. Does not remove the inbox file."
    )]
    async fn archives_import(
        &self,
        Parameters(r): Parameters<crate::archives::ImportStaged>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::import_staged(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Preview deletion of completed archive files by project or UI pipeline key, fixed age cutoff and optional byte target. Keeps latest copies and pins; does not delete anything. Execute only returned plan via storage_gc_execute. Original runs/artifacts and restored files remain unchanged."
    )]
    async fn archives_cleanup_preview(
        &self,
        Parameters(r): Parameters<crate::archive_gc::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archive_gc::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Pin or unpin one available archive with optimistic revision and exact request replay. Pins protect archive files from manual and automatic retention; they do not pin original run payloads."
    )]
    fn archives_pin(
        &self,
        Parameters(r): Parameters<crate::archive_gc::Pin>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archive_gc::pin(&self.engine, r))
    }
    #[tool(
        description = "Read compact archive state, SHA256, bytes, selected components and missing already-cleaned paths. A ready archive can be verified, downloaded or restored to isolated files."
    )]
    fn archives_get(
        &self,
        Parameters(r): Parameters<crate::archives::Id>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::get(&self.engine, &r))
    }
    #[tool(
        description = "Page run archives within a project, optionally one run or pipeline identity. Responses omit file manifests and raw logs."
    )]
    fn archives_list(
        &self,
        Parameters(r): Parameters<crate::archives::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::list(&self.engine, r))
    }
    #[tool(
        description = "Verify archive SHA256 and decrypted content (encrypted archives need identity_file_env; key material never enters requests), versioned manifest, entry paths, sizes and per-file SHA256. Does not execute or change live data."
    )]
    async fn archives_verify(
        &self,
        Parameters(r): Parameters<crate::archives::Unlock>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::verify_unlocked(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Extract a verified saved archive into a new private isolated server directory. Never overwrites projects, live database or prior restorations; does not execute archived code. Raw logs/environment may contain secrets; do not publish them."
    )]
    async fn archives_restore(
        &self,
        Parameters(r): Parameters<crate::archives::Restore>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::archives::restore(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Preview bounded project pending-queue clearing, exact pipeline artifacts expiry, or recoverable pipeline/run deletion. No mutation of work or files. Execute only the returned plan; progressed tasks are skipped."
    )]
    async fn management_preview(
        &self,
        Parameters(r): Parameters<crate::management::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::management::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Apply a reviewed management plan with actor, reason and request_id. Identical retry replays. Artifact plans instead use storage_gc_execute. Never cancels progressed work during pending queue clearing."
    )]
    fn management_execute(
        &self,
        Parameters(r): Parameters<crate::management::Execute>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::management::execute(&self.engine, r))
    }
    #[tool(
        description = "Page recoverably deleted project pipelines and runs. Metadata, revisions and evidence remain retained."
    )]
    fn management_trash(
        &self,
        Parameters(r): Parameters<crate::management::Trash>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::management::trash(&self.engine, r))
    }
    #[tool(
        description = "Restore one project pipeline or run from Trash, preserving its identity, revisions and evidence."
    )]
    fn management_restore(
        &self,
        Parameters(r): Parameters<crate::management::Restore>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::management::restore(&self.engine, r))
    }
    #[tool(
        description = "Read opt-in stage-workspace and artifact-expiry TTL policies, durable cycles, scopes, fixed cutoff and GC plan IDs. Empty policies disables automatic cleanup. Optional quota high_bytes/low_bytes uses bounded complete file-length accounting; alternative disk_pressure start_below_bytes/stop_at_bytes reads data_dir filesystem availability. Inspect disk_observation for the latter; inspect quota_observation and errors for unmet targets. Configure through authenticated server_settings; artifact expiry preserves history/SHA, pins, cache, modules, experiments and leases."
    )]
    fn storage_policy_status(&self) -> Result<CallToolResult, ErrorData> {
        result(crate::storage_policy::status(&self.engine))
    }
    #[tool(
        description = "Bind a selected run to its originating Codex thread UUID. On completion the configured local Codex CLI queues one message to that thread. Requires user authorization for this target. Repeated binding is idempotent; disabled bindings remain disabled."
    )]
    fn codex_hook_bind(
        &self,
        Parameters(r): Parameters<crate::codex_bridge::Bind>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::codex_bridge::bind(&self.engine, &r))
    }
    #[tool(
        description = "Inspect Codex completion hook bindings for a run, including delivery attempts and uncertain outcomes. Delivered means CLI success, not proof that a model processed the message."
    )]
    fn codex_hook_status(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.codex_bindings(&r.run_id))
    }
    #[tool(
        description = "Disable a pending/failed/uncertain Codex completion binding while preserving audit history. A currently sending binding returns busy; already delivered messages cannot be withdrawn."
    )]
    fn codex_hook_disable(
        &self,
        Parameters(r): Parameters<crate::codex_bridge::BindingId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.disable_codex(&r))
    }
    #[tool(
        description = "Explicitly retry a Codex hook only if the command definitely never started, at most three attempts. Unknown/uncertain queue acceptance cannot be retried automatically."
    )]
    fn codex_hook_retry(
        &self,
        Parameters(r): Parameters<crate::codex_bridge::BindingId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.retry_codex(&r))
    }
    #[tool(
        description = "Read durable project-scoped run completion events immediately, with resumable cursor and bounded pagination. Reconnects can replay events; historical backfill is marked. Use the resource URI to subscribe for update hints."
    )]
    fn events_read(
        &self,
        Parameters(r): Parameters<crate::events::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.completion_events(&r))
    }
    #[tool(
        description = "Create an immutable project change queue; independent exact Git sources run up to max_parallel, cumulative patches share a base commit and stop on failure. Returns queue_id; exact request_id replay is safe."
    )]
    fn queue_create(
        &self,
        Parameters(r): Parameters<crate::queues::Create>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::queues::create(&self.engine, r))
    }
    #[tool(
        description = "Read compact queue states and run IDs. details=true adds source identities; no logs or raw patches."
    )]
    fn queue_status(
        &self,
        Parameters(r): Parameters<crate::queues::Id>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::queues::status(&self.engine, &r.queue_id, r.details))
    }
    #[tool(
        description = "List project change queues; executions_list provides the cross-project execution overview."
    )]
    fn queues_list(
        &self,
        Parameters(r): Parameters<crate::queues::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::queues::list(&self.engine, &r))
    }
    #[tool(
        description = "Cancel pending and active runs of one queue; retain outcomes and history."
    )]
    fn queue_cancel(
        &self,
        Parameters(r): Parameters<crate::queues::Cancel>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::queues::cancel(&self.engine, r))
    }
    #[tool(
        description = "Inspect server slot and optional memory capacities, current atomic reservations and waiting count. Stages declare resources.slots/memory_mib; omitted memory uses the configured default. Admission control, not OS CPU or memory isolation."
    )]
    fn scheduler_status(&self) -> Result<CallToolResult, ErrorData> {
        result(Ok(self.engine.scheduler_status()))
    }
    #[tool(
        description = "Inspect worker job states for a run, including lease expiry and loss reasons. No claim credentials or full command output are exposed."
    )]
    fn runs_worker_jobs(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.worker_jobs(&r.run_id))
    }
    #[tool(
        description = "Prepare a verified frozen pipeline source snapshot for one project worker generation. Returns transfer_id/tree_sha256 for worker fetch; no job is executed. Grant expires in 1..600 seconds. Legacy mutable workspaces are refused."
    )]
    async fn worker_inputs_prepare(
        &self,
        Parameters(r): Parameters<crate::worker_transfers::Prepare>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::worker_transfers::prepare(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Issue a secret enrollment token for one project worker and expected OS/architecture, valid at most 600 seconds. Treat output as a credential; do not log or put in pipeline definitions. Creating a token does not start a worker or job; execution requires a compatible authenticated worker and verified transfers."
    )]
    fn worker_enrollment_create(
        &self,
        Parameters(r): Parameters<crate::workers::CreateEnrollment>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.worker_enrollment(&r))
    }
    #[tool(
        description = "List project workers with liveness, OS/architecture and negotiated job protocol. execution_ready is true only for a registered worker with a fresh heartbeat advertising the current job protocol and no abandoned job operations. requires_recovery/blocked_operations explain the guard. It does not prove cross-host acceptance or remote cache support."
    )]
    fn workers_list(
        &self,
        Parameters(r): Parameters<crate::workers::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.workers(&r))
    }
    #[tool(
        description = "Inspect one worker job's journal recovery authorization. Recovery requires a lost job and an existing operator-attested operation_resolve; returns lease/worker identity and next-step policy, never claim secrets."
    )]
    fn worker_recovery_status(
        &self,
        Parameters(q): Parameters<crate::worker_recovery::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.worker_recovery_status(&q))
    }
    #[tool(
        description = "Revoke a project worker credential at its listed generation and record actor/reason. Repeated revocation is idempotent. Re-enrollment can retain the worker identity with a new credential."
    )]
    fn worker_revoke(
        &self,
        Parameters(r): Parameters<crate::workers::Revoke>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.worker_revoke(&r))
    }
    #[tool(
        description = "Search all projects by UUID, hash, name, test, input path or redacted log terms. Bounded results include project/run context and optional interface target; use next_cursor for more. Optional project_id narrows scope. Never executes work."
    )]
    fn search_global(
        &self,
        Parameters(r): Parameters<crate::search::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::search::search(&self.engine, &r))
    }
    #[tool(
        description = "Start a project SSH worker connection asynchronously. Same request_id replays the same operation; requires configured worker gateway and an allowed local bundle directory. Optional reconnect sets bounded retries (max_retries 1..10, delay_seconds 1..300); absent means no retries. Stop cancels pending retries; server restart never silently relaunches interrupted connections."
    )]
    fn worker_ssh_start(
        &self,
        Parameters(r): Parameters<crate::worker_connections::Start>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::worker_connections::start(&self.engine, r))
    }
    #[tool(
        description = "Read compact project SSH connection state. Abandoned job operations set requires_recovery and block execution_ready; recovery_hint names the operator workflow. details=true includes the nonsecret request."
    )]
    fn worker_ssh_status(
        &self,
        Parameters(r): Parameters<crate::worker_connections::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::worker_connections::status(&self.engine, &r))
    }
    #[tool(
        description = "Stop only this owned SSH connection. For interrupted recovery require operator process_stopped=true; saved PIDs never authorize signals."
    )]
    fn worker_ssh_stop(
        &self,
        Parameters(r): Parameters<crate::worker_connections::Stop>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::worker_connections::stop(&self.engine, r))
    }
    #[tool(description = "List project SSH connections with bounded pagination, newest first.")]
    fn worker_ssh_list(
        &self,
        Parameters(r): Parameters<crate::worker_connections::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::worker_connections::list(&self.engine, &r))
    }
    #[tool(
        description = "List abandoned operations for a project, 100 per page. Use runs_operations to inspect protection; no automatic expiry."
    )]
    fn operations_abandoned(
        &self,
        Parameters(r): Parameters<crate::operation_resolution::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.abandoned_operations(&r))
    }
    #[tool(
        description = "Release one abandoned lease after an operator verified process termination. Requires process_stopped=true, actor, reason and project-scoped idempotency key. Never auto-attest; active operations and nonterminal runs are rejected. Deletes no files."
    )]
    fn operation_resolve(
        &self,
        Parameters(r): Parameters<crate::operation_resolution::Resolve>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.resolve_operation(&r))
    }
    #[tool(
        description = "Read optional release notifications. details=true adds bounded changelog. Product SemVer is independent of MCP protocol. Disabled unless release_repository is configured; no binary installation."
    )]
    fn server_release_status(
        &self,
        Parameters(q): Parameters<crate::releases::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(Ok(crate::releases::status(&self.engine, q.details)))
    }
    #[tool(
        description = "Admin-only public GitHub release check. Requires admin_token; at most once per minute. Returns notification status; details=true includes bounded changelog. Never installs binaries or stops runs."
    )]
    async fn server_release_check(
        &self,
        Parameters(q): Parameters<crate::releases::Check>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::releases::request(&self.engine, q).await)
    }
    #[tool(
        description = "Admin-only server settings. Read or hot-reload using admin_token and expected_revision. Never include credentials in logs."
    )]
    fn server_settings(
        &self,
        Parameters(r): Parameters<crate::settings::Request>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::settings::request(&self.engine, r))
    }
    #[tool(
        description = "Admin-only macOS login startup. Omit enabled to inspect. To change, supply enabled and the current server_settings expected_revision. Changes apply to future user logins and preserve the current server and runs. Reuses an existing matching Homebrew/Preflight agent; never bootstraps or stops it. Environment-only settings credentials are persisted in a private external file when enabling. Never log admin_token."
    )]
    fn server_autostart(
        &self,
        Parameters(r): Parameters<crate::autostart::Request>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::autostart::request(&self.engine, r))
    }
    #[tool(
        description = "Read one captured file's verified text diff between runs of the same project. Requires project_id, run_id, baseline_id, path. At most 256 KiB per side; never reads current checkout. Missing/tampered/binary evidence returns an error, not an invented diff."
    )]
    async fn runs_source_diff(
        &self,
        Parameters(r): Parameters<crate::source_diff::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::source_diff::compare(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Read bounded storage usage for a project and optional exact cache pipeline ID. Categories: run_workspaces (project only), stage_workspaces, artifacts, module_copies. Returns observed file lengths, completeness and next_cursor. Overlapping/shared bytes are not reclaimable bytes. No deletion."
    )]
    async fn storage_usage(
        &self,
        Parameters(r): Parameters<crate::storage_usage::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::storage_usage::usage(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Preview physical deletion of disposable stage workspaces by project or explicit cache pipeline ID. Optional finished_before is an exclusive past Unix-millisecond cutoff, fixed in the plan and rechecked before deletion. Retains sources, published artifacts, modules, logs and history; returns a fixed bounded plan and file-byte estimate."
    )]
    async fn storage_gc_preview(
        &self,
        Parameters(r): Parameters<crate::storage_gc::StagePreview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::storage_gc::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Preview irreversible old successful log/source payload expiry in an exact project/cache-pipeline scope. Retain metadata; protect failures, baselines, pins, modules and leases. Execute returned plan with storage_gc_execute; never claim SQLite filesystem shrinkage."
    )]
    async fn evidence_expiry_preview(
        &self,
        Parameters(r): Parameters<crate::evidence_expiry::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::evidence_expiry::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Preview explicit physical cache-output eviction ranked by recency, hit frequency and measured rebuild cost. Protected owners/leases/baselines/modules retain bytes. Execute the frozen plan with storage_gc_execute; invalidation and expiry are audited together."
    )]
    async fn cache_eviction_preview(
        &self,
        Parameters(r): Parameters<crate::cache_eviction::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::cache_eviction::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "List operator-configured optional read-only GitHub/GitLab CI sources. Credentials remain in server environment; no remote build can be started."
    )]
    fn ci_sources_list(
        &self,
        Parameters(r): Parameters<crate::ci::SourceQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::sources(&self.engine, &r))
    }
    #[tool(
        description = "Queue a bounded read-only import of completed external CI results. Return sync_id immediately. Save IDs/cursor, inspect ci_sync_status and ci_run_get. Stable request_id supports replay; missing structured test reports mean unknown counts."
    )]
    fn ci_sync(
        &self,
        Parameters(r): Parameters<crate::ci::Sync>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::sync(&self.engine, r))
    }
    #[tool(
        description = "Read compact background CI import status. Partial coverage, rate-limit retry_at and continuation are explicit; no provider execution or mutation."
    )]
    fn ci_sync_status(
        &self,
        Parameters(r): Parameters<crate::ci::Status>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::sync_status(&self.engine, &r))
    }
    #[tool(
        description = "Page imported external CI runs in an exact local project; preserve provider run ID, revision, SHA and URL."
    )]
    fn ci_runs_list(
        &self,
        Parameters(r): Parameters<crate::ci::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::runs(&self.engine, &r))
    }
    #[tool(
        description = "Bounded external CI run provenance, import coverage/issues, jobs and artifact metadata. Retain next_offset. Use ordinary build_report, logs/evidence and tests tools for normalized results."
    )]
    fn ci_run_get(
        &self,
        Parameters(r): Parameters<crate::ci::Get>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::get(&self.engine, &r))
    }
    #[tool(
        description = "Queue a selected external CI artifact download by retained local run and artifact IDs. Returns sync_id; SHA-addressed local bytes/metadata only, no binary base64. Provider API remains GET-only."
    )]
    fn ci_artifact_fetch(
        &self,
        Parameters(r): Parameters<crate::ci::FetchArtifact>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::artifact_fetch(&self.engine, r))
    }
    #[tool(
        description = "Page the durable local CI import queue, optionally across projects. These are read-only imports, not provider builds. Retain project/sync IDs for cancellation."
    )]
    fn ci_syncs_list(
        &self,
        Parameters(r): Parameters<crate::ci::SyncList>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::syncs(&self.engine, &r))
    }
    #[tool(
        description = "Stop one queued/running local CI import. No provider build is cancelled or changed. An in-flight atomic publication may finish; already imported evidence remains."
    )]
    fn ci_sync_cancel(
        &self,
        Parameters(r): Parameters<crate::ci::Cancel>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::ci::cancel(&self.engine, &r))
    }
    #[tool(
        description = "Preview physical eviction of unused module copies by project or cache pipeline ID. Pins, consumers and active/abandoned operations protect copies. Producer artifacts/history stay retained. Execute returned plan with storage_gc_execute."
    )]
    async fn module_gc_preview(
        &self,
        Parameters(r): Parameters<crate::storage_gc::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::module_gc::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Explicit retention preview for successful run output bytes. Requires expire_artifacts=true and a past finished_before timestamp in Unix milliseconds. Active cache, modules, experiments and leases protect outputs. History/SHA remain as artifact_expired; execute via storage_gc_execute."
    )]
    async fn artifact_expiry_preview(
        &self,
        Parameters(r): Parameters<crate::artifact_expiry::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::artifact_expiry::preview(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Execute or resume an approved storage GC plan with actor, reason and request_id. Rechecks leases and tree identity, journals quarantine, deletes approved disposable stage or unused module copies. Identical retries resume; changed requests conflict."
    )]
    async fn storage_gc_execute(
        &self,
        Parameters(r): Parameters<crate::storage_gc::Execute>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::storage_gc::execute(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Read project-scoped GC progress, audit, removed file bytes and skipped/failed items. Does not delete data."
    )]
    fn storage_gc_status(
        &self,
        Parameters(r): Parameters<crate::storage_gc::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::storage_gc::status(&self.engine, &r))
    }
    #[tool(
        description = "Preview up to 100 cache entry invalidations within a project or pipeline. Stores an immutable plan. Retains run history, artifacts and shared modules; deletes zero physical bytes."
    )]
    fn cache_cleanup_preview(
        &self,
        Parameters(r): Parameters<crate::cache::CleanupPreview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.cache_cleanup_preview(&r))
    }
    #[tool(
        description = "Execute a stored cache invalidation plan idempotently, rechecking active and abandoned operation leases in one transaction. Retains artifacts and modules; returns protected entries and exact result on retry."
    )]
    fn cache_cleanup_execute(
        &self,
        Parameters(r): Parameters<crate::cache::CleanupExecute>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.cache_cleanup_execute(&r))
    }
    #[tool(
        description = "Explain project module retention: publication, producer/consumer history, pipeline cache owners, pins and active/abandoned operations. Paginated live evidence, not permission to delete."
    )]
    fn module_retention(
        &self,
        Parameters(r): Parameters<crate::retention::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module_retention(&r))
    }
    #[tool(
        description = "Pin or release one explicit module retention owner, with expected revision, actor, reason and idempotency key. Releasing a pin never deletes content or removes other owners."
    )]
    fn module_pin_set(
        &self,
        Parameters(r): Parameters<crate::retention::Pin>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module_pin_set(&r))
    }
    #[tool(
        description = "Read this run's shared execution IDs and subscription states. Read shared logs using execution_run and stage native; cancel only the subscribing run."
    )]
    fn runs_shared(&self, Parameters(r): Parameters<RunId>) -> Result<CallToolResult, ErrorData> {
        result(crate::single_flight::inspect(&self.engine, &r.run_id))
    }
    #[tool(
        description = "Explain per-stage cache decisions for a run: disabled, miss, verified hit, cancelled hit or invalidated corrupt entry; includes producer provenance and scoped pipeline identity."
    )]
    fn runs_cache(&self, Parameters(r): Parameters<RunId>) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.cache_decisions(&r.run_id))
    }
    #[tool(
        description = "Discover Preflight version, capabilities, limits and unsupported features"
    )]
    fn server_capabilities(&self) -> Result<CallToolResult, ErrorData> {
        result(Ok(crate::control::capabilities(&self.engine)))
    }
    #[tool(
        description = "Read captured input hashes and configuration identity, 100 paths per page; no Git required"
    )]
    fn runs_inputs(
        &self,
        Parameters(r): Parameters<crate::inputs::InputQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.input_page(&r))
    }
    #[tool(
        description = "Compare actual captured paths/hashes and configuration between two runs in one project; 100 changes per page"
    )]
    fn runs_compare_inputs(
        &self,
        Parameters(r): Parameters<crate::inputs::CompareQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.compare_inputs(&r))
    }
    #[tool(
        description = "Publish a successful run artifact as a verified project-scoped module addressed by SHA-256"
    )]
    fn modules_publish(
        &self,
        Parameters(r): Parameters<crate::modules::Publish>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::modules::publish(&self.engine, &r))
    }
    #[tool(description = "Read prepared module metadata, producer and path by project and SHA-256")]
    fn modules_get(
        &self,
        Parameters(r): Parameters<crate::modules::Get>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module(&r.project_id, &r.sha256))
    }
    #[tool(
        description = "Read verified module input paths and producer provenance recorded for a run; 100 per page"
    )]
    fn runs_module_inputs(
        &self,
        Parameters(r): Parameters<crate::inputs::InputQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module_inputs(&r))
    }
    #[tool(
        description = "Read a stored directory module manifest, at most 100 paths per page; file modules return null manifest"
    )]
    fn modules_manifest(
        &self,
        Parameters(r): Parameters<crate::modules::ManifestQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module_manifest(&r))
    }
    #[tool(
        description = "Start an explicit pipeline from an exact local Git commit and ordered text patches; never changes developer checkout"
    )]
    fn pipeline_start_git(
        &self,
        Parameters(r): Parameters<crate::sources::StartGitPipeline>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start_git_pipeline(r)
                .map(|run| serde_json::json!({"run_id":run.id,"status":run.status})),
        )
    }
    #[tool(
        description = "Start an explicit pipeline from checked Git working-tree bytes, deletions and selected untracked files; preserves checkout and index"
    )]
    fn pipeline_start_dirty(
        &self,
        Parameters(r): Parameters<crate::sources::StartDirtyPipeline>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start_dirty_pipeline(r)
                .map(|run| serde_json::json!({"run_id":run.id,"status":run.status})),
        )
    }
    #[tool(
        description = "Start a pipeline from 1..8 named registered Git repositories with explicit non-overlapping workspace paths and Git/dirty revisions"
    )]
    fn pipeline_start_multi(
        &self,
        Parameters(r): Parameters<crate::multi_source::StartMultiPipeline>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start_multi_pipeline(r)
                .map(|run| serde_json::json!({"run_id":run.id,"status":run.status})),
        )
    }
    #[tool(
        description = "Read one source root input manifest, at most 100 paths per page; runs_source lists root names and aggregate identity"
    )]
    fn runs_source_root_inputs(
        &self,
        Parameters(r): Parameters<crate::multi_source::RootInputsQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.source_root_inputs(&r))
    }
    #[tool(
        description = "Read requested Git source identity, prepared snapshot path and tree/config hashes; patches summarized by digest"
    )]
    fn runs_source(&self, Parameters(r): Parameters<RunId>) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.source_evidence(&r.run_id))
    }
    #[tool(
        description = "Read one stored source patch by zero-based ordinal; optional root_name selects a multi-repository root; at most 32 KiB"
    )]
    fn runs_source_patch(
        &self,
        Parameters(r): Parameters<crate::sources::PatchQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.source_patch(&r))
    }
    #[tool(description = "Get the pipeline JSON Schema including fixture actions")]
    fn pipeline_schema(&self) -> Result<CallToolResult, ErrorData> {
        result(Ok(crate::control::schema()))
    }
    #[tool(description = "Read a registered project without changing filesystem authority")]
    fn projects_get(
        &self,
        Parameters(r): Parameters<crate::control::ProjectId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.project(&r.project_id))
    }
    #[tool(
        description = "List up to 100 saved pipeline names and latest revisions; explicit truncation flag"
    )]
    fn pipeline_list(
        &self,
        Parameters(r): Parameters<crate::control::ProjectId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.pipeline_list(&r.project_id))
    }
    #[tool(description = "Read/export a saved pipeline definition; omit revision for latest")]
    fn pipeline_get(
        &self,
        Parameters(r): Parameters<crate::control::ConfigRef>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.pipeline_config(&r))
    }
    #[tool(
        description = "Validate and save an immutable pipeline revision. expected_revision=0 creates; stale updates fail"
    )]
    fn pipeline_save(
        &self,
        Parameters(r): Parameters<crate::control::SavePipeline>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.save_pipeline(&r))
    }
    #[tool(
        description = "Preview retry of exactly one stage or test from a terminal run. Reuses only successful dependency artifacts, captures current project files unless source supplied; does not execute or change the parent."
    )]
    fn pipeline_retry_preview(
        &self,
        Parameters(r): Parameters<crate::retry::Selection>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::retry::preview(&self.engine, &r))
    }
    #[tool(
        description = "Start an immutable partial retry with actor/reason and idempotency_key. Exactly one stage; optional fixture case or explicitly configured testing.rerun command. Original results stay unchanged; this is not a whole-pipeline verdict."
    )]
    fn pipeline_retry(
        &self,
        Parameters(r): Parameters<crate::retry::Start>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            crate::retry::start(&self.engine, r)
                .map(|r| serde_json::json!({"run_id":r.id,"status":r.status,"retry":r.retry})),
        )
    }
    #[tool(
        description = "Run an explicit saved pipeline revision without project YAML files; optional idempotency_key safely replays the same request"
    )]
    fn pipeline_run_saved(
        &self,
        Parameters(r): Parameters<crate::profiles::SavedStart>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            crate::profiles::start(&self.engine, r)
                .map(|r| serde_json::json!({"run_id":r.id,"status":r.status})),
        )
    }
    #[tool(
        description = "Page compact run history by project/status, newest first; pass next_cursor as before"
    )]
    fn runs_list(
        &self,
        Parameters(r): Parameters<crate::control::RunQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.query_runs(&r))
    }
    #[tool(
        description = "Save a language-neutral toolchain profile: tools, argv defaults, optional probes and environment"
    )]
    fn toolchain_save(
        &self,
        Parameters(r): Parameters<crate::profiles::SaveProfile>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::profiles::save(&self.engine, &r))
    }
    #[tool(
        description = "Read/export a toolchain profile revision with optional explicit content pins"
    )]
    fn toolchain_get(
        &self,
        Parameters(r): Parameters<crate::control::ConfigRef>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::profiles::get(&self.engine, &r))
    }
    #[tool(description = "List project toolchain profiles and latest revisions")]
    fn toolchain_list(
        &self,
        Parameters(r): Parameters<crate::control::ProjectId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::profiles::list(&self.engine, &r.project_id))
    }
    #[tool(description = "Get language-neutral toolchain profile JSON Schema")]
    fn toolchain_schema(&self) -> Result<CallToolResult, ErrorData> {
        result(Ok(crate::profiles::schema()))
    }
    #[tool(
        description = "Run explicitly configured probe argv as an asynchronous pipeline; no automatic version inference"
    )]
    fn toolchain_probe(
        &self,
        Parameters(r): Parameters<crate::profiles::Probe>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            crate::profiles::probe(&self.engine, r)
                .map(|r| serde_json::json!({"run_id":r.id,"status":r.status})),
        )
    }
    #[tool(
        description = "Inspect a permitted local executable/file/SDK tree to obtain a pin digest; the pin is checked again during preparation"
    )]
    async fn toolchain_inspect_pin(
        &self,
        Parameters(r): Parameters<crate::pins::Inspect>,
    ) -> Result<CallToolResult, ErrorData> {
        let root = self.engine.db.project(&r.project_id).map(|p| p.root);
        let config = self.engine.config.clone();
        result(match root {
            Ok(root) => {
                match tokio::task::spawn_blocking(move || crate::pins::inspect(&config, &root, &r))
                    .await
                {
                    Ok(r) => r,
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e),
        })
    }
    #[tool(
        description = "Start an explicit pipeline with a pinned profile revision; accepts optional Git/dirty/multi sources and an idempotency key"
    )]
    fn pipeline_start_pinned(
        &self,
        Parameters(r): Parameters<crate::pins::StartPinned>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            crate::profiles::start_pinned(&self.engine, r)
                .map(|run| serde_json::json!({"run_id":run.id,"status":run.status})),
        )
    }
    #[tool(
        description = "Inspect up to 100 most recent durable local operation leases for a run, including protected resources and released/abandoned history. No time-based expiry or cache deletion."
    )]
    fn runs_operations(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.operations(&r.run_id))
    }
    #[tool(
        description = "Explain persisted stage and build-cache identities, including ordered source provenance, optional remote runtime receipt and changed component hashes against a baseline. Null evidence means unknown; diagnostic keys never authorize reuse."
    )]
    fn runs_stage_key(
        &self,
        Parameters(r): Parameters<crate::stage_keys::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::stage_keys::explain(&self.engine.db, &r))
    }
    #[tool(
        description = "Read actually verified toolchain pins, private paths, host/target and declared ABI contract; null verified means unpinned"
    )]
    fn runs_toolchain(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .toolchain(&r.run_id)
                .map(|v| serde_json::json!({"run_id":r.run_id,"verified":v})),
        )
    }
    #[tool(
        description = "List up to 100 producer compatibility contracts for a prepared module, including explicitly unverified legacy records"
    )]
    fn modules_contracts(
        &self,
        Parameters(r): Parameters<crate::modules::ManifestQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.module_contracts(&r))
    }
    #[tool(description = "List up to 100 registered local projects")]
    fn projects_list(&self) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.projects())
    }
    #[tool(description = "Register a local project inside configured allowed_roots")]
    fn projects_register(
        &self,
        Parameters(request): Parameters<RegisterProject>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.register(request))
    }
    #[tool(
        description = "Start an asynchronous native command in an isolated project copy; returns run_id immediately"
    )]
    fn build_start(
        &self,
        Parameters(request): Parameters<StartBuild>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start(request)
                .map(|run| serde_json::json!({"run_id":run.id,"status":run.status})),
        )
    }
    #[tool(description = "Validate a versioned pipeline DAG without starting commands")]
    fn pipeline_validate(
        &self,
        Parameters(file): Parameters<PipelineFile>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            pipeline::validate(&file)
                .map(|_| serde_json::json!({"valid":true,"stages":file.pipeline.stages.len()})),
        )
    }
    #[tool(
        description = "Start a native DAG pipeline; omit definition to load the project's preflight.yaml"
    )]
    fn pipeline_start(
        &self,
        Parameters(request): Parameters<StartPipeline>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start_pipeline(request)
                .map(|r| serde_json::json!({"run_id":r.id,"status":r.status})),
        )
    }
    #[tool(
        description = "Start fixture groups against one compiled artifact; batch_id equals run_id"
    )]
    fn batch_start(
        &self,
        Parameters(request): Parameters<crate::fixtures::StartBatch>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .start_batch(request)
                .map(|r| serde_json::json!({"batch_id":r.id,"run_id":r.id,"status":r.status})),
        )
    }
    #[tool(description = "Run configured YAML fixtures asynchronously")]
    fn fixtures_run(
        &self,
        Parameters(request): Parameters<crate::fixtures::StartBatch>,
    ) -> Result<CallToolResult, ErrorData> {
        self.batch_start(Parameters(request))
    }
    #[tool(description = "List configured fixture groups and case counts without executing")]
    fn fixtures_list(
        &self,
        Parameters(request): Parameters<crate::fixtures::StartBatch>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.configured_batch(&request).map(|p| p.pipeline.stages.iter().filter_map(|s|s.fixtures.as_ref().map(|a|serde_json::json!({"stage_id":s.id,"suite":a.suite.suite,"cases":a.suite.cases.len()}))).collect::<Vec<_>>()))
    }
    #[tool(description = "Validate a YAML fixture group's JSON representation and adapter")]
    fn fixtures_validate(
        &self,
        Parameters(action): Parameters<crate::fixtures::FixtureAction>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::fixtures::validate(&action).map(|_| serde_json::json!({"valid":true})))
    }
    #[tool(description = "Read at most 20 fixture case summaries, counts and next_cursor")]
    fn fixtures_results(
        &self,
        Parameters(request): Parameters<FixturePage>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .fixture_results(&request.run_id, request.offset),
        )
    }
    #[tool(description = "Read one fixture's saved expected/actual values and concise differences")]
    fn fixtures_diff(
        &self,
        Parameters(request): Parameters<FixtureDetail>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .fixture_diff(&request.run_id, &request.stage_id, &request.case_id)
                .map(crate::fixtures::compact_diff),
        )
    }
    #[tool(description = "Read compact batch status and fixture counts")]
    fn batch_status(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::api::compact_batch(&self.engine, &request.run_id))
    }
    #[tool(description = "Read compact batch report and first page of cases")]
    fn batch_report(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        self.batch_status(Parameters(request))
    }
    #[tool(description = "Cancel a batch and all active fixture process trees")]
    fn batch_cancel(
        &self,
        Parameters(request): Parameters<CancelRun>,
    ) -> Result<CallToolResult, ErrorData> {
        self.build_cancel(Parameters(request))
    }
    #[tool(description = "Compare fixture statuses against another batch from the same project")]
    fn batch_compare(
        &self,
        Parameters(request): Parameters<BatchComparison>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .compare_batches(&request.run_id, &request.baseline_id),
        )
    }
    #[tool(
        description = "Create or revise an immutable project experiment under .preflight with optimistic expected_revision. Records pipeline revision, baseline, source, profile, overrides and fixture selection."
    )]
    fn experiment_save(
        &self,
        Parameters(r): Parameters<crate::experiments::Save>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::save(&self.engine, &r))
    }
    #[tool(
        description = "Validate experiment references and return its effective pipeline; does not start commands."
    )]
    fn experiment_validate(
        &self,
        Parameters(r): Parameters<crate::experiments::Save>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::validate(&self.engine, &r))
    }
    #[tool(description = "Read an immutable experiment definition revision.")]
    fn experiment_get(
        &self,
        Parameters(r): Parameters<crate::control::ConfigRef>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::get(&self.engine, &r))
    }
    #[tool(description = "List up to 100 project experiment names and latest revisions.")]
    fn experiments_list(
        &self,
        Parameters(r): Parameters<crate::control::ProjectId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::list(&self.engine, &r))
    }
    #[tool(
        description = "Start an explicit experiment revision with required idempotency key. Returns run_id; shared engine and cancellation semantics apply."
    )]
    fn experiment_start(
        &self,
        Parameters(r): Parameters<crate::experiments::Start>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::start(&self.engine, r))
    }
    #[tool(description = "Read compact experiment run status and exact definition revision.")]
    fn experiment_status(
        &self,
        Parameters(r): Parameters<crate::experiments::Run>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::status(&self.engine, &r))
    }
    #[tool(
        description = "Cancel one project experiment run with actor/reason; preserve results and history."
    )]
    fn experiment_cancel(
        &self,
        Parameters(r): Parameters<crate::experiments::Cancel>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::cancel(&self.engine, r))
    }
    #[tool(
        description = "Compare two same-project runs: stage/test outcomes, artifact hashes, input identity and toolchain equality. Byte-bounded pages use offset/next_cursor per change category. Missing test evidence stays unknown; no performance claim. Source diff and diagnostic tools are referenced separately."
    )]
    fn runs_compare(
        &self,
        Parameters(r): Parameters<crate::run_comparison::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::run_comparison::compare(&self.engine, &r))
    }
    #[tool(
        description = "Compare an experiment run against an explicit baseline: inputs, individual test outcomes, artifact hashes and toolchain compatibility. Bounded pages; missing evidence stays unknown."
    )]
    fn experiment_compare(
        &self,
        Parameters(r): Parameters<crate::experiments::Compare>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::experiments::compare(&self.engine, &r))
    }
    #[tool(
        description = "Advance the durable bounded diagnostic index and page normalized groups with log evidence IDs. Optional same-project baseline labels new/fixed only when both indexes are complete; partial indexes stay unknown. Repeat to continue indexing; output offset pages groups independently. Filters: severity, normalized file, literal query. Formats: rustc JSON/text, GCC/Clang, Go, generic. No test counts inferred."
    )]
    async fn diagnostics_report(
        &self,
        Parameters(r): Parameters<crate::diagnostics::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        result(
            tokio::task::spawn_blocking(move || crate::diagnostics::report(&e, &r))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r),
        )
    }
    #[tool(
        description = "Retrieve bounded redacted build/test log evidence. Filters: project, run/batch, stage, exact prepared Git SHA, normalized severity/file, test/kind/stream and from_ms-inclusive/to_ms-exclusive time. Empty query browses; literal terms use local FTS5. Normalized filters report indexing completeness. Reuse evidence IDs and cursor."
    )]
    fn evidence_search(
        &self,
        Parameters(r): Parameters<crate::evidence::Search>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::evidence::search(&self.engine, &r))
    }
    #[tool(
        description = "Build compact retrieval context with stable citations from local FTS5. max_chars defaults to 6000; no external embedding service or generated answer. Use evidence_read to expand a citation."
    )]
    fn evidence_context(
        &self,
        Parameters(r): Parameters<crate::evidence::Search>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::evidence::context(&self.engine, &r))
    }
    #[tool(
        description = "Expand authorized evidence with bounded neighbors. For complete large-event reading use offset:0,neighbors:0,max_bytes; repeat with next_offset. Offsets are UTF-8 bytes in the redacted text and remain stable after compression. No binary data."
    )]
    fn evidence_read(
        &self,
        Parameters(r): Parameters<crate::evidence::Read>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::evidence::read(&self.engine, &r))
    }

    #[tool(
        description = "Observe cross-project active, planned or recent work; paginated metadata and stage-based progress, including experiments."
    )]
    fn executions_list(
        &self,
        Parameters(q): Parameters<crate::executions::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::executions::list(&self.engine, &q))
    }
    #[tool(
        description = "Compact cross-project progress since a monitoring timestamp; active stages, pending changes and planned schedules. Percentages represent completed work units, never estimated time."
    )]
    fn executions_summary(
        &self,
        Parameters(q): Parameters<crate::executions::Summary>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::executions::summary(&self.engine, &q))
    }
    #[tool(
        description = "Cancel one project-owned run, change queue, pending queue item (ordinal), or future schedule (revision). Active experiment runs use target_kind=run. History is retained."
    )]
    fn executions_cancel(
        &self,
        Parameters(q): Parameters<crate::executions::Cancel>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::executions::cancel(&self.engine, &q))
    }
    #[tool(
        description = "Check pipeline JSON text with line/column and structured-path diagnostics."
    )]
    fn pipeline_check(
        &self,
        Parameters(q): Parameters<crate::pipeline_editor::Check>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::pipeline_editor::check(&q))
    }
    #[tool(description = "Page immutable revisions of a named project pipeline.")]
    fn pipeline_revisions(
        &self,
        Parameters(q): Parameters<crate::pipeline_editor::Revisions>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::pipeline_editor::revisions(&self.engine, &q))
    }
    #[tool(
        description = "Restore a previous pipeline definition as a NEW revision, guarded by expected_revision; preserves all history."
    )]
    fn pipeline_restore(
        &self,
        Parameters(q): Parameters<crate::pipeline_editor::Restore>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::pipeline_editor::restore(&self.engine, &q))
    }
    #[tool(
        description = "Page workspaces, pipelines or experiments with server-side search and a stable cursor."
    )]
    fn catalog_list(
        &self,
        Parameters(q): Parameters<crate::catalog::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::catalog::list(&self.engine, &q))
    }
    #[tool(description = "Inspect project-scoped local evidence index counts and size.")]
    fn evidence_stats(
        &self,
        Parameters(r): Parameters<crate::evidence::Project>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::evidence::stats(&self.engine, &r))
    }
    #[tool(
        description = "Read the recorded acceptance verdict and each criterion with observed values. Unknown evidence never passes; configured criteria gate successful pipeline completion."
    )]
    fn runs_acceptance(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::acceptance::read(&self.engine, &r.run_id))
    }
    #[tool(
        description = "Search project/run/stage logs with up to 100 bounded redacted excerpts, stable evidence IDs and a cursor. Literal terms only; full logs remain available through logs_read."
    )]
    fn logs_search(
        &self,
        Parameters(r): Parameters<crate::log_search::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::log_search::search(&self.engine, &r))
    }
    #[tool(
        description = "Read up to 100 individual JUnit case results for one project/run, with per-stage completeness and next_cursor. Missing cases remain unknown; fixtures_results is separate."
    )]
    fn tests_results(
        &self,
        Parameters(r): Parameters<crate::inspection::Tests>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::inspection::tests(&self.engine, &r))
    }
    #[tool(
        description = "Read verified captured source for one reported JUnit test, up to 256 KiB. Includes validated VS Code URI; current project file opens only if its bytes match captured source."
    )]
    fn tests_source(
        &self,
        Parameters(r): Parameters<crate::inspection::Source>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::inspection::source(&self.engine, &r))
    }
    #[tool(
        description = "List up to 100 project pipeline identities and latest runs. Filter runs_list by pipeline_key to keep histories separate."
    )]
    fn project_pipelines(
        &self,
        Parameters(r): Parameters<crate::inspection::Pipelines>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::inspection::pipelines(&self.engine, &r))
    }
    #[tool(description = "Get persisted stage outcomes, timing and test details for a run")]
    fn stages_get(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.run(&request.run_id).map(|mut r| {
            for stage in &mut r.stages {
                if let Some(t) = &mut stage.tests {
                    t.cases.clear();
                    t.cases_complete = false;
                }
            }
            r.stages
        }))
    }
    #[tool(
        description = "List immutable artifact metadata, availability (available, artifact_expired or artifact_missing), and local paths; no binary content"
    )]
    fn artifacts_list(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .run(&request.run_id)
                .and_then(|_| self.engine.db.artifacts_report(&request.run_id)),
        )
    }
    #[tool(
        description = "Preview up to ten nominal UTC occurrence timestamps for one-shot, interval or five-field numeric cron in an IANA timezone. No schedule or run is created; overlap/misfire decisions happen at dispatch."
    )]
    fn schedules_preview(
        &self,
        Parameters(r): Parameters<crate::schedules::Preview>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::preview(&r))
    }
    #[tool(
        description = "Read project-scoped schedule occurrences and live run status, twenty per page; pass next_cursor as before_at."
    )]
    fn schedules_history(
        &self,
        Parameters(r): Parameters<crate::schedules::History>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::history(&self.engine, &r))
    }
    #[tool(
        description = "Create an audited one-shot, fixed-interval or timezone-aware cron schedule for an immutable saved pipeline/profile revision. at_ms is absolute UTC Unix milliseconds; misfire run_once or skip with grace_ms. Same request_id never creates another schedule. Optional repeat specifies either every_seconds (60..31536000) or cron plus IANA timezone, with overlap skip/queue; missed intervals coalesce. Source is captured at dispatch time."
    )]
    fn schedules_create(
        &self,
        Parameters(r): Parameters<crate::schedules::Create>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::create(&self.engine, &r))
    }
    #[tool(
        description = "List project schedules, 20 per page; executions_list(view=planned) provides cross-project planned work."
    )]
    fn schedules_list(
        &self,
        Parameters(r): Parameters<crate::schedules::List>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::list(&self.engine, &r))
    }
    #[tool(description = "Read a schedule, its revision/run_id and recent audit events.")]
    fn schedules_get(
        &self,
        Parameters(r): Parameters<crate::schedules::Id>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::get(&self.engine, &r))
    }
    #[tool(
        description = "Edit future pending/paused schedule configuration with expected_revision. Definition uses the create shape and a new audited request_id; replay is idempotent. Keeps schedule identity, history and pause state; dispatched runs are untouched."
    )]
    fn schedules_update(
        &self,
        Parameters(r): Parameters<crate::schedules::Update>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::update(&self.engine, &r))
    }
    #[tool(
        description = "Pause/resume/cancel or soft-delete future dispatch using expected_revision and an audited request_id. Already dispatched runs must be cancelled separately with pipeline_cancel."
    )]
    fn schedules_change(
        &self,
        Parameters(r): Parameters<crate::schedules::Change>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::change(&self.engine, &r))
    }
    #[tool(
        description = "Dispatch a bounded set of due schedules; omit project_id for all projects. Concurrent calls and crash recovery reuse stable run idempotency keys. Server also dispatches internally."
    )]
    fn schedules_run_due(
        &self,
        Parameters(r): Parameters<crate::schedules::Due>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::schedules::run_due(&self.engine, &r))
    }
    #[tool(
        description = "Losslessly compress a bounded batch of completed-run logs into zstd chunks. Repeat safely to resume; cursor IDs and search stay unchanged. Background archiving also runs automatically."
    )]
    async fn logs_archive(
        &self,
        Parameters(r): Parameters<crate::log_archive::Request>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::log_archive::archive(self.engine.clone(), r).await)
    }
    #[tool(
        description = "Read raw/compressed log payload byte counts and archive progress. Counts exclude SQLite pages and the redacted FTS index; no disk-free-space claim."
    )]
    fn logs_storage(
        &self,
        Parameters(r): Parameters<crate::log_archive::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.logs_storage(&r))
    }
    #[tool(
        description = "Pin or release one owner of a run artifact. Project/run/stage/artifact identity is exact; revision prevents lost updates, request_id gives exact replay. Pins block expiry; releasing never deletes files."
    )]
    fn artifact_pin_set(
        &self,
        Parameters(r): Parameters<crate::artifact_retention::Pin>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.artifact_pin_set(&r))
    }
    #[tool(
        description = "Read artifact availability and retention owners, including released revisions; 100 owners per page. Other stage pins may also protect its output directory."
    )]
    fn artifact_retention(
        &self,
        Parameters(r): Parameters<crate::artifact_retention::Query>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.db.artifact_retention(&r))
    }
    #[tool(description = "Get persisted build state")]
    fn build_status(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::api::compact_report(&self.engine, &request.run_id))
    }
    #[tool(
        description = "Cancel one selected pipeline run: remove queued work or stop its active stages and process trees; retain history, logs and published evidence. Completion is asynchronous; poll build_status."
    )]
    fn pipeline_cancel(
        &self,
        Parameters(request): Parameters<CancelRun>,
    ) -> Result<CallToolResult, ErrorData> {
        self.build_cancel(Parameters(request))
    }
    #[tool(description = "Cancel a build; records user or agent actor and reason")]
    fn build_cancel(
        &self,
        Parameters(request): Parameters<CancelRun>,
    ) -> Result<CallToolResult, ErrorData> {
        result(self.engine.cancel(request).map(
            |r| serde_json::json!({"run_id":r.id,"status":r.status,"cancel_actor":r.cancel_actor}),
        ))
    }
    #[tool(
        description = "Read the exact recorded pipeline definition for a run, or its native command. Use only when detailed configuration is needed; build_report references this tool to avoid repeating definitions."
    )]
    fn runs_definition(
        &self,
        Parameters(r): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::api::run_definition(&self.engine, &r.run_id))
    }
    #[tool(description = "Get compact build report; unknown test counts are null")]
    fn build_report(
        &self,
        Parameters(request): Parameters<RunId>,
    ) -> Result<CallToolResult, ErrorData> {
        result(crate::api::compact_report(&self.engine, &request.run_id))
    }
    #[tool(
        description = "Read bounded stdout/stderr chunks with a cursor; at most 16 events per page"
    )]
    fn logs_read(
        &self,
        Parameters(request): Parameters<ReadLogs>,
    ) -> Result<CallToolResult, ErrorData> {
        result(
            self.engine
                .db
                .stage_logs(&request.run_id, request.stage.as_deref(), request.after),
        )
    }
}
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadLogs {
    pub run_id: String,
    #[serde(default)]
    pub after: i64,
    pub stage: Option<String>,
}
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FixturePage {
    pub run_id: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FixtureDetail {
    pub run_id: String,
    pub stage_id: String,
    pub case_id: String,
}
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchComparison {
    pub run_id: String,
    pub baseline_id: String,
}
#[tool_handler(router = self.tool_router)]
impl ServerHandler for Mcp {
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![
                ResourceTemplate::new("preflight://batches/{run_id}", "Batch compact report")
                    .with_mime_type("application/json"),
                ResourceTemplate::new(
                    "preflight://projects/{project_id}/artifacts/{sha256}",
                    "Scoped artifact metadata",
                )
                .with_mime_type("application/json"),
                ResourceTemplate::new(
                    "preflight://projects/{project_id}/completions",
                    "Project completion cursor",
                )
                .with_mime_type("application/json"),
                ResourceTemplate::new("preflight://runs/{run_id}", "Run compact report")
                    .with_mime_type("application/json"),
                ResourceTemplate::new(
                    "preflight://runs/{run_id}/diagnostics",
                    "Run normalized diagnostics",
                )
                .with_mime_type("application/json"),
            ],
            ..Default::default()
        })
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if let Some(id) = request.uri.strip_prefix("preflight://batches/") {
            uuid::Uuid::parse_str(id)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let value = crate::api::compact_batch(&self.engine, id)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                value.to_string(),
                request.uri,
            )])
            .into());
        }
        if let Some(path) = request.uri.strip_prefix("preflight://projects/")
            && let Some((project, sha)) = path.split_once("/artifacts/")
        {
            self.engine
                .db
                .project(project)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            crate::modules::validate_digest(sha)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let rows = {
                let c = self
                    .engine
                    .db
                    .connection()
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut q=c.prepare("SELECT a.run_id,a.stage_id,a.name,a.size,EXISTS(SELECT 1 FROM artifact_expiry x WHERE x.run_id=a.run_id AND x.stage_id=a.stage_id) FROM artifacts a JOIN runs r ON r.id=a.run_id WHERE r.project_id=?1 AND a.sha256=?2 ORDER BY a.run_id DESC,a.stage_id,a.name LIMIT 21").map_err(|e|ErrorData::internal_error(e.to_string(),None))?;
                q.query_map(rusqlite::params![project,sha],|r|Ok(serde_json::json!({"run_id":r.get::<_,String>(0)?,"stage_id":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"size":r.get::<_,u64>(3)?,"state":if r.get::<_,bool>(4)?{"artifact_expired"}else{"metadata_only"}}))).map_err(|e|ErrorData::internal_error(e.to_string(),None))?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|ErrorData::internal_error(e.to_string(),None))?
            };
            let value = serde_json::json!({"project_id":project,"sha256":sha,"truncated":rows.len()>20,"owners":rows.into_iter().take(20).collect::<Vec<_>>(),"availability_tool":"artifacts_list","content":"authorized local/HTTP path; binary data never embedded in MCP"});
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                value.to_string(),
                request.uri,
            )])
            .into());
        }
        if let Some(path) = request.uri.strip_prefix("preflight://runs/") {
            let (id, diagnostics) = path
                .strip_suffix("/diagnostics")
                .map_or((path, false), |id| (id, true));
            uuid::Uuid::parse_str(id)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let run = self
                .engine
                .db
                .run(id)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let value = if diagnostics {
                let r: crate::diagnostics::Query = serde_json::from_value(
                    serde_json::json!({"project_id":run.project_id,"run_id":id}),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let e = self.engine.clone();
                tokio::task::spawn_blocking(move || crate::diagnostics::report(&e, &r))
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
            } else {
                crate::api::compact_report(&self.engine, id)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
            };
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                value.to_string(),
                request.uri,
            )])
            .into());
        }
        let project = crate::events::project(&request.uri)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let value = self
            .engine
            .db
            .completion_tail(project)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(value.to_string(), request.uri)])
                .into(),
        )
    }
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let uris = requested.resource_subscriptions.as_ref()?;
        if uris.len() > 32 {
            return None;
        }
        let valid = uris
            .iter()
            .filter(|uri| {
                crate::events::project(uri).is_ok_and(|p| self.engine.db.project(p).is_ok())
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut accepted = SubscriptionFilter::new();
        accepted.resource_subscriptions = Some(valid);
        Some(accepted)
    }
    async fn listen(&self, context: rmcp::service::SubscriptionContext) -> Result<(), ErrorData> {
        let uris = context
            .accepted()
            .resource_subscriptions
            .clone()
            .unwrap_or_default();
        let mut cursors = std::collections::HashMap::new();
        loop {
            if self.engine.is_stopping() {
                return Ok(());
            }
            for uri in &uris {
                let project = crate::events::project(uri)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
                let cursor = self
                    .engine
                    .db
                    .completion_cursor(project)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                if cursors.get(uri) != Some(&cursor) {
                    context
                        .sink()
                        .notify_resource_updated(uri.clone())
                        .await
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    cursors.insert(uri.clone(), cursor);
                }
            }
            tokio::select! {_=context.cancelled()=>return Ok(()),_=tokio::time::sleep(std::time::Duration::from_millis(200))=>{}}
        }
    }
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        let project = crate::events::project(&request.uri)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
            .to_owned();
        self.engine
            .db
            .project(&project)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let token = tokio_util::sync::CancellationToken::new();
        {
            let mut watches = self
                .watches
                .0
                .lock()
                .map_err(|_| ErrorData::internal_error("watch mutex poisoned", None))?;
            watches.retain(|_, token| !token.is_cancelled());
            if watches.contains_key(&request.uri) {
                return Ok(());
            }
            if watches.len() >= 32 {
                return Err(ErrorData::invalid_params(
                    "at most 32 resource subscriptions",
                    None,
                ));
            }
            watches.insert(request.uri.clone(), token.clone());
        }
        let engine = self.engine.clone();
        let peer = context.peer;
        tokio::spawn(async move {
            let mut previous = None;
            loop {
                if token.is_cancelled() || peer.is_transport_closed() || engine.is_stopping() {
                    break;
                }
                let Ok(cursor) = engine.db.completion_cursor(&project) else {
                    break;
                };
                if previous != Some(cursor) {
                    if peer
                        .notify_resource_updated(ResourceUpdatedNotificationParam::new(
                            request.uri.clone(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    previous = Some(cursor);
                }
                tokio::select! {_=token.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(200))=>{}}
            }
            token.cancel();
        });
        Ok(())
    }
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        let mut watches = self
            .watches
            .0
            .lock()
            .map_err(|_| ErrorData::internal_error("watch mutex poisoned", None))?;
        if let Some(token) = watches.remove(&request.uri) {
            token.cancel();
        }
        Ok(())
    }
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().enable_resources_subscribe().build()).with_server_info(Implementation::new("preflight", env!("CARGO_PKG_VERSION"))).with_instructions("Preflight: trusted native DAG builds. Read events_read by project cursor or subscribe to its completion resource URI; notifications are hints, durable events are authoritative. Use logs_read for details. No test counts inferred from exit status. Service-owned descriptions, status text and error messages are English only, independent of client locale. Preserve project names, source, paths, logs and other user data verbatim.")
    }
}
pub fn service(engine: Arc<Engine>) -> StreamableHttpService<Mcp, LocalSessionManager> {
    let stop = engine.transport_stop();
    StreamableHttpService::new(
        move || Ok(Mcp::new(engine.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(stop)
            .with_legacy_session_mode(true)
            .with_json_response(true),
    )
}

#[cfg(test)]
mod language_tests {
    use super::*;
    #[test]
    fn service_errors_are_english_and_untrusted_diagnostics_are_not_localized_as_service_text() {
        let known = result(Err::<serde_json::Value, _>(invalid("invalid project ID"))).unwrap();
        assert_eq!(known.is_error, Some(true));
        assert_eq!(
            known.content[0].as_text().unwrap().text,
            "invalid project ID"
        );
        let diagnostic = "\u{43e}\u{448}\u{438}\u{431}\u{43a}\u{430}";
        let unknown = result(Err::<serde_json::Value, _>(anyhow::anyhow!(diagnostic))).unwrap();
        assert_eq!(unknown.is_error, Some(true));
        assert_eq!(
            unknown.content[0].as_text().unwrap().text,
            "operation failed; see server log"
        );
    }
    #[test]
    fn compact_responses_preserve_original_language_data() {
        let name = "\u{41f}\u{440}\u{43e}\u{435}\u{43a}\u{442}";
        let response = result(Ok(
            serde_json::json!({"status":"succeeded","name":name,"path":format!("{name}/test.rs")}),
        ))
        .unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["name"], name);
        assert_eq!(payload["path"], format!("{name}/test.rs"));
    }
}
