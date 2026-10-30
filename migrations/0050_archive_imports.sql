CREATE TABLE archive_imports (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 origin_project_id TEXT NOT NULL,
 run_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state='ready'),
 sha256 TEXT NOT NULL, file_bytes INTEGER NOT NULL,
 manifest_json TEXT NOT NULL, error TEXT,
 created_at INTEGER NOT NULL, finished_at INTEGER NOT NULL,
 UNIQUE(project_id,request_id)
);
CREATE INDEX archive_imports_project ON archive_imports(project_id,id);
CREATE VIEW archive_inventory AS
 SELECT id,project_id,run_id,request_id,request_json,state,sha256,file_bytes,
 manifest_json,error,created_at,finished_at,project_id AS origin_project_id,0 AS imported FROM run_archives
 UNION ALL
 SELECT id,project_id,run_id,request_id,request_json,state,sha256,file_bytes,
 manifest_json,error,created_at,finished_at,origin_project_id,1 AS imported FROM archive_imports;
INSERT INTO schema_version VALUES (50);
