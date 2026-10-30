CREATE VIRTUAL TABLE log_search USING fts5(text, tokenize='unicode61');
INSERT INTO schema_version VALUES (30);
