CREATE TABLE schedule_hidden (
 schedule_id TEXT PRIMARY KEY REFERENCES schedules(id),
 deleted_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (37);
