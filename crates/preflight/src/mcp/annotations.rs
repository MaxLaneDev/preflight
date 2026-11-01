//! Discovery hints describe maximum supported effects, not authorization.
use super::{Mcp, ToolRouter};

fn effects(name: &str) -> Option<(bool, bool, bool)> {
    let (read_only, destructive) = match name {
        "access_audit"
        | "containers_list"
        | "archives_get"
        | "archives_list"
        | "archives_verify"
        | "management_trash"
        | "storage_policy_status"
        | "codex_hook_status"
        | "events_read"
        | "queue_status"
        | "queues_list"
        | "scheduler_status"
        | "runs_worker_jobs"
        | "workers_list"
        | "worker_recovery_status"
        | "search_global"
        | "worker_ssh_status"
        | "worker_ssh_list"
        | "operations_abandoned"
        | "server_release_status"
        | "runs_source_diff"
        | "storage_usage"
        | "ci_sources_list"
        | "ci_sync_status"
        | "ci_runs_list"
        | "ci_run_get"
        | "ci_syncs_list"
        | "storage_gc_status"
        | "module_retention"
        | "runs_shared"
        | "runs_cache"
        | "server_capabilities"
        | "runs_inputs"
        | "runs_compare_inputs"
        | "modules_get"
        | "runs_module_inputs"
        | "modules_manifest"
        | "runs_source_root_inputs"
        | "runs_source"
        | "runs_source_patch"
        | "pipeline_schema"
        | "projects_get"
        | "pipeline_list"
        | "pipeline_get"
        | "pipeline_retry_preview"
        | "runs_list"
        | "toolchain_get"
        | "toolchain_list"
        | "toolchain_schema"
        | "toolchain_inspect_pin"
        | "runs_operations"
        | "runs_stage_key"
        | "runs_toolchain"
        | "modules_contracts"
        | "projects_list"
        | "pipeline_validate"
        | "fixtures_list"
        | "fixtures_validate"
        | "fixtures_results"
        | "fixtures_diff"
        | "batch_status"
        | "batch_report"
        | "batch_compare"
        | "experiment_validate"
        | "experiment_get"
        | "experiments_list"
        | "experiment_status"
        | "runs_compare"
        | "experiment_compare"
        | "diagnostics_report"
        | "evidence_search"
        | "evidence_context"
        | "evidence_read"
        | "executions_list"
        | "executions_summary"
        | "pipeline_check"
        | "pipeline_revisions"
        | "catalog_list"
        | "evidence_stats"
        | "runs_acceptance"
        | "logs_search"
        | "tests_results"
        | "tests_source"
        | "project_pipelines"
        | "stages_get"
        | "artifacts_list"
        | "schedules_preview"
        | "schedules_history"
        | "schedules_list"
        | "schedules_get"
        | "logs_storage"
        | "artifact_retention"
        | "build_status"
        | "runs_definition"
        | "build_report"
        | "logs_read" => (true, false),

        // Previews persist an execution plan; inbox/restore create isolated files.
        "archives_create"
        | "archives_inbox"
        | "archives_import"
        | "archives_cleanup_preview"
        | "archives_restore"
        | "management_preview"
        | "worker_inputs_prepare"
        | "worker_enrollment_create"
        | "server_release_check"
        | "storage_gc_preview"
        | "evidence_expiry_preview"
        | "cache_eviction_preview"
        | "ci_artifact_fetch"
        | "module_gc_preview"
        | "artifact_expiry_preview"
        | "cache_cleanup_preview"
        | "modules_publish"
        | "ci_sync"
        | "codex_hook_bind"
        | "codex_hook_retry" => (false, false),

        // Mixed read/write tools use the write classification for their whole schema.
        "containers_cleanup"
        | "archives_pin"
        | "management_execute"
        | "management_restore"
        | "codex_hook_disable"
        | "queue_create"
        | "queue_cancel"
        | "worker_revoke"
        | "worker_ssh_start"
        | "worker_ssh_stop"
        | "operation_resolve"
        | "server_settings"
        | "server_autostart"
        | "ci_sync_cancel"
        | "storage_gc_execute"
        | "cache_cleanup_execute"
        | "module_pin_set"
        | "pipeline_start_git"
        | "pipeline_start_dirty"
        | "pipeline_start_multi"
        | "pipeline_save"
        | "pipeline_retry"
        | "pipeline_run_saved"
        | "toolchain_save"
        | "toolchain_probe"
        | "pipeline_start_pinned"
        | "projects_register"
        | "build_start"
        | "pipeline_start"
        | "batch_start"
        | "fixtures_run"
        | "batch_cancel"
        | "experiment_save"
        | "experiment_start"
        | "experiment_cancel"
        | "executions_cancel"
        | "pipeline_restore"
        | "schedules_create"
        | "schedules_update"
        | "schedules_change"
        | "schedules_run_due"
        | "logs_archive"
        | "artifact_pin_set"
        | "pipeline_cancel"
        | "build_cancel" => (false, true),
        _ => return None,
    };
    let open_world = matches!(
        name,
        "build_start"
            | "pipeline_start"
            | "pipeline_start_git"
            | "pipeline_start_dirty"
            | "pipeline_start_multi"
            | "pipeline_start_pinned"
            | "pipeline_run_saved"
            | "pipeline_retry"
            | "batch_start"
            | "fixtures_run"
            | "experiment_start"
            | "queue_create"
            | "toolchain_probe"
            | "worker_ssh_start"
            | "worker_ssh_stop"
            | "codex_hook_bind"
            | "codex_hook_retry"
            | "ci_sync"
            | "ci_artifact_fetch"
            | "server_release_check"
            | "schedules_create"
            | "schedules_update"
            | "schedules_change"
            | "schedules_run_due"
    );
    Some((read_only, destructive, open_world))
}

pub(super) fn apply(router: &mut ToolRouter<Mcp>) {
    for route in router.map.values_mut() {
        // New tools stay conservative until their effects are explicitly classified.
        let (read_only, destructive, open_world) =
            effects(route.name()).unwrap_or((false, true, true));
        route.attr.annotations = Some(
            route
                .attr
                .annotations
                .take()
                .unwrap_or_default()
                .read_only(read_only)
                .destructive(destructive)
                .open_world(open_world),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tool_has_explicit_effects_and_serialized_boolean_hints() {
        let mut router = Mcp::tool_router();
        for name in router.map.keys() {
            assert!(effects(name).is_some(), "classify effects for {name}");
        }
        apply(&mut router);
        for tool in router.list_all() {
            let annotations = serde_json::to_value(tool.annotations.unwrap()).unwrap();
            for hint in ["readOnlyHint", "destructiveHint", "openWorldHint"] {
                assert!(annotations[hint].is_boolean(), "{}: {hint}", tool.name);
            }
        }
        assert_eq!(effects("build_report"), Some((true, false, false)));
        assert_eq!(effects("storage_gc_preview"), Some((false, false, false)));
        assert_eq!(effects("schedules_preview"), Some((true, false, false)));
        assert_eq!(effects("pipeline_cancel"), Some((false, true, false)));
        assert_eq!(effects("server_settings"), Some((false, true, false)));
        assert_eq!(effects("pipeline_start"), Some((false, true, true)));
        assert_eq!(effects("server_release_check"), Some((false, false, true)));
    }
}
