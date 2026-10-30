CREATE TABLE worker_outputs (
 job_id TEXT PRIMARY KEY REFERENCES worker_jobs(id),
 tree_sha256 TEXT NOT NULL,
 directory TEXT NOT NULL,
 wire_bytes INTEGER NOT NULL,
 created_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (24);
