ALTER TABLE workers ADD COLUMN job_protocol INTEGER;
INSERT INTO schema_version VALUES (25);
