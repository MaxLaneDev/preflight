ALTER TABLE modules ADD COLUMN manifest_json TEXT;
INSERT INTO schema_version VALUES (8);
