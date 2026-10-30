-- Keep creation replay stable after edits of future dispatch configuration.
CREATE TABLE schedule_original_requests (
    schedule_id TEXT PRIMARY KEY REFERENCES schedules(id),
    request_json TEXT NOT NULL
);

INSERT INTO schema_version VALUES (39);
