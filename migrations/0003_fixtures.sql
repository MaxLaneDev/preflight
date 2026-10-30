CREATE TABLE fixture_results (
    run_id TEXT NOT NULL,
    stage_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    result_json TEXT,
    PRIMARY KEY(run_id,stage_id,case_id),
    FOREIGN KEY(run_id,stage_id) REFERENCES stage_runs(run_id,name)
);
INSERT INTO schema_version VALUES (3);
