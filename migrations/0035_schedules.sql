CREATE TABLE schedules (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 at_ms INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','paused','dispatching','submitted','skipped','cancelled','failed')),
 revision INTEGER NOT NULL,
 run_id TEXT REFERENCES runs(id),
 reason TEXT,
 created_at INTEGER NOT NULL,
 UNIQUE(project_id,request_id)
);
CREATE INDEX schedules_due ON schedules(state,at_ms,id);
CREATE TABLE schedule_changes (
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 result_json TEXT NOT NULL,
 PRIMARY KEY(project_id,request_id)
);
CREATE TABLE schedule_events (
 id INTEGER PRIMARY KEY,
 schedule_id TEXT NOT NULL REFERENCES schedules(id),
 state TEXT NOT NULL,
 actor TEXT NOT NULL,
 reason TEXT NOT NULL,
 timestamp INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (35);
