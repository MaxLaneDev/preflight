ALTER TABLE storage_gc_items ADD COLUMN object_key TEXT;
CREATE TABLE module_evictions (
 project_id TEXT NOT NULL,
 sha256 TEXT NOT NULL,
 plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id),
 ordinal INTEGER NOT NULL,
 PRIMARY KEY(project_id,sha256),
 FOREIGN KEY(project_id,sha256) REFERENCES modules(project_id,sha256)
);
INSERT INTO schema_version VALUES (31);
