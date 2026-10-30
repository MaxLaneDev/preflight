CREATE TABLE artifact_pins (
 run_id TEXT NOT NULL,
 stage_id TEXT NOT NULL,
 artifact_name TEXT NOT NULL,
 owner TEXT NOT NULL,
 pinned INTEGER NOT NULL CHECK(pinned IN (0,1)),
 revision INTEGER NOT NULL CHECK(revision>0),
 actor TEXT NOT NULL,
 reason TEXT NOT NULL,
 PRIMARY KEY(run_id,stage_id,artifact_name,owner),
 FOREIGN KEY(run_id,stage_id,artifact_name) REFERENCES artifacts(run_id,stage_id,name)
);
CREATE TABLE artifact_pin_changes (
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 result_json TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(project_id,request_id)
);
INSERT INTO schema_version VALUES (33);
