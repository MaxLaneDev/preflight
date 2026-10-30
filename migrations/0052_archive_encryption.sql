CREATE TABLE archive_encryption (
 archive_id TEXT PRIMARY KEY,
 format TEXT NOT NULL CHECK(format='age-encryption.org/v1'),
 recipient TEXT
);
INSERT INTO schema_version VALUES (52);
