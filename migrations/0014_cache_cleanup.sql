CREATE TABLE cache_cleanup_plans(
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 pipeline_id TEXT,
 plan_json TEXT NOT NULL,
 result_json TEXT,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO schema_version VALUES (14);
