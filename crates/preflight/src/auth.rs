//! Optional operation-scoped credentials. Native execution is delegated only to trusted runners.
use crate::engine::Engine;
use anyhow::{Result, ensure};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    ReadOnly,
    Execute,
    Admin,
}
impl Role {
    fn allows(self, needed: Self) -> bool {
        self >= needed
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub name: String,
    pub token_env: String,
    pub role: Role,
}
pub fn validate(tokens: &[Token]) -> Result<()> {
    ensure!(
        tokens.len() <= 32,
        "at most 32 API credentials are supported"
    );
    let mut names = BTreeSet::new();
    let mut variables = BTreeSet::new();
    for token in tokens {
        ensure!(
            !token.name.is_empty()
                && token.name.len() <= 64
                && token
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                && names.insert(&token.name),
            "API credential names must be unique ASCII names"
        );
        ensure!(
            !token.token_env.is_empty()
                && token.token_env.len() <= 128
                && token
                    .token_env
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                && !token.token_env.as_bytes()[0].is_ascii_digit()
                && variables.insert(&token.token_env),
            "API credential environment names must be unique uppercase identifiers"
        );
    }
    Ok(())
}
fn valid_secret(value: &str) -> bool {
    (32..=512).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._~+/=".contains(&c))
}
pub fn validate_environment(tokens: &[Token]) -> Result<()> {
    let mut digests = BTreeSet::new();
    for token in tokens {
        let value = std::env::var(&token.token_env).unwrap_or_default();
        ensure!(
            valid_secret(&value),
            "API credential environment variable {} must contain 32..512 non-whitespace ASCII characters",
            token.token_env
        );
        ensure!(
            digests.insert(crate::hashing::hex(Sha256::digest(value.as_bytes()))),
            "API credentials must not share the same value"
        );
    }
    Ok(())
}
#[derive(Clone)]
pub struct Principal {
    pub name: String,
    pub role: Role,
    digest: String,
}
fn authenticate(
    tokens: &[Token],
    supplied: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> Option<Principal> {
    if !valid_secret(supplied) {
        return None;
    }
    let candidate = Sha256::digest(supplied.as_bytes());
    for token in tokens {
        let Some(expected) = resolve(&token.token_env).filter(|v| valid_secret(v)) else {
            continue;
        };
        let expected = Sha256::digest(expected.as_bytes());
        if candidate
            .iter()
            .zip(expected.iter())
            .fold(0u8, |n, (a, b)| n | (a ^ b))
            == 0
        {
            return Some(Principal {
                name: token.name.clone(),
                role: token.role,
                digest: crate::hashing::hex(candidate),
            });
        }
    }
    None
}
#[derive(Default)]
pub struct Sessions(Mutex<BTreeMap<String, (String, Instant)>>);
impl Sessions {
    fn owner(&self, id: &str, digest: &str) -> bool {
        let Ok(mut sessions) = self.0.lock() else {
            return false;
        };
        let Some((owner, seen)) = sessions.get_mut(id) else {
            return false;
        };
        let browser = id.starts_with("web:");
        if owner != digest
            || seen.elapsed() > Duration::from_secs(if browser { 3600 } else { 86400 })
        {
            return false;
        }
        if !browser {
            *seen = Instant::now();
        }
        true
    }
    fn bind(&self, id: String, digest: String) {
        if let Ok(mut sessions) = self.0.lock() {
            sessions.retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(86400));
            if sessions.len() >= 4096
                && let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, (_, seen))| *seen)
                    .map(|(id, _)| id.clone())
            {
                sessions.remove(&oldest);
            }
            sessions.insert(id, (digest, Instant::now()));
        }
    }
    fn remove(&self, id: &str) {
        if let Ok(mut sessions) = self.0.lock() {
            sessions.remove(id);
        }
    }
}
pub fn tool_role(name: &str) -> Role {
    // Unknown/new operations require administration until explicitly classified.
    if READ_TOOLS.split_whitespace().any(|v| v == name) {
        Role::ReadOnly
    } else if EXECUTE_TOOLS.split_whitespace().any(|v| v == name) {
        Role::Execute
    } else {
        Role::Admin
    }
}
const READ_TOOLS:&str="
server_release_status
archives_cleanup_preview containers_list archives_get archives_list archives_verify management_preview management_trash storage_policy_status
codex_hook_status events_read queue_status queues_list scheduler_status runs_worker_jobs workers_list
worker_recovery_status search_global worker_ssh_status worker_ssh_list operations_abandoned runs_source_diff
storage_usage storage_gc_preview evidence_expiry_preview cache_eviction_preview ci_sources_list ci_sync_status
ci_runs_list ci_run_get ci_syncs_list module_gc_preview artifact_expiry_preview storage_gc_status cache_cleanup_preview
module_retention runs_shared runs_cache server_capabilities runs_inputs runs_compare_inputs modules_get
runs_module_inputs modules_manifest runs_source_root_inputs runs_source runs_source_patch pipeline_schema projects_get
pipeline_list pipeline_get pipeline_retry_preview runs_list toolchain_get toolchain_list toolchain_schema
runs_operations runs_stage_key runs_toolchain modules_contracts projects_list pipeline_validate fixtures_list
fixtures_validate fixtures_results fixtures_diff batch_status batch_report batch_compare experiment_validate
experiment_get experiments_list experiment_status runs_compare experiment_compare diagnostics_report evidence_search
evidence_context evidence_read executions_list executions_summary pipeline_check pipeline_revisions catalog_list
evidence_stats runs_acceptance logs_search tests_results tests_source project_pipelines stages_get artifacts_list
schedules_preview schedules_history schedules_list schedules_get logs_storage artifact_retention build_status
runs_definition build_report logs_read";
const EXECUTE_TOOLS:&str="
queue_create queue_cancel ci_sync ci_artifact_fetch ci_sync_cancel modules_publish pipeline_start_git
pipeline_start_dirty pipeline_start_multi pipeline_save pipeline_retry pipeline_run_saved toolchain_save
toolchain_probe toolchain_inspect_pin pipeline_start_pinned projects_register build_start pipeline_start
batch_start fixtures_run batch_cancel experiment_save experiment_start experiment_cancel executions_cancel
pipeline_restore schedules_create schedules_update schedules_change schedules_run_due pipeline_cancel build_cancel
archives_create logs_archive";
fn http_role(path: &str, method: &str) -> Role {
    if path == "/api/v1/browser-session" {
        return Role::ReadOnly;
    }
    if matches!(
        path,
        "/api/v1/server-settings" | "/api/v1/server-autostart" | "/api/v1/access-audit"
    ) {
        return Role::Admin;
    }
    if matches!(method, "GET" | "HEAD") {
        return Role::ReadOnly;
    }
    if READ_PATHS.split_whitespace().any(|v| v == path) {
        return Role::ReadOnly;
    }
    if EXECUTE_PATHS.split_whitespace().any(|v| v == path)
        || path.starts_with("/api/v1/runs/") && path.ends_with("/cancel")
        || path.starts_with("/api/v1/batches/") && path.ends_with("/cancel")
    {
        return Role::Execute;
    }
    Role::Admin
}
const READ_PATHS:&str="/api/v1/archive/cleanup/preview /api/v1/storage/evidence/preview /api/v1/storage/cache/preview /api/v1/pipeline-retry/preview
/api/v1/archive/verify /api/v1/management/preview /api/v1/pipeline-editor/check /api/v1/schedules/preview
/api/v1/storage-gc-preview /api/v1/module-gc-preview /api/v1/artifact-expiry-preview /api/v1/cache-cleanup-preview
/api/v1/experiments/validate /api/v1/fixtures/validate /api/v1/pipelines/validate";
const EXECUTE_PATHS:&str="/api/v1/ci/cancel /api/v1/ci/sync /api/v1/ci/artifact-fetch /api/v1/projects
/api/v1/pipeline-runs /api/v1/pipeline-retry /api/v1/archives /api/v1/executions/cancel
/api/v1/pipeline-editor/restore /api/v1/pipeline-configs /api/v1/saved-pipeline-runs /api/v1/modules
/api/v1/pinned-pipeline-runs /api/v1/toolchain-inspect-pin /api/v1/change-queues /api/v1/change-queues/cancel
/api/v1/schedules /api/v1/schedules/change /api/v1/schedules/update /api/v1/schedules/run-due
/api/v1/logs-archive /api/v1/git-pipeline-runs /api/v1/dirty-pipeline-runs /api/v1/multi-pipeline-runs
/api/v1/experiments /api/v1/experiment-runs /api/v1/experiments/cancel /api/v1/toolchains /api/v1/toolchain-probes
/api/v1/batches /api/v1/runs";

fn denied(status: StatusCode, code: &str, message: &str) -> Response {
    let mut response = (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer realm=\"Preflight\""),
        );
    }
    response
}
fn audit(e: &Engine, principal: Option<&Principal>, operation: &str, outcome: &str) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    tx.execute("INSERT INTO access_audit(principal,role,operation,outcome,created_at) VALUES (?1,?2,?3,?4,?5)",rusqlite::params![principal.map(|p|p.name.as_str()).unwrap_or("unauthenticated"),principal.map(|p|serde_json::to_string(&p.role)).transpose()?,e.db.redact(operation).chars().take(200).collect::<String>(),outcome,crate::db::now()])?;
    tx.execute(
        "DELETE FROM access_audit WHERE id <= (SELECT max(id)-10000 FROM access_audit)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}
fn browser_cookie(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == "preflight_session"
                && value.len() == 64
                && value.bytes().all(|v| v.is_ascii_hexdigit()))
            .then_some(value)
        })
}

pub async fn browser_login(
    State(e): State<Arc<Engine>>,
    principal: Option<axum::Extension<Principal>>,
) -> Response {
    let Some(axum::Extension(principal)) = principal else {
        return Json(json!({"authenticated": true, "role": "local_owner"})).into_response();
    };
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return denied(
            StatusCode::SERVICE_UNAVAILABLE,
            "entropy_unavailable",
            "Cannot create a browser session",
        );
    }
    let id: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
    e.auth_sessions.bind(format!("web:{id}"), principal.digest);
    (
        [
            (
                header::SET_COOKIE,
                format!(
                    "preflight_session={id}; HttpOnly; SameSite=Strict; Path=/api/v1/; Max-Age=3600"
                ),
            ),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        Json(json!({"authenticated": true, "role": principal.role})),
    )
        .into_response()
}

pub async fn browser_logout(State(e): State<Arc<Engine>>, request: Request) -> Response {
    if let Some(id) = browser_cookie(request.headers()) {
        e.auth_sessions.remove(&format!("web:{id}"));
    }
    (
        [
            (
                header::SET_COOKIE,
                "preflight_session=; HttpOnly; SameSite=Strict; Path=/api/v1/; Max-Age=0",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({"authenticated": false})),
    )
        .into_response()
}

pub async fn guard(State(e): State<Arc<Engine>>, mut request: Request, next: Next) -> Response {
    let tokens = e.current_config().access_tokens;
    if tokens.is_empty()
        || matches!(request.uri().path(), "/health" | "/ready" | "/connection")
        || matches!(request.method().as_str(), "GET" | "HEAD")
            && preflight_client::is_asset(request.uri().path())
    {
        return crate::secrets::response(next.run(request).await, e.db.secret_values());
    }
    let credential = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(principal) = credential
        .and_then(|v| authenticate(&tokens, v, |name| std::env::var(name).ok()))
        .or_else(|| {
            if credential.is_some() || !request.uri().path().starts_with("/api/v1/") {
                return None;
            }
            let id = browser_cookie(request.headers())?;
            tokens.iter().find_map(|token| {
                let secret = std::env::var(&token.token_env).ok()?;
                let principal = authenticate(std::slice::from_ref(token), &secret, |_| {
                    Some(secret.clone())
                })?;
                e.auth_sessions
                    .owner(&format!("web:{id}"), &principal.digest)
                    .then_some(principal)
            })
        })
    else {
        let _ = audit(&e, None, request.uri().path(), "denied");
        return denied(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "A valid Preflight bearer credential is required",
        );
    };
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let session = request
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mcp = path == "/mcp";
    if mcp
        && session
            .as_ref()
            .is_some_and(|id| !e.auth_sessions.owner(id, &principal.digest))
    {
        return denied(
            StatusCode::UNAUTHORIZED,
            "session_owner_mismatch",
            "Start a fresh MCP session with this credential",
        );
    }
    let mut needed = http_role(&path, &method);
    let mut operation = path.clone();
    if mcp {
        needed = Role::ReadOnly;
        if method == "POST" {
            let (parts, body) = request.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, 512 * 1024).await else {
                return denied(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "body_too_large",
                    "MCP request exceeds 512 KiB",
                );
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                return denied(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Invalid MCP JSON request",
                );
            };
            let rpc = value["method"].as_str().unwrap_or("");
            if rpc == "tools/call" {
                let name = value["params"]["name"].as_str().unwrap_or("");
                needed = tool_role(name);
                operation = format!("mcp:{name}");
            } else if !matches!(
                rpc,
                "initialize"
                    | "ping"
                    | "notifications/initialized"
                    | "notifications/cancelled"
                    | "tools/list"
                    | "resources/list"
                    | "resources/templates/list"
                    | "resources/read"
                    | "resources/subscribe"
                    | "resources/unsubscribe"
            ) {
                needed = Role::Admin;
            }
            request = Request::from_parts(parts, Body::from(bytes));
        }
    }
    if !principal.role.allows(needed) {
        let _ = audit(&e, Some(&principal), &operation, "denied");
        return denied(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "This operation requires a higher Preflight access role",
        );
    }
    if needed > Role::ReadOnly && audit(&e, Some(&principal), &operation, "authorized").is_err() {
        return denied(
            StatusCode::SERVICE_UNAVAILABLE,
            "audit_unavailable",
            "Cannot authorize a mutation while its audit record is unavailable",
        );
    }
    request.extensions_mut().insert(principal.clone());
    let mut response = next.run(request).await;
    if mcp {
        if method == "DELETE" {
            if let Some(id) = session {
                e.auth_sessions.remove(&id);
            }
        } else if let Some(id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            e.auth_sessions.bind(id.into(), principal.digest);
        }
    }
    response.headers_mut().insert(
        "x-preflight-role",
        serde_json::to_value(principal.role)
            .ok()
            .and_then(|v| v.as_str().and_then(|v| v.parse().ok()))
            .unwrap_or(header::HeaderValue::from_static("unknown")),
    );
    crate::secrets::response(response, e.db.secret_values())
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    pub before: Option<i64>,
    pub limit: Option<usize>,
}
pub fn audit_page(e: &Engine, before: Option<i64>, limit: usize) -> Result<Value> {
    ensure!(
        (1..=100).contains(&limit),
        crate::model::invalid("limit must be 1..100")
    );
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT id,principal,role,operation,outcome,created_at FROM access_audit WHERE (?1 IS NULL OR id<?1) ORDER BY id DESC LIMIT ?2")?.query_map(rusqlite::params![before,limit+1],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"principal":r.get::<_,String>(1)?,"role":r.get::<_,Option<String>>(2)?.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"operation":r.get::<_,String>(3)?,"outcome":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"events":rows.iter().take(limit).collect::<Vec<_>>(),"next_cursor":(rows.len()>limit).then(||rows[limit-1]["id"].clone())}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roles_fail_closed_for_mutations_and_new_tools() {
        assert_eq!(tool_role("logs_read"), Role::ReadOnly);
        assert_eq!(tool_role("build_start"), Role::Execute);
        for name in [
            "server_settings",
            "server_autostart",
            "storage_gc_execute",
            "future_operation",
            "archives_import",
            "archives_inbox",
        ] {
            assert_eq!(tool_role(name), Role::Admin);
        }
        assert_eq!(http_role("/api/v1/archive/import", "POST"), Role::Admin);
        assert!(!Role::ReadOnly.allows(tool_role("pipeline_start")));
        assert!(!Role::Execute.allows(tool_role("management_execute")));
    }
    #[test]
    fn credentials_never_fall_back_to_legacy_local_trust() {
        let tokens = vec![Token {
            name: "reader".into(),
            token_env: "PF_READ".into(),
            role: Role::ReadOnly,
        }];
        assert!(authenticate(&tokens, "bad", |_| Some("a".repeat(32))).is_none());
        assert!(authenticate(&tokens, &"b".repeat(32), |_| Some("a".repeat(32))).is_none());
        assert!(authenticate(&tokens, &"a".repeat(32), |_| None).is_none());
        assert_eq!(
            authenticate(&tokens, &"a".repeat(32), |_| Some("a".repeat(32)))
                .unwrap()
                .role,
            Role::ReadOnly
        );
    }
    #[test]
    fn sessions_belong_to_the_credential_not_just_a_role() {
        let sessions = Sessions::default();
        sessions.bind("session".into(), "owner".into());
        assert!(sessions.owner("session", "owner"));
        assert!(!sessions.owner("session", "other"));
        sessions.remove("session");
        assert!(!sessions.owner("session", "owner"));
    }
    #[test]
    fn browser_sessions_expire_without_sliding_or_sharing_mcp_ownership() {
        let sessions = Sessions::default();
        sessions.bind("web:cookie".into(), "owner".into());
        let created = sessions.0.lock().unwrap()["web:cookie"].1;
        assert!(sessions.owner("web:cookie", "owner"));
        assert_eq!(sessions.0.lock().unwrap()["web:cookie"].1, created);
        assert!(!sessions.owner("cookie", "owner"));
        sessions.0.lock().unwrap().get_mut("web:cookie").unwrap().1 =
            Instant::now() - Duration::from_secs(3601);
        assert!(!sessions.owner("web:cookie", "owner"));
    }
}
