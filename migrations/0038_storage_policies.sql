CREATE TABLE storage_policy_cycles (
 id TEXT PRIMARY KEY,
 policy_hash TEXT NOT NULL,
 policy_json TEXT NOT NULL,
 project_id TEXT NOT NULL REFERENCES projects(id),
 cutoff INTEGER NOT NULL,
 cursor TEXT,
 plan_id TEXT REFERENCES storage_gc_plans(id),
 next_cursor TEXT,
 state TEXT NOT NULL CHECK(state IN ('active','complete','failed')),
 error TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX storage_policy_active ON storage_policy_cycles(policy_hash) WHERE state='active';
CREATE INDEX storage_policy_history ON storage_policy_cycles(policy_hash,created_at DESC);
INSERT INTO schema_version VALUES (38);
