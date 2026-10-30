CREATE TABLE evidence_expiry (
 run_id TEXT NOT NULL REFERENCES runs(id),
 category TEXT NOT NULL CHECK(category IN ('logs','source_snapshots')),
 plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id),
 ordinal INTEGER NOT NULL,
 expired_at INTEGER NOT NULL,
 PRIMARY KEY(run_id,category)
);
CREATE TABLE cache_access (
 cache_id TEXT PRIMARY KEY REFERENCES stage_cache(id),
 hits INTEGER NOT NULL DEFAULT 0,
 last_used INTEGER NOT NULL,
 rebuild_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE module_publications(project_id TEXT NOT NULL,sha256 TEXT NOT NULL,published_at INTEGER NOT NULL,PRIMARY KEY(project_id,sha256),FOREIGN KEY(project_id,sha256) REFERENCES modules(project_id,sha256));
INSERT INTO schema_version VALUES (47);
