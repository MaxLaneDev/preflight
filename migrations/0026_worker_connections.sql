CREATE TABLE worker_connections (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 name TEXT NOT NULL,
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','starting','connected','stopping','stopped','failed','interrupted')),
 worker_id TEXT REFERENCES workers(id),
 launcher_pid INTEGER,
 created_at INTEGER NOT NULL,
 finished_at INTEGER,
 actor TEXT,
 reason TEXT,
 UNIQUE(project_id,request_id)
);
CREATE UNIQUE INDEX worker_connection_active_name ON worker_connections(project_id,name)
 WHERE state IN ('queued','starting','connected','stopping','interrupted');
INSERT INTO schema_version VALUES (26);
