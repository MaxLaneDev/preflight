CREATE TABLE change_queues (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_key TEXT NOT NULL,
 request_json TEXT NOT NULL,
 snapshot_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('active','cancelling','finished','cancelled')),
 cancel_actor TEXT,
 cancel_reason TEXT,
 UNIQUE(project_id,request_key)
);
CREATE TABLE change_queue_items (
 queue_id TEXT NOT NULL REFERENCES change_queues(id),
 ordinal INTEGER NOT NULL,
 name TEXT NOT NULL,
 source_json TEXT NOT NULL,
 run_id TEXT UNIQUE REFERENCES runs(id),
 state TEXT NOT NULL CHECK(state IN ('pending','running','succeeded','failed','cancelled','blocked')),
 error TEXT,
 PRIMARY KEY(queue_id,ordinal),
 UNIQUE(queue_id,name)
);
CREATE INDEX active_change_queues ON change_queues(state,id);
INSERT INTO schema_version VALUES (18);
