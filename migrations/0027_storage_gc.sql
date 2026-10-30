CREATE TABLE storage_gc_plans (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 pipeline_id TEXT,
 preview_json TEXT NOT NULL,
 request_id TEXT,
 request_json TEXT,
 result_json TEXT,
 created_at INTEGER NOT NULL,
 UNIQUE(project_id,request_id)
);
CREATE TABLE storage_gc_items (
 plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id),
 ordinal INTEGER NOT NULL,
 run_id TEXT NOT NULL REFERENCES runs(id),
 stage_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 manifest_json TEXT NOT NULL,
 bytes INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','moving','quarantined','deleted','skipped','failed')),
 reason TEXT,
 PRIMARY KEY(plan_id,ordinal)
);
INSERT INTO schema_version VALUES (27);
