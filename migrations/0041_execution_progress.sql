ALTER TABLE change_queue_items ADD COLUMN finished_at INTEGER;
UPDATE change_queue_items SET finished_at=(SELECT r.finished_at FROM runs r WHERE r.id=change_queue_items.run_id) WHERE run_id IS NOT NULL;
CREATE INDEX runs_execution_state ON runs(status,id);
CREATE INDEX runs_execution_finished ON runs(finished_at,id);
CREATE INDEX change_items_execution_state ON change_queue_items(state,queue_id,ordinal);
CREATE INDEX change_items_execution_finished ON change_queue_items(finished_at,queue_id,ordinal);
INSERT INTO schema_version VALUES (41);
