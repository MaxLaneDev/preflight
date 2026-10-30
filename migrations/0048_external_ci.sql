CREATE TABLE ci_syncs(
 id TEXT PRIMARY KEY, source_key TEXT NOT NULL, source_name TEXT NOT NULL,
 project_id TEXT NOT NULL REFERENCES projects(id), request_id TEXT NOT NULL,
 request_json TEXT NOT NULL, source_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','running','complete','partial','failed','cancelled')),
 result_json TEXT, error TEXT, retry_at INTEGER, cancel_requested INTEGER NOT NULL DEFAULT 0,cancel_actor TEXT,cancel_reason TEXT, created_at INTEGER NOT NULL, finished_at INTEGER,
 UNIQUE(project_id,request_id)
);
CREATE INDEX ci_syncs_pending ON ci_syncs(state,created_at,id);
CREATE TABLE ci_source_state(source_key TEXT PRIMARY KEY,last_polled INTEGER NOT NULL DEFAULT 0,retry_at INTEGER,cursor TEXT);
CREATE TABLE ci_runs(
 run_id TEXT PRIMARY KEY REFERENCES runs(id),source_key TEXT NOT NULL,source_name TEXT NOT NULL,
 provider TEXT NOT NULL,provider_run_id TEXT NOT NULL,provider_revision TEXT NOT NULL,
 workflow_key TEXT NOT NULL,git_sha TEXT,web_url TEXT,metadata_json TEXT NOT NULL,
 coverage_json TEXT NOT NULL,progress_json TEXT NOT NULL,state TEXT NOT NULL,
 imported_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
 UNIQUE(source_key,provider_run_id,provider_revision)
);
CREATE INDEX ci_runs_project_time ON ci_runs(source_key,run_id);
CREATE TABLE ci_jobs(
 run_id TEXT NOT NULL REFERENCES runs(id),stage_id TEXT NOT NULL,provider_job_id TEXT NOT NULL,
 metadata_json TEXT NOT NULL,log_state TEXT NOT NULL DEFAULT 'pending',log_bytes INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(run_id,stage_id)
);
CREATE TABLE ci_log_parts(
 run_id TEXT NOT NULL REFERENCES runs(id),stage_id TEXT NOT NULL,byte_offset INTEGER NOT NULL,
 sha256 TEXT NOT NULL,bytes INTEGER NOT NULL,PRIMARY KEY(run_id,stage_id,byte_offset)
);
CREATE TABLE ci_artifacts(
 run_id TEXT NOT NULL REFERENCES runs(id),artifact_id TEXT NOT NULL,stage_id TEXT NOT NULL,
 name TEXT NOT NULL,provider_json TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'metadata',
 sha256 TEXT,path TEXT,size INTEGER,PRIMARY KEY(run_id,artifact_id)
);
CREATE TABLE ci_test_outputs(run_id TEXT NOT NULL REFERENCES runs(id),case_id TEXT NOT NULL,stream TEXT NOT NULL,sha256 TEXT NOT NULL,PRIMARY KEY(run_id,case_id,stream));
CREATE TABLE ci_test_pages(run_id TEXT NOT NULL REFERENCES runs(id),page_key TEXT NOT NULL,report_json TEXT NOT NULL,PRIMARY KEY(run_id,page_key));
CREATE TABLE ci_sync_runs(sync_id TEXT NOT NULL REFERENCES ci_syncs(id),run_id TEXT NOT NULL REFERENCES runs(id),PRIMARY KEY(sync_id,run_id));
INSERT INTO schema_version VALUES (48);
