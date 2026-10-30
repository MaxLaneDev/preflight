ALTER TABLE runs ADD COLUMN pipeline_json TEXT;
ALTER TABLE stage_runs RENAME TO stage_runs_v1;
CREATE TABLE stage_runs (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    name TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    definition_json TEXT NOT NULL,
    status TEXT NOT NULL,
    exit_code INTEGER,
    started_at INTEGER,
    finished_at INTEGER,
    cancel_actor TEXT,
    reason TEXT,
    tests_json TEXT,
    UNIQUE(run_id, name)
);
INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status,exit_code,started_at,finished_at,cancel_actor,reason)
SELECT s.id,s.run_id,'native',0,json_object('id','native','command',json(r.command_json)),s.status,s.exit_code,r.created_at,r.finished_at,r.cancel_actor,r.reason
FROM stage_runs_v1 s JOIN runs r ON r.id=s.run_id;
DROP TABLE stage_runs_v1;
ALTER TABLE log_events ADD COLUMN stage_id TEXT NOT NULL DEFAULT 'native';
CREATE INDEX logs_by_stage ON log_events(run_id, stage_id, id);
CREATE TABLE artifacts (
    run_id TEXT NOT NULL REFERENCES runs(id),
    stage_id TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(run_id, stage_id, name),
    FOREIGN KEY(run_id, stage_id) REFERENCES stage_runs(run_id, name)
);
INSERT INTO schema_version VALUES (2);
