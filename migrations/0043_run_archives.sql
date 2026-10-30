CREATE TABLE run_archives (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 run_id TEXT NOT NULL REFERENCES runs(id),
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','creating','ready','failed')),
 sha256 TEXT, file_bytes INTEGER, manifest_json TEXT, error TEXT,
 created_at INTEGER NOT NULL, finished_at INTEGER,
 UNIQUE(project_id,request_id)
);
CREATE INDEX run_archives_project ON run_archives(project_id,id);
INSERT INTO schema_version VALUES (43);
