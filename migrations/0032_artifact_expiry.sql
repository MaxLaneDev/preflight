CREATE TABLE artifact_expiry (
 run_id TEXT NOT NULL,
 stage_id TEXT NOT NULL,
 plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id),
 ordinal INTEGER NOT NULL,
 PRIMARY KEY(run_id,stage_id),
 FOREIGN KEY(run_id,stage_id) REFERENCES stage_runs(run_id,name)
);
INSERT INTO schema_version VALUES (32);
