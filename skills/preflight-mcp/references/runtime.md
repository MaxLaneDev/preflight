# Runtime and access (protocol 1.57+)

Check capabilities and the live tool schema. The installed server may be older
than the source checkout. Docker is optional; native execution needs no daemon.

API bearer credentials are supplied by the MCP client's environment/header
configuration, not tool arguments. Read/execute/admin roles apply across registered
projects. Native execution is trusted operator execution, not user isolation.
An MCP session must keep its credential; a session ID does not replace it.
Never echo credentials. Administrator settings still have their separate
administrative credential guard. Ask for operator setup if the client lacks access.

For local Docker stages use executor docker and container with immutable image digest,
linux/arm64 or linux/amd64 platform. An operator must configure docker_command and
cache the image. The daemon must be local. Network is none and the image root is
read-only by default. No implicit pull, native fallback or remote host bind mount.
Use build_report/logs_read for command outcomes. containers_list returns bounded
job metadata, image identity, private paths and known exit/OOM/timestamps.

If cleanup_pending follows possible execution, new commands pause while current
commands continue. Keep the job ID. An administrator can call containers_cleanup
with exact project_id, job_id, request_id, actor and reason; replay only identical
arguments. Cancel a live run with pipeline_cancel first. Do not resolve a container
lease through a generic process-stopped assertion. Foreign ownership or failed
cleanup retains workspace protection. A never-started container may protect
storage without pausing command admission.

Prepared modules from Docker require the matching image/platform/runtime contract.
Use modules_contracts to inspect provenance. They do not become native legacy
modules. Host-profile requirements and tests-not-cached policy remain unchanged
for native builds. runs_cache/runs_stage_key explain reuse; inspect a shared
execution with runs_shared rather than reading empty subscriber logs.

A missing create acknowledgement is quarantined even if a label scan is empty:
the daemon could still complete that create later. Retry when the exact owned
container appears. Only an operator who has confirmed outstanding daemon create
requests have settled may use `creation_settled: true` to release an absent job.
This changes the request arguments and therefore needs a new request_id. CLI exit
alone is not that confirmation. No arbitrary container or image is removed.


Managed SSH on the M8 server requires worker package v2 with cli_protocol 2.
Check worker_package_format / worker_package_cli_protocol capabilities; repackage
an older bundle rather than retrying its rejected installation. Native gateway
protocol 1 and job protocol 3 are unchanged. SSH owns stdin lifetime; its closure
stops the worker before releasing state ownership. A bounded lock handoff waits
for actual release; it never authorizes replay, lock stealing or auto recovery.

Protocol 1.66 adds `stage_setup` and `prepared_container_images`. Ordinary stages
accept `setup: [{program,args}]`; these literal POSIX commands share the main
command's environment and timeout. Source an activation script with program `.`.
Setup runs on the selected native worker or inside the stage's local container;
SSH bootstrap itself still installs only the worker. Setup cannot use fixture
adapters, cache reuse or pinned profiles.

For reusable container tools, a producer can run `docker build --iidfile image-id .`
and publish image-id as a file output. A dependent local Docker stage uses
`container.image_from: "${artifact.prepare.image}"` and `needs: ["prepare"]`.
Omit literal image. Immutable ID, file digest and platform are verified. Images
remain on that daemon; cross-run module inputs with image_from are unsupported.
Remote Docker requires protocol 1.67 and the policy described below. User-created images are not artifact-GC targets.


Protocol 1.67 adds remote_container_stages. workers_list.container_jobs identifies
an opted-in job-protocol-4 worker. Native protocol-3 workers remain compatible.
A managed SSH connection can include runtime:{docker_command,max_memory_mib}; the
path belongs to the remote computer and is operator policy, never a job command.
Use executor:"docker", worker selector and container settings together. The server
need not enable its own Docker executor. Image consumers never pull; prepared image
producer/consumer must name the same explicit worker. Remote cache reuse, fixture
adapters and prepared module publication remain unsupported.

A pending worker container retains its journal/workspace. Do not resolve protection
or rerun merely because SSH exited. Stop the old worker; use worker journal and
worker containers-cleanup on that computer, then the existing lost-job recovery.
An explicit --creation-settled is valid only after actual daemon requests settled.


Protocol 1.68 advertises remote_live_logs. Updated workers forward bounded frames
into ordinary log storage while executing; logs_read and evidence_search need no
new model-facing endpoint. Retain their returned server cursors; worker log_cursor
is an internal acknowledgement, not a logs_read cursor. runs_worker_jobs includes
log_bytes and log_truncated. A storage limit closes forwarding with a visible marker,
not a build cancellation. Legacy workers keep bounded completion snapshots. A lost
job's accepted output remains readable; do not replay its command to recover logs.

Codex delivery (protocol 1.73+): `server_settings` can hot-reload
`configuration.codex_delivery_mode=queued|immediate`; preserve the full `saved`
configuration and pass its revision and administrative credential. `queued` never
steers/resumes. `immediate` requires private Unix desktop IPC, steers the active
owner, or requests idle continuation with `codex queue --resume` only after a
definite no-owner/idle response. Unknown acceptance is never resent. Omission
retains legacy behavior. Read `codex_hook_status.delivery_policy` for current
policy; it is not a historical receipt or proof the model answered. Bind only
the user-authorized originating chat; delivery does not authorize new work.
