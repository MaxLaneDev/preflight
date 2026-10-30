# Storage lifecycle (1.55+)

Keep project_id and exact scope. storage_usage pages file lengths or logical log
payload bytes; partial accounting is not a quota total. min_free_disk_bytes is an
optional server admission floor, not a process limit or disk reservation.

Preview irreversible old successful payload expiry with evidence_expiry_preview:
project_id, category (logs/source_snapshots), acknowledge_loss=true, finished_before
(past Unix milliseconds). Optional pipeline_id is the cache-pipeline identity;
pipeline_key is the UI/saved-definition identity; run_id narrows to one run.
Do not substitute one kind of ID for another. Keep returned plan_id/cursor and
inspect protected reasons. Failures, baseline ownership, modules and leases retain
payloads. Mixed native source/output workspaces are protected.

Preview rebuildable cache-output removal with cache_eviction_preview using the same
scope/acknowledgment/cutoff. Pages use offset. Ranking combines idle time, hit count
and measured producer duration; each plan freezes exact entries. Other owners remain
protected. cache_cleanup invalidates entries only; this separate preview removes
eligible output bytes while preserving history.

Apply either plan with storage_gc_execute: project_id, plan_id, unique request_id,
actor, reason. Save the exact request for replay; use storage_gc_status. Do not retry
with changed arguments. Moving/quarantined work resumes; a failed terminal plan may
need a new preview after resolving its reported cause. Never widen a scope to force
a target. Inspect complete, per-item reasons and unmet quota observations.

logs_expired and expired_payloads mean intentionally unavailable data, not zero test
failures or an empty successful console. Log expiry preserves event IDs/timestamps
and releases logical SQLite payload pages. expired_log_payload_bytes is separate
from deleted_file_bytes; neither proves increased filesystem availability.

Automatic operator policies support stage_workspaces, artifact_expiry, logs,
source_snapshots, module_copies and cache_artifacts. TTL is always required; quotas
and disk triggers are optional and mutually exclusive. Logs reject disk triggers.
Module age uses actual publication time; unknown legacy age remains protected.
Newly released ownership is reconsidered in a later configured cycle. Settings hot
reload leaves current runs and approved cleanup pages undisturbed. Policies default
empty. No completion hook authorizes a cleanup or a new build.


Archives: retain archive_id/request_id/SHA256. Creation accepts an optional public
age X25519 encryption_recipient with protocol 1.61+. Discover configured
features.archive_identity_refs once; pass one exact name as identity_file_env for
encrypted archives_import, archives_verify or archives_restore. Never send a private
identity or key path. Download checksums describe ciphertext. Import must verify
both decryption and the normal manifest. Exact replay uses the original request;
changing its key reference conflicts. Ready may still have explicitly missing
previously-cleaned source paths. Restore returns isolated files and never activates
a live database/project. Full server recovery is a separate offline operator step.
