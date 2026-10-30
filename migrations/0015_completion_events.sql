CREATE TABLE completion_events(
 cursor INTEGER PRIMARY KEY AUTOINCREMENT,
 run_id TEXT NOT NULL UNIQUE REFERENCES runs(id),
 project_id TEXT NOT NULL REFERENCES projects(id),
 status TEXT NOT NULL,
 finished_at INTEGER NOT NULL,
 historical INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX completion_events_project ON completion_events(project_id,cursor);
INSERT INTO completion_events(run_id,project_id,status,finished_at,historical)
 SELECT id,project_id,status,finished_at,1 FROM runs WHERE finished_at IS NOT NULL ORDER BY finished_at,id;
INSERT INTO schema_version VALUES (15);
