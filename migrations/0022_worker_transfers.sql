CREATE TABLE worker_transfers (
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES runs(id),
 worker_id TEXT NOT NULL REFERENCES workers(id),
 generation INTEGER NOT NULL,
 directory TEXT NOT NULL,
 tree_sha256 TEXT NOT NULL,
 wire_bytes INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 created_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (22);
