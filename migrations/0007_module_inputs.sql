CREATE TABLE module_inputs(run_id TEXT NOT NULL, stage_id TEXT NOT NULL, path TEXT NOT NULL, artifact_json TEXT NOT NULL, PRIMARY KEY(run_id,stage_id,path), FOREIGN KEY(run_id,stage_id) REFERENCES stage_runs(run_id,name));
INSERT INTO schema_version VALUES (7);
