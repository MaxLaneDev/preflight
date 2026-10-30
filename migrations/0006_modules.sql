CREATE TABLE modules(project_id TEXT NOT NULL REFERENCES projects(id), sha256 TEXT NOT NULL, artifact_json TEXT NOT NULL, PRIMARY KEY(project_id,sha256));
INSERT INTO schema_version VALUES (6);
