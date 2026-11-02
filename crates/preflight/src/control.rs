use crate::{engine::Engine, model::*, pipeline::PipelineFile};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 73;
pub const PROTOCOL_PATCH: u32 = 0;
pub const PROTOCOL_VERSION: &str = "1.73.0";
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectId {
    pub project_id: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigRef {
    pub project_id: String,
    pub name: String,
    pub revision: Option<u64>,
}
pub type SavePipeline = SaveDefinition<PipelineFile>;
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct RunQuery {
    pub pipeline_key: Option<String>,
    pub experiment_name: Option<String>,
    #[serde(default)]
    pub include_experiments: bool,
    pub project_id: Option<String>,
    pub status: Option<String>,
    pub before: Option<String>,
    pub limit: Option<usize>,
}
pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        invalid("pipeline name must be 1..64 ASCII letters/digits/_/-")
    );
    Ok(())
}
pub fn capabilities(e: &Engine) -> Value {
    let config = e.current_config();
    let mut features = json!({"dag":true,"durable_completion_events":true,"completion_resource_subscriptions":true,"weighted_stage_slots":true,"os_resource_isolation":false,"fixtures":true,"junit":true,"remote_workers":false,"toolchain_profiles":true,"toolchain_binary_pinning":true,"sdk_tree_pinning":true,"toolchain_identity_format":crate::pins::FORMAT,"git_snapshots":true,"git_source_modes":["exact_local_commit","checked_working_tree","multi_repository"],"git_dirty_snapshots":true,"multi_repository_sources":true,"multi_source_format":crate::multi_source::FORMAT,"ordered_text_patches":true,"git_mr_sources":false,"git_source_format":crate::sources::SOURCE_FORMAT,"prepared_file_modules":true,"module_directory_trees":true,"module_tree_format":crate::trees::FORMAT,"module_compatibility_verification":true,"module_compatibility_policy":"exact pinned profile or container contract; legacy requires legacy publication","input_manifests":true,"input_comparison":true,"stage_key_evidence":true,"stage_key_format":crate::stage_keys::FORMAT,"operation_leases":true,"operation_lease_policy":"local exclusive owner; no expiry","cache":true,"cache_format":crate::cache::FORMAT,"cache_policy":"explicit deterministic complete-input pinned build stages only","scoped_cache_cleanup":true,"cache_cleanup_modes":["invalidate_entries"],"physical_cache_deletion":true,"idempotent_start":true,"idempotent_operations":["pipeline_run_saved","toolchain_probe","pipeline_start_pinned"]});
    features["cache_key_explanations"] = true.into();
    features["stage_retry"] = true.into();
    features["fixture_case_retry"] = true.into();
    features["configured_test_retry"] = true.into();
    features["log_search"] = true.into();
    features["experiments"] = true.into();
    features["individual_junit_cases"] = true.into();
    features["test_result_filters"] = true.into();
    features["verified_test_sources"] = true.into();
    features["project_pipeline_history"] = true.into();
    features["stage_workspace_gc"] = true.into();
    features["configured_stage_ttl"] = true.into();
    features["configured_artifact_ttl"] = true.into();
    features["configured_storage_quotas"] = true.into();
    features["configured_disk_watermarks"] = true.into();
    features["external_ci_ingestion"] = true.into();
    features["ci_providers"] = json!(["github", "gitlab"]);
    features["ci_provider_mutations"] = false.into();
    features["evidence_expiry"] = true.into();
    features["ranked_cache_eviction"] = true.into();
    features["disk_admission"] = true.into();
    features["storage_size_admission"] = true.into();
    features["diagnostic_reports"] = true.into();
    features["incremental_diagnostics"] = true.into();
    features["evidence_filters"] = true.into();
    features["evidence_byte_windows"] = true.into();
    features["run_comparison"] = true.into();
    features["verified_in_place_tools"] = true.into();
    features["stage_workspace_retention_cutoff"] = true.into();
    features["bounded_storage_usage"] = true.into();
    features["admin_server_settings"] = true.into();
    features["verified_source_diffs"] = true.into();
    features["module_copy_gc"] = true.into();
    features["explicit_artifact_expiry"] = true.into();
    features["artifact_retention_pins"] = true.into();
    features["compressed_log_chunks"] = true.into();
    features["one_shot_schedules"] = true.into();
    features["interval_schedules"] = true.into();
    features["calendar_schedules"] = true.into();
    features["schedule_editing"] = true.into();
    features["catalog_pagination"] = true.into();
    features["run_archives"] = true.into();
    features["archive_import"] = true.into();
    features["archive_encryption"] = true.into();
    features["archive_identity_refs"] =
        serde_json::to_value(crate::archive_crypto::identity_references()).unwrap_or_default();
    features["archive_retention"] = true.into();
    features["archive_pins"] = true.into();
    features["archive_format"] = crate::archives::FORMAT.into();
    features["scoped_management"] = true.into();
    features["recoverable_deletion"] = true.into();
    features["global_executions"] = true.into();
    features["experiment_schedules"] = true.into();
    features["pipeline_editor"] = true.into();
    features["client_sse"] = true.into();
    features["shared_sse_hints"] = true.into();
    features["bundled_web_ui"] = true.into();
    features["browser_sessions"] = true.into();
    features["evidence_retrieval"] = true.into();
    features["acceptance_criteria"] = true.into();
    features["normalized_test_reports"] = true.into();
    features["managed_worker_ssh"] = cfg!(unix).into();
    features["worker_ssh_reconnect"] = cfg!(unix).into();
    features["global_search"] = true.into();
    features["worker_capability_negotiation"] = true.into();
    features["remote_native_stages"] = true.into();
    features["worker_output_transfer"] = true.into();
    features["worker_job_protocol"] = crate::worker_jobs::PROTOCOL.into();
    features["worker_package_cli_protocol"] = crate::worker_package::CLI_PROTOCOL.into();
    features["worker_package_format"] = crate::worker_package::FORMAT.into();
    features["worker_ssh_session_lifetime"] = true.into();
    features["worker_source_transfers"] = true.into();
    features["worker_registration"] = true.into();
    features["worker_gateway_configured"] = config.worker_bind.is_some().into();
    features["worker_protocol"] = crate::workers::PROTOCOL.into();
    features["abandoned_operation_resolution"] = true.into();
    features["single_flight"] = true.into();
    features["operation_scoped_access"] = true.into();
    features["worker_recovery_terminal_ledger"] = true.into();
    features["release_notifications"] = true.into();
    features["worker_runtime_receipts"] = true.into();
    features["worker_runtime_receipt_format"] = crate::worker_receipts::FORMAT.into();
    features["remote_live_logs"] = true.into();
    features["remote_container_stages"] = true.into();
    features["worker_container_protocol"] = crate::worker_jobs::CONTAINER_PROTOCOL.into();
    features["stage_setup"] = true.into();
    features["prepared_container_images"] = true.into();
    features["docker_executor"] = true.into();
    features["container_module_contracts"] = true.into();
    features["docker_configured"] = config.docker_command.is_some().into();
    features["container_cleanup_admission"] = true.into();
    features["change_queues"] = true.into();
    features["memory_admission"] = true.into();
    features["worker_journal_recovery"] = true.into();
    features["worker_recovery_readiness"] = true.into();
    features["macos_login_startup"] = cfg!(target_os = "macos").into();
    features["settings_hot_reload"] = true.into();
    features["memory_admission_configured"] = config.memory_budget_mib.is_some().into();
    features["module_retention"] = true.into();
    features["module_pins"] = true.into();
    features["codex_completion_bridge"] = true.into();
    features["codex_delivery_modes"] = true.into();
    features["codex_delivery_policy"] = crate::codex_bridge::policy(&config);
    features["codex_delivery_configured"] = config.codex_command.is_some().into();
    json!({"product":"preflight","version":env!("CARGO_PKG_VERSION"),"schema_version":crate::migrations::VERSION,"service_language":"en","data_language":"original","preflight_protocol":{"version":PROTOCOL_VERSION,"major":PROTOCOL_MAJOR,"minor":PROTOCOL_MINOR,"patch":PROTOCOL_PATCH,"compatibility":"same major, server minor >= required minor; additive fields must be ignored","transport_version":"negotiated independently by MCP initialize"},"migration_system":{"version":1,"checksums":"sha256","policy":"forward-only, transactional, contiguous; legacy adoption marked"},"transport":"streamable_http","execution":{"mode":"local_native","executors":["native","docker"],"docker_enabled":config.docker_command.is_some(),"container_scope":"local daemon and private stage-root mount; no native fallback","os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"max_parallel_commands":config.max_parallel_commands},"configuration":{"saved_pipelines":true,"immutable_revisions":true,"optimistic_writes":true,"project_roots":"operator_allowlist","export":"pipeline_get returns definition JSON"},"features":features,"limits":{"sse_connections":crate::live::MAX_STREAMS,"archive_file_bytes":1073741824,"archive_entries":50000,"archive_metadata_bytes":16777216,"archive_creators":1,"definition_bytes":65536,"stages":32,"active_runs":config.max_active_runs,"run_page":100},"schema_uri":"/api/v1/pipeline-schema"})
}
pub fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(PipelineFile)).expect("JSON schema serializes")
}
pub fn start_saved(e: &Arc<Engine>, r: ConfigRef) -> Result<Run> {
    crate::profiles::start(
        e,
        crate::profiles::SavedStart {
            project_id: r.project_id,
            name: r.name,
            revision: r.revision,
            profile: None,
            idempotency_key: None,
        },
    )
}

// Config paths deliberately allow the project .preflight directory; execution
// paths still reserve it for verified inputs and isolated case workspaces.
pub fn config_path(root: &std::path::Path, relative: &str) -> Result<std::path::PathBuf> {
    use std::path::Component;
    ensure!(
        root.canonicalize()? == root,
        invalid("project root moved or became a symlink")
    );
    let mut path = root.to_path_buf();
    for part in std::path::Path::new(relative).components() {
        match part {
            Component::Normal(v) => path.push(v),
            _ => return Err(invalid("invalid config path")),
        }
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            ensure!(
                !m.file_type().is_symlink(),
                invalid("config path refuses symlinks")
            );
        }
    }
    Ok(path)
}
fn ignore(root: &std::path::Path) -> Result<()> {
    use std::io::Write;
    let p = config_path(root, ".gitignore")?;
    let old = if p.exists() {
        crate::fixtures::read(&p, 1024 * 1024)?
    } else {
        vec![]
    };
    let text = std::str::from_utf8(&old).map_err(|_| invalid("gitignore is not UTF-8"))?;
    if !text
        .lines()
        .any(|l| [".preflight", ".preflight/", "/.preflight", "/.preflight/"].contains(&l.trim()))
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)?;
        if !old.is_empty() && !old.ends_with(b"\n") {
            f.write_all(b"\n")?;
        }
        f.write_all(b"/.preflight/\n")?;
        f.sync_all()?;
    }
    Ok(())
}
pub(crate) fn latest(root: &std::path::Path, kind: &str, name: &str) -> Result<u64> {
    validate_name(name)?;
    let path = config_path(root, &format!(".preflight/{kind}/{name}"))?;
    if !path.exists() {
        return Ok(0);
    }
    let mut revision = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if let Some(n) = file
            .strip_suffix(".json")
            .and_then(|v| v.parse::<u64>().ok())
        {
            revision = revision.max(n);
        }
    }
    Ok(revision)
}
pub fn get(root: &std::path::Path, r: &ConfigRef) -> Result<Value> {
    get_document::<PipelineFile>(root, "pipelines", r, crate::pipeline::validate)
}
pub fn get_document<T: serde::de::DeserializeOwned + Serialize>(
    root: &std::path::Path,
    kind: &str,
    r: &ConfigRef,
    validate: fn(&T) -> Result<()>,
) -> Result<Value> {
    let rev = r.revision.unwrap_or(latest(root, kind, &r.name)?);
    ensure!(rev > 0, Fault::NotFound);
    let p = config_path(root, &format!(".preflight/{kind}/{}/{rev}.json", r.name))?;
    let bytes = crate::fixtures::read(&p, 128 * 1024).map_err(|e| {
        if !p.exists() {
            anyhow::Error::from(Fault::NotFound)
        } else {
            e
        }
    })?;
    let v: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        v["revision"].as_u64() == Some(rev) && v["name"] == r.name,
        invalid("config identity mismatch")
    );
    let definition: T = serde_json::from_value(v["definition"].clone())?;
    validate(&definition)?;
    use sha2::{Digest, Sha256};
    let hash = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&definition)?));
    ensure!(
        v["sha256"] == hash,
        invalid("saved revision was modified; restore it before updating")
    );
    Ok(v)
}
pub fn save(root: &std::path::Path, r: &SavePipeline) -> Result<Value> {
    save_document(root, "pipelines", r, crate::pipeline::validate)
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDefinition<T> {
    pub project_id: String,
    pub name: String,
    pub expected_revision: u64,
    pub definition: T,
}
pub fn save_document<T: serde::de::DeserializeOwned + Serialize>(
    root: &std::path::Path,
    kind: &str,
    r: &SaveDefinition<T>,
    validate: fn(&T) -> Result<()>,
) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    validate_name(&r.name)?;
    validate(&r.definition)?;
    ensure!(r.expected_revision < u64::MAX, invalid("revision overflow"));
    ensure!(
        latest(root, kind, &r.name)? == r.expected_revision,
        Fault::Conflict
    );
    if r.expected_revision > 0 {
        get_document::<T>(
            root,
            kind,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: r.name.clone(),
                revision: Some(r.expected_revision),
            },
            validate,
        )?;
    }
    ignore(root)?;
    let revision = r.expected_revision + 1;
    let hash = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&r.definition)?));
    let value = json!({"project_id":r.project_id,"name":r.name,"revision":revision,"sha256":hash,"definition":r.definition});
    let path = config_path(
        root,
        &format!(".preflight/{kind}/{}/{revision}.json", r.name),
    )?;
    let parent = path.parent().ok_or_else(|| invalid("config parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec_pretty(&value)?)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&path).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::Error::from(Fault::Conflict)
        } else {
            e.error.into()
        }
    })?;
    Ok(json!({"project_id":r.project_id,"name":r.name,"revision":revision,"sha256":hash}))
}
pub fn list(root: &std::path::Path, id: &str) -> Result<Value> {
    list_documents(root, "pipelines", id)
}
pub fn list_documents(root: &std::path::Path, kind: &str, id: &str) -> Result<Value> {
    let path = config_path(root, &format!(".preflight/{kind}"))?;
    let mut rows = vec![];
    if path.exists() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if validate_name(&name).is_ok() {
                let rev = latest(root, kind, &name)?;
                if rev > 0 {
                    rows.push(json!({"name":name,"revision":rev}));
                }
            }
        }
    }
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let truncated = rows.len() > 100;
    rows.truncate(100);
    Ok(json!({"project_id":id,kind:rows,"truncated":truncated}))
}
