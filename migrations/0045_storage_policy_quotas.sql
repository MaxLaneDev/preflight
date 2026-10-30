ALTER TABLE storage_policy_cycles ADD COLUMN observation_json TEXT;
INSERT INTO schema_version VALUES (45);
