CREATE TABLE operation_leases(
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    kind TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active','released','abandoned')),
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE TABLE operation_resources(
    lease_id TEXT NOT NULL REFERENCES operation_leases(id),
    resource TEXT NOT NULL,
    PRIMARY KEY(lease_id,resource)
);
CREATE INDEX active_operation_leases ON operation_leases(state,run_id);
CREATE INDEX operation_resource_lookup ON operation_resources(resource,lease_id);
INSERT INTO schema_version VALUES (12);
