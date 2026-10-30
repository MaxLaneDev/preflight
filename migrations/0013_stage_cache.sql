CREATE TABLE stage_cache(
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 pipeline_id TEXT NOT NULL,
 cache_key TEXT NOT NULL,
 producer_run TEXT NOT NULL,
 producer_stage TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('active','invalidated')),
 FOREIGN KEY(producer_run,producer_stage) REFERENCES stage_runs(run_id,name)
);
CREATE INDEX stage_cache_lookup ON stage_cache(project_id,pipeline_id,cache_key,state);
CREATE TABLE stage_cache_decisions(
 run_id TEXT NOT NULL,
 stage_id TEXT NOT NULL,
 decision_json TEXT NOT NULL,
 PRIMARY KEY(run_id,stage_id),
 FOREIGN KEY(run_id,stage_id) REFERENCES stage_runs(run_id,name)
);
INSERT INTO schema_version VALUES (13);
