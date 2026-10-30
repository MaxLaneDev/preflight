CREATE TABLE schedule_occurrences (
 schedule_id TEXT NOT NULL REFERENCES schedules(id),
 at_ms INTEGER NOT NULL,
 state TEXT NOT NULL,
 run_id TEXT REFERENCES runs(id),
 reason TEXT NOT NULL,
 PRIMARY KEY(schedule_id,at_ms)
);
CREATE INDEX schedule_occurrences_run ON schedule_occurrences(run_id);
INSERT INTO schema_version VALUES (36);
