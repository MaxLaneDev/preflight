ALTER TABLE worker_connections ADD COLUMN retry_limit INTEGER NOT NULL DEFAULT 0 CHECK(retry_limit BETWEEN 0 AND 10);
ALTER TABLE worker_connections ADD COLUMN retry_delay_seconds INTEGER NOT NULL DEFAULT 10 CHECK(retry_delay_seconds BETWEEN 1 AND 300);
ALTER TABLE worker_connections ADD COLUMN retries INTEGER NOT NULL DEFAULT 0 CHECK(retries >= 0);
ALTER TABLE worker_connections ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0);
ALTER TABLE worker_connections ADD COLUMN next_retry_at INTEGER;
ALTER TABLE worker_connections ADD COLUMN last_error TEXT;
INSERT INTO schema_version VALUES (44);
