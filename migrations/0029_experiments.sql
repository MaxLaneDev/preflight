CREATE TABLE experiment_runs (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 project_id TEXT NOT NULL REFERENCES projects(id),
 name TEXT NOT NULL,
 revision INTEGER NOT NULL,
 definition_json TEXT NOT NULL,
 baseline_run TEXT REFERENCES runs(id)
);
CREATE INDEX experiment_history ON experiment_runs(project_id,name,run_id);
INSERT INTO schema_version VALUES (29);
