CREATE TABLE log_chunks (
 id INTEGER PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES runs(id),
 first_event INTEGER NOT NULL,
 last_event INTEGER NOT NULL,
 event_count INTEGER NOT NULL,
 raw_bytes INTEGER NOT NULL,
 text_bytes INTEGER NOT NULL,
 sha256 TEXT NOT NULL,
 codec TEXT NOT NULL CHECK(codec='zstd-json-v1'),
 payload BLOB NOT NULL,
 UNIQUE(run_id,first_event)
);
ALTER TABLE log_events ADD COLUMN chunk_id INTEGER REFERENCES log_chunks(id);
CREATE INDEX logs_unarchived ON log_events(run_id,chunk_id,id);
INSERT INTO schema_version VALUES (34);
