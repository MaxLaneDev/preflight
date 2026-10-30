CREATE TABLE archive_pins (
 archive_id TEXT PRIMARY KEY, pinned INTEGER NOT NULL CHECK(pinned IN (0,1)),
 revision INTEGER NOT NULL, actor TEXT NOT NULL, reason TEXT NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE archive_pin_requests (
 project_id TEXT NOT NULL REFERENCES projects(id), request_id TEXT NOT NULL,
 request_json TEXT NOT NULL, result_json TEXT NOT NULL,
 PRIMARY KEY(project_id,request_id)
);
CREATE TABLE archive_gc_items (
 plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id), ordinal INTEGER NOT NULL,
 archive_id TEXT NOT NULL, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL,
 identity TEXT NOT NULL, pin_revision INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','deleting','deleted','skipped','blocked')),
 reason TEXT, removed_bytes INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(plan_id,ordinal)
);
CREATE TABLE archive_disposals (
 archive_id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES storage_gc_plans(id),
 ordinal INTEGER NOT NULL, created_at INTEGER NOT NULL
);
INSERT INTO schema_version VALUES (51);
