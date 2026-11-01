---
name: preflight-mcp
description: Submit and inspect Preflight builds, tests and change queues through MCP using compact requests, result IDs and bounded evidence. Use for operating Preflight, not general CI advice.
---

Use the connected Preflight MCP tools. Keep project_id, queue_id, run_id, request_id
and cursors from responses; do not rediscover them on every call. Check capabilities
once per server/session when compatibility is unknown. Queues require protocol 1.13+.
Tool schemas remain authoritative if they differ from this skill.

Send only required fields and non-default options. Save/reuse pipeline definitions
and explicit profile revisions when the operation supports them. Do not invent aliases
or replace typed JSON with positional strings. A queue shares one definition across
all its changes; read [queues](references/queues.md) only for queue work.

Read isError before parsing. Successful text content is compact JSON. Unknown additive
fields may be ignored; absent counts are unknown, not zero. Errors are diagnostic text;
do not automatically retry mutations. Exact documented idempotency keys can be replayed
with identical arguments after uncertain transport outcomes. Otherwise inspect first.

For diagnostic search, comparisons or large logs (protocol 1.54+), read
[evidence](references/evidence.md) only when needed.

For optional completed external CI ingestion (protocol 1.56+), read
[CI imports](references/ci.md) only when needed. Never trigger provider workflows.

For environment setup (1.66+), Docker execution, role failures or quarantined container cleanup
(protocol 1.57+), read [runtime](references/runtime.md) only when needed.

After submission, retain the returned ID and use build_report or queue_status.
A queue status omits source details by default; request details=true only to compare
source identities. For a shared build (runs_shared), read logs using its execution_run and stage native;
its subscriber keeps a separate outcome. For failures, read the affected run's report first, then bounded
logs_read pages for its stage using the returned cursor. Fetch patches, manifests or
full schemas only for the specific question; stop paging once the needed evidence is
found and mention truncation when it prevents a conclusion.

Use project-scoped completion events/cursors or an authorized originating-thread Codex
hook instead of long polling loops. A queue's individual runs can be bound once their
IDs appear; there is currently no queue-level completion hook. A delivered hook is a
CLI acknowledgement, not proof the model consumed it. Do not send unrelated chats.

Cancel a selected run with pipeline_cancel, or the selected queue with queue_cancel.
Cache invalidation is separate from physical deletion; unpinning never deletes a module.
Runtime limits, native execution trust and unavailable features remain as advertised.
Do not claim token savings from smaller examples alone: compare equivalent completed
workflows including discovery, skill loading, calls, retries, bytes and actual tokens
when the client supplies them.

For physical reclamation (protocol 1.27+), use storage_gc_preview for disposable
stage trees or module_gc_preview for unused module copies. Review returned scope,
protected reasons and estimated bytes, then storage_gc_execute with plan_id,
project_id, request_id, actor and reason. Retain the exact request for resumable
replay; storage_gc_status reads progress. Module eviction preserves producer artifacts
and history but requires republication before that module can be used again.

Explicit evidence expiry uses artifact_expiry_preview (1.28+) with project_id,
expire_artifacts=true and a past finished_before Unix-millisecond cutoff. It is
separate from cache cleanup; retain metadata and show artifact_expired honestly.
Active cache entries must be invalidated separately. Execute the reviewed plan with
the same storage_gc_execute contract. Saved experiment baselines are protected.

Before explicit retention expiry, retain important outputs with artifact_pin_set
(1.29+): exact project/run/stage/artifact, owner, pinned, expected_revision,
request_id, actor and reason. Read artifact_retention for current revisions and
stage_active_owners. Releasing a pin never deletes bytes or bypasses other references.

Logs may be stored as verified zstd chunks (1.30+); logs_read cursors stay unchanged.
Use logs_storage only when diagnosing storage. logs_archive advances a bounded
completed-run batch; repeat while more=true. Payload-byte savings exclude SQLite
and FTS overhead. Treat a chunk integrity error as missing evidence, not empty logs.

Schedules (1.31+): create with exact pipeline/profile revisions, absolute UTC at_ms,
explicit misfire/grace_ms and request_id/actor/reason. Sources are captured at dispatch.
Retain schedule_id/revision; read schedules_get, change with expected_revision.
Cancel pending schedules with schedules_change; cancel submitted runs separately.
The server dispatches automatically; schedules_run_due is a bounded manual trigger.

For observed disk usage (protocol 1.32+), call storage_usage with project_id,
category (run_workspaces, stage_workspaces, module_copies), optional pipeline_id,
limit and max_entries. Pass next_cursor as before. run_workspaces is project-only.
Inspect page_complete; partial scans and active runs are not complete totals.
Categories overlap and shared module bytes are not reclaimable bytes. Use the
existing GC preview/execute tools to determine safe deletion.

For code differences (protocol 1.34+), first use runs_compare_inputs to identify
paths, then runs_source_diff with project_id, run_id, baseline_id and one path.
Read only relevant files: responses contain unified text, at most 256 KiB per side.
Snapshots are digest-verified; unavailable, changed or binary evidence is an error.
Do not substitute the live checkout when historical source evidence is unavailable.

For age-scoped stage cleanup (protocol 1.35+), pass finished_before to
storage_gc_preview as an absolute past Unix-millisecond cutoff. It is exclusive,
recorded in the plan and rechecked during execute. Keep the same cutoff while paging;
do not recalculate a moving threshold on retries. Source/history/artifact retention
is unchanged. This field is not supported by module_gc_preview.

For schedule or experiment work, read [workflows](references/schedules.md).

For automatic retention status/configuration, read [storage policies](references/storage.md).

From protocol 1.39, build_report omits repeated definitions/profile snapshots. Use runs_definition for recorded configuration, runs_toolchain for verified paths/contracts, and the bounded stage_failures log references. Original-path tools require explicit run_in_place with pins; private copies remain the default.

For log investigation use evidence_search with project_id and optional run_id, stage_id, test_id, kind and stream. Keep log:ID references; evidence_read expands one reference, evidence_context returns bounded cited text. Read runs_acceptance for recorded criteria and verdicts. Read [evidence](references/evidence.md) for retrieval details. These are literal retrieval tools; do not infer absent test counts or semantic matches.

For large navigation lists use catalog_list: kind=workspaces|pipelines|saved_pipelines|experiments, project_id when scoped, query, limit (default25/max100), after=returned next_cursor. Preserve the same filter while paging; fetch definition details only after choosing a returned name/revision.

Cross-project work: executions_list(view=active|planned|recent, kind=all|pipeline|experiment, optional project_id/query/after/limit). Retain each row ownership and target reference. executions_summary returns since: reuse it to observe one progress cohort. executions_cancel requires project_id, target_kind=run|queue_item|queue|schedule, target_id, actor and reason; use ordinal for a change item and revision for a schedule. Never widen selected cancellation. Check actual terminal state after requesting stop. pipeline_check validates JSON text; pipeline_revisions pages immutable history; pipeline_restore creates a new revision with an explicit expected_revision. Future experiment schedules pin experiment plus its matching base pipeline and use the saved experiment profile.

Manual maintenance: management_preview(project_id, action=clear_queue|artifacts|delete_pipeline|delete_run, optional target_id/pipeline_key). Keep the exact returned plan and execute_tool. Apply management plans with management_execute(project_id,plan_id,request_id,actor,reason); artifact expiry uses storage_gc_execute. Repeat only an identical execute for replay. Re-preview next batches; do not widen scope. Trash is recoverable, does not free artifact bytes, and retains existing revision/evidence references. management_trash pages deleted project objects; management_restore restores one exact kind/target. Clear pending queue skips progressed/edited items and leaves active work running.

Optional archives: archives_create(project_id,run_id,request_id,actor,reason) on terminal runs; logs/artifacts/source default true, environment defaults false. Retain archive_id and inspect archives_get until ready/failed. A ready archive with missing paths is partial. archives_list pages project/run/pipeline snapshots. archives_verify checks the saved bytes; archives_restore writes only a new private isolated directory and never rehydrates a live DB or executes code. A used restore request never overwrites files. Native workspace capture excludes the host OS and external installed tools/SDKs. Raw archives may contain secrets: do not publish or transmit without authorization.

Partial retries (protocol 1.45+): pipeline_retry_preview accepts project_id, parent
run_id, stage_id and optional test_id, stage_override and source. pipeline_retry
accepts that object as selection plus idempotency_key, actor and reason. Keep the
returned run_id; report/logs/completion tools remain unchanged. A retry is exactly one
stage, preserves the parent and whole_pipeline_verdict=false. Dependencies must have
succeeded; their declared outputs are SHA-verified modules. Current project files are
captured by default; use a new key for another edit. Native fixtures select a case ID;
other runners need testing.rerun with a whole ${test.name} or ${test.id} argument and
an actual complete single-case report. Never infer test counts or pretend the whole
pipeline is now successful.

Language policy (1.45.1): Preflight-owned descriptions, statuses, errors and hints
are English-only, independent of client locale. Capabilities expose
service_language=en and data_language=original. Preserve names, paths, source,
commands, raw logs/test diagnostics and user reasons verbatim; do not translate
canonical fields or evidence.

Managed SSH reconnect (protocol 1.46+): worker_ssh_start optionally accepts sibling
reconnect={max_retries:1..10,delay_seconds:1..300}; default is no retries and delay
is 10 seconds when omitted. Not valid with connection.once=true. Reuse the returned
connection_id. Status reconnect includes attempt/retries/next_retry_at/last_error;
queued plus next_retry_at means backoff, not execution readiness. Stop cancels backoff.
Policy changes conflict with the same request_id. After readiness, reconnect must
reuse private identity without new enrollment; it never replays lost jobs. Server
restart still requires explicit interrupted-process recovery, never stale-PID signals.

Global search (protocol 1.47+): search_global({query,limit:5,optional project_id,after})
locates project/pipeline/run/stage/test IDs and names, artifact/module/input/source/stage
hashes and redacted indexed log terms. #N resolves a log event. Retain IDs and target
project/run/stage context, then use the focused detail tool. Reuse next_cursor as after
with the same query/scope only when more results are needed. Exact identities rank
first. Hidden runs/pipelines are omitted; unavailable_projects concerns saved catalogs,
not proof that all persisted evidence is absent. No case evidence means no invented test
hits. Search performs no build, cancellation or configuration mutation.

For a lost worker job, read worker_recovery_status(project_id, job_id). Verify old
processes stopped, then use the returned lease_id with operation_resolve and an
explicit actor/reason/process_stopped attestation. Never infer termination from a
failed connection alone. After recovery_allowed=true, start a new managed request
for the same worker with connection.recovery={job_id,process_stopped:true}; this
archives only that private pending journal and resumes without enrollment or replay.
Reuse request_id only with identical arguments. Private receipts retain claim
material: use public journal/recovery summaries, never paste identity/receipt files.

Automatic cleanup is opt-in server_settings.storage_policies configuration.
Read advertised storage_policy_categories before writing: stage_workspaces removes
regenerable copies; artifact_expiry explicitly expires eligible successful output
bytes while retaining metadata/SHA/history and pins/references/leases. Keep policy
cycle_id/plan_id from storage_policy_status and inspect storage_gc_status for reasons.
Changing policy does not invalidate cache or unpin outputs. Never enable an automatic
expiry policy merely to inspect storage or test connectivity.

When configured_storage_quotas is advertised, a policy may include
quota:{high_bytes,low_bytes} (0 <= low < high, integer bytes). This is a soft
known-file-length target combined with TTL, not free-disk enforcement. Keep cycle
IDs and read quota_observation.complete/triggered and error; incomplete scans or
protected unmet targets are not successful reclamation. Clearing quota restores
TTL-only behavior, not disabled cleanup. Empty storage_policies disables both.

When configured_disk_watermarks is advertised, disk_pressure:{start_below_bytes,
stop_at_bytes} is an alternative to quota on a policy. It samples availability of
the data_dir filesystem, not file lengths. TTL/protections still apply; inspect
separate disk_observation and errors. Unmet triggered cycles retain hysteresis
across retry. Clearing the condition restores TTL-only cleanup; remove the policy
to disable cleanup. Do not enable disk cleanup for a read-only probe.

For category retention, ranked physical cache eviction or disk admission (1.55+),
read [storage](references/storage.md). Never confuse logical log payload expiry
with filesystem space reclaimed or invalidate_entries with physical output eviction.
