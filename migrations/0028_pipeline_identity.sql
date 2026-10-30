CREATE TABLE run_pipeline_identity (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 pipeline_key TEXT NOT NULL,
 name TEXT NOT NULL,
 saved_name TEXT
);
CREATE INDEX run_pipeline_key ON run_pipeline_identity(pipeline_key,run_id);
INSERT INTO schema_version VALUES (28);
