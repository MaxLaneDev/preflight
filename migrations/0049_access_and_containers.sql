CREATE TABLE access_audit(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 principal TEXT NOT NULL,role TEXT,operation TEXT NOT NULL,outcome TEXT NOT NULL,created_at INTEGER NOT NULL
);
CREATE TABLE container_jobs(
 id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),stage_id TEXT NOT NULL,
 lease_id TEXT NOT NULL REFERENCES operation_leases(id),platform TEXT NOT NULL,endpoint TEXT NOT NULL,container_id TEXT,actual_image_id TEXT,may_run INTEGER NOT NULL DEFAULT 0,
 name TEXT NOT NULL UNIQUE,owner_token TEXT NOT NULL,image TEXT NOT NULL,workspace TEXT NOT NULL,
 state TEXT NOT NULL,reason TEXT,result_json TEXT,created_at INTEGER NOT NULL,finished_at INTEGER
);
CREATE TABLE container_cleanup_requests(project_id TEXT NOT NULL REFERENCES projects(id),request_id TEXT NOT NULL,request_json TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(project_id,request_id));
CREATE INDEX container_jobs_state ON container_jobs(state,created_at);
INSERT INTO schema_version VALUES (49);
