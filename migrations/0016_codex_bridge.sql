CREATE TABLE codex_bindings(
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES runs(id),
 thread_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','sending','delivered','failed','uncertain','disabled')),
 attempts INTEGER NOT NULL DEFAULT 0,
 last_error TEXT,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 UNIQUE(run_id,thread_id)
);
CREATE INDEX codex_bindings_pending ON codex_bindings(state,run_id);
INSERT INTO schema_version VALUES (16);
