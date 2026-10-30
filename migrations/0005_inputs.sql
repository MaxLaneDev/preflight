CREATE TABLE run_inputs (run_id TEXT PRIMARY KEY REFERENCES runs(id), manifest_json TEXT NOT NULL);
INSERT INTO schema_version VALUES (5);
