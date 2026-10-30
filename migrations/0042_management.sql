CREATE TABLE management_trash (
 project_id TEXT NOT NULL REFERENCES projects(id), kind TEXT NOT NULL CHECK(kind IN ('pipeline','run')),
 target_id TEXT NOT NULL, deleted_at INTEGER NOT NULL, restored_at INTEGER,
 PRIMARY KEY(project_id,kind,target_id)
);
CREATE TABLE management_plans (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
 preview_json TEXT NOT NULL, created_at INTEGER NOT NULL, request_json TEXT, result_json TEXT
);
INSERT INTO schema_version(version) VALUES (42);
