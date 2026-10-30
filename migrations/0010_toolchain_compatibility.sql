CREATE TABLE run_toolchains(run_id TEXT PRIMARY KEY REFERENCES runs(id), verified_json TEXT NOT NULL);
CREATE TABLE module_contracts(project_id TEXT NOT NULL, sha256 TEXT NOT NULL, contract_sha256 TEXT NOT NULL, contract_json TEXT, artifact_json TEXT NOT NULL, PRIMARY KEY(project_id,sha256,contract_sha256), FOREIGN KEY(project_id,sha256) REFERENCES modules(project_id,sha256));
INSERT INTO module_contracts(project_id,sha256,contract_sha256,contract_json,artifact_json) SELECT project_id,sha256,'',NULL,artifact_json FROM modules;
INSERT INTO schema_version VALUES (10);
