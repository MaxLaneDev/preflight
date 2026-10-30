ALTER TABLE log_events ADD COLUMN ingested_at INTEGER;
CREATE TABLE diagnostic_scans (
 run_id TEXT PRIMARY KEY REFERENCES runs(id),
 format TEXT NOT NULL,
 event_id INTEGER NOT NULL DEFAULT 0,
 byte_offset INTEGER NOT NULL DEFAULT -1,
 events INTEGER NOT NULL DEFAULT 0,
 bytes INTEGER NOT NULL DEFAULT 0,
 state_json TEXT NOT NULL DEFAULT '[]',
 omitted INTEGER NOT NULL DEFAULT 0,
 drained INTEGER NOT NULL DEFAULT 0,
 terminal_flushed INTEGER NOT NULL DEFAULT 0,
 updated_at INTEGER NOT NULL
);
CREATE TABLE diagnostics (
 id INTEGER PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES runs(id),
 fingerprint TEXT NOT NULL,
 stage_id TEXT NOT NULL,
 severity TEXT NOT NULL,
 code TEXT,
 file TEXT,
 line INTEGER,
 column_number INTEGER,
 message TEXT NOT NULL,
 normalized_message TEXT NOT NULL,
 details_json TEXT NOT NULL DEFAULT '[]',
 adapter TEXT NOT NULL,
 occurrences INTEGER NOT NULL,
 evidence_json TEXT NOT NULL,
 UNIQUE(run_id,fingerprint)
);
CREATE INDEX diagnostics_scope ON diagnostics(run_id,stage_id,severity,file);
CREATE TABLE diagnostic_evidence (
 diagnostic_id INTEGER NOT NULL REFERENCES diagnostics(id) ON DELETE CASCADE,
 event_id INTEGER NOT NULL REFERENCES log_events(id),
 PRIMARY KEY(diagnostic_id,event_id)
);
CREATE INDEX diagnostic_evidence_event ON diagnostic_evidence(event_id,diagnostic_id);
CREATE VIRTUAL TABLE diagnostics_search USING fts5(text, tokenize='unicode61');
CREATE TRIGGER diagnostics_insert AFTER INSERT ON diagnostics BEGIN
 INSERT INTO diagnostics_search(rowid,text) VALUES (new.id,new.normalized_message||' '||new.details_json||' '||COALESCE(new.file,'')||' '||new.severity||' '||COALESCE(new.code,''));
END;
CREATE TRIGGER diagnostics_delete AFTER DELETE ON diagnostics BEGIN
 DELETE FROM diagnostics_search WHERE rowid=old.id;
END;
INSERT INTO schema_version VALUES (46);
