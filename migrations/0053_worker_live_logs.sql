ALTER TABLE worker_jobs ADD COLUMN log_cursor INTEGER NOT NULL DEFAULT 0;
ALTER TABLE worker_jobs ADD COLUMN log_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE worker_jobs ADD COLUMN log_events INTEGER NOT NULL DEFAULT 0;
ALTER TABLE worker_jobs ADD COLUMN log_truncated INTEGER NOT NULL DEFAULT 0;
CREATE TABLE worker_log_batches(
 job_id TEXT NOT NULL REFERENCES worker_jobs(id),
 after_cursor INTEGER NOT NULL,
 last_cursor INTEGER NOT NULL,
 payload_sha256 TEXT NOT NULL,
 PRIMARY KEY(job_id,after_cursor)
);
-- Checkpoints contain a known-secret digest and prefix length, never pending plaintext.
CREATE TABLE worker_log_streams(
 job_id TEXT NOT NULL REFERENCES worker_jobs(id),
 stream TEXT NOT NULL,
 checkpoint TEXT,
 PRIMARY KEY(job_id,stream)
);
INSERT INTO schema_version VALUES (53);
