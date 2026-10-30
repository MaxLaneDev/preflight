# Search build and test output

Open **Evidence**. Filter by run, stage, test, output stream or severity.

`evidence_search`

<!-- mcp: evidence_search -->
```json
{
  "project_id": "PROJECT_ID",
  "query": "undefined reference",
  "run_id": "RUN_ID",
  "limit": 10
}
```

`evidence_read`

<!-- mcp: evidence_read -->
```json
{
  "project_id": "PROJECT_ID",
  "evidence_id": "EVIDENCE_ID"
}
```

To send bounded retrieval context to a model:

`evidence_context`

<!-- mcp: evidence_context -->
```json
{
  "project_id": "PROJECT_ID",
  "query": "failed assertion",
  "max_chars": 4000
}
```

Results link to recorded events. Keep returned IDs/cursors for subsequent pages.
Search is over indexed evidence; expired or truncated payloads are reported. It
does not infer test counts or recreate missing logs. [Log retention](log-storage.md).
