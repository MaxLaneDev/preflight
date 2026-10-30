ALTER TABLE log_events ADD COLUMN test_id TEXT;
CREATE INDEX logs_by_test ON log_events(run_id,stage_id,test_id,id);
CREATE TABLE acceptance_results (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 report_json TEXT NOT NULL,
 evaluated_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (40);
