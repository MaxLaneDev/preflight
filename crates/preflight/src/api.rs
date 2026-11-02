use crate::{
    engine::{Engine, aggregate, stage_group},
    model::*,
    pipeline::{self, PipelineFile, StageKind, StartPipeline},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct ApiError(anyhow::Error);
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self.0.downcast_ref::<Fault>() {
            Some(Fault::Invalid(message)) => {
                (StatusCode::BAD_REQUEST, "invalid_request", message.clone())
            }
            Some(Fault::NotFound) => (StatusCode::NOT_FOUND, "not_found", "not found".into()),
            Some(Fault::IdempotencyConflict) => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "idempotency key is already bound to a different request".into(),
            ),
            Some(Fault::Conflict) => (
                StatusCode::CONFLICT,
                "revision_conflict",
                "configuration revision conflict; read latest revision and retry explicitly".into(),
            ),
            Some(Fault::Busy) => (
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "resource is busy; retry after the active operation finishes".into(),
            ),
            None => {
                eprintln!("API error: {:#}", self.0);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "operation failed; see server log".into(),
                )
            }
        };
        (
            status,
            Json(json!({"error":{"code":code,"message":message}})),
        )
            .into_response()
    }
}
pub fn router(engine: Arc<Engine>) -> Router {
    let mcp = crate::mcp::service(engine.clone());
    preflight_client::router()
        .route(
            "/api/v1/storage/evidence/preview",
            post(evidence_expiry_preview),
        )
        .route(
            "/api/v1/storage/cache/preview",
            post(cache_eviction_preview),
        )
        .route("/api/v1/ci/sources", get(ci_sources))
        .route("/api/v1/containers", get(containers))
        .route("/api/v1/containers/cleanup", post(container_cleanup))
        .route("/api/v1/access-audit", get(access_audit))
        .route("/api/v1/ci/imports", get(ci_syncs))
        .route("/api/v1/ci/cancel", post(ci_sync_cancel))
        .route("/api/v1/ci/sync", post(ci_sync))
        .route("/api/v1/ci/sync-status", get(ci_sync_status))
        .route("/api/v1/ci/runs", get(ci_runs))
        .route("/api/v1/ci/run", get(ci_run))
        .route("/api/v1/ci/artifact-fetch", post(ci_artifact_fetch))
        .route("/api/v1/ci/artifact-file", get(ci_artifact_file))
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/ready", get(ready))
        .route(
            "/connection",
            get(|request: Request| async move {
                let host = request
                    .headers()
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("127.0.0.1");
                Json(json!({"server": format!("http://{host}"), "bundled": true}))
            }),
        )
        .route(
            "/api/v1/browser-session",
            post(crate::auth::browser_login).delete(crate::auth::browser_logout),
        )
        .route("/api/v1/projects", get(projects).post(register))
        .route("/api/v1/projects/{id}", get(project))
        .route("/api/v1/projects/{id}/pipeline", get(configured_pipeline))
        .route("/api/v1/pipeline-runs", post(start_pipeline))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/pipeline-retry/preview", post(retry_preview))
        .route("/api/v1/pipeline-retry", post(retry_start))
        .route("/api/v1/scheduler", get(scheduler_status))
        .route("/api/v1/worker-recovery", get(worker_recovery_status))
        .route("/api/v1/worker-enrollments", post(worker_enrollment))
        .route("/api/v1/worker-inputs", post(worker_inputs))
        .route("/api/v1/worker-jobs", get(worker_jobs))
        .route("/api/v1/workers", get(workers))
        .route("/api/v1/worker-revoke", post(worker_revoke))
        .route(
            "/api/v1/worker-connections",
            post(worker_ssh_start).get(worker_ssh_list),
        )
        .route("/api/v1/worker-connection", get(worker_ssh_status))
        .route("/api/v1/worker-connections/stop", post(worker_ssh_stop))
        .route(
            "/api/v1/codex-hooks",
            post(codex_hook_bind).get(codex_hook_status),
        )
        .route("/api/v1/codex-hooks/disable", post(codex_hook_disable))
        .route("/api/v1/codex-hooks/retry", post(codex_hook_retry))
        .route("/api/v1/completion-events", get(completion_events))
        .route("/api/v1/catalog", get(catalog_page))
        .route("/api/v1/search", get(global_search))
        .route("/api/v1/live", get(live_events))
        .route("/api/v1/live/snapshot", get(live_snapshot))
        .route("/api/v1/archives", get(archives_list).post(archive_create))
        .route("/api/v1/archive", get(archive_get))
        .route(
            "/api/v1/archive/cleanup/preview",
            post(archive_cleanup_preview),
        )
        .route("/api/v1/archive/pin", post(archive_pin))
        .route("/api/v1/archive/verify", post(archive_verify))
        .route("/api/v1/archive/restore", post(archive_restore))
        .route("/api/v1/archive/download", get(archive_download))
        .route(
            "/api/v1/archive/import",
            post(archive_import).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/v1/management/preview", post(management_preview))
        .route("/api/v1/management/execute", post(management_execute))
        .route("/api/v1/management/trash", get(management_trash))
        .route("/api/v1/management/restore", post(management_restore))
        .route("/api/v1/executions", get(executions_list))
        .route("/api/v1/executions/summary", get(executions_summary))
        .route("/api/v1/executions/cancel", post(executions_cancel))
        .route(
            "/api/v1/pipeline-editor/check",
            post(editor_check).layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route("/api/v1/pipeline-editor/revisions", get(editor_revisions))
        .route("/api/v1/pipeline-editor/restore", post(editor_restore))
        .route(
            "/api/v1/pipeline-schema",
            get(|| async { Json(crate::control::schema()) }),
        )
        .route(
            "/api/v1/pipeline-configs",
            post(save_config).get(get_config),
        )
        .route("/api/v1/projects/{id}/pipeline-configs", get(list_configs))
        .route("/api/v1/saved-pipeline-runs", post(run_saved))
        .route("/api/v1/modules", post(publish_module).get(get_module))
        .route("/api/v1/module-manifest", get(module_manifest))
        .route("/api/v1/module-contracts", get(module_contracts))
        .route("/api/v1/pinned-pipeline-runs", post(run_pinned))
        .route("/api/v1/toolchain-inspect-pin", post(inspect_pin))
        .route("/api/v1/run-toolchain", get(run_toolchain))
        .route("/api/v1/stage-key", get(stage_key))
        .route("/api/v1/run-cache", get(run_cache))
        .route("/api/v1/runs/{id}/shared", get(runs_shared))
        .route("/api/v1/change-queues", post(queue_create).get(queues_list))
        .route("/api/v1/change-queues/{id}", get(queue_status))
        .route("/api/v1/change-queues/cancel", post(queue_cancel))
        .route("/api/v1/module-retention", get(module_retention))
        .route("/api/v1/module-pins", post(module_pin_set))
        .route("/api/v1/artifact-pins", post(artifact_pin_set))
        .route(
            "/api/v1/schedules",
            post(schedules_create).get(schedules_list),
        )
        .route("/api/v1/schedule", get(schedules_get))
        .route("/api/v1/schedules/preview", post(schedules_preview))
        .route("/api/v1/schedule-occurrences", get(schedules_history))
        .route("/api/v1/schedules/change", post(schedules_change))
        .route("/api/v1/schedules/update", post(schedules_update))
        .route("/api/v1/schedules/run-due", post(schedules_due))
        .route("/api/v1/evidence/search", get(evidence_search))
        .route("/api/v1/diagnostics", get(diagnostics_report))
        .route("/api/v1/evidence/read", get(evidence_read))
        .route("/api/v1/evidence/context", get(evidence_context))
        .route("/api/v1/evidence/stats", get(evidence_stats))
        .route("/api/v1/runs/{id}/acceptance", get(run_acceptance))
        .route("/api/v1/logs-archive", post(logs_archive))
        .route("/api/v1/logs-storage", get(logs_storage))
        .route("/api/v1/artifact-retention", get(artifact_retention))
        .route("/api/v1/operations-abandoned", get(operations_abandoned))
        .route("/api/v1/operation-resolve", post(operation_resolve))
        .route("/api/v1/storage-gc-preview", post(storage_gc_preview))
        .route("/api/v1/module-gc-preview", post(module_gc_preview))
        .route(
            "/api/v1/artifact-expiry-preview",
            post(artifact_expiry_preview),
        )
        .route("/api/v1/storage-gc-execute", post(storage_gc_execute))
        .route("/api/v1/storage-gc-status", get(storage_gc_status))
        .route("/api/v1/storage-usage", get(storage_usage))
        .route("/api/v1/storage-policies", get(storage_policy_status))
        .route(
            "/api/v1/server-release",
            get(server_release_status).post(server_release_check),
        )
        .route("/api/v1/server-settings", post(server_settings))
        .route("/api/v1/server-autostart", post(server_autostart))
        .route("/api/v1/cache-cleanup-preview", post(cache_cleanup_preview))
        .route("/api/v1/cache-cleanup-execute", post(cache_cleanup_execute))
        .route("/api/v1/run-operations", get(run_operations))
        .route("/api/v1/git-pipeline-runs", post(run_git))
        .route("/api/v1/dirty-pipeline-runs", post(run_dirty))
        .route("/api/v1/multi-pipeline-runs", post(run_multi))
        .route(
            "/api/v1/run-source-root-inputs",
            get(run_source_root_inputs),
        )
        .route("/api/v1/run-source", get(run_source))
        .route("/api/v1/run-source-patch", get(run_source_patch))
        .route(
            "/api/v1/experiments",
            post(experiment_save).get(experiments_list),
        )
        .route("/api/v1/experiment", get(experiment_get))
        .route("/api/v1/experiments/validate", post(experiment_validate))
        .route("/api/v1/experiment-runs", post(experiment_start))
        .route("/api/v1/experiment-status", get(experiment_status))
        .route("/api/v1/experiments/cancel", post(experiment_cancel))
        .route("/api/v1/experiment-compare", get(experiment_compare))
        .route("/api/v1/log-search", get(log_search))
        .route("/api/v1/project-pipelines", get(project_pipelines))
        .route("/api/v1/test-results", get(test_results))
        .route("/api/v1/test-source", get(test_source))
        .route("/api/v1/run-history", get(run_history))
        .route("/api/v1/run-compare", get(run_compare))
        .route("/api/v1/run-inputs", get(run_inputs))
        .route("/api/v1/run-module-inputs", get(run_module_inputs))
        .route("/api/v1/run-input-diff", get(run_input_diff))
        .route("/api/v1/run-source-diff", get(run_source_diff))
        .route(
            "/api/v1/toolchains",
            post(save_toolchain).get(get_toolchain),
        )
        .route("/api/v1/projects/{id}/toolchains", get(list_toolchains))
        .route(
            "/api/v1/toolchain-schema",
            get(|| async { Json(crate::profiles::schema()) }),
        )
        .route("/api/v1/toolchain-probes", post(probe_toolchain))
        .route("/api/v1/batches", post(start_batch))
        .route("/api/v1/batches/{id}", get(report))
        .route("/api/v1/batches/{id}/report", get(batch_report))
        .route("/api/v1/batches/{id}/cancel", post(cancel))
        .route(
            "/api/v1/batches/{id}/compare/{baseline}",
            get(batch_compare),
        )
        .route("/api/v1/projects/{id}/fixtures", get(fixture_list))
        .route("/api/v1/fixtures/validate", post(fixture_validate))
        .route(
            "/api/v1/fixtures/schema",
            get(|| async { Json(rmcp::schemars::schema_for!(crate::fixtures::FixtureAction)) }),
        )
        .route("/api/v1/runs/{id}/fixtures", get(fixture_results))
        .route(
            "/api/v1/runs/{id}/fixtures/{stage}/{case}",
            get(fixture_diff),
        )
        .route("/api/v1/pipelines/validate", post(validate_pipeline))
        .route("/api/v1/runs", get(runs).post(start))
        .route("/api/v1/runs/{id}", get(status))
        .route("/api/v1/runs/{id}/cancel", post(cancel))
        .route("/api/v1/runs/{id}/logs", get(logs))
        .route("/api/v1/runs/{id}/report", get(report))
        .route("/api/v1/runs/{id}/definition", get(run_definition_http))
        .route("/api/v1/runs/{id}/artifacts", get(artifacts))
        .nest_service("/mcp", mcp)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            engine.clone(),
            crate::auth::guard,
        ))
        .layer(middleware::from_fn(local_request))
        .with_state(engine)
}
async fn local_request(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    let valid_host = host
        .and_then(|h| h.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|a| matches!(a.host(), "127.0.0.1" | "localhost" | "[::1]"));
    let valid_origin = request.headers().get(header::ORIGIN).is_none_or(|origin| {
        origin
            .to_str()
            .ok()
            .is_some_and(|s| Some(s.strip_prefix("http://").unwrap_or("")) == host)
    });
    if !valid_host || !valid_origin {
        return (StatusCode::FORBIDDEN,Json(json!({"error":{"code":"forbidden_origin","message":"local same-origin requests only"}}))).into_response();
    }
    // The import handler bounds the binary stream on disk; do not buffer it as JSON.
    if request.uri().path() == "/api/v1/archive/import" {
        return next.run(request).await;
    }
    let editor_text = request.uri().path() == "/api/v1/pipeline-editor/check";
    let mcp = request.uri().path() == "/mcp";
    let limit = if editor_text || mcp {
        512 * 1024
    } else {
        64 * 1024
    };
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(
                json!({"error":{"code":"body_too_large","message":"request body exceeds 64 KiB"}}),
            ),
        )
            .into_response();
    };
    if bytes.len() > 64 * 1024 && !editor_text {
        let check = mcp
            && serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|v| {
                v["method"] == "tools/call" && v["params"]["name"] == "pipeline_check"
            });
        if !check {
            return (StatusCode::PAYLOAD_TOO_LARGE,Json(json!({"error":{"code":"body_too_large","message":"request body exceeds 64 KiB"}}))).into_response();
        }
    }
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}
async fn ready(State(e): State<Arc<Engine>>) -> Response {
    let ready = !e.is_stopping()
        && crate::docker::ensure_admission(&e.db).is_ok()
        && crate::auth::validate_environment(&e.current_config().access_tokens).is_ok()
        && e.db
            .connection()
            .and_then(|c| {
                Ok(
                    c.query_row("SELECT max(version) FROM schema_version", [], |r| {
                        r.get::<_, usize>(0)
                    })? == crate::migrations::VERSION,
                )
            })
            .unwrap_or(false);
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({"status":if ready{"ready"}else{"not_ready"},"realtime":{"active_streams":crate::live::MAX_STREAMS-e.live_slots.available_permits(),"stream_limit":crate::live::MAX_STREAMS}})),
    )
        .into_response()
}
async fn projects(State(e): State<Arc<Engine>>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(e.db.projects()?))
}
async fn register(
    State(e): State<Arc<Engine>>,
    Json(request): Json<RegisterProject>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(e.register(request)?))
}
async fn project(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(e.db.project(&id)?))
}
async fn runs(State(e): State<Arc<Engine>>) -> Result<Json<Vec<Run>>, ApiError> {
    Ok(Json(e.db.runs()?))
}
async fn start(
    State(e): State<Arc<Engine>>,
    Json(request): Json<StartBuild>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start(request)?)))
}
async fn status(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Run>, ApiError> {
    let mut run = e.db.run(&id)?;
    for stage in &mut run.stages {
        if let Some(t) = &mut stage.tests {
            t.cases.clear();
            t.cases_complete = false;
        }
    }
    if let Some(t) = &mut run.stage.tests {
        t.cases.clear();
        t.cases_complete = false;
    }
    Ok(Json(run))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBody {
    actor: Actor,
    reason: String,
}
async fn cancel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(body): Json<CancelBody>,
) -> Result<Json<Run>, ApiError> {
    Ok(Json(e.cancel(CancelRun {
        run_id: id,
        actor: body.actor,
        reason: body.reason,
    })?))
}
#[derive(Deserialize, Default)]
struct Cursor {
    #[serde(default)]
    after: i64,
    stage: Option<String>,
}
async fn logs(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(cursor): Query<Cursor>,
) -> Result<Json<LogPage>, ApiError> {
    Ok(Json(e.db.stage_logs(
        &id,
        cursor.stage.as_deref(),
        cursor.after,
    )?))
}
async fn report(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(compact_report(&e, &id)?))
}
async fn configured_pipeline(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<PipelineFile>, ApiError> {
    Ok(Json(e.configured_pipeline(&id)?))
}
async fn validate_pipeline(
    Json(file): Json<PipelineFile>,
) -> Result<Json<serde_json::Value>, ApiError> {
    pipeline::validate(&file)?;
    Ok(Json(
        json!({"valid":true,"stages":file.pipeline.stages.len()}),
    ))
}
async fn start_pipeline(
    State(e): State<Arc<Engine>>,
    Json(request): Json<StartPipeline>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start_pipeline(request)?)))
}
async fn artifacts(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    Ok(Json(e.db.artifacts_report(&id)?))
}
pub fn compact_report(e: &Engine, id: &str) -> anyhow::Result<serde_json::Value> {
    let run = e.db.run(id)?;
    let build = aggregate(&stage_group(&run, StageKind::Build));
    let test_stages = stage_group(&run, StageKind::Test);
    let tests = aggregate(&test_stages);
    let known = !test_stages.is_empty() && test_stages.iter().all(|s| s.tests.is_some());
    let mut combined = TestReport::default();
    for report in test_stages.iter().filter_map(|s| s.tests.as_ref()) {
        combined.merge(report.clone());
    }
    let outcome = if run.status == RunState::Failed
        && build == Some(RunState::Succeeded)
        && tests == Some(RunState::Failed)
    {
        "tests_failed"
    } else {
        run.status.as_str()
    };
    let failures=combined.failures.iter().take(3).map(|f|json!({"name":f.name.chars().take(128).collect::<String>(),"message":f.message.chars().take(256).collect::<String>()})).collect::<Vec<_>>();
    let mut submission = e.db.submission(id)?;
    if let Some(value) = submission.as_mut()
        && let Some(object) = value.as_object_mut()
    {
        let experiment =
            object.get("operation").and_then(|v| v.as_str()) == Some("experiment_start");
        object.remove("experiment_definition");
        object.remove("profile_snapshot");
        if let Some(request) = object.get_mut("request").and_then(|v| v.as_object_mut()) {
            request.remove("definition");
            if let Some(selection) = request.get_mut("selection").and_then(|s| s.as_object_mut()) {
                selection.remove("stage_override");
                selection.remove("source");
            }
            if request.remove("source").is_some() {
                object.insert("source_tool".into(), json!("runs_source"));
            }
        }
        object.insert(
            "definition_tool".into(),
            json!(if experiment {
                "experiment_get"
            } else {
                "runs_definition"
            }),
        );
    }
    let stage_failures = run
        .stages
        .iter()
        .filter(|stage| {
            matches!(
                stage.status,
                RunState::Failed | RunState::TimedOut | RunState::InfrastructureError
            )
        })
        .collect::<Vec<_>>();
    let failed_count = stage_failures.len();
    let stage_failures=stage_failures.into_iter().take(3).map(|s|json!({"stage_id":s.definition.id,"status":s.status,"exit_code":s.exit_code,"reason":s.reason.as_ref().map(|r|r.chars().take(256).collect::<String>()),"logs_read":{"run_id":id,"stage":s.definition.id}})).collect::<Vec<_>>();
    let mut acceptance = crate::acceptance::read(e, id)?;
    if let Some(object) = acceptance.as_object_mut() {
        object.remove("criteria");
        object.insert("details_tool".into(), json!("runs_acceptance"));
    }
    Ok(
        json!({"run_id":run.id,"external":run.external,"status":run.status,"outcome":outcome,"retry":run.retry,"submission":submission,
        "build":{"status":build,"exit_code":if run.pipeline.is_none(){run.exit_code}else{None},"cached":false},
        "acceptance":acceptance,
        "tests":{"status":tests,"passed":known.then_some(combined.counts.passed),"failed":known.then_some(combined.counts.failed),"skipped":known.then_some(combined.counts.skipped),"failures":failures,"truncated":combined.truncated||combined.failures.len()>3},
        "stage_failures":stage_failures,"stage_failures_truncated":failed_count>3,
        "stages":{"failed":failed_count,"total":run.stages.len(),"succeeded":run.stages.iter().filter(|s|s.status==RunState::Succeeded).count(),"blocked":run.stages.iter().filter(|s|s.status==RunState::Blocked).count()},
        "expired_payloads":run.expired_payloads,"context_levels":{"compact":"build_report","diagnostic":"diagnostics_report","detailed":["evidence_read","runs_source_diff","fixtures_diff"]},"diagnostics":crate::diagnostic_index::compact(e,id,run.status.terminal())?,"cancel_actor":run.cancel_actor,"reason":run.reason,"details_uri":format!("/api/v1/runs/{id}"),"logs_uri":format!("/api/v1/runs/{id}/logs"),"artifacts_uri":format!("/api/v1/runs/{id}/artifacts")}),
    )
}

async fn start_batch(
    State(e): State<Arc<Engine>>,
    Json(request): Json<crate::fixtures::StartBatch>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start_batch(request)?)))
}
async fn fixture_list(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let p = e.configured_batch(&crate::fixtures::StartBatch {
        project_id: id,
        path: "preflight-fixtures.yaml".into(),
    })?;
    Ok(Json(
        json!({"groups":p.pipeline.stages.iter().filter_map(|s|s.fixtures.as_ref().map(|a|json!({"stage_id":s.id,"suite":a.suite.suite,"cases":a.suite.cases.len()}))).collect::<Vec<_>>()}),
    ))
}
async fn fixture_validate(
    Json(action): Json<crate::fixtures::FixtureAction>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::fixtures::validate(&action)?;
    Ok(Json(json!({"valid":true,"cases":action.suite.cases.len()})))
}
#[derive(Deserialize)]
struct FixtureCursor {
    #[serde(default)]
    offset: usize,
}
async fn fixture_results(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<FixtureCursor>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.fixture_results(&id, q.offset)?))
}
async fn fixture_diff(
    State(e): State<Arc<Engine>>,
    Path((id, stage, case)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.fixture_diff(&id, &stage, &case)?))
}
pub fn compact_batch(e: &Engine, id: &str) -> anyhow::Result<serde_json::Value> {
    let mut report = compact_report(e, id)?;
    report["fixtures"] = e.db.fixture_results(id, 0)?;
    report["batch_id"] = id.into();
    Ok(report)
}
async fn batch_report(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(compact_batch(&e, &id)?))
}
async fn batch_compare(
    State(e): State<Arc<Engine>>,
    Path((id, baseline)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.compare_batches(&id, &baseline)?))
}

async fn capabilities(
    State(e): State<Arc<Engine>>,
    principal: Option<axum::Extension<crate::auth::Principal>>,
) -> Json<serde_json::Value> {
    let mut value = crate::control::capabilities(&e);
    value["access"] = json!({"mode":if e.current_config().access_tokens.is_empty(){"trusted_loopback"}else{"bearer"},"role":principal.map(|p|serde_json::to_value(p.role).unwrap_or_default()).unwrap_or(json!("local_owner")),"scope":"operation roles across registered local projects; native execution is trusted"});
    Json(value)
}
async fn containers(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::docker::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::docker::list(&e.db, &q)?))
}
async fn container_cleanup(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::docker::Cleanup>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::docker::cleanup_request(&e, &r).await?))
}
async fn access_audit(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::auth::AuditQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::auth::audit_page(
        &e,
        q.before,
        q.limit.unwrap_or(20),
    )?))
}

async fn save_config(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::control::SavePipeline>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.save_pipeline(&r)?))
}
async fn get_config(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::control::ConfigRef>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.pipeline_config(&r)?))
}
async fn list_configs(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.pipeline_list(&id)?))
}
async fn run_saved(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::profiles::SavedStart>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(crate::profiles::start(&e, r)?)))
}
async fn run_history(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::control::RunQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.query_runs(&r)?))
}

async fn save_toolchain(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::profiles::SaveProfile>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::profiles::save(&e, &r)?))
}
async fn get_toolchain(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::control::ConfigRef>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::profiles::get(&e, &r)?))
}
async fn list_toolchains(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::profiles::list(&e, &id)?))
}
async fn probe_toolchain(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::profiles::Probe>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(crate::profiles::probe(&e, r)?)))
}

async fn run_inputs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inputs::InputQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.input_page(&r)?))
}
async fn run_input_diff(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inputs::CompareQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.compare_inputs(&r)?))
}

async fn publish_module(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::modules::Publish>,
) -> Result<Json<Artifact>, ApiError> {
    Ok(Json(crate::modules::publish(&e, &r)?))
}
async fn get_module(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::modules::Get>,
) -> Result<Json<Artifact>, ApiError> {
    Ok(Json(e.db.module(&r.project_id, &r.sha256)?))
}

async fn run_module_inputs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inputs::InputQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.module_inputs(&r)?))
}

async fn module_manifest(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::modules::ManifestQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.module_manifest(&r)?))
}

async fn run_git(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::sources::StartGitPipeline>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start_git_pipeline(r)?)))
}
async fn run_dirty(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::sources::StartDirtyPipeline>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start_dirty_pipeline(r)?)))
}
async fn run_multi(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::multi_source::StartMultiPipeline>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(e.start_multi_pipeline(r)?)))
}
async fn run_source_root_inputs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::multi_source::RootInputsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.source_root_inputs(&r)?))
}
async fn run_source(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.source_evidence(&r.run_id)?))
}
async fn run_source_patch(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::sources::PatchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.source_patch(&r)?))
}

async fn run_pinned(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::pins::StartPinned>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::profiles::start_pinned(&e, r)?),
    ))
}
async fn inspect_pin(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::pins::Inspect>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = e.db.project(&r.project_id)?.root;
    let config = e.config.clone();
    let result = tokio::task::spawn_blocking(move || crate::pins::inspect(&config, &root, &r))
        .await
        .map_err(anyhow::Error::from)??;
    Ok(Json(result))
}
async fn run_toolchain(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        serde_json::json!({"run_id":r.run_id,"verified":e.db.toolchain(&r.run_id)?}),
    ))
}
async fn module_contracts(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::modules::ManifestQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.module_contracts(&r)?))
}

async fn stage_key(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::stage_keys::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::stage_keys::explain(&e.db, &r)?))
}

async fn run_operations(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.operations(&r.run_id)?))
}

async fn run_cache(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.cache_decisions(&r.run_id)?))
}

async fn cache_cleanup_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::cache::CleanupPreview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.cache_cleanup_preview(&r)?))
}
async fn cache_cleanup_execute(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::cache::CleanupExecute>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.cache_cleanup_execute(&r)?))
}

async fn scheduler_status(State(e): State<Arc<Engine>>) -> Json<serde_json::Value> {
    Json(e.scheduler_status())
}

async fn completion_events(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::events::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.completion_events(&r)?))
}

async fn codex_hook_bind(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::codex_bridge::Bind>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::codex_bridge::bind(&e, &r)?))
}
async fn codex_hook_status(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::codex_bridge::status(&e, &r.run_id)?))
}
async fn codex_hook_disable(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::codex_bridge::BindingId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.disable_codex(&r)?))
}
async fn codex_hook_retry(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::codex_bridge::BindingId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.retry_codex(&r)?))
}

async fn logs_archive(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::log_archive::Request>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::log_archive::archive(e, r).await?))
}
async fn logs_storage(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::log_archive::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.logs_storage(&r)?))
}
async fn schedules_create(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::schedules::Create>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::create(&e, &r)?))
}
async fn schedules_list(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::schedules::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::list(&e, &r)?))
}
async fn schedules_get(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::schedules::Id>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::get(&e, &r)?))
}
async fn schedules_update(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::schedules::Update>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::update(&e, &r)?))
}
async fn schedules_change(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::schedules::Change>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::change(&e, &r)?))
}
async fn schedules_due(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::schedules::Due>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::run_due(&e, &r)?))
}
async fn artifact_pin_set(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::artifact_retention::Pin>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.artifact_pin_set(&r)?))
}
async fn artifact_retention(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::artifact_retention::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.artifact_retention(&r)?))
}
async fn module_retention(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::retention::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.module_retention(&r)?))
}
async fn module_pin_set(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::retention::Pin>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.module_pin_set(&r)?))
}

async fn queue_create(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::queues::Create>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::queues::create(&e, r)?))
}
async fn queue_status(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(r): Query<crate::queues::Detail>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::queues::status(&e, &id, r.details)?))
}
async fn queues_list(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::queues::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::queues::list(&e, &r)?))
}
async fn queue_cancel(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::queues::Cancel>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::queues::cancel(&e, r)?))
}

async fn runs_shared(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::single_flight::inspect(&e, &id)?))
}

async fn operations_abandoned(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::operation_resolution::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.abandoned_operations(&r)?))
}
async fn operation_resolve(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::operation_resolution::Resolve>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.resolve_operation(&r)?))
}

async fn worker_enrollment(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::workers::CreateEnrollment>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.worker_enrollment(&r)?))
}
async fn workers(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::workers::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.workers(&r)?))
}
async fn worker_revoke(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::workers::Revoke>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.worker_revoke(&r)?))
}

async fn worker_inputs(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::worker_transfers::Prepare>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::worker_transfers::prepare(e, r).await?))
}

async fn worker_jobs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<RunId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.worker_jobs(&r.run_id)?))
}

async fn worker_ssh_start(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::worker_connections::Start>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::worker_connections::start(&e, r)?))
}

async fn worker_ssh_status(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::worker_connections::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::worker_connections::status(&e, &r)?))
}

async fn worker_ssh_stop(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::worker_connections::Stop>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::worker_connections::stop(&e, r)?))
}

async fn worker_ssh_list(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::worker_connections::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::worker_connections::list(&e, &r)?))
}

async fn artifact_expiry_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::artifact_expiry::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::artifact_expiry::preview(e, r).await?))
}
async fn module_gc_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::storage_gc::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::module_gc::preview(e, r).await?))
}
async fn storage_gc_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::storage_gc::StagePreview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::storage_gc::preview(e, r).await?))
}

async fn storage_gc_execute(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::storage_gc::Execute>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::storage_gc::execute(e, r).await?))
}

async fn storage_gc_status(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::storage_gc::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::storage_gc::status(&e, &r)?))
}

async fn project_pipelines(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inspection::Pipelines>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::inspection::pipelines(&e, &r)?))
}

async fn test_results(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inspection::Tests>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::inspection::tests(&e, &r)?))
}

async fn test_source(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::inspection::Source>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::inspection::source(&e, &r)?))
}

async fn experiment_save(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::experiments::Save>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::save(&e, &r)?))
}

async fn experiment_validate(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::experiments::Save>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::validate(&e, &r)?))
}

async fn experiment_get(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::control::ConfigRef>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::get(&e, &r)?))
}

async fn experiments_list(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::control::ProjectId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::list(&e, &r)?))
}

async fn experiment_start(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::experiments::Start>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::start(&e, r)?))
}

async fn experiment_status(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::experiments::Run>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::status(&e, &r)?))
}

async fn experiment_cancel(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::experiments::Cancel>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::cancel(&e, r)?))
}

async fn experiment_compare(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::experiments::Compare>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::experiments::compare(&e, &r)?))
}

async fn log_search(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::log_search::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::log_search::search(&e, &r)?))
}

async fn storage_usage(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::storage_usage::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::storage_usage::usage(e, r).await?))
}

async fn server_autostart(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::autostart::Request>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::autostart::request(&e, r)?))
}
async fn server_settings(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::settings::Request>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::settings::request(&e, r)?))
}

async fn run_source_diff(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::source_diff::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::source_diff::compare(e, r).await?))
}

async fn schedules_preview(
    Json(r): Json<crate::schedules::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::preview(&r)?))
}
async fn schedules_history(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::schedules::History>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::schedules::history(&e, &r)?))
}

async fn storage_policy_status(
    State(e): State<Arc<Engine>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::storage_policy::status(&e)?))
}

pub fn run_definition(e: &Engine, id: &str) -> anyhow::Result<serde_json::Value> {
    let run = e.db.run(id)?;
    let native = if run.pipeline.is_none() {
        run.stages.first().map(|s| s.definition.command.clone())
    } else {
        None
    };
    Ok(json!({"run_id":run.id,"definition":run.pipeline,"native_command":native}))
}
async fn run_definition_http(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(run_definition(&e, &id)?))
}

async fn evidence_search(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::evidence::Search>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::evidence::search(&e, &r)?))
}
async fn evidence_context(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::evidence::Search>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::evidence::context(&e, &r)?))
}
async fn evidence_read(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::evidence::Read>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::evidence::read(&e, &r)?))
}
async fn evidence_stats(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::evidence::Project>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::evidence::stats(&e, &r)?))
}
async fn run_acceptance(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::acceptance::read(&e, &id)?))
}

async fn catalog_page(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::catalog::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::catalog::list(&e, &q)?))
}
async fn live_snapshot(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::live::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::live::snapshot(&e, &q)?))
}
async fn live_events(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::live::Query>,
) -> Result<Response, ApiError> {
    Ok(crate::live::subscribe(&e, q)?)
}

async fn executions_list(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::executions::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::executions::list(&e, &q)?))
}
async fn executions_summary(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::executions::Summary>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::executions::summary(&e, &q)?))
}
async fn executions_cancel(
    State(e): State<Arc<Engine>>,
    Json(q): Json<crate::executions::Cancel>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::executions::cancel(&e, &q)?))
}
async fn editor_check(
    Json(q): Json<crate::pipeline_editor::Check>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::pipeline_editor::check(&q)?))
}
async fn editor_revisions(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::pipeline_editor::Revisions>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::pipeline_editor::revisions(&e, &q)?))
}
async fn editor_restore(
    State(e): State<Arc<Engine>>,
    Json(q): Json<crate::pipeline_editor::Restore>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::pipeline_editor::restore(&e, &q)?))
}

async fn management_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::management::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::management::preview(e, r).await?))
}
async fn management_execute(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::management::Execute>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::management::execute(&e, r)?))
}
async fn management_trash(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::management::Trash>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::management::trash(&e, r)?))
}
async fn management_restore(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::management::Restore>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::management::restore(&e, r)?))
}

async fn archive_cleanup_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::archive_gc::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archive_gc::preview(e, r).await?))
}
async fn archive_pin(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::archive_gc::Pin>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archive_gc::pin(&e, r)?))
}
async fn archive_create(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::archives::Create>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archives::create(&e, r)?))
}
async fn archives_list(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::archives::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archives::list(&e, r)?))
}
async fn archive_get(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::archives::Id>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archives::get(&e, &r)?))
}
async fn archive_verify(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::archives::Unlock>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archives::verify_unlocked(e, r).await?))
}
async fn archive_restore(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::archives::Restore>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::archives::restore(e, r).await?))
}
async fn archive_import(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::archives::Import>,
    request: Request,
) -> Result<Json<serde_json::Value>, ApiError> {
    use futures_core::Stream;
    use tokio::io::AsyncWriteExt;
    e.db.project(&r.project_id)?;
    if let Some(replay) = crate::archives::import_replay(&e, &r)? {
        return Ok(Json(replay));
    }
    let permit = e
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::Error::from(Fault::Busy))?;
    let temporary = tempfile::NamedTempFile::new_in(crate::archives::inbox(&e)?)
        .map_err(anyhow::Error::from)?;
    let mut output = tokio::fs::File::from_std(temporary.reopen().map_err(anyhow::Error::from)?);
    let mut stream = request.into_body().into_data_stream();
    let mut bytes = 0u64;
    while let Some(chunk) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx)).await
    {
        let chunk = chunk.map_err(anyhow::Error::from)?;
        bytes += chunk.len() as u64;
        if bytes > crate::archives::MAX_BYTES {
            return Err(invalid("compressed archive exceeds 1 GiB").into());
        }
        output
            .write_all(&chunk)
            .await
            .map_err(anyhow::Error::from)?;
    }
    output.flush().await.map_err(anyhow::Error::from)?;
    drop(output);
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::archives::import_file(&e, r, temporary.into_file())
    })
    .await
    .map_err(anyhow::Error::from)??;
    Ok(Json(result))
}
async fn archive_download(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::archives::Id>,
) -> Result<Response, ApiError> {
    let encrypted = crate::archives::get(&e, &r)?["encryption"].is_object();
    let name = format!(
        "preflight-{}.tar.zst{}",
        r.archive_id,
        if encrypted { ".age" } else { "" }
    );
    let file = tokio::task::spawn_blocking(move || crate::archives::download_file(&e, &r))
        .await
        .map_err(anyhow::Error::from)??;
    let length = file.metadata().map_err(anyhow::Error::from)?.len();
    Ok((
        [
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_TYPE,
                if encrypted {
                    "application/octet-stream"
                } else {
                    "application/zstd"
                }
                .to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::fs::File::from_std(file),
        )),
    )
        .into_response())
}

async fn retry_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::retry::Selection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::retry::preview(&e, &r)?))
}
async fn retry_start(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::retry::Start>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(crate::retry::start(&e, r)?)))
}

async fn global_search(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::search::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::search::search(&e, &q)?))
}

async fn worker_recovery_status(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::worker_recovery::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(e.db.worker_recovery_status(&q)?))
}

async fn diagnostics_report(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::diagnostics::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        tokio::task::spawn_blocking(move || crate::diagnostics::report(&e, &r))
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

async fn run_compare(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::run_comparison::Query>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::run_comparison::compare(&e, &r)?))
}

async fn evidence_expiry_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::evidence_expiry::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::evidence_expiry::preview(e, r).await?))
}

async fn cache_eviction_preview(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::cache_eviction::Preview>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::cache_eviction::preview(e, r).await?))
}

async fn ci_sources(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::SourceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::sources(&e, &r)?))
}
async fn ci_sync(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::ci::Sync>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::sync(&e, r)?))
}
async fn ci_sync_status(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::Status>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::sync_status(&e, &r)?))
}
async fn ci_runs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::List>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::runs(&e, &r)?))
}
async fn ci_run(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::Get>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::get(&e, &r)?))
}
async fn ci_artifact_fetch(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::ci::FetchArtifact>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::artifact_fetch(&e, r)?))
}
async fn ci_artifact_file(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::ArtifactFile>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let (bytes, sha) = tokio::task::spawn_blocking(move || crate::ci::artifact_file(&e, &r))
        .await
        .map_err(anyhow::Error::from)??;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/zip"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=preflight-ci-artifact.zip",
            ),
            (axum::http::header::ETAG, &format!("\"{sha}\"")),
        ],
        bytes,
    )
        .into_response())
}

async fn ci_syncs(
    State(e): State<Arc<Engine>>,
    Query(r): Query<crate::ci::SyncList>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::syncs(&e, &r)?))
}
async fn ci_sync_cancel(
    State(e): State<Arc<Engine>>,
    Json(r): Json<crate::ci::Cancel>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::ci::cancel(&e, &r)?))
}

async fn server_release_status(
    State(e): State<Arc<Engine>>,
    Query(q): Query<crate::releases::Query>,
) -> Json<serde_json::Value> {
    Json(crate::releases::status(&e, q.details))
}
async fn server_release_check(
    State(e): State<Arc<Engine>>,
    Json(q): Json<crate::releases::Check>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(crate::releases::request(&e, q).await?))
}
