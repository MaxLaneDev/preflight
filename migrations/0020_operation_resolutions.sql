CREATE TABLE operation_resolutions (
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_id TEXT NOT NULL,
 lease_id TEXT NOT NULL REFERENCES operation_leases(id),
 request_json TEXT NOT NULL,
 result_json TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(project_id, request_id),
 UNIQUE(lease_id)
);
INSERT INTO schema_version VALUES (20);
