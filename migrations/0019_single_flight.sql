CREATE TABLE shared_executions (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 owner_run TEXT NOT NULL,
 owner_stage TEXT NOT NULL,
 pipeline_id TEXT NOT NULL,
 cache_key TEXT NOT NULL,
 FOREIGN KEY(owner_run,owner_stage) REFERENCES stage_runs(run_id,name)
);
CREATE TABLE shared_subscribers (
 run_id TEXT NOT NULL,
 stage_id TEXT NOT NULL,
 execution_run TEXT NOT NULL REFERENCES shared_executions(run_id),
 state TEXT NOT NULL CHECK(state IN ('waiting','succeeded','failed','cancelled','abandoned')),
 PRIMARY KEY(run_id,stage_id),
 FOREIGN KEY(run_id,stage_id) REFERENCES stage_runs(run_id,name)
);
CREATE INDEX shared_execution_subscribers ON shared_subscribers(execution_run,state);
INSERT INTO schema_version VALUES (19);
