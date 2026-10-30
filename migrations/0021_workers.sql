CREATE TABLE worker_enrollments (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 name TEXT NOT NULL,
 expected_os TEXT NOT NULL,
 expected_arch TEXT NOT NULL,
 token_hash TEXT NOT NULL UNIQUE,
 expires_at INTEGER NOT NULL,
 used_by TEXT,
 credential_hash TEXT
);
CREATE TABLE workers (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id),
 name TEXT NOT NULL,
 os TEXT NOT NULL,
 arch TEXT NOT NULL,
 credential_hash TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('registered','revoked')),
 generation INTEGER NOT NULL DEFAULT 1 CHECK(generation>0),
 heartbeat_seq INTEGER NOT NULL DEFAULT 0,
 last_seen INTEGER,
 UNIQUE(project_id,name)
);
CREATE TABLE worker_audit (
 id INTEGER PRIMARY KEY,
 worker_id TEXT NOT NULL REFERENCES workers(id),
 generation INTEGER NOT NULL,
 action TEXT NOT NULL,
 actor TEXT NOT NULL,
 reason TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (21);
