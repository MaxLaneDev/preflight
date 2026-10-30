CREATE TABLE run_sources(run_id TEXT PRIMARY KEY REFERENCES runs(id), request_json TEXT NOT NULL);
INSERT INTO schema_version VALUES (9);
