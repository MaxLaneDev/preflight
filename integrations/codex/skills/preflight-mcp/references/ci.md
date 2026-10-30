# External CI — protocol 1.56+

Import completed results only. Never trigger, retry or cancel provider workflows.
Check features.external_ci_ingestion. Admin server_settings manages ci_sources;
credentials stay in server environment variables. Empty sources is valid.

1. ci_sources_list {project_id,limit:10}. Keep source name and project ID.
2. ci_sync {source,request_id}; add provider_run_id for an exact decimal ID.
   Defaults: one run, two pages. Maximum: five runs, ten component pages and 32
   provider requests per sync.
3. Keep sync_id. Read ci_sync_status {project_id,sync_id} with bounded waits.
   States: queued/running/complete/partial/failed/cancelled. retry_at is Unix
   milliseconds; never request earlier after a rate limit.
4. Keep run_id. Use build_report and bounded logs_read, or ci_run_get for import
   coverage. Default 8192-byte budget, maximum 65536; page with next_offset.
5. Continue discovery with ci_sync, same source, fresh request_id and next_cursor.
   Continue a resumable partial run with its provider_run_id. Do not combine
   cursor and provider ID. Stop after obtaining the needed evidence.

ci_runs_list pages project imports; Trash stays hidden. ci_syncs_list pages local
imports globally or per project. ci_sync_cancel {project_id,sync_id,actor,reason}
stops queued/running local retrieval, never the remote job.

ci_artifact_fetch {project_id,run_id,artifact_id,request_id} queues a selected ZIP.
Keep its sync ID. Download through scoped HTTP /api/v1/ci/artifact-file after
completion; do not request MCP base64. SHA-256 and local/provider expiry are distinct.

Replay identical request IDs/arguments after uncertain responses. Partial coverage,
unknown tests and missing sources/dependencies stay explicit. Job success is not
a test count. Native stage retry is unavailable for imported terminal runs.
