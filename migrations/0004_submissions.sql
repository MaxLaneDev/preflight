CREATE TABLE submissions (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_key TEXT,
 fingerprint TEXT NOT NULL,
 request_json TEXT NOT NULL,
 UNIQUE(project_id,request_key)
);
INSERT INTO schema_version VALUES (4);
