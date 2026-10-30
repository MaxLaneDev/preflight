CREATE TABLE stage_keys(run_id TEXT NOT NULL, stage_id TEXT NOT NULL, evidence_json TEXT NOT NULL, PRIMARY KEY(run_id, stage_id), FOREIGN KEY(run_id, stage_id) REFERENCES stage_runs(run_id, name));
INSERT INTO schema_version VALUES (11);
