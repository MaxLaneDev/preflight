CREATE TABLE worker_jobs (
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES runs(id),
 stage_id TEXT NOT NULL,
 worker_id TEXT NOT NULL REFERENCES workers(id),
 generation INTEGER NOT NULL,
 transfer_id TEXT NOT NULL REFERENCES worker_transfers(id),
 operation_id TEXT NOT NULL REFERENCES operation_leases(id),
 spec_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','leased','cancelling','succeeded','failed','cancelled','timed_out','infrastructure_error','lost')),
 claim_hash TEXT UNIQUE,
 renew_sequence INTEGER NOT NULL DEFAULT 0,
 lease_expires_at INTEGER,
 deadline INTEGER NOT NULL,
 result_json TEXT,
 reason TEXT,
 UNIQUE(run_id,stage_id)
);
INSERT INTO schema_version VALUES (23);
